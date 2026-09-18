//! Egui desktop GUI (feature `gui`).

use crate::anthropic::Anthropic;
use crate::anthropic::AiFeature;
use crate::api::LeagueSession;
use crate::config::Config;
use crate::draft::{DraftManager, DraftSuggestion};
use crate::lineup;
use crate::notify::{Alert, AlertKind, Notifier};
use crate::scheduler::{AppData, Scheduler};
use crate::strategy::Strategy;
use crate::trade::{self, TradeAnalysis};
use crate::types::*;
use crate::waiver::{self, WaiverReport};
use eframe::egui;
use parking_lot::Mutex;
use std::sync::Arc;
use std::time::Duration;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Tab {
    Roster,
    Lineup,
    Waiver,
    Trade,
    Trending,
    Activity,
    Draft,
    Matchup,
    League,
    News,
    Settings,
}

const ALL_TABS: &[(Tab, &str)] = &[
    (Tab::Roster, "Roster"),
    (Tab::Lineup, "Lineup"),
    (Tab::Waiver, "Waiver"),
    (Tab::Trade, "Trades"),
    (Tab::Trending, "Trending"),
    (Tab::Activity, "Activity"),
    (Tab::Draft, "Draft"),
    (Tab::Matchup, "Matchup"),
    (Tab::League, "League"),
    (Tab::News, "News"),
    (Tab::Settings, "Settings"),
];

pub struct GuiApp {
    rt: tokio::runtime::Handle,
    session: Arc<LeagueSession>,
    anthropic: Arc<Anthropic>,
    scheduler: Arc<Scheduler>,
    strategy: Strategy,
    tab: Tab,
    status: Arc<Mutex<String>>,
    lineup: Arc<Mutex<Option<Lineup>>>,
    waiver: Arc<Mutex<Option<WaiverReport>>>,
    trade: Arc<Mutex<Option<TradeAnalysis>>>,
    draft_sugg: Arc<Mutex<Option<DraftSuggestion>>>,
    busy: Arc<Mutex<std::collections::HashSet<String>>>,
    trade_partner: String,
    trade_send: String,
    trade_receive: String,
    logo_tex: Option<egui::TextureHandle>,
    /// Live-editable copy of config.yaml backing the Settings tab.
    cfg: Config,
    /// Context files edited as one path per line, flattened back on save.
    context_files_text: String,
    leagues: Arc<Mutex<Vec<DiscoveredLeague>>>,
    settings_msg: Arc<Mutex<String>>,
    /// Headshots, shared by every tab that lists players.
    images: Arc<crate::images::ImageCache>,
    /// Player whose detail window is open. Behind a Mutex because the list
    /// renderers take &self.
    selected: Arc<Mutex<Option<Player>>>,
    /// Reveals the API key field, which is masked by default.
    show_api_key: bool,
    /// AI explanations of why a player is trending, keyed by player id.
    trend_why: Arc<Mutex<std::collections::HashMap<String, String>>>,
    /// Result of the league-wide trade scan on the Trades tab.
    trade_scan: Arc<Mutex<Option<String>>>,
    /// Parsed trade ideas backing the visual cards.
    trade_ideas: Arc<Mutex<Vec<trade::TradeIdea>>>,
    /// Suggestion controls.
    trade_count: usize,
    trade_multi: bool,
    trade_horizon: trade::Horizon,
    trade_send_hint: String,
    trade_want: Vec<String>,
}

/// Models offered in the settings dropdown. The field stays free-text so a
/// newer id can be used without waiting on a release.
const MODELS: &[&str] = &[
    "claude-opus-4-8",
    "claude-sonnet-4-6",
    "claude-haiku-4-5",
];

/// Mutable handle on one feature's backend setting.
fn feature_slot(f: &mut crate::config::FeatureBackends, feat: AiFeature) -> &mut String {
    match feat {
        AiFeature::Lineup => &mut f.lineup,
        AiFeature::Waiver => &mut f.waiver,
        AiFeature::Trade => &mut f.trade,
        AiFeature::Draft => &mut f.draft,
        AiFeature::Trending => &mut f.trending,
        AiFeature::Daemon => &mut f.daemon,
    }
}

/// "3rd", "11th" — ranks read better than bare numbers next to a total.
fn ordinal(n: u32) -> String {
    let suffix = match (n % 10, n % 100) {
        (_, 11..=13) => "th",
        (1, _) => "st",
        (2, _) => "nd",
        (3, _) => "rd",
        _ => "th",
    };
    format!("{n}{suffix}")
}

/// Green for the top third of the league, red for the bottom third.
fn rank_colour(rank: u32, teams: u32) -> egui::Color32 {
    if teams < 3 {
        return egui::Color32::GRAY;
    }
    if rank * 3 <= teams {
        egui::Color32::LIGHT_GREEN
    } else if rank * 3 > teams * 2 {
        egui::Color32::LIGHT_RED
    } else {
        egui::Color32::from_rgb(220, 220, 120)
    }
}

/// Radar plot of starter rank per position.
///
/// Plots rank rather than raw points: positions score on wildly different
/// scales (a QB outscores a kicker every week), so a points-based radar would
/// just show the scoring system rather than the team. Outer edge is best in
/// the league, centre is last.
fn radar_chart(ui: &mut egui::Ui, ranks: &[crate::league::PosRank]) {
    const SIZE: f32 = 260.0;
    let (resp, painter) =
        ui.allocate_painter(egui::vec2(SIZE, SIZE), egui::Sense::hover());
    let rect = resp.rect;
    let centre = rect.center();
    // Leave room for the position labels around the outside.
    let radius = SIZE / 2.0 - 26.0;
    let n = ranks.len();
    if n < 3 {
        return;
    }

    // Angle for axis i, starting at the top and going clockwise.
    let angle = |i: usize| -> f32 {
        std::f32::consts::TAU * (i as f32) / (n as f32) - std::f32::consts::FRAC_PI_2
    };
    let at = |i: usize, r: f32| -> egui::Pos2 {
        let a = angle(i);
        egui::pos2(centre.x + r * a.cos(), centre.y + r * a.sin())
    };

    // Grid rings.
    for step in 1..=4 {
        let r = radius * step as f32 / 4.0;
        let ring: Vec<egui::Pos2> = (0..n).map(|i| at(i, r)).collect();
        painter.add(egui::Shape::closed_line(
            ring,
            egui::Stroke::new(1.0_f32, BRAND_STROKE),
        ));
    }
    // Spokes and labels.
    for (i, r) in ranks.iter().enumerate() {
        painter.line_segment([centre, at(i, radius)], egui::Stroke::new(1.0_f32, BRAND_STROKE));
        let label_pos = at(i, radius + 15.0);
        painter.text(
            label_pos,
            egui::Align2::CENTER_CENTER,
            r.position.to_string(),
            egui::FontId::proportional(12.0),
            BRAND_TEXT,
        );
    }

    // The team's shape.
    let pts: Vec<egui::Pos2> = ranks
        .iter()
        .enumerate()
        .map(|(i, r)| at(i, radius * r.starter_score().clamp(0.0, 1.0)))
        .collect();
    painter.add(egui::Shape::convex_polygon(
        pts.clone(),
        BRAND_PURPLE.gamma_multiply(0.35),
        egui::Stroke::new(2.0_f32, BRAND_PURPLE),
    ));
    for p in &pts {
        painter.circle_filled(*p, 3.0, BRAND_PURPLE);
    }

    resp.on_hover_text("Outer edge = best in league at that position, centre = last.");
}

/// Starters in slot order, bench and IR excluded.
fn starters(r: &Roster) -> Vec<&Player> {
    r.players.iter().filter(|p| p.roster_slot.is_starter_slot()).collect()
}

/// Chance of winning given a projected margin, as a percentage.
///
/// Weekly fantasy scores are noisy enough that a projected edge is far from
/// decisive; a ~26 point standard deviation on the margin is the usual
/// rule of thumb for a full-roster head-to-head.
fn win_probability(margin: f32) -> f32 {
    const SIGMA: f32 = 26.0;
    50.0 * (1.0 + erf(margin / (SIGMA * std::f32::consts::SQRT_2)))
}

/// Abramowitz & Stegun 7.1.26 — plenty accurate for a win-probability bar.
/// Computed in f64 so the published coefficients keep their precision.
fn erf(x: f32) -> f32 {
    let x = x as f64;
    let sign = if x < 0.0 { -1.0 } else { 1.0 };
    let x = x.abs();
    let t = 1.0 / (1.0 + 0.327_591_1 * x);
    let y = 1.0
        - (((((1.061_405_429 * t - 1.453_152_027) * t) + 1.421_413_741) * t - 0.284_496_736) * t
            + 0.254_829_592)
            * t
            * (-x * x).exp();
    (sign * y) as f32
}

// Palette from the Claude-designed logo (assets/logo-mark.svg).
const BRAND_BG: egui::Color32 = egui::Color32::from_rgb(0x23, 0x25, 0x32);
const BRAND_BG_LIGHT: egui::Color32 = egui::Color32::from_rgb(0x2b, 0x2d, 0x3a);
const BRAND_STROKE: egui::Color32 = egui::Color32::from_rgb(0x4a, 0x4d, 0x5a);
const BRAND_PURPLE: egui::Color32 = egui::Color32::from_rgb(0x91, 0x84, 0xd9);
const BRAND_TEXT: egui::Color32 = egui::Color32::from_rgb(0xe9, 0xe9, 0xed);

