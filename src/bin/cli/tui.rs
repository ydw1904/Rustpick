//! Interactive terminal UI, opened by a bare `rustpick` in a terminal.
//!
//! Same wiring as the other frontends: miner [`Event`]s update [`App`], keys and
//! `/` commands become [`Command`]s.

use anyhow::Result;
use chrono::{DateTime, Local};
use crossterm::event::{Event as TermEvent, EventStream, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use futures_util::StreamExt;
use ratatui::layout::{Constraint, Flex, Layout, Rect};
use ratatui::style::{Color, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Cell, Clear, LineGauge, Paragraph, Row, Table, TableState};
use ratatui::{DefaultTerminal, Frame};
use rustpick::events::{ChannelStatus, ChannelView, ProgressView};
use rustpick::{Command, Event, Miner, MinerHandle, Settings};
use std::collections::VecDeque;
use tokio::sync::broadcast;
use tokio::task::JoinHandle;

/// Lines kept in the Output and Log panes.
const MAX_LINES: usize = 1000;

/// Slash commands, in completion order.
const COMMANDS: &[&str] = &["watch", "auto", "pause", "resume", "reload", "quit"];

const KEYS: &str = " / command  1-2 tabs  j/k move  enter watch  p pause  r reload  q quit";

#[derive(Clone, Copy, Default, PartialEq, Eq)]
enum Tab {
    #[default]
    Main,
    Log,
}

#[derive(Default)]
struct App {
    tab: Tab,
    status: String,
    paused: bool,
    /// Online first, then drops enabled, then most viewers.
    channels: Vec<ChannelView>,
    watching: Option<u64>,
    progress: Option<ProgressView>,
    /// Device-code login in progress: (url, code).
    login: Option<(String, String)>,
    output: VecDeque<Line<'static>>,
    log: VecDeque<Line<'static>>,
    /// How many lines the Log tab is scrolled up from the newest; 0 follows new lines.
    log_scroll: usize,
    table: TableState,
    /// Some while the `/` command line is open.
    input: Option<String>,
    /// Footer feedback, cleared by the next key.
    message: Option<String>,
    quitting: bool,
}

/// Run the miner behind the TUI until it exits. `events` should be the sender logging forwards to.
pub async fn run(settings: Settings, events: broadcast::Sender<Event>) -> Result<()> {
    let mut rx = events.subscribe();
    let (miner, handle) = Miner::new(settings, events, false)?;
    let mut task = tokio::spawn(miner.run());
    let mut terminal = ratatui::init();
    let result = event_loop(&mut terminal, &mut rx, &mut task, &handle).await;
    ratatui::restore();
    result
}

async fn event_loop(
    terminal: &mut DefaultTerminal,
    events: &mut broadcast::Receiver<Event>,
    task: &mut JoinHandle<Result<()>>,
    miner: &MinerHandle,
) -> Result<()> {
    let mut app = App::default();
    let mut keys = EventStream::new();
    loop {
        terminal.draw(|f| app.draw(f))?;
        tokio::select! {
            r = &mut *task => return r?,
            ev = events.recv() => match ev {
                Ok(e) => app.on_event(e),
                Err(broadcast::error::RecvError::Lagged(_)) => {}
                Err(broadcast::error::RecvError::Closed) => return (&mut *task).await?,
            },
            // anything else (resizes) just falls through to a redraw
            Some(ev) = keys.next() => {
                if let Ok(TermEvent::Key(key)) = ev
                    && key.kind == KeyEventKind::Press
                {
                    app.on_key(key, miner);
                }
            }
        }
    }
}

impl App {
    fn on_event(&mut self, e: Event) {
        match e {
            Event::Print(s) => self.say(s),
            Event::Log { ts, level, msg } => {
                let style = match level.as_str() {
                    "ERROR" => Style::new().red(),
                    "WARN" => Style::new().yellow(),
                    _ => Style::new(),
                };
                push(&mut self.log, stamped(ts.with_timezone(&Local), format!("{level:<5} {msg}"), style));
                if self.log_scroll > 0 {
                    self.log_scroll += 1; // keep a scrolled-up view still
                }
            }
            Event::Status(s) => self.status = s,
            Event::Paused(p) => self.paused = p,
            Event::LoginRequired => self.say("The saved Twitch login has expired."),
            // the miner prints the code to Output itself
            Event::LoginCode { url, code } => self.login = Some((url, code)),
            Event::LoggedIn { user_id } => {
                self.login = None;
                self.say(format!("Logged in (user ID {user_id})"));
            }
            Event::AllDone => self.say("All campaigns are done."),
            Event::UpdateAvailable { version, url } => self.say(format!("Rustpick {version} is available: {url}")),
            Event::Channels(mut channels) => {
                channels.sort_by_key(|c| {
                    (c.status != ChannelStatus::Online, !c.drops_enabled, std::cmp::Reverse(c.viewers.unwrap_or(0)))
                });
                // keep the cursor on the same channel while the list reorders
                let selected = self.table.selected().and_then(|i| self.channels.get(i)).map(|c| c.id);
                self.channels = channels;
                let index = selected.and_then(|id| self.channels.iter().position(|c| c.id == id));
                self.table.select(index.or((!self.channels.is_empty()).then_some(0)));
            }
            Event::Watching(id) => self.watching = id,
            Event::Progress(p) => self.progress = p,
            Event::Exited => self.say("Exited."),
            // the miner already prints a line for claims and unlinked campaigns
            Event::Inventory(_) | Event::Websocket { .. } | Event::DropClaimed(_) | Event::UnlinkedCampaign { .. } => {}
        }
    }

    fn say(&mut self, s: impl Into<String>) {
        push(&mut self.output, stamped(Local::now(), s.into(), Style::new()));
    }

    fn on_key(&mut self, key: KeyEvent, miner: &MinerHandle) {
        self.message = None;
        if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
            return self.quit(miner);
        }
        if let Some(input) = &mut self.input {
            match key.code {
                KeyCode::Esc => self.input = None,
                KeyCode::Enter => {
                    let line = self.input.take().unwrap_or_default();
                    self.run_command(&line, miner);
                }
                KeyCode::Tab => {
                    if let Some(first) = completions(input, &self.channels).into_iter().next() {
                        *input = first;
                    }
                }
                KeyCode::Backspace => {
                    if input.pop().is_none() {
                        self.input = None;
                    }
                }
                KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => input.push(c),
                _ => {}
            }
            return;
        }
        match key.code {
            KeyCode::Esc if self.login.is_some() => self.login = None,
            KeyCode::Char('q') => self.quit(miner),
            KeyCode::Char('/' | ':') => self.input = Some(String::new()),
            KeyCode::Char('1') => self.tab = Tab::Main,
            KeyCode::Char('2') => self.tab = Tab::Log,
            KeyCode::Tab => self.tab = if self.tab == Tab::Main { Tab::Log } else { Tab::Main },
            KeyCode::Char('p') => miner.send(Command::Pause(!self.paused)),
            KeyCode::Char('r') => miner.send(Command::Reload),
            KeyCode::Up | KeyCode::Char('k') => self.scroll(-1),
            KeyCode::Down | KeyCode::Char('j') => self.scroll(1),
            KeyCode::PageUp => self.scroll(-10),
            KeyCode::PageDown => self.scroll(10),
            KeyCode::End | KeyCode::Char('G') => self.log_scroll = 0,
            KeyCode::Enter if self.tab == Tab::Main => {
                if let Some(c) = self.table.selected().and_then(|i| self.channels.get(i)) {
                    miner.send(Command::Select(Some(c.id)));
                    self.message = Some(format!("Switching to {}", c.name));
                }
            }
            _ => {}
        }
    }

    /// Move the channel cursor on Main, or scroll the Log tab (negative = up, older).
    fn scroll(&mut self, delta: isize) {
        match self.tab {
            Tab::Main if !self.channels.is_empty() => {
                let i = self.table.selected().unwrap_or(0) as isize + delta;
                self.table.select(Some(i.clamp(0, self.channels.len() as isize - 1) as usize));
            }
            Tab::Main => {}
            Tab::Log => self.log_scroll = (self.log_scroll as isize - delta).clamp(0, self.log.len() as isize) as usize,
        }
    }

    /// First press asks the miner to exit cleanly; a second one leaves immediately.
    fn quit(&mut self, miner: &MinerHandle) {
        if self.quitting {
            ratatui::restore();
            std::process::exit(130);
        }
        self.quitting = true;
        miner.send(Command::Exit);
    }

    fn run_command(&mut self, line: &str, miner: &MinerHandle) {
        let line = line.trim();
        let (cmd, arg) = line.split_once(' ').map_or((line, ""), |(c, a)| (c, a.trim()));
        match cmd {
            "" => {}
            "watch" => match self.channels.iter().find(|c| c.login.eq_ignore_ascii_case(arg) || c.name.eq_ignore_ascii_case(arg)) {
                Some(c) => {
                    miner.send(Command::Select(Some(c.id)));
                    self.message = Some(format!("Switching to {}", c.name));
                }
                None => self.message = Some(format!("No tracked channel named '{arg}'")),
            },
            "auto" => miner.send(Command::Select(None)),
            "pause" => miner.send(Command::Pause(true)),
            "resume" => miner.send(Command::Pause(false)),
            "reload" => miner.send(Command::Reload),
            "quit" | "exit" => self.quit(miner),
            _ => self.message = Some(format!("Unknown command /{cmd} (try {})", COMMANDS.join(", "))),
        }
    }

    fn draw(&mut self, f: &mut Frame) {
        let [header, body, footer] =
            Layout::vertical([Constraint::Length(1), Constraint::Fill(1), Constraint::Length(1)]).areas(f.area());
        self.draw_header(f, header);
        match self.tab {
            Tab::Main => {
                let [progress, channels, output] =
                    Layout::vertical([Constraint::Length(6), Constraint::Fill(3), Constraint::Fill(2)]).areas(body);
                self.draw_progress(f, progress);
                self.draw_channels(f, channels);
                draw_lines(f, output, " Output ".into(), &self.output, 0);
            }
            Tab::Log => {
                let title = match self.log_scroll {
                    0 => " Log ".to_owned(),
                    n => format!(" Log (scrolled up {n}, G to follow) "),
                };
                draw_lines(f, body, title, &self.log, self.log_scroll);
            }
        }
        self.draw_footer(f, footer);
        if let Some((url, code)) = &self.login {
            draw_login(f, url, code);
        }
    }

    fn draw_header(&self, f: &mut Frame, area: Rect) {
        let mut spans = vec![" Rustpick ".bold().magenta()];
        for (tab, name) in [(Tab::Main, " 1 Main "), (Tab::Log, " 2 Log ")] {
            spans.push(if self.tab == tab { name.reversed() } else { name.into() });
        }
        let tabs = Line::from(spans);
        let [left, right] = Layout::horizontal([Constraint::Length(tabs.width() as u16), Constraint::Fill(1)]).areas(area);
        let state = if self.quitting {
            "exiting".yellow()
        } else if self.paused {
            "paused".yellow()
        } else {
            "running".green()
        };
        f.render_widget(tabs, left);
        f.render_widget(Line::from(vec![format!("{}  ", self.status).dark_gray(), state, " ".into()]).right_aligned(), right);
    }

    fn draw_progress(&self, f: &mut Frame, area: Rect) {
        let Some(p) = &self.progress else {
            let block = Block::bordered().title(" Progress ");
            f.render_widget(Paragraph::new("Not earning a drop right now.").dark_gray().block(block), area);
            return;
        };
        let block = Block::bordered().title(format!(" {} | {} ", p.game, p.campaign_name));
        let [drop_text, drop_bar, campaign_text, campaign_bar] = Layout::vertical([Constraint::Length(1); 4]).areas(block.inner(area));
        f.render_widget(block, area);
        f.render_widget(Line::from(vec!["Drop      ".dark_gray(), p.drop_rewards.clone().into()]), drop_text);
        f.render_widget(bar(p.drop_progress, format!("{}/{} min", p.drop_current, p.drop_required)), drop_bar);
        let campaign = format!(
            "{}/{} drops, {} left",
            p.campaign_claimed,
            p.campaign_total,
            duration(p.campaign_remaining_minutes)
        );
        f.render_widget(Line::from(vec!["Campaign  ".dark_gray(), campaign.into()]), campaign_text);
        f.render_widget(bar(p.campaign_progress, String::new()), campaign_bar);
    }

    fn draw_channels(&mut self, f: &mut Frame, area: Rect) {
        let rows = self.channels.iter().map(|c| {
            let (status, color) = match c.status {
                ChannelStatus::Online => ("online", Color::Green),
                ChannelStatus::Pending => ("pending", Color::Yellow),
                ChannelStatus::Offline => ("offline", Color::DarkGray),
            };
            Row::new(vec![
                Cell::from(if Some(c.id) == self.watching { "▶" } else { "" }).magenta(),
                Cell::from(c.name.clone()),
                Cell::from(c.game.clone().unwrap_or_default()),
                Cell::from(status).fg(color),
                Cell::from(Line::from(c.viewers.map(viewers).unwrap_or_default()).right_aligned()),
                Cell::from(if c.drops_enabled { "drops" } else { "" }),
            ])
        });
        let widths = [
            Constraint::Length(1),
            Constraint::Fill(2),
            Constraint::Fill(3),
            Constraint::Length(7),
            Constraint::Length(7),
            Constraint::Length(5),
        ];
        let table = Table::new(rows, widths)
            .header(Row::new(["", "Channel", "Game", "Status", "Viewers", "Drops"]).dark_gray())
            .block(Block::bordered().title(format!(" Channels ({}) ", self.channels.len())))
            .row_highlight_style(Style::new().reversed());
        f.render_stateful_widget(table, area, &mut self.table);
    }

    fn draw_footer(&self, f: &mut Frame, area: Rect) {
        let Some(input) = &self.input else {
            let line = if self.quitting {
                Line::from("Exiting... press q again to force".yellow())
            } else if let Some(m) = &self.message {
                Line::from(m.as_str())
            } else {
                Line::from(KEYS.dark_gray())
            };
            return f.render_widget(line, area);
        };
        let prompt = format!("/{input}");
        f.set_cursor_position((area.x + Line::from(prompt.as_str()).width() as u16, area.y));
        let mut spans = vec![Span::raw(prompt)];
        for (i, c) in completions(input, &self.channels).into_iter().take(8).enumerate() {
            let hint = format!("  {}", c.trim_end().rsplit(' ').next().unwrap_or_default());
            spans.push(if i == 0 { hint.bold() } else { hint.dark_gray() });
        }
        f.render_widget(Line::from(spans), area);
    }
}

