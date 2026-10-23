use crate::runtime::{RuntimeRequest, RuntimeResponse};
use anyhow::{Context, Result};
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};

#[derive(Clone, Debug)]
pub struct AssetRoot {
    pub package: String,
    pub html_ui: PathBuf,
    pub panel_dir: PathBuf,
}

pub struct BridgeState {
    pub roots: HashMap<String, AssetRoot>,
    pub sessions: HashMap<u64, String>,
    pub requests: Option<Sender<RuntimeRequest>>,
}

impl Default for BridgeState {
    fn default() -> Self {
        Self { roots: HashMap::new(), sessions: HashMap::new(), requests: None }
    }
}

pub struct Bridge {
    pub port: u16,
    state: Arc<Mutex<BridgeState>>,
}

impl Bridge {
    pub fn start() -> Result<Self> {
        let listener = TcpListener::bind("127.0.0.1:0")?;
        let port = listener.local_addr()?.port();
        let state = Arc::new(Mutex::new(BridgeState::default()));
        let shared = state.clone();
        std::thread::Builder::new()
            .name("msfs-bridge".into())
            .spawn(move || {
                for stream in listener.incoming() {
                    let Ok(stream) = stream else { continue };
                    let state = shared.clone();
                    let _ = std::thread::Builder::new()
                        .name("msfs-bridge-conn".into())
                        .spawn(move || {
                            let _ = handle(stream, state);
                        });
                }
            })
            .context("spawn bridge listener")?;
        Ok(Bridge { port, state })
    }

    pub fn register(&self, id: u64, package: &str, html_ui: PathBuf, panel_dir: PathBuf) {
        let mut state = self.state.lock().unwrap();
        state.roots.insert(
            package.to_string(),
            AssetRoot { package: package.to_string(), html_ui, panel_dir },
        );
        state.sessions.insert(id, package.to_string());
    }

    pub fn base_url(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }

    pub fn set_requests(&self, requests: Sender<RuntimeRequest>) {
        self.state.lock().unwrap().requests = Some(requests);
    }
}

fn handle(mut stream: TcpStream, state: Arc<Mutex<BridgeState>>) -> Result<()> {
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut request_line = String::new();
    if reader.read_line(&mut request_line)? == 0 {
        return Ok(());
    }
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or("GET").to_string();
    let target = parts.next().unwrap_or("/").to_string();
    let mut content_length = 0usize;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line)? == 0 {
            break;
        }
        let trimmed = line.trim_end();
        if trimmed.is_empty() {
            break;
        }
        if let Some((k, v)) = trimmed.split_once(':') {
            if k.eq_ignore_ascii_case("content-length") {
                content_length = v.trim().parse().unwrap_or(0);
            }
        }
    }
    let mut body = vec![0u8; content_length];
    if content_length > 0 {
        use std::io::Read;
        reader.read_exact(&mut body)?;
    }

    let (path, query) = match target.split_once('?') {
        Some((p, q)) => (p.to_string(), q.to_string()),
        None => (target.clone(), String::new()),
    };
    let params = parse_query(&query);
    let Some(requests) = state.lock().unwrap().requests.clone() else {
        return write_body(&mut stream, "text/plain", b"runtime not attached");
    };

    if path == "/api" {
        let response = api(&params, &state, &requests, &body);
        return write_json(&mut stream, &response);
    }
    if let Some(rest) = path.strip_prefix("/surface/") {
        let id: u64 = rest.trim_end_matches(".png").parse().unwrap_or(0);
        let (tx, rx) = std::sync::mpsc::channel();
        let request = RuntimeRequest::WithReply(Box::new(RuntimeRequest::SurfacePng { session: id }), tx);
        if requests.send(request).is_ok() {
            if let Ok(RuntimeResponse::Binary(bytes)) = rx.recv_timeout(std::time::Duration::from_secs(3)) {
                return write_body(&mut stream, "image/png", &bytes);
            }
        }
        return write_body(&mut stream, "image/png", &[]);
    }
    if path == "/wasm-instrument.html" {
        let session = params.get("session").cloned().unwrap_or_default();
        let page = include_str!("wasm_instrument.html").replace("__SESSION__", &session);
        return write_body(&mut stream, "text/html", page.as_bytes());
    }
    if path == "/inject.js" {
        let js = include_str!("inject.js");
        return write_body(&mut stream, "application/javascript", js.as_bytes());
    }
    if let Some(rest) = path.strip_prefix("/instrument/") {
        let (id, inner) = match rest.split_once('/') {
            Some(v) => v,
            None => return write_body(&mut stream, "text/plain", b"bad instrument path"),
        };
        let id: u64 = id.parse().unwrap_or(0);
        let package = state.lock().unwrap().sessions.get(&id).cloned();
        let Some(package) = package else {
            return write_body(&mut stream, "text/plain", b"unknown instrument session");
        };
        let root = state.lock().unwrap().roots.get(&package).cloned();
        let Some(root) = root else {
            return write_body(&mut stream, "text/plain", b"unknown package");
        };
        let decoded = percent_decode(inner);
        let candidate = root.html_ui.join(&decoded);
        let path = if candidate.is_file() {
            candidate
        } else {
            let fallback = root.panel_dir.join(decoded.rsplit('/').next().unwrap_or(&decoded));
            if fallback.is_file() {
                fallback
            } else {
                return write_body(&mut stream, "text/plain", b"asset not found");
            }
        };
        return serve_file(&mut stream, &path, &method);
    }

    write_body(&mut stream, "text/plain", b"not found")
}

