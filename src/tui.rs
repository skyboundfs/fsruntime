use crate::pkg::{Community, Gauge, GaugeKind, Package, Target};
use crate::gui::{GuiCommand, GuiSink};
use crate::runtime::{Runtime, RuntimeRequest};
use anyhow::Result;
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyModifiers};
use crossterm::terminal;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Wrap};
use ratatui::Frame;
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, Sender};
use std::time::Duration;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    Packages,
    Aircraft,
    Gauges,
    Instances,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Browser,
    Runtime,
    SimVars,
    Log,
}

pub struct App {
    pub community: Option<Community>,
    pub root: PathBuf,
    pub focus: Focus,
    pub tab: Tab,
    pub package_state: ListState,
    pub aircraft_state: ListState,
    pub gauge_state: ListState,
    pub instance_state: ListState,
    pub log_scroll: u16,
    pub simvar_scroll: u16,
    pub status: String,
    pub version_override: Option<Target>,
    pub input: Option<InputMode>,
}

pub enum InputMode {
    Path(String),
    SimVarName(String),
    SimVarValue(String),
}

impl App {
    pub fn new(root: PathBuf) -> Self {
        let mut package_state = ListState::default();
        package_state.select(Some(0));
        let mut aircraft_state = ListState::default();
        aircraft_state.select(Some(0));
        let mut gauge_state = ListState::default();
        gauge_state.select(Some(0));
        let mut instance_state = ListState::default();
        instance_state.select(Some(0));
        App {
            community: None,
            root,
            focus: Focus::Packages,
            tab: Tab::Browser,
            package_state,
            aircraft_state,
            gauge_state,
            instance_state,
            log_scroll: 0,
            simvar_scroll: 0,
            status: String::new(),
            version_override: None,
            input: None,
        }
    }

    fn selected_package(&self) -> Option<&Package> {
        let community = self.community.as_ref()?;
        let index = self.package_state.selected()?;
        community.packages.get(index)
    }

    fn selected_aircraft(&self) -> Option<&crate::pkg::Aircraft> {
        let package = self.selected_package()?;
        let index = self.aircraft_state.selected()?;
        package.aircraft.get(index)
    }

    fn selected_gauge(&self) -> Option<&Gauge> {
        let aircraft = self.selected_aircraft()?;
        let index = self.gauge_state.selected()?;
        aircraft.gauges().nth(index)
    }

    fn clamp_selection(&mut self) {
        let package_len = self.community.as_ref().map(|c| c.packages.len()).unwrap_or(0);
        clamp(&mut self.package_state, package_len);
        let aircraft_len = self.selected_package().map(|p| p.aircraft.len()).unwrap_or(0);
        clamp(&mut self.aircraft_state, aircraft_len);
        let gauge_len = self.selected_aircraft().map(|a| a.gauges().count()).unwrap_or(0);
        clamp(&mut self.gauge_state, gauge_len);
    }
}

fn clamp(state: &mut ListState, len: usize) {
    if len == 0 {
        state.select(None);
        return;
    }
    let current = state.selected().unwrap_or(0);
    state.select(Some(current.min(len - 1)));
}

pub fn run(
    runtime: &mut Runtime,
    request_tx: Sender<RuntimeRequest>,
    request_rx: Receiver<RuntimeRequest>,
    gui: GuiSink,
    root: PathBuf,
    launch: Option<(String, Option<String>)>,
) -> Result<()> {
    terminal::enable_raw_mode()?;
    let mut stdout = std::io::stdout();
    crossterm::execute!(stdout, terminal::EnterAlternateScreen)?;
    let backend = ratatui::backend::CrosstermBackend::new(stdout);
    let mut terminal = ratatui::Terminal::new(backend)?;
    terminal.clear()?;

    let mut app = App::new(root.clone());
    if !root.as_os_str().is_empty() {
        match runtime.scan(&root) {
            Ok(_) => {
                app.community = runtime.community.clone();
                app.status = format!("scanned {}", root.display());
            }
            Err(e) => app.status = format!("scan failed: {e}"),
        }
    }

    if let Some((package, aircraft)) = launch {
        auto_launch(runtime, &mut app, &gui, &package, aircraft.as_deref());
    }

    let result = event_loop(runtime, &mut terminal, &mut app, request_tx, request_rx, gui);

    terminal::disable_raw_mode()?;
    crossterm::execute!(terminal.backend_mut(), terminal::LeaveAlternateScreen)?;
    terminal.show_cursor()?;
    result
}

