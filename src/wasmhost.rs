use crate::fsrender::{FsPaint, FsPath, FsScissor, FsVertex};
use crate::render::{Renderer, Scissor, Vertex};
use crate::sim::{SimState, FS_VAR_ERROR_INVALID_ARGS, FS_VAR_ERROR_NONE};
use anyhow::{bail, Context, Result};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex, MutexGuard};
use wasmtime::{Caller, Engine, Instance, Linker, Memory, Module, Store, Table, TypedFunc, Val};

pub const FS_COMMBUS_BROADCAST_JS: u32 = 1 << 0;
pub const FS_COMMBUS_BROADCAST_WASM: u32 = 1 << 1;
pub const FS_COMMBUS_BROADCAST_WASM_SELF: u32 = 1 << 2;

#[derive(Clone, Debug)]
pub enum HostEvent {
    CommBusCall { name: String, payload: Vec<u8>, broadcast: u32 },
    KeyEvent { id: u32 },
    Log(String),
    Frame { context: u64 },
    Unsupported(String),
}

#[derive(Default)]
pub struct GaugeCallbacks {
    pub init: Option<TypedFunc<(u64, u32), u32>>,
    pub update: Option<TypedFunc<(u64, f32), u32>>,
    pub draw: Option<TypedFunc<(u64, u32), u32>>,
    pub kill: Option<TypedFunc<u64, u32>>,
    pub mouse: Option<MouseHandler>,
}

/// MSFS modules export mouse entry points in two shapes: `<gauge>_mouse_callback`
/// takes (x, y, flags) and `<gauge>_gauge_mouse_handler` takes the context first.
pub enum MouseHandler {
    Plain(TypedFunc<(f32, f32, u32), ()>),
    Context(TypedFunc<(u64, f32, f32, u32), ()>),
}

struct CommBusHandler {
    callback: u32,
    context: u32,
}

struct SimConnectState {
    open: bool,
    next_request: u32,
    data_defs: HashMap<u32, Vec<(String, u32, u32)>>,
    pending: Vec<Vec<u8>>,
}

impl Default for SimConnectState {
    fn default() -> Self {
        SimConnectState { open: false, next_request: 1, data_defs: HashMap::new(), pending: Vec::new() }
    }
}

pub struct HostState {
    sim: Arc<Mutex<SimState>>,
    wasi: wasmtime_wasi::p1::WasiP1Ctx,
    memory: Option<Memory>,
    instance: Option<Instance>,
    table: Option<Table>,
    events: Sender<HostEvent>,
    pub render: Renderer,
    pub render_ctx: u64,
    pub gauge_width: u32,
    pub gauge_height: u32,
    pub gauge_params: String,
    commbus: HashMap<String, Vec<CommBusHandler>>,
    key_handlers: Vec<(u32, u32)>,
    simconnect: SimConnectState,
    io: HashMap<u64, IoFile>,
    next_io: u64,
    pub unsupported: Vec<String>,
    pub label: String,
    pub module_name: String,
    pub data_dir: Option<PathBuf>,
    guest_alloc: Option<TypedFunc<i32, i32>>,
    guest_free: Option<TypedFunc<i32, ()>>,
    pub frame_dirty: bool,
}

struct IoFile {
    data: Vec<u8>,
    position: u64,
    open: bool,
    error: u32,
}

pub struct GaugeInstance {
    pub store: Store<HostState>,
    pub callbacks: GaugeCallbacks,
    pub label: String,
    pub module_name: String,
    pub unsupported: Vec<String>,
    pub context: u64,
}

impl HostState {
    fn sim(&self) -> MutexGuard<'_, SimState> {
        self.sim.lock().unwrap_or_else(|e| e.into_inner())
    }
}

impl GaugeInstance {
    pub fn init(&mut self) -> Result<u32> {
        let Some(init) = self.callbacks.init.clone() else { return Ok(0) };
        let (w, h, params) = {
            let d = self.store.data();
            (d.gauge_width, d.gauge_height, d.gauge_params.clone())
        };
        let install = self.write_install_data(w, h, &params)?;
        let ctx = self.store.data().render_ctx;
        init.call(&mut self.store, (ctx, install)).map_err(|e| anyhow::anyhow!("gauge_init trapped: {e}"))
    }

    pub fn update(&mut self, dt: f32) -> Result<u32> {
        let Some(update) = self.callbacks.update.clone() else { return Ok(0) };
        let ctx = self.store.data().render_ctx;
        update.call(&mut self.store, (ctx, dt)).map_err(|e| anyhow::anyhow!("gauge_update trapped: {e}"))
    }

    pub fn draw(&mut self, elapsed: f64, dt: f64) -> Result<u32> {
        let Some(draw) = self.callbacks.draw.clone() else { return Ok(0) };
        let ctx = self.store.data().render_ctx;
        let data = self.write_draw_data(elapsed, dt)?;
        draw.call(&mut self.store, (ctx, data))
            .map_err(|e| anyhow::anyhow!("gauge_draw trapped: {e}"))
    }

    fn write_draw_data(&mut self, elapsed: f64, dt: f64) -> Result<u32> {
        let Some(memory) = self.store.data().memory else { return Ok(0) };
        let alloc = self.store.data().guest_alloc.clone();
        let ptr = match alloc {
            Some(f) => f.call(&mut self.store, 48)? as u32,
            None => 8192,
        };
        let (w, h) = {
            let d = self.store.data();
            (d.gauge_width as i32, d.gauge_height as i32)
        };
        let mut bytes = [0u8; 48];
        bytes[0..8].copy_from_slice(&(-1.0f64).to_le_bytes());
        bytes[8..16].copy_from_slice(&(-1.0f64).to_le_bytes());
        bytes[16..24].copy_from_slice(&elapsed.to_le_bytes());
        bytes[24..32].copy_from_slice(&dt.to_le_bytes());
        bytes[32..36].copy_from_slice(&w.to_le_bytes());
        bytes[36..40].copy_from_slice(&h.to_le_bytes());
        bytes[40..44].copy_from_slice(&w.to_le_bytes());
        bytes[44..48].copy_from_slice(&h.to_le_bytes());
        memory.write(&mut self.store, ptr as usize, &bytes)?;
        Ok(ptr)
    }