fn serve_file(stream: &mut TcpStream, path: &Path, _method: &str) -> Result<()> {
    let data = std::fs::read(path)?;
    let mime = mime_for(path);
    write_body(stream, mime, &data)
}

fn mime_for(path: &Path) -> &'static str {
    match path.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase().as_str() {
        "html" | "htm" => "text/html",
        "js" | "mjs" => "application/javascript",
        "css" => "text/css",
        "json" => "application/json",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "svg" => "image/svg+xml",
        "woff" => "font/woff",
        "woff2" => "font/woff2",
        "ttf" => "font/ttf",
        "otf" => "font/otf",
        "wasm" => "application/wasm",
        _ => "application/octet-stream",
    }
}

fn write_body(stream: &mut TcpStream, mime: &str, body: &[u8]) -> Result<()> {
    let header = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: {mime}\r\nContent-Length: {}\r\nAccess-Control-Allow-Origin: *\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n",
        body.len()
    );
    stream.write_all(header.as_bytes())?;
    stream.write_all(body)?;
    stream.flush()?;
    Ok(())
}

fn write_json(stream: &mut TcpStream, value: &str) -> Result<()> {
    write_body(stream, "application/json", value.as_bytes())
}

fn parse_query(query: &str) -> HashMap<String, String> {
    let mut out = HashMap::new();
    for pair in query.split('&') {
        if pair.is_empty() {
            continue;
        }
        let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
        out.insert(k.to_ascii_lowercase(), percent_decode(v));
    }
    out
}

fn percent_decode(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' if i + 2 < bytes.len() => {
                let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or("");
                match u8::from_str_radix(hex, 16) {
                    Ok(b) => {
                        out.push(b);
                        i += 3;
                    }
                    Err(_) => {
                        out.push(bytes[i]);
                        i += 1;
                    }
                }
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).to_string()
}

fn api(
    params: &HashMap<String, String>,
    state: &Arc<Mutex<BridgeState>>,
    requests: &Sender<RuntimeRequest>,
    body: &[u8],
) -> String {
    let get = |k: &str| params.get(k).cloned().unwrap_or_default();
    let op = get("op");
    let session: u64 = get("session").parse().unwrap_or(0);
    let _ = state;
    let body_text = String::from_utf8_lossy(body).to_string();
    let request = match op.as_str() {
        "simvar" => RuntimeRequest::SimVar { session, name: get("name"), unit: get("unit") },
        "simvar_set" => RuntimeRequest::SimVarSet {
            session,
            name: get("name"),
            value: get("value").parse().unwrap_or(0.0),
        },
        "lvar" => RuntimeRequest::LVar { session, name: get("name") },
        "lvar_set" => RuntimeRequest::LVarSet {
            session,
            name: get("name"),
            value: get("value").parse().unwrap_or(0.0),
        },
        "gamevar" => RuntimeRequest::LVar { session, name: get("name") },
        "event" => RuntimeRequest::Event { session, name: get("name") },
        "commbus_call" => RuntimeRequest::CommBusCall {
            session,
            name: get("name"),
            payload: if body_text.is_empty() { get("payload") } else { body_text },
            broadcast: get("broadcast").parse().unwrap_or(0),
        },
        "commbus_poll" => RuntimeRequest::CommBusPoll { session, since: get("since").parse().unwrap_or(0) },
        "log" => RuntimeRequest::Log { session, message: get("message") },
        "context" => RuntimeRequest::Context { session },
        "mouse" => RuntimeRequest::Mouse {
            session,
            x: get("x").parse().unwrap_or(0.0),
            y: get("y").parse().unwrap_or(0.0),
            flags: get("flags").parse().unwrap_or(0),
        },
        _ => RuntimeRequest::Unknown { op },
    };
    let (tx, rx) = std::sync::mpsc::channel();
    if requests.send(RuntimeRequest::WithReply(Box::new(request), tx)).is_err() {
        return "{\"error\":\"runtime unavailable\"}".to_string();
    }
    match rx.recv_timeout(std::time::Duration::from_secs(5)) {
        Ok(RuntimeResponse::Value(v)) => format!("{{\"value\":{v}}}"),
        Ok(RuntimeResponse::Text(t)) => serde_json::json!({ "value": t }).to_string(),
        Ok(RuntimeResponse::List(items)) => serde_json::json!({ "items": items }).to_string(),
        Ok(RuntimeResponse::Binary(_)) => "{\"error\":\"binary response on json endpoint\"}".to_string(),
        Ok(RuntimeResponse::Error(e)) => serde_json::json!({ "error": e }).to_string(),
        Err(_) => "{\"error\":\"timeout\"}".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percent_decode_handles_escapes() {
        assert_eq!(percent_decode("a%20b%2Fc"), "a b/c");
        assert_eq!(percent_decode("plain"), "plain");
    }

    #[test]
    fn query_parsing_is_case_insensitive_on_keys() {
        let q = parse_query("OP=simvar&Name=PLANE+ALTITUDE");
        assert_eq!(q.get("op").unwrap(), "simvar");
        assert_eq!(q.get("name").unwrap(), "PLANE ALTITUDE");
    }

    #[test]
    fn mime_types_cover_instrument_assets() {
        assert_eq!(mime_for(Path::new("a.html")), "text/html");
        assert_eq!(mime_for(Path::new("a.JS")), "application/javascript");
        assert_eq!(mime_for(Path::new("f.woff2")), "font/woff2");
    }
}
