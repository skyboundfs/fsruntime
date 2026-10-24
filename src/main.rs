mod bridge;
mod engine;
mod pkg;
mod runtime;
mod ui;

use anyhow::Result;
use std::path::PathBuf;
use std::sync::mpsc;

fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let mut root = std::env::current_dir()?;
    let mut launch: Option<(String, Option<String>)> = None;

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--community" | "-c" => {
                if let Some(path) = args.next() {
                    root = PathBuf::from(path);
                }
            }
            "--aircraft" | "-a" => {
                let package = args.next().unwrap_or_default();
                let aircraft = args.next();
                launch = Some((package, aircraft));
            }
            "--module" | "-m" => {
                let path = args.next().unwrap_or_default();
                launch = Some((String::new(), Some(path)));
            }
            "--help" | "-h" => {
                print_help();
                return Ok(());
            }
            other => {
                let candidate = PathBuf::from(other);
                if candidate.is_dir() {
                    root = candidate;
                }
            }
        }
    }

    if std::env::var("FSRUNTIME_HEADLESS").is_ok() {
        return headless(root, launch);
    }

    // tao's EventLoop is !Send, so the window thread must create it too.
    // eframe owns the main thread for the control window.
    let (sink_tx, sink_rx) = mpsc::channel();
    let window_thread = std::thread::Builder::new()
        .name("fsruntime-windows".into())
        .spawn(move || {
            let (event_loop, sink) = ui::windows::make_event_loop();
            if sink_tx.send(sink.clone()).is_err() {
                return;
            }
            ui::windows::run(event_loop, sink);
        })?;
    let sink = sink_rx
        .recv()
        .map_err(|_| anyhow::anyhow!("window thread failed to start"))?;

    let (request_tx, request_rx) = mpsc::channel::<runtime::RuntimeRequest>();
    let runtime = runtime::Runtime::new()?;
    sink.set_base_url(runtime.bridge.base_url());
    runtime.bridge.set_requests(request_tx.clone());

    let result = ui::control::run(runtime, request_tx, request_rx, sink, root, launch);
    let _ = window_thread.join();
    result
}

fn headless(root: PathBuf, launch: Option<(String, Option<String>)>) -> Result<()> {
    let (request_tx, request_rx) = mpsc::channel::<runtime::RuntimeRequest>();
    let mut runtime = runtime::Runtime::new()?;
    runtime.scan(&root)?;
    for package in runtime.package.iter() {
        println!(
            "package {} target={} confidence={}",
            package.name,
            package.classification.target.label(),
            package.classification.confidence.label()
        );
        for evidence in &package.classification.evidence {
            println!("    evidence: {evidence}");
        }
        for aircraft in &package.aircraft {
            println!("    aircraft {} ({} variants, {} gauges)", aircraft.key, aircraft.variants.len(), aircraft.gauges().count());
            for gauge in aircraft.gauges() {
                println!("      gauge {} {}x{}", gauge.title(), gauge.width, gauge.height);
            }
            for module in &aircraft.wasm_modules {
                println!("      module {}", module.display());
            }
        }
    }
    for module in &runtime.modules {
        println!(
            "module {} gauges={:?} unsupported={}",
            module.path.display(),
            module.report.gauge_names,
            module.report.imports.iter().filter(|i| !i.supported).count()
        );
    }

    if let Some((package, target)) = launch {
        match target {
            Some(aircraft) => {
                let variant = std::env::var("FSRUNTIME_VARIANT").ok();
                let ids = runtime.start_aircraft(&package, &aircraft, variant.as_deref())?;
                println!("started {} sessions", ids.len());
            }
            None => {
                if let Some(entry) = runtime.modules.iter().find(|m| m.path.to_string_lossy().contains(&package)) {
                    let gauge = entry.report.gauge_names.first().cloned().unwrap_or_default();
                    let path = entry.path.clone();
                    let id = runtime
                        .answer(runtime::RuntimeRequest::StartModule { path, gauge })
                        .clone();
                    println!("started module session {id:?}");
                }
            }
        }
        let ids: Vec<u64> = runtime.sessions.keys().copied().collect();
        for id in ids.iter().take(6) {
            let t = std::time::Instant::now();
            runtime.tick_session(*id, 0.033);
            println!("session {id}: {:?}", t.elapsed());
        }
        for session in runtime.sessions.values() {
            println!(
                "session {} {} update={:?} draw={:?} error={:?} unsupported={}",
                session.id,
                session.title,
                session.last_update,
                session.last_draw,
                session.error,
                session.unsupported.len()
            );
            if let Some(instance) = &session.instance {
                let sim = instance.sim();
                println!("  simvars={} lvars={}", sim.simvar_table().len(), sim.lvars().count());
                let (w, h, pixels) = instance.take_surface_copy();
                let opaque = pixels.chunks_exact(4).filter(|p| p[3] > 0).count();
                let mut hist = std::collections::HashMap::new();
                for px in pixels.chunks_exact(4) {
                    *hist.entry((px[0], px[1], px[2], px[3])).or_insert(0usize) += 1;
                }
                let mut top: Vec<_> = hist.into_iter().collect();
                top.sort_by_key(|(_, n)| std::cmp::Reverse(*n));
                let summary: Vec<String> = top.iter().take(4).map(|(c, n)| format!("{c:?}x{n}")).collect();
                println!("  surface {w}x{h} opaque={opaque} colors={}", summary.join(" "));
                if let Some(path) = std::env::var("FSRUNTIME_PNG_DIR").ok() {
                    let out = std::path::PathBuf::from(path).join(format!("session-{}.png", session.id));
                    let _ = std::fs::create_dir_all(out.parent().unwrap());
                    if let Ok(file) = std::fs::File::create(&out) {
                        let mut enc = png::Encoder::new(file, w, h);
                        enc.set_color(png::ColorType::Rgba);
                        enc.set_depth(png::BitDepth::Eight);
                        if let Ok(mut wr) = enc.write_header() {
                            let _ = wr.write_image_data(&pixels);
                        }
                    }
                }
            }
        }
    }
    drop(request_rx);
    drop(request_tx);
    Ok(())
}

fn print_help() {
    println!("fsruntime [--community PATH] [--aircraft PACKAGE AIRCRAFT] [--module PATH]");
    println!();
    println!("Runs MSFS 2020/2024 addon WASM gauges and HTML instruments without the simulator.");
    println!("Set FSRUNTIME_HEADLESS=1 for a non-interactive scan/report.");
}
