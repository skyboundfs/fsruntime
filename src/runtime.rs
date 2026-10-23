use crate::bridge::Bridge;
use crate::pkg::{Community, Gauge, GaugeKind, Target};
use crate::render::Surface;
use crate::sim::SimState;
use crate::wasmhost::{EngineConfig, GaugeInstance, HostEvent, ModuleReport, WasmHost};
use anyhow::{anyhow, Result};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Arc, Mutex};

#[derive(Debug)]
pub enum RuntimeRequest {
    SimVar { session: u64, name: String, unit: String },
    SimVarSet { session: u64, name: String, value: f64 },
    LVar { session: u64, name: String },
    LVarSet { session: u64, name: String, value: f64 },
    Event { session: u64, name: String },
    CommBusCall { session: u64, name: String, payload: String, broadcast: u32 },
    CommBusPoll { session: u64, since: u64 },
    Log { session: u64, message: String },
    Context { session: u64 },
    SurfacePng { session: u64 },
    Mouse { session: u64, x: f32, y: f32, flags: u32 },
    Unknown { op: String },
    WithReply(Box<RuntimeRequest>, Sender<RuntimeResponse>),
    StartAircraft { package: String, aircraft: String, variant: Option<String> },
    StartModule { path: PathBuf, gauge: String },
    OpenInstrument { session: u64, index: usize },
    StopAll,
    Tick(f64),
    Quit,
}

#[derive(Debug, Clone)]
pub enum RuntimeResponse {
    Value(f64),
    Text(String),
    List(Vec<PollItem>),
    Binary(Vec<u8>),
    Error(String),
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct PollItem {
    pub seq: u64,
    pub name: String,
    pub payload: String,
}

pub struct InstrumentSession {
    pub id: u64,
    pub package: String,
    pub title: String,
    pub gauge: Gauge,
    pub instance: Option<GaugeInstance>,
    pub sim: Arc<Mutex<SimState>>,
    pub context: u64,
    pub error: Option<String>,
    pub unsupported: Vec<String>,
    pub last_update: std::time::Duration,
    pub last_draw: std::time::Duration,
    pub frame: u64,
    png_cache: Option<(u64, Vec<u8>)>,
}

pub struct ModuleEntry {
    pub path: PathBuf,
    pub report: ModuleReport,
    pub error: Option<String>,
}

pub struct Runtime {
    pub host: WasmHost,
    pub bridge: Bridge,
    pub community: Option<Community>,
    pub sessions: HashMap<u64, InstrumentSession>,
    pub modules: Vec<ModuleEntry>,
    pub next_session: u64,
    pub next_context: u64,
    pub events_rx: Receiver<HostEvent>,
    events_tx: Sender<HostEvent>,
    pub log: Vec<String>,
    pub commbus_seq: u64,
    pub commbus_out: Vec<PollItem>,
    pub running: bool,
}

impl Runtime {
    pub fn new() -> Result<Self> {
        let (events_tx, events_rx) = std::sync::mpsc::channel();
        let (request_tx, _request_rx) = std::sync::mpsc::channel();
        let bridge = Bridge::start()?;
        bridge.set_requests(request_tx.clone());
        let host = WasmHost::new(EngineConfig::default())?;
        Ok(Runtime {
            host,
            bridge,
            community: None,
            sessions: HashMap::new(),
            modules: Vec::new(),
            next_session: 1,
            next_context: 1,
            events_rx,
            events_tx,
            log: Vec::new(),
            commbus_seq: 0,
            commbus_out: Vec::new(),
            running: false,
        })
    }

    pub fn scan(&mut self, root: &std::path::Path) -> Result<usize> {
        let community = Community::scan(root)?;
        let count = community.packages.len();
        self.log.push(format!("scanned {} packages in {}", count, root.display()));
        for issue in &community.issues {
            self.log.push(format!("scan issue: {issue}"));
        }
        self.community = Some(community);
        self.index_modules();
        Ok(count)
    }

    fn index_modules(&mut self) {
        self.modules.clear();
        let Some(community) = &self.community else { return };
        let mut paths: Vec<PathBuf> = Vec::new();
        for package in &community.packages {
            for aircraft in &package.aircraft {
                for module in &aircraft.wasm_modules {
                    if !paths.contains(module) {
                        paths.push(module.clone());
                    }
                }
            }
        }
        for path in paths {
            match self.host.describe(&path) {
                Ok(report) => self.modules.push(ModuleEntry { path, report, error: None }),
                Err(e) => self.modules.push(ModuleEntry {
                    path,
                    report: ModuleReport { imports: Vec::new(), exports: Vec::new(), gauge_names: Vec::new() },
                    error: Some(e.to_string()),
                }),
            }
        }
    }