fn auto_launch(
    runtime: &mut Runtime,
    app: &mut App,
    gui: &GuiSink,
    package: &str,
    aircraft: Option<&str>,
) {
    let plane = aircraft
        .map(str::to_string)
        .or_else(|| {
            app.community
                .as_ref()?
                .package(package)?
                .aircraft
                .first()
                .map(|a| a.key.clone())
        });
    let Some(plane) = plane else {
        app.status = format!("no aircraft in package {package}");
        return;
    };
    let variant = std::env::var("MSFSRUNNER_VARIANT").ok();
    match runtime.start_aircraft(package, &plane, variant.as_deref()) {
        Ok(ids) => {
            for id in &ids {
                let session = match runtime.sessions.get(id) {
                    Some(s) => s,
                    None => continue,
                };
                if session.error.is_some() {
                    continue;
                }
                let html = instrument_url(runtime, *id, &session.gauge);
                let title = session.title.clone();
                let (w, h) = (session.gauge.width, session.gauge.height);
                gui.send(GuiCommand::OpenInstrument {
                    session: *id,
                    title,
                    html,
                    width: w,
                    height: h,
                });
            }
            app.status = format!("launched {plane} ({} sessions)", ids.len());
        }
        Err(e) => app.status = format!("launch failed: {e}"),
    }
}

fn event_loop(
    runtime: &mut Runtime,
    terminal: &mut ratatui::Terminal<ratatui::backend::CrosstermBackend<std::io::Stdout>>,
    app: &mut App,
    request_tx: Sender<RuntimeRequest>,
    request_rx: Receiver<RuntimeRequest>,
    gui: GuiSink,
) -> Result<()> {
    loop {
        while let Ok(request) = request_rx.try_recv() {
            runtime.answer(request);
        }
        runtime.pump();
        terminal.draw(|frame| draw(frame, app, runtime))?;

        if event::poll(Duration::from_millis(33))? {
            let ev = event::read()?;
            if let Event::Key(key) = ev {
                if !handle_key(app, runtime, key, &request_tx, &gui)? {
                    return Ok(());
                }
            }
        }

        runtime.tick(0.033);
    }
}

fn handle_key(
    app: &mut App,
    runtime: &mut Runtime,
    key: KeyEvent,
    request_tx: &Sender<RuntimeRequest>,
    gui: &GuiSink,
) -> Result<bool> {
    if let Some(mode) = app.input.take() {
        return Ok(handle_input(app, runtime, mode, key, request_tx));
    }

    if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
        gui.send(GuiCommand::Quit);
        return Ok(false);
    }

    match key.code {
        KeyCode::Char('q') => {
            gui.send(GuiCommand::Quit);
            return Ok(false);
        }
        KeyCode::Tab => {
            app.tab = match app.tab {
                Tab::Browser => Tab::Runtime,
                Tab::Runtime => Tab::SimVars,
                Tab::SimVars => Tab::Log,
                Tab::Log => Tab::Browser,
            };
        }
        KeyCode::Char('1') => app.tab = Tab::Browser,
        KeyCode::Char('2') => app.tab = Tab::Runtime,
        KeyCode::Char('3') => app.tab = Tab::SimVars,
        KeyCode::Char('4') => app.tab = Tab::Log,
        KeyCode::Char('r') => {
            let root = app.root.clone();
            match runtime.scan(&root) {
                Ok(n) => {
                    app.community = runtime.community.clone();
                    app.status = format!("rescanned: {n} packages");
                    app.clamp_selection();
                }
                Err(e) => app.status = format!("rescan failed: {e}"),
            }
        }
        KeyCode::Char('o') => {
            app.input = Some(InputMode::Path(app.root.display().to_string()));
        }
        KeyCode::Char('j') | KeyCode::Down => move_selection(app, 1),
        KeyCode::Char('k') | KeyCode::Up => move_selection(app, -1),
        KeyCode::Char('l') | KeyCode::Right => next_focus(app),
        KeyCode::Char('h') | KeyCode::Left => prev_focus(app),
        KeyCode::Enter => {
            launch(app, runtime, gui, request_tx);
        }
        KeyCode::Char('s') => {
            let _ = request_tx.send(RuntimeRequest::StopAll);
            gui.send(GuiCommand::CloseAll);
            app.status = "stopped all instances".to_string();
        }
        KeyCode::Char('v') => {
            app.version_override = match app.version_override {
                None => Some(Target::Msfs2020),
                Some(Target::Msfs2020) => Some(Target::Msfs2024),
                Some(Target::Msfs2024) => Some(Target::Both),
                Some(Target::Both) => Some(Target::Unknown),
                Some(Target::Unknown) => None,
            };
            app.status = match app.version_override {
                Some(t) => format!("target version forced to {}", t.label()),
                None => "target version follows detection".to_string(),
            };
        }
        KeyCode::Char('i') => {
            app.tab = Tab::SimVars;
            app.input = Some(InputMode::SimVarName(String::new()));
        }
        _ => {}
    }
    Ok(true)
}

