//! Interactive operations console over the same REST API and SSE stream as the dashboard.

use std::collections::{HashMap, VecDeque};
use std::io::{self, IsTerminal};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Gauge, List, ListItem, Paragraph, Wrap};
use tokio::sync::mpsc;

use crate::app::{ConfigReport, MinersReport, SourcesReport};
use crate::client::DaemonClient;
use crate::client::events::Notice;
use crate::domain::{Job, JobId, JobKind, JobStatus};
use crate::error::{Error, Result};
use crate::search::{SearchHit, SearchOptions};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Page {
    Jobs,
    Search,
    Readiness,
    Maintenance,
}

impl Page {
    fn title(self) -> &'static str {
        match self {
            Self::Jobs => "Jobs",
            Self::Search => "Search test",
            Self::Readiness => "Sources & miners",
            Self::Maintenance => "Maintenance",
        }
    }
}

enum Input {
    Mine,
    Search,
    Ranking,
    Wing,
    SignIn,
    RunMiner,
}

enum Message {
    Stream(bool),
    Refresh,
    Snapshot(Instant, Vec<Job>),
    MaintenanceSnapshot(Instant, Vec<Job>),
    Job(Box<Job>),
    Sources(SourcesReport),
    SourcesUnavailable(String),
    Miners(MinersReport),
    MinersUnavailable(String),
    Config(Box<ConfigReport>),
    Search(Vec<SearchHit>, Duration),
    AuthChallenge(String),
    ActionResult(String),
    Info(String),
}

struct Console {
    page: Page,
    jobs: HashMap<JobId, Job>,
    maintenance: HashMap<JobId, Job>,
    event_updates: HashMap<JobId, Instant>,
    maintenance_selected: usize,
    maintenance_detail: bool,
    detail_scroll: u16,
    selected: usize,
    activity: VecDeque<String>,
    sources: Option<SourcesReport>,
    miners: Option<MinersReport>,
    config: Option<ConfigReport>,
    hits: Vec<SearchHit>,
    query: String,
    ranking: String,
    wing: Option<String>,
    auth_challenge: Option<String>,
    readiness_scroll: u16,
    input: Option<Input>,
    typed: String,
    confirm: Option<JobId>,
    live: bool,
    color: bool,
    busy: bool,
    message: String,
    last_refresh: Option<Instant>,
}

impl Default for Console {
    fn default() -> Self {
        Self {
            page: Page::Jobs,
            jobs: HashMap::new(),
            maintenance: HashMap::new(),
            event_updates: HashMap::new(),
            maintenance_selected: 0,
            maintenance_detail: false,
            detail_scroll: 0,
            selected: 0,
            activity: VecDeque::new(),
            sources: None,
            miners: None,
            config: None,
            hits: Vec::new(),
            query: String::new(),
            ranking: "auto".into(),
            wing: None,
            auth_challenge: None,
            readiness_scroll: 0,
            input: None,
            typed: String::new(),
            confirm: None,
            live: false,
            color: crate::term::stdout_color(),
            busy: false,
            message: "Connecting to daemon…".into(),
            last_refresh: None,
        }
    }
}

impl Console {
    fn tone(&self, color: Color) -> Style {
        if self.color {
            Style::default().fg(color)
        } else {
            Style::default()
        }
    }

    fn ordered(&self) -> Vec<&Job> {
        let mut jobs: Vec<_> = self.jobs.values().collect();
        jobs.sort_by_key(|job| std::cmp::Reverse(job.created_at));
        jobs
    }

    fn selected_job(&self) -> Option<&Job> {
        self.ordered().get(self.selected).copied()
    }

    fn notice(&mut self, value: String) {
        self.message = value.clone();
        self.activity.push_front(value);
        self.activity.truncate(8);
    }

    fn receive(&mut self, message: Message) {
        match message {
            Message::Stream(live) => {
                self.live = live;
                if !live {
                    self.notice("Disconnected: retrying; displayed state may be stale".into());
                }
            }
            Message::Refresh => {
                self.notice("Refreshing after stream reconnect or missed events".into())
            }
            Message::Snapshot(started, jobs) => {
                let selected = self.selected_job().map(|job| job.id);
                let mut latest: HashMap<_, _> = jobs.into_iter().map(|job| (job.id, job)).collect();
                // A notice fetched while this page was in flight outranks its older
                // snapshot, including a job absent from the bounded recent page.
                for (id, job) in &self.jobs {
                    if self.event_updates.get(id).is_some_and(|at| *at >= started) {
                        latest.insert(*id, job.clone());
                    }
                }
                self.jobs = latest;
                self.event_updates.retain(|id, at| {
                    *at >= started
                        && (self.jobs.contains_key(id) || self.maintenance.contains_key(id))
                });
                self.select_or_clamp(selected);
                self.last_refresh = Some(Instant::now());
            }
            Message::MaintenanceSnapshot(started, jobs) => {
                let mut latest: HashMap<_, _> = jobs.into_iter().map(|job| (job.id, job)).collect();
                for (id, job) in &self.maintenance {
                    if self.event_updates.get(id).is_some_and(|at| *at >= started) {
                        latest.insert(*id, job.clone());
                    }
                }
                self.maintenance = latest;
                self.maintenance_selected = self
                    .maintenance_selected
                    .min(self.maintenance.len().min(10).saturating_sub(1));
            }
            Message::Job(job) => {
                self.event_updates.insert(job.id, Instant::now());
                self.notice(format!("{}: {}", job.id, job.status));
                if matches!(job.kind, JobKind::Mine { .. }) {
                    let selected = self.selected_job().map(|job| job.id);
                    self.jobs.insert(job.id, *job);
                    self.select_or_clamp(selected);
                } else {
                    self.maintenance.insert(job.id, *job);
                }
            }
            Message::Sources(report) => {
                self.sources = Some(report);
                self.last_refresh = Some(Instant::now());
            }
            Message::SourcesUnavailable(error) => {
                self.sources = None;
                self.notice(format!("Source status unavailable: {error}"));
            }
            Message::Miners(report) => {
                if let Some(error) = &report.error {
                    // The daemon can return its last good copy while the file
                    // is unreadable; calling those old miners ready would mislead operators.
                    self.miners = None;
                    self.notice(format!("Miner status unknown: {error}"));
                } else {
                    self.miners = Some(report);
                    self.last_refresh = Some(Instant::now());
                }
            }
            Message::MinersUnavailable(error) => {
                self.miners = None;
                self.notice(format!("Miner status unavailable: {error}"));
            }
            Message::Config(report) => self.config = Some(*report),
            Message::Search(hits, elapsed) => {
                self.hits = hits;
                self.busy = false;
                self.notice(format!(
                    "Search completed in {} ms (client-measured)",
                    elapsed.as_millis()
                ));
            }
            Message::AuthChallenge(instructions) => {
                self.auth_challenge = Some(instructions.clone());
                self.readiness_scroll = 0;
                self.busy = false;
                self.notice(instructions);
            }
            Message::ActionResult(text) => {
                self.busy = false;
                self.notice(text);
            }
            Message::Info(text) => {
                if text.contains("signed in") || text.starts_with("Sign-in failed:") {
                    self.auth_challenge = None;
                }
                self.notice(text);
            }
        }
    }

