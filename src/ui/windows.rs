use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tao::event::{Event, WindowEvent};
use tao::event_loop::{ControlFlow, EventLoopBuilder, EventLoopProxy, EventLoopWindowTarget};
use tao::window::{Window, WindowBuilder, WindowId};

#[derive(Clone, Debug)]
pub enum GuiCommand {
    OpenInstrument { session: u64, title: String, html: String, width: u32, height: u32 },
    CloseInstrument { session: u64 },
    CloseAll,
    Quit,
}

pub enum GuiEvent {
    Command(GuiCommand),
}

#[derive(Clone)]
pub struct GuiSink {
    proxy: EventLoopProxy<GuiEvent>,
    pub base_url: Arc<Mutex<String>>,
}

impl GuiSink {
    pub fn send(&self, command: GuiCommand) {
        let _ = self.proxy.send_event(GuiEvent::Command(command));
    }

    pub fn set_base_url(&self, url: String) {
        if let Ok(mut slot) = self.base_url.lock() {
            *slot = url;
        }
    }

    fn base_url(&self) -> String {
        self.base_url.lock().map(|s| s.clone()).unwrap_or_default()
    }
}

pub struct InstrumentWindow {
    pub window: Window,
    pub webview: wry::WebView,
    pub session: u64,
}

pub fn run(event_loop: tao::event_loop::EventLoop<GuiEvent>, sink: GuiSink) {
    let mut windows: HashMap<WindowId, InstrumentWindow> = HashMap::new();
    event_loop.run(move |event, target, control_flow| {
        *control_flow = ControlFlow::Wait;
        match event {
            Event::UserEvent(GuiEvent::Command(GuiCommand::OpenInstrument { session, title, html, width, height })) => {
                match open_window(target, session, &title, &html, width, height, sink.base_url()) {
                    Ok(window) => {
                        windows.insert(window.window.id(), window);
                    }
                    Err(e) => eprintln!("failed to open instrument window: {e}"),
                }
            }
            Event::UserEvent(GuiEvent::Command(GuiCommand::CloseInstrument { session })) => {
                let id = windows.iter().find(|(_, w)| w.session == session).map(|(id, _)| *id);
                if let Some(id) = id {
                    windows.remove(&id);
                }
            }
            Event::UserEvent(GuiEvent::Command(GuiCommand::CloseAll)) => {
                windows.clear();
            }
            Event::UserEvent(GuiEvent::Command(GuiCommand::Quit)) => {
                windows.clear();
                *control_flow = ControlFlow::Exit;
            }
            Event::WindowEvent { window_id, event: WindowEvent::CloseRequested, .. } => {
                // Closing the last instrument leaves the app running; the TUI
                // stays alive and can spawn new windows.
                windows.remove(&window_id);
            }
            _ => {}
        }
    });
}

pub fn make_event_loop() -> (tao::event_loop::EventLoop<GuiEvent>, GuiSink) {
    use tao::platform::unix::EventLoopBuilderExtUnix;
    // eframe owns the main thread, so the instrument loop lives on a worker.
    let mut builder = EventLoopBuilder::<GuiEvent>::with_user_event();
    builder.with_any_thread(true);
    let event_loop = builder.build();
    let sink = GuiSink {
        proxy: event_loop.create_proxy(),
        base_url: Arc::new(Mutex::new(String::new())),
    };
    (event_loop, sink)
}

fn open_window(
    target: &EventLoopWindowTarget<GuiEvent>,
    session: u64,
    title: &str,
    url: &str,
    width: u32,
    height: u32,
    base_url: String,
) -> anyhow::Result<InstrumentWindow> {
    let window = WindowBuilder::new()
        .with_title(format!("{title} [{session}]"))
        .with_inner_size(tao::dpi::LogicalSize::new(width.max(160) as f64, height.max(120) as f64))
        .build(target)?;
    let init = format!(
        "window.__MSFS_SESSION__ = {session}; window.__MSFS_BASE__ = {base:?};",
        base = base_url
    );
    let inject = include_str!("../web/inject.js");
    let builder = wry::WebViewBuilder::new()
        .with_url(url)
        .with_initialization_script(&init)
        .with_initialization_script(inject)
        .with_ipc_handler(move |request: wry::http::Request<String>| {
            let body = request.body().to_string();
            if body.starts_with("log:") {
                eprintln!("[{session}] {}", &body[4..]);
            }
        });
    use tao::platform::unix::WindowExtUnix;
    use wry::WebViewBuilderExtUnix;
    let vbox = window
        .default_vbox()
        .ok_or_else(|| anyhow::anyhow!("window has no default container"))?;
    let webview = builder.build_gtk(vbox)?;
    Ok(InstrumentWindow { window, webview, session })
}