fn handle_input(
    app: &mut App,
    runtime: &mut Runtime,
    mode: InputMode,
    key: KeyEvent,
    request_tx: &Sender<RuntimeRequest>,
) -> bool {
    match mode {
        InputMode::Path(mut buffer) => match key.code {
            KeyCode::Esc => {}
            KeyCode::Enter => {
                let path = PathBuf::from(buffer);
                match runtime.scan(&path) {
                    Ok(n) => {
                        app.root = path;
                        app.community = runtime.community.clone();
                        app.status = format!("scanned {n} packages");
                        app.clamp_selection();
                    }
                    Err(e) => app.status = format!("scan failed: {e}"),
                }
            }
            KeyCode::Backspace => {
                buffer.pop();
                app.input = Some(InputMode::Path(buffer));
            }
            KeyCode::Char(c) => {
                buffer.push(c);
                app.input = Some(InputMode::Path(buffer));
            }
            _ => app.input = Some(InputMode::Path(buffer)),
        },
        InputMode::SimVarName(mut buffer) => match key.code {
            KeyCode::Esc => {}
            KeyCode::Enter => {
                app.input = Some(InputMode::SimVarValue(buffer));
            }
            KeyCode::Backspace => {
                buffer.pop();
                app.input = Some(InputMode::SimVarName(buffer));
            }
            KeyCode::Char(c) => {
                buffer.push(c);
                app.input = Some(InputMode::SimVarName(buffer));
            }
            _ => app.input = Some(InputMode::SimVarName(buffer)),
        },
        InputMode::SimVarValue(name) => match key.code {
            KeyCode::Esc => {}
            KeyCode::Enter => {
                if let Some(session) = runtime.sessions.keys().next().copied() {
                    let _ = request_tx.send(RuntimeRequest::SimVarSet {
                        session,
                        name: name.clone(),
                        value: 0.0,
                    });
                    app.status = format!("reset {name}");
                }
            }
            KeyCode::Backspace => {
                let mut n = name;
                n.pop();
                app.input = Some(InputMode::SimVarName(n));
            }
            _ => {}
        },
    }
    true
}

fn move_selection(app: &mut App, delta: i32) {
    let len = match app.focus {
        Focus::Packages => app.community.as_ref().map(|c| c.packages.len()).unwrap_or(0),
        Focus::Aircraft => app.selected_package().map(|p| p.aircraft.len()).unwrap_or(0),
        Focus::Gauges => app.selected_aircraft().map(|a| a.gauges().count()).unwrap_or(0),
        Focus::Instances => 0,
    };
    let state = match app.focus {
        Focus::Packages => &mut app.package_state,
        Focus::Aircraft => &mut app.aircraft_state,
        Focus::Gauges => &mut app.gauge_state,
        Focus::Instances => &mut app.instance_state,
    };
    if len == 0 {
        return;
    }
    let current = state.selected().unwrap_or(0) as i32;
    let next = (current + delta).clamp(0, len as i32 - 1) as usize;
    state.select(Some(next));
}

fn next_focus(app: &mut App) {
    app.focus = match app.focus {
        Focus::Packages => Focus::Aircraft,
        Focus::Aircraft => Focus::Gauges,
        Focus::Gauges => Focus::Instances,
        Focus::Instances => Focus::Packages,
    };
}

fn prev_focus(app: &mut App) {
    app.focus = match app.focus {
        Focus::Packages => Focus::Instances,
        Focus::Aircraft => Focus::Packages,
        Focus::Gauges => Focus::Aircraft,
        Focus::Instances => Focus::Gauges,
    };
}