    pub fn start_gauge(
        &mut self,
        package: &str,
        title: &str,
        gauge: Gauge,
    ) -> Result<u64> {
        let id = self.next_session;
        self.next_session += 1;
        let context = self.next_context;
        self.next_context += 1;

        let community = self.community.as_ref().ok_or_else(|| anyhow!("no community folder scanned"))?;
        let pkg = community
            .package(package)
            .ok_or_else(|| anyhow!("unknown package {package}"))?;
        let html_ui = crate::pkg::find_dir_ci(&pkg.dir, "html_ui").unwrap_or_else(|| pkg.dir.join("html_ui"));
        let panel_dir = gauge.panel.parent().map(Path::to_path_buf).unwrap_or_else(|| pkg.dir.clone());
        self.bridge.register(id, package, html_ui, panel_dir);

        let mut session = InstrumentSession {
            id,
            package: package.to_string(),
            title: title.to_string(),
            gauge: gauge.clone(),
            instance: None,
            sim: Arc::new(Mutex::new(SimState::new())),
            context,
            error: None,
            unsupported: Vec::new(),
            last_update: std::time::Duration::ZERO,
            last_draw: std::time::Duration::ZERO,
            frame: 0,
            png_cache: None,
        };

        match &gauge.kind {
            GaugeKind::Wasm { gauge: name, params, module_path, .. } => {
                let Some(path) = module_path.clone() else {
                    session.error = Some(format!("wasm module not found for gauge {name}"));
                    self.sessions.insert(id, session);
                    return Ok(id);
                };
                match self.host.instantiate(
                    &path,
                    name,
                    context,
                    gauge.width,
                    gauge.height,
                    params,
                    self.events_tx.clone(),
                ) {
                    Ok(mut instance) => {
                        let sim = instance.sim_handle();
                        session.sim = sim;
                        match instance.init() {
                            Ok(code) => {
                                self.log.push(format!("{title}: gauge_init -> {code}"));
                            }
                            Err(e) => {
                                session.error = Some(e.to_string());
                            }
                        }
                        session.unsupported = instance.unsupported.clone();
                        session.instance = Some(instance);
                    }
                    Err(e) => {
                        session.error = Some(e.to_string());
                    }
                }
            }
            GaugeKind::Html { page_path, .. } => {
                if page_path.is_none() {
                    session.error = Some("instrument page not found in package".to_string());
                }
            }
        }

        self.log.push(format!("session {id} {title}"));
        self.sessions.insert(id, session);
        Ok(id)
    }

    pub fn start_aircraft(&mut self, package: &str, aircraft: &str, variant: Option<&str>) -> Result<Vec<u64>> {
        let community = self.community.as_ref().ok_or_else(|| anyhow!("no community scanned"))?;
        let pkg = community.package(package).ok_or_else(|| anyhow!("unknown package"))?;
        let plane = pkg
            .aircraft
            .iter()
            .find(|a| a.key == aircraft)
            .ok_or_else(|| anyhow!("unknown aircraft {aircraft}"))?;
        let key = plane.key.clone();
        let variant = match variant {
            Some(name) => plane
                .variants
                .iter()
                .find(|v| v.title.eq_ignore_ascii_case(name) || v.label().eq_ignore_ascii_case(name))
                .ok_or_else(|| anyhow!("unknown variant {name}"))?,
            None => plane.variants.first().ok_or_else(|| anyhow!("{aircraft} has no variants"))?,
        };
        let gauges: Vec<Gauge> = variant.gauges.clone();
        if gauges.is_empty() {
            return Err(anyhow!("{aircraft} declares no gauges"));
        }
        let mut ids = Vec::new();
        for gauge in gauges {
            let title = format!("{} {}", key, gauge.title());
            let id = self.start_gauge(package, &title, gauge)?;
            ids.push(id);
        }
        Ok(ids)
    }

    pub fn pump(&mut self) {
        while let Ok(event) = self.events_rx.try_recv() {
            match event {
                HostEvent::CommBusCall { name, payload, broadcast } => {
                    self.dispatch_commbus(name, payload, broadcast);
                }
                HostEvent::KeyEvent { id } => {
                    self.log.push(format!("wasm triggered key event #{id}"));
                }
                HostEvent::Log(message) => self.log.push(message),
                HostEvent::Frame { .. } => {}
                HostEvent::Unsupported(name) => {
                    self.log.push(format!("unsupported host function: {name}"));
                }
            }
        }
    }

