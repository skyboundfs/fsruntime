use anyhow::{anyhow, Context, Result};
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Target {
    Msfs2020,
    Msfs2024,
    Both,
    Unknown,
}

impl Target {
    pub fn label(self) -> &'static str {
        match self {
            Target::Msfs2020 => "MSFS 2020",
            Target::Msfs2024 => "MSFS 2024",
            Target::Both => "2020+2024",
            Target::Unknown => "unresolved",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Confidence {
    None,
    Low,
    Medium,
    High,
}

impl Confidence {
    pub fn label(self) -> &'static str {
        match self {
            Confidence::None => "none",
            Confidence::Low => "low",
            Confidence::Medium => "medium",
            Confidence::High => "high",
        }
    }
}

#[derive(Clone, Debug)]
pub struct Classification {
    pub target: Target,
    pub confidence: Confidence,
    pub evidence: Vec<String>,
}

impl Classification {
    fn empty() -> Self {
        Classification { target: Target::Unknown, confidence: Confidence::None, evidence: Vec::new() }
    }

    fn push(&mut self, target: Target, confidence: Confidence, evidence: String) {
        self.evidence.push(evidence);
        if confidence > self.confidence {
            self.confidence = confidence;
        }
        self.target = match (self.target, target) {
            (Target::Unknown, t) => t,
            (a, b) if a == b => a,
            _ => Target::Both,
        };
    }
}

#[derive(Clone, Debug, Default)]
pub struct Manifest {
    pub title: String,
    pub creator: String,
    pub content_type: String,
    pub package_version: String,
    pub minimum_game_version: String,
    pub minimum_compatibility_version: String,
    pub builder: String,
    pub export_type: String,
    pub package_order_hint: String,
    pub ingested: bool,
    pub dependencies: Vec<String>,
}

impl Manifest {
    fn from_json(value: &Value) -> Self {
        let s = |key: &str| value.get(key).and_then(|v| v.as_str()).unwrap_or("").to_string();
        let dependencies = value
            .get("dependencies")
            .and_then(|v| v.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|d| d.get("name").and_then(|n| n.as_str()).map(str::to_string))
                    .collect()
            })
            .unwrap_or_default();
        Manifest {
            title: s("title"),
            creator: s("creator"),
            content_type: s("content_type"),
            package_version: s("package_version"),
            minimum_game_version: s("minimum_game_version"),
            minimum_compatibility_version: s("minimum_compatibility_version"),
            builder: s("builder"),
            export_type: s("export_type"),
            package_order_hint: s("package_order_hint"),
            ingested: value.get("ingested").and_then(|v| v.as_bool()).unwrap_or(false),
            dependencies,
        }
    }

    pub fn classify(&self) -> Classification {
        let mut out = Classification::empty();
        let builder = self.builder.to_ascii_lowercase();
        if builder.contains("2024") {
            out.push(Target::Msfs2024, Confidence::High, format!("manifest.builder = {:?}", self.builder));
        } else if builder.contains("2020") {
            out.push(Target::Msfs2020, Confidence::High, format!("manifest.builder = {:?}", self.builder));
        }
        if !self.minimum_compatibility_version.is_empty() {
            let major = self
                .minimum_compatibility_version
                .split('.')
                .next()
                .and_then(|v| v.parse::<u32>().ok())
                .unwrap_or(0);
            if major >= 7 {
                out.push(
                    Target::Msfs2024,
                    Confidence::Medium,
                    format!("minimum_compatibility_version = {}", self.minimum_compatibility_version),
                );
            }
        }
        if self.builder.is_empty() && !self.minimum_game_version.is_empty() {
            out.push(
                Target::Msfs2020,
                Confidence::Low,
                "manifest has no builder field (2024 SDK always writes one)".to_string(),
            );
        }
        if self.ingested {
            out.push(Target::Msfs2024, Confidence::Low, "manifest.ingested = true".to_string());
        }
        if !self.package_order_hint.is_empty() {
            out.push(
                Target::Msfs2024,
                Confidence::Low,
                format!("package_order_hint = {}", self.package_order_hint),
            );
        }
        out
    }
}