    fn select_or_clamp(&mut self, selected: Option<JobId>) {
        self.selected = selected
            .and_then(|id| self.ordered().iter().position(|job| job.id == id))
            .unwrap_or(self.selected.min(self.jobs.len().saturating_sub(1)));
    }

    fn selected_maintenance(&self) -> Option<&Job> {
        let mut jobs: Vec<_> = self.maintenance.values().collect();
        jobs.sort_by_key(|job| std::cmp::Reverse(job.created_at));
        jobs.get(self.maintenance_selected).copied()
    }
}

fn progress(job: &Job) -> String {
    let state = if job.status == JobStatus::Running {
        "running"
    } else {
        job.status.as_str()
    };
    let detail = job.progress.message.as_deref().unwrap_or(state);
    match job.progress.total {
        Some(total) if total > 0 => {
            let width = 16;
            let filled = (job.progress.current.min(total) as usize * width) / total as usize;
            format!(
                "[{}{}] {}/{} {detail}",
                "━".repeat(filled),
                "─".repeat(width - filled),
                job.progress.current,
                total
            )
        }
        _ if job.status == JobStatus::Running => format!("◌ {detail}"),
        _ => format!("[{state}] {detail}"),
    }
}

fn status_mark(status: JobStatus) -> (&'static str, Color) {
    match status {
        JobStatus::Queued => ("◷", Color::Yellow),
        JobStatus::Running => ("●", Color::Cyan),
        JobStatus::Paused => ("Ⅱ", Color::Magenta),
        JobStatus::Completed => ("✓", Color::Green),
        JobStatus::Failed => ("✕", Color::Red),
        JobStatus::Cancelled => ("○", Color::Gray),
    }
}

fn source(job: &Job) -> String {
    match &job.kind {
        JobKind::Mine {
            source: crate::domain::MiningSource::Named { source, .. },
            ..
        } => source.clone(),
        JobKind::Mine { .. } => "directory".into(),
        _ => "other".into(),
    }
}

fn maintenance_kind(kind: &JobKind) -> &'static str {
    match kind {
        JobKind::Audit { .. } => "audit",
        JobKind::Repair { .. } => "repair",
        JobKind::Embed { .. } => "embed",
        JobKind::Extract { .. } => "extract",
        _ => "job",
    }
}

fn draw_header(frame: &mut ratatui::Frame<'_>, app: &Console, area: Rect) {
    let mut parts = vec![
        Span::styled(
            " 🏰 MEMCASTLE ",
            app.tone(Color::Cyan).add_modifier(Modifier::BOLD),
        ),
        Span::styled("│ OPS  ", app.tone(Color::Gray)),
    ];
    for (number, page) in [Page::Jobs, Page::Search, Page::Readiness, Page::Maintenance]
        .into_iter()
        .enumerate()
    {
        let label = format!(" {} {} ", number + 1, page.title());
        let style = if app.page == page {
            app.tone(Color::Yellow)
                .add_modifier(Modifier::BOLD | Modifier::UNDERLINED)
        } else {
            app.tone(Color::Gray)
        };
        parts.push(Span::styled(label, style));
        parts.push(Span::styled("│", app.tone(Color::DarkGray)));
    }
    let (indicator, color) = if app.live {
        (" ● LIVE ", Color::Green)
    } else {
        (" ◌ RECONNECTING ", Color::Yellow)
    };
    parts.push(Span::styled(
        indicator,
        app.tone(color).add_modifier(Modifier::BOLD),
    ));
    let age = app
        .last_refresh
        .map_or("never".into(), |at| format!("{}s", at.elapsed().as_secs()));
    parts.push(Span::styled(
        format!(" · sync {age}"),
        app.tone(Color::Gray),
    ));
    frame.render_widget(
        Paragraph::new(Line::from(parts)).block(
            Block::default()
                .borders(Borders::BOTTOM)
                .border_style(app.tone(Color::Cyan)),
        ),
        area,
    );
}

fn draw_jobs(frame: &mut ratatui::Frame<'_>, app: &Console, area: Rect) {
    let wide = area.width >= 95;
    let panes = Layout::default()
        .direction(if wide {
            Direction::Horizontal
        } else {
            Direction::Vertical
        })
        .constraints(if wide {
            [Constraint::Percentage(62), Constraint::Percentage(38)]
        } else {
            [Constraint::Min(5), Constraint::Length(10)]
        })
        .split(area);
    let jobs = app.ordered();
    let rows: Vec<Line<'_>> = if jobs.is_empty() {
        vec![Line::styled(
            "  ◌ No mining jobs yet · press n to start a run",
            app.tone(Color::Gray),
        )]
    } else {
        jobs.iter()
            .enumerate()
            .map(|(index, job)| {
                let (mark, color) = status_mark(job.status);
                let elapsed = job.started_at.map_or("—".into(), |started| {
                    format!(
                        "{}s",
                        (job.completed_at.unwrap_or_else(chrono::Utc::now) - started)
                            .num_seconds()
                            .max(0)
                    )
                });
                let text = format!(
                    " {:<11} {:<10} {:>5}  {}",
                    source(job),
                    job.status,
                    elapsed,
                    progress(job)
                );
                let mut row = Line::from(vec![
                    Span::styled(
                        if index == app.selected {
                            " ▸ "
                        } else {
                            "   "
                        },
                        app.tone(Color::Yellow),
                    ),
                    Span::styled(
                        format!("{mark} "),
                        app.tone(color).add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(
                        text,
                        if index == app.selected {
                            app.tone(Color::White).add_modifier(Modifier::BOLD)
                        } else {
                            app.tone(Color::Gray)
                        },
                    ),
                ]);
                if index == app.selected && app.color {
                    row = row.style(Style::default().bg(Color::DarkGray));
                }
                row
            })
            .collect()
    };
    let visible = usize::from(panes[0].height.saturating_sub(2)).max(1);
    let scroll = u16::try_from(app.selected.saturating_sub(visible - 1)).unwrap_or(u16::MAX);
    frame.render_widget(
        Paragraph::new(rows).scroll((scroll, 0)).block(
            Block::default()
                .title(format!(" MINING JOBS · {} ", jobs.len()))
                .borders(Borders::ALL)
                .border_style(app.tone(Color::Cyan)),
        ),
        panes[0],
    );
    draw_job_detail(frame, app, panes[1]);
}