    fn dispatch_commbus(&mut self, name: String, payload: Vec<u8>, broadcast: u32) {
        if broadcast & crate::wasmhost::FS_COMMBUS_BROADCAST_WASM != 0 {
            for session in self.sessions.values_mut() {
                if let Some(instance) = session.instance.as_mut() {
                    let _ = instance.deliver_commbus(&name, &payload);
                }
            }
        }
        if broadcast & crate::wasmhost::FS_COMMBUS_BROADCAST_JS != 0 {
            self.commbus_seq += 1;
            self.commbus_out.push(PollItem {
                seq: self.commbus_seq,
                name,
                payload: String::from_utf8_lossy(&payload).to_string(),
            });
            if self.commbus_out.len() > 500 {
                self.commbus_out.remove(0);
            }
        }
    }

    pub fn tick(&mut self, dt: f32) {
        let ids: Vec<u64> = self.sessions.keys().copied().collect();
        for id in ids {
            self.tick_session(id, dt);
        }
        self.pump();
    }

    pub fn tick_session(&mut self, id: u64, dt: f32) {
        let Some(session) = self.sessions.get_mut(&id) else { return };
        if let Some(instance) = session.instance.as_mut() {
            instance.sim_mut().tick(dt as f64);
            let update_start = std::time::Instant::now();
            if let Err(e) = instance.update(dt) {
                session.error.get_or_insert_with(|| e.to_string());
                return;
            }
            session.last_update = update_start.elapsed();
            let draw_start = std::time::Instant::now();
            let elapsed = instance.sim().elapsed;
            if let Err(e) = instance.draw(elapsed, dt as f64) {
                session.error.get_or_insert_with(|| e.to_string());
            }
            session.last_draw = draw_start.elapsed();
            session.frame = session.frame.wrapping_add(1);
        }
    }

    pub fn surface(&self, session: u64) -> Option<&Surface> {
        self.sessions.get(&session).and_then(|s| s.instance.as_ref()).map(|i| i.surface())
    }

    pub fn surface_png(&mut self, session: u64) -> Option<Vec<u8>> {
        let entry = self.sessions.get_mut(&session)?;
        let frame = entry.frame;
        if let Some((cached, bytes)) = &entry.png_cache {
            if *cached == frame {
                return Some(bytes.clone());
            }
        }
        let (width, height, pixels) = entry.instance.as_ref()?.take_surface_copy();
        if width == 0 || height == 0 {
            return None;
        }
        let mut out = Vec::new();
        {
            let mut encoder = png::Encoder::new(&mut out, width, height);
            encoder.set_color(png::ColorType::Rgba);
            encoder.set_depth(png::BitDepth::Eight);
            encoder.set_compression(png::Compression::Fast);
            let mut writer = encoder.write_header().ok()?;
            writer.write_image_data(&pixels).ok()?;
        }
        entry.png_cache = Some((frame, out.clone()));
        Some(out)
    }