#[derive(Clone, Debug, Default)]
pub struct Variant {
    pub title: String,
    pub ui_variation: String,
    pub ui_type: String,
    pub ui_manufacturer: String,
    pub atc_id: String,
    pub source: PathBuf,
    pub panel_dir: Option<PathBuf>,
    pub gauges: Vec<Gauge>,
    pub modules: Vec<PathBuf>,
}

impl Variant {
    pub fn label(&self) -> String {
        if self.ui_variation.is_empty() {
            self.title.clone()
        } else {
            format!("{} ({})", self.title, self.ui_variation)
        }
    }
}

#[derive(Clone, Debug)]
pub enum GaugeKind {
    Wasm { module: String, gauge: String, params: String, module_path: Option<PathBuf> },
    Html { page: String, query: String, page_path: Option<PathBuf> },
}

#[derive(Clone, Debug)]
pub struct Gauge {
    pub section: String,
    pub kind: GaugeKind,
    pub width: u32,
    pub height: u32,
    pub panel: PathBuf,
}

impl Gauge {
    pub fn title(&self) -> String {
        match &self.kind {
            GaugeKind::Wasm { gauge, params, .. } => {
                if params.is_empty() {
                    format!("{gauge} [wasm]")
                } else {
                    format!("{gauge} [{params}]")
                }
            }
            GaugeKind::Html { page, query, .. } => {
                let name = page.rsplit('/').next().unwrap_or(page);
                if query.is_empty() {
                    name.to_string()
                } else {
                    format!("{name}?{query}")
                }
            }
        }
    }
}

#[derive(Clone, Debug)]
pub struct Aircraft {
    pub key: String,
    pub dir: PathBuf,
    pub variants: Vec<Variant>,
    pub wasm_modules: Vec<PathBuf>,
    pub issues: Vec<String>,
}

impl Aircraft {
    pub fn gauges(&self) -> impl Iterator<Item = &Gauge> {
        self.variants.iter().flat_map(|v| v.gauges.iter())
    }
}

#[derive(Clone, Debug)]
pub struct Package {
    pub dir: PathBuf,
    pub name: String,
    pub manifest: Manifest,
    pub classification: Classification,
    pub aircraft: Vec<Aircraft>,
}

impl Package {
    pub fn scan(path: &Path) -> Result<Self> {
        let dir = if path.is_dir() { path.to_path_buf() } else { sibling_ci(path)? };
        if !dir.join("manifest.json").is_file() {
            return Err(anyhow!("{} has no manifest.json", dir.display()));
        }
        Self::load(&dir)
    }

    pub fn load(dir: &Path) -> Result<Self> {
        let manifest_path = dir.join("manifest.json");
        let text = std::fs::read_to_string(&manifest_path)
            .with_context(|| format!("read {}", manifest_path.display()))?;
        let value: Value = serde_json::from_str(&text)
            .with_context(|| format!("parse {}", manifest_path.display()))?;
        let manifest = Manifest::from_json(&value);
        let classification = manifest.classify();
        let name = dir.file_name().and_then(|n| n.to_str()).unwrap_or("package").to_string();
        let mut aircraft = Vec::new();
        let mut issues = Vec::new();
        for plane in find_aircraft_dirs(dir) {
            match Aircraft::load(dir, &plane) {
                Ok(a) => aircraft.push(a),
                Err(e) => issues.push(format!("{}: {e}", plane.display())),
            }
        }
        Ok(Package { dir: dir.to_path_buf(), name, manifest, classification, aircraft })
    }
}