fn draw_job_detail(frame: &mut ratatui::Frame<'_>, app: &Console, area: Rect) {
    let block = Block::default()
        .title(" SELECTED · DETAILS ")
        .borders(Borders::ALL)
        .border_style(app.tone(Color::Gray));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let Some(job) = app.selected_job() else {
        frame.render_widget(
            Paragraph::new(" Select a mining job to inspect its progress and actions.")
                .style(app.tone(Color::Gray)),
            inner,
        );
        return;
    };
    let (mark, color) = status_mark(job.status);
    let started = job.started_at.map_or("not started".into(), |time| {
        time.format("%Y-%m-%d %H:%M:%S UTC").to_string()
    });
    let mut lines = vec![
        Line::from(vec![
            Span::styled(
                format!(" {mark} {}", job.status),
                app.tone(color).add_modifier(Modifier::BOLD),
            ),
            Span::styled(format!("  {}", source(job)), app.tone(Color::Cyan)),
        ]),
        Line::styled(format!(" ID       {}", job.id), app.tone(Color::Gray)),
        Line::styled(format!(" Started  {started}"), app.tone(Color::Gray)),
        Line::styled(
            format!(" Progress {}", progress(job)),
            app.tone(Color::White),
        ),
    ];
    if let Some(error) = &job.error {
        lines.push(Line::styled(format!(" ✕ {error}"), app.tone(Color::Red)));
    }
    if job.status == JobStatus::Running {
        lines.push(Line::styled(
            "  p pause  ·  c stop  ·  f force-cancel",
            app.tone(Color::Yellow),
        ));
    }
    // A terminal job with no measurable total needs all of the available
    // height for its error; reserving an empty gauge hid that line on short terminals.
    let indicator =
        job.status == JobStatus::Running || job.progress.total.is_some_and(|total| total > 0);
    let areas = indicator.then(|| {
        Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(4), Constraint::Length(2)])
            .split(inner)
    });
    frame.render_widget(
        Paragraph::new(lines).wrap(Wrap { trim: true }),
        areas.as_ref().map_or(inner, |parts| parts[0]),
    );
    if let Some(total) = job.progress.total.filter(|total| *total > 0) {
        let ratio = f64::from(job.progress.current.min(total)) / f64::from(total);
        frame.render_widget(
            Gauge::default()
                .gauge_style(app.tone(Color::Cyan))
                .ratio(ratio)
                .label(format!("{}/{}", job.progress.current, total)),
            areas
                .as_ref()
                .expect("determinate progress has an indicator")[1],
        );
    } else if job.status == JobStatus::Running {
        frame.render_widget(
            Paragraph::new(" ◌ Working · total not yet known").style(app.tone(Color::Yellow)),
            areas.as_ref().expect("running progress has an indicator")[1],
        );
    }
}