fn launch(app: &mut App, runtime: &mut Runtime, gui: &GuiSink, _request_tx: &Sender<RuntimeRequest>) {
    let Some(gauge) = app.selected_gauge().cloned() else {
        app.status = "nothing selected to launch".to_string();
        return;
    };
    let Some(package) = app.selected_package() else {
        app.status = "no package selected".to_string();
        return;
    };
    let package_name = package.name.clone();
    let title = gauge.title();
    match runtime.start_gauge(&package_name, &title, gauge.clone()) {
        Ok(id) => {
            let session = runtime.sessions.get(&id);
            if let Some(session) = session {
                if let Some(error) = &session.error {
                    app.status = format!("session {id} failed: {error}");
                    return;
                }
            }
            let html = instrument_url(runtime, id, &gauge);
            gui.send(GuiCommand::OpenInstrument {
                session: id,
                title,
                html,
                width: gauge.width,
                height: gauge.height,
            });
            app.status = format!("launched session {id}");
        }
        Err(e) => app.status = format!("launch failed: {e}"),
    }
}

fn instrument_url(runtime: &Runtime, id: u64, gauge: &Gauge) -> String {
    let base = runtime.bridge.base_url();
    match &gauge.kind {
        GaugeKind::Html { page, query, .. } => {
            let q = if query.is_empty() { String::new() } else { format!("?{query}") };
            format!("{base}/instrument/{id}/{page}{q}")
        }
        GaugeKind::Wasm { .. } => format!("{base}/wasm-instrument.html?session={id}"),
    }
}

fn draw(frame: &mut Frame, app: &mut App, runtime: &Runtime) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Min(6), Constraint::Length(3)])
        .split(frame.area());

    draw_header(frame, chunks[0], app);
    match app.tab {
        Tab::Browser => draw_browser(frame, chunks[1], app),
        Tab::Runtime => draw_runtime(frame, chunks[1], app, runtime),
        Tab::SimVars => draw_simvars(frame, chunks[1], app, runtime),
        Tab::Log => draw_log(frame, chunks[1], app, runtime),
    }
    draw_status(frame, chunks[2], app, runtime);
}

fn draw_header(frame: &mut Frame, area: Rect, app: &App) {
    let tabs = [
        ("1 Browser", Tab::Browser),
        ("2 Runtime", Tab::Runtime),
        ("3 SimVars", Tab::SimVars),
        ("4 Log", Tab::Log),
    ];
    let mut spans = Vec::new();
    for (label, tab) in tabs {
        let style = if app.tab == tab {
            Style::default().fg(Color::Black).bg(Color::Cyan).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::Gray)
        };
        spans.push(Span::styled(format!(" {label} "), style));
        spans.push(Span::raw(" "));
    }
    let forced = match app.version_override {
        Some(t) => format!("target:{}", t.label()),
        None => "target:auto".to_string(),
    };
    spans.push(Span::styled(
        format!("  {forced}"),
        Style::default().fg(Color::Yellow),
    ));
    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn draw_browser(frame: &mut Frame, area: Rect, app: &mut App) {
    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(28), Constraint::Percentage(30), Constraint::Percentage(42)])
        .split(area);

    let packages: Vec<ListItem> = app
        .community
        .as_ref()
        .map(|c| {
            c.packages
                .iter()
                .map(|p| {
                    let color = match p.classification.target {
                        Target::Msfs2020 => Color::Green,
                        Target::Msfs2024 => Color::Cyan,
                        Target::Both => Color::Yellow,
                        Target::Unknown => Color::Red,
                    };
                    ListItem::new(Line::from(vec![
                        Span::styled(format!("{} ", p.name), Style::default().fg(Color::White)),
                        Span::styled(p.classification.target.label().to_string(), Style::default().fg(color)),
                    ]))
                })
                .collect()
        })
        .unwrap_or_default();
    let package_list = List::new(packages)
        .block(block("Packages", app.focus == Focus::Packages))
        .highlight_style(Style::default().bg(Color::DarkGray));
    frame.render_stateful_widget(package_list, cols[0], &mut app.package_state);

    let aircraft: Vec<ListItem> = app
        .selected_package()
        .map(|p| {
            p.aircraft
                .iter()
                .map(|a| {
                    ListItem::new(Line::from(vec![
                        Span::raw(a.key.clone()),
                        Span::styled(
                            format!("  {} variants / {} gauges", a.variants.len(), a.gauges().count()),
                            Style::default().fg(Color::DarkGray),
                        ),
                    ]))
                })
                .collect()
        })
        .unwrap_or_default();
    let aircraft_list = List::new(aircraft)
        .block(block("Aircraft", app.focus == Focus::Aircraft))
        .highlight_style(Style::default().bg(Color::DarkGray));
    frame.render_stateful_widget(aircraft_list, cols[1], &mut app.aircraft_state);

    let gauges: Vec<ListItem> = app
        .selected_aircraft()
        .map(|a| {
            a.gauges()
                .map(|g| {
                    let kind = match &g.kind {
                        GaugeKind::Wasm { .. } => "wasm",
                        GaugeKind::Html { .. } => "html",
                    };
                    let color = if matches!(&g.kind, GaugeKind::Wasm { module_path: None, .. }) {
                        Color::Red
                    } else {
                        Color::Gray
                    };
                    ListItem::new(Line::from(vec![
                        Span::styled(format!("{:<5}", kind), Style::default().fg(color)),
                        Span::raw(format!("{}x{} ", g.width, g.height)),
                        Span::raw(g.title()),
                    ]))
                })
                .collect()
        })
        .unwrap_or_default();
    let gauge_list = List::new(gauges)
        .block(block("Instruments", app.focus == Focus::Gauges))
        .highlight_style(Style::default().bg(Color::DarkGray));
    frame.render_stateful_widget(gauge_list, cols[2], &mut app.gauge_state);
}

