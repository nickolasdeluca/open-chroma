//! HTTP endpoints: the Chroma SDK REST API (localhost:54235) and OpenChroma's
//! own control API and web UI (localhost:54240).

use std::io::Read;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use chroma_proto::{AppInfo, Category, SdkEffect};
use serde_json::{json, Value};
use tiny_http::{Header, Method, Request, Response, Server};

use crate::config::{sdk_port, Config, UI_PORT};
use crate::engine::{lock, Shared};
use crate::sdk::Client;

const MAX_BODY: u64 = 1 << 20;
const UI_HTML: &str = include_str!("../assets/index.html");

/// Whether the REST API currently owns its port (Razer's service may hold it).
pub static SDK_PORT_BOUND: AtomicBool = AtomicBool::new(false);

fn header(name: &str, value: &str) -> Header {
    Header::from_bytes(name.as_bytes(), value.as_bytes()).expect("valid header")
}

fn reply(req: Request, status: u16, body: Value) {
    let resp = Response::from_string(body.to_string())
        .with_status_code(status)
        .with_header(header("Content-Type", "application/json"))
        .with_header(header("Access-Control-Allow-Origin", "*"))
        .with_header(header("Access-Control-Allow-Methods", "GET, POST, PUT, DELETE, OPTIONS"))
        .with_header(header("Access-Control-Allow-Headers", "Content-Type"));
    let _ = req.respond(resp);
}

fn body(req: &mut Request) -> Value {
    let mut s = String::new();
    let _ = req.as_reader().take(MAX_BODY).read_to_string(&mut s);
    serde_json::from_str(&s).unwrap_or(Value::Null)
}

/// Bind with retries: on a machine with Synapse installed the SDK port is
/// held by Razer's service until it is stopped.
fn serve_forever(addr: String, on_bound: impl Fn(bool) + Send + 'static, handle: impl Fn(Request) + Send + Sync + 'static) {
    thread::Builder::new()
        .name(format!("http {addr}"))
        .spawn(move || {
            let mut warned = false;
            loop {
                match Server::http(&addr) {
                    Ok(server) => {
                        log::info!("listening on http://{addr}");
                        on_bound(true);
                        for req in server.incoming_requests() {
                            handle(req);
                        }
                        on_bound(false);
                    }
                    Err(e) => {
                        if !warned {
                            log::warn!("cannot listen on {addr} ({e}); retrying. Is Razer's Chroma SDK service still running?");
                            warned = true;
                        }
                        thread::sleep(Duration::from_secs(5));
                    }
                }
            }
        })
        .expect("spawn http thread");
}

pub fn start(shared: Arc<Shared>) {
    // Clients connect to "localhost", which may resolve to either stack.
    let port = sdk_port();
    for addr in [format!("127.0.0.1:{port}"), format!("[::1]:{port}")] {
        let ipv4 = addr.starts_with("127");
        let shared = shared.clone();
        serve_forever(
            addr,
            move |bound| {
                if ipv4 {
                    SDK_PORT_BOUND.store(bound, Ordering::SeqCst)
                }
            },
            move |req| handle_sdk(req, &shared),
        );
    }
    serve_forever(format!("127.0.0.1:{UI_PORT}"), |_| {}, move |req| handle_ui(req, &shared));
}

// ---------------------------------------------------------------------------
// Chroma SDK REST API
// ---------------------------------------------------------------------------

const RESULT_OK: i64 = 0;
const RESULT_NOT_FOUND: i64 = 1168;
const RESULT_INVALID_PARAMETER: i64 = 87;
const RESULT_NOT_SUPPORTED: i64 = 50;