/// Dark theme matching the logo palette.
pub fn brand_visuals() -> egui::Visuals {
    let mut v = egui::Visuals::dark();
    v.panel_fill = BRAND_BG;
    v.window_fill = BRAND_BG;
    v.extreme_bg_color = egui::Color32::from_rgb(0x1b, 0x1d, 0x28);
    v.faint_bg_color = BRAND_BG_LIGHT;
    v.widgets.noninteractive.bg_fill = BRAND_BG;
    v.widgets.noninteractive.bg_stroke = egui::Stroke::new(1.0_f32, BRAND_STROKE);
    v.widgets.inactive.bg_fill = BRAND_BG_LIGHT;
    v.widgets.hovered.bg_fill = BRAND_STROKE;
    v.widgets.active.bg_fill = BRAND_PURPLE;
    v.selection.bg_fill = BRAND_PURPLE.linear_multiply(0.35);
    v.selection.stroke = egui::Stroke::new(1.0_f32, BRAND_PURPLE);
    v.hyperlink_color = BRAND_PURPLE;
    v.override_text_color = Some(BRAND_TEXT);
    v
}

impl GuiApp {
    fn data(&self) -> AppData {
        self.scheduler.data.read().clone()
    }
    fn is_busy(&self, key: &str) -> bool {
        self.busy.lock().contains(key)
    }
}

impl GuiApp {
    /// Lazily upload the embedded logo PNG as an egui texture.
    fn logo(&mut self, ctx: &egui::Context) -> Option<egui::TextureHandle> {
        if self.logo_tex.is_none() {
            let icon =
                eframe::icon_data::from_png_bytes(include_bytes!("../assets/icon-256.png")).ok()?;
            let img = egui::ColorImage::from_rgba_unmultiplied(
                [icon.width as usize, icon.height as usize],
                &icon.rgba,
            );
            self.logo_tex = Some(ctx.load_texture("logo", img, egui::TextureOptions::LINEAR));
        }
        self.logo_tex.clone()
    }
}

impl eframe::App for GuiApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        ctx.request_repaint_after(Duration::from_secs(1));
        let logo = self.logo(ctx);
        egui::TopBottomPanel::top("top").show(ctx, |ui| {
            ui.horizontal(|ui| {
                if let Some(tex) = &logo {
                    ui.add(egui::Image::new(tex).fit_to_exact_size(egui::vec2(28.0, 28.0)));
                }
                ui.label(
                    egui::RichText::new("SLEEPER")
                        .size(17.0)
                        .strong()
                        .color(BRAND_TEXT),
                );
                ui.add_space(-4.0);
                ui.label(
                    egui::RichText::new("AGENT")
                        .size(17.0)
                        .strong()
                        .color(BRAND_PURPLE),
                );
                ui.separator();
                let data = self.data();
                ui.label(format!("week {}", data.week));
                ui.separator();
                let mut s = self.strategy;
                egui::ComboBox::from_id_salt("strategy")
                    .selected_text(s.label())
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut s, Strategy::Conservative, "Conservative");
                        ui.selectable_value(&mut s, Strategy::Balanced, "Balanced");
                        ui.selectable_value(&mut s, Strategy::HighStakes, "High Stakes");
                    });
                self.strategy = s;
                ui.separator();
                if ui.button("Refresh now").clicked() {
                    self.scheduler.poke();
                }
                if let Some(t) = data.last_refresh {
                    ui.label(format!("refreshed {}s ago", t.elapsed().as_secs()));
                }
                if let Some(err) = &data.last_error {
                    ui.colored_label(egui::Color32::RED, err);
                }
            });
            ui.horizontal(|ui| {
                for (t, label) in ALL_TABS {
                    if ui.selectable_label(self.tab == *t, *label).clicked() {
                        self.tab = *t;
                    }
                }
            });
        });
        egui::TopBottomPanel::bottom("status").show(ctx, |ui| {
            ui.label(self.status.lock().clone());
        });
        // Right-hand detail panel, opened by clicking any player. Declared
        // before the central panel so egui gives it the space first.
        if self.selected.lock().is_some() {
            egui::SidePanel::right("player_detail")
                .resizable(true)
                .default_width(380.0)
                .width_range(300.0..=560.0)
                .show(ctx, |ui| self.render_player_detail(ui, ctx));
        }
        egui::CentralPanel::default().show(ctx, |ui| match self.tab {
            Tab::Roster => self.render_roster(ui, ctx),
            Tab::Matchup => self.render_matchup(ui, ctx),
            Tab::League => self.render_league(ui, ctx),
            Tab::Lineup => self.render_lineup(ui, ctx),
            Tab::Waiver => self.render_waiver(ui, ctx),
            Tab::Trade => self.render_trade(ui, ctx),
            Tab::Trending => self.render_trending(ui, ctx),
            Tab::Activity => self.render_activity(ui),
            Tab::Draft => self.render_draft(ui, ctx),
            Tab::News => self.render_news(ui),
            Tab::Settings => self.render_settings(ui, ctx),
        });
    }
}

impl GuiApp {
    fn render_roster(&self, ui: &mut egui::Ui, ctx: &egui::Context) {
        let data = self.data();
        let Some(r) = data.roster else {
            ui.label("Waiting for first refresh…");
            return;
        };
        ui.label(format!(
            "{} ({}-{}-{}, PF {:.1}, PA {:.1})",
            r.team_name, r.wins, r.losses, r.ties, r.points_for, r.points_against
        ));
        ui.separator();
        egui::ScrollArea::vertical().show(ui, |ui| {
            egui::Grid::new("roster").num_columns(6).striped(true).show(ui, |ui| {
                for h in ["Slot", "Player", "Pos", "Team", "Status", "Proj"] {
                    ui.strong(h);
                }
                ui.end_row();
                for p in &r.players {
                    let color = match p.status {
                        PlayerStatus::Out | PlayerStatus::IR | PlayerStatus::Suspended => {
                            egui::Color32::LIGHT_RED
                        }
                        PlayerStatus::Doubtful => egui::Color32::YELLOW,
                        PlayerStatus::Questionable => egui::Color32::from_rgb(220, 220, 120),
                        _ => ui.style().visuals.text_color(),
                    };
                    ui.colored_label(color, p.roster_slot.to_string());
                    self.player_cell(ui, ctx, p, color);
                    ui.colored_label(color, p.position.to_string());
                    ui.colored_label(color, &p.team);
                    ui.colored_label(color, p.status.to_string());
                    ui.colored_label(color, format!("{:.1}", p.projected_points));
                    ui.end_row();
                }
            });
        });
    }


    // -- player chrome ------------------------------------------------------

    /// A player's headshot at `size`, or a neutral placeholder while it loads
    /// (or permanently, for team defenses, which have no portrait).
    fn headshot(&self, ui: &mut egui::Ui, ctx: &egui::Context, player_id: &str, size: f32) {
        match self.images.texture(ctx, player_id) {
            Some(tex) => {
                ui.add(egui::Image::new(egui::load::SizedTexture::new(
                    tex.id(),
                    egui::vec2(size, size),
                )));
            }
            None => {
                let (rect, _) =
                    ui.allocate_exact_size(egui::vec2(size, size), egui::Sense::hover());
                ui.painter().circle_filled(rect.center(), size / 2.0, BRAND_BG_LIGHT);
            }
        }
    }

    /// Headshot plus clickable name. Opens the detail window when clicked, so
    /// every list of players behaves the same way.
    fn player_cell(
        &self,
        ui: &mut egui::Ui,
        ctx: &egui::Context,
        p: &Player,
        color: egui::Color32,
    ) {
        ui.horizontal(|ui| {
            self.headshot(ui, ctx, &p.id, 22.0);
            let resp = ui.add(
                egui::Label::new(egui::RichText::new(&p.name).color(color))
                    .sense(egui::Sense::click()),
            );
            if resp.on_hover_text("Click for player details").clicked() {
                *self.selected.lock() = Some(p.clone());
            }
        });
    }

    /// A clickable name for lists that only carry a name (the draft board).
    /// Falls back to a plain label when the player cannot be resolved to a
    /// roster entry, so an undrafted or unknown name simply is not a link.
    fn player_name_link(&self, ui: &mut egui::Ui, text: &str, name: &str, data: &AppData) {
        let found = data
            .all_rosters
            .iter()
            .flat_map(|r| r.players.iter())
            .find(|p| p.name == name)
            .cloned();
        match found {
            Some(p) => {
                let resp = ui.add(egui::Label::new(text).sense(egui::Sense::click()));
                if resp.on_hover_text("Click for player details").clicked() {
                    *self.selected.lock() = Some(p);
                }
            }
            None => {
                ui.label(text);
            }
        }
    }