fn sibling_ci(path: &Path) -> Result<PathBuf> {
    let (Some(parent), Some(name)) = (path.parent(), path.file_name().and_then(|n| n.to_str())) else {
        return Err(anyhow!("{} is not a directory", path.display()));
    };
    let hit = std::fs::read_dir(parent)
        .ok()
        .and_then(|rd| {
            rd.filter_map(|e| e.ok())
                .map(|e| e.path())
                .find(|p| p.is_dir() && p.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.eq_ignore_ascii_case(name)))
        });
    hit.ok_or_else(|| anyhow!("{} is not a directory", path.display()))
}

fn find_aircraft_dirs(package: &Path) -> Vec<PathBuf> {
    let simobjects = find_dir_ci(package, "simobjects");
    let Some(simobjects) = simobjects else { return Vec::new() };
    let Some(airplanes) = find_dir_ci(&simobjects, "airplanes") else { return Vec::new() };
    let mut out = Vec::new();
    if let Ok(rd) = std::fs::read_dir(&airplanes) {
        for entry in rd.filter_map(|e| e.ok()) {
            if entry.path().is_dir() {
                out.push(entry.path());
            }
        }
    }
    out.sort();
    out
}

impl Aircraft {
    fn load(package: &Path, dir: &Path) -> Result<Self> {
        let key = dir.file_name().and_then(|n| n.to_str()).unwrap_or("aircraft").to_string();
        let mut variants = Vec::new();
        let mut issues = Vec::new();

        let preset_root = find_dir_ci(dir, "presets");
        if let Some(preset_root) = &preset_root {
            for cfg in collect_configs(preset_root, "aircraft.cfg") {
                let variant_dir = cfg.parent().and_then(Path::parent).map(Path::to_path_buf);
                match parse_variants(&cfg) {
                    Ok(parsed) => {
                        for mut variant in parsed {
                            if variant.panel_dir.is_none() {
                                variant.panel_dir = resolve_panel_dir(dir, variant_dir.as_deref());
                            }
                            variant.gauges = load_gauges(package, &variant, dir, &mut issues);
                            variant.modules = module_list(variant.panel_dir.as_deref());
                            variants.push(variant);
                        }
                    }
                    Err(e) => issues.push(format!("{}: {e}", cfg.display())),
                }
            }
        }

        if variants.is_empty() {
            for cfg in collect_configs(dir, "aircraft.cfg") {
                let variant_dir = cfg.parent().and_then(Path::parent).map(Path::to_path_buf);
                match parse_variants(&cfg) {
                    Ok(parsed) => {
                        for mut variant in parsed {
                            if variant.panel_dir.is_none() {
                                variant.panel_dir = resolve_panel_dir(dir, variant_dir.as_deref());
                            }
                            variant.gauges = load_gauges(package, &variant, dir, &mut issues);
                            variant.modules = module_list(variant.panel_dir.as_deref());
                            variants.push(variant);
                        }
                    }
                    Err(e) => issues.push(format!("{}: {e}", cfg.display())),
                }
            }
        }

        if variants.is_empty() {
            let fallback = resolve_panel_dir(dir, None);
            let mut variant = Variant {
                title: key.clone(),
                panel_dir: fallback.clone(),
                ..Default::default()
            };
            variant.gauges = load_gauges(package, &variant, dir, &mut issues);
            variant.modules = module_list(variant.panel_dir.as_deref());
            variants.push(variant);
        }

        variants.sort_by(|a, b| a.title.cmp(&b.title));
        variants.dedup_by(|a, b| a.title == b.title);

        let mut wasm_modules = Vec::new();
        for entry in walkdir::WalkDir::new(dir).follow_links(false) {
            let Ok(entry) = entry else { continue };
            if !entry.file_type().is_file() {
                continue;
            }
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()).map(|e| e.eq_ignore_ascii_case("wasm")).unwrap_or(false) {
                wasm_modules.push(path.to_path_buf());
            }
        }
        wasm_modules.sort();

        Ok(Aircraft { key, dir: dir.to_path_buf(), variants, wasm_modules, issues })
    }
}

