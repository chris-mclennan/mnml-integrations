//! App state + TUI event loop.

use crate::auth;
use crate::config::Config;
use crate::gcal::{self, Event};
use crate::theme::Theme;
use anyhow::{Context, Result};
use chrono::{DateTime, Duration, Utc};
use crossterm::event::{self, Event as CrosstermEvent, KeyCode, KeyEventKind, KeyModifiers};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use std::io;
use std::time::{Duration as StdDuration, Instant};

pub struct App {
    pub cfg: Config,
    pub view: View,
    pub events: Vec<Event>,
    pub selected: usize,
    pub loading: bool,
    pub last_error: Option<String>,
    pub last_refresh: Option<Instant>,
    pub theme: Theme,
    /// Held `Some` while a token is available; the client rebuilds
    /// silently on refresh.
    token: Option<auth::Token>,
}

/// Which slice of the calendar the user is viewing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum View {
    Today,
    Week,
    Upcoming,
}

impl App {
    pub fn new(cfg: Config) -> Self {
        Self {
            cfg,
            view: View::Today,
            events: Vec::new(),
            selected: 0,
            loading: false,
            last_error: None,
            last_refresh: None,
            theme: Theme::cyberdream(),
            token: None,
        }
    }

    /// Load + fetch events for the current view.
    pub fn refresh(&mut self) -> Result<()> {
        self.loading = true;
        let token = self.ensure_token()?;
        let (start, end) = self.time_bounds();
        let client = gcal::Client::new(&token.access_token);
        match client.list_events(&self.cfg.calendar_id, start, end) {
            Ok(mut evs) => {
                evs.sort_by_key(|e| event_start_ts(e));
                self.events = evs;
                self.selected = self.selected.min(self.events.len().saturating_sub(1));
                self.last_error = None;
            }
            Err(e) => {
                self.last_error = Some(format!("{e}"));
                self.events.clear();
            }
        }
        self.loading = false;
        self.last_refresh = Some(Instant::now());
        Ok(())
    }

    fn ensure_token(&mut self) -> Result<auth::Token> {
        if let Some(t) = &self.token
            && !t.is_expired()
        {
            return Ok(t.clone());
        }
        let cached = auth::load_token().ok();
        let tok = match cached {
            Some(t) if !t.is_expired() => t,
            Some(t) => auth::refresh_token(&t)
                .context("refresh expired token — try `mnml-msg-gcal auth` again")?,
            None => auth::interactive_login()
                .context("no cached token — running interactive login failed")?,
        };
        self.token = Some(tok.clone());
        Ok(tok)
    }

    fn time_bounds(&self) -> (DateTime<Utc>, DateTime<Utc>) {
        let now = Utc::now();
        match self.view {
            View::Today => (start_of_day(now), start_of_day(now) + Duration::days(1)),
            View::Week => (start_of_day(now), start_of_day(now) + Duration::days(7)),
            View::Upcoming => (now, now + Duration::days(self.cfg.upcoming_days as i64)),
        }
    }

    pub fn set_view(&mut self, v: View) {
        if self.view != v {
            self.view = v;
            self.selected = 0;
            let _ = self.refresh();
        }
    }

    pub fn move_selection(&mut self, delta: i64) {
        if self.events.is_empty() {
            return;
        }
        let n = self.events.len() as i64;
        let new = ((self.selected as i64 + delta).rem_euclid(n)) as usize;
        self.selected = new;
    }

    pub fn open_selected(&self) {
        if let Some(e) = self.events.get(self.selected)
            && let Some(url) = &e.html_link
        {
            let _ = webbrowser::open(url);
        }
    }

    pub fn yank_selected(&self) -> Option<String> {
        self.events
            .get(self.selected)
            .and_then(|e| e.html_link.clone())
    }
}

fn event_start_ts(e: &Event) -> String {
    e.start
        .date_time
        .clone()
        .or_else(|| e.start.date.clone())
        .unwrap_or_default()
}

fn start_of_day(t: DateTime<Utc>) -> DateTime<Utc> {
    use chrono::Timelike;
    t.date_naive()
        .and_hms_opt(0, 0, 0)
        .unwrap()
        .and_utc()
        .with_nanosecond(0)
        .unwrap()
}

pub fn run() -> Result<()> {
    let cfg = crate::config::load()?;
    let mut app = App::new(cfg);

    // Initial fetch (blocks; runs the OAuth loopback flow if there's
    // no cached token).
    if let Err(e) = app.refresh() {
        // Print + exit cleanly rather than launching the TUI with
        // a permanently-failed state.
        eprintln!("initial fetch failed: {e}");
        return Ok(());
    }

    // Enter alt-screen + raw mode.
    let mut stdout = io::stdout();
    enable_raw_mode().context("enable raw mode")?;
    execute!(stdout, EnterAlternateScreen).context("enter alt screen")?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend).context("build ratatui terminal")?;

    let result = run_loop(&mut terminal, &mut app);

    // Restore terminal even on error.
    disable_raw_mode().ok();
    execute!(terminal.backend_mut(), LeaveAlternateScreen).ok();
    terminal.show_cursor().ok();

    result
}

fn run_loop(terminal: &mut Terminal<CrosstermBackend<io::Stdout>>, app: &mut App) -> Result<()> {
    let tick = StdDuration::from_millis(200);
    loop {
        terminal.draw(|f| crate::ui::draw(f, app))?;

        // Auto-refresh timer.
        if app.cfg.refresh_secs > 0
            && let Some(last) = app.last_refresh
            && last.elapsed().as_secs() >= app.cfg.refresh_secs
        {
            let _ = app.refresh();
        }

        if event::poll(tick)? {
            match event::read()? {
                CrosstermEvent::Key(k) if k.kind == KeyEventKind::Press => {
                    if handle_key(app, k.code, k.modifiers) {
                        return Ok(());
                    }
                }
                CrosstermEvent::Resize(_, _) => {
                    // Ratatui handles this via the next draw call.
                }
                _ => {}
            }
        }
    }
}

/// Handle one key press. Returns `true` to exit.
fn handle_key(app: &mut App, code: KeyCode, mods: KeyModifiers) -> bool {
    match (code, mods) {
        (KeyCode::Char('q'), _) | (KeyCode::Esc, _) => return true,
        (KeyCode::Char('c'), KeyModifiers::CONTROL) => return true,
        (KeyCode::Char('1'), _) => app.set_view(View::Today),
        (KeyCode::Char('2'), _) => app.set_view(View::Week),
        (KeyCode::Char('3'), _) => app.set_view(View::Upcoming),
        (KeyCode::Char('j') | KeyCode::Down, _) => app.move_selection(1),
        (KeyCode::Char('k') | KeyCode::Up, _) => app.move_selection(-1),
        (KeyCode::PageDown, _) => app.move_selection(10),
        (KeyCode::PageUp, _) => app.move_selection(-10),
        (KeyCode::Char('g'), _) => app.selected = 0,
        (KeyCode::Char('G'), _) => {
            app.selected = app.events.len().saturating_sub(1);
        }
        (KeyCode::Enter, _) => app.open_selected(),
        (KeyCode::Char('y'), _) => {
            if let Some(url) = app.yank_selected() {
                crate::clipboard::yank(&url);
            }
        }
        (KeyCode::Char('r'), _) => {
            let _ = app.refresh();
        }
        _ => {}
    }
    false
}