    pub fn kill(&mut self) -> Result<u32> {
        let Some(kill) = self.callbacks.kill.clone() else { return Ok(0) };
        let ctx = self.store.data().render_ctx;
        kill.call(&mut self.store, ctx).map_err(|e| anyhow::anyhow!("gauge_kill trapped: {e}"))
    }

    pub fn mouse(&mut self, x: f32, y: f32, flags: u32) -> Result<()> {
        let Some(handler) = self.callbacks.mouse.as_ref() else { return Ok(()) };
        match handler {
            MouseHandler::Plain(f) => f.call(&mut self.store, (x, y, flags)),
            MouseHandler::Context(f) => {
                let ctx = self.store.data().render_ctx;
                f.call(&mut self.store, (ctx, x, y, flags))
            }
        }
        .map_err(|e| anyhow::anyhow!("mouse handler trapped: {e}"))
    }

    fn write_install_data(&mut self, w: u32, h: u32, params: &str) -> Result<u32> {
        let Some(memory) = self.store.data().memory else { return Ok(0) };
        let alloc = self.store.data().guest_alloc.clone();
        let ptr = match alloc.clone() {
            Some(f) => f.call(&mut self.store, 16)? as u32,
            None => 4096,
        };
        let params_ptr = match alloc {
            Some(f) => f.call(&mut self.store, params.len() as i32 + 1)? as u32,
            None => ptr + 16,
        };
        let mut struct_bytes = [0u8; 16];
        struct_bytes[0..4].copy_from_slice(&(w as i32).to_le_bytes());
        struct_bytes[4..8].copy_from_slice(&(h as i32).to_le_bytes());
        struct_bytes[8..12].copy_from_slice(&(params_ptr as i32).to_le_bytes());
        memory.write(&mut self.store, ptr as usize, &struct_bytes)?;
        let mut text = params.as_bytes().to_vec();
        text.push(0);
        memory.write(&mut self.store, params_ptr as usize, &text)?;
        Ok(ptr)
    }

    pub fn write_guest(&mut self, data: &[u8]) -> Result<u32> {
        let Some(memory) = self.store.data().memory else { bail!("module has no memory") };
        let alloc = self.store.data().guest_alloc.clone();
        let ptr = match alloc {
            Some(f) => f.call(&mut self.store, data.len() as i32 + 1)? as u32,
            None => bail!("module exports no allocator"),
        };
        memory.write(&mut self.store, ptr as usize, data)?;
        Ok(ptr)
    }

    pub fn deliver_commbus(&mut self, name: &str, payload: &[u8]) -> Result<usize> {
        let handlers: Vec<(u32, u32)> = {
            let d = self.store.data();
            d.commbus
                .get(name)
                .map(|v| v.iter().map(|h| (h.callback, h.context)).collect())
                .unwrap_or_default()
        };
        if handlers.is_empty() {
            return Ok(0);
        }
        let ptr = self.write_guest(payload)?;
        let table = self.store.data().table;
        let mut delivered = 0;
        for (cb, ctx) in handlers {
            let Some(table) = table else { continue };
            if let Some(func) = table.get(&mut self.store, cb as u64).and_then(|r| r.unwrap_func().copied()) {
                let _ = func.call(&mut self.store, &[Val::I32(ptr as i32), Val::I32(payload.len() as i32), Val::I32(ctx as i32)], &mut []);
                delivered += 1;
            }
        }
        let free = self.store.data().guest_free.clone();
        if let Some(free) = free {
            let _ = free.call(&mut self.store, ptr as i32);
        }
        Ok(delivered)
    }

    pub fn trigger_key(&mut self, id: u32) -> Result<usize> {
        let handlers: Vec<(u32, u32)> = self.store.data().key_handlers.clone();
        let table = self.store.data().table;
        let mut delivered = 0;
        for (cb, user) in handlers {
            let Some(table) = table else { continue };
            if let Some(func) = table.get(&mut self.store, cb as u64).and_then(|r| r.unwrap_func().copied()) {
                let _ = func.call(
                    &mut self.store,
                    &[Val::I32(id as i32), Val::I32(0), Val::I32(user as i32)],
                    &mut [],
                );
                delivered += 1;
            }
        }
        Ok(delivered)
    }

    pub fn set_simvar(&mut self, name: &str, value: f64) {
        self.store.data().sim().set_simvar_by_name(name, value);
    }

    pub fn sim(&self) -> MutexGuard<'_, SimState> {
        self.store.data().sim()
    }

    pub fn sim_handle(&self) -> Arc<Mutex<SimState>> {
        self.store.data().sim.clone()
    }

    pub fn sim_mut(&mut self) -> MutexGuard<'_, SimState> {
        self.store.data().sim()
    }

    pub fn take_surface_copy(&self) -> (u32, u32, Vec<u8>) {
        let surface = &self.store.data().render.surface;
        (surface.width, surface.height, surface.pixels.clone())
    }

    pub fn surface(&self) -> &crate::render::Surface {
        &self.store.data().render.surface
    }
}

pub struct EngineConfig {
    pub max_fuel: u64,
    pub data_dir: Option<PathBuf>,
}

impl Default for EngineConfig {
    fn default() -> Self {
        EngineConfig { max_fuel: 600_000_000, data_dir: None }
    }
}

pub struct WasmHost {
    engine: Engine,
    config: EngineConfig,
}

pub struct ModuleReport {
    pub imports: Vec<ImportInfo>,
    pub exports: Vec<String>,
    pub gauge_names: Vec<String>,
}

pub struct ImportInfo {
    pub module: String,
    pub name: String,
    pub kind: String,
    pub supported: bool,
}

impl WasmHost {
    pub fn new(config: EngineConfig) -> Result<Self> {
        let mut cfg = wasmtime::Config::new();
        cfg.consume_fuel(true);
        cfg.max_wasm_stack(2 << 20);
        cfg.cranelift_opt_level(wasmtime::OptLevel::Speed);
        let engine = Engine::new(&cfg)?;
        Ok(WasmHost { engine, config })
    }

    pub fn set_data_dir(&mut self, dir: PathBuf) {
        self.config.data_dir = Some(dir);
    }

