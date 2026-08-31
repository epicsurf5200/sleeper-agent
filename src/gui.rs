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
    busy: Arc<Mutex<std::collections::HashSet<&'static str>>>,
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
        AiFeature::Daemon => &mut f.daemon,
    }
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
        egui::CentralPanel::default().show(ctx, |ui| match self.tab {
            Tab::Roster => self.render_roster(ui, ctx),
            Tab::Matchup => self.render_matchup(ui, ctx),
            Tab::Lineup => self.render_lineup(ui, ctx),
            Tab::Waiver => self.render_waiver(ui, ctx),
            Tab::Trade => self.render_trade(ui, ctx),
            Tab::Trending => self.render_trending(ui, ctx),
            Tab::Activity => self.render_activity(ui),
            Tab::Draft => self.render_draft(ui, ctx),
            Tab::News => self.render_news(ui),
            Tab::Settings => self.render_settings(ui, ctx),
        });
        // Drawn last so it floats above whichever tab opened it.
        self.render_player_detail(ctx);
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

    /// Floating detail window for the selected player.
    fn render_player_detail(&self, ctx: &egui::Context) {
        let Some(p) = self.selected.lock().clone() else {
            return;
        };
        let data = self.data();
        let mut open = true;
        egui::Window::new(format!("{} · {} {}", p.name, p.position, p.team))
            .open(&mut open)
            .default_width(430.0)
            .collapsible(false)
            .show(ctx, |ui| {
                ui.horizontal_top(|ui| {
                    self.headshot(ui, ctx, &p.id, 120.0);
                    ui.add_space(10.0);
                    ui.vertical(|ui| {
                        ui.heading(&p.name);
                        ui.label(format!("{} · {} · {}", p.position, p.team, p.status));
                        ui.label(
                            egui::RichText::new(format!(
                                "Projected this week: {:.1} pts ({} scoring)",
                                p.projected_points,
                                data.settings.as_ref().map(|s| s.scoring.as_str()).unwrap_or("?")
                            ))
                            .color(BRAND_PURPLE),
                        );
                        if p.avg_points > 0.0 {
                            ui.label(format!("Season average: {:.1} pts/game", p.avg_points));
                        }
                        if let Some(b) = p.bye_week {
                            ui.label(format!("Bye week: {b}"));
                        }
                    });
                });

                ui.add_space(8.0);
                ui.separator();
                ui.heading("Upcoming games");
                let games = crate::player_detail::upcoming_for_team(
                    &data.schedule,
                    &p.team,
                    data.week,
                    5,
                );
                if games.is_empty() {
                    ui.weak("No scheduled games found.");
                } else {
                    egui::Grid::new("detail_sched").num_columns(3).striped(true).show(ui, |ui| {
                        for h in ["Week", "Opponent", "Date"] {
                            ui.strong(h);
                        }
                        ui.end_row();
                        for g in games {
                            ui.label(g.week.to_string());
                            ui.label(g.label());
                            ui.label(g.date.clone().unwrap_or_default());
                            ui.end_row();
                        }
                    });
                }

                ui.add_space(8.0);
                ui.separator();
                match data.perf.get(&p.id) {
                    Some(rec) if rec.games > 0 => {
                        ui.heading("Against projection");
                        // The table can lag the live season during the
                        // preseason, so always say which year it describes.
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
                                "Outperformed projection {:.0}% of games ({}/{})",
                                pct, rec.beat, rec.games
                            ))
                            .color(colour)
                            .strong(),
                        );
                        ui.label(format!(
                            "Average {:+.1} pts vs projection ({:.1} actual vs {:.1} projected)",
                            rec.avg_diff(),
                            rec.avg_actual(),
                            rec.avg_proj()
                        ));
                        ui.label(
                            egui::RichText::new(format!(
                                "Best week {:+.1} · worst week {:+.1}",
                                rec.best_diff, rec.worst_diff
                            ))
                            .small()
                            .weak(),
                        );

                        let totals = crate::player_detail::notable_stats(
                            &rec.totals,
                            &p.position.to_string(),
                        );
                        if !totals.is_empty() {
                            ui.add_space(6.0);
                            ui.heading(format!("{} totals", data.perf.season));
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
                        ui.heading("Against projection");
                        ui.weak(
                            "No completed games on record yet — this fills in once the season \
                             is under way.",
                        );
                    }
                }

                if !p.news.is_empty() {
                    ui.add_space(8.0);
                    ui.separator();
                    ui.heading("News");
                    for n in p.news.iter().take(5) {
                        ui.label(format!("• {n}"));
                    }
                }
            });
        if !open {
            *self.selected.lock() = None;
        }
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
                                ui.label(&p.name);
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
                        });
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
                        });
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
        self.busy.lock().insert(key);
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
        self.busy.lock().insert(key);
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
        self.busy.lock().insert(key);
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
        self.busy.lock().insert(key);
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
