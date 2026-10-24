mod bridge;
mod fsrender;
mod gui;
mod keyevents;
mod pkg;
mod render;
mod runtime;
mod sim;
mod tui;
mod wasmhost;

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

    if std::env::var("MSFSRUNNER_HEADLESS").is_ok() {
        return headless(root, launch);
    }

    let (event_loop, sink) = gui::make_event_loop();
    let (request_tx, request_rx) = mpsc::channel::<runtime::RuntimeRequest>();
    let mut runtime = runtime::Runtime::new()?;
    sink.set_base_url(runtime.bridge.base_url());
    runtime.bridge.set_requests(request_tx.clone());

    let tui_sink = sink.clone();
    let tui_root = root.clone();
    let tui_requests = request_tx.clone();
    let tui_launch = launch.clone();

    let tui_thread = std::thread::Builder::new()
        .name("msfs-tui".into())
        .spawn(move || -> Result<()> {
            tui::run(&mut runtime, tui_requests, request_rx, tui_sink, tui_root, tui_launch)
        })?;

    gui::run(event_loop, sink);
    let _ = tui_thread.join();
    Ok(())
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
                let variant = std::env::var("MSFSRUNNER_VARIANT").ok();
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
                if let Some(path) = std::env::var("MSFSRUNNER_PNG_DIR").ok() {
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
    println!("msfsrunner [--community PATH] [--aircraft PACKAGE AIRCRAFT] [--module PATH]");
    println!();
    println!("Runs MSFS 2020/2024 addon WASM gauges and HTML instruments without the simulator.");
    println!("Set MSFSRUNNER_HEADLESS=1 for a non-interactive scan/report.");
}