    /// One trade proposal, rendered as a card rather than prose.
    fn trade_card(
        &self,
        ui: &mut egui::Ui,
        ctx: &egui::Context,
        index: usize,
        idea: &trade::TradeIdea,
    ) {
        let data = self.data();
        egui::Frame::group(ui.style()).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new(format!("#{}", index + 1)).weak());
                ui.strong(if idea.headline.is_empty() {
                    "Trade idea".to_string()
                } else {
                    idea.headline.clone()
                });
                if let Some(shape) = idea.shape_label() {
                    ui.label(egui::RichText::new(shape).small().color(BRAND_PURPLE));
                }
            });
            ui.separator();

            for (n, step) in idea.steps.iter().enumerate() {
                if idea.is_multi_tier() {
                    ui.label(
                        egui::RichText::new(format!("Step {} · with {}", n + 1, step.partner))
                            .small()
                            .strong(),
                    );
                } else {
                    ui.label(egui::RichText::new(format!("With {}", step.partner)).small().strong());
                }
                ui.horizontal_top(|ui| {
                    ui.vertical(|ui| {
                        ui.label(egui::RichText::new("You send").small().weak());
                        for name in &step.send {
                            self.trade_chip(ui, ctx, name, &data, egui::Color32::LIGHT_RED);
                        }
                    });
                    ui.add_space(8.0);
                    ui.vertical(|ui| {
                        ui.add_space(14.0);
                        ui.label("→");
                    });
                    ui.add_space(8.0);
                    ui.vertical(|ui| {
                        ui.label(egui::RichText::new("You get").small().weak());
                        for name in &step.receive {
                            self.trade_chip(ui, ctx, name, &data, egui::Color32::LIGHT_GREEN);
                        }
                    });
                });
                if !step.why.is_empty() {
                    ui.label(egui::RichText::new(&step.why).small().weak());
                }
                ui.add_space(4.0);
            }

            // For a chain, spell out what actually ends up on the roster —
            // the per-step lists include players who are only passing through.
            if idea.is_chained() {
                ui.separator();
                ui.label(egui::RichText::new("Net effect").small().strong());
                ui.horizontal_wrapped(|ui| {
                    ui.label(egui::RichText::new("Out:").small().weak());
                    for name in idea.net_send() {
                        self.trade_chip(ui, ctx, &name, &data, egui::Color32::LIGHT_RED);
                    }
                    ui.label(egui::RichText::new("In:").small().weak());
                    for name in idea.net_receive() {
                        self.trade_chip(ui, ctx, &name, &data, egui::Color32::LIGHT_GREEN);
                    }
                });
            }

            if !idea.why.is_empty() {
                ui.add_space(4.0);
                ui.label(&idea.why);
            }
            if !idea.risk.is_empty() {
                ui.label(
                    egui::RichText::new(format!("Risk: {}", idea.risk))
                        .small()
                        .color(egui::Color32::YELLOW),
                );
            }
        });
        ui.add_space(6.0);
    }

    /// A player in a trade card: headshot and clickable name when the name
    /// resolves to a real roster entry, and visibly flagged when it does not —
    /// a name the model invented should not look like a real player.
    fn trade_chip(
        &self,
        ui: &mut egui::Ui,
        ctx: &egui::Context,
        name: &str,
        data: &AppData,
        colour: egui::Color32,
    ) {
        let found = data
            .all_rosters
            .iter()
            .flat_map(|r| r.players.iter())
            .find(|p| p.name.eq_ignore_ascii_case(name))
            .cloned();
        match found {
            Some(p) => {
                ui.horizontal(|ui| {
                    self.headshot(ui, ctx, &p.id, 22.0);
                    let resp = ui.add(
                        egui::Label::new(egui::RichText::new(&p.name).color(colour))
                            .sense(egui::Sense::click()),
                    );
                    if resp.on_hover_text("Click for player details").clicked() {
                        *self.selected.lock() = Some(p.clone());
                    }
                    ui.label(
                        egui::RichText::new(format!("{} {} · {:.1}", p.position, p.team, p.projected_points))
                            .small()
                            .weak(),
                    );
                });
            }
            None => {
                ui.label(
                    egui::RichText::new(format!("{name}  (not on any roster)"))
                        .color(egui::Color32::YELLOW)
                        .italics(),
                )
                .on_hover_text(
                    "This name does not match any player in the league — treat the \
                     suggestion with suspicion.",
                );
            }
        }
    }

    /// Per-row "why is this trending?" button.
    fn trend_why_button(
        &self,
        ui: &mut egui::Ui,
        ctx: &egui::Context,
        t: &TrendingPlayer,
        dir: TrendDirection,
    ) {
        let busy = self.is_busy(&format!("why-{}", t.player.id));
        if busy {
            ui.spinner();
            return;
        }
        let done = self.trend_why.lock().contains_key(&t.player.id);
        let label = if done { "↻" } else { "Why?" };
        if ui
            .small_button(label)
            .on_hover_text("Ask Claude why this player is trending")
            .clicked()
        {
            self.spawn_trend_why(ctx.clone(), t.player.clone(), t.count, dir);
        }
    }

    /// The explanation, once it has arrived.
    fn trend_why_body(&self, ui: &mut egui::Ui, player_id: &str) {
        let Some(text) = self.trend_why.lock().get(player_id).cloned() else {
            return;
        };
        ui.indent(("why", player_id), |ui| {
            ui.label(egui::RichText::new(text).small());
            if ui.small_button("Dismiss").clicked() {
                self.trend_why.lock().remove(player_id);
            }
        });
        ui.add_space(4.0);
    }

    /// Detail for the selected player, rendered into the right-hand panel.
    fn render_player_detail(&self, ui: &mut egui::Ui, ctx: &egui::Context) {
        let Some(p) = self.selected.lock().clone() else {
            return;
        };
        let data = self.data();

        ui.horizontal(|ui| {
            ui.heading("Player");
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button("✕").on_hover_text("Close").clicked() {
                    *self.selected.lock() = None;
                }
            });
        });
        ui.separator();

        egui::ScrollArea::vertical().id_salt("detail_scroll").show(ui, |ui| {
            ui.vertical_centered(|ui| {
                self.headshot(ui, ctx, &p.id, 128.0);
                ui.heading(&p.name);
                ui.label(format!("{} · {} · {}", p.position, p.team, p.status));
            });

            ui.add_space(8.0);
            let scoring = data.settings.as_ref().map(|s| s.scoring.as_str()).unwrap_or("?");
            egui::Grid::new("detail_head").num_columns(2).spacing([10.0, 4.0]).show(ui, |ui| {
                ui.label("Projected");
                ui.label(
                    egui::RichText::new(format!("{:.1} pts ({scoring})", p.projected_points))
                        .color(BRAND_PURPLE)
                        .strong(),
                );
                ui.end_row();
                if p.avg_points > 0.0 {
                    ui.label("Season avg");
                    ui.label(format!("{:.1} pts/game", p.avg_points));
                    ui.end_row();
                }
                ui.label("Roster slot");
                ui.label(p.roster_slot.to_string());
                ui.end_row();
                if let Some(o) = &p.opponent {
                    ui.label("This week");
                    ui.label(o.clone());
                    ui.end_row();
                }
                if let Some(b) = p.bye_week {
                    ui.label("Bye week");
                    ui.label(b.to_string());
                    ui.end_row();
                }
                // Who in the league holds him — the first thing you want to
                // know before proposing anything.
                let owner = data
                    .all_rosters
                    .iter()
                    .find(|r| r.players.iter().any(|q| q.id == p.id))
                    .map(|r| r.team_name.clone());
                ui.label("Rostered by");
                ui.label(owner.unwrap_or_else(|| "Free agent".into()));
                ui.end_row();

                // League-wide add/drop interest, when he appears in either feed.
                let add = data.trending_add.iter().find(|t| t.player.id == p.id).map(|t| t.count);
                let drop = data.trending_drop.iter().find(|t| t.player.id == p.id).map(|t| t.count);
                if let Some(c) = add {
                    ui.label("Trending");
                    ui.label(
                        egui::RichText::new(format!("+{c} adds (24h)"))
                            .color(egui::Color32::LIGHT_GREEN),
                    );
                    ui.end_row();
                }
                if let Some(c) = drop {
                    ui.label("Trending");
                    ui.label(
                        egui::RichText::new(format!("-{c} drops (24h)"))
                            .color(egui::Color32::LIGHT_RED),
                    );
                    ui.end_row();
                }
            });

            ui.add_space(8.0);
            ui.separator();
            ui.strong("Upcoming games");
            let games =
                crate::player_detail::upcoming_for_team(&data.schedule, &p.team, data.week, 5);
            if games.is_empty() {
                ui.weak("No scheduled games found.");
            } else {
                egui::Grid::new("detail_sched").num_columns(3).striped(true).show(ui, |ui| {
                    for g in games {
                        ui.label(format!("Wk {}", g.week));
                        ui.label(g.label());
                        ui.label(
                            egui::RichText::new(g.date.clone().unwrap_or_default()).small().weak(),
                        );
                        ui.end_row();
                    }
                });
            }

            ui.add_space(8.0);
            ui.separator();
            ui.strong("Against projection");
            match data.perf.get(&p.id) {
                Some(rec) if rec.games > 0 => {
                    // Always name the season: during the preseason this is
                    // last year's record, not this year's.
                    ui.label(
                        egui::RichText::new(format!(
                            "{} season · {} games",
                            data.perf.season, rec.games
                        ))
                        .small()
                        .weak(),
                    );
                    let pct = rec.beat_pct();
                    let colour = if pct >= 50.0 {
                        egui::Color32::LIGHT_GREEN
                    } else {
                        egui::Color32::LIGHT_RED
                    };
                    ui.label(
                        egui::RichText::new(format!(
                            "Beat projection {pct:.0}% of games ({}/{})",
                            rec.beat, rec.games
                        ))
                        .color(colour)
                        .strong(),
                    );
                    ui.add(
                        egui::ProgressBar::new((pct / 100.0).clamp(0.0, 1.0))
                            .desired_width(220.0)
                            .text(format!("{pct:.0}%")),
                    );
                    ui.label(format!(
                        "Average {:+.1} vs projection ({:.1} actual, {:.1} projected)",
                        rec.avg_diff(),
                        rec.avg_actual(),
                        rec.avg_proj()
                    ));
                    ui.label(
                        egui::RichText::new(format!(
                            "Best {:+.1} · worst {:+.1}",
                            rec.best_diff, rec.worst_diff
                        ))
                        .small()
                        .weak(),
                    );

                    let totals =
                        crate::player_detail::notable_stats(&rec.totals, &p.position.to_string());
                    if !totals.is_empty() {
                        ui.add_space(6.0);
                        ui.strong(format!("{} totals", data.perf.season));
                        egui::Grid::new("detail_stats").num_columns(2).striped(true).show(
                            ui,
                            |ui| {
                                for (label, v) in totals {
                                    ui.label(label);
                                    ui.label(format!("{v:.0}"));
                                    ui.end_row();
                                }
                            },
                        );
                    }
                }
                _ => {
                    ui.weak("No completed games on record yet.");
                }
            }

            // Headlines mentioning him, from the shared news feed as well as
            // anything already attached to the player record.
            let mut headlines: Vec<String> = p.news.clone();
            for n in &data.news {
                if n.title.contains(&p.name) && !headlines.iter().any(|h| h == &n.title) {
                    headlines.push(n.title.clone());
                }
            }
            ui.add_space(8.0);
            ui.separator();
            ui.strong("News");
            if headlines.is_empty() {
                ui.weak("No recent headlines mention this player.");
            } else {
                for h in headlines.iter().take(8) {
                    ui.label(format!("• {h}"));
                }
            }
        });
    }


    // -- league comparison --------------------------------------------------

    /// How this roster stacks up against the league, position by position.
    fn render_league(&self, ui: &mut egui::Ui, ctx: &egui::Context) {
        let data = self.data();
        let Some(me) = data.roster.clone() else {
            ui.label("Waiting for first refresh…");
            return;
        };
        if data.all_rosters.len() < 2 {
            ui.label("Need the full league loaded to compare rosters.");
            return;
        }
        let ranks = crate::league::rank_team(&data.all_rosters, &me.team_name);

        ui.heading(format!("{} vs the league", me.team_name));
        ui.label(
            egui::RichText::new(format!(
                "{} teams · ranked on projected points at each position",
                data.all_rosters.len()
            ))
            .small()
            .weak(),
        );
        ui.separator();

        egui::ScrollArea::vertical().id_salt("league_scroll").show(ui, |ui| {
            ui.horizontal_top(|ui| {
                radar_chart(ui, &ranks);
                ui.add_space(12.0);
                ui.vertical(|ui| {
                    // The actionable summary: where to buy and where to sell.
                    let weak: Vec<&crate::league::PosRank> =
                        ranks.iter().filter(|r| r.is_weakness()).collect();
                    let strong: Vec<&crate::league::PosRank> =
                        ranks.iter().filter(|r| r.is_strength()).collect();

                    ui.strong("Where to improve");
                    if weak.is_empty() {
                        ui.weak("No position sits in the bottom third of the league.");
                    } else {
                        for r in &weak {
                            ui.label(
                                egui::RichText::new(format!(
                                    "{}  —  {} of {}, {:+.1} vs league average",
                                    r.position,
                                    ordinal(r.rank_starters),
                                    r.teams,
                                    r.vs_average()
                                ))
                                .color(egui::Color32::LIGHT_RED),
                            );
                        }
                    }
                    ui.add_space(8.0);
                    ui.strong("Surplus to trade from");
                    if strong.is_empty() {
                        ui.weak("No position sits in the top third of the league.");
                    } else {
                        for r in &strong {
                            ui.label(
                                egui::RichText::new(format!(
                                    "{}  —  {} of {}, {:+.1} vs league average",
                                    r.position,
                                    ordinal(r.rank_starters),
                                    r.teams,
                                    r.vs_average()
                                ))
                                .color(egui::Color32::LIGHT_GREEN),
                            );
                        }
                    }
                });
            });

            ui.add_space(12.0);
            ui.separator();
            egui::Grid::new("league_grid").num_columns(6).striped(true).show(ui, |ui| {
                for h in ["Pos", "Starters", "Rank", "vs avg", "Bench", "Bench rank"] {
                    ui.strong(h);
                }
                ui.end_row();
                for r in &ranks {
                    ui.label(r.position.to_string());
                    ui.label(format!("{:.1}", r.starters));
                    ui.colored_label(rank_colour(r.rank_starters, r.teams), ordinal(r.rank_starters));
                    let d = r.vs_average();
                    ui.colored_label(
                        if d >= 0.0 {
                            egui::Color32::LIGHT_GREEN
                        } else {
                            egui::Color32::LIGHT_RED
                        },
                        format!("{d:+.1}"),
                    );
                    ui.label(format!("{:.1}", r.bench));
                    ui.colored_label(rank_colour(r.rank_bench, r.teams), ordinal(r.rank_bench));
                    ui.end_row();
                }
            });

            ui.add_space(12.0);
            ui.separator();
            ui.strong("Your starters by position");
            for r in &ranks {
                let players = crate::league::starters_at(&me, r.position);
                if players.is_empty() {
                    continue;
                }
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new(r.position.to_string()).strong());
                    for p in players {
                        self.player_cell(ui, ctx, p, ui.style().visuals.text_color());
                        ui.label(
                            egui::RichText::new(format!("{:.1}", p.projected_points)).weak(),
                        );
                    }
                });
            }
        });
    }

    // -- weekly matchup -----------------------------------------------------

    /// Head-to-head view of this week's fantasy matchup.
    fn render_matchup(&self, ui: &mut egui::Ui, ctx: &egui::Context) {
        let data = self.data();
        let Some(me) = data.roster.clone() else {
            ui.label("Waiting for first refresh…");
            return;
        };
        let Some(m) = data
            .matchups
            .iter()
            .find(|m| m.home_team == me.team_name || m.away_team == me.team_name)
            .cloned()
        else {
            ui.label(format!("No matchup scheduled for week {}.", data.week));
            return;
        };

        let i_am_home = m.home_team == me.team_name;
        let (my_proj, opp_proj, opp_name, my_score, opp_score) = if i_am_home {
            (m.home_projected, m.away_projected, m.away_team.clone(), m.home_score, m.away_score)
        } else {
            (m.away_projected, m.home_projected, m.home_team.clone(), m.away_score, m.home_score)
        };
        let opp = data.all_rosters.iter().find(|r| r.team_name == opp_name).cloned();

        ui.horizontal(|ui| {
            ui.heading(format!("Week {} matchup", data.week));
            ui.add_space(8.0);
            if ui.button("Refresh").clicked() {
                self.scheduler.poke();
            }
        });
        ui.separator();

        // Score line: the headline comparison.
        let diff = my_proj - opp_proj;
        let win_pct = win_probability(diff);
        ui.horizontal(|ui| {
            ui.vertical(|ui| {
                ui.label(egui::RichText::new(&me.team_name).strong());
                ui.label(
                    egui::RichText::new(format!("{my_proj:.1}")).size(30.0).color(BRAND_PURPLE),
                );
                ui.label(egui::RichText::new(format!("live {my_score:.1}")).small().weak());
            });
            ui.add_space(18.0);
            ui.vertical(|ui| {
                ui.add_space(10.0);
                ui.label(egui::RichText::new("vs").weak());
            });
            ui.add_space(18.0);
            ui.vertical(|ui| {
                ui.label(egui::RichText::new(&opp_name).strong());
                ui.label(egui::RichText::new(format!("{opp_proj:.1}")).size(30.0));
                ui.label(egui::RichText::new(format!("live {opp_score:.1}")).small().weak());
            });
        });

        ui.add_space(6.0);
        let (verdict, colour) = if diff >= 0.0 {
            (format!("Favoured by {:.1} — {:.0}% to win", diff, win_pct), egui::Color32::LIGHT_GREEN)
        } else {
            (
                format!("Underdog by {:.1} — {:.0}% to win", -diff, win_pct),
                egui::Color32::LIGHT_RED,
            )
        };
        ui.label(egui::RichText::new(verdict).color(colour).strong());
        ui.add(
            egui::ProgressBar::new((win_pct / 100.0).clamp(0.0, 1.0))
                .desired_width(320.0)
                .text(format!("{win_pct:.0}%")),
        );
        ui.label(
            egui::RichText::new(
                "Win chance assumes a ~26 pt standard deviation on the weekly margin.",
            )
            .small()
            .weak(),
        );

        ui.add_space(10.0);
        ui.separator();
        egui::ScrollArea::vertical().show(ui, |ui| {
            egui::Grid::new("matchup_grid").num_columns(5).striped(true).show(ui, |ui| {
                for h in [me.team_name.as_str(), "Proj", "", "Proj", opp_name.as_str()] {
                    ui.strong(h);
                }
                ui.end_row();

                let mine = starters(&me);
                let theirs = opp.as_ref().map(|r| starters(r)).unwrap_or_default();
                let rows = mine.len().max(theirs.len());
                for i in 0..rows {
                    match mine.get(i) {
                        Some(p) => {
                            self.player_cell(ui, ctx, p, ui.style().visuals.text_color());
                            ui.label(format!("{:.1}", p.projected_points));
                        }
                        None => {
                            ui.label("");
                            ui.label("");
                        }
                    }
                    // Slot label sits between the two sides.
                    let slot = mine
                        .get(i)
                        .or_else(|| theirs.get(i))
                        .map(|p| p.roster_slot.to_string())
                        .unwrap_or_default();
                    ui.label(egui::RichText::new(slot).weak());
                    match theirs.get(i) {
                        Some(p) => {
                            ui.label(format!("{:.1}", p.projected_points));
                            self.player_cell(ui, ctx, p, ui.style().visuals.text_color());
                        }
                        None => {
                            ui.label("");
                            ui.label("");
                        }
                    }
                    ui.end_row();
                }
            });
            if opp.is_none() {
                ui.add_space(6.0);
                ui.weak("Opponent roster not loaded yet.");
            }
        });
    }

    fn render_lineup(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        ui.horizontal(|ui| {
            let busy = self.is_busy("lineup");
            if ui.add_enabled(!busy, egui::Button::new("Generate AI lineup")).clicked() {
                self.spawn_lineup(ctx.clone());
            }
            if busy {
                ui.spinner();
            }
        });
        ui.separator();
        if let Some(l) = self.lineup.lock().clone() {
            ui.label(format!("Week {} — projected {:.1}", l.week, l.projected_total));
            egui::ScrollArea::vertical().show(ui, |ui| {
                egui::Grid::new("lineup").num_columns(4).striped(true).show(ui, |ui| {
                    for s in &l.starters {
                        ui.label(s.slot.to_string());
                        match &s.player {
                            Some(p) => {
                                self.player_cell(ui, ctx, p, ui.style().visuals.text_color());
                                ui.label(p.status.to_string());
                                ui.label(format!("{:.1}", p.projected_points));
                            }
                            None => {
                                ui.label("(empty)");
                                ui.label("");
                                ui.label("");
                            }
                        }
                        ui.end_row();
                    }
                });
                ui.separator();
                ui.label(&l.reasoning);
            });
        } else {
            ui.label("No lineup yet.");
        }
    }

    fn render_waiver(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        ui.horizontal(|ui| {
            let busy = self.is_busy("waiver");
            if ui
                .add_enabled(!busy, egui::Button::new("Suggest waiver pickups"))
                .clicked()
            {
                self.spawn_waiver(ctx.clone());
            }
            if busy {
                ui.spinner();
            }
        });
        ui.separator();
        if let Some(r) = self.waiver.lock().clone() {
            egui::ScrollArea::vertical().show(ui, |ui| {
                egui::Grid::new("waiver").num_columns(7).striped(true).show(ui, |ui| {
                    for h in ["#", "Pickup", "Pos/Team", "Proj/wk", "ROS", "Adds/24h", "Drop"] {
                        ui.strong(h);
                    }
                    ui.end_row();
                    for c in &r.candidates {
                        ui.label(c.priority.to_string());
                        self.player_cell(ui, ctx, &c.player, ui.style().visuals.text_color());
                        ui.label(format!("{} {}", c.player.position, c.player.team));
                        ui.label(format!("{:.1}", c.metrics.adjusted_next_week));
                        ui.label(format!("{:.0}", c.metrics.ros_value));
                        ui.label(
                            c.trending_adds
                                .map(|n| n.to_string())
                                .unwrap_or_else(|| "—".into()),
                        );
                        match &c.drop_candidate {
                            Some(d) => {
                                ui.label(format!("{} (Δ{:+.0})", d.player.name, d.net_ros_delta))
                            }
                            None => ui.label("—"),
                        };
                        ui.end_row();
                    }
                });
                ui.separator();
                ui.label(&r.raw);
            });
        } else {
            ui.label("No suggestions yet.");
        }
    }

    fn render_trade(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        // League-wide scan first: it needs no input beyond these controls, so
        // it is the useful starting point when you have no deal in mind.
        ui.strong("Find trades");
        egui::Grid::new("trade_opts").num_columns(2).spacing([12.0, 6.0]).show(ui, |ui| {
            ui.label("Suggestions");
            ui.add(egui::DragValue::new(&mut self.trade_count).range(1..=8));
            ui.end_row();

            ui.label("Judge on");
            ui.horizontal(|ui| {
                for h in [trade::Horizon::ThisWeek, trade::Horizon::RestOfSeason] {
                    if ui.selectable_label(self.trade_horizon == h, h.label()).clicked() {
                        self.trade_horizon = h;
                    }
                }
            });
            ui.end_row();

            ui.label("Trade away");
            ui.add(
                egui::TextEdit::singleline(&mut self.trade_send_hint)
                    .desired_width(260.0)
                    .hint_text("player or position, comma separated (optional)"),
            );
            ui.end_row();

            ui.label("Looking for");
            ui.horizontal_wrapped(|ui| {
                for pos in ["QB", "RB", "WR", "TE", "K", "DST"] {
                    let mut on = self.trade_want.iter().any(|p| p == pos);
                    if ui.toggle_value(&mut on, pos).changed() {
                        if on {
                            self.trade_want.push(pos.to_string());
                        } else {
                            self.trade_want.retain(|p| p != pos);
                        }
                    }
                }
            });
            ui.end_row();

            ui.label("Multi-team");
            ui.checkbox(
                &mut self.trade_multi,
                "Allow chained trades (acquire, then flip on)",
            )
            .on_hover_text(
                "Each extra leg is another manager who has to agree, so most ideas \
                 will still be a single trade.",
            );
            ui.end_row();
        });

        ui.add_space(6.0);
        ui.horizontal(|ui| {
            let scanning = self.is_busy("trade_scan");
            if ui
                .add_enabled(!scanning, egui::Button::new("Scan league for trades"))
                .clicked()
            {
                self.spawn_trade_scan(ctx.clone());
            }
            if scanning {
                ui.spinner();
                ui.label("Comparing every roster…");
            }
            if !self.trade_ideas.lock().is_empty() && ui.button("Clear").clicked() {
                self.trade_ideas.lock().clear();
                *self.trade_scan.lock() = None;
            }
        });

        let ideas = self.trade_ideas.lock().clone();
        if !ideas.is_empty() {
            ui.add_space(6.0);
            egui::ScrollArea::vertical().id_salt("trade_ideas").max_height(420.0).show(
                ui,
                |ui| {
                    for (i, idea) in ideas.iter().enumerate() {
                        self.trade_card(ui, ctx, i, idea);
                    }
                },
            );
        } else if let Some(raw) = self.trade_scan.lock().clone() {
            // Structured parsing failed — show what came back rather than
            // silently reporting nothing.
            ui.add_space(6.0);
            ui.label(
                egui::RichText::new("Could not read that as structured trades:")
                    .small()
                    .color(egui::Color32::YELLOW),
            );
            egui::ScrollArea::vertical().id_salt("scan_raw").max_height(220.0).show(ui, |ui| {
                ui.label(raw);
            });
        }

        ui.add_space(6.0);
        ui.separator();
        ui.strong("Evaluate a specific trade");
        egui::Grid::new("trade_in").num_columns(2).show(ui, |ui| {
            ui.label("Partner team:");
            ui.text_edit_singleline(&mut self.trade_partner);
            ui.end_row();
            ui.label("You send:");
            ui.text_edit_singleline(&mut self.trade_send);
            ui.end_row();
            ui.label("You receive:");
            ui.text_edit_singleline(&mut self.trade_receive);
            ui.end_row();
        });
        let busy = self.is_busy("trade");
        if ui.add_enabled(!busy, egui::Button::new("Analyze trade")).clicked() {
            self.spawn_trade(ctx.clone());
        }
        if busy {
            ui.spinner();
        }
        ui.separator();
        if let Some(a) = self.trade.lock().clone() {
            let color = match a.verdict {
                "ACCEPT" => egui::Color32::LIGHT_GREEN,
                "DECLINE" => egui::Color32::LIGHT_RED,
                _ => egui::Color32::YELLOW,
            };
            ui.horizontal(|ui| {
                ui.heading("Verdict:");
                ui.colored_label(color, a.verdict);
                ui.label(format!(
                    "net ROS {:+.1} | fairness {:.0}%",
                    a.net_ros_delta,
                    a.fairness * 100.0
                ));
            });
            egui::ScrollArea::vertical().show(ui, |ui| {
                ui.heading("You send");
                for m in &a.send {
                    ui.label(m.one_line());
                }
                ui.heading("You receive");
                for m in &a.receive {
                    ui.label(m.one_line());
                }
                ui.separator();
                ui.label(&a.ai_summary);
            });
        }
    }

    fn render_trending(&self, ui: &mut egui::Ui, ctx: &egui::Context) {
        let data = self.data();
        let text = ui.style().visuals.text_color();
        ui.columns(2, |cols| {
            cols[0].heading("Trending ADDS (24h)");
            egui::ScrollArea::vertical()
                .id_salt("adds")
                .show(&mut cols[0], |ui| {
                    for t in &data.trending_add {
                        ui.horizontal(|ui| {
                            ui.label(egui::RichText::new(format!("{:>6}", t.count)).weak());
                            self.player_cell(ui, ctx, &t.player, text);
                            ui.label(format!(
                                "({} {}) proj {:.1}",
                                t.player.position, t.player.team, t.player.projected_points
                            ));
                            self.trend_why_button(ui, ctx, t, TrendDirection::Add);
                        });
                        self.trend_why_body(ui, &t.player.id);
                    }
                });
            cols[1].heading("Trending DROPS (24h)");
            egui::ScrollArea::vertical()
                .id_salt("drops")
                .show(&mut cols[1], |ui| {
                    for t in &data.trending_drop {
                        ui.horizontal(|ui| {
                            ui.label(egui::RichText::new(format!("{:>6}", t.count)).weak());
                            self.player_cell(ui, ctx, &t.player, text);
                            ui.label(format!("({} {})", t.player.position, t.player.team));
                            self.trend_why_button(ui, ctx, t, TrendDirection::Drop);
                        });
                        self.trend_why_body(ui, &t.player.id);
                    }
                });
        });
    }

    fn render_activity(&self, ui: &mut egui::Ui) {
        let data = self.data();
        ui.heading("League activity");
        egui::ScrollArea::vertical().show(ui, |ui| {
            for t in data.transactions.iter().take(50) {
                let adds: Vec<String> = t.adds.iter().map(|(p, tm)| format!("{tm} +{p}")).collect();
                let drops: Vec<String> = t.drops.iter().map(|(p, tm)| format!("{tm} -{p}")).collect();
                ui.label(format!(
                    "wk{} {} [{}] {} {}{}",
                    t.week,
                    t.kind,
                    t.status,
                    adds.join(", "),
                    drops.join(", "),
                    t.waiver_bid.map(|b| format!(" (${b} FAAB)")).unwrap_or_default(),
                ));
            }
            if !data.traded_picks.is_empty() {
                ui.separator();
                ui.heading("Traded picks");
                for p in &data.traded_picks {
                    ui.label(format!(
                        "{} R{}: {} → {}",
                        p.season, p.round, p.original_owner, p.current_owner
                    ));
                }
            }
            if !data.winners_bracket.is_empty() {
                ui.separator();
                ui.heading("Winners bracket");
                for m in &data.winners_bracket {
                    ui.label(format!(
                        "R{} M{}: {} vs {}{}",
                        m.round,
                        m.match_id,
                        m.team1.as_deref().unwrap_or("TBD"),
                        m.team2.as_deref().unwrap_or("TBD"),
                        m.winner.as_deref().map(|w| format!(" → {w}")).unwrap_or_default(),
                    ));
                }
            }
        });
    }

    fn render_draft(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        ui.horizontal(|ui| {
            let busy = self.is_busy("draft");
            if ui
                .add_enabled(!busy, egui::Button::new("Refresh draft & suggest"))
                .clicked()
            {
                self.spawn_draft(ctx.clone());
            }
            if busy {
                ui.spinner();
            }
        });
        ui.separator();
        let data = self.data();
        if let Some(d) = &data.draft {
            ui.label(format!(
                "Pick #{} — round {}/{} — {} teams{}",
                d.current_pick,
                ((d.current_pick.saturating_sub(1) / d.team_count.max(1)) + 1),
                d.total_rounds,
                d.team_count,
                d.on_the_clock_team
                    .as_deref()
                    .map(|t| format!(" — {t} on the clock"))
                    .unwrap_or_default(),
            ));
            for p in d.picks.iter().rev().take(10).rev() {
                let name = p.player_name.as_deref().unwrap_or("?");
                self.player_name_link(
                    ui,
                    &format!("R{}.{} {} → {}", p.round, p.pick_number, p.team_name, name),
                    name,
                    &data,
                );
            }
        } else {
            ui.label("No draft state.");
        }
        if let Some(s) = self.draft_sugg.lock().clone() {
            ui.separator();
            ui.heading("Top 3 candidates");
            for p in &s.picks {
                ui.label(format!(
                    "{}. {} ({}) — {}",
                    p.rank,
                    p.name,
                    p.position.map(|x| x.to_string()).unwrap_or_else(|| "?".into()),
                    p.rationale
                ));
            }
        }
    }

    fn render_news(&self, ui: &mut egui::Ui) {
        let data = self.data();
        egui::ScrollArea::vertical().show(ui, |ui| {
            for n in data.news.iter().take(80) {
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new(format!("[{}]", n.source)).strong());
                    if !n.url.is_empty() {
                        ui.hyperlink_to(&n.title, &n.url);
                    } else {
                        ui.label(&n.title);
                    }
                });
            }
        });
    }

    fn spawn_lineup(&self, ctx: egui::Context) {
        let key = "lineup";
        self.busy.lock().insert(key.to_string());
        let data = self.data();
        let Some(roster) = data.roster else {
            *self.status.lock() = "No roster yet.".into();
            self.busy.lock().remove(key);
            return;
        };
        let settings = data.settings.unwrap_or_default();
        let (week, news, matchups) = (data.week, data.news, data.matchups);
        let (strat, anthropic) = (self.strategy, self.anthropic.clone());
        let (slot, busy, status) = (self.lineup.clone(), self.busy.clone(), self.status.clone());
        self.rt.spawn(async move {
            match lineup::ai_optimize(&anthropic, &roster, &settings, &matchups, &news, strat, week).await {
                Ok(l) => {
                    *status.lock() = format!("Lineup ready — projected {:.1}", l.projected_total);
                    *slot.lock() = Some(l);
                }
                Err(e) => *status.lock() = format!("lineup error: {e}"),
            }
            busy.lock().remove(key);
            ctx.request_repaint();
        });
    }

    fn spawn_waiver(&self, ctx: egui::Context) {
        let key = "waiver";
        self.busy.lock().insert(key.to_string());
        let session = self.session.clone();
        let anthropic = self.anthropic.clone();
        let strat = self.strategy;
        let news = self.data().news;
        let (slot, busy, status) = (self.waiver.clone(), self.busy.clone(), self.status.clone());
        self.rt.spawn(async move {
            match waiver::analyze(&session, &anthropic, strat, &news, 300).await {
                Ok(r) => {
                    *status.lock() = format!("{} waiver candidates.", r.candidates.len());
                    *slot.lock() = Some(r);
                }
                Err(e) => *status.lock() = format!("waiver error: {e}"),
            }
            busy.lock().remove(key);
            ctx.request_repaint();
        });
    }

    /// Ask Claude why one player is trending.
    fn spawn_trend_why(
        &self,
        ctx: egui::Context,
        player: Player,
        count: u64,
        direction: TrendDirection,
    ) {
        let key = format!("why-{}", player.id);
        if !self.busy.lock().insert(key.clone()) {
            return; // already running for this player
        }
        let anthropic = self.anthropic.clone();
        let strat = self.strategy;
        let data = self.data();
        let Some(roster) = data.roster.clone() else {
            self.busy.lock().remove(&key);
            *self.status.lock() = "Roster not loaded yet.".into();
            return;
        };
        let news = data.news;
        let (out, busy, status) =
            (self.trend_why.clone(), self.busy.clone(), self.status.clone());
        let pid = player.id.clone();
        let name = player.name.clone();
        *self.status.lock() = format!("Analysing why {name} is trending…");
        self.rt.spawn(async move {
            match waiver::explain_trending(
                &anthropic, &player, count, direction, &roster, &news, strat,
            )
            .await
            {
                Ok(text) => {
                    *status.lock() = format!("Analysis ready for {name}.");
                    out.lock().insert(pid, text);
                }
                Err(e) => *status.lock() = format!("trending analysis error: {e}"),
            }
            busy.lock().remove(&key);
            ctx.request_repaint();
        });
    }

    /// Scan every roster in the league for trades worth proposing.
    fn spawn_trade_scan(&self, ctx: egui::Context) {
        let key = "trade_scan";
        self.busy.lock().insert(key.to_string());
        let session = self.session.clone();
        let anthropic = self.anthropic.clone();
        let strat = self.strategy;
        let data = self.data();
        let news = data.news;
        let week = data.week;
        let opts = trade::SuggestOptions {
            count: self.trade_count,
            multi_tier: self.trade_multi,
            send_hints: self
                .trade_send_hint
                .split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(String::from)
                .collect(),
            want_positions: self.trade_want.clone(),
            horizon: self.trade_horizon,
            week,
        };
        let (raw_out, ideas_out, busy, status) = (
            self.trade_scan.clone(),
            self.trade_ideas.clone(),
            self.busy.clone(),
            self.status.clone(),
        );
        *self.status.lock() = "Scanning the league for trades…".into();
        self.rt.spawn(async move {
            let result = async {
                let roster = session.my_roster(week).await?;
                let all = session.all_rosters(week).await?;
                trade::suggest_ideas(&anthropic, &roster, &all, strat, &news, &opts).await
            }
            .await;
            match result {
                Ok((ideas, raw)) => {
                    *status.lock() = if ideas.is_empty() {
                        "No trades suggested.".into()
                    } else {
                        format!("{} trade idea(s).", ideas.len())
                    };
                    *ideas_out.lock() = ideas;
                    *raw_out.lock() = Some(raw);
                }
                Err(e) => *status.lock() = format!("trade scan error: {e}"),
            }
            busy.lock().remove(key);
            ctx.request_repaint();
        });
    }

    fn spawn_trade(&self, ctx: egui::Context) {
        let key = "trade";
        let (partner, send_input, recv_input) = (
            self.trade_partner.clone(),
            self.trade_send.clone(),
            self.trade_receive.clone(),
        );
        if partner.trim().is_empty() || send_input.trim().is_empty() || recv_input.trim().is_empty() {
            *self.status.lock() = "Fill partner / send / receive.".into();
            return;
        }
        self.busy.lock().insert(key.to_string());
        let data = self.data();
        let Some(roster) = data.roster.clone() else {
            *self.status.lock() = "No roster yet.".into();
            self.busy.lock().remove(key);
            return;
        };
        let (others, news, strat, anthropic) = (
            data.all_rosters.clone(),
            data.news.clone(),
            self.strategy,
            self.anthropic.clone(),
        );
        let (slot, busy, status) = (self.trade.clone(), self.busy.clone(), self.status.clone());
        self.rt.spawn(async move {
            let split = |s: &str| -> Vec<String> {
                s.split(',').map(|x| x.trim().to_string()).filter(|x| !x.is_empty()).collect()
            };
            match trade::analyze(
                &anthropic,
                &roster,
                &partner,
                &split(&send_input),
                &split(&recv_input),
                &others,
                strat,
                &news,
            )
            .await
            {
                Ok(a) => {
                    *status.lock() =
                        format!("Verdict {} (net ROS {:+.1})", a.verdict, a.net_ros_delta);
                    *slot.lock() = Some(a);
                }
                Err(e) => *status.lock() = format!("trade error: {e}"),
            }
            busy.lock().remove(key);
            ctx.request_repaint();
        });
    }

    fn spawn_draft(&self, ctx: egui::Context) {
        let key = "draft";
        self.busy.lock().insert(key.to_string());
        let session = self.session.clone();
        let anthropic = self.anthropic.clone();
        let news = self.data().news;
        let team = self.data().roster.map(|r| r.team_name).unwrap_or_default();
        let strat = self.strategy;
        let (slot, busy, status) = (self.draft_sugg.clone(), self.busy.clone(), self.status.clone());
        self.rt.spawn(async move {
            let dm = DraftManager {
                session: &session,
                anthropic: &anthropic,
                strategy: strat,
                my_team_name: team,
            };
            let result = async {
                let state = dm.snapshot().await?;
                dm.ask_claude(&state, &news).await
            }
            .await;
            match result {
                Ok(s) => {
                    *status.lock() = format!("{} draft candidates.", s.picks.len());
                    *slot.lock() = Some(s);
                }
                Err(e) => *status.lock() = format!("draft error: {e}"),
            }
            busy.lock().remove(key);
            ctx.request_repaint();
        });
    }
}

