//! `cell-hub console`: a terminal console for the hub's game servers.
//! Left: the servers. Right: the live log of the selected one. Bottom: a
//! line to send to the server's stdin.

use std::io;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use cell_hub_api::{ConsoleInput, LogLine, LogPage, ServerInstance};
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use crossterm::ExecutableCommand;
use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Wrap};

struct Client {
    http: reqwest::blocking::Client,
    url: String,
    token: Option<String>,
}

impl Client {
    fn req(&self, method: reqwest::Method, path: &str) -> reqwest::blocking::RequestBuilder {
        let r = self.http.request(method, format!("{}{}", self.url, path));
        match &self.token {
            Some(t) => r.bearer_auth(t),
            None => r,
        }
    }
    fn servers(&self) -> Result<Vec<ServerInstance>> {
        Ok(self
            .req(reqwest::Method::GET, "/api/servers")
            .send()?
            .error_for_status()?
            .json()?)
    }
    fn log(&self, id: &str, after: u64) -> Result<LogPage> {
        Ok(self
            .req(
                reqwest::Method::GET,
                &format!("/api/servers/{id}/log?after={after}&limit=500"),
            )
            .send()?
            .error_for_status()?
            .json()?)
    }
    fn action(&self, id: &str, action: &str) -> Result<()> {
        let r = self
            .req(
                reqwest::Method::POST,
                &format!("/api/servers/{id}/{action}"),
            )
            .send()?;
        if !r.status().is_success() {
            let text = r.text().unwrap_or_default();
            anyhow::bail!("{action}: {text}");
        }
        Ok(())
    }
    fn input(&self, id: &str, line: &str) -> Result<()> {
        let r = self
            .req(reqwest::Method::POST, &format!("/api/servers/{id}/input"))
            .json(&ConsoleInput {
                line: line.to_string(),
            })
            .send()?;
        if !r.status().is_success() {
            anyhow::bail!("input: {}", r.text().unwrap_or_default());
        }
        Ok(())
    }
}

enum Update {
    Servers(Vec<ServerInstance>),
    Log(String, LogPage),
    Error(String),
}

pub fn run(url: &str, token: Option<String>) -> Result<()> {
    let url = url.trim_end_matches('/').to_string();
    let client = Client {
        http: reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(5))
            .build()?,
        url: url.clone(),
        token: token.clone(),
    };
    client.servers().context("connect to the hub")?;

    // A poller thread keeps the UI thread free for key presses.
    let (tx, rx) = mpsc::channel::<Update>();
    let (sel_tx, sel_rx) = mpsc::channel::<Option<String>>();
    {
        let poll = Client {
            http: reqwest::blocking::Client::builder()
                .timeout(Duration::from_secs(5))
                .build()?,
            url: url.clone(),
            token: token.clone(),
        };
        std::thread::spawn(move || {
            let mut selected: Option<String> = None;
            let mut seq = 0u64;
            let mut last_servers = Instant::now() - Duration::from_secs(10);
            loop {
                while let Ok(s) = sel_rx.try_recv() {
                    if s != selected {
                        selected = s;
                        seq = 0;
                    }
                }
                if last_servers.elapsed() > Duration::from_secs(2) {
                    match poll.servers() {
                        Ok(v) => {
                            let _ = tx.send(Update::Servers(v));
                        }
                        Err(e) => {
                            let _ = tx.send(Update::Error(format!("{e:#}")));
                        }
                    }
                    last_servers = Instant::now();
                }
                if let Some(id) = &selected {
                    match poll.log(id, seq) {
                        Ok(page) => {
                            if page.seq < seq {
                                seq = 0;
                            }
                            if !page.lines.is_empty() {
                                seq = page.lines.last().map(|l| l.seq).unwrap_or(seq);
                            }
                            let _ = tx.send(Update::Log(id.clone(), page));
                        }
                        Err(e) => {
                            let _ = tx.send(Update::Error(format!("{e:#}")));
                        }
                    }
                }
                std::thread::sleep(Duration::from_millis(400));
            }
        });
    }

    enable_raw_mode()?;
    io::stdout().execute(EnterAlternateScreen)?;
    let mut terminal = Terminal::new(CrosstermBackend::new(io::stdout()))?;
    let result = ui_loop(&mut terminal, &client, rx, sel_tx);
    disable_raw_mode()?;
    io::stdout().execute(LeaveAlternateScreen)?;
    result
}

struct UiState {
    servers: Vec<ServerInstance>,
    list: ListState,
    log: Vec<LogLine>,
    log_for: Option<String>,
    input: String,
    status: String,
    follow: bool,
    scroll: usize,
}

fn selected_id(s: &UiState) -> Option<String> {
    s.list
        .selected()
        .and_then(|i| s.servers.get(i))
        .map(|x| x.id.clone())
}