    pub fn describe(&self, path: &Path) -> Result<ModuleReport> {
        let bytes = std::fs::read(path).with_context(|| format!("read {}", path.display()))?;
        let module = Module::new(&self.engine, &bytes)
            .map_err(|e| anyhow::anyhow!("parse {}: {e}", path.display()))?;
        let mut imports = Vec::new();
        for import in module.imports() {
            let kind = match import.ty() {
                wasmtime::ExternType::Func(f) => {
                    let params: Vec<String> = f.params().map(|v| format!("{v:?}")).collect();
                    let results: Vec<String> = f.results().map(|v| format!("{v:?}")).collect();
                    format!("fn({}) -> ({})", params.join(","), results.join(","))
                }
                wasmtime::ExternType::Memory(_) => "memory".to_string(),
                wasmtime::ExternType::Global(_) => "global".to_string(),
                wasmtime::ExternType::Table(_) => "table".to_string(),
                _ => "other".to_string(),
            };
            let supported = import.module() == "wasi_snapshot_preview1"
                || (import.module() == "env" && is_known_env(import.name()));
            imports.push(ImportInfo {
                module: import.module().to_string(),
                name: import.name().to_string(),
                kind,
                supported,
            });
        }
        let exports: Vec<String> = module.exports().map(|e| e.name().to_string()).collect();
        let mut gauge_names: Vec<String> = exports
            .iter()
            .filter_map(|e| e.strip_suffix("_gauge_init").map(str::to_string))
            .collect();
        gauge_names.sort();
        Ok(ModuleReport { imports, exports, gauge_names })
    }

    pub fn instantiate(
        &self,
        path: &Path,
        label: &str,
        context: u64,
        width: u32,
        height: u32,
        params: &str,
        events: Sender<HostEvent>,
    ) -> Result<GaugeInstance> {
        let bytes = std::fs::read(path).with_context(|| format!("read {}", path.display()))?;
        let module = Module::new(&self.engine, &bytes)
            .map_err(|e| anyhow::anyhow!("parse {}: {e}", path.display()))?;
        let mut linker: Linker<HostState> = Linker::new(&self.engine);
        wasmtime_wasi::p1::add_to_linker_sync(&mut linker, |s| &mut s.wasi)?;
        let unsupported = register_hosts(&mut linker, &self.engine, &module, &self.config)?;
        let state = HostState {
            sim: Arc::new(Mutex::new(SimState::new())),
            wasi: build_wasi(self.config.data_dir.as_deref()),
            memory: None,
            instance: None,
            table: None,
            events,
            render: Renderer::new(width.max(1), height.max(1)),
            render_ctx: context,
            gauge_width: width,
            gauge_height: height,
            gauge_params: params.to_string(),
            commbus: HashMap::new(),
            key_handlers: Vec::new(),
            simconnect: SimConnectState::default(),
            io: HashMap::new(),
            next_io: 1,
            unsupported: unsupported.clone(),
            label: label.to_string(),
            module_name: path.file_name().and_then(|n| n.to_str()).unwrap_or("module").to_string(),
            data_dir: self.config.data_dir.clone(),
            guest_alloc: None,
            guest_free: None,
            frame_dirty: false,
        };
        let mut store = Store::new(&self.engine, state);
        store.set_fuel(self.config.max_fuel)?;
        let instance = linker
            .instantiate(&mut store, &module)
            .map_err(|e| anyhow::anyhow!("instantiate {}: {e}", path.display()))?;
        let memory = instance.get_memory(&mut store, "memory");
        let table = instance.get_table(&mut store, "__indirect_function_table");
        let guest_alloc = instance.get_typed_func::<i32, i32>(&mut store, "malloc").ok();
        let guest_free = instance.get_typed_func::<i32, ()>(&mut store, "free").ok();
        {
            let data = store.data_mut();
            data.memory = memory;
            data.table = table;
            data.instance = Some(instance);
            data.guest_alloc = guest_alloc;
            data.guest_free = guest_free;
        }
        // These modules are WASI reactors: nothing runs their ctors, and
        // __wasilibc_populate_preopens lives there. Without this call every
        // fopen("./data/...") fails and gauge fonts never load.
        if let Ok(ctors) = instance.get_typed_func::<(), ()>(&mut store, "__wasm_call_ctors") {
            ctors.call(&mut store, ()).map_err(|e| anyhow::anyhow!("__wasm_call_ctors trapped: {e}"))?;
        }
        let callbacks = bind_callbacks(&mut store, &instance, label);
        Ok(GaugeInstance {
            store,
            callbacks,
            label: label.to_string(),
            module_name: path.file_name().and_then(|n| n.to_str()).unwrap_or("module").to_string(),
            unsupported,
            context,
        })
    }
}

fn build_wasi(data_dir: Option<&Path>) -> wasmtime_wasi::p1::WasiP1Ctx {
    let mut builder = wasmtime_wasi::WasiCtxBuilder::new();
    builder.inherit_stdio();
    if let Some(dir) = data_dir {
        let _ = builder.preopened_dir(case_view(dir), ".", wasmtime_wasi::filesystem::FsPerms::ReadOnly);
    }
    builder.build_p1()
}

/// MSFS resolves package paths case-insensitively; WASI does not, and gauges
/// ask for "./data/..." while packages ship "Data/". WASI also refuses
/// symlinks, so build a temp view with a real lowercase copy of the Data
/// directory (a few MB) and preopen that as ".".
///
/// shortcut: only the mixed-case "Data" directory is mirrored, since that is
/// the only such path gauges open; widen this if other directories are read.
fn case_view(dir: &Path) -> PathBuf {
    static VIEWS: std::sync::OnceLock<std::sync::Mutex<HashMap<PathBuf, PathBuf>>> =
        std::sync::OnceLock::new();
    let views = VIEWS.get_or_init(Default::default);
    if let Some(hit) = views.lock().ok().and_then(|m| m.get(dir).cloned()) {
        return hit;
    }
    let data = std::fs::read_dir(dir).into_iter().flatten().flatten().find(|e| {
        let name = e.file_name().to_string_lossy().to_string();
        name.eq_ignore_ascii_case("data") && name != "data" && e.path().is_dir()
    });
    let Some(data) = data else { return dir.to_path_buf() };
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    dir.hash(&mut hasher);
    let view = std::env::temp_dir().join(format!("fsruntime-view-{:x}", hasher.finish()));
    let _ = std::fs::remove_dir_all(&view);
    if std::fs::create_dir_all(&view).is_err() {
        return dir.to_path_buf();
    }
    let target = view.join("data");
    let built = match std::fs::create_dir_all(&target) {
        Ok(()) => {
            copy_tree(&data.path(), &target);
            view
        }
        Err(_) => dir.to_path_buf(),
    };
    if let Ok(mut m) = views.lock() {
        m.insert(dir.to_path_buf(), built.clone());
    }
    built
}

