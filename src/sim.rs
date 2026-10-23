use std::collections::HashMap;

pub type SimVarId = i32;
pub type NamedVarId = i32;
pub type UnitId = i32;
pub type EventId = i32;

pub const SIM_OBJECT_ID_USER: u32 = 0;

pub const FS_VAR_ERROR_NONE: u32 = 0;
pub const FS_VAR_ERROR_FAIL: u32 = 0xffff_ffff;
pub const FS_VAR_ERROR_BAD_DATA: u32 = 2;
pub const FS_VAR_ERROR_NOT_SUPPORTED: u32 = 3;
pub const FS_VAR_ERROR_INVALID_ARGS: u32 = 4;

#[derive(Clone, Debug)]
pub struct SimVarDef {
    pub name: String,
    pub unit: String,
    pub value: f64,
    pub writable: bool,
}

#[derive(Clone, Debug)]
pub struct EventRecord {
    pub id: EventId,
    pub name: String,
    pub data: Vec<f64>,
}

#[derive(Default)]
pub struct SimState {
    pub title: String,
    simvars: Vec<SimVarDef>,
    simvar_index: HashMap<String, SimVarId>,
    units: Vec<String>,
    unit_index: HashMap<String, UnitId>,
    lvars: Vec<(String, f64)>,
    lvar_index: HashMap<String, NamedVarId>,
    hvars: Vec<(String, f64)>,
    hvar_index: HashMap<String, NamedVarId>,
    pub events: Vec<EventRecord>,
    pub event_log: Vec<String>,
    pub elapsed: f64,
    pub last_change: Option<String>,
}

impl SimState {
    pub fn new() -> Self {
        let mut state = SimState::default();
        for (name, unit, value, writable) in default_simvars() {
            state.define_simvar(name, unit, value, writable);
        }
        state
    }

    pub fn define_simvar(&mut self, name: &str, unit: &str, value: f64, writable: bool) -> SimVarId {
        let key = name.to_ascii_uppercase();
        if let Some(id) = self.simvar_index.get(&key) {
            return *id;
        }
        let id = self.simvars.len() as SimVarId;
        self.simvars.push(SimVarDef { name: key.clone(), unit: unit.to_string(), value, writable });
        self.simvar_index.insert(key, id);
        id
    }

    pub fn simvar_id(&mut self, name: &str) -> SimVarId {
        let key = name.to_ascii_uppercase();
        if let Some(id) = self.simvar_index.get(&key) {
            return *id;
        }
        self.define_simvar(name, "number", 0.0, false)
    }

    pub fn simvar_by_id(&self, id: SimVarId) -> Option<&SimVarDef> {
        self.simvars.get(id as usize)
    }

    pub fn simvar_name(&self, id: SimVarId) -> String {
        self.simvars
            .get(id as usize)
            .map(|v| v.name.clone())
            .unwrap_or_else(|| format!("UNKNOWN:{id}"))
    }

    pub fn get(&self, id: SimVarId) -> Option<f64> {
        self.simvars.get(id as usize).map(|v| v.value)
    }

    pub fn get_named(&self, name: &str) -> f64 {
        self.simvar_index
            .get(&name.to_ascii_uppercase())
            .and_then(|id| self.get(*id))
            .unwrap_or(0.0)
    }

    pub fn set(&mut self, id: SimVarId, value: f64) -> bool {
        let Some(def) = self.simvars.get_mut(id as usize) else { return false };
        def.value = value;
        self.last_change = Some(format!("A:{} = {value}", def.name));
        true
    }

    pub fn set_named(&mut self, name: &str, value: f64) {
        let id = self.simvar_id(name);
        self.set(id, value);
    }

    pub fn set_title(&mut self, title: &str) {
        self.title = title.to_string();
        self.set_named("TITLE", 0.0);
    }

