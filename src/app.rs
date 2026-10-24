//! egui control window: browse packages, run an aircraft variant the way
//! MSFS would (every screen in its panel.cfg), and inspect live sessions.
//!
//! The runtime is owned by this thread; the instrument windows live on the
//! tao/wry thread and are driven through `GuiSink`.

use crate::gui::{GuiCommand, GuiSink};
use crate::pkg::{Gauge, GaugeKind, Package};
use crate::runtime::{Runtime, RuntimeRequest};
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, Sender};
use std::time::{Duration, Instant};

pub fn run(
    runtime: Runtime,
    request_tx: Sender<RuntimeRequest>,
    request_rx: Receiver<RuntimeRequest>,
    gui: GuiSink,
    root: PathBuf,
    launch: Option<(String, Option<String>)>,
) -> anyhow::Result<()> {
    let mut app = App::new(runtime, request_tx, request_rx, gui, root);
    app.initial_launch = launch;
    app.startup();

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([980.0, 640.0])
            .with_min_inner_size([560.0, 360.0])
            .with_title("msfsrunner"),
        ..Default::default()
    };
    eframe::run_native(
        "msfsrunner",
        options,
        Box::new(move |_cc| Ok(Box::new(app))),
    )
    .map_err(|e| anyhow::anyhow!("eframe: {e}"))
}

struct App {
    runtime: Runtime,
    request_tx: Sender<RuntimeRequest>,
    request_rx: Receiver<RuntimeRequest>,
    gui: GuiSink,
    root: PathBuf,
    initial_launch: Option<(String, Option<String>)>,
    tab: Tab,
    status: String,
    aircraft: usize,
    variant: usize,
    instance: Option<u64>,
    path_input: String,
    last_tick: Instant,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Tab {
    Browser,
    Sessions,
    Log,
}

impl App {
    fn new(
        runtime: Runtime,
        request_tx: Sender<RuntimeRequest>,
        request_rx: Receiver<RuntimeRequest>,
        gui: GuiSink,
        root: PathBuf,
    ) -> Self {
        App {
            runtime,
            request_tx,
            request_rx,
            gui,
            path_input: root.display().to_string(),
            root,
            initial_launch: None,
            tab: Tab::Browser,
            status: String::new(),
            aircraft: 0,
            variant: 0,
            instance: None,
            last_tick: Instant::now(),
        }
    }

    fn startup(&mut self) {
        if !self.root.as_os_str().is_empty() {
            match self.runtime.scan(&self.root) {
                Ok(()) => self.status = format!("loaded {}", self.root.display()),
                Err(e) => self.status = format!("load failed: {e}"),
            }
        }
        if let Some((package, aircraft)) = self.initial_launch.take() {
            let plane = aircraft.or_else(|| {
                self.runtime
                    .package
                    .as_ref()?
                    .aircraft
                    .first()
                    .map(|a| a.key.clone())
            });
            if let Some(plane) = plane {
                let variant = std::env::var("MSFSRUNNER_VARIANT").ok();
                self.run_variant(&package, &plane, variant.as_deref());
            }
        }
    }

    fn package(&self) -> Option<&Package> {
        self.runtime.package.as_ref()
    }

    fn selected_aircraft(&self) -> Option<&crate::pkg::Aircraft> {
        self.package()?.aircraft.get(self.aircraft)
    }

    fn selected_variant(&self) -> Option<&crate::pkg::Variant> {
        self.selected_aircraft()?.variants.get(self.variant)
    }

    /// Launch every gauge in the chosen variant - the way MSFS loads a plane.
    fn run_variant(&mut self, package: &str, aircraft: &str, variant: Option<&str>) {
        match self.runtime.start_aircraft(package, aircraft, variant) {
            Ok(ids) => {
                let mut opened = 0;
                for id in &ids {
                    if self.open_window(*id) {
                        opened += 1;
                    }
                }
                self.status = format!("launched {aircraft} - {opened}/{} screens", ids.len());
            }
            Err(e) => self.status = format!("launch failed: {e}"),
        }
    }

    fn open_window(&mut self, id: u64) -> bool {
        let Some(session) = self.runtime.sessions.get(&id) else { return false };
        if session.error.is_some() {
            return false;
        }
        let html = instrument_url(&self.runtime, id, &session.gauge);
        let title = session.title.clone();
        let (w, h) = (session.gauge.width, session.gauge.height);
        self.gui.send(GuiCommand::OpenInstrument { session: id, title, html, width: w, height: h });
        true
    }

    fn launch_single(&mut self, gauge: &Gauge) {
        let Some(package) = self.package() else {
            self.status = "no package loaded".to_string();
            return;
        };
        let package_name = package.name.clone();
        let title = gauge.title();
        match self.runtime.start_gauge(&package_name, &title, gauge.clone()) {
            Ok(id) => {
                if self.open_window(id) {
                    self.instance = Some(id);
                    self.status = format!("launched session {id}");
                } else {
                    let err = self
                        .runtime
                        .sessions
                        .get(&id)
                        .and_then(|s| s.error.clone())
                        .unwrap_or_else(|| "unknown error".to_string());
                    self.status = format!("session {id} failed: {err}");
                }
            }
            Err(e) => self.status = format!("launch failed: {e}"),
        }
    }

