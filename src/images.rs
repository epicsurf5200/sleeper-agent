//! Player headshots, fetched from Sleeper's CDN and cached on disk.
//!
//! Sleeper serves a portrait per player id at a stable URL, so this is a plain
//! keyed fetch. Images are cached to disk on first download and decoded into an
//! egui texture on first use; both layers are shared across every tab, so the
//! roster list and the detail window never fetch the same face twice.

use eframe::egui::{self, ColorImage, TextureHandle, TextureOptions};
use parking_lot::Mutex;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;

const CDN: &str = "https://sleepercdn.com/content/nfl/players";

/// Cache state for one player id.
enum Slot {
    /// Decoded and ready to upload to the GPU on the next frame.
    Decoded(ColorImage),
    /// Uploaded.
    Ready(TextureHandle),
    /// No image available (404, decode failure). Remembered so we stop asking.
    Missing,
}

pub struct ImageCache {
    slots: Mutex<HashMap<String, Slot>>,
    inflight: Mutex<HashSet<String>>,
    rt: tokio::runtime::Handle,
    http: reqwest::Client,
    dir: PathBuf,
}

impl ImageCache {
    pub fn new(rt: tokio::runtime::Handle) -> Self {
        let dir = match std::env::var("SA_CACHE_DIR") {
            Ok(d) => PathBuf::from(d),
            Err(_) => dirs::cache_dir()
                .unwrap_or_else(|| PathBuf::from("."))
                .join("sleeper-agent"),
        }
        .join("headshots");
        let _ = std::fs::create_dir_all(&dir);
        Self {
            slots: Mutex::new(HashMap::new()),
            inflight: Mutex::new(HashSet::new()),
            rt,
            http: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(15))
                .user_agent("sleeper-agent/0.1 (headshots)")
                .build()
                .unwrap_or_default(),
            dir,
        }
    }

    /// Texture for a player, kicking off a fetch on first miss. Returns None
    /// until the image is available — callers draw a placeholder meanwhile.
    pub fn texture(
        self: &Arc<Self>,
        ctx: &egui::Context,
        player_id: &str,
    ) -> Option<TextureHandle> {
        // Team defenses use a team code as their id and have no headshot.
        if player_id.is_empty() {
            return None;
        }
        {
            let mut slots = self.slots.lock();
            match slots.get(player_id) {
                Some(Slot::Ready(t)) => return Some(t.clone()),
                Some(Slot::Missing) => return None,
                Some(Slot::Decoded(_)) => {
                    // Upload on the current frame, then keep the handle.
                    let Some(Slot::Decoded(img)) = slots.remove(player_id) else {
                        return None;
                    };
                    let tex = ctx.load_texture(
                        format!("headshot-{player_id}"),
                        img,
                        TextureOptions::LINEAR,
                    );
                    slots.insert(player_id.to_string(), Slot::Ready(tex.clone()));
                    return Some(tex);
                }
                None => {}
            }
        }
        self.spawn_fetch(ctx, player_id);
        None
    }

    fn spawn_fetch(self: &Arc<Self>, ctx: &egui::Context, player_id: &str) {
        if !self.inflight.lock().insert(player_id.to_string()) {
            return; // already being fetched
        }
        let this = self.clone();
        let ctx = ctx.clone();
        let id = player_id.to_string();
        self.rt.spawn(async move {
            let slot = match this.load(&id).await {
                Some(img) => Slot::Decoded(img),
                None => Slot::Missing,
            };
            this.slots.lock().insert(id.clone(), slot);
            this.inflight.lock().remove(&id);
            // Wake the UI so the new face appears without waiting for input.
            ctx.request_repaint();
        });
    }

    /// Disk first, then the CDN. Returns None when there is no usable image.
    async fn load(&self, player_id: &str) -> Option<ColorImage> {
        let path = self.dir.join(format!("{player_id}.jpg"));
        if let Ok(bytes) = tokio::fs::read(&path).await {
            if let Some(img) = decode(&bytes) {
                return Some(img);
            }
        }
        let url = format!("{CDN}/{player_id}.jpg");
        let resp = self.http.get(&url).send().await.ok()?;
        if !resp.status().is_success() {
            tracing::debug!(player_id, status = %resp.status(), "no headshot available");
            return None;
        }
        let bytes = resp.bytes().await.ok()?;
        let img = decode(&bytes)?;
        // Best-effort persist; a failed write just means we refetch next run.
        let _ = tokio::fs::write(&path, &bytes).await;
        Some(img)
    }
}

fn decode(bytes: &[u8]) -> Option<ColorImage> {
    let img = image::load_from_memory(bytes).ok()?;
    let rgba = img.to_rgba8();
    let (w, h) = rgba.dimensions();
    Some(ColorImage::from_rgba_unmultiplied(
        [w as usize, h as usize],
        rgba.as_raw(),
    ))
}
