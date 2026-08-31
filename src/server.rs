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

use crate::job_dispatcher;
use crate::mission::MissionRegistry;

type SharedRegistry = Arc<Mutex<MissionRegistry>>;

/// Real shared state every request handler sees: the in-memory mission
/// registry, plus HYDRA-UMC-JOB-DISPATCHER's own base URL (`None` if
/// this Orchestrator was started without `--job-dispatcher-url` - the
/// integration is a real, but optional, best-effort add-on, not a hard
/// dependency this server refuses to start without).
pub struct AppState {
    pub registry: SharedRegistry,
    pub job_dispatcher_url: Option<String>,
}

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

pub fn run(server: Server, job_dispatcher_url: Option<String>) {
    let state = AppState {
        registry: Arc::new(Mutex::new(MissionRegistry::new())),
        job_dispatcher_url,
    };

    for mut request in server.incoming_requests() {
        let url = request.url().to_string();
        let path = url.split('?').next().unwrap_or("").to_string();
        let method = request.method().clone();
        let parts = segments(&path);

        match (method.clone(), parts.as_slice()) {
            (Method::Get, ["missions"]) => handle_list(request, &state),
            (Method::Get, ["missions", id]) => handle_get(request, &state, id),
            (Method::Post, ["missions"]) => {
                let raw = match read_body(&mut request) {
                    Ok(r) => r,
                    Err(e) => { write_json(request, 400, &json!({"error": e.to_string()})); continue; }
                };
                handle_add(request, &state, &raw);
            }
            (Method::Post, ["missions", id, "dispatch"]) => {
                let raw = match read_body(&mut request) {
                    Ok(r) => r,
                    Err(e) => { write_json(request, 400, &json!({"error": e.to_string()})); continue; }
                };
                handle_dispatch(request, &state, id, &raw);
            }
            (Method::Post, ["missions", id, "auto-dispatch"]) => handle_auto_dispatch(request, &state, id),
            (Method::Post, ["missions", id, "start"]) => handle_start(request, &state, id),
            (Method::Post, ["missions", id, "complete"]) => handle_complete(request, &state, id),
            (Method::Post, ["missions", id, "cancel"]) => handle_cancel(request, &state, id),
            (Method::Post, ["missions", id, "fail"]) => {
                let raw = match read_body(&mut request) {
                    Ok(r) => r,
                    Err(e) => { write_json(request, 400, &json!({"error": e.to_string()})); continue; }
                };
                handle_fail(request, &state, id, &raw);
            }
            (Method::Post, ["nodes", node, "recover"]) => handle_recover(request, &state, node),
            (Method::Get, ["stats"]) => {
                let reg = state.registry.lock().unwrap();
                write_json(request, 200, &json!({
                    "missionCount": reg.all().count(),
                    "jobDispatcherUrl": state.job_dispatcher_url,
                }));
            }
            _ => write_json(request, 404, &json!({"error": "not found"})),
        }
    }
}

#[derive(Deserialize)]
struct AddRequest {
    id: String,
}

fn handle_add(request: tiny_http::Request, state: &AppState, raw: &str) {
    let req: AddRequest = match serde_json::from_str(raw) {
        Ok(r) => r,
        Err(e) => { write_json(request, 400, &json!({"error": format!("malformed request JSON: {e}")})); return; }
    };

    let mission_snapshot = {
        let mut reg = state.registry.lock().unwrap();
        let mission = reg.add(req.id);
        serde_json::to_value(&*mission).unwrap()
    };
    // Real, deliberate second half of the "full chain": every mission
    // this Orchestrator now knows about is also submitted as a real job
    // to Job-Dispatcher's own queue, so its real tool-aware/fairness
    // routing has something to route - best-effort (a Job-Dispatcher
    // that's down doesn't stop a mission from existing here, this
    // registry is the source of truth for mission STATE regardless).
    let job_submission = submit_to_job_dispatcher_if_configured(state, mission_snapshot["id"].as_str().unwrap_or_default());

    write_json(request, 200, &json!({"mission": mission_snapshot, "jobDispatcher": job_submission}));
}

/// Returns a real, honest status string for the mission's own JSON
/// response - never silently swallowed, so a caller can tell "queued
/// for real routing" apart from "Job-Dispatcher wasn't configured/was
/// unreachable" without needing to read this server's own logs.
fn submit_to_job_dispatcher_if_configured(state: &AppState, mission_id: &str) -> serde_json::Value {
    let Some(base_url) = &state.job_dispatcher_url else {
        return json!({"submitted": false, "reason": "no --job-dispatcher-url configured"});
    };
    match job_dispatcher::submit_job(base_url, mission_id) {
        Ok(()) => json!({"submitted": true}),
        Err(e) => json!({"submitted": false, "reason": e.to_string()}),
    }
}