fn ui_loop(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    client: &Client,
    rx: mpsc::Receiver<Update>,
    sel_tx: mpsc::Sender<Option<String>>,
) -> Result<()> {
    let mut s = UiState {
        servers: Vec::new(),
        list: ListState::default(),
        log: Vec::new(),
        log_for: None,
        input: String::new(),
        status: "F2 start  F3 stop  F4 restart  F5 backup  Enter send  Up/Down select  PgUp/PgDn scroll  Ctrl-C quit".into(),
        follow: true,
        scroll: 0,
    };
    loop {
        while let Ok(u) = rx.try_recv() {
            match u {
                Update::Servers(v) => {
                    if s.list.selected().is_none() && !v.is_empty() {
                        s.list.select(Some(0));
                    }
                    s.servers = v;
                    let _ = sel_tx.send(selected_id(&s));
                }
                Update::Log(id, page) => {
                    if s.log_for.as_deref() != Some(&id) {
                        s.log.clear();
                        s.log_for = Some(id);
                    }
                    s.log.extend(page.lines);
                    let n = s.log.len();
                    if n > 3000 {
                        s.log.drain(0..n - 3000);
                    }
                }
                Update::Error(e) => s.status = format!("error: {e}"),
            }
        }
        terminal.draw(|f| draw(f, &mut s))?;
        if event::poll(Duration::from_millis(100))? {
            if let Event::Key(k) = event::read()? {
                if k.kind != KeyEventKind::Press {
                    continue;
                }
                let id = selected_id(&s);
                match (k.code, k.modifiers) {
                    (KeyCode::Char('c'), KeyModifiers::CONTROL) => break,
                    (KeyCode::Esc, _) if s.input.is_empty() => break,
                    (KeyCode::Up, _) => {
                        let i = s.list.selected().unwrap_or(0);
                        s.list.select(Some(i.saturating_sub(1)));
                        s.log.clear();
                        s.log_for = None;
                        let _ = sel_tx.send(selected_id(&s));
                    }
                    (KeyCode::Down, _) => {
                        let i = s.list.selected().unwrap_or(0);
                        if i + 1 < s.servers.len() {
                            s.list.select(Some(i + 1));
                        }
                        s.log.clear();
                        s.log_for = None;
                        let _ = sel_tx.send(selected_id(&s));
                    }
                    (KeyCode::PageUp, _) => {
                        s.follow = false;
                        s.scroll = s.scroll.saturating_add(10);
                    }
                    (KeyCode::PageDown, _) => {
                        s.scroll = s.scroll.saturating_sub(10);
                        if s.scroll == 0 {
                            s.follow = true;
                        }
                    }
                    (KeyCode::F(2), _) => act(client, &id, "start", &mut s),
                    (KeyCode::F(3), _) => act(client, &id, "stop", &mut s),
                    (KeyCode::F(4), _) => act(client, &id, "restart", &mut s),
                    (KeyCode::F(5), _) => act(client, &id, "backups", &mut s),
                    (KeyCode::Enter, _) => {
                        if let Some(id) = &id {
                            let line = std::mem::take(&mut s.input);
                            match client.input(id, &line) {
                                Ok(()) => s.status = format!("sent: {line}"),
                                Err(e) => s.status = format!("{e:#}"),
                            }
                        }
                    }
                    (KeyCode::Backspace, _) => {
                        s.input.pop();
                    }
                    (KeyCode::Char(c), _) => s.input.push(c),
                    _ => {}
                }
            }
        }
    }
    Ok(())
}

fn act(client: &Client, id: &Option<String>, action: &str, s: &mut UiState) {
    let Some(id) = id else {
        s.status = "no server selected".into();
        return;
    };
    match client.action(id, action) {
        Ok(()) => s.status = format!("{action}: ok"),
        Err(e) => s.status = format!("{e:#}"),
    }
}

fn draw(f: &mut Frame, s: &mut UiState) {
    let outer = Layout::vertical([
        Constraint::Min(5),
        Constraint::Length(3),
        Constraint::Length(1),
    ])
    .split(f.area());
    let main = Layout::horizontal([Constraint::Length(34), Constraint::Min(20)]).split(outer[0]);

    let items: Vec<ListItem> = s
        .servers
        .iter()
        .map(|x| {
            let dot = if x.state == "running" { "●" } else { "○" };
            let color = if x.state == "running" {
                Color::Green
            } else {
                Color::DarkGray
            };
            ListItem::new(Line::from(vec![
                Span::styled(format!("{dot} "), Style::default().fg(color)),
                Span::raw(x.name.clone()),
                Span::styled(
                    format!("  {}", x.game_name),
                    Style::default().fg(Color::DarkGray),
                ),
            ]))
        })
        .collect();
    let list = List::new(items)
        .block(Block::default().borders(Borders::ALL).title(" Servers "))
        .highlight_style(
            Style::default()
                .bg(Color::Rgb(0x1a, 0x9f, 0xff))
                .fg(Color::Black),
        )
        .highlight_symbol("▶ ");
    f.render_stateful_widget(list, main[0], &mut s.list);

    let title = s
        .list
        .selected()
        .and_then(|i| s.servers.get(i))
        .map(|x| format!(" {} — {} ", x.name, x.state))
        .unwrap_or_else(|| " Log ".into());
    let height = main[1].height.saturating_sub(2) as usize;
    let total = s.log.len();
    let end = if s.follow {
        total
    } else {
        total.saturating_sub(s.scroll)
    };
    let start = end.saturating_sub(height);
    let lines: Vec<Line> = s.log[start..end]
        .iter()
        .map(|l| {
            let color = match l.stream.as_str() {
                "err" => Color::Red,
                "hub" => Color::Rgb(0x66, 0xc0, 0xf4),
                _ => Color::Reset,
            };
            Line::from(Span::styled(l.text.clone(), Style::default().fg(color)))
        })
        .collect();
    let log = Paragraph::new(lines)
        .block(Block::default().borders(Borders::ALL).title(title))
        .wrap(Wrap { trim: false });
    f.render_widget(log, main[1]);

    let input = Paragraph::new(format!("> {}", s.input)).block(
        Block::default()
            .borders(Borders::ALL)
            .title(" Console input "),
    );
    f.render_widget(input, outer[1]);
    let status = Paragraph::new(s.status.clone()).style(Style::default().fg(Color::DarkGray));
    f.render_widget(status, outer[2]);
}
