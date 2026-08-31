// =============================================================================
// HYDRA-UMC-ORCHESTRATOR - src/server.rs
// Copyright (C) 2026 JuanenRac (Electro Hobby 3D) <electrohobby3d@gmail.com>
// GPL-3.0 - see LICENSE
// =============================================================================
//! Plain JSON/HTTP surface (`tiny_http`, blocking, no async runtime) -
//! same convention as `HYDRA-UMC-TWIN`'s/`HYDRA-UMC-SWARM-SYNC`'s/
//! `HYDRA-UMC-HIL-BRIDGE`'s own `server.rs`.
//!
//! Real gap this closes: `mission.rs`'s own `MissionRegistry` (dispatch/
//! start/complete/cancel/fail/recover, and the node-failure recovery
//! sweep) was only ever exercised through `mission-demo`'s hardcoded,
//! fixed scenario - never reachable with a real caller-supplied mission
//! id or node name. This is still purely in-memory bookkeeping: no real
//! gRPC wiring to `HYDRA-UMC-JOB-DISPATCHER`/`HYDRA-UMC-NODE-HEALING`
//! exists (see `main.rs`'s own module doc), and there is no real
//! E-STOP-sending code anywhere in this repository to expose - this
//! server does not grant any new physical authority, it makes the exact
//! same state machine `mission-demo` already exercises reachable over a
//! real API instead of only a fixed demo script.
//!
//! Unlike this ecosystem's other Rust services' `server.rs` (all
//! stateless computations), the `MissionRegistry` is real, shared,
//! mutable state that must persist across requests - `Arc<Mutex<..>>`,
//! one lock acquired per request, released before the response is
//! written.

use std::sync::{Arc, Mutex};

use serde::Deserialize;
use serde_json::json;
use tiny_http::{Header, Method, Response, Server};

use crate::mission::MissionRegistry;

type SharedRegistry = Arc<Mutex<MissionRegistry>>;

fn json_header() -> Header {
    Header::from_bytes(&b"Content-Type"[..], &b"application/json"[..]).unwrap()
}

fn write_json(request: tiny_http::Request, status: u16, body: &serde_json::Value) {
    let text = body.to_string();
    let response = Response::from_string(text)
        .with_status_code(status)
        .with_header(json_header());
    let _ = request.respond(response);
}

fn read_body(request: &mut tiny_http::Request) -> std::io::Result<String> {
    // as_reader() returns `&mut dyn Read` - a trait object, so the method
    // call below resolves via dynamic dispatch and needs no local
    // `use std::io::Read` (only calling through a generic `T: Read`
    // bound would).
    let mut raw = String::new();
    request.as_reader().read_to_string(&mut raw)?;
    Ok(raw)
}

/// Splits `/missions/m1/dispatch` into `["missions", "m1", "dispatch"]`,
/// dropping empty segments (a leading `/` would otherwise produce a
/// leading empty string).
fn segments(path: &str) -> Vec<&str> {
    path.split('/').filter(|s| !s.is_empty()).collect()
}

pub fn bind(addr: &str) -> std::io::Result<Server> {
    Server::http(addr).map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))
}

pub fn run(server: Server) {
    let registry: SharedRegistry = Arc::new(Mutex::new(MissionRegistry::new()));

    for mut request in server.incoming_requests() {
        let url = request.url().to_string();
        let path = url.split('?').next().unwrap_or("").to_string();
        let method = request.method().clone();
        let parts = segments(&path);

        match (method.clone(), parts.as_slice()) {
            (Method::Get, ["missions"]) => handle_list(request, &registry),
            (Method::Get, ["missions", id]) => handle_get(request, &registry, id),
            (Method::Post, ["missions"]) => {
                let raw = match read_body(&mut request) {
                    Ok(r) => r,
                    Err(e) => { write_json(request, 400, &json!({"error": e.to_string()})); continue; }
                };
                handle_add(request, &registry, &raw);
            }
            (Method::Post, ["missions", id, "dispatch"]) => {
                let raw = match read_body(&mut request) {
                    Ok(r) => r,
                    Err(e) => { write_json(request, 400, &json!({"error": e.to_string()})); continue; }
                };
                handle_dispatch(request, &registry, id, &raw);
            }
            (Method::Post, ["missions", id, "start"]) => handle_start(request, &registry, id),
            (Method::Post, ["missions", id, "complete"]) => handle_complete(request, &registry, id),
            (Method::Post, ["missions", id, "cancel"]) => handle_cancel(request, &registry, id),
            (Method::Post, ["missions", id, "fail"]) => {
                let raw = match read_body(&mut request) {
                    Ok(r) => r,
                    Err(e) => { write_json(request, 400, &json!({"error": e.to_string()})); continue; }
                };
                handle_fail(request, &registry, id, &raw);
            }
            (Method::Post, ["nodes", node, "recover"]) => handle_recover(request, &registry, node),
            (Method::Get, ["stats"]) => {
                let reg = registry.lock().unwrap();
                write_json(request, 200, &json!({"missionCount": reg.all().count()}));
            }
            _ => write_json(request, 404, &json!({"error": "not found"})),
        }
    }
}

