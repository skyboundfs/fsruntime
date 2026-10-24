//! egui control window: browse packages, run an aircraft variant the way
//! MSFS would (every screen in its panel.cfg), and inspect live sessions.
//!
//! The runtime is owned by this thread; the instrument windows live on the
//! tao/wry thread and are driven through `GuiSink`.

use crate::pkg::{Gauge, GaugeKind, Package};
use crate::runtime::{Runtime, RuntimeRequest};
use crate::ui::windows::{GuiCommand, GuiSink};
use egui::{Align, Color32, Layout, RichText};
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
            .with_inner_size([1040.0, 680.0])
            .with_min_inner_size([640.0, 400.0])
            .with_title("fsruntime"),
        ..Default::default()
    };
    eframe::run_native(
        "fsruntime",
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
            self.load_root(self.root.clone(), "loaded");
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
                let variant = std::env::var("FSRUNTIME_VARIANT").ok();
                self.run_variant(&package, &plane, variant.as_deref());
            }
        }
    }

    /// Scan a package root, reset selection, report the outcome in the status bar.
    fn load_root(&mut self, path: PathBuf, verb: &str) {
        match self.runtime.scan(&path) {
            Ok(()) => {
                self.root = path;
                self.aircraft = 0;
                self.variant = 0;
                self.status = format!("{verb} {}", self.root.display());
            }
            Err(e) => self.status = format!("{verb} failed: {e}"),
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
                self.tab = Tab::Sessions;
                self.instance = ids.first().copied();
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

        self.toolbar(ctx);
        self.status_bar(ctx);

        egui::CentralPanel::default().show(ctx, |ui| match self.tab {
            Tab::Browser => self.browser(ui),
            Tab::Sessions => self.sessions(ui),
            Tab::Log => self.log(ui),
        });
    }
}