fn copy_tree(src: &Path, dst: &Path) {
    for entry in std::fs::read_dir(src).into_iter().flatten().flatten() {
        let target = dst.join(entry.file_name());
        if entry.path().is_dir() {
            if std::fs::create_dir_all(&target).is_ok() {
                copy_tree(&entry.path(), &target);
            }
        } else {
            let _ = std::fs::copy(entry.path(), &target);
        }
    }
}

fn bind_callbacks(store: &mut Store<HostState>, instance: &Instance, label: &str) -> GaugeCallbacks {
    let init = instance
        .get_typed_func::<(u64, u32), u32>(&mut *store, &format!("{label}_gauge_init"))
        .ok();
    let update = instance
        .get_typed_func::<(u64, f32), u32>(&mut *store, &format!("{label}_gauge_update"))
        .ok();
    let draw = instance
        .get_typed_func::<(u64, u32), u32>(&mut *store, &format!("{label}_gauge_draw"))
        .ok();
    let kill = instance
        .get_typed_func::<u64, u32>(&mut *store, &format!("{label}_gauge_kill"))
        .ok();
    let mouse = instance
        .get_typed_func::<(f32, f32, u32), ()>(&mut *store, &format!("{label}_mouse_callback"))
        .ok()
        .map(MouseHandler::Plain)
        .or_else(|| {
            instance
                .get_typed_func::<(u64, f32, f32, u32), ()>(&mut *store, &format!("{label}_gauge_mouse_handler"))
                .ok()
                .map(MouseHandler::Context)
        });
    GaugeCallbacks { init, update, draw, kill, mouse }
}

fn is_known_env(name: &str) -> bool {
    matches!(
        name,
        "fsVarsGetUnitId"
            | "fsVarsGetAVarId"
            | "fsVarsGetEVarId"
            | "fsVarsGetLVarId"
            | "fsVarsAVarGet"
            | "fsVarsAVarBufferGet"
            | "fsVarsLVarGet"
            | "fsVarsLVarSet"
            | "fsVarsRegisterLVar"
            | "fsVarsEVarGet"
            | "fsRenderCreate"
            | "fsRenderCreateTexture"
            | "fsRenderDeleteTexture"
            | "fsRenderUpdateTexture"
            | "fsRenderGetTextureSize"
            | "fsRenderViewport"
            | "fsRenderCancel"
            | "fsRenderFlush"
            | "fsRenderFill"
            | "fsRenderStroke"
            | "fsRenderTriangles"
            | "fsRenderClearStencil"
            | "fsRenderDelete"
            | "fsCommBusRegister"
            | "fsCommBusCall"
            | "fsCommBusUnregisterOneEvent"
            | "fsCommBusUnregisterAll"
            | "fsCommBusUnregister"
            | "fsEventsRegisterKeyEventHandler"
            | "fsEventsUnregisterKeyEventHandler"
            | "fsEventsTriggerKeyEvent"
            | "fsIOOpen"
            | "fsIOOpenRead"
            | "fsIORead"
            | "fsIOWrite"
            | "fsIOClose"
            | "fsIOIsOpened"
            | "fsIOInProgress"
            | "fsIOIsDone"
            | "fsIOHasError"
            | "fsIOGetLastError"
            | "fsIOGetFileSize"
            | "fsUtilsGetStrCRC"
            | "lookup_var"
            | "execute_calculator_code"
            | "SimConnect_Open"
            | "SimConnect_Close"
            | "SimConnect_GetNextDispatch"
            | "SimConnect_CallDispatch"
            | "SimConnect_MapClientEventToSimEvent"
            | "SimConnect_AddClientEventToNotificationGroup"
            | "SimConnect_SetNotificationGroupPriority"
            | "SimConnect_SubscribeToSystemEvent"
            | "SimConnect_RequestSystemState"
            | "SimConnect_TransmitClientEvent"
            | "SimConnect_TransmitClientEvent_EX1"
            | "SimConnect_AddToDataDefinition"
            | "SimConnect_RequestDataOnSimObject"
            | "SimConnect_RequestDataOnSimObjectType"
            | "SimConnect_SetDataOnSimObject"
            | "SimConnect_RequestClientData"
            | "SimConnect_SetClientData"
            | "SimConnect_CreateClientData"
            | "SimConnect_MapClientDataNameToID"
            | "SimConnect_AddToClientDataDefinition"
            | "SimConnect_RequestFacilityData"
            | "SimConnect_RequestFacilityData_EX1"
            | "SimConnect_AddToFacilityDefinition"
            | "SimConnect_GetLastSentPacketID"
            | "SimConnect_AICreateSimulatedObject"
            | "SimConnect_AIRemoveObject"
    )
}

fn register_hosts(
    linker: &mut Linker<HostState>,
    engine: &Engine,
    module: &Module,
    config: &EngineConfig,
) -> Result<Vec<String>> {
    let mut unsupported = Vec::new();
    for import in module.imports() {
        let module_name: String = import.module().to_string();
        let name: String = import.name().to_string();
        let wasmtime::ExternType::Func(ty) = import.ty() else { continue };
        if module_name == "wasi_snapshot_preview1" {
            // wasmtime-wasi defines the standard preview1 surface; anything it
            // does not define (e.g. commit_pages) gets a zero-returning stub so
            // the module can still initialise. Shadowing is refused by the
            // linker, which is how we tell the two cases apart.
            let result = linker.func_new(&module_name, &name, ty, |_c, _p, results| {
                for slot in results.iter_mut() {
                    *slot = Val::I32(0);
                }
                Ok(())
            });
            if result.is_ok() {
                unsupported.push(format!("{module_name}.{name}"));
            }
            continue;
        }
        if module_name == "env" && is_known_env(&name) {
            let result = linker.func_new("env", &name, ty, {
                let name = name.clone();
                move |mut caller, params, results| {
                    dispatch(&mut caller, &name, params, results)
                        .map_err(|e| wasmtime::Error::msg(e.to_string()))
                }
            });
            match result {
                Ok(_) => {}
                Err(e) => {
                    unsupported.push(format!("{module_name}.{name}: {e}"));
                }
            }
            continue;
        }
        unsupported.push(format!("{module_name}.{name}"));
        let label = format!("{module_name}.{name}");
        let _ = linker.func_new(&module_name, &name, ty, {
            let label = label.clone();
            move |mut caller, _params, results| {
                let label = label.clone();
                let state = caller.data_mut();
                if !state.unsupported.contains(&label) {
                    state.unsupported.push(label.clone());
                }
                let _ = state.events.send(HostEvent::Unsupported(label));
                for r in results.iter_mut() {
                    *r = default_val();
                }
                Ok(())
            }
        });
    }
    let _ = (engine, config);
    Ok(unsupported)
}