fn draw(frame: &mut ratatui::Frame<'_>, app: &Console) {
    let parts = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(5),
            Constraint::Length(5),
            Constraint::Length(4),
        ])
        .split(frame.area());
    draw_header(frame, app, parts[0]);
    if app.page == Page::Jobs {
        draw_jobs(frame, app, parts[1]);
    } else {
        let lines: Vec<Line<'_>> = match app.page {
            Page::Jobs => unreachable!("jobs have their own split view"),
            Page::Search => {
                let mut lines = vec![Line::from(vec![
                    Span::styled(
                        " ⌕ SEARCH TEST  ",
                        app.tone(Color::Cyan).add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(
                        format!(
                            "{}  ",
                            if app.query.is_empty() {
                                "press / to run a query"
                            } else {
                                &app.query
                            }
                        ),
                        app.tone(Color::White),
                    ),
                    Span::styled(
                        format!(
                            "◆ {}  ◇ {}",
                            app.ranking,
                            app.wing.as_deref().unwrap_or("all wings")
                        ),
                        app.tone(Color::Gray),
                    ),
                ])];
                lines.extend(app.hits.iter().take(12).map(|hit| {
                    let excerpt: String = hit
                        .drawer
                        .content
                        .lines()
                        .next()
                        .unwrap_or("")
                        .chars()
                        .take(72)
                        .collect();
                    Line::from(vec![
                        Span::styled(
                            format!("  ◇ {}  ", hit.drawer.name.as_deref().unwrap_or("unnamed")),
                            app.tone(Color::Cyan),
                        ),
                        Span::styled(format!("{:.3}  ", hit.score), app.tone(Color::Green)),
                        Span::styled(
                            format!(
                                "lex {:?}  sem {:?}  graph {:?}  │  {excerpt}",
                                hit.signals.lexical, hit.signals.semantic, hit.signals.graph
                            ),
                            app.tone(Color::Gray),
                        ),
                    ])
                }));
                lines
            }
            Page::Readiness => {
                let mut lines = Vec::new();
                if let Some(challenge) = &app.auth_challenge {
                    lines.push(Line::styled(
                        format!("  ◈ SIGN-IN  {challenge}"),
                        app.tone(Color::Yellow),
                    ));
                }
                if let Some(config) = &app.config {
                    lines.push(Line::styled(
                        "  ◈ PROVIDERS  ·  configured ≠ connected",
                        app.tone(Color::Cyan).add_modifier(Modifier::BOLD),
                    ));
                    lines.push(Line::from(format!(
                        "    Embeddings  {}",
                        config.embeddings.provider
                    )));
                    lines.push(Line::from(format!(
                        "    Extraction  {}",
                        config.extraction.provider
                    )));
                } else {
                    lines.push(Line::from("Provider status unknown"));
                }
                if let Some(sources) = &app.sources {
                    lines.push(Line::styled(
                        "  ◈ MINING SOURCES",
                        app.tone(Color::Cyan).add_modifier(Modifier::BOLD),
                    ));
                    lines.extend(sources.adapters.iter().map(|adapter| {
                        let auth = adapter.auth.as_ref().map_or("no sign-in required", |auth| {
                            if auth.signed_in {
                                "signed in"
                            } else {
                                "not signed in (or status unavailable)"
                            }
                        });
                        let (icon, color) = match adapter.state {
                            crate::domain::SourceState::Enabled => ("●", Color::Green),
                            crate::domain::SourceState::Unavailable => ("✕", Color::Red),
                            _ => ("○", Color::Yellow),
                        };
                        Line::from(vec![
                            Span::styled(format!("    {icon} "), app.tone(color)),
                            Span::styled(format!("{:<16}", adapter.name), app.tone(Color::White)),
                            Span::styled(
                                format!(
                                    "{: <13} {auth} {}",
                                    adapter.state,
                                    adapter.unavailable_reason.as_deref().unwrap_or("")
                                ),
                                app.tone(Color::Gray),
                            ),
                        ])
                    }));
                } else {
                    lines.push(Line::from("Source status unknown; press r to refresh."));
                }
                if let Some(miners) = &app.miners {
                    lines.push(Line::styled(
                        "  ◈ CONFIGURED MINERS",
                        app.tone(Color::Cyan).add_modifier(Modifier::BOLD),
                    ));
                    lines.extend(miners.miners.iter().map(|miner| {
                        let (icon, color) = match miner.state {
                            crate::app::MinerState::Ready => ("●", Color::Green),
                            crate::app::MinerState::Unavailable => ("✕", Color::Red),
                            crate::app::MinerState::Disabled => ("○", Color::Yellow),
                        };
                        Line::from(vec![
                            Span::styled(format!("    {icon} "), app.tone(color)),
                            Span::styled(format!("{:<16}", miner.name), app.tone(Color::White)),
                            Span::styled(
                                format!(
                                    "{:<12} {:?} {}",
                                    miner.source,
                                    miner.state,
                                    miner.reason.as_deref().unwrap_or("")
                                ),
                                app.tone(Color::Gray),
                            ),
                        ])
                    }));
                } else {
                    lines.push(Line::from("Miner status unknown; press r to refresh."));
                }
                lines
            }
            Page::Maintenance => {
                if app.maintenance_detail {
                    let detail =
                        app.selected_maintenance()
                            .map_or("Job no longer listed".into(), |job| {
                                let result = job
                                    .result
                                    .as_ref()
                                    .map(|value| {
                                        serde_json::to_string_pretty(value).unwrap_or_default()
                                    })
                                    .unwrap_or_else(|| {
                                        job.error.clone().unwrap_or_else(|| progress(job))
                                    });
                                format!("Job {} ({})\n{result}", job.id, job.status)
                            });
                    detail
                        .lines()
                        .map(|line| Line::from(line.to_owned()))
                        .collect()
                } else {
                    let mut lines = vec![
                        Line::styled(
                            "  ◈ MAINTENANCE  ·  safe by default",
                            app.tone(Color::Cyan).add_modifier(Modifier::BOLD),
                        ),
                        Line::styled(
                            "    a audit  ·  d repair preview  ·  e embed  ·  x extract",
                            app.tone(Color::Yellow),
                        ),
                    ];
                    let mut jobs: Vec<_> = app.maintenance.values().collect();
                    jobs.sort_by_key(|job| std::cmp::Reverse(job.created_at));
                    lines.extend(jobs.into_iter().take(10).enumerate().map(|(index, job)| {
                        let detail = job
                            .error
                            .as_deref()
                            .or_else(|| {
                                job.result
                                    .as_ref()
                                    .map(|_| "report ready · Enter to inspect")
                            })
                            .unwrap_or_else(|| {
                                job.progress.message.as_deref().unwrap_or("waiting")
                            });
                        let (mark, color) = status_mark(job.status);
                        let mut row = Line::from(vec![
                            Span::styled(
                                if index == app.maintenance_selected {
                                    "  ▸ "
                                } else {
                                    "    "
                                },
                                app.tone(Color::Yellow),
                            ),
                            Span::styled(format!("{mark} "), app.tone(color)),
                            Span::styled(
                                format!("{:<10} ", maintenance_kind(&job.kind)),
                                app.tone(Color::White),
                            ),
                            Span::styled(
                                format!("{}  {}  {detail}", &job.id.to_string()[..8], job.status),
                                app.tone(Color::Gray),
                            ),
                        ]);
                        if index == app.maintenance_selected && app.color {
                            row = row.style(Style::default().bg(Color::DarkGray));
                        }
                        row
                    }));
                    lines
                }
            }
        };
        let title = if app.maintenance_detail && app.page == Page::Maintenance {
            "Maintenance result · Esc to close"
        } else {
            app.page.title()
        };
        let content =
            Paragraph::new(lines).block(Block::default().borders(Borders::ALL).title(title));
        let content = if app.maintenance_detail && app.page == Page::Maintenance {
            content.scroll((app.detail_scroll, 0))
        } else if app.page == Page::Maintenance {
            let visible = usize::from(parts[1].height.saturating_sub(2)).max(1);
            content.scroll((
                u16::try_from((app.maintenance_selected + 3).saturating_sub(visible))
                    .unwrap_or(u16::MAX),
                0,
            ))
        } else if app.page == Page::Readiness {
            content
                .wrap(Wrap { trim: true })
                .scroll((app.readiness_scroll, 0))
        } else {
            content.wrap(Wrap { trim: true })
        };
        frame.render_widget(content, parts[1]);
    }
    let feed: Vec<ListItem<'_>> = app
        .activity
        .iter()
        .take(3)
        .map(|entry| {
            let color = if entry.contains("failed")
                || entry.contains("ERROR")
                || entry.contains("Disconnected")
            {
                Color::Red
            } else if entry.contains("queued") || entry.contains("Refreshing") {
                Color::Yellow
            } else {
                Color::Green
            };
            ListItem::new(Line::from(vec![
                Span::styled("  • ", app.tone(color)),
                Span::styled(entry.as_str(), app.tone(Color::Gray)),
            ]))
        })
        .collect();
    frame.render_widget(
        List::new(feed).block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(app.tone(Color::Gray))
                .title(" ACTIVITY "),
        ),
        parts[2],
    );
    let prompt = if app.confirm.is_some() {
        "Confirm force-cancel? y/N".to_string()
    } else if app.input.is_some() {
        format!("> {}_", app.typed)
    } else if app.busy {
        "Waiting for daemon acknowledgement…".to_string()
    } else {
        app.message.clone()
    };
    let keys = match app.page {
        Page::Jobs => "n mine · p pause · u resume · c cancel · f force · t retry",
        Page::Search => "/ query · g ranking · w wing",
        Page::Readiness => "s sign in · m run miner",
        Page::Maintenance => {
            "a audit · d dry run · e embed · x extract · Enter inspect result · Esc back"
        }
    };
    let hint = Line::from(vec![
        Span::styled("  ⌨ ", app.tone(Color::Cyan)),
        Span::styled(keys, app.tone(Color::Yellow)),
        Span::styled(
            "    ·    Tab/1–4 views  ↑↓ select  r refresh  q quit",
            app.tone(Color::Gray),
        ),
    ]);
    frame.render_widget(
        Paragraph::new(vec![
            Line::styled(
                format!("  {prompt}"),
                if app.confirm.is_some() {
                    app.tone(Color::Red)
                } else {
                    app.tone(Color::White)
                },
            ),
            hint,
        ])
        .wrap(Wrap { trim: true })
        .block(
            Block::default()
                .borders(Borders::TOP)
                .border_style(app.tone(Color::Cyan)),
        ),
        parts[3],
    );
}