fn resolve_panel_dir(aircraft_dir: &Path, variant_dir: Option<&Path>) -> Option<PathBuf> {
    if let Some(variant_dir) = variant_dir {
        let candidate = variant_dir.join("panel");
        if candidate.join("panel.cfg").is_file() {
            return Some(candidate);
        }
    }
    let common = find_dir_ci(aircraft_dir, "common").map(|c| c.join("panel"));
    if let Some(common) = common {
        if common.join("panel.cfg").is_file() {
            return Some(common);
        }
    }
    None
}

fn load_gauges(package: &Path, variant: &Variant, aircraft_dir: &Path, issues: &mut Vec<String>) -> Vec<Gauge> {
    let mut gauges = Vec::new();
    let mut panels: Vec<PathBuf> = Vec::new();
    if let Some(panel_dir) = &variant.panel_dir {
        panels.push(panel_dir.join("panel.cfg"));
    }
    if panels.is_empty() {
        if let Some(attachments) = find_dir_ci(aircraft_dir, "attachments") {
            for entry in walkdir::WalkDir::new(attachments).follow_links(false) {
                let Ok(entry) = entry else { continue };
                if entry.file_type().is_file()
                    && entry.file_name().to_string_lossy().eq_ignore_ascii_case("panel.cfg")
                {
                    panels.push(entry.path().to_path_buf());
                }
            }
        }
    }
    panels.sort();
    for panel in panels {
        match parse_panel(package, &panel) {
            Ok(mut g) => gauges.append(&mut g),
            Err(e) => issues.push(format!("{}: {e}", panel.display())),
        }
    }
    gauges
}

fn module_list(panel_dir: Option<&Path>) -> Vec<PathBuf> {
    let Some(panel_dir) = panel_dir else { return Vec::new() };
    let mut out = Vec::new();
    if let Ok(rd) = std::fs::read_dir(panel_dir) {
        for entry in rd.filter_map(|e| e.ok()) {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()).map(|e| e.eq_ignore_ascii_case("wasm")).unwrap_or(false) {
                out.push(path);
            }
        }
    }
    out.sort();
    out
}

fn collect_configs(dir: &Path, file: &str) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for entry in walkdir::WalkDir::new(dir).follow_links(false).max_depth(6) {
        let Ok(entry) = entry else { continue };
        if !entry.file_type().is_file() {
            continue;
        }
        let name = entry.file_name().to_string_lossy();
        if name.eq_ignore_ascii_case(file) {
            out.push(entry.path().to_path_buf());
        }
    }
    out.sort();
    out
}

fn parse_variants(path: &Path) -> Result<Vec<Variant>> {
    let text = std::fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    let ini = Ini::parse(&text);
    let variant_dir = path.parent().unwrap_or(Path::new("."));
    let mut out = Vec::new();
    for section in ini.sections.keys() {
        if !section.to_ascii_uppercase().starts_with("FLTSIM.") {
            continue;
        }
        let get = |key: &str| ini.get(section, key).unwrap_or_default();
        let title = get("title");
        if title.is_empty() {
            continue;
        }
        let panel_key = get("panel");
        let panel_dir = if !panel_key.is_empty() {
            let cleaned = panel_key.trim_start_matches('\\').replace('\\', "/");
            let mut resolved = variant_dir.join(&cleaned);

            // Try canonicalizing the path
            let mut canonical = resolved.canonicalize().ok();

            // Workaround: some packages have an extra leading .. in the Panel path
            if canonical.is_none() && cleaned.starts_with("../") {
                let fallback = cleaned.strip_prefix("../").unwrap_or(&cleaned);
                resolved = variant_dir.join(fallback);
                canonical = resolved.canonicalize().ok();
            }

            canonical.filter(|p| p.join("panel.cfg").is_file())
        } else {
            None
        };
        out.push(Variant {
            title,
            ui_variation: get("ui_variation"),
            ui_type: get("ui_type"),
            ui_manufacturer: get("ui_manufacturer"),
            atc_id: get("atc_id"),
            source: path.to_path_buf(),
            panel_dir,
            ..Default::default()
        });
    }
    Ok(out)
}

