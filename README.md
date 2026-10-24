# msfsrunner

Standalone runtime for MSFS 2020/2024 addon WASM modules and HTML instruments. Runs Community addon gauges and instrument screens without the simulator.

> **Highly experimental.** This is an unfinished research tool. Gauges, host
> functions and rendering paths are incomplete, behaviour differs from the real
> simulator, and the CLI and internal APIs change without notice. Do not rely on
> it for anything you care about.

## Build

```sh
cargo build --release
./target/release/msfsrunner --community /path/to/Community
```

Set `MSFSRUNNER_HEADLESS=1` for a non-interactive scan and diagnostic report.

## Pull requests

Not open unless contacted and agreed beforehand.