fn handle_sdk(mut req: Request, shared: &Shared) {
    let method = req.method().clone();
    let url = req.url().split('?').next().unwrap_or("").trim_end_matches('/').to_string();
    if method == Method::Options {
        return reply(req, 200, json!({}));
    }
    let parts: Vec<&str> = url.split('/').filter(|s| !s.is_empty()).collect();

    match (method, parts.as_slice()) {
        (Method::Get, ["razer", "chromasdk"]) => reply(req, 200, json!({"core": "3.38.04", "device": "3.38.04", "version": "3.38.04"})),
        (Method::Post, ["razer", "chromasdk"]) => {
            let b = body(&mut req);
            let app = AppInfo {
                title: b["title"].as_str().unwrap_or("Web application").to_string(),
                description: b["description"].as_str().unwrap_or_default().to_string(),
                author: b["author"]["name"].as_str().unwrap_or_default().to_string(),
                contact: b["author"]["contact"].as_str().unwrap_or_default().to_string(),
                category: match b["category"].as_str() {
                    Some("game") => 2,
                    _ => 1,
                },
            };
            let id = shared.open_session(app, Client::Rest);
            reply(req, 200, json!({"sessionid": id, "uri": format!("http://localhost:{}/sid/{id}/chromasdk", sdk_port())}))
        }
        (method, ["sid", id, "chromasdk", rest @ ..]) => {
            let Ok(id) = id.parse::<u64>() else { return reply(req, 404, json!({"result": RESULT_NOT_FOUND})) };
            let b = body(&mut req);
            let (status, out) = session_call(shared, id, &method, rest, &b);
            reply(req, status, out)
        }
        _ => reply(req, 404, json!({"result": RESULT_NOT_FOUND})),
    }
}

fn session_call(shared: &Shared, id: u64, method: &Method, path: &[&str], b: &Value) -> (u16, Value) {
    let mut sessions = lock(&shared.sessions);
    if path.is_empty() && *method == Method::Delete {
        return match sessions.close(id) {
            true => (200, json!({"result": RESULT_OK})),
            false => (404, json!({"result": RESULT_NOT_FOUND})),
        };
    }
    let Some(session) = sessions.get_mut(id) else { return (404, json!({"result": RESULT_NOT_FOUND})) };

    match (method, path) {
        (Method::Put, ["heartbeat"]) => {
            session.heartbeats += 1;
            (200, json!({"tick": session.heartbeats}))
        }
        (Method::Put, ["effect"]) | (Method::Delete, ["effect"]) => {
            let apply = *method == Method::Put;
            let mut one = |eid: &str| -> i64 {
                if apply {
                    match session.stored.get(eid).cloned() {
                        Some((cat, effect)) => {
                            session.shown.insert(cat, (effect, std::time::Instant::now()));
                            RESULT_OK
                        }
                        None => RESULT_NOT_FOUND,
                    }
                } else if session.stored.remove(eid).is_some() {
                    RESULT_OK
                } else {
                    RESULT_NOT_FOUND
                }
            };
            if let Some(eid) = b["id"].as_str() {
                (200, json!({"result": one(eid)}))
            } else if let Some(ids) = b["ids"].as_array() {
                let results: Vec<Value> = ids.iter().filter_map(Value::as_str).map(|eid| json!({"id": eid, "result": one(eid)})).collect();
                (200, json!({"results": results}))
            } else {
                (400, json!({"result": RESULT_INVALID_PARAMETER}))
            }
        }
        (Method::Put | Method::Post, [device]) => {
            let Some(category) = Category::from_rest_name(device) else { return (404, json!({"result": RESULT_NOT_FOUND})) };
            let effect = match parse_rest_effect(category, b) {
                Ok(e) => e,
                Err(code) => return (400, json!({"result": code})),
            };
            if *method == Method::Put {
                session.shown.insert(category, (effect, std::time::Instant::now()));
                (200, json!({"result": RESULT_OK}))
            } else {
                let eid = new_effect_id(id, session.stored.len());
                session.stored.insert(eid.clone(), (category, effect));
                (200, json!({"id": eid, "result": RESULT_OK}))
            }
        }
        _ => (404, json!({"result": RESULT_NOT_FOUND})),
    }
}

fn new_effect_id(session: u64, n: usize) -> String {
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
    format!("{:08x}-{:04x}-4000-8000-{:012x}", session as u32, n as u16, nanos as u64 & 0xFFFF_FFFF_FFFF)
}

fn flatten(v: &Value) -> Vec<u32> {
    match v {
        Value::Array(items) => items.iter().flat_map(flatten).collect(),
        Value::Number(n) => vec![n.as_u64().unwrap_or(0) as u32],
        _ => vec![],
    }
}