fn default_val() -> Val {
    Val::I32(0)
}

fn i32_at(params: &[Val], i: usize) -> i32 {
    match params.get(i) {
        Some(Val::I32(v)) => *v,
        _ => 0,
    }
}

fn f32_at(params: &[Val], i: usize) -> f32 {
    match params.get(i) {
        Some(Val::F32(v)) => f32::from_bits(*v),
        _ => 0.0,
    }
}

fn f64_at(params: &[Val], i: usize) -> f64 {
    match params.get(i) {
        Some(Val::F64(v)) => f64::from_bits(*v),
        _ => 0.0,
    }
}

fn i64_at(params: &[Val], i: usize) -> i64 {
    match params.get(i) {
        Some(Val::I64(v)) => *v,
        _ => 0,
    }
}

fn set_i32(results: &mut [Val], value: i32) {
    if let Some(slot) = results.first_mut() {
        *slot = Val::I32(value);
    }
}

fn mem(caller: &Caller<'_, HostState>) -> Option<Memory> {
    caller.data().memory
}

fn read_str(caller: &mut Caller<'_, HostState>, ptr: i32) -> String {
    let Some(memory) = mem(caller) else { return String::new() };
    if ptr <= 0 {
        return String::new();
    }
    let data = memory.data(&*caller);
    let start = ptr as usize;
    if start >= data.len() {
        return String::new();
    }
    let end = data[start..].iter().position(|b| *b == 0).map(|n| start + n).unwrap_or(data.len());
    String::from_utf8_lossy(&data[start..end]).to_string()
}

fn read_bytes(caller: &mut Caller<'_, HostState>, ptr: i32, len: i32) -> Vec<u8> {
    let Some(memory) = mem(caller) else { return Vec::new() };
    if ptr <= 0 || len <= 0 {
        return Vec::new();
    }
    let data = memory.data(&*caller);
    let start = ptr as usize;
    let end = (start + len as usize).min(data.len());
    if start >= end {
        return Vec::new();
    }
    data[start..end].to_vec()
}

fn write_f64(caller: &mut Caller<'_, HostState>, ptr: i32, value: f64) {
    if ptr <= 0 {
        return;
    }
    let Some(memory) = mem(caller) else { return };
    let _ = memory.write(&mut *caller, ptr as usize, &value.to_le_bytes());
}

fn write_i32(caller: &mut Caller<'_, HostState>, ptr: i32, value: i32) {
    if ptr <= 0 {
        return;
    }
    let Some(memory) = mem(caller) else { return };
    let _ = memory.write(&mut *caller, ptr as usize, &value.to_le_bytes());
}

fn read_struct<T: Copy>(caller: &mut Caller<'_, HostState>, ptr: i32) -> Option<T> {
    let memory = mem(caller)?;
    if ptr <= 0 {
        return None;
    }
    let mut buf = vec![0u8; std::mem::size_of::<T>()];
    memory.read(&*caller, ptr as usize, &mut buf).ok()?;
    Some(unsafe { std::ptr::read_unaligned(buf.as_ptr() as *const T) })
}

fn read_vertices(caller: &mut Caller<'_, HostState>, ptr: i32, count: i32) -> Vec<Vertex> {
    if ptr <= 0 || count <= 0 {
        return Vec::new();
    }
    let Some(memory) = mem(caller) else { return Vec::new() };
    let mut out = Vec::with_capacity(count as usize);
    let mut buf = vec![0u8; std::mem::size_of::<FsVertex>()];
    for i in 0..count {
        let addr = ptr as usize + i as usize * std::mem::size_of::<FsVertex>();
        if memory.read(&*caller, addr, &mut buf).is_err() {
            break;
        }
        let v: FsVertex = unsafe { std::ptr::read_unaligned(buf.as_ptr() as *const FsVertex) };
        out.push(Vertex { x: v.x, y: v.y, u: v.u, v: v.v });
    }
    out
}