fn handle_list(request: tiny_http::Request, state: &AppState) {
    let reg = state.registry.lock().unwrap();
    let missions: Vec<_> = reg.all().collect();
    write_json(request, 200, &json!({"missions": missions}));
}

fn handle_get(request: tiny_http::Request, state: &AppState, id: &str) {
    let reg = state.registry.lock().unwrap();
    match reg.get(id) {
        Some(m) => write_json(request, 200, &serde_json::to_value(m).unwrap()),
        None => write_json(request, 404, &json!({"error": format!("no mission {id:?}")})),
    }
}

#[derive(Deserialize)]
struct DispatchRequest {
    node: String,
}

fn handle_dispatch(request: tiny_http::Request, state: &AppState, id: &str, raw: &str) {
    let req: DispatchRequest = match serde_json::from_str(raw) {
        Ok(r) => r,
        Err(e) => { write_json(request, 400, &json!({"error": format!("malformed request JSON: {e}")})); return; }
    };
    let mut reg = state.registry.lock().unwrap();
    let Some(mission) = reg.get_mut(id) else {
        write_json(request, 404, &json!({"error": format!("no mission {id:?}")}));
        return;
    };
    match mission.dispatch(req.node) {
        Ok(()) => write_json(request, 200, &serde_json::to_value(&*mission).unwrap()),
        Err(e) => write_json(request, 409, &json!({"error": e.to_string(), "transition": e})),
    }
}

/// The real "full chain" dispatch path: asks Job-Dispatcher to run one
/// real scheduling pass (`POST /dispatch` - tool-aware matching,
/// fairness by `Load`, the actual routing algorithm this project's own
/// `mission.rs` never had), then transitions this mission's LOCAL state
/// using whichever robot Job-Dispatcher's real algorithm assigned it -
/// never a caller-supplied node the way `POST /missions/:id/dispatch`
/// (manual override, kept unchanged above) still takes one directly.
fn handle_auto_dispatch(request: tiny_http::Request, state: &AppState, id: &str) {
    let Some(base_url) = &state.job_dispatcher_url else {
        write_json(request, 503, &json!({"error": "no --job-dispatcher-url configured on this orchestrator"}));
        return;
    };

    {
        let reg = state.registry.lock().unwrap();
        if reg.get(id).is_none() {
            write_json(request, 404, &json!({"error": format!("no mission {id:?}")}));
            return;
        }
    }

    let assignments = match job_dispatcher::run_dispatch(base_url) {
        Ok(a) => a,
        Err(e) => {
            write_json(request, 502, &json!({"error": format!("job-dispatcher dispatch pass failed: {e}")}));
            return;
        }
    };
    let Some(assignment) = assignments.iter().find(|a| a.job_id == id) else {
        // A real, honest "not yet" - the mission is queued at
        // Job-Dispatcher, but no robot matched this pass (none
        // available/right tool right now) - reconsidered on a future
        // /dispatch call, same as Job-Dispatcher's own README already
        // documents for its own /dispatch endpoint.
        write_json(request, 200, &json!({"assigned": false, "reason": "no matching robot on this dispatch pass"}));
        return;
    };

    let mut reg = state.registry.lock().unwrap();
    let mission = reg.get_mut(id).unwrap(); // existence already confirmed above, still held real by the lock
    match mission.dispatch(assignment.robot_id.clone()) {
        Ok(()) => write_json(request, 200, &json!({"assigned": true, "mission": &*mission})),
        Err(e) => write_json(request, 409, &json!({"error": e.to_string(), "transition": e})),
    }
}

fn handle_start(request: tiny_http::Request, state: &AppState, id: &str) {
    let mut reg = state.registry.lock().unwrap();
    let Some(mission) = reg.get_mut(id) else {
        write_json(request, 404, &json!({"error": format!("no mission {id:?}")}));
        return;
    };
    match mission.start() {
        Ok(()) => write_json(request, 200, &serde_json::to_value(&*mission).unwrap()),
        Err(e) => write_json(request, 409, &json!({"error": e.to_string(), "transition": e})),
    }
}

fn handle_complete(request: tiny_http::Request, state: &AppState, id: &str) {
    let mut reg = state.registry.lock().unwrap();
    let Some(mission) = reg.get_mut(id) else {
        write_json(request, 404, &json!({"error": format!("no mission {id:?}")}));
        return;
    };
    match mission.complete() {
        Ok(()) => write_json(request, 200, &serde_json::to_value(&*mission).unwrap()),
        Err(e) => write_json(request, 409, &json!({"error": e.to_string(), "transition": e})),
    }
}