/// What Tab can complete `input` to, best first: command names, or channel logins after `watch `.
fn completions(input: &str, channels: &[ChannelView]) -> Vec<String> {
    match input.split_once(' ') {
        None => COMMANDS.iter().filter(|c| c.starts_with(input)).map(|c| format!("{c} ")).collect(),
        Some(("watch", arg)) => {
            let arg = arg.to_lowercase();
            channels.iter().filter(|c| c.login.starts_with(&arg)).map(|c| format!("watch {}", c.login)).collect()
        }
        Some(_) => Vec::new(),
    }
}

fn push(pane: &mut VecDeque<Line<'static>>, line: Line<'static>) {
    if pane.len() == MAX_LINES {
        pane.pop_front();
    }
    pane.push_back(line);
}

fn stamped(ts: DateTime<Local>, text: String, style: Style) -> Line<'static> {
    Line::from(vec![ts.format("%Y-%m-%d %H:%M:%S ").to_string().dark_gray(), Span::styled(text, style)])
}

/// The newest lines that fit, `scroll` lines up from the bottom. Long lines are cut, not wrapped.
fn draw_lines(f: &mut Frame, area: Rect, title: String, lines: &VecDeque<Line<'static>>, scroll: usize) {
    let block = Block::bordered().title(title);
    let end = lines.len().saturating_sub(scroll);
    let start = end.saturating_sub(block.inner(area).height as usize);
    let visible: Vec<Line> = lines.range(start..end).cloned().collect();
    f.render_widget(Paragraph::new(visible).block(block), area);
}