impl App {
    fn toolbar(&mut self, ctx: &egui::Context) {
        egui::TopBottomPanel::top("toolbar").show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.selectable_value(&mut self.tab, Tab::Browser, "📦  Browser");
                ui.selectable_value(&mut self.tab, Tab::Sessions, {
                    let n = self.runtime.sessions.len();
                    if n > 0 { format!("▶  Sessions ({n})") } else { "▶  Sessions".to_string() }
                });
                ui.selectable_value(&mut self.tab, Tab::Log, "📄  Log");
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if ui.button("Stop all").on_hover_text("Kill every running instrument session and close its window").clicked() {
                        let _ = self.request_tx.send(RuntimeRequest::StopAll);
                        self.gui.send(GuiCommand::CloseAll);
                        self.instance = None;
                        self.status = "stopped all instances".to_string();
                    }
                    if ui.button("Reload").on_hover_text("Re-scan the current package").clicked() {
                        let root = self.root.clone();
                        self.load_root(root, "reloaded");
                    }
                    if ui.button("Open package…").clicked() {
                        if let Some(dir) = rfd::FileDialog::new().pick_folder() {
                            self.path_input = dir.display().to_string();
                            self.load_root(dir, "loaded");
                        }
                    }
                });
            });
        });
    }

    fn status_bar(&mut self, ctx: &egui::Context) {
        egui::TopBottomPanel::bottom("status").show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.label(&self.status);
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    ui.label(
                        RichText::new(format!(
                            "{} sessions · bridge {}",
                            self.runtime.sessions.len(),
                            self.runtime.bridge.base_url()
                        ))
                        .weak(),
                    );
                });
            });
        });
    }

    fn browser(&mut self, ui: &mut egui::Ui) {
        if self.package().is_none() {
            self.empty_browser(ui);
            return;
        }

        let Some(package) = self.package() else { return };
        let aircraft_keys: Vec<String> = package.aircraft.iter().map(|a| a.key.clone()).collect();
        let package_name = package.name.clone();

        egui::SidePanel::left("aircraft_pane")
            .exact_width(220.0)
            .show_inside(ui, |ui| {
                ui.label(RichText::new(&package_name).strong());
                ui.label(RichText::new(format!("{} aircraft", aircraft_keys.len())).weak());
                ui.separator();
                egui::ScrollArea::vertical().show(ui, |ui| {
                    for (i, key) in aircraft_keys.iter().enumerate() {
                        if ui.selectable_label(self.aircraft == i, key).clicked() {
                            self.aircraft = i;
                            self.variant = 0;
                        }
                    }
                });
            });

        let variants: Vec<(String, usize)> = self
            .selected_aircraft()
            .map(|a| a.variants.iter().map(|v| (v.label(), v.gauges.len())).collect())
            .unwrap_or_default();

        let mut run = None;
        egui::SidePanel::left("variants_pane")
            .exact_width(240.0)
            .show_inside(ui, |ui| {
                ui.label(RichText::new("Variants").strong());
                ui.separator();
                egui::ScrollArea::vertical().show(ui, |ui| {
                    for (i, (label, gauges)) in variants.iter().enumerate() {
                        ui.horizontal(|ui| {
                            if ui
                                .selectable_label(self.variant == i, label)
                                .on_hover_text(format!("{gauges} screens"))
                                .clicked()
                            {
                                self.variant = i;
                            }
                            if ui.small_button("▶").on_hover_text(format!("Run {label}")).clicked() {
                                self.variant = i;
                                run = Some(i);
                            }
                        });
                        ui.label(RichText::new(format!("   {gauges} screens")).weak());
                    }
                });
            });

        egui::CentralPanel::default().show_inside(ui, |ui| {
            self.variant_detail(ui, &mut run);
        });

        if run.is_some() {
            let plane = aircraft_keys.get(self.aircraft).cloned().unwrap_or_default();
            let variant = variants.get(self.variant).map(|(l, _)| l.clone());
            self.run_variant(&package_name, &plane, variant.as_deref());
        }
    }

    fn empty_browser(&mut self, ui: &mut egui::Ui) {
        ui.vertical_centered(|ui| {
            ui.add_space(ui.available_height() * 0.25);
            ui.heading("No package loaded");
            ui.label(RichText::new("Point fsruntime at an MSFS Community folder or an addon package.").weak());
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                ui.add_sized([360.0, 24.0], egui::TextEdit::singleline(&mut self.path_input).hint_text("/path/to/Community"));
                if ui.button("Load").clicked() {
                    let path = PathBuf::from(self.path_input.clone());
                    self.load_root(path, "loaded");
                }
                if ui.button("Browse…").clicked() {
                    if let Some(dir) = rfd::FileDialog::new().pick_folder() {
                        self.path_input = dir.display().to_string();
                        self.load_root(dir, "loaded");
                    }
                }
            });
        });
    }

    /// Right pane of the browser: details of the selected variant plus its
    /// instrument list with per-screen launch buttons.
    fn variant_detail(&mut self, ui: &mut egui::Ui, run: &mut Option<usize>) {
        let Some(variant) = self.selected_variant() else {
            ui.label("Select an aircraft and variant.");
            return;
        };
        let label = variant.label();
        let gauges = variant.gauges.clone();
        ui.horizontal(|ui| {
            ui.heading(&label);
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if ui
                    .add_enabled(!gauges.is_empty(), egui::Button::new("▶  Run variant"))
                    .on_hover_text("Opens every screen declared in the variant's panel.cfg")
                    .clicked()
                {
                    *run = Some(self.variant);
                }
            });
        });
        ui.label(RichText::new(format!("{} screens", gauges.len())).weak());
        ui.separator();

        if gauges.is_empty() {
            ui.label("This variant declares no instruments.");
            return;
        }
        let mut launch: Option<Gauge> = None;
        egui::ScrollArea::vertical().show(ui, |ui| {
            for gauge in &gauges {
                let (kind, name) = match &gauge.kind {
                    GaugeKind::Wasm { gauge, .. } => ("wasm", gauge.clone()),
                    GaugeKind::Html { page, .. } => ("html", page.clone()),
                };
                ui.horizontal(|ui| {
                    let badge = RichText::new(kind.to_uppercase()).monospace().color(if kind == "wasm" {
                        Color32::from_rgb(120, 190, 255)
                    } else {
                        Color32::from_rgb(160, 220, 140)
                    });
                    ui.label(badge);
                    ui.monospace(format!("{:>4}x{:<4}", gauge.width, gauge.height));
                    ui.label(gauge.title());
                    ui.label(RichText::new(name).weak());
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if ui.small_button("Open").clicked() {
                            launch = Some(gauge.clone());
                        }
                    });
                });
                ui.separator();
            }
        });
        if let Some(gauge) = launch {
            self.launch_single(&gauge);
        }
    }

    fn sessions(&mut self, ui: &mut egui::Ui) {
        if self.runtime.sessions.is_empty() {
            ui.vertical_centered(|ui| {
                ui.add_space(ui.available_height() * 0.3);
                ui.heading("No sessions");
                ui.label(RichText::new("Run a variant from the Browser tab to start instruments.").weak());
            });
            return;
        }
        let mut ids: Vec<u64> = self.runtime.sessions.keys().copied().collect();
        ids.sort_unstable();

        egui::SidePanel::left("session_list")
            .resizable(true)
            .default_width(280.0)
            .show_inside(ui, |ui| {
                ui.label(RichText::new(format!("{} sessions", ids.len())).strong());
                ui.separator();
                egui::ScrollArea::vertical().show(ui, |ui| {
                    for id in &ids {
                        let Some(session) = self.runtime.sessions.get(id) else { continue };
                        let failed = session.error.is_some();
                        let dot = if failed { Color32::from_rgb(230, 90, 90) } else { Color32::from_rgb(90, 200, 120) };
                        ui.horizontal(|ui| {
                            let (rect, _) = ui.allocate_exact_size(egui::vec2(8.0, 8.0), egui::Sense::hover());
                            ui.painter().circle_filled(rect.center(), 4.0, dot);
                            let text = format!("#{id}  {}", session.title);
                            if ui.selectable_label(self.instance == Some(*id), text).clicked() {
                                self.instance = Some(*id);
                            }
                        });
                    }
                });
            });

        let selected = self
            .instance
            .filter(|id| self.runtime.sessions.contains_key(id))
            .or_else(|| ids.first().copied());
        // Deferred: the panel closure borrows the runtime immutably, so any
        // &mut self action runs after it closes.
        let mut reopen = false;
        egui::CentralPanel::default().show_inside(ui, |ui| {
            let Some(id) = selected else {
                ui.label("Select a session.");
                return;
            };
            let Some(session) = self.runtime.sessions.get(&id) else { return };

            ui.horizontal(|ui| {
                ui.heading(&session.title);
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    ui.add_enabled_ui(session.error.is_none(), |ui| {
                        reopen |= ui.button("Open window").clicked();
                    });
                });
            });
            ui.horizontal(|ui| {
                ui.label(RichText::new(format!("session #{id}")).weak());
                ui.label(RichText::new(format!("·  {}x{}", session.gauge.width, session.gauge.height)).weak());
                ui.label(RichText::new(format!("·  {}", session.package)).weak());
                if session.frame > 0 {
                    ui.label(RichText::new(format!("·  frame {}", session.frame)).weak());
                }
            });
            ui.separator();

            if let Some(err) = &session.error {
                ui.colored_label(Color32::from_rgb(230, 90, 90), format!("error: {err}"));
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
            if let Some(instance) = &session.instance {
                let sim = instance.sim();
                egui::CollapsingHeader::new(format!(
                    "Simulated state ({} simvars, {} lvars)",
                    sim.simvar_table().len(),
                    sim.lvars().count()
                ))
                .default_open(true)
                .show(ui, |ui| {
                    egui::ScrollArea::vertical().max_height(ui.available_height()).show(ui, |ui| {
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
                });
            }
        });
        if reopen {
            if let Some(id) = selected {
                self.open_window(id);
            }
        }
    }

    fn log(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.label(RichText::new(format!("{} lines", self.runtime.log.len())).strong());
            if ui.small_button("Clear").clicked() {
                self.runtime.log.clear();
            }
        });
        ui.separator();
        egui::ScrollArea::vertical()
            .stick_to_bottom(true)
            .show(ui, |ui| {
                for line in &self.runtime.log {
                    let color = if line.contains("error") || line.contains("failed") {
                        Color32::from_rgb(230, 120, 120)
                    } else if line.contains("unsupported") {
                        Color32::from_rgb(230, 200, 110)
                    } else {
                        Color32::GRAY
                    };
                    ui.colored_label(color, line);
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