struct Screen;
impl Screen {
    fn enter() -> Result<Self> {
        enable_raw_mode().map_err(|error| Error::io("terminal", error))?;
        if let Err(error) = execute!(io::stdout(), EnterAlternateScreen, crossterm::cursor::Hide) {
            let _ = disable_raw_mode();
            return Err(Error::io("terminal", error));
        }
        Ok(Self)
    }
}
impl Drop for Screen {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = execute!(io::stdout(), LeaveAlternateScreen, crossterm::cursor::Show);
    }
}

fn refresh(client: Arc<DaemonClient>, tx: mpsc::UnboundedSender<Message>) {
    tokio::spawn(async move {
        let started = Instant::now();
        // The active status queries prevent a very old running job falling off the recent page.
        let mut jobs = match client.list_jobs_page("mine", 50).await {
            Ok(jobs) => jobs,
            Err(error) => {
                let _ = tx.send(Message::Info(format!("Jobs refresh failed: {error}")));
                return;
            }
        };
        for status in [JobStatus::Running, JobStatus::Paused, JobStatus::Queued] {
            if let Ok(active) = client.list_jobs(Some(status)).await {
                for job in active {
                    if matches!(job.kind, JobKind::Mine { .. })
                        && !jobs.iter().any(|seen| seen.id == job.id)
                    {
                        jobs.push(job);
                    }
                }
            }
        }
        let _ = tx.send(Message::Snapshot(started, jobs));
        let mut maintenance = Vec::new();
        for kind in ["audit", "repair", "embed", "extract"] {
            if let Ok(page) = client.list_jobs_page(kind, 10).await {
                maintenance.extend(page);
            }
        }
        let _ = tx.send(Message::MaintenanceSnapshot(started, maintenance));
        match client.list_sources().await {
            Ok(report) => {
                let _ = tx.send(Message::Sources(report));
            }
            Err(error) => {
                let _ = tx.send(Message::SourcesUnavailable(error.to_string()));
            }
        }
        match client.list_miners().await {
            Ok(report) => {
                let _ = tx.send(Message::Miners(report));
            }
            Err(error) => {
                let _ = tx.send(Message::MinersUnavailable(error.to_string()));
            }
        }
        if let Ok(report) = client.config_report().await {
            let _ = tx.send(Message::Config(Box::new(report)));
        }
    });
}

fn stream(
    client: Arc<DaemonClient>,
    tx: mpsc::UnboundedSender<Message>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut delay = Duration::from_secs(1);
        loop {
            match client.events().await {
                Ok(mut events) => {
                    while let Ok(Some(notice)) = events.next().await {
                        match notice {
                            Notice::Open | Notice::Resync => {
                                let _ = tx.send(Message::Stream(true));
                                let _ = tx.send(Message::Refresh);
                                refresh(client.clone(), tx.clone());
                                delay = Duration::from_secs(1);
                            }
                            Notice::Job { id, kind }
                                if matches!(
                                    kind.as_str(),
                                    "mine" | "audit" | "repair" | "embed" | "extract"
                                ) =>
                            {
                                if let Ok(id) = id.parse::<JobId>()
                                    && let Ok(job) = client.get_job(id).await
                                {
                                    let _ = tx.send(Message::Job(Box::new(job)));
                                }
                            }
                            Notice::Job { .. } => {}
                        }
                    }
                }
                Err(Error::Remote {
                    status: 401 | 403, ..
                }) => {
                    let _ = tx.send(Message::Info(
                        "Event stream refused; check token and memory mode, then restart the TUI"
                            .into(),
                    ));
                    return;
                }
                Err(error) => {
                    let _ = tx.send(Message::Info(format!("Event stream unavailable: {error}")));
                }
            }
            let _ = tx.send(Message::Stream(false));
            tokio::time::sleep(delay).await;
            delay = (delay * 2).min(Duration::from_secs(30));
        }
    })
}

