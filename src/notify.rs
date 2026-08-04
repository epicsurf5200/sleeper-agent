//! Outbound alerts over a generic webhook.
//!
//! Discord is the default target — its incoming-webhook endpoint takes a JSON
//! body with an `embeds` array, which renders as a coloured card on phone and
//! desktop. `format: json` posts the alert verbatim instead, which is what you
//! want for Home Assistant, n8n, or anything that parses fields itself.

use crate::config::NotifyConfig;
use anyhow::{Context, Result};
use serde::Serialize;
use std::time::Duration;

/// What produced an alert. Doubles as the dedupe namespace so a standing
/// waiver suggestion never suppresses a fresh injury alert.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
pub enum AlertKind {
    Lineup,
    Injury,
    Waiver,
    Trade,
}

impl AlertKind {
    pub fn label(&self) -> &'static str {
        match self {
            AlertKind::Lineup => "Lineup",
            AlertKind::Injury => "Injury",
            AlertKind::Waiver => "Waiver",
            AlertKind::Trade => "Trade",
        }
    }

    /// Discord embed colour, as a decimal RGB int.
    fn color(&self) -> u32 {
        match self {
            AlertKind::Injury => 0xE0_5252,  // red — time-critical
            AlertKind::Lineup => 0x91_84D9,  // brand purple
            AlertKind::Waiver => 0x4C_9A6B,  // green
            AlertKind::Trade => 0xD9_9A3F,   // amber
        }
    }

    fn emoji(&self) -> &'static str {
        match self {
            AlertKind::Injury => "\u{1F6A8}",
            AlertKind::Lineup => "\u{1F504}",
            AlertKind::Waiver => "\u{1F4C8}",
            AlertKind::Trade => "\u{1F91D}",
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Alert {
    pub kind: AlertKind,
    pub title: String,
    pub body: String,
    /// Stable identity of the *content*. Two alerts with the same fingerprint
    /// are the same news, so the daemon only sends the first.
    pub fingerprint: String,
}

impl Alert {
    pub fn new(
        kind: AlertKind,
        title: impl Into<String>,
        body: impl Into<String>,
        fingerprint: impl Into<String>,
    ) -> Self {
        Self {
            kind,
            title: title.into(),
            body: body.into(),
            fingerprint: fingerprint.into(),
        }
    }
}

pub struct Notifier {
    http: reqwest::Client,
    url: String,
    discord: bool,
}

impl Notifier {
    /// Returns None when no webhook is configured — notifications are simply off.
    pub fn new(cfg: &NotifyConfig) -> Result<Option<Self>> {
        if !cfg.is_enabled() {
            return Ok(None);
        }
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(20))
            .user_agent("sleeper-agent")
            .build()
            .context("building webhook http client")?;
        Ok(Some(Self {
            http,
            url: cfg.webhook_url.clone(),
            // Default to Discord unless explicitly asked for raw JSON.
            discord: !cfg.format.eq_ignore_ascii_case("json"),
        }))
    }

    pub fn send(&self, alert: &Alert) -> impl std::future::Future<Output = Result<()>> + '_ {
        let payload = if self.discord {
            discord_payload(alert)
        } else {
            serde_json::to_value(alert).unwrap_or(serde_json::Value::Null)
        };
        let req = self.http.post(&self.url).json(&payload);
        async move {
            let resp = req.send().await.context("posting webhook")?;
            let status = resp.status();
            if !status.is_success() {
                let body = resp.text().await.unwrap_or_default();
                anyhow::bail!("webhook returned {status}: {}", truncate(&body, 300));
            }
            Ok(())
        }
    }
}

/// Discord caps embed descriptions at 4096 chars and titles at 256.
fn discord_payload(alert: &Alert) -> serde_json::Value {
    serde_json::json!({
        "username": "Sleeper Agent",
        "embeds": [{
            "title": truncate(
                &format!("{} {}", alert.kind.emoji(), alert.title),
                256,
            ),
            "description": truncate(&alert.body, 4096),
            "color": alert.kind.color(),
            "footer": { "text": format!("sleeper-agent · {}", alert.kind.label()) },
        }],
    })
}

/// Char-based so a multi-byte name at the boundary can't panic.
fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    format!("{}…", s.chars().take(max.saturating_sub(1)).collect::<String>())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn alert() -> Alert {
        Alert::new(AlertKind::Injury, "Starter is Out", "Bijan Robinson — Out", "fp-1")
    }

    #[test]
    fn discord_payload_has_embed_shape() {
        let v = discord_payload(&alert());
        let embed = &v["embeds"][0];
        assert!(embed["title"].as_str().unwrap().contains("Starter is Out"));
        assert_eq!(embed["description"], "Bijan Robinson — Out");
        assert_eq!(embed["color"], 0xE0_5252);
    }

    #[test]
    fn discord_limits_are_respected() {
        let long = "x".repeat(9000);
        let v = discord_payload(&Alert::new(AlertKind::Trade, &long, &long, "fp"));
        assert!(v["embeds"][0]["title"].as_str().unwrap().chars().count() <= 256);
        assert!(v["embeds"][0]["description"].as_str().unwrap().chars().count() <= 4096);
    }

    #[test]
    fn truncate_does_not_split_multibyte_chars() {
        // Would panic under byte slicing.
        let s = "é".repeat(50);
        assert_eq!(truncate(&s, 10).chars().count(), 10);
    }

    #[test]
    fn json_format_serializes_alert_verbatim() {
        let v = serde_json::to_value(alert()).unwrap();
        assert_eq!(v["kind"], "Injury");
        assert_eq!(v["fingerprint"], "fp-1");
    }

    #[test]
    fn notifier_is_none_without_a_url() {
        let cfg = NotifyConfig::default();
        assert!(Notifier::new(&cfg).unwrap().is_none());
    }
}