impl GuiApp {
    fn render_settings(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        egui::ScrollArea::vertical().show(ui, |ui| {
            ui.heading("Account");
            ui.label(
                egui::RichText::new(format!("Config file: {}", self.cfg.path.display()))
                    .small()
                    .weak(),
            );
            ui.add_space(4.0);

            egui::Grid::new("account_grid").num_columns(2).spacing([12.0, 8.0]).show(ui, |ui| {
                ui.label("Sleeper username");
                ui.horizontal(|ui| {
                    ui.add(
                        egui::TextEdit::singleline(&mut self.cfg.sleeper.username)
                            .desired_width(220.0)
                            .hint_text("your Sleeper username"),
                    );
                    if ui.button("Find leagues").clicked() {
                        self.find_leagues(ctx);
                    }
                });
                ui.end_row();

                ui.label("League");
                let leagues = self.leagues.lock().clone();
                let selected = leagues
                    .iter()
                    .find(|l| l.league_id == self.cfg.sleeper.league_id)
                    .map(|l| format!("{} ({} teams)", l.name, l.total_rosters))
                    .unwrap_or_else(|| {
                        if self.cfg.sleeper.league_id.is_empty() {
                            "(auto-detect)".to_string()
                        } else {
                            self.cfg.sleeper.league_id.clone()
                        }
                    });
                egui::ComboBox::from_id_salt("league_pick")
                    .selected_text(selected)
                    .width(300.0)
                    .show_ui(ui, |ui| {
                        ui.selectable_value(
                            &mut self.cfg.sleeper.league_id,
                            String::new(),
                            "(auto-detect)",
                        );
                        for l in &leagues {
                            ui.selectable_value(
                                &mut self.cfg.sleeper.league_id,
                                l.league_id.clone(),
                                format!("{} — {} teams, {}", l.name, l.total_rosters, l.scoring),
                            );
                        }
                    });
                ui.end_row();

                ui.label("Strategy");
                egui::ComboBox::from_id_salt("settings_strategy")
                    .selected_text(self.cfg.settings.strategy.label())
                    .show_ui(ui, |ui| {
                        for s in [Strategy::Conservative, Strategy::Balanced, Strategy::HighStakes]
                        {
                            ui.selectable_value(&mut self.cfg.settings.strategy, s, s.label());
                        }
                    });
                ui.end_row();

                ui.label("Refresh (seconds)");
                ui.add(egui::DragValue::new(&mut self.cfg.settings.refresh_seconds).range(30..=7200));
                ui.end_row();
            });

            ui.add_space(6.0);
            ui.label("Context files (one path per line — league rules, keeper notes)");
            ui.add(
                egui::TextEdit::multiline(&mut self.context_files_text)
                    .desired_width(f32::INFINITY)
                    .desired_rows(3)
                    .hint_text("league-rules.md"),
            );

            ui.add_space(12.0);
            ui.separator();
            ui.heading("Background monitoring");
            ui.label(
                egui::RichText::new(
                    "Used by `sa daemon` — the headless service that watches your league and \
                     pings you when something needs a decision.",
                )
                .small()
                .weak(),
            );
            ui.add_space(4.0);

            egui::Grid::new("daemon_grid").num_columns(2).spacing([12.0, 8.0]).show(ui, |ui| {
                ui.label("Analysis every (minutes)");
                ui.add(
                    egui::DragValue::new(&mut self.cfg.daemon.interval_minutes).range(5..=1440),
                );
                ui.end_row();

                ui.label("Active hours");
                ui.horizontal(|ui| {
                    ui.add(egui::DragValue::new(&mut self.cfg.daemon.active_hour_start).range(0..=23));
                    ui.label("to");
                    ui.add(egui::DragValue::new(&mut self.cfg.daemon.active_hour_end).range(0..=23));
                    ui.label(egui::RichText::new("(local time)").small().weak());
                });
                ui.end_row();

                ui.label("Alert me about");
                ui.vertical(|ui| {
                    let t = &mut self.cfg.daemon.triggers;
                    ui.checkbox(&mut t.better_lineup, "A better lineup is available");
                    ui.checkbox(&mut t.injured_starter, "A starter is Out / Doubtful / IR");
                    ui.checkbox(&mut t.waiver, "Waiver or free-agent upgrade");
                    ui.checkbox(&mut t.trade, "Trade ideas");
                });
                ui.end_row();

                ui.label("Webhook URL");
                ui.add(
                    egui::TextEdit::singleline(&mut self.cfg.notify.webhook_url)
                        .desired_width(380.0)
                        .hint_text("https://discord.com/api/webhooks/…"),
                );
                ui.end_row();

                ui.label("Payload format");
                ui.horizontal(|ui| {
                    let is_json = self.cfg.notify.format.eq_ignore_ascii_case("json");
                    if ui.selectable_label(!is_json, "Discord").clicked() {
                        self.cfg.notify.format = "discord".into();
                    }
                    if ui.selectable_label(is_json, "Raw JSON").clicked() {
                        self.cfg.notify.format = "json".into();
                    }
                });
                ui.end_row();
            });

            if self.cfg.webhook_from_env {
                ui.label(
                    egui::RichText::new(
                        "Webhook is set via SA_WEBHOOK_URL — it will not be written to config.yaml.",
                    )
                    .small()
                    .color(BRAND_PURPLE),
                );
            }

            ui.add_space(10.0);
            ui.horizontal(|ui| {
                if ui.button("Save settings").clicked() {
                    self.save_settings();
                }
                if ui.button("Send test notification").clicked() {
                    self.send_test_notification(ctx);
                }
            });
            ui.add_space(6.0);
            let msg = self.settings_msg.lock().clone();
            if !msg.is_empty() {
                ui.label(egui::RichText::new(msg).color(BRAND_PURPLE));
            }


            ui.add_space(14.0);
            ui.separator();
            ui.add_space(6.0);
            ui.heading("Advanced — Claude");

            egui::Grid::new("advanced_grid").num_columns(2).spacing([12.0, 8.0]).show(ui, |ui| {
                ui.label("API key");
                ui.horizontal(|ui| {
                    ui.add(
                        egui::TextEdit::singleline(&mut self.cfg.anthropic.api_key)
                            .desired_width(260.0)
                            .password(!self.show_api_key)
                            .hint_text("sk-ant-… (blank uses the Claude CLI)"),
                    );
                    let label = if self.show_api_key { "Hide" } else { "Show" };
                    if ui.button(label).clicked() {
                        self.show_api_key = !self.show_api_key;
                    }
                });
                ui.end_row();

                ui.label("Model");
                ui.horizontal(|ui| {
                    egui::ComboBox::from_id_salt("model_combo")
                        .selected_text(self.cfg.anthropic.model.clone())
                        .width(230.0)
                        .show_ui(ui, |ui| {
                            for m in MODELS {
                                ui.selectable_value(
                                    &mut self.cfg.anthropic.model,
                                    (*m).to_string(),
                                    *m,
                                );
                            }
                        });
                    ui.label(egui::RichText::new("or type below").small().weak());
                });
                ui.end_row();

                ui.label("");
                ui.add(
                    egui::TextEdit::singleline(&mut self.cfg.anthropic.model)
                        .desired_width(260.0)
                        .hint_text("any model id"),
                );
                ui.end_row();

                ui.label("Max response tokens");
                ui.add(egui::DragValue::new(&mut self.cfg.anthropic.max_tokens).range(256..=8192));
                ui.end_row();

                ui.label("Thinking budget");
                ui.horizontal(|ui| {
                    ui.add(
                        egui::DragValue::new(&mut self.cfg.anthropic.thinking_tokens)
                            .range(0..=8192),
                    );
                    ui.label(
                        egui::RichText::new("0 = off, ~2x faster (CLI only)").small().weak(),
                    );
                });
                ui.end_row();
            });

            if self.cfg.api_key_from_env {
                ui.label(
                    egui::RichText::new(
                        "API key is set via ANTHROPIC_API_KEY — it will not be written to config.yaml.",
                    )
                    .small()
                    .color(BRAND_PURPLE),
                );
            }

            ui.add_space(10.0);
            ui.label(egui::RichText::new("Backend per feature").strong());
            ui.label(
                egui::RichText::new(
                    "API is faster and billed per token; the Claude CLI uses your Pro/Max \
                     subscription. \"Inherit\" follows the default above (API when a key is \
                     set, otherwise the CLI).",
                )
                .small()
                .weak(),
            );
            ui.add_space(4.0);
            egui::Grid::new("feature_backends").num_columns(3).spacing([12.0, 6.0]).show(
                ui,
                |ui| {
                    ui.strong("Feature");
                    ui.strong("Backend");
                    ui.strong("In effect");
                    ui.end_row();
                    for feat in AiFeature::ALL {
                        ui.label(feat.label());
                        let slot = feature_slot(&mut self.cfg.anthropic.features, feat);
                        egui::ComboBox::from_id_salt(("backend", feat.label()))
                            .selected_text(match slot.as_str() {
                                "api" => "API",
                                "claude-cli" => "Claude CLI",
                                _ => "Inherit",
                            })
                            .width(130.0)
                            .show_ui(ui, |ui| {
                                ui.selectable_value(slot, String::new(), "Inherit");
                                ui.selectable_value(slot, "api".to_string(), "API");
                                ui.selectable_value(
                                    slot,
                                    "claude-cli".to_string(),
                                    "Claude CLI",
                                );
                            });
                        // What the *running* client resolved, which can differ
                        // from the saved setting until the app is restarted.
                        ui.label(
                            egui::RichText::new(self.anthropic.backend_name(feat)).small().weak(),
                        );
                        ui.end_row();
                    }
                },
            );
            ui.label(
                egui::RichText::new("Backend changes take effect on restart.").small().weak(),
            );

            ui.add_space(14.0);
            ui.separator();
            ui.add_space(6.0);
            ui.heading("About");
            ui.label(
                egui::RichText::new(format!("Sleeper Agent {}", crate::build_info::VERSION))
                    .strong(),
            );
            // Commit and date are the parts that actually distinguish two
            // builds — the crate version rarely moves between them.
            ui.label(
                egui::RichText::new(format!(
                    "Build {} · {}",
                    crate::build_info::COMMIT,
                    crate::build_info::COMMIT_DATE
                ))
                .small()
                .weak(),
            );
            ui.label(
                egui::RichText::new(format!("AI model: {}", self.cfg.anthropic.model))
                    .small()
                    .weak(),
            );
        });
    }