fn draw_runtime(frame: &mut Frame, area: Rect, app: &mut App, runtime: &Runtime) {
    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(40), Constraint::Percentage(60)])
        .split(area);

    let instances: Vec<ListItem> = runtime
        .sessions
        .values()
        .map(|s| {
            let state = if s.error.is_some() {
                Span::styled("error", Style::default().fg(Color::Red))
            } else if s.instance.is_some() {
                Span::styled("running", Style::default().fg(Color::Green))
            } else {
                Span::styled("declared", Style::default().fg(Color::DarkGray))
            };
            ListItem::new(Line::from(vec![
                Span::raw(format!("{} ", s.id)),
                state,
                Span::raw(format!("  {}", s.title)),
            ]))
        })
        .collect();
    let list = List::new(instances)
        .block(block("Instances", app.focus == Focus::Instances))
        .highlight_style(Style::default().bg(Color::DarkGray));
    frame.render_stateful_widget(list, cols[0], &mut app.instance_state);

    let selected = app
        .instance_state
        .selected()
        .and_then(|i| runtime.sessions.values().nth(i));
    let mut lines: Vec<Line> = Vec::new();
    if let Some(session) = selected {
        lines.push(Line::from(vec![
            Span::styled("session ", Style::default().fg(Color::DarkGray)),
            Span::raw(session.id.to_string()),
        ]));
        lines.push(Line::from(vec![
            Span::styled("package ", Style::default().fg(Color::DarkGray)),
            Span::raw(session.package.clone()),
        ]));
        lines.push(Line::from(vec![
            Span::styled("title   ", Style::default().fg(Color::DarkGray)),
            Span::raw(session.title.clone()),
        ]));
        lines.push(Line::from(vec![
            Span::styled("surface ", Style::default().fg(Color::DarkGray)),
            Span::raw(format!("{}x{}", session.gauge.width, session.gauge.height)),
        ]));
        if let Some(err) = &session.error {
            lines.push(Line::from(Span::styled(format!("error: {err}"), Style::default().fg(Color::Red))));
        }
        if !session.unsupported.is_empty() {
            lines.push(Line::from(Span::styled(
                format!("unsupported imports: {}", session.unsupported.len()),
                Style::default().fg(Color::Yellow),
            )));
            for name in session.unsupported.iter().take(12) {
                lines.push(Line::from(Span::styled(format!("  {name}"), Style::default().fg(Color::Yellow))));
            }
        }
        if let Some(instance) = &session.instance {
            let sim = instance.sim();
            lines.push(Line::from(Span::styled("simvars:", Style::default().fg(Color::DarkGray))));
            for (name, value) in [("PLANE ALTITUDE", 0), ("AIRSPEED INDICATED", 1), ("PLANE HEADING DEGREES TRUE", 2)] {
                let _ = value;
                lines.push(Line::from(format!("  {name} = {:.2}", sim.get_named(name))));
            }
            let lvars: Vec<String> = sim.lvars().take(8).map(|(n, v)| format!("  L:{n} = {v:.2}")).collect();
            if !lvars.is_empty() {
                lines.push(Line::from(Span::styled("lvars:", Style::default().fg(Color::DarkGray))));
                for l in lvars {
                    lines.push(Line::from(l));
                }
            }
        }
    } else {
        lines.push(Line::from("no instance selected"));
    }
    let detail = Paragraph::new(lines)
        .block(block("Session", false))
        .wrap(Wrap { trim: false });
    frame.render_widget(detail, cols[1]);
}