/// Run the interactive client until the user quits or presses Ctrl-C.
pub async fn run(client: DaemonClient) -> Result<()> {
    if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
        return Err(Error::invalid_input(
            "tui",
            "an interactive stdin and stdout are required; use `memcastle job list` in scripts",
        ));
    }
    let _screen = Screen::enter()?;
    let mut terminal = Terminal::new(CrosstermBackend::new(io::stdout()))
        .map_err(|error| Error::io("terminal", error))?;
    let client = Arc::new(client);
    let (tx, mut rx) = mpsc::unbounded_channel();
    let stop = Arc::new(AtomicBool::new(false));
    let (keys_tx, mut keys) = mpsc::unbounded_channel();
    let stopping = stop.clone();
    let input_thread = std::thread::spawn(move || {
        while !stopping.load(Ordering::Relaxed) {
            if event::poll(Duration::from_millis(80)).unwrap_or(false)
                && let Ok(event) = event::read()
            {
                let _ = keys_tx.send(event);
            }
        }
    });
    let reader = stream(client.clone(), tx.clone());
    refresh(client.clone(), tx.clone());
    let mut app = Console::default();
    let mut tick = tokio::time::interval(Duration::from_millis(150));
    let mut reconcile = tokio::time::interval(Duration::from_secs(60));
    // The initial snapshot was requested above; interval's immediate first tick
    // would issue a second concurrent copy and potentially overwrite newer events.
    reconcile.tick().await;
    let mut terminal_error = None;
    loop {
        tokio::select! {
            _ = tick.tick() => {
                if let Err(error) = terminal.draw(|frame| draw(frame, &app)) {
                    terminal_error = Some(Error::io("terminal", error));
                    break;
                }
            }
            _ = reconcile.tick() => refresh(client.clone(), tx.clone()),
            Some(message) = rx.recv() => app.receive(message),
            Some(event) = keys.recv() => {
                let Event::Key(key) = event else { continue; };
                if key.kind != KeyEventKind::Press { continue; }
                if key.modifiers.contains(event::KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') { break; }
                if let Some(id) = app.confirm.take() {
                    if key.code == KeyCode::Char('y') {
                        app.busy = true;
                        let client = client.clone(); let tx = tx.clone();
                        tokio::spawn(async move { send_job_result(client.force_cancel_job(id).await.map(|result| format!("Force-cancel acknowledged: {:?}", result.status)), id, client, tx).await; });
                    }
                    continue;
                }
                if app.input.is_some() {
                    match key.code {
                        KeyCode::Esc => { app.input = None; app.typed.clear(); }
                        KeyCode::Backspace => { app.typed.pop(); }
                        KeyCode::Char(ch) => app.typed.push(ch),
                        KeyCode::Enter => {
                            let input = match app.input.take() { Some(input) => input, None => continue };
                            let value = std::mem::take(&mut app.typed);
                            if matches!(input, Input::Ranking) {
                                match value.parse::<crate::domain::RankingMode>() {
                                    Ok(_) => app.ranking = value,
                                    Err(error) => app.notice(error),
                                }
                                continue;
                            }
                            if matches!(input, Input::Wing) {
                                app.wing = (!value.is_empty()).then_some(value);
                                continue;
                            }
                            if matches!(input, Input::Search) { app.query = value.clone(); }
                            app.busy = true;
                            let search_options = SearchOptions { ranking: Some(app.ranking.clone()), wing: app.wing.clone(), ..Default::default() };
                            let client = client.clone(); let tx = tx.clone();
                            tokio::spawn(async move { run_input(input, value, search_options, client, tx).await; });
                        }
                        _ => {}
                    }
                    continue;
                }
                match key.code {
                    KeyCode::Char('q') => break,
                    KeyCode::Char('1') => app.page = Page::Jobs,
                    KeyCode::Char('2') => app.page = Page::Search,
                    KeyCode::Char('3') => app.page = Page::Readiness,
                    KeyCode::Char('4') => app.page = Page::Maintenance,
                    KeyCode::Tab => app.page = match app.page { Page::Jobs => Page::Search, Page::Search => Page::Readiness, Page::Readiness => Page::Maintenance, Page::Maintenance => Page::Jobs },
                    KeyCode::Char('r') => refresh(client.clone(), tx.clone()),
                    KeyCode::Up | KeyCode::Char('k') if app.page == Page::Maintenance && app.maintenance_detail => app.detail_scroll = app.detail_scroll.saturating_sub(1),
                    KeyCode::Down | KeyCode::Char('j') if app.page == Page::Maintenance && app.maintenance_detail => app.detail_scroll = app.detail_scroll.saturating_add(1),
                    KeyCode::Up | KeyCode::Char('k') if app.page == Page::Readiness => app.readiness_scroll = app.readiness_scroll.saturating_sub(1),
                    KeyCode::Down | KeyCode::Char('j') if app.page == Page::Readiness => app.readiness_scroll = app.readiness_scroll.saturating_add(1),
                    KeyCode::Up | KeyCode::Char('k') if app.page == Page::Maintenance => app.maintenance_selected = app.maintenance_selected.saturating_sub(1),
                    KeyCode::Down | KeyCode::Char('j') if app.page == Page::Maintenance => app.maintenance_selected = (app.maintenance_selected + 1).min(app.maintenance.len().min(10).saturating_sub(1)),
                    KeyCode::Up | KeyCode::Char('k') => app.selected = app.selected.saturating_sub(1),
                    KeyCode::Down | KeyCode::Char('j') => app.selected = (app.selected + 1).min(app.jobs.len().saturating_sub(1)),
                    KeyCode::Enter if app.page == Page::Maintenance && app.selected_maintenance().is_some() => {
                        app.maintenance_detail = true; app.detail_scroll = 0;
                    }
                    KeyCode::Esc if app.page == Page::Maintenance => app.maintenance_detail = false,
                    KeyCode::Char('n') if app.page == Page::Jobs => app.input = Some(Input::Mine),
                    KeyCode::Char('/') if app.page == Page::Search => app.input = Some(Input::Search),
                    KeyCode::Char('g') if app.page == Page::Search => app.input = Some(Input::Ranking),
                    KeyCode::Char('w') if app.page == Page::Search => app.input = Some(Input::Wing),
                    KeyCode::Char('s') if app.page == Page::Readiness => app.input = Some(Input::SignIn),
                    KeyCode::Char('m') if app.page == Page::Readiness => app.input = Some(Input::RunMiner),
                    KeyCode::Char(action @ ('p' | 'u' | 'c' | 'f' | 't')) if app.page == Page::Jobs && !app.busy => {
                        if let Some(job) = app.selected_job() {
                            let id = job.id;
                            if !valid_action(job.status, action) { app.notice("Action unavailable for this job status".into()); continue; }
                            if action == 'f' { app.confirm = Some(id); continue; }
                            app.busy = true;
                            let client = client.clone(); let tx = tx.clone();
                            tokio::spawn(async move {
                                let result = match action { 'p' => client.pause_job(id).await, 'u' => client.resume_job(id).await, 'c' => client.cancel_job(id).await, _ => client.retry_job(id).await };
                                send_job_result(result.map(|value| format!("Daemon acknowledged: {:?}", value.status)), id, client, tx).await;
                            });
                        }
                    }
                    KeyCode::Char(action @ ('a' | 'd' | 'e' | 'x')) if app.page == Page::Maintenance && !app.busy => {
                        if matches!(action, 'e' | 'x') {
                            let provider = app.config.as_ref().map(|config| if action == 'e' { &config.embeddings.provider } else { &config.extraction.provider });
                            if provider.is_none_or(|value| value == "none") {
                                app.notice("Provider not configured or readiness unknown; inspect the readiness view".into());
                                continue;
                            }
                        }
                        app.busy = true;
                        let client = client.clone(); let tx = tx.clone();
                        tokio::spawn(async move {
                            let result = match action { 'a' => client.submit_audit(None).await, 'd' => client.submit_repair(true, None).await, 'e' => client.submit_embed(None).await, _ => client.submit_extract(None).await };
                            match result {
                                Ok(job) => {
                                    let _ = tx.send(Message::ActionResult(format!("Submitted {} job {}", action, job.id)));
                                    let _ = tx.send(Message::Job(Box::new(job)));
                                }
                                Err(error) => { let _ = tx.send(Message::ActionResult(format!("Maintenance failed: {error}"))); }
                            }
                        });
                    }
                    _ => {}
                }
            }
        }
    }
    stop.store(true, Ordering::Relaxed);
    reader.abort();
    let _ = input_thread.join();
    if let Some(error) = terminal_error {
        return Err(error);
    }
    Ok(())
}

fn valid_action(status: JobStatus, action: char) -> bool {
    match action {
        'p' | 'f' => status == JobStatus::Running,
        'u' => status == JobStatus::Paused,
        'c' => matches!(
            status,
            JobStatus::Running | JobStatus::Paused | JobStatus::Queued
        ),
        't' => status == JobStatus::Failed,
        _ => false,
    }
}