    fn save_settings(&mut self) {
        self.cfg.settings.context_files = self
            .context_files_text
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .map(String::from)
            .collect();
        // Keep the live strategy selector in sync with what was just saved.
        self.strategy = self.cfg.settings.strategy;
        match self.cfg.save() {
            Ok(()) => {
                *self.settings_msg.lock() = format!(
                    "Saved to {}. Username, league and daemon changes take effect on restart.",
                    self.cfg.path.display()
                )
            }
            Err(e) => *self.settings_msg.lock() = format!("Save failed: {e}"),
        }
    }

    fn find_leagues(&mut self, ctx: &egui::Context) {
        let client = self.session.client.clone();
        let username = self.cfg.sleeper.username.clone();
        let leagues = self.leagues.clone();
        let msg = self.settings_msg.clone();
        let ctx = ctx.clone();
        if username.trim().is_empty() {
            *self.settings_msg.lock() = "Enter a Sleeper username first.".into();
            return;
        }
        *self.settings_msg.lock() = format!("Looking up leagues for '{username}'…");
        self.rt.spawn(async move {
            match LeagueSession::discover_leagues(&client, &username).await {
                Ok(found) => {
                    *msg.lock() = if found.is_empty() {
                        format!("No leagues found for '{username}'.")
                    } else {
                        format!("Found {} league(s) for '{username}'.", found.len())
                    };
                    *leagues.lock() = found;
                }
                Err(e) => *msg.lock() = format!("Lookup failed: {e}"),
            }
            ctx.request_repaint();
        });
    }