#[derive(Deserialize)]
struct AddRequest {
    id: String,
}

fn handle_add(request: tiny_http::Request, registry: &SharedRegistry, raw: &str) {
    let req: AddRequest = match serde_json::from_str(raw) {
        Ok(r) => r,
        Err(e) => { write_json(request, 400, &json!({"error": format!("malformed request JSON: {e}")})); return; }
    };
    let mut reg = registry.lock().unwrap();
    let mission = reg.add(req.id);
    write_json(request, 200, &serde_json::to_value(&*mission).unwrap());
}

fn handle_list(request: tiny_http::Request, registry: &SharedRegistry) {
    let reg = registry.lock().unwrap();
    let missions: Vec<_> = reg.all().collect();
    write_json(request, 200, &json!({"missions": missions}));
}

fn handle_get(request: tiny_http::Request, registry: &SharedRegistry, id: &str) {
    let reg = registry.lock().unwrap();
    match reg.get(id) {
        Some(m) => write_json(request, 200, &serde_json::to_value(m).unwrap()),
        None => write_json(request, 404, &json!({"error": format!("no mission {id:?}")})),
    }
}

#[derive(Deserialize)]
struct DispatchRequest {
    node: String,
}

fn handle_dispatch(request: tiny_http::Request, registry: &SharedRegistry, id: &str, raw: &str) {
    let req: DispatchRequest = match serde_json::from_str(raw) {
        Ok(r) => r,
        Err(e) => { write_json(request, 400, &json!({"error": format!("malformed request JSON: {e}")})); return; }
    };
    let mut reg = registry.lock().unwrap();
    let Some(mission) = reg.get_mut(id) else {
        write_json(request, 404, &json!({"error": format!("no mission {id:?}")}));
        return;
    };
    match mission.dispatch(req.node) {
        Ok(()) => write_json(request, 200, &serde_json::to_value(&*mission).unwrap()),
        Err(e) => write_json(request, 409, &json!({"error": e.to_string(), "transition": e})),
    }
}

fn handle_start(request: tiny_http::Request, registry: &SharedRegistry, id: &str) {
    let mut reg = registry.lock().unwrap();
    let Some(mission) = reg.get_mut(id) else {
        write_json(request, 404, &json!({"error": format!("no mission {id:?}")}));
        return;
    };
    match mission.start() {
        Ok(()) => write_json(request, 200, &serde_json::to_value(&*mission).unwrap()),
        Err(e) => write_json(request, 409, &json!({"error": e.to_string(), "transition": e})),
    }
}

fn handle_complete(request: tiny_http::Request, registry: &SharedRegistry, id: &str) {
    let mut reg = registry.lock().unwrap();
    let Some(mission) = reg.get_mut(id) else {
        write_json(request, 404, &json!({"error": format!("no mission {id:?}")}));
        return;
    };
    match mission.complete() {
        Ok(()) => write_json(request, 200, &serde_json::to_value(&*mission).unwrap()),
        Err(e) => write_json(request, 409, &json!({"error": e.to_string(), "transition": e})),
    }
}

fn handle_cancel(request: tiny_http::Request, registry: &SharedRegistry, id: &str) {
    let mut reg = registry.lock().unwrap();
    let Some(mission) = reg.get_mut(id) else {
        write_json(request, 404, &json!({"error": format!("no mission {id:?}")}));
        return;
    };
    match mission.cancel() {
        Ok(outcome) => write_json(request, 200, &json!({"outcome": outcome, "mission": &*mission})),
        Err(e) => write_json(request, 409, &json!({"error": e.to_string(), "transition": e})),
    }
}

#[derive(Deserialize)]
struct FailRequest {
    reason: String,
}

fn handle_fail(request: tiny_http::Request, registry: &SharedRegistry, id: &str, raw: &str) {
    let req: FailRequest = match serde_json::from_str(raw) {
        Ok(r) => r,
        Err(e) => { write_json(request, 400, &json!({"error": format!("malformed request JSON: {e}")})); return; }
    };
    let mut reg = registry.lock().unwrap();
    let Some(mission) = reg.get_mut(id) else {
        write_json(request, 404, &json!({"error": format!("no mission {id:?}")}));
        return;
    };
    match mission.fail(req.reason) {
        Ok(()) => write_json(request, 200, &serde_json::to_value(&*mission).unwrap()),
        Err(e) => write_json(request, 409, &json!({"error": e.to_string(), "transition": e})),
    }
}