    pub fn answer(&mut self, request: RuntimeRequest) -> RuntimeResponse {
        match request {
            RuntimeRequest::SimVar { session, name, .. } => {
                match self.sessions.get(&session) {
                    Some(s) => {
                        let sim = s.sim.lock().unwrap();
                        RuntimeResponse::Value(sim.get_named(&name))
                    }
                    None => RuntimeResponse::Value(0.0),
                }
            }
            RuntimeRequest::SimVarSet { session, name, value } => {
                if let Some(s) = self.sessions.get_mut(&session) {
                    s.sim.lock().unwrap().set_simvar_by_name(&name, value);
                    RuntimeResponse::Value(value)
                } else {
                    RuntimeResponse::Error("no session".into())
                }
            }
            RuntimeRequest::LVar { session, name } => {
                match self.sessions.get_mut(&session) {
                    Some(s) => {
                        let mut sim = s.sim.lock().unwrap();
                        RuntimeResponse::Value(sim.lvar_by_name(&name).unwrap_or(0.0))
                    }
                    None => RuntimeResponse::Value(0.0),
                }
            }
            RuntimeRequest::LVarSet { session, name, value } => {
                if let Some(s) = self.sessions.get_mut(&session) {
                    let mut sim = s.sim.lock().unwrap();
                    let id = sim.register_lvar(&name);
                    sim.lvar_set(id, value);
                    RuntimeResponse::Value(value)
                } else {
                    RuntimeResponse::Error("no session".into())
                }
            }
            RuntimeRequest::Event { session, name } => {
                let id = crate::keyevents::key_event_id(&name).unwrap_or(0);
                if let Some(s) = self.sessions.get_mut(&session) {
                    if let Some(instance) = s.instance.as_mut() {
                        let _ = instance.trigger_key(id);
                    }
                }
                RuntimeResponse::Value(id as f64)
            }
            RuntimeRequest::CommBusCall { session, name, payload, broadcast } => {
                let flags = if broadcast == 0 { 3 } else { broadcast };
                if let Some(s) = self.sessions.get_mut(&session) {
                    if let Some(instance) = s.instance.as_mut() {
                        let delivered = instance
                            .deliver_commbus(&name, payload.as_bytes())
                            .unwrap_or(0);
                        let _ = delivered;
                    }
                }
                self.dispatch_commbus(name, payload.into_bytes(), flags);
                RuntimeResponse::Value(1.0)
            }
            RuntimeRequest::CommBusPoll { since, .. } => {
                let items: Vec<PollItem> = self
                    .commbus_out
                    .iter()
                    .filter(|i| i.seq > since)
                    .cloned()
                    .collect();
                RuntimeResponse::List(items)
            }
            RuntimeRequest::Log { message, .. } => {
                self.log.push(message);
                RuntimeResponse::Value(0.0)
            }
            RuntimeRequest::Context { session } => {
                let text = self
                    .sessions
                    .get(&session)
                    .map(|s| {
                        format!(
                            "session={} package={} title={} version={} unsupported={}",
                            s.id,
                            s.package,
                            s.title,
                            Target::Unknown.label(),
                            s.unsupported.len()
                        )
                    })
                    .unwrap_or_else(|| "unknown session".to_string());
                RuntimeResponse::Text(text)
            }
            RuntimeRequest::Mouse { session, x, y, flags } => {
                if let Some(s) = self.sessions.get_mut(&session) {
                    if let Some(instance) = s.instance.as_mut() {
                        if let Err(e) = instance.mouse(x, y, flags) {
                            s.error.get_or_insert_with(|| e.to_string());
                        }
                    }
                }
                RuntimeResponse::Value(1.0)
            }
            RuntimeRequest::SurfacePng { session } => match self.surface_png(session) {
                Some(bytes) => RuntimeResponse::Binary(bytes),
                None => RuntimeResponse::Error("no rendered surface".into()),
            },
            RuntimeRequest::Unknown { op } => RuntimeResponse::Error(format!("unknown op {op}")),
            RuntimeRequest::WithReply(inner, reply) => {
                let response = self.answer(*inner);
                let _ = reply.send(response.clone());
                response
            }
            RuntimeRequest::StartAircraft { package, aircraft, variant } => match self.start_aircraft(&package, &aircraft, variant.as_deref()) {
                Ok(ids) => RuntimeResponse::Value(ids.len() as f64),
                Err(e) => RuntimeResponse::Error(e.to_string()),
            },
            RuntimeRequest::StartModule { path, gauge } => {
                let title = path
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or("module")
                    .to_string();
                let synthetic = Gauge {
                    section: "manual".into(),
                    kind: GaugeKind::Wasm {
                        module: title.clone(),
                        gauge,
                        params: String::new(),
                        module_path: Some(path.clone()),
                    },
                    width: 1024,
                    height: 768,
                    panel: path.clone(),
                };
                match self.start_gauge("__manual__", &title, synthetic) {
                    Ok(id) => RuntimeResponse::Value(id as f64),
                    Err(e) => RuntimeResponse::Error(e.to_string()),
                }
            }
            RuntimeRequest::OpenInstrument { session, .. } => {
                RuntimeResponse::Value(session as f64)
            }
            RuntimeRequest::StopAll => {
                for (_, mut session) in self.sessions.drain() {
                    if let Some(instance) = session.instance.as_mut() {
                        let _ = instance.kill();
                    }
                }
                self.running = false;
                RuntimeResponse::Value(0.0)
            }
            RuntimeRequest::Tick(dt) => {
                self.tick(dt as f32);
                RuntimeResponse::Value(0.0)
            }
            RuntimeRequest::Quit => RuntimeResponse::Value(0.0),
        }
    }
}

use std::path::Path;