fn draw_login(f: &mut Frame, url: &str, code: &str) {
    let [area] = Layout::vertical([Constraint::Length(5)]).flex(Flex::Center).areas(f.area());
    let [area] = Layout::horizontal([Constraint::Max(60)]).flex(Flex::Center).areas(area);
    let text = vec![
        Line::from("Open this page in a browser:"),
        Line::from(url.to_owned().cyan()),
        Line::from(vec!["and enter the code ".into(), code.to_owned().bold().magenta()]),
    ];
    let block = Block::bordered().title(" Log in to Twitch ").title_bottom(" esc to hide ");
    f.render_widget(Clear, area);
    f.render_widget(Paragraph::new(text).centered().block(block), area);
}

fn bar(ratio: f32, label: String) -> LineGauge<'static> {
    let ratio = ratio.clamp(0.0, 1.0);
    LineGauge::default()
        .filled_style(Style::new().magenta())
        .unfilled_style(Style::new().dark_gray())
        .ratio(f64::from(ratio))
        .label(format!("{label} {:>3.0}%", ratio * 100.0))
}

/// 95 -> "1h 35m", 3000 -> "2d 2h".
fn duration(minutes: u32) -> String {
    match (minutes / 1440, minutes % 1440 / 60, minutes % 60) {
        (0, 0, m) => format!("{m}m"),
        (0, h, m) => format!("{h}h {m}m"),
        (d, h, _) => format!("{d}d {h}h"),
    }
}

/// 950 -> "950", 24300 -> "24.3k".
fn viewers(n: u64) -> String {
    if n < 1000 { n.to_string() } else { format!("{:.1}k", n as f64 / 1000.0) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn completes_commands_then_channel_logins() {
        let channel = |login: &str| ChannelView {
            id: 1,
            login: login.into(),
            name: login.into(),
            url: String::new(),
            acl_based: false,
            status: ChannelStatus::Online,
            game: None,
            drops_enabled: true,
            viewers: None,
        };
        let channels = [channel("shroud"), channel("tenz")];
        assert_eq!(completions("re", &channels), ["resume ", "reload "]);
        assert_eq!(completions("watch SH", &channels), ["watch shroud"]);
        assert_eq!(completions("watch ", &channels).len(), 2);
        assert!(completions("pause x", &channels).is_empty());
        assert_eq!(duration(95), "1h 35m");
        assert_eq!(duration(3000), "2d 2h");
    }
}