    pub fn unit_id(&mut self, name: &str) -> UnitId {
        let key = name.to_ascii_uppercase();
        if let Some(id) = self.unit_index.get(&key) {
            return *id;
        }
        let id = self.units.len() as UnitId;
        self.units.push(key.clone());
        self.unit_index.insert(key, id);
        id
    }

    pub fn unit_name(&self, id: UnitId) -> &str {
        self.units.get(id as usize).map(String::as_str).unwrap_or("number")
    }

    pub fn register_lvar(&mut self, name: &str) -> NamedVarId {
        let key = name.to_ascii_uppercase();
        if let Some(id) = self.lvar_index.get(&key) {
            return *id;
        }
        let id = self.lvars.len() as NamedVarId;
        self.lvars.push((name.to_string(), 0.0));
        self.lvar_index.insert(key, id);
        id
    }

    pub fn lvar_id(&mut self, name: &str) -> NamedVarId {
        self.register_lvar(name)
    }

    pub fn lvar_get(&self, id: NamedVarId) -> Option<f64> {
        self.lvars.get(id as usize).map(|(_, v)| *v)
    }

    pub fn lvar_set(&mut self, id: NamedVarId, value: f64) -> bool {
        let Some(entry) = self.lvars.get_mut(id as usize) else { return false };
        entry.1 = value;
        self.last_change = Some(format!("L:{} = {value}", entry.0));
        true
    }

    pub fn lvar_by_name(&mut self, name: &str) -> Option<f64> {
        let key = name.to_ascii_uppercase();
        self.lvar_index.get(&key).and_then(|id| self.lvar_get(*id))
    }

    pub fn lvars(&self) -> impl Iterator<Item = &(String, f64)> {
        self.lvars.iter()
    }

    pub fn hvar_id(&mut self, name: &str) -> NamedVarId {
        let key = name.to_ascii_uppercase();
        if let Some(id) = self.hvar_index.get(&key) {
            return *id;
        }
        let id = self.hvars.len() as NamedVarId;
        self.hvars.push((name.to_string(), 0.0));
        self.hvar_index.insert(key, id);
        id
    }

    pub fn hvar_get(&self, id: NamedVarId) -> Option<f64> {
        self.hvars.get(id as usize).map(|(_, v)| *v)
    }

    pub fn hvar_set(&mut self, id: NamedVarId, value: f64) -> bool {
        let Some(entry) = self.hvars.get_mut(id as usize) else { return false };
        entry.1 = value;
        self.last_change = Some(format!("H:{} = {value}", entry.0));
        true
    }

    pub fn hvar_by_name(&mut self, name: &str) -> Option<f64> {
        let key = name.to_ascii_uppercase();
        self.hvar_index.get(&key).and_then(|id| self.hvar_get(*id))
    }

    pub fn push_event(&mut self, id: EventId, name: &str, data: &[f64]) {
        self.event_log.push(format!("#{id} {name} {data:?}"));
        if self.event_log.len() > 200 {
            self.event_log.remove(0);
        }
        self.events.push(EventRecord { id, name: name.to_string(), data: data.to_vec() });
    }

    pub fn take_events(&mut self) -> Vec<EventRecord> {
        std::mem::take(&mut self.events)
    }

    pub fn tick(&mut self, dt: f64) {
        self.elapsed += dt;
        self.set_named("ABSOLUTE TIME", self.elapsed);
        self.set_named("E:ABSOLUTE TIME", self.elapsed);
    }

    pub fn simvar_table(&self) -> &[SimVarDef] {
        &self.simvars
    }

    pub fn set_simvar_by_name(&mut self, name: &str, value: f64) {
        let id = self.simvar_id(name);
        self.set(id, value);
    }
}