pub fn parse_rest_effect(category: Category, b: &Value) -> Result<SdkEffect, i64> {
    let param = &b["param"];
    match b["effect"].as_str().ok_or(RESULT_INVALID_PARAMETER)? {
        "CHROMA_NONE" => Ok(SdkEffect::None),
        "CHROMA_STATIC" => Ok(SdkEffect::Static { color: param["color"].as_u64().ok_or(RESULT_INVALID_PARAMETER)? as u32 }),
        "CHROMA_CUSTOM" | "CHROMA_CUSTOM2" => {
            let colors = flatten(param);
            if colors.is_empty() {
                return Err(RESULT_INVALID_PARAMETER);
            }
            Ok(match category {
                Category::Mousepad if colors.len() == 15 => SdkEffect::mousepad_v1(&colors),
                Category::Mouse if colors.len() == 30 => SdkEffect::mouse_v1(&colors),
                _ => SdkEffect::custom(category, colors),
            })
        }
        "CHROMA_CUSTOM_KEY" if category == Category::Keyboard => {
            Ok(SdkEffect::custom_key(&flatten(&param["color"]), &flatten(&param["key"])))
        }
        _ => Err(RESULT_NOT_SUPPORTED),
    }
}

// ---------------------------------------------------------------------------
// Control API and UI
// ---------------------------------------------------------------------------

fn handle_ui(mut req: Request, shared: &Shared) {
    // Only answer to local hostnames (blocks DNS-rebinding pages), and make
    // state changes require a custom header, which cross-site pages cannot
    // send without a CORS preflight this server never approves.
    let host = req.headers().iter().find(|h| h.field.equiv("Host")).map(|h| h.value.as_str().to_string()).unwrap_or_default();
    let host_name = host.rsplit_once(':').map_or(host.as_str(), |(h, _)| h);
    if !matches!(host_name, "127.0.0.1" | "localhost") {
        let _ = req.respond(Response::from_string("forbidden").with_status_code(403));
        return;
    }
    let trusted = req.headers().iter().any(|h| h.field.equiv("X-OpenChroma"));
    let method = req.method().clone();
    let url = req.url().split('?').next().unwrap_or("").to_string();

    match (method, url.as_str()) {
        (Method::Get, "/") => {
            let resp = Response::from_string(UI_HTML).with_header(header("Content-Type", "text/html; charset=utf-8"));
            let _ = req.respond(resp);
        }
        (Method::Get, "/api/status") => reply(req, 200, status(shared)),
        (Method::Get, "/api/config") => reply(req, 200, serde_json::to_value(shared.config()).expect("config serializes")),
        (Method::Put, "/api/config") if trusted => match serde_json::from_value::<Config>(body(&mut req)) {
            Ok(cfg) => match shared.update_config(|c| *c = cfg) {
                Ok(()) => reply(req, 200, json!({"ok": true})),
                Err(e) => reply(req, 500, json!({"error": format!("saved in memory but not to disk: {e}")})),
            },
            Err(e) => reply(req, 400, json!({"error": e.to_string()})),
        },
        (Method::Post, "/api/settings") if trusted => {
            let b = body(&mut req);
            let cfg = shared.config();
            if let Some(name) = b["active_profile"].as_str() {
                if !cfg.profiles.iter().any(|p| p.name == name) {
                    return reply(req, 404, json!({"error": format!("no profile named {name:?}")}));
                }
            }
            let result = shared.update_config(|c| {
                if let Some(name) = b["active_profile"].as_str() {
                    c.active_profile = name.to_string();
                }
                if let Some(v) = b["brightness"].as_u64() {
                    c.brightness = v.min(100) as u8;
                }
                if let Some(v) = b["sdk_enabled"].as_bool() {
                    c.sdk_enabled = v;
                }
            });
            match result {
                Ok(()) => reply(req, 200, json!({"ok": true})),
                Err(e) => reply(req, 500, json!({"error": e.to_string()})),
            }
        }
        (Method::Post, "/api/identify") if trusted => {
            // {"target": "argb:4"} flashes that device or zone; null stops.
            let b = body(&mut req);
            *lock(&shared.identify) = b["target"].as_str().map(|t| (t.to_string(), std::time::Instant::now()));
            reply(req, 200, json!({"ok": true}))
        }
        (Method::Post, "/api/apps") if trusted => {
            let b = body(&mut req);
            match (b["title"].as_str(), b["allowed"].as_bool()) {
                (Some(title), Some(allowed)) if shared.set_app_allowed(title, allowed) => reply(req, 200, json!({"ok": true})),
                (Some(title), Some(_)) => reply(req, 404, json!({"error": format!("no app named {title:?}")})),
                _ => reply(req, 400, json!({"error": "expected {\"title\": string, \"allowed\": bool}"})),
            }
        }
        (Method::Get, "/api/update") => reply(req, 200, crate::update::status()),
        (Method::Post, "/api/update") if trusted => match crate::update::start() {
            Ok(()) => reply(req, 202, json!({"ok": true})),
            Err(e) => reply(req, 409, json!({"error": e})),
        },
        (Method::Put | Method::Post, _) => reply(req, 403, json!({"error": "missing X-OpenChroma header"})),
        _ => reply(req, 404, json!({"error": "not found"})),
    }
}

