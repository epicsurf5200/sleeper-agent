use crate::anthropic::Anthropic;
use crate::api::LeagueSession;
use crate::draft::DraftManager;
use crate::lineup;
use crate::news::NewsFetcher;
use crate::scheduler::Scheduler;
use crate::strategy::Strategy;
use crate::trade::{self, TradeAnalysis};
use crate::types::*;
use crate::waiver::{self, WaiverReport};
use anyhow::Result;
use crossterm::event::{self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEventKind};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, Paragraph, Tabs, Wrap};
use ratatui::Terminal;
use std::io;
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
    Bracket,
    Draft,
    News,
    Help,
}

impl Tab {
    const ALL: [Tab; 10] = [
        Tab::Roster,
        Tab::Lineup,
        Tab::Waiver,
        Tab::Trade,
        Tab::Trending,
        Tab::Activity,
        Tab::Bracket,
        Tab::Draft,
        Tab::News,
        Tab::Help,
    ];
    fn label(&self) -> &'static str {
        match self {
            Self::Roster => "Roster",
            Self::Lineup => "Lineup",
            Self::Waiver => "Waiver",
            Self::Trade => "Trade",
            Self::Trending => "Trending",
            Self::Activity => "Activity",
            Self::Bracket => "Bracket",
            Self::Draft => "Draft",
            Self::News => "News",
            Self::Help => "Help",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum TradeField {
    Partner,
    Send,
    Receive,
}

pub struct App {
    session: Arc<LeagueSession>,
    anthropic: Anthropic,
    #[allow(dead_code)]
    news_fetcher: Arc<NewsFetcher>,
    strategy: Strategy,
    scheduler: Arc<Scheduler>,
    tab: Tab,
    status: String,
    lineup: Option<Lineup>,
    waiver: Option<WaiverReport>,
    trade: Option<TradeAnalysis>,
    draft_suggestion: Option<crate::draft::DraftSuggestion>,
    trade_partner_input: String,
    trade_send_input: String,
    trade_recv_input: String,
    trade_focus: TradeField,
}

impl App {
    pub fn new(
        session: Arc<LeagueSession>,
        anthropic: Anthropic,
        news_fetcher: Arc<NewsFetcher>,
        strategy: Strategy,
        refresh_interval: Duration,
    ) -> Self {
        let scheduler = Arc::new(Scheduler::new(refresh_interval));
        scheduler.spawn(session.clone(), news_fetcher.clone());
        Self {
            session,
            anthropic,
            news_fetcher,
            strategy,
            scheduler,
            tab: Tab::Roster,
            status: "r=refresh l=lineup w=waiver g=trending a=activity b=bracket d=draft s=strategy q=quit".into(),
            lineup: None,
            waiver: None,
            trade: None,
            draft_suggestion: None,
            trade_partner_input: String::new(),
            trade_send_input: String::new(),
            trade_recv_input: String::new(),
            trade_focus: TradeField::Partner,
        }
    }

    pub async fn run(mut self) -> Result<()> {
        // A panic mid-session would otherwise leave the shell in raw mode on
        // the alternate screen — restore the terminal before printing it.
        let default_hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            let _ = disable_raw_mode();
            let _ = execute!(io::stdout(), LeaveAlternateScreen, DisableMouseCapture);
            default_hook(info);
        }));
        enable_raw_mode()?;
        let mut stdout = io::stdout();
        execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
        let backend = CrosstermBackend::new(stdout);
        let mut terminal = Terminal::new(backend)?;
        let res = self.event_loop(&mut terminal).await;
        disable_raw_mode()?;
        execute!(terminal.backend_mut(), LeaveAlternateScreen, DisableMouseCapture)?;
        terminal.show_cursor()?;
        res
    }

    async fn event_loop(
        &mut self,
        terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    ) -> Result<()> {
        loop {
            terminal.draw(|f| self.render(f))?;
            if event::poll(Duration::from_millis(250))? {
                if let Event::Key(key) = event::read()? {
                    if key.kind != KeyEventKind::Press {
                        continue;
                    }
                    if self.tab == Tab::Trade {
                        // Show "working…" before the (blocking) AI call starts.
                        if matches!(key.code, KeyCode::Enter | KeyCode::F(2)) {
                            self.status = "Asking Claude…".into();
                            terminal.draw(|f| self.render(f))?;
                        }
                        if self.handle_trade_key(key.code).await {
                            continue;
                        }
                    }
                    // AI-backed actions block the loop for the duration of the
                    // Sleeper + Claude calls — draw the status line first so
                    // the user sees progress instead of a frozen frame.
                    if matches!(
                        key.code,
                        KeyCode::Char('l') | KeyCode::Char('w') | KeyCode::Char('d')
                    ) {
                        self.status = "Asking Claude…".into();
                        terminal.draw(|f| self.render(f))?;
                    }
                    match key.code {
                        KeyCode::Char('q') | KeyCode::Esc => break,
                        KeyCode::Tab | KeyCode::Right => self.cycle_tab(1),
                        KeyCode::BackTab | KeyCode::Left => self.cycle_tab(-1),
                        KeyCode::Char(c @ '1'..='9') => {
                            let idx = (c as u8 - b'1') as usize;
                            if idx < Tab::ALL.len() {
                                self.tab = Tab::ALL[idx];
                            }
                        }
                        KeyCode::Char('0') | KeyCode::Char('?') => self.tab = Tab::Help,
                        KeyCode::Char('r') => {
                            self.scheduler.poke();
                            self.status = "Refresh requested.".into();
                        }
                        KeyCode::Char('l') => self.compute_lineup().await,
                        KeyCode::Char('w') => self.compute_waiver().await,
                        KeyCode::Char('g') => self.tab = Tab::Trending,
                        KeyCode::Char('a') => self.tab = Tab::Activity,
                        KeyCode::Char('b') => self.tab = Tab::Bracket,
                        KeyCode::Char('d') => self.compute_draft().await,
                        KeyCode::Char('s') => self.cycle_strategy(),
                        _ => {}
                    }
                }
            }
        }
        Ok(())
    }

    async fn handle_trade_key(&mut self, code: KeyCode) -> bool {
        match code {
            KeyCode::Enter | KeyCode::F(2) => {
                self.compute_trade().await;
                true
            }
            KeyCode::Tab => {
                self.trade_focus = match self.trade_focus {
                    TradeField::Partner => TradeField::Send,
                    TradeField::Send => TradeField::Receive,
                    TradeField::Receive => TradeField::Partner,
                };
                true
            }
            KeyCode::Backspace => {
                self.trade_field_mut().pop();
                true
            }
            KeyCode::Char(c) => {
                self.trade_field_mut().push(c);
                true
            }
            _ => false,
        }
    }

    fn trade_field_mut(&mut self) -> &mut String {
        match self.trade_focus {
            TradeField::Partner => &mut self.trade_partner_input,
            TradeField::Send => &mut self.trade_send_input,
            TradeField::Receive => &mut self.trade_recv_input,
        }
    }

    fn cycle_tab(&mut self, dir: i32) {
        let mut idx = Tab::ALL.iter().position(|t| *t == self.tab).unwrap_or(0) as i32;
        idx = (idx + dir).rem_euclid(Tab::ALL.len() as i32);
        self.tab = Tab::ALL[idx as usize];
    }

    fn cycle_strategy(&mut self) {
        self.strategy = match self.strategy {
            Strategy::Conservative => Strategy::Balanced,
            Strategy::Balanced => Strategy::HighStakes,
            Strategy::HighStakes => Strategy::Conservative,
        };
        self.status = format!("Strategy → {}", self.strategy.label());
    }

    fn data(&self) -> crate::scheduler::AppData {
        self.scheduler.data.read().clone()
    }

    async fn compute_lineup(&mut self) {
        let data = self.data();
        let Some(roster) = data.roster else {
            self.status = "Waiting for first refresh.".into();
            return;
        };
        let settings = data.settings.unwrap_or_default();
        self.status = format!("Asking Claude for {} lineup…", self.strategy.label());
        match lineup::ai_optimize(
            &self.anthropic,
            &roster,
            &settings,
            &data.matchups,
            &data.news,
            self.strategy,
            data.week,
        )
        .await
        {
            Ok(l) => {
                self.status = format!("Lineup ready — projected {:.1} pts", l.projected_total);
                self.lineup = Some(l);
                self.tab = Tab::Lineup;
            }
            Err(e) => self.status = format!("lineup error: {e}"),
        }
    }

    async fn compute_waiver(&mut self) {
        let news = self.data().news;
        self.status = "Scoring free agents (projections + trending)…".into();
        match waiver::analyze(&self.session, &self.anthropic, self.strategy, &news, 300).await {
            Ok(r) => {
                self.status = format!("{} waiver candidates.", r.candidates.len());
                self.waiver = Some(r);
                self.tab = Tab::Waiver;
            }
            Err(e) => self.status = format!("waiver error: {e}"),
        }
    }

    async fn compute_trade(&mut self) {
        if self.trade_partner_input.trim().is_empty()
            || self.trade_send_input.trim().is_empty()
            || self.trade_recv_input.trim().is_empty()
        {
            self.status = "Fill partner / send / receive (Tab switches fields).".into();
            return;
        }
        let data = self.data();
        let Some(roster) = data.roster else {
            self.status = "Waiting for first refresh.".into();
            return;
        };
        let send: Vec<String> = self
            .trade_send_input
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
        let recv: Vec<String> = self
            .trade_recv_input
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
        self.status = "Computing metrics and asking Claude…".into();
        match trade::analyze(
            &self.anthropic,
            &roster,
            self.trade_partner_input.trim(),
            &send,
            &recv,
            &data.all_rosters,
            self.strategy,
            &data.news,
        )
        .await
        {
            Ok(a) => {
                self.status = format!("Verdict {} (net ROS {:+.1})", a.verdict, a.net_ros_delta);
                self.trade = Some(a);
            }
            Err(e) => self.status = format!("trade error: {e}"),
        }
    }

    async fn compute_draft(&mut self) {
        let team_name = self.data().roster.map(|r| r.team_name).unwrap_or_default();
        let dm = DraftManager {
            session: &self.session,
            anthropic: &self.anthropic,
            strategy: self.strategy,
            my_team_name: team_name,
        };
        self.status = "Fetching draft + asking Claude…".into();
        match dm.snapshot().await {
            Ok(state) => {
                let news = self.data().news;
                match dm.ask_claude(&state, &news).await {
                    Ok(s) => {
                        self.status = format!("{} draft candidates.", s.picks.len());
                        self.draft_suggestion = Some(s);
                        self.tab = Tab::Draft;
                    }
                    Err(e) => self.status = format!("draft AI error: {e}"),
                }
            }
            Err(e) => self.status = format!("draft state error: {e}"),
        }
    }

    fn render(&self, f: &mut ratatui::Frame) {
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(3),
                Constraint::Min(1),
                Constraint::Length(3),
            ])
            .split(f.area());
        self.render_tabs(f, chunks[0]);
        match self.tab {
            Tab::Roster => self.render_roster(f, chunks[1]),
            Tab::Lineup => self.render_lineup(f, chunks[1]),
            Tab::Waiver => self.render_waiver(f, chunks[1]),
            Tab::Trade => self.render_trade(f, chunks[1]),
            Tab::Trending => self.render_trending(f, chunks[1]),
            Tab::Activity => self.render_activity(f, chunks[1]),
            Tab::Bracket => self.render_bracket(f, chunks[1]),
            Tab::Draft => self.render_draft(f, chunks[1]),
            Tab::News => self.render_news(f, chunks[1]),
            Tab::Help => self.render_help(f, chunks[1]),
        }
        let p = Paragraph::new(self.status.clone())
            .block(Block::default().borders(Borders::ALL).title(" Status "))
            .wrap(Wrap { trim: true });
        f.render_widget(p, chunks[2]);
    }

    fn render_tabs(&self, f: &mut ratatui::Frame, area: Rect) {
        let titles: Vec<Line> = Tab::ALL.iter().map(|t| Line::from(t.label())).collect();
        let selected = Tab::ALL.iter().position(|t| *t == self.tab).unwrap_or(0);
        let data = self.data();
        let last = data
            .last_refresh
            .map(|t| format!("{}s ago", t.elapsed().as_secs()))
            .unwrap_or_else(|| "never".into());
        let tabs = Tabs::new(titles)
            .select(selected)
            .block(Block::default().borders(Borders::ALL).title(format!(
                " sleeper-agent — wk {} | {} | refresh {} ",
                data.week,
                self.strategy.label(),
                last
            )))
            .highlight_style(Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD));
        f.render_widget(tabs, area);
    }

    fn render_roster(&self, f: &mut ratatui::Frame, area: Rect) {
        let data = self.data();
        let items: Vec<ListItem> = match data.roster {
            None => vec![ListItem::new("Waiting for first refresh…")],
            Some(r) => r
                .players
                .iter()
                .map(|p| {
                    let line = format!(
                        "{:>5}  {:<24}  {:>3}  {:<4}  {:>4}  proj {:>5.1}",
                        p.roster_slot.to_string(),
                        truncate(&p.name, 24),
                        p.position.to_string(),
                        p.team,
                        p.status.to_string(),
                        p.projected_points,
                    );
                    let style = match p.status {
                        PlayerStatus::Out | PlayerStatus::IR | PlayerStatus::Suspended => {
                            Style::default().fg(Color::Red)
                        }
                        PlayerStatus::Doubtful => Style::default().fg(Color::Yellow),
                        PlayerStatus::Questionable => Style::default().fg(Color::LightYellow),
                        _ => Style::default(),
                    };
                    ListItem::new(Line::from(Span::styled(line, style)))
                })
                .collect(),
        };
        f.render_widget(
            List::new(items).block(Block::default().borders(Borders::ALL).title(" My Roster ")),
            area,
        );
    }

    fn render_lineup(&self, f: &mut ratatui::Frame, area: Rect) {
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(1), Constraint::Length(8)])
            .split(area);
        let items: Vec<ListItem> = match &self.lineup {
            None => vec![ListItem::new("Press 'l' for an AI lineup.")],
            Some(l) => l
                .starters
                .iter()
                .map(|s| {
                    let name = s
                        .player
                        .as_ref()
                        .map(|p| {
                            format!(
                                "{} ({} {}, {}) proj {:.1}",
                                p.name, p.position, p.team, p.status, p.projected_points
                            )
                        })
                        .unwrap_or_else(|| "(empty)".into());
                    ListItem::new(format!("{:<6}  {}", s.slot.to_string(), name))
                })
                .collect(),
        };
        f.render_widget(
            List::new(items).block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(" Recommended Lineup "),
            ),
            chunks[0],
        );
        let body = self
            .lineup
            .as_ref()
            .map(|l| format!("Projected total: {:.1}\n\n{}", l.projected_total, l.reasoning))
            .unwrap_or_else(|| "—".into());
        f.render_widget(
            Paragraph::new(body)
                .wrap(Wrap { trim: true })
                .block(Block::default().borders(Borders::ALL).title(" Reasoning ")),
            chunks[1],
        );
    }

    fn render_waiver(&self, f: &mut ratatui::Frame, area: Rect) {
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(1), Constraint::Length(9)])
            .split(area);
        let items: Vec<ListItem> = match &self.waiver {
            None => vec![ListItem::new("Press 'w' — projections + trending + AI re-rank.")],
            Some(r) => r
                .candidates
                .iter()
                .map(|c| {
                    let drop = c
                        .drop_candidate
                        .as_ref()
                        .map(|d| format!("drop {} (Δ{:+.0})", d.player.name, d.net_ros_delta))
                        .unwrap_or_else(|| "no drop".into());
                    ListItem::new(format!(
                        "{}. {:<22} {:>3} {:<4} proj {:>4.1} ROS {:>4.0}{} | {}",
                        c.priority,
                        truncate(&c.player.name, 22),
                        c.player.position.to_string(),
                        c.player.team,
                        c.metrics.adjusted_next_week,
                        c.metrics.ros_value,
                        c.trending_adds
                            .map(|n| format!(" +{n}/24h"))
                            .unwrap_or_default(),
                        drop,
                    ))
                })
                .collect(),
        };
        f.render_widget(
            List::new(items).block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(" Waiver candidates "),
            ),
            chunks[0],
        );
        let raw = self.waiver.as_ref().map(|w| w.raw.clone()).unwrap_or_default();
        f.render_widget(
            Paragraph::new(raw)
                .wrap(Wrap { trim: true })
                .block(Block::default().borders(Borders::ALL).title(" Claude analysis ")),
            chunks[1],
        );
    }

    fn render_trade(&self, f: &mut ratatui::Frame, area: Rect) {
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(7), Constraint::Min(1)])
            .split(area);
        let mark = |field: TradeField| if self.trade_focus == field { "▶" } else { " " };
        let inputs = format!(
            "{} Partner: {}\n{} Send: {}\n{} Receive: {}\n\n[Tab] switch field   [Enter] analyze",
            mark(TradeField::Partner),
            self.trade_partner_input,
            mark(TradeField::Send),
            self.trade_send_input,
            mark(TradeField::Receive),
            self.trade_recv_input
        );
        f.render_widget(
            Paragraph::new(inputs)
                .block(Block::default().borders(Borders::ALL).title(" Trade inputs ")),
            chunks[0],
        );
        let body = match &self.trade {
            None => "No analysis yet.".to_string(),
            Some(a) => {
                let send_lines = a
                    .send
                    .iter()
                    .map(|m| format!("    {}", m.one_line()))
                    .collect::<Vec<_>>()
                    .join("\n");
                let recv_lines = a
                    .receive
                    .iter()
                    .map(|m| format!("    {}", m.one_line()))
                    .collect::<Vec<_>>()
                    .join("\n");
                format!(
                    "VERDICT: {}    net ROS {:+.1}    fairness {:.0}%\n\nYOU SEND (ROS {:.0})\n{}\n\nYOU RECEIVE (ROS {:.0})\n{}\n\n{}",
                    a.verdict,
                    a.net_ros_delta,
                    a.fairness * 100.0,
                    a.send_pkg.total_ros,
                    send_lines,
                    a.receive_pkg.total_ros,
                    recv_lines,
                    a.ai_summary
                )
            }
        };
        f.render_widget(
            Paragraph::new(body)
                .wrap(Wrap { trim: true })
                .block(Block::default().borders(Borders::ALL).title(" Trade analysis ")),
            chunks[1],
        );
    }

    fn render_trending(&self, f: &mut ratatui::Frame, area: Rect) {
        let data = self.data();
        let halves = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
            .split(area);
        let make_items = |list: &[TrendingPlayer]| -> Vec<ListItem> {
            if list.is_empty() {
                return vec![ListItem::new("(waiting for refresh)")];
            }
            list.iter()
                .map(|t| {
                    ListItem::new(format!(
                        "{:>7}  {:<22} {:>3} {:<4} proj {:>4.1}",
                        t.count,
                        truncate(&t.player.name, 22),
                        t.player.position.to_string(),
                        t.player.team,
                        t.player.projected_points,
                    ))
                })
                .collect()
        };
        f.render_widget(
            List::new(make_items(&data.trending_add)).block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(" Trending ADDS (24h) "),
            ),
            halves[0],
        );
        f.render_widget(
            List::new(make_items(&data.trending_drop)).block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(" Trending DROPS (24h) "),
            ),
            halves[1],
        );
    }

    fn render_activity(&self, f: &mut ratatui::Frame, area: Rect) {
        let data = self.data();
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(1), Constraint::Length(8)])
            .split(area);
        let items: Vec<ListItem> = if data.transactions.is_empty() {
            vec![ListItem::new("No transactions in the lookback window.")]
        } else {
            data.transactions
                .iter()
                .take(40)
                .map(|t| {
                    let adds: Vec<String> =
                        t.adds.iter().map(|(p, tm)| format!("{tm} +{p}")).collect();
                    let drops: Vec<String> =
                        t.drops.iter().map(|(p, tm)| format!("{tm} -{p}")).collect();
                    ListItem::new(format!(
                        "wk{:<2} {:<10} [{}] {} {}{}",
                        t.week,
                        t.kind,
                        t.status,
                        adds.join(", "),
                        drops.join(", "),
                        t.waiver_bid.map(|b| format!(" (${b})")).unwrap_or_default(),
                    ))
                })
                .collect()
        };
        f.render_widget(
            List::new(items).block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(" League activity (waivers / trades / FA) "),
            ),
            chunks[0],
        );
        let picks: Vec<String> = data
            .traded_picks
            .iter()
            .take(6)
            .map(|p| format!("{} R{}: {} → {}", p.season, p.round, p.original_owner, p.current_owner))
            .collect();
        f.render_widget(
            Paragraph::new(if picks.is_empty() {
                "No traded picks.".to_string()
            } else {
                picks.join("\n")
            })
            .block(Block::default().borders(Borders::ALL).title(" Traded picks ")),
            chunks[1],
        );
    }

    fn render_bracket(&self, f: &mut ratatui::Frame, area: Rect) {
        let data = self.data();
        let halves = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
            .split(area);
        let make_items = |list: &[BracketMatch]| -> Vec<ListItem> {
            if list.is_empty() {
                return vec![ListItem::new("(no bracket yet)")];
            }
            list.iter()
                .map(|m| {
                    ListItem::new(format!(
                        "R{} M{}: {} vs {}{}",
                        m.round,
                        m.match_id,
                        m.team1.as_deref().unwrap_or("TBD"),
                        m.team2.as_deref().unwrap_or("TBD"),
                        m.winner
                            .as_deref()
                            .map(|w| format!(" → {w}"))
                            .unwrap_or_default(),
                    ))
                })
                .collect()
        };
        f.render_widget(
            List::new(make_items(&data.winners_bracket)).block(
                Block::default().borders(Borders::ALL).title(" Winners bracket "),
            ),
            halves[0],
        );
        f.render_widget(
            List::new(make_items(&data.losers_bracket)).block(
                Block::default().borders(Borders::ALL).title(" Losers bracket "),
            ),
            halves[1],
        );
    }

    fn render_draft(&self, f: &mut ratatui::Frame, area: Rect) {
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(8), Constraint::Min(1)])
            .split(area);
        let data = self.data();
        let header = match data.draft {
            None => "Press 'd' to fetch draft state and AI suggestions.".to_string(),
            Some(d) => format!(
                "Pick #{} (round {}/{}) — {} teams{}{}\nLast 5:\n{}",
                d.current_pick,
                ((d.current_pick.saturating_sub(1) / d.team_count.max(1)) + 1),
                d.total_rounds,
                d.team_count,
                d.on_the_clock_team
                    .as_deref()
                    .map(|t| format!(" — {t} on the clock"))
                    .unwrap_or_default(),
                if d.completed { " — COMPLETE" } else { "" },
                d.picks
                    .iter()
                    .rev()
                    .take(5)
                    .rev()
                    .map(|p| format!(
                        "  R{}.{} {} → {}",
                        p.round,
                        p.pick_number,
                        p.team_name,
                        p.player_name.as_deref().unwrap_or("?")
                    ))
                    .collect::<Vec<_>>()
                    .join("\n")
            ),
        };
        f.render_widget(
            Paragraph::new(header)
                .wrap(Wrap { trim: true })
                .block(Block::default().borders(Borders::ALL).title(" Draft ")),
            chunks[0],
        );
        let items: Vec<ListItem> = match &self.draft_suggestion {
            None => vec![ListItem::new("(press 'd' for suggestions)")],
            Some(s) => s
                .picks
                .iter()
                .map(|p| {
                    ListItem::new(format!(
                        "{}. {} ({}) — {}",
                        p.rank,
                        p.name,
                        p.position.map(|x| x.to_string()).unwrap_or_else(|| "?".into()),
                        p.rationale
                    ))
                })
                .collect(),
        };
        f.render_widget(
            List::new(items).block(
                Block::default().borders(Borders::ALL).title(" Top 3 candidates "),
            ),
            chunks[1],
        );
    }

    fn render_news(&self, f: &mut ratatui::Frame, area: Rect) {
        let news = self.data().news;
        let items: Vec<ListItem> = if news.is_empty() {
            vec![ListItem::new("Waiting for first news fetch…")]
        } else {
            news.iter()
                .take(50)
                .map(|n| {
                    ListItem::new(Line::from(vec![
                        Span::styled(format!("[{}] ", n.source), Style::default().fg(Color::Cyan)),
                        Span::raw(n.title.clone()),
                    ]))
                })
                .collect()
        };
        f.render_widget(
            List::new(items).block(Block::default().borders(Borders::ALL).title(" News ")),
            area,
        );
    }

    fn render_help(&self, f: &mut ratatui::Frame, area: Rect) {
        let body = "\
Keys
  r    Force refresh (scheduler runs continuously in the background)
  l    AI lineup            w    Waiver suggestions (trending-aware)
  g    Trending tab         a    Activity tab (transactions + traded picks)
  b    Bracket tab          d    Draft suggestions
  s    Cycle strategy       1-9/0  Jump to tab      Tab/← →  Switch tab
  q    Quit

Sleeper data used
  Real weekly projections (pts_ppr / half / std matched to league scoring),
  trending adds & drops (24h, league-wide), transactions with FAAB bids,
  traded picks, playoff brackets, live draft feed with on-the-clock detection.

Strategies
  Conservative — floor over ceiling.  Balanced — risk-adjusted EV (default).
  High Stakes — chase ceiling, tolerate injury risk.";
        f.render_widget(
            Paragraph::new(body)
                .wrap(Wrap { trim: false })
                .block(Block::default().borders(Borders::ALL).title(" Help ")),
            area,
        );
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
        out.push('…');
        out
    }
}