async fn send_job_result(
    result: Result<String>,
    id: JobId,
    client: Arc<DaemonClient>,
    tx: mpsc::UnboundedSender<Message>,
) {
    let _ = tx.send(Message::ActionResult(match result {
        Ok(value) => value,
        Err(error) => format!("Job action failed: {error}"),
    }));
    if let Ok(job) = client.get_job(id).await {
        let _ = tx.send(Message::Job(Box::new(job)));
    }
}

async fn run_input(
    input: Input,
    value: String,
    search_options: SearchOptions,
    client: Arc<DaemonClient>,
    tx: mpsc::UnboundedSender<Message>,
) {
    match input {
        Input::Search => {
            let start = Instant::now();
            match search_options.into_query(value) {
                Ok(query) => match client.search(&query).await {
                    Ok(hits) => {
                        let _ = tx.send(Message::Search(hits, start.elapsed()));
                    }
                    Err(error) => {
                        let _ = tx.send(Message::ActionResult(format!("Search failed: {error}")));
                    }
                },
                Err(error) => {
                    let _ = tx.send(Message::ActionResult(error.to_string()));
                }
            }
        }
        Input::Mine => {
            let result = async {
                let (target, rest, wing, full) = parse_mine_input(&value)?;
                let parsed = crate::client::mine::parse(target, rest)?;
                let adapters = if parsed.needs_sources() {
                    Some(client.list_sources().await?.adapters)
                } else {
                    None
                };
                let (source, options) = crate::client::mine::build(parsed, adapters.as_deref())?;
                client.submit_mine(source, options, wing, full).await
            }
            .await;
            match result {
                Ok(job) => {
                    let _ = tx.send(Message::ActionResult(format!(
                        "Mining job queued: {}",
                        job.id
                    )));
                    let _ = tx.send(Message::Job(Box::new(job)));
                }
                Err(error) => {
                    let _ = tx.send(Message::ActionResult(format!("Mine failed: {error}")));
                }
            }
        }
        Input::RunMiner => match client.run_miner(value.trim(), false).await {
            Ok(job) => {
                let _ = tx.send(Message::ActionResult(format!(
                    "Miner job queued: {}",
                    job.id
                )));
                let _ = tx.send(Message::Job(Box::new(job)));
            }
            Err(error) => {
                let _ = tx.send(Message::ActionResult(format!("Miner failed: {error}")));
            }
        },
        Input::SignIn => {
            let name = value.trim();
            match client.begin_source_auth(name).await {
                Ok(challenge) => {
                    let instruction = auth_instructions(name, &challenge);
                    let _ = tx.send(Message::AuthChallenge(instruction));
                    loop {
                        match client.wait_source_auth(name, &challenge.flow).await {
                            Ok(crate::app::FlowStatus::Pending) => {}
                            Ok(crate::app::FlowStatus::SignedIn(_)) => {
                                let _ = tx.send(Message::Info(format!("{name} signed in")));
                                if let Ok(report) = client.list_sources().await {
                                    let _ = tx.send(Message::Sources(report));
                                }
                                break;
                            }
                            Err(error) => {
                                let _ = tx.send(Message::Info(format!("Sign-in failed: {error}")));
                                break;
                            }
                        }
                    }
                }
                Err(error) => {
                    let _ = tx.send(Message::ActionResult(format!("Sign-in failed: {error}")));
                }
            }
        }
        Input::Ranking | Input::Wing => {}
    }
}

fn parse_mine_input(value: &str) -> Result<(String, Vec<String>, Option<String>, bool)> {
    let words = shell_words::split(value).map_err(|error| {
        Error::invalid_input("mine", format!("could not parse quoted input: {error}"))
    })?;
    let mut args = Vec::new();
    let mut wing = None;
    let mut full = false;
    let mut words = words.into_iter();
    while let Some(word) = words.next() {
        if word == "--full" {
            if full {
                return Err(Error::invalid_input("mine", "--full was given twice"));
            }
            full = true;
        } else if word == "--wing" || word.starts_with("--wing=") {
            if wing.is_some() {
                return Err(Error::invalid_input("mine", "--wing was given twice"));
            }
            let name = if word == "--wing" {
                words.next().unwrap_or_default()
            } else {
                word[7..].to_owned()
            };
            if name.is_empty() || name.starts_with("--") {
                return Err(Error::invalid_input("mine", "--wing needs a wing name"));
            }
            wing = Some(name);
        } else if word.starts_with("--") {
            return Err(Error::invalid_input(
                "mine",
                format!("unknown option `{word}`"),
            ));
        } else {
            args.push(word);
        }
    }
    let Some(target) = args.first().cloned() else {
        return Err(Error::invalid_input(
            "mine",
            "enter a source name or directory",
        ));
    };
    Ok((target, args.into_iter().skip(1).collect(), wing, full))
}