fn dispatch(caller: &mut Caller<'_, HostState>, name: &str, params: &[Val], results: &mut [Val]) -> Result<()> {
    match name {
        "fsVarsGetUnitId" => {
            let text = read_str(caller, i32_at(params, 0));
            let id = caller.data().sim().unit_id(&text);
            set_i32(results, id);
        }
        "fsVarsGetAVarId" => {
            let text = read_str(caller, i32_at(params, 0));
            let id = caller.data().sim().simvar_id(&text);
            set_i32(results, id);
        }
        "fsVarsGetLVarId" => {
            let text = read_str(caller, i32_at(params, 0));
            let id = caller.data().sim().register_lvar(&text);
            set_i32(results, id);
        }
        "fsVarsRegisterLVar" => {
            let text = read_str(caller, i32_at(params, 0));
            let id = caller.data().sim().register_lvar(&text);
            set_i32(results, id);
        }
        "fsVarsGetEVarId" => {
            let text = read_str(caller, i32_at(params, 0));
            let id = caller.data().sim().register_lvar(&format!("E:{text}"));
            set_i32(results, id);
        }
        "fsVarsAVarGet" => {
            let id = i32_at(params, 0);
            let out = i32_at(params, 4);
            let value = caller.data().sim().get(id).unwrap_or(0.0);
            write_f64(caller, out, value);
            set_i32(results, FS_VAR_ERROR_NONE as i32);
        }
        "fsVarsLVarGet" => {
            let id = i32_at(params, 0);
            let out = i32_at(params, 2);
            let value = caller.data().sim().lvar_get(id).unwrap_or(0.0);
            write_f64(caller, out, value);
            set_i32(results, FS_VAR_ERROR_NONE as i32);
        }
        "fsVarsLVarSet" => {
            let id = i32_at(params, 0);
            let value = f64_at(params, 2);
            let ok = caller.data().sim().lvar_set(id, value);
            set_i32(results, if ok { FS_VAR_ERROR_NONE as i32 } else { FS_VAR_ERROR_INVALID_ARGS as i32 });
        }
        "fsVarsEVarGet" => {
            let id = i32_at(params, 0);
            let out = i32_at(params, 2);
            let value = caller.data().sim().lvar_get(id).unwrap_or(0.0);
            write_f64(caller, out, value);
            set_i32(results, FS_VAR_ERROR_NONE as i32);
        }
        "fsVarsAVarBufferGet" => {
            set_i32(results, FS_VAR_ERROR_INVALID_ARGS as i32);
        }
        "fsRenderCreate" => {
            let id = caller.data_mut().render.create_texture(false, 1, 1, None);
            set_i32(results, id);
        }
        "fsRenderCreateTexture" => {
            let kind = i32_at(params, 1);
            let w = i32_at(params, 2).max(1) as u32;
            let h = i32_at(params, 3).max(1) as u32;
            let data_ptr = i32_at(params, 5);
            let bytes = if data_ptr > 0 {
                Some(read_bytes(caller, data_ptr, (w * h * if kind == 1 { 1 } else { 4 }) as i32))
            } else {
                None
            };
            let id = caller
                .data_mut()
                .render
                .create_texture(kind == 1, w, h, bytes.as_deref());
            set_i32(results, id);
        }
        "fsRenderDeleteTexture" => {
            let id = i32_at(params, 1);
            caller.data_mut().render.textures.remove(&id);
            set_i32(results, id);
        }
        "fsRenderUpdateTexture" => {
            let id = i32_at(params, 1);
            let x = i32_at(params, 2);
            let y = i32_at(params, 3);
            let w = i32_at(params, 4);
            let h = i32_at(params, 5);
            let data_ptr = i32_at(params, 6);
            // nanovg passes the whole texture here and the dirty rect
            // separately, so read the full buffer rather than w*h.
            let (alpha, width, height) = caller
                .data()
                .render
                .textures
                .get(&id)
                .map(|t| (t.alpha, t.width, t.height))
                .unwrap_or((false, 1, 1));
            let bpp = if alpha { 1 } else { 4 };
            let bytes = read_bytes(caller, data_ptr, (width * height * bpp) as i32);
            let ok = caller.data_mut().render.update_texture(id, x, y, w, h, &bytes);
            set_i32(results, if ok { 1 } else { 0 });
        }
        "fsRenderGetTextureSize" => {
            let id = i32_at(params, 1);
            let w_ptr = i32_at(params, 2);
            let h_ptr = i32_at(params, 3);
            let size = caller.data().render.textures.get(&id).map(|t| (t.width as i32, t.height as i32));
            let (w, h) = size.unwrap_or((0, 0));
            write_i32(caller, w_ptr, w);
            write_i32(caller, h_ptr, h);
            set_i32(results, 1);
        }
        "fsRenderViewport" => {
            let w = f32_at(params, 1);
            let h = f32_at(params, 2);
            if w >= 1.0 && h >= 1.0 {
                caller.data_mut().render.surface.resize(w as u32, h as u32);
            }
        }
        "fsRenderCancel" => {}
        "fsRenderFlush" => {
            caller.data_mut().render.end_frame();
            caller.data_mut().frame_dirty = true;
            let ctx = caller.data().render_ctx;
            let _ = caller.data().events.send(HostEvent::Frame { context: ctx });
        }
        "fsRenderFill" => {
            let paint = read_struct::<FsPaint>(caller, i32_at(params, 1));
            if std::env::var("FSRUNTIME_TRACE_RENDER").is_ok() {
                eprintln!(
                    "fill paint={:?} paths={} npaths={}",
                    paint.map(|p| (p.inner_color, p.image, p.extent, p.xform)),
                    i32_at(params, 6),
                    i32_at(params, 7)
                );
            }
            let scissor = read_struct::<FsScissor>(caller, i32_at(params, 3));
            let paths_ptr = i32_at(params, 6);
            let npaths = i32_at(params, 7);
            if let (Some(paint), Some(scissor)) = (paint, scissor) {
                let paint = paint.to_paint();
                let scissor: Scissor = scissor.to_scissor();
                for i in 0..npaths.max(0) {
                    let addr = paths_ptr + i * std::mem::size_of::<FsPath>() as i32;
                    let Some(path) = read_struct::<FsPath>(caller, addr) else { continue };
                    let fan = read_vertices(caller, path.fill as i32, path.nfill);
                    let strip = read_vertices(caller, path.stroke as i32, path.nstroke);
                    if std::env::var("FSRUNTIME_TRACE_RENDER").is_ok() {
                        if let Some(memory) = caller.data().memory {
                            let mut dump = [0u8; 40];
                            let _ = memory.read(&*caller, paths_ptr as usize, &mut dump);
                            eprintln!("  paths bytes: {}", dump.iter().map(|b| format!("{b:02x}")).collect::<String>());
                            let mut v = [0u8; 64];
                            let _ = memory.read(&*caller, 0x33c00usize, &mut v);
                            eprintln!("  @33c00: {}", v.iter().map(|b| format!("{b:02x}")).collect::<String>());
                        }
                        eprintln!(
                            "  raw path @{addr}: first={} count={} closed={} nbevel={} fill=0x{:x} nfill={} stroke=0x{:x} nstroke={} winding={} convex={}",
                            path.first, path.count, path.closed, path.nbevel, path.fill, path.nfill,
                            path.stroke, path.nstroke, path.winding, path.convex
                        );
                        eprintln!(
                            "  path nfill={} nstroke={} fan_first={:?} fan_last={:?} strip_first={:?} strip_last={:?}",
                            path.nfill,
                            path.nstroke,
                            fan.first().map(|v| (v.x, v.y)),
                            fan.last().map(|v| (v.x, v.y)),
                            strip.first().map(|v| (v.x, v.y)),
                            strip.last().map(|v| (v.x, v.y)),
                        );
                    }
                    caller.data_mut().render.fill_path(&paint, &scissor, &fan, &strip);
                }
            }
        }
        "fsRenderStroke" => {
            let paint = read_struct::<FsPaint>(caller, i32_at(params, 1));
            let scissor = read_struct::<FsScissor>(caller, i32_at(params, 3));
            let paths_ptr = i32_at(params, 6);
            let npaths = i32_at(params, 7);
            if let (Some(paint), Some(scissor)) = (paint, scissor) {
                let paint = paint.to_paint();
                let scissor: Scissor = scissor.to_scissor();
                for i in 0..npaths.max(0) {
                    let addr = paths_ptr + i * std::mem::size_of::<FsPath>() as i32;
                    let Some(path) = read_struct::<FsPath>(caller, addr) else { continue };
                    let strip = read_vertices(caller, path.stroke as i32, path.nstroke);
                    caller.data_mut().render.stroke(&paint, &scissor, &[strip]);
                }
            }
        }
        "fsRenderTriangles" => {
            let paint = read_struct::<FsPaint>(caller, i32_at(params, 1));
            if std::env::var("FSRUNTIME_TRACE_RENDER").is_ok() {
                eprintln!(
                    "triangles paint={:?} nverts={}",
                    paint.map(|p| (p.inner_color, p.image, p.extent)),
                    i32_at(params, 5)
                );
            }
            let scissor = read_struct::<FsScissor>(caller, i32_at(params, 3));
            let verts_ptr = i32_at(params, 4);
            let nverts = i32_at(params, 5);
            if let (Some(paint), Some(scissor)) = (paint, scissor) {
                let verts = read_vertices(caller, verts_ptr, nverts);
                caller
                    .data_mut()
                    .render
                    .triangles(&paint.to_paint(), &scissor.to_scissor(), &verts);
            }
        }
        "fsRenderClearStencil" => {}
        "fsRenderDelete" => {}
        "fsCommBusRegister" => {
            let event = read_str(caller, i32_at(params, 0));
            let callback = i32_at(params, 1);
            let context = i32_at(params, 2);
            caller
                .data_mut()
                .commbus
                .entry(event)
                .or_default()
                .push(CommBusHandler { callback: callback as u32, context: context as u32 });
            set_i32(results, 1);
        }
        "fsCommBusUnregisterOneEvent" => {
            let event = read_str(caller, i32_at(params, 0));
            let callback = i32_at(params, 1);
            let context = i32_at(params, 2);
            let removed = caller
                .data_mut()
                .commbus
                .get_mut(&event)
                .map(|list| {
                    let before = list.len();
                    list.retain(|h| h.callback != callback as u32 || h.context != context as u32);
                    before != list.len()
                })
                .unwrap_or(false);
            set_i32(results, if removed { 1 } else { 0 });
        }
        "fsCommBusUnregisterAll" => {
            caller.data_mut().commbus.clear();
            set_i32(results, 1);
        }
        "fsCommBusUnregister" => {
            caller.data_mut().commbus.clear();
            set_i32(results, 1);
        }
        "fsCommBusCall" => {
            let event = read_str(caller, i32_at(params, 0));
            let buf = i32_at(params, 1);
            let size = i32_at(params, 2);
            let flags = i32_at(params, 3) as u32;
            let payload = read_bytes(caller, buf, size);
            let _ = caller.data().events.send(HostEvent::CommBusCall {
                name: event,
                payload,
                broadcast: flags,
            });
            set_i32(results, 1);
        }
        "fsEventsRegisterKeyEventHandler" => {
            let handler = i32_at(params, 0);
            let user = i32_at(params, 1);
            caller.data_mut().key_handlers.push((handler as u32, user as u32));
        }
        "fsEventsUnregisterKeyEventHandler" => {
            let handler = i32_at(params, 0);
            let user = i32_at(params, 1);
            caller
                .data_mut()
                .key_handlers
                .retain(|(h, u)| *h != handler as u32 || *u != user as u32);
        }
        "fsEventsTriggerKeyEvent" => {
            let id = i32_at(params, 0);
            let _ = caller.data().events.send(HostEvent::KeyEvent { id: id as u32 });
        }
        "fsUtilsGetStrCRC" => {
            let text = read_str(caller, i32_at(params, 0));
            let crc = crc64(text.as_bytes());
            if let Some(slot) = results.first_mut() {
                *slot = Val::I64(crc as i64);
            }
        }
        "fsIOOpen" | "fsIOOpenRead" => {
            let path = read_str(caller, i32_at(params, 0));
            let handle = caller.data().next_io;
            caller.data_mut().next_io += 1;
            let resolved = caller
                .data()
                .data_dir
                .as_ref()
                .map(|d| d.join(path.trim_start_matches(['.', '/', '\\'])));
            let data = resolved.as_ref().and_then(|p| std::fs::read(p).ok()).unwrap_or_default();
            let found = resolved.as_ref().map(|p| p.is_file()).unwrap_or(false);
            caller.data_mut().io.insert(
                handle,
                IoFile { data, position: 0, open: found, error: if found { 0 } else { 2 } },
            );
            if let Some(slot) = results.first_mut() {
                *slot = Val::I64(handle as i64);
            }
        }
        "fsIORead" => {
            let handle = i64_at(params, 0) as u64;
            let out_ptr = i32_at(params, 1);
            let offset = i32_at(params, 2) as u64;
            let count = i32_at(params, 3) as usize;
            let mut chunk = Vec::new();
            if let Some(file) = caller.data_mut().io.get_mut(&handle) {
                let start = offset.min(file.data.len() as u64) as usize;
                let end = (start + count).min(file.data.len());
                chunk = file.data[start..end].to_vec();
                file.position = end as u64;
            }
            if let Some(memory) = mem(caller) {
                let _ = memory.write(&mut *caller, out_ptr as usize, &chunk);
            }
            set_i32(results, FS_VAR_ERROR_NONE as i32);
        }
        "fsIOWrite" => {
            set_i32(results, FS_VAR_ERROR_NONE as i32);
        }
        "fsIOClose" => {
            let handle = i64_at(params, 0) as u64;
            caller.data_mut().io.remove(&handle);
            set_i32(results, FS_VAR_ERROR_NONE as i32);
        }
        "fsIOIsOpened" | "fsIOIsDone" => {
            let handle = i64_at(params, 0) as u64;
            let open = caller.data().io.get(&handle).map(|f| f.open).unwrap_or(false);
            set_i32(results, if open { 1 } else { 0 });
        }
        "fsIOInProgress" => {
            set_i32(results, 0);
        }
        "fsIOHasError" => {
            let handle = i64_at(params, 0) as u64;
            let err = caller.data().io.get(&handle).map(|f| f.error).unwrap_or(2);
            set_i32(results, if err != 0 { 1 } else { 0 });
        }
        "fsIOGetLastError" => {
            let handle = i64_at(params, 0) as u64;
            let err = caller.data().io.get(&handle).map(|f| f.error).unwrap_or(2);
            set_i32(results, err as i32);
        }
        "fsIOGetFileSize" => {
            let handle = i64_at(params, 0) as u64;
            let size = caller.data().io.get(&handle).map(|f| f.data.len() as i64).unwrap_or(0);
            if let Some(slot) = results.first_mut() {
                *slot = Val::I64(size);
            }
        }
        "SimConnect_Open" => {
            caller.data_mut().simconnect.open = true;
            let open = simconnect_open_message();
            caller.data_mut().simconnect.pending.push(open);
            set_i32(results, 0);
        }
        "SimConnect_Close" => {
            caller.data_mut().simconnect.open = false;
            set_i32(results, 0);
        }
        "SimConnect_GetNextDispatch" | "SimConnect_CallDispatch" => {
            let out_ptr = i32_at(params, 1);
            let size_ptr = i32_at(params, 2);
            let message = caller.data_mut().simconnect.pending.pop();
            match message {
                Some(bytes) => {
                    if let Some(memory) = mem(caller) {
                        let _ = memory.write(&mut *caller, out_ptr as usize, &bytes);
                    }
                    write_i32(caller, size_ptr, bytes.len() as i32);
                    set_i32(results, 0);
                }
                None => set_i32(results, 1),
            }
        }
        "SimConnect_MapClientEventToSimEvent" => set_i32(results, 0),
        "SimConnect_AddClientEventToNotificationGroup" => set_i32(results, 0),
        "SimConnect_SetNotificationGroupPriority" => set_i32(results, 0),
        "SimConnect_SubscribeToSystemEvent" => set_i32(results, 0),
        "SimConnect_RequestSystemState" => set_i32(results, 0),
        "SimConnect_TransmitClientEvent" => set_i32(results, 0),
        "SimConnect_TransmitClientEvent_EX1" => set_i32(results, 0),
        "SimConnect_AddToDataDefinition" => {
            let define = i32_at(params, 1);
            let datum = read_str(caller, i32_at(params, 2));
            let unit = read_str(caller, i32_at(params, 3));
            let dtype = i32_at(params, 4);
            caller
                .data_mut()
                .simconnect
                .data_defs
                .entry(define as u32)
                .or_default()
                .push((datum, 0, dtype as u32));
            let _ = unit;
            set_i32(results, 0);
        }
        "SimConnect_RequestDataOnSimObject" => {
            let request = caller.data().simconnect.next_request;
            caller.data_mut().simconnect.next_request += 1;
            write_i32(caller, i32_at(params, 1), request as i32);
            set_i32(results, 0);
        }
        "SimConnect_RequestDataOnSimObjectType" => set_i32(results, 0),
        "SimConnect_SetDataOnSimObject" => set_i32(results, 0),
        "SimConnect_RequestClientData" => set_i32(results, 0),
        "SimConnect_SetClientData" => set_i32(results, 0),
        "SimConnect_CreateClientData" => set_i32(results, 0),
        "SimConnect_MapClientDataNameToID" => set_i32(results, 0),
        "SimConnect_AddToClientDataDefinition" => set_i32(results, 0),
        "SimConnect_RequestFacilityData" => set_i32(results, 0),
        "SimConnect_RequestFacilityData_EX1" => set_i32(results, 0),
        "SimConnect_AddToFacilityDefinition" => set_i32(results, 0),
        "SimConnect_GetLastSentPacketID" => set_i32(results, 0),
        "SimConnect_AICreateSimulatedObject" => set_i32(results, 0),
        "SimConnect_AIRemoveObject" => set_i32(results, 0),
        "lookup_var" | "execute_calculator_code" => {
            let label = format!("env.{name}");
            let state = caller.data_mut();
            if !state.unsupported.contains(&label) {
                state.unsupported.push(label.clone());
                let _ = state.events.send(HostEvent::Unsupported(label));
            }
            set_i32(results, 0);
        }
        other => {
            let label = format!("env.{other}");
            let state = caller.data_mut();
            if !state.unsupported.contains(&label) {
                state.unsupported.push(label.clone());
                let _ = state.events.send(HostEvent::Unsupported(label));
            }
            for r in results.iter_mut() {
                *r = default_val();
            }
        }
    }
    Ok(())
}