fn draw_simvars(frame: &mut Frame, area: Rect, app: &mut App, runtime: &Runtime) {
    let session = runtime.sessions.values().next();
    let mut lines: Vec<Line> = Vec::new();
    if let Some(session) = session {
        if let Some(instance) = &session.instance {
            let sim = instance.sim();
            lines.push(Line::from(Span::styled(
                format!("session {} - {} simvars", session.id, sim.simvar_table().len()),
                Style::default().fg(Color::DarkGray),
            )));
            for def in sim.simvar_table().iter().take(400) {
                lines.push(Line::from(vec![
                    Span::styled(format!("{:<38}", def.name), Style::default().fg(Color::White)),
                    Span::styled(format!("{:<18}", def.unit), Style::default().fg(Color::DarkGray)),
                    Span::raw(format!("{:>12.3}", def.value)),
                ]));
            }
            let lvars: Vec<Line> = sim
                .lvars()
                .map(|(n, v)| Line::from(format!("L:{n:<34} {v:>12.3}")))
                .collect();
            if !lvars.is_empty() {
                lines.push(Line::from(""));
                lines.push(Line::from(Span::styled("LVars", Style::default().fg(Color::Cyan))));
                lines.extend(lvars);
            }
        } else {
            lines.push(Line::from("selected session has no live module"));
        }
    } else {
        lines.push(Line::from("launch a session to inspect simulated state"));
    }
    let paragraph = Paragraph::new(lines)
        .block(block("Simulated state", false))
        .scroll((app.simvar_scroll, 0));
    frame.render_widget(paragraph, area);
}

fn draw_log(frame: &mut Frame, area: Rect, app: &mut App, runtime: &Runtime) {
    let lines: Vec<Line> = runtime
        .log
        .iter()
        .rev()
        .skip(app.log_scroll as usize)
        .take(area.height as usize)
        .map(|l| Line::from(l.as_str()))
        .collect();
    let paragraph = Paragraph::new(lines).block(block("Runtime log", false));
    frame.render_widget(paragraph, area);
}

fn draw_status(frame: &mut Frame, area: Rect, app: &App, runtime: &Runtime) {
    let input_line = match &app.input {
        Some(InputMode::Path(buf)) => format!("path> {buf}"),
        Some(InputMode::SimVarName(buf)) => format!("simvar name> {buf}"),
        Some(InputMode::SimVarValue(name)) => format!("set {name} to> "),
        None => String::new(),
    };
    let hints = if input_line.is_empty() {
        "j/k move  h/l focus  enter launch  o open folder  r rescan  s stop all  v version  tab switch  q quit".to_string()
    } else {
        "enter confirm  esc cancel".to_string()
    };
    let lines = vec![
        Line::from(if input_line.is_empty() { app.status.clone() } else { input_line }),
        Line::from(Span::styled(hints, Style::default().fg(Color::DarkGray))),
        Line::from(Span::styled(
            format!(
                "{} sessions  {} modules  bridge {}",
                runtime.sessions.len(),
                runtime.modules.len(),
                runtime.bridge.base_url()
            ),
            Style::default().fg(Color::DarkGray),
        )),
    ];
    frame.render_widget(Paragraph::new(lines).block(Block::default().borders(Borders::TOP)), area);
}

fn block(title: &str, focused: bool) -> Block<'_> {
    let style = if focused {
        Style::default().fg(Color::Cyan)
    } else {
        Style::default().fg(Color::DarkGray)
    };
    Block::default().borders(Borders::ALL).title(title).border_style(style)
}