    fn pump(&mut self) {
        while let Ok(request) = self.request_rx.try_recv() {
            self.runtime.answer(request);
        }
        self.runtime.pump();
        let now = Instant::now();
        let dt = now.duration_since(self.last_tick).as_secs_f32().min(0.1);
        self.last_tick = now;
        self.runtime.tick(dt);
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.pump();
        // The runtime advances continuously, so keep repainting.
        ctx.request_repaint_after(Duration::from_millis(33));

        egui::TopBottomPanel::top("tabs").show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.selectable_value(&mut self.tab, Tab::Browser, "Browser");
                ui.selectable_value(&mut self.tab, Tab::Sessions, "Sessions");
                ui.selectable_value(&mut self.tab, Tab::Log, "Log");
                ui.separator();
                if ui.button("Open package…").clicked() {
                    if let Some(dir) = rfd_pick_folder() {
                        self.path_input = dir.display().to_string();
                        match self.runtime.scan(&dir) {
                            Ok(()) => {
                                self.root = dir;
                                self.aircraft = 0;
                                self.variant = 0;
                                self.status = format!("loaded {}", self.root.display());
                            }
                            Err(e) => self.status = format!("load failed: {e}"),
                        }
                    }
                }
                if ui.button("Reload").clicked() {
                    let root = self.root.clone();
                    match self.runtime.scan(&root) {
                        Ok(()) => self.status = format!("reloaded {}", root.display()),
                        Err(e) => self.status = format!("reload failed: {e}"),
                    }
                }
                if ui.button("Stop all").clicked() {
                    let _ = self.request_tx.send(RuntimeRequest::StopAll);
                    self.gui.send(GuiCommand::CloseAll);
                    self.status = "stopped all instances".to_string();
                }
            });
        });

        egui::TopBottomPanel::bottom("status").show(ctx, |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.label(&self.status);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(
                        egui::RichText::new(format!(
                            "{} sessions · bridge {}",
                            self.runtime.sessions.len(),
                            self.runtime.bridge.base_url()
                        ))
                        .weak(),
                    );
                });
            });
        });

        egui::CentralPanel::default().show(ctx, |ui| match self.tab {
            Tab::Browser => self.browser(ui),
            Tab::Sessions => self.sessions(ui),
            Tab::Log => self.log(ui),
        });
    }
}

impl App {
    fn browser(&mut self, ui: &mut egui::Ui) {
        if self.package().is_none() {
            ui.label("No package loaded. Use “Open package…” or pass a path on the command line.");
            ui.horizontal(|ui| {
                ui.text_edit_singleline(&mut self.path_input);
                if ui.button("Load").clicked() {
                    let path = PathBuf::from(self.path_input.clone());
                    match self.runtime.scan(&path) {
                        Ok(()) => {
                            self.root = path;
                            self.aircraft = 0;
                            self.variant = 0;
                            self.status = format!("loaded {}", self.root.display());
                        }
                        Err(e) => self.status = format!("load failed: {e}"),
                    }
                }
            });
            return;
        }

        let Some(package) = self.package() else { return };
        let aircraft_keys: Vec<String> = package.aircraft.iter().map(|a| a.key.clone()).collect();
        let variants: Vec<(String, usize)> = self
            .selected_aircraft()
            .map(|a| {
                a.variants
                    .iter()
                    .map(|v| (v.label(), v.gauges.len()))
                    .collect()
            })
            .unwrap_or_default();

        let mut run = false;
        let list_height = (ui.available_height() - 40.0).max(80.0);
        ui.columns(2, |cols| {
            cols[0].heading("Aircraft");
            cols[0].separator();
            egui::ScrollArea::vertical()
                .id_salt("aircraft")
                .max_height(list_height)
                .show(&mut cols[0], |ui| {
                    for (i, key) in aircraft_keys.iter().enumerate() {
                        if ui.selectable_label(self.aircraft == i, key).clicked() {
                            self.aircraft = i;
                            self.variant = 0;
                        }
                    }
                });

            cols[1].heading("Variants");
            cols[1].separator();
            egui::ScrollArea::vertical()
                .id_salt("variants")
                .max_height(list_height)
                .show(&mut cols[1], |ui| {
                    for (i, (label, gauges)) in variants.iter().enumerate() {
                        let text = format!("{label}  ·  {gauges} screens");
                        if ui.selectable_label(self.variant == i, text).clicked() {
                            self.variant = i;
                        }
                        if ui.small_button("Run").clicked() {
                            self.variant = i;
                            run = true;
                        }
                    }
                });
        });

        ui.separator();
        let can_run = !aircraft_keys.is_empty() && !variants.is_empty();
        ui.horizontal(|ui| {
            if ui
                .add_enabled(can_run, egui::Button::new("Run this variant"))
                .clicked()
            {
                run = true;
            }
            ui.label(
                egui::RichText::new("Opens every screen declared in the variant's panel.cfg")
                    .weak(),
            );
        });

        if run {
            let package = self.package().map(|p| p.name.clone()).unwrap_or_default();
            let plane = aircraft_keys.get(self.aircraft).cloned().unwrap_or_default();
            let variant = variants.get(self.variant).map(|(l, _)| l.clone());
            self.run_variant(&package, &plane, variant.as_deref());
        }

        ui.separator();
        ui.collapsing("Individual instruments", |ui| {
            let gauges: Vec<Gauge> = self
                .selected_variant()
                .map(|v| v.gauges.clone())
                .unwrap_or_default();
            if gauges.is_empty() {
                ui.label("No instruments in this variant.");
                return;
            }
            let mut launch: Option<Gauge> = None;
            for gauge in &gauges {
                ui.horizontal(|ui| {
                    let kind = match &gauge.kind {
                        GaugeKind::Wasm { .. } => "wasm",
                        GaugeKind::Html { .. } => "html",
                    };
                    ui.monospace(format!("{kind:<4} {}x{}", gauge.width, gauge.height));
                    ui.label(gauge.title());
                    if ui.small_button("Open").clicked() {
                        launch = Some(gauge.clone());
                    }
                });
            }
            if let Some(gauge) = launch {
                self.launch_single(&gauge);
            }
        });
    }