fn default_simvars() -> Vec<(&'static str, &'static str, f64, bool)> {
    vec![
        ("PLANE LATITUDE", "degrees", 47.4647, true),
        ("PLANE LONGITUDE", "degrees", -122.3088, true),
        ("PLANE ALTITUDE", "feet", 0.0, true),
        ("PLANE ALT ABOVE GROUND", "feet", 0.0, true),
        ("PLANE HEADING DEGREES TRUE", "degrees", 0.0, true),
        ("PLANE HEADING DEGREES MAGNETIC", "degrees", 0.0, true),
        ("PLANE PITCH DEGREES", "degrees", 0.0, true),
        ("PLANE BANK DEGREES", "degrees", 0.0, true),
        ("PLANE ALTITUDE RATE", "feet per second", 0.0, false),
        ("AIRSPEED INDICATED", "knots", 0.0, true),
        ("AIRSPEED TRUE", "knots", 0.0, true),
        ("AIRSPEED MACH", "mach", 0.0, false),
        ("GROUND VELOCITY", "knots", 0.0, false),
        ("VERTICAL SPEED", "feet per minute", 0.0, false),
        ("ATTITUDE INDICATOR PITCH DEGREES", "degrees", 0.0, false),
        ("ATTITUDE INDICATOR BANK DEGREES", "degrees", 0.0, false),
        ("VELOCITY BODY X", "feet per second", 0.0, false),
        ("VELOCITY BODY Y", "feet per second", 0.0, false),
        ("VELOCITY BODY Z", "feet per second", 0.0, false),
        ("VELOCITY WORLD X", "feet per second", 0.0, false),
        ("VELOCITY WORLD Y", "feet per second", 0.0, false),
        ("VELOCITY WORLD Z", "feet per second", 0.0, false),
        ("ACCELERATION BODY X", "feet per second squared", 0.0, false),
        ("ACCELERATION BODY Y", "feet per second squared", 0.0, false),
        ("ACCELERATION BODY Z", "feet per second squared", 0.0, false),
        ("INCIDENCE ALPHA", "degrees", 0.0, false),
        ("AMBIENT TEMPERATURE", "celsius", 15.0, true),
        ("AMBIENT PRESSURE", "inches of mercury", 29.92, true),
        ("AMBIENT WIND Y", "knots", 0.0, false),
        ("AMBIENT WIND VELOCITY", "knots", 0.0, false),
        ("SEA LEVEL PRESSURE", "millibars", 1013.25, true),
        ("MAGVAR", "degrees", 0.0, false),
        ("IS SLEW ACTIVE", "bool", 0.0, true),
        ("SIM ON GROUND", "bool", 1.0, true),
        ("GEAR HANDLE POSITION", "bool", 1.0, true),
        ("GEAR POSITION", "percent", 100.0, true),
        ("FLAPS HANDLE INDEX", "number", 0.0, true),
        ("FLAPS HANDLE PERCENT", "percent", 0.0, true),
        ("BRAKE PARKING POSITION", "bool", 1.0, true),
        ("AUTOPILOT MASTER", "bool", 0.0, true),
        ("AUTOPILOT HEADING LOCK", "bool", 0.0, true),
        ("AUTOPILOT ALTITUDE LOCK", "bool", 0.0, true),
        ("AUTOPILOT VERTICAL HOLD", "bool", 0.0, true),
        ("AUTOPILOT AIRSPEED HOLD", "bool", 0.0, true),
        ("AUTOPILOT APPROACH HOLD", "bool", 0.0, true),
        ("AUTOPILOT NAV1 LOCK", "bool", 0.0, true),
        ("AUTOPILOT FLIGHT LEVEL CHANGE", "bool", 0.0, true),
        ("AUTOPILOT PITCH HOLD REF", "degrees", 0.0, true),
        ("AUTOPILOT BANK HOLD REF", "degrees", 0.0, true),
        ("AUTOPILOT HEADING LOCK DIR", "degrees", 0.0, true),
        ("AUTOPILOT ALTITUDE LOCK VAR", "feet", 0.0, true),
        ("AUTOPILOT VERTICAL HOLD VAR", "feet per minute", 0.0, true),
        ("AUTOPILOT AIRSPEED HOLD VAR", "knots", 0.0, true),
        ("AUTOPILOT MANAGED SPEED IN MACH", "bool", 0.0, true),
        ("AUTOPILOT MACH HOLD VAR", "mach", 0.0, true),
        ("NUMBER OF ENGINES", "number", 2.0, false),
        ("ENGINE TYPE", "enum", 1.0, false),
        ("GENERAL ENG COMBUSTION:1", "bool", 1.0, true),
        ("GENERAL ENG COMBUSTION:2", "bool", 1.0, true),
        ("GENERAL ENG RPM:1", "rpm", 0.0, true),
        ("GENERAL ENG RPM:2", "rpm", 0.0, true),
        ("ENG N1 RPM:1", "percent", 0.0, true),
        ("ENG N1 RPM:2", "percent", 0.0, true),
        ("ENG N2 RPM:1", "percent", 0.0, true),
        ("ENG N2 RPM:2", "percent", 0.0, true),
        ("ENG FUEL FLOW PPH:1", "pounds per hour", 0.0, true),
        ("ENG FUEL FLOW PPH:2", "pounds per hour", 0.0, true),
        ("ENG OIL PRESSURE:1", "psi", 0.0, true),
        ("ENG OIL TEMPERATURE:1", "celsius", 0.0, true),
        ("TURB ENG N1:1", "percent", 0.0, true),
        ("TURB ENG N1:2", "percent", 0.0, true),
        ("FUEL TOTAL QUANTITY", "gallons", 0.0, true),
        ("FUEL TOTAL QUANTITY WEIGHT", "pounds", 0.0, true),
        ("FUEL LEFT QUANTITY", "gallons", 0.0, true),
        ("FUEL RIGHT QUANTITY", "gallons", 0.0, true),
        ("TOTAL WEIGHT", "pounds", 0.0, true),
        ("EMPTY WEIGHT", "pounds", 0.0, true),
        ("ELECTRICAL MASTER BATTERY", "bool", 1.0, true),
        ("ELECTRICAL BATTERY VOLTAGE", "volts", 24.0, true),
        ("ELECTRICAL TOTAL LOAD AMPS", "amps", 0.0, true),
        ("AVIONICS MASTER SWITCH", "bool", 1.0, true),
        ("CIRCUIT GENERAL PANEL ON", "bool", 1.0, true),
        ("CIRCUIT AVIONICS ON", "bool", 1.0, true),
        ("LIGHT PANEL", "bool", 0.0, true),
        ("LIGHT LANDING", "bool", 0.0, true),
        ("LIGHT BEACON", "bool", 0.0, true),
        ("LIGHT STROBE", "bool", 0.0, true),
        ("LIGHT TAXI", "bool", 0.0, true),
        ("LIGHT NAV", "bool", 0.0, true),
        ("TRANSPONDER STATE", "enum", 0.0, true),
        ("TRANSPONDER CODE:1", "number", 1200.0, true),
        ("COM ACTIVE FREQUENCY:1", "megahertz", 121.5, true),
        ("COM STANDBY FREQUENCY:1", "megahertz", 121.5, true),
        ("NAV ACTIVE FREQUENCY:1", "megahertz", 110.5, true),
        ("NAV STANDBY FREQUENCY:1", "megahertz", 110.5, true),
        ("ADF ACTIVE FREQUENCY:1", "kilohertz", 400.0, true),
        ("GPS POSITION LAT", "degrees", 47.4647, false),
        ("GPS POSITION LON", "degrees", -122.3088, false),
        ("GPS POSITION ALT", "meters", 0.0, false),
        ("GPS GROUND SPEED", "knots", 0.0, false),
        ("GPS GROUND TRUE TRACK", "degrees", 0.0, false),
        ("GPS WP NEXT ID", "string", 0.0, false),
        ("GPS WP DISTANCE", "nautical miles", 0.0, false),
        ("GPS WP BEARING", "degrees", 0.0, false),
        ("GPS DRIVES NAV1", "bool", 0.0, true),
        ("NAV CDI:1", "number", 0.0, true),
        ("NAV GS FLAG:1", "bool", 1.0, true),
        ("NAV TOFROM:1", "enum", 0.0, true),
        ("NAV OBS:1", "degrees", 0.0, true),
        ("HSI CDI NEEDLE", "number", 0.0, true),
        ("HSI GSI NEEDLE", "number", 0.0, true),
        ("HSI CDI NEEDLE VALID", "bool", 0.0, true),
        ("HSI GSI NEEDLE VALID", "bool", 0.0, true),
        ("PLANE LATITUDE", "degrees", 47.4647, true),
        ("ABSOLUTE TIME", "seconds", 0.0, false),
        ("E:ABSOLUTE TIME", "seconds", 0.0, false),
        ("LOCAL TIME", "seconds", 0.0, false),
        ("ZULU TIME", "seconds", 0.0, false),
        ("SIMULATION RATE", "number", 1.0, true),
        ("PAUSED", "bool", 0.0, true),
        ("TITLE", "string", 0.0, false),
        ("ATC MODEL", "string", 0.0, false),
        ("ATC TYPE", "string", 0.0, false),
        ("CATEGORY", "string", 0.0, false),
        ("WISPS", "number", 0.0, false),
        ("BAROMETER PRESSURE", "millibars", 1013.25, true),
        ("KOHLSMAN SETTING HG", "inches of mercury", 29.92, true),
        ("INDICATED ALTITUDE", "feet", 0.0, false),
        ("PLANE TOUCHDOWN NORMAL VELOCITY", "feet per second", 0.0, false),
        ("STALL WARNING", "bool", 0.0, false),
        ("OVERSPEED WARNING", "bool", 0.0, false),
        ("GEAR WARNING", "bool", 0.0, false),
        ("ANTISKID BRAKES ACTIVE", "bool", 0.0, false),
        ("SPOILERS HANDLE POSITION", "percent", 0.0, true),
        ("SPOILERS ARMED", "bool", 0.0, true),
        ("FLAPS NUM HANDLE POSITIONS", "number", 4.0, false),
        ("SPOILER POSITION", "percent", 0.0, true),
        ("RUDDER DEFLECTION", "percent", 0.0, false),
        ("AILERON POSITION", "percent", 0.0, false),
        ("ELEVATOR POSITION", "percent", 0.0, false),
        ("EXIT OPEN:0", "percent", 0.0, true),
        ("PUSHBACK ATTACHED", "bool", 0.0, true),
        ("TOW CONNECTION", "bool", 0.0, true),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn simvar_roundtrip_by_id() {
        let mut s = SimState::new();
        let id = s.simvar_id("PLANE ALTITUDE");
        assert!(s.set(id, 35000.0));
        assert_eq!(s.get(id), Some(35000.0));
        assert_eq!(s.get_named("plane altitude"), 35000.0);
    }

    #[test]
    fn lvars_are_created_on_demand_and_shared() {
        let mut s = SimState::new();
        let a = s.register_lvar("XMLVAR_TEST");
        let b = s.lvar_id("xmlvar_test");
        assert_eq!(a, b);
        assert!(s.lvar_set(a, 42.0));
        assert_eq!(s.lvar_by_name("XMLVAR_TEST"), Some(42.0));
    }

    #[test]
    fn unknown_simvars_are_reported_as_zero_not_fabricated() {
        let mut s = SimState::new();
        let id = s.simvar_id("SOME UNDOCUMENTED VAR");
        assert_eq!(s.get(id), Some(0.0));
        assert_eq!(s.simvar_name(id), "SOME UNDOCUMENTED VAR");
    }

    #[test]
    fn units_are_deduplicated() {
        let mut s = SimState::new();
        let a = s.unit_id("DEGREES");
        let b = s.unit_id("degrees");
        assert_eq!(a, b);
        assert_eq!(s.unit_name(a), "DEGREES");
    }
}