fn handle_cancel(request: tiny_http::Request, state: &AppState, id: &str) {
    let mut reg = state.registry.lock().unwrap();
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

fn handle_fail(request: tiny_http::Request, state: &AppState, id: &str, raw: &str) {
    let req: FailRequest = match serde_json::from_str(raw) {
        Ok(r) => r,
        Err(e) => { write_json(request, 400, &json!({"error": format!("malformed request JSON: {e}")})); return; }
    };
    let mut reg = state.registry.lock().unwrap();
    let Some(mission) = reg.get_mut(id) else {
        write_json(request, 404, &json!({"error": format!("no mission {id:?}")}));
        return;
    };
    match mission.fail(req.reason) {
        Ok(()) => write_json(request, 200, &serde_json::to_value(&*mission).unwrap()),
        Err(e) => write_json(request, 409, &json!({"error": e.to_string(), "transition": e})),
    }
}

fn handle_recover(request: tiny_http::Request, state: &AppState, node: &str) {
    let mut reg = state.registry.lock().unwrap();
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
        start_test_server_with_job_dispatcher(None)
    }

    fn start_test_server_with_job_dispatcher(job_dispatcher_url: Option<String>) -> u16 {
        let server = bind("127.0.0.1:0").expect("bind on an OS-assigned port must succeed");
        let port = server
            .server_addr()
            .to_ip()
            .expect("tiny_http always binds a real IP socket for an http:// server")
            .port();
        thread::spawn(move || run(server, job_dispatcher_url));
        port
    }

    /// A tiny, real fake Job-Dispatcher answering every request the same
    /// way, forever (unlike job_dispatcher.rs's own single-shot fake) -
    /// this module's own tests need to survive both the /jobs/submit
    /// call handle_add makes AND a later /dispatch call in the same test.
    fn fake_job_dispatcher(status: u16, body: &'static str) -> String {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { continue };
                let mut buf = [0u8; 4096];
                let _ = stream.read(&mut buf);
                let response = format!(
                    "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = stream.write_all(response.as_bytes());
            }
        });
        format!("http://127.0.0.1:{port}")
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

    #[test]
    fn add_without_job_dispatcher_configured_reports_it_honestly() {
        let port = start_test_server(); // no job_dispatcher_url
        let (status, body) = post(port, "/missions", r#"{"id":"m1"}"#);
        assert_eq!(status, 200);
        assert!(body.contains("\"submitted\":false"));
        assert!(body.contains("no --job-dispatcher-url configured"));
    }

    #[test]
    fn add_submits_the_real_mission_to_job_dispatcher_when_configured() {
        let jd_url = fake_job_dispatcher(201, r#"{"ID":"m1","Status":"pending","result":"created"}"#);
        let port = start_test_server_with_job_dispatcher(Some(jd_url));
        let (status, body) = post(port, "/missions", r#"{"id":"m1"}"#);
        assert_eq!(status, 200);
        assert!(body.contains("\"submitted\":true"));
    }

    #[test]
    fn auto_dispatch_without_job_dispatcher_configured_is_503() {
        let port = start_test_server();
        post(port, "/missions", r#"{"id":"m1"}"#);
        let (status, _) = post(port, "/missions/m1/auto-dispatch", "");
        assert_eq!(status, 503);
    }

    #[test]
    fn auto_dispatch_unknown_mission_is_404() {
        let jd_url = fake_job_dispatcher(200, "[]");
        let port = start_test_server_with_job_dispatcher(Some(jd_url));
        let (status, _) = post(port, "/missions/does-not-exist/auto-dispatch", "");
        assert_eq!(status, 404);
    }

    #[test]
    fn auto_dispatch_transitions_the_mission_to_whatever_robot_job_dispatcher_assigned() {
        let jd_url = fake_job_dispatcher(200, r#"[{"JobID":"m1","RobotID":"arm-3"}]"#);
        let port = start_test_server_with_job_dispatcher(Some(jd_url));
        post(port, "/missions", r#"{"id":"m1"}"#);

        let (status, body) = post(port, "/missions/m1/auto-dispatch", "");
        assert_eq!(status, 200);
        assert!(body.contains("\"assigned\":true"));
        assert!(body.contains("arm-3"));

        let (_, m1_body) = get(port, "/missions/m1");
        assert!(m1_body.contains("Dispatched"));
        assert!(m1_body.contains("arm-3"));
    }

    #[test]
    fn auto_dispatch_reports_honestly_when_no_robot_matched_this_pass() {
        let jd_url = fake_job_dispatcher(200, "[]"); // real, empty dispatch pass
        let port = start_test_server_with_job_dispatcher(Some(jd_url));
        post(port, "/missions", r#"{"id":"m1"}"#);

        let (status, body) = post(port, "/missions/m1/auto-dispatch", "");
        assert_eq!(status, 200);
        assert!(body.contains("\"assigned\":false"));

        let (_, m1_body) = get(port, "/missions/m1");
        assert!(m1_body.contains("\"Pending\""), "an unmatched mission must stay Pending, not silently move on");
    }
}