fn handle_recover(request: tiny_http::Request, registry: &SharedRegistry, node: &str) {
    let mut reg = registry.lock().unwrap();
    let requeued = reg.recover_node_unavailable(node);
    write_json(request, 200, &json!({"requeuedMissions": requeued}));
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpStream;
    use std::thread;

    fn start_test_server() -> u16 {
        let server = bind("127.0.0.1:0").expect("bind on an OS-assigned port must succeed");
        let port = server
            .server_addr()
            .to_ip()
            .expect("tiny_http always binds a real IP socket for an http:// server")
            .port();
        thread::spawn(move || run(server));
        port
    }

    fn request(port: u16, method: &str, path: &str, body: &str) -> (u16, String) {
        let mut stream = TcpStream::connect(("127.0.0.1", port)).expect("connect must succeed");
        let raw_request = format!(
            "{method} {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        stream.write_all(raw_request.as_bytes()).unwrap();
        let mut raw = String::new();
        stream.read_to_string(&mut raw).unwrap();
        let (headers, resp_body) = raw.split_once("\r\n\r\n").unwrap_or((raw.as_str(), ""));
        let status_line = headers.lines().next().unwrap_or("");
        let status: u16 = status_line.split_whitespace().nth(1).and_then(|s| s.parse().ok()).unwrap_or(0);
        (status, resp_body.to_string())
    }

    fn post(port: u16, path: &str, body: &str) -> (u16, String) {
        request(port, "POST", path, body)
    }
    fn get(port: u16, path: &str) -> (u16, String) {
        request(port, "GET", path, "")
    }

    #[test]
    fn add_mission_starts_pending() {
        let port = start_test_server();
        let (status, body) = post(port, "/missions", r#"{"id":"m1"}"#);
        assert_eq!(status, 200);
        assert!(body.contains("\"Pending\""));
    }

    #[test]
    fn full_happy_path_via_http() {
        let port = start_test_server();
        post(port, "/missions", r#"{"id":"m1"}"#);
        let (status, body) = post(port, "/missions/m1/dispatch", r#"{"node":"node-a"}"#);
        assert_eq!(status, 200);
        assert!(body.contains("Dispatched"));
        assert!(body.contains("node-a"));

        let (status, body) = post(port, "/missions/m1/start", "");
        assert_eq!(status, 200);
        assert!(body.contains("InProgress"));

        let (status, body) = post(port, "/missions/m1/complete", "");
        assert_eq!(status, 200);
        assert!(body.contains("Completed"));
    }

    #[test]
    fn invalid_transition_is_409() {
        let port = start_test_server();
        post(port, "/missions", r#"{"id":"m1"}"#);
        // start before dispatch is invalid - still Pending.
        let (status, body) = post(port, "/missions/m1/start", "");
        assert_eq!(status, 409);
        assert!(body.contains("Pending"));
    }

    #[test]
    fn cancel_is_idempotent_via_http() {
        let port = start_test_server();
        post(port, "/missions", r#"{"id":"m1"}"#);
        let (status, body) = post(port, "/missions/m1/cancel", "");
        assert_eq!(status, 200);
        assert!(body.contains("\"Cancelled\""));
        let (status, body) = post(port, "/missions/m1/cancel", "");
        assert_eq!(status, 200);
        assert!(body.contains("AlreadyCancelled"));
    }

    #[test]
    fn fail_records_a_reason() {
        let port = start_test_server();
        post(port, "/missions", r#"{"id":"m1"}"#);
        let (status, body) = post(port, "/missions/m1/fail", r#"{"reason":"no healthy node"}"#);
        assert_eq!(status, 200);
        assert!(body.contains("no healthy node"));
    }

    #[test]
    fn recover_requeues_only_missions_on_the_affected_node() {
        let port = start_test_server();
        post(port, "/missions", r#"{"id":"m1"}"#);
        post(port, "/missions", r#"{"id":"m2"}"#);
        post(port, "/missions/m1/dispatch", r#"{"node":"node-a"}"#);
        post(port, "/missions/m2/dispatch", r#"{"node":"node-b"}"#);

        let (status, body) = post(port, "/nodes/node-a/recover", "");
        assert_eq!(status, 200);
        assert!(body.contains("m1"));
        assert!(!body.contains("m2"));

        let (_, m1_body) = get(port, "/missions/m1");
        assert!(m1_body.contains("\"Pending\""));
    }

    #[test]
    fn list_and_get_missions() {
        let port = start_test_server();
        post(port, "/missions", r#"{"id":"m1"}"#);
        let (status, body) = get(port, "/missions");
        assert_eq!(status, 200);
        assert!(body.contains("m1"));

        let (status, _) = get(port, "/missions/does-not-exist");
        assert_eq!(status, 404);
    }

    #[test]
    fn stats_reports_mission_count() {
        let port = start_test_server();
        post(port, "/missions", r#"{"id":"m1"}"#);
        post(port, "/missions", r#"{"id":"m2"}"#);
        let (status, body) = get(port, "/stats");
        assert_eq!(status, 200);
        assert!(body.contains("\"missionCount\":2"));
    }

    #[test]
    fn unknown_path_is_404() {
        let port = start_test_server();
        let (status, _) = get(port, "/nope");
        assert_eq!(status, 404);
    }
}