fn simconnect_open_message() -> Vec<u8> {
    // SIMCONNECT_RECV_OPEN is 4+4+4 + 256 + 4*10 bytes, packed(1).
    let mut msg = vec![0u8; 4 + 4 + 4 + 256 + 40];
    let size = msg.len() as u32;
    msg[0..4].copy_from_slice(&size.to_le_bytes());
    msg[4..8].copy_from_slice(&1u32.to_le_bytes());
    msg[8..12].copy_from_slice(&2u32.to_le_bytes());
    let name = b"fsruntime";
    msg[12..12 + name.len()].copy_from_slice(name);
    let base = 12 + 256;
    for (i, v) in [1u32, 0, 0, 0, 11, 0, 0, 0, 0, 0].iter().enumerate() {
        msg[base + i * 4..base + i * 4 + 4].copy_from_slice(&v.to_le_bytes());
    }
    msg
}

fn crc64(data: &[u8]) -> u64 {
    let mut crc = u64::MAX;
    for byte in data {
        crc ^= *byte as u64;
        for _ in 0..8 {
            let mask = (crc & 1).wrapping_neg();
            crc = (crc >> 1) ^ (0xC96C_5795_D787_0F42 & mask);
        }
    }
    !crc
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crc64_is_stable() {
        assert_eq!(crc64(b""), 0);
        assert_ne!(crc64(b"a"), crc64(b"b"));
        assert_eq!(crc64(b"a"), crc64(b"a"));
    }

    #[test]
    fn simconnect_open_message_has_correct_header() {
        let msg = simconnect_open_message();
        assert_eq!(u32::from_le_bytes(msg[0..4].try_into().unwrap()) as usize, msg.len());
        assert_eq!(u32::from_le_bytes(msg[8..12].try_into().unwrap()), 2);
    }

    #[test]
    fn known_env_covers_observed_imports() {
        for name in [
            "fsVarsGetAVarId",
            "fsRenderTriangles",
            "fsCommBusCall",
            "fsEventsTriggerKeyEvent",
            "SimConnect_Open",
        ] {
            assert!(is_known_env(name), "{name} should be recognized");
        }
        assert!(!is_known_env("fsSomethingElse"));
    }
}