fn parse_panel(package: &Path, path: &Path) -> Result<Vec<Gauge>> {
    let text = std::fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    let ini = Ini::parse(&text);
    let panel_dir = path.parent().unwrap_or(Path::new("."));
    let mut out = Vec::new();
    for (section, keys) in &ini.sections {
        if !section.to_ascii_uppercase().starts_with("VCOCKPIT") {
            continue;
        }
        let size = keys
            .get("size_mm")
            .map(|v| v.trim().to_string())
            .unwrap_or_default();
        for (key, value) in keys {
            if !key.to_ascii_lowercase().starts_with("htmlgauge") {
                continue;
            }
            let Some((spec, rect)) = value.split_once(',') else { continue };
            let spec = spec.trim();
            // "<page>, x, y, width, height[, params]" - the params are what
            // MSFS passes to the gauge as strParameters.
            let mut parts = rect.split(',').map(str::trim);
            let _x = parts.next();
            let _y = parts.next();
            let width = parts.next().and_then(|v| v.parse::<u32>().ok()).unwrap_or(0);
            let height = parts.next().and_then(|v| v.parse::<u32>().ok()).unwrap_or(0);
            let panel_params: String = parts.collect::<Vec<_>>().join(",");
            let (page, query) = match spec.split_once('?') {
                Some((p, q)) => (p, q),
                None => (spec, ""),
            };
            let kind = if page.eq_ignore_ascii_case("WasmInstrument/WasmInstrument.html") {
                let params = parse_query(query);
                let module = params.get("wasm_module").cloned().unwrap_or_default();
                let gauge = params.get("wasm_gauge").cloned().unwrap_or_default();
                let extra = params
                    .get("params")
                    .cloned()
                    .unwrap_or_else(|| panel_params.clone());
                let module_path = resolve_wasm(package, panel_dir, &module);
                GaugeKind::Wasm { module, gauge, params: extra, module_path }
            } else {
                let page_path = resolve_page(package, page);
                GaugeKind::Html { page: page.to_string(), query: query.to_string(), page_path }
            };
            let (w, h) = if width == 0 || height == 0 {
                parse_size_mm(&size).unwrap_or((width.max(1), height.max(1)))
            } else {
                (width, height)
            };
            out.push(Gauge {
                section: section.clone(),
                kind,
                width: w.max(1),
                height: h.max(1),
                panel: path.to_path_buf(),
            });
        }
    }
    Ok(out)
}

fn parse_size_mm(value: &str) -> Option<(u32, u32)> {
    let mut parts = value.split(',');
    let w = parts.next()?.trim().parse::<u32>().ok()?;
    let h = parts.next()?.trim().parse::<u32>().ok()?;
    Some((w.max(1), h.max(1)))
}

fn parse_query(query: &str) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    for pair in query.split('&') {
        if let Some((k, v)) = pair.split_once('=') {
            out.insert(k.to_ascii_lowercase(), v.to_string());
        }
    }
    out
}

fn resolve_wasm(package: &Path, panel_dir: &Path, module: &str) -> Option<PathBuf> {
    if module.is_empty() {
        return None;
    }
    let normalized = module.replace('\\', "/");
    let candidate = panel_dir.join(&normalized);
    if candidate.is_file() {
        return Some(candidate);
    }
    let candidate = package.join(&normalized);
    if candidate.is_file() {
        return Some(candidate);
    }
    let name = normalized.rsplit('/').next().unwrap_or(&normalized);
    for entry in walkdir::WalkDir::new(package).follow_links(false) {
        let Ok(entry) = entry else { continue };
        if !entry.file_type().is_file() {
            continue;
        }
        if entry.file_name().to_string_lossy().eq_ignore_ascii_case(name) {
            return Some(entry.path().to_path_buf());
        }
    }
    None
}