    fn send_test_notification(&mut self, ctx: &egui::Context) {
        let msg = self.settings_msg.clone();
        let ctx = ctx.clone();
        let notify_cfg = self.cfg.notify.clone();
        self.rt.spawn(async move {
            match Notifier::new(&notify_cfg) {
                Ok(None) => *msg.lock() = "Set a webhook URL first.".into(),
                Ok(Some(n)) => {
                    let alert = Alert::new(
                        AlertKind::Lineup,
                        "Test notification",
                        "If you can read this, sleeper-agent can reach your webhook.",
                        "test",
                    );
                    *msg.lock() = match n.send(&alert).await {
                        Ok(()) => "Test notification sent.".into(),
                        Err(e) => format!("Send failed: {e}"),
                    };
                }
                Err(e) => *msg.lock() = format!("Notifier error: {e}"),
            }
            ctx.request_repaint();
        });
    }
}

pub fn run(
    rt: tokio::runtime::Handle,
    session: Arc<LeagueSession>,
    anthropic: Anthropic,
    scheduler: Arc<Scheduler>,
    cfg: Config,
) -> anyhow::Result<()> {
    // The image cache needs its own handle to spawn fetches.
    let rt2 = rt.clone();
    let app = GuiApp {
        rt,
        session,
        anthropic: Arc::new(anthropic),
        scheduler,
        strategy: cfg.settings.strategy,
        context_files_text: cfg.settings.context_files.join("\n"),
        cfg,
        tab: Tab::Roster,
        status: Arc::new(Mutex::new("Background refresh running.".into())),
        lineup: Arc::new(Mutex::new(None)),
        waiver: Arc::new(Mutex::new(None)),
        trade: Arc::new(Mutex::new(None)),
        draft_sugg: Arc::new(Mutex::new(None)),
        busy: Arc::new(Mutex::new(Default::default())),
        trade_partner: String::new(),
        trade_send: String::new(),
        trade_receive: String::new(),
        logo_tex: None,
        leagues: Arc::new(Mutex::new(Vec::new())),
        settings_msg: Arc::new(Mutex::new(String::new())),
        images: Arc::new(crate::images::ImageCache::new(rt2)),
        selected: Arc::new(Mutex::new(None)),
        show_api_key: false,
        trend_why: Arc::new(Mutex::new(std::collections::HashMap::new())),
        trade_scan: Arc::new(Mutex::new(None)),
        trade_ideas: Arc::new(Mutex::new(Vec::new())),
        trade_count: 3,
        trade_multi: false,
        trade_horizon: trade::Horizon::RestOfSeason,
        trade_send_hint: String::new(),
        trade_want: Vec::new(),
    };
    // App logo (assets/logo-mark.svg rasterized to PNG at build time).
    let icon = eframe::icon_data::from_png_bytes(include_bytes!("../assets/icon-256.png"))
        .ok()
        .map(std::sync::Arc::new);
    let mut viewport = egui::ViewportBuilder::default()
        .with_inner_size([1150.0, 740.0])
        .with_title("sleeper-agent");
    if let Some(icon) = icon {
        viewport = viewport.with_icon(icon);
    }
    let options = eframe::NativeOptions {
        viewport,
        ..Default::default()
    };
    eframe::run_native(
        "sleeper-agent",
        options,
        Box::new(|cc| {
            // Theme the whole app with the logo palette.
            cc.egui_ctx.set_visuals(brand_visuals());
            Ok(Box::new(app))
        }),
    )
    .map_err(|e| anyhow::anyhow!("eframe: {e}"))
}
