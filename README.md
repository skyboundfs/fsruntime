# msfsrunner

Standalone runtime for MSFS 2020/2024 addon WASM modules and HTML instruments. Runs Community addon gauges and instrument screens without the simulator.

## Build

```sh
cargo build --release
./target/release/msfsrunner --community /path/to/Community
```

Set `MSFSRUNNER_HEADLESS=1` for a non-interactive scan and diagnostic report.

## Pull requests

Not open unless contacted and agreed beforehand.