fn resolve_page(package: &Path, page: &str) -> Option<PathBuf> {
    let normalized = page.replace('\\', "/");
    let roots = [
        package.join("html_ui/Pages/VCockpit/Instruments"),
        package.join("html_ui"),
        package.to_path_buf(),
    ];
    for root in roots {
        let candidate = root.join(&normalized);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    let mut wanted: Vec<String> = normalized
        .to_ascii_lowercase()
        .split('/')
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect();
    if wanted.is_empty() {
        return None;
    }
    for entry in walkdir::WalkDir::new(package).follow_links(false) {
        let Ok(entry) = entry else { continue };
        if !entry.file_type().is_file() {
            continue;
        }
        let rel = entry.path().strip_prefix(package).ok()?.to_string_lossy().to_ascii_lowercase();
        let parts: Vec<&str> = rel.split('/').collect();
        if wanted.iter().all(|seg| parts.contains(&seg.as_str())) {
            return Some(entry.path().to_path_buf());
        }
    }
    wanted.clear();
    None
}

pub fn find_dir_ci(root: &Path, name: &str) -> Option<PathBuf> {
    let direct = root.join(name);
    if direct.is_dir() {
        return Some(direct);
    }
    std::fs::read_dir(root).ok()?.filter_map(|e| e.ok()).find_map(|e| {
        let p = e.path();
        if p.is_dir() && e.file_name().to_string_lossy().eq_ignore_ascii_case(name) {
            Some(p)
        } else {
            None
        }
    })
}

#[derive(Default)]
pub struct Ini {
    pub sections: BTreeMap<String, BTreeMap<String, String>>,
}

impl Ini {
    pub fn parse(text: &str) -> Self {
        let mut ini = Ini::default();
        let mut current = String::new();
        for raw in text.lines() {
            let line = strip_comment(raw).trim();
            if line.is_empty() {
                continue;
            }
            if line.starts_with('[') && line.ends_with(']') {
                current = line[1..line.len() - 1].trim().to_string();
                ini.sections.entry(current.clone()).or_default();
                continue;
            }
            if current.is_empty() {
                continue;
            }
            let Some((key, value)) = line.split_once('=') else { continue };
            ini.sections
                .entry(current.clone())
                .or_default()
                .insert(key.trim().to_ascii_lowercase(), unquote(value.trim()));
        }
        ini
    }

    pub fn get(&self, section: &str, key: &str) -> Option<String> {
        self.sections.get(section)?.get(&key.to_ascii_lowercase()).cloned()
    }
}

fn strip_comment(line: &str) -> &str {
    let mut in_quote = false;
    for (i, ch) in line.char_indices() {
        match ch {
            '"' => in_quote = !in_quote,
            ';' if !in_quote => return &line[..i],
            _ => {}
        }
    }
    line
}

fn unquote(value: &str) -> String {
    let v = value.trim();
    if v.len() >= 2 && v.starts_with('"') && v.ends_with('"') {
        v[1..v.len() - 1].to_string()
    } else {
        v.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ini_handles_quotes_and_comments() {
        let ini = Ini::parse("[FLTSIM.0]\ntitle = \"A; B\" ; comment\nui_variation = Default\n");
        assert_eq!(ini.get("FLTSIM.0", "title").as_deref(), Some("A; B"));
        assert_eq!(ini.get("FLTSIM.0", "ui_variation").as_deref(), Some("Default"));
    }

    #[test]
    fn manifest_builder_selects_target() {
        let mut m = Manifest::default();
        m.builder = "Microsoft Flight Simulator 2024".into();
        assert_eq!(m.classify().target, Target::Msfs2024);
        let mut m = Manifest::default();
        m.builder = "Microsoft Flight Simulator 2020".into();
        assert_eq!(m.classify().target, Target::Msfs2020);
        let m = Manifest::default();
        assert_eq!(m.classify().target, Target::Unknown);
    }

    #[test]
    fn conflicting_evidence_reports_both() {
        let mut m = Manifest::default();
        m.builder = "Microsoft Flight Simulator 2020".into();
        m.ingested = true;
        assert_eq!(m.classify().target, Target::Both);
    }
}