    fn sessions(&mut self, ui: &mut egui::Ui) {
        if self.runtime.sessions.is_empty() {
            ui.label("No sessions yet. Run a variant from the Browser tab.");
            return;
        }
        let mut ids: Vec<u64> = self.runtime.sessions.keys().copied().collect();
        ids.sort_unstable();

        egui::SidePanel::left("session_list")
            .resizable(true)
            .default_width(260.0)
            .show_inside(ui, |ui| {
                egui::ScrollArea::vertical().show(ui, |ui| {
                    for id in &ids {
                        let Some(session) = self.runtime.sessions.get(id) else { continue };
                        let state = if session.error.is_some() { "error" } else { "running" };
                        let label = format!("{id}  {state}  {}", session.title);
                        if ui
                            .selectable_label(self.instance == Some(*id), label)
                            .clicked()
                        {
                            self.instance = Some(*id);
                        }
                    }
                });
            });

        let selected = self
            .instance
            .filter(|id| self.runtime.sessions.contains_key(id))
            .or_else(|| ids.first().copied());
        egui::CentralPanel::default().show_inside(ui, |ui| {
            let Some(id) = selected else {
                ui.label("Select a session.");
                return;
            };
            let Some(session) = self.runtime.sessions.get(&id) else { return };
            egui::Grid::new("session_detail").num_columns(2).show(ui, |ui| {
                ui.label("session");
                ui.monospace(id.to_string());
                ui.end_row();
                ui.label("title");
                ui.label(&session.title);
                ui.end_row();
                ui.label("surface");
                ui.monospace(format!("{}x{}", session.gauge.width, session.gauge.height));
                ui.end_row();
                ui.label("package");
                ui.label(&session.package);
                ui.end_row();
            });
            if let Some(err) = &session.error {
                ui.colored_label(egui::Color32::RED, err);
            }
            if !session.unsupported.is_empty() {
                ui.collapsing(
                    format!("Unsupported imports ({})", session.unsupported.len()),
                    |ui| {
                        for name in &session.unsupported {
                            ui.monospace(name);
                        }
                    },
                );
            }
            let reopen = ui.button("Open window").clicked();
            if let Some(instance) = &session.instance {
                let sim = instance.sim();
                ui.separator();
                ui.heading("Simulated state");
                egui::ScrollArea::vertical().show(ui, |ui| {
                    egui::Grid::new("simvars").num_columns(2).striped(true).show(ui, |ui| {
                        for def in sim.simvar_table().iter().take(300) {
                            ui.monospace(&def.name);
                            ui.monospace(format!("{:.3} {}", def.value, def.unit));
                            ui.end_row();
                        }
                        for (name, value) in sim.lvars().take(200) {
                            ui.monospace(format!("L:{name}"));
                            ui.monospace(format!("{value:.3}"));
                            ui.end_row();
                        }
                    });
                });
            }
            if reopen {
                self.open_window(id);
            }
        });
    }

    fn log(&mut self, ui: &mut egui::Ui) {
        egui::ScrollArea::vertical()
            .stick_to_bottom(true)
            .show(ui, |ui| {
                for line in &self.runtime.log {
                    ui.monospace(line);
                }
            });
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

/// Native folder picker; falls back to None when no desktop portal is present.
fn rfd_pick_folder() -> Option<PathBuf> {
    rfd::FileDialog::new().pick_folder()
}
