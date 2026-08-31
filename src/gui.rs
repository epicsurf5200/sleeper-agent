//! Egui desktop GUI (feature `gui`).

use crate::anthropic::Anthropic;
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
            Tab::Roster => self.render_roster(ui),
            Tab::Lineup => self.render_lineup(ui, ctx),
            Tab::Waiver => self.render_waiver(ui, ctx),
            Tab::Trade => self.render_trade(ui, ctx),
            Tab::Trending => self.render_trending(ui),
            Tab::Activity => self.render_activity(ui),
            Tab::Draft => self.render_draft(ui, ctx),
            Tab::News => self.render_news(ui),
            Tab::Settings => self.render_settings(ui, ctx),
        });
    }
}

impl GuiApp {
    fn render_roster(&self, ui: &mut egui::Ui) {
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
                    ui.colored_label(color, &p.name);
                    ui.colored_label(color, p.position.to_string());
                    ui.colored_label(color, &p.team);
                    ui.colored_label(color, p.status.to_string());
                    ui.colored_label(color, format!("{:.1}", p.projected_points));
                    ui.end_row();
                }
            });
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
                        ui.label(&c.player.name);
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

    fn render_trending(&self, ui: &mut egui::Ui) {
        let data = self.data();
        ui.columns(2, |cols| {
            cols[0].heading("Trending ADDS (24h)");
            egui::ScrollArea::vertical()
                .id_salt("adds")
                .show(&mut cols[0], |ui| {
                    for t in &data.trending_add {
                        ui.label(format!(
                            "{:>6}  {} ({} {}) proj {:.1}",
                            t.count,
                            t.player.name,
                            t.player.position,
                            t.player.team,
                            t.player.projected_points
                        ));
                    }
                });
            cols[1].heading("Trending DROPS (24h)");
            egui::ScrollArea::vertical()
                .id_salt("drops")
                .show(&mut cols[1], |ui| {
                    for t in &data.trending_drop {
                        ui.label(format!(
                            "{:>6}  {} ({} {})",
                            t.count, t.player.name, t.player.position, t.player.team
                        ));
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
                ui.label(format!(
                    "R{}.{} {} → {}",
                    p.round,
                    p.pick_number,
                    p.team_name,
                    p.player_name.as_deref().unwrap_or("?")
                ));
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