fn auth_instructions(name: &str, challenge: &crate::app::Challenge) -> String {
    match challenge.kind {
        crate::app::FlowKind::Device => format!(
            "Sign in {name}: open {} and enter code {}. Waiting for completion.",
            challenge
                .verification_uri
                .as_deref()
                .or(challenge.url.as_deref())
                .unwrap_or("the provider's page"),
            challenge.user_code.as_deref().unwrap_or("(see provider)")
        ),
        crate::app::FlowKind::Browser => format!(
            "Sign in {name}: open {} on the daemon's machine for its loopback redirect. Waiting for completion.",
            challenge.url.as_deref().unwrap_or("the provider's page")
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{JobProgress, Priority};
    use ratatui::backend::TestBackend;

    fn screen(app: &Console, width: u16, height: u16) -> String {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal.draw(|frame| draw(frame, app)).unwrap();
        let buffer = terminal.backend().buffer();
        (0..height)
            .map(|y| {
                (0..width)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn mine_job() -> Job {
        Job::new(
            JobKind::Mine {
                source: crate::domain::MiningSource::Directory {
                    path: "/tmp/console-test".into(),
                },
                wing: None,
                full: false,
                options: Default::default(),
            },
            Priority::Background,
            "test",
        )
    }

    #[test]
    fn actions_are_restricted_to_legal_statuses() {
        assert!(valid_action(JobStatus::Running, 'f'));
        assert!(!valid_action(JobStatus::Queued, 'f'));
        assert!(!valid_action(JobStatus::Completed, 'c'));
        assert!(valid_action(JobStatus::Paused, 'u'));
    }

    #[test]
    fn a_running_job_without_a_total_never_shows_an_invented_percentage() {
        let mut job = mine_job();
        job.apply(crate::domain::JobEvent::Claim).unwrap();
        job.progress = JobProgress {
            current: 8,
            total: None,
            message: Some("discovering".into()),
        };
        assert_eq!(progress(&job), "◌ discovering");
        job.progress.total = Some(0);
        assert_eq!(progress(&job), "◌ discovering");
        job.progress.total = Some(16);
        assert!(progress(&job).contains("8/16 discovering"));
        job.progress.total = None;
        job.apply(crate::domain::JobEvent::Cancel).unwrap();
        assert_eq!(progress(&job), "[cancelled] discovering");
    }

    #[test]
    fn a_snapshot_replaces_stale_jobs_after_a_reconnect() {
        let mut app = Console::default();
        let old = mine_job();
        app.receive(Message::Job(Box::new(old.clone())));
        app.receive(Message::Snapshot(Instant::now(), Vec::new()));
        assert!(
            app.jobs.is_empty(),
            "a removed job must not survive a full reconciliation"
        );
    }

    #[test]
    fn an_event_newer_than_a_snapshot_cannot_be_rolled_back_by_that_snapshot() {
        let mut app = Console::default();
        let stale = mine_job();
        let started = Instant::now();
        let mut advanced = stale.clone();
        advanced.progress.current = 7;
        app.receive(Message::Job(Box::new(advanced)));
        app.receive(Message::Snapshot(started, vec![stale]));
        assert_eq!(app.selected_job().unwrap().progress.current, 7);
    }

    #[test]
    fn a_maintenance_event_does_not_replace_the_selected_mining_job() {
        let mut app = Console::default();
        let mine = mine_job();
        app.receive(Message::Job(Box::new(mine.clone())));
        let audit = Job::new(JobKind::Audit { wing: None }, Priority::Normal, "test");
        app.receive(Message::Job(Box::new(audit.clone())));
        assert_eq!(app.selected_job().unwrap().id, mine.id);
        assert_eq!(app.selected_maintenance().unwrap().id, audit.id);
    }

    #[test]
    fn a_failed_action_does_not_change_a_job_status() {
        let mut app = Console::default();
        let job = mine_job();
        app.receive(Message::Job(Box::new(job.clone())));
        app.busy = true;
        app.receive(Message::ActionResult("Job action failed: refused".into()));
        assert_eq!(app.selected_job().unwrap().status, JobStatus::Queued);
        assert!(!app.busy);
    }

    #[test]
    fn a_device_handoff_shows_the_code_without_claiming_a_local_browser_is_required() {
        let challenge = crate::app::Challenge {
            flow: "test".into(),
            kind: crate::app::FlowKind::Device,
            user_code: Some("ABCD-1234".into()),
            verification_uri: Some("https://example.org/verify".into()),
            url: None,
            expires_in: 120,
        };
        let instructions = auth_instructions("example", &challenge);
        assert!(instructions.contains("ABCD-1234"));
        assert!(instructions.contains("https://example.org/verify"));
        assert!(!instructions.contains("daemon's machine"));
    }

    #[test]
    fn a_browser_handoff_says_where_the_redirect_must_land() {
        let challenge = crate::app::Challenge {
            flow: "test".into(),
            kind: crate::app::FlowKind::Browser,
            user_code: None,
            verification_uri: None,
            url: Some("https://example.org/authorize".into()),
            expires_in: 120,
        };
        assert!(auth_instructions("example", &challenge).contains("daemon's machine"));
    }

    #[test]
    fn mine_input_keeps_quoted_paths_and_cli_flags_before_shared_validation() {
        let (target, rest, wing, full) =
            parse_mine_input("directory '/work/a b' since=2026-09 --wing notes --full").unwrap();
        assert_eq!(target, "directory");
        assert_eq!(rest, ["/work/a b", "since=2026-09"]);
        assert_eq!(wing.as_deref(), Some("notes"));
        assert!(full);
        assert!(parse_mine_input("directory /work --wing").is_err());
    }

    #[test]
    fn narrow_and_wide_jobs_views_keep_the_selected_progress_and_failure_visible() {
        let mut app = Console::default();
        let mut running = mine_job();
        running.apply(crate::domain::JobEvent::Claim).unwrap();
        running.progress = JobProgress {
            current: 2,
            total: Some(5),
            message: Some("filing documents".into()),
        };
        app.receive(Message::Job(Box::new(running)));
        let wide = screen(&app, 120, 35);
        assert!(wide.contains("SELECTED · DETAILS"));
        assert!(wide.contains("2/5"));

        let mut failed = mine_job();
        failed.apply(crate::domain::JobEvent::Claim).unwrap();
        failed.error = Some("provider refused the request".into());
        failed.apply(crate::domain::JobEvent::Fail).unwrap();
        app.receive(Message::Job(Box::new(failed)));
        app.selected = app
            .ordered()
            .iter()
            .position(|job| job.status == JobStatus::Failed)
            .unwrap();
        let narrow = screen(&app, 72, 24);
        assert!(narrow.contains("provider refused"), "{narrow}");
        assert!(narrow.contains("MINING JOBS"));
    }

    #[test]
    fn readiness_and_maintenance_present_a_handoff_and_the_selected_report() {
        let mut app = Console {
            color: false,
            page: Page::Readiness,
            ..Console::default()
        };
        app.receive(Message::AuthChallenge(
            "open https://example.org/verify and enter ABCD-1234".into(),
        ));
        let readiness = screen(&app, 100, 32);
        assert!(readiness.contains("SIGN-IN"));
        assert!(readiness.contains("ABCD-1234"));
        assert!(readiness.contains("Provider status unknown"));

        app.page = Page::Maintenance;
        let mut audit = Job::new(JobKind::Audit { wing: None }, Priority::Normal, "test");
        audit.apply(crate::domain::JobEvent::Claim).unwrap();
        audit.result = Some(serde_json::json!({"issues": 2}));
        audit.apply(crate::domain::JobEvent::Complete).unwrap();
        app.receive(Message::Job(Box::new(audit)));
        app.maintenance_detail = true;
        assert!(screen(&app, 100, 32).contains("\"issues\": 2"));
    }

    #[test]
    fn readiness_clears_cached_sources_and_miners_when_authoritative_checks_fail() {
        let mut app = Console {
            page: Page::Readiness,
            ..Console::default()
        };
        app.receive(Message::Sources(SourcesReport {
            adapters: Vec::new(),
            sources: Vec::new(),
        }));
        app.receive(Message::Miners(MinersReport {
            miners: Vec::new(),
            config_file: None,
            error: None,
        }));
        assert!(app.sources.is_some() && app.miners.is_some());

        app.receive(Message::SourcesUnavailable("daemon disconnected".into()));
        app.receive(Message::Miners(MinersReport {
            miners: Vec::new(),
            config_file: None,
            error: Some("config could not be read".into()),
        }));
        assert!(app.sources.is_none() && app.miners.is_none());
        let rendered = screen(&app, 100, 30);
        assert!(rendered.contains("status unknown"));
        assert!(!rendered.contains("CONFIGURED MINERS"));
    }
}