/// DLL states change rarely and checking reads the files; status is polled
/// several times a second, so cache for a few seconds.
fn sdk_dlls() -> Value {
    static CACHE: std::sync::Mutex<Option<(std::time::Instant, Value)>> = std::sync::Mutex::new(None);
    let mut cache = lock(&CACHE);
    if let Some((at, v)) = cache.as_ref() {
        if at.elapsed() < std::time::Duration::from_secs(5) {
            return v.clone();
        }
    }
    let v = json!(crate::install::system_dll_states().into_iter().collect::<std::collections::BTreeMap<_, _>>());
    *cache = Some((std::time::Instant::now(), v.clone()));
    v
}

pub fn status(shared: &Shared) -> Value {
    let cfg = shared.config();
    let devices = lock(&shared.devices).clone();
    let sessions = lock(&shared.sessions);
    let active = sessions.active().map(|s| s.id);
    let sessions: Vec<Value> = sessions
        .all()
        .map(|s| {
            json!({
                "id": s.id,
                "title": s.app.title,
                "client": s.client,
                "active": Some(s.id) == active && cfg.sdk_enabled,
                "allowed": s.allowed,
                "categories": s.shown.keys().collect::<Vec<_>>(),
                "seconds": s.started.elapsed().as_secs(),
            })
        })
        .collect();
    json!({
        "version": env!("CARGO_PKG_VERSION"),
        "devices": devices,
        "sessions": sessions,
        "active_profile": cfg.active_profile,
        "profiles": cfg.profiles.iter().map(|p| &p.name).collect::<Vec<_>>(),
        "brightness": cfg.brightness,
        "sdk_enabled": cfg.sdk_enabled,
        "sdk_port_bound": SDK_PORT_BOUND.load(Ordering::SeqCst),
        "sdk_port": sdk_port(),
        "sdk_dlls": sdk_dlls(),
        "apps": lock(&shared.apps).list(),
        "identify": lock(&shared.identify).as_ref().map(|(t, _)| t.clone()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rest_keyboard_custom() {
        let grid: Vec<Vec<u32>> = (0..6).map(|r| (0..22).map(|c| r * 100 + c).collect()).collect();
        let e = parse_rest_effect(Category::Keyboard, &json!({"effect": "CHROMA_CUSTOM", "param": grid})).unwrap();
        let SdkEffect::Custom { colors } = e else { panic!() };
        assert_eq!(colors.len(), 132);
        assert_eq!(colors[22 + 3], 103);
    }

    #[test]
    fn rest_static_and_errors() {
        assert_eq!(
            parse_rest_effect(Category::ChromaLink, &json!({"effect": "CHROMA_STATIC", "param": {"color": 255}})),
            Ok(SdkEffect::Static { color: 255 })
        );
        assert_eq!(parse_rest_effect(Category::Mouse, &json!({"effect": "CHROMA_BOGUS"})), Err(RESULT_NOT_SUPPORTED));
        assert_eq!(parse_rest_effect(Category::Mouse, &json!({})), Err(RESULT_INVALID_PARAMETER));
    }
}
