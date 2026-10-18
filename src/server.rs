// =============================================================================
// HYDRA-UMC-ORCHESTRATOR - src/server.rs
// Copyright (C) 2026 JuanenRac (Electro Hobby 3D) <electrohobby3d@gmail.com>
// GPL-3.0-or-later - see LICENSE
// =============================================================================
//! Plain JSON/HTTP surface (`tiny_http`, blocking, no async runtime) -
//! same convention as `HYDRA-UMC-TWIN`'s/`HYDRA-UMC-SWARM-SYNC`'s/
//! `HYDRA-UMC-HIL-BRIDGE`'s own `server.rs`.
//!
//! The `MissionRegistry` is real, shared, mutable state that must persist
//! across requests - `Arc<Mutex<..>>`, one lock acquired per request and
//! released before any call to Job-Dispatcher or before the response is
//! written, and durable across a real process restart (`mission.rs`'s own
//! `MissionRegistry::load()`/`persist()`, wired in by `main.rs`): every
//! handler below that mutates a mission calls `reg.persist()` before
//! responding, the same explicit convention `outbox.rs`'s callers use.
//!
//! Requests are served by a small pool of worker threads that share the
//! listener, so a slow client or a slow Job-Dispatcher call holds one
//! worker, not the whole API (`GET /health` keeps answering).
//!
//! There is no real E-STOP-sending code anywhere in this repository to
//! expose - this server does not grant any new physical authority, it
//! makes the mission state machine reachable over a real API.

use std::collections::HashMap;
use std::io::Read;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use serde::Deserialize;
use serde_json::json;
use tiny_http::{Header, Method, Response, Server};

use crate::job_dispatcher;
use crate::mission::{
    is_valid_mission_id, CancelOutcome, FenceError, JobSpec, MissionRegistry, TransitionError,
};
use crate::outbox::RemoteCloseOutbox;

// how often the background pass below looks for a mission whose terminal
// outcome still has to be confirmed to Job-Dispatcher - see
// reconcile_pending_remote_closes()'s own doc comment. A close that keeps
// failing is retried less and less often (REMOTE_CLOSE_MAX_BACKOFF).
const REMOTE_CLOSE_RETRY_INTERVAL: Duration = Duration::from_secs(30);
const REMOTE_CLOSE_MAX_BACKOFF: Duration = Duration::from_secs(30 * 60);

/// How many threads serve requests at the same time.
const WORKER_THREADS: usize = 8;

type SharedRegistry = Arc<Mutex<MissionRegistry>>;
// shared the same way SharedRegistry is - both the request
// handlers below and the background reconciliation thread touch it.
// RemoteCloseOutbox already guards its own internal state with a Mutex
// (see outbox.rs), so only the Arc for cross-thread sharing is needed
// here, no extra outer lock.
type SharedOutbox = Arc<RemoteCloseOutbox>;

/// Real shared state every request handler sees: the in-memory mission
/// registry, HYDRA-UMC-JOB-DISPATCHER's own base URL (`None` if this
/// Orchestrator was started without `--job-dispatcher-url` - the
/// integration is a real, but optional, best-effort add-on, not a hard
/// dependency this server refuses to start without), and the real,
/// durable pending-remote-close outbox (see outbox.rs's own module doc).
pub struct AppState {
    pub registry: SharedRegistry,
    pub job_dispatcher_url: Option<String>,
    pub close_outbox: SharedOutbox,
    pub token: Option<String>,
    pub require_attempt: bool,
}

/// How the HTTP surface is protected. The default - no token, attempt
/// optional - is what a loopback-only deployment (the shipped systemd unit)
/// has always run with.
#[derive(Debug, Clone, Default)]
pub struct ServerOptions {
    /// when set, every route except `GET /health` needs
    /// `Authorization: Bearer <token>`
    pub token: Option<String>,
    /// when set, `start`/`complete`/`fail` must carry the `attempt` the
    /// mission was dispatched with, so a node that lost the mission (and was
    /// declared unavailable) cannot overwrite the one that holds it now
    pub require_attempt: bool,
}

/// Request bodies here are a few dozen bytes; anything past this is refused
/// with 413 instead of being read into memory.
pub const MAX_BODY_BYTES: u64 = 64 * 1024;

/// True for the addresses that cannot be reached from another machine. Takes
/// a bare host (`127.0.0.1`, `::1`, `localhost`) or a host with a port
/// (`127.0.0.1:8114`, `[::1]:8114`, `localhost:8114`).
pub fn is_loopback_addr(addr: &str) -> bool {
    if let Ok(socket) = addr.parse::<std::net::SocketAddr>() {
        return socket.ip().is_loopback();
    }
    let host = match addr.rsplit_once(':') {
        Some((host, port)) if !host.contains(':') && port.chars().all(|c| c.is_ascii_digit()) => {
            host
        }
        _ => addr,
    };
    if host.eq_ignore_ascii_case("localhost") {
        return true;
    }
    host.trim_matches(|c| c == '[' || c == ']')
        .parse::<std::net::IpAddr>()
        .map(|ip| ip.is_loopback())
        .unwrap_or(false)
}

/// Why this process must not start: an API that dispatches, completes and
/// fails the missions of the fleet must not listen on the network without a
/// token unless the operator said so on purpose.
pub fn startup_problem(addr: &str, token: Option<&str>, allow_anonymous: bool) -> Option<String> {
    let has_token = token.is_some_and(|t| !t.is_empty());
    if is_loopback_addr(addr) || has_token || allow_anonymous {
        return None;
    }
    Some(format!(
        "refusing to listen on {addr} without a token: set --token (or ORCHESTRATOR_TOKEN), bind --addr 127.0.0.1, or pass --allow-anonymous to run an open API on purpose"
    ))
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

fn authorized(request: &tiny_http::Request, token: &Option<String>) -> bool {
    let Some(expected) = token else {
        return true;
    };
    request
        .headers()
        .iter()
        .find(|h| h.field.equiv("Authorization"))
        .and_then(|h| h.value.as_str().strip_prefix("Bearer "))
        .is_some_and(|given| constant_time_eq(given.as_bytes(), expected.as_bytes()))
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

struct BodyError {
    status: u16,
    message: String,
}

fn read_body(request: &mut tiny_http::Request) -> Result<String, BodyError> {
    // a body that announces more than the limit is refused before a single
    // byte is read, so a client cannot park a worker on a huge, slow upload
    if request
        .body_length()
        .is_some_and(|len| len as u64 > MAX_BODY_BYTES)
    {
        return Err(BodyError {
            status: 413,
            message: format!("request body is larger than {MAX_BODY_BYTES} bytes"),
        });
    }
    let mut raw = String::new();
    match Read::take(request.as_reader(), MAX_BODY_BYTES + 1).read_to_string(&mut raw) {
        Ok(_) if raw.len() as u64 > MAX_BODY_BYTES => Err(BodyError {
            status: 413,
            message: format!("request body is larger than {MAX_BODY_BYTES} bytes"),
        }),
        Ok(_) => Ok(raw),
        Err(e) => Err(BodyError {
            status: 400,
            message: e.to_string(),
        }),
    }
}

/// Reads the body or answers 400/413 and moves on to the next request.
macro_rules! body_or_reply {
    ($request:ident) => {
        match read_body(&mut $request) {
            Ok(raw) => raw,
            Err(e) => {
                write_json($request, e.status, &json!({"error": e.message}));
                continue;
            }
        }
    };
}

/// Decodes `%XX` escapes in one path segment (`a%20b` -> `a b`). A malformed
/// escape is kept as it is.
fn percent_decode(segment: &str) -> String {
    let bytes = segment.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).ok();
            if let Some(value) = hex.and_then(|h| u8::from_str_radix(h, 16).ok()) {
                out.push(value);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Splits `/missions/m1/dispatch` into `["missions", "m1", "dispatch"]`,
/// dropping empty segments (a leading `/` would otherwise produce a
/// leading empty string) and decoding `%XX` escapes, so a node name with a
/// space or a slash in it still names the node it was meant to.
fn segments(path: &str) -> Vec<String> {
    path.split('/')
        .filter(|s| !s.is_empty())
        .map(percent_decode)
        .collect()
}

pub fn bind(addr: &str) -> std::io::Result<Server> {
    Server::http(addr).map_err(std::io::Error::other)
}

pub fn run(
    server: Server,
    job_dispatcher_url: Option<String>,
    close_outbox: RemoteCloseOutbox,
    registry: MissionRegistry,
    options: ServerOptions,
) {
    let state = Arc::new(AppState {
        registry: Arc::new(Mutex::new(registry)),
        job_dispatcher_url,
        close_outbox: Arc::new(close_outbox),
        token: options.token.filter(|t| !t.is_empty()),
        require_attempt: options.require_attempt,
    });

    // only spawned when Job-Dispatcher integration is actually
    // configured at all (matching every other real call site in this
    // file) - see reconcile_pending_remote_closes()'s own doc comment.
    if let Some(base_url) = state.job_dispatcher_url.clone() {
        let registry = Arc::clone(&state.registry);
        let outbox = Arc::clone(&state.close_outbox);
        // A terminal mission whose close was never confirmed (the process
        // died between recording the outcome and the outbox entry) is
        // picked up from the registry itself.
        adopt_unconfirmed_closes(&registry, &outbox);
        thread::spawn(move || {
            // A real attempt right now, before waiting a full interval: the
            // whole point of persisting the outbox is a restart recovering
            // fast, not just eventually.
            let mut schedule = RetrySchedule::default();
            loop {
                reconcile_due(&registry, &outbox, &base_url, &mut schedule, Instant::now());
                thread::sleep(REMOTE_CLOSE_RETRY_INTERVAL);
            }
        });
    }

    let server = Arc::new(server);
    for _ in 1..WORKER_THREADS {
        let server = Arc::clone(&server);
        let state = Arc::clone(&state);
        thread::spawn(move || serve(&server, &state));
    }
    serve(&server, &state);
}

/// One worker: takes requests off the shared listener until it closes.
fn serve(server: &Server, state: &AppState) {
    for mut request in server.incoming_requests() {
        let url = request.url().to_string();
        let path = url.split('?').next().unwrap_or("").to_string();
        let method = request.method().clone();
        let decoded = segments(&path);
        let parts: Vec<&str> = decoded.iter().map(String::as_str).collect();

        // liveness stays open so a watchdog can probe it without a credential
        let is_health = matches!((&method, parts.as_slice()), (Method::Get, ["health"]));
        if !is_health && !authorized(&request, &state.token) {
            write_json(
                request,
                401,
                &json!({"error": "missing or wrong bearer token"}),
            );
            continue;
        }

        match (method.clone(), parts.as_slice()) {
            (Method::Get, ["health"]) => write_json(
                request,
                200,
                &json!({
                    "status": "ok",
                    "name": "HYDRA-UMC-ORCHESTRATOR",
                    "version": env!("CARGO_PKG_VERSION"),
                }),
            ),
            (Method::Get, ["missions"]) => handle_list(request, state, &url),
            (Method::Get, ["missions", id]) => handle_get(request, state, id),
            (Method::Post, ["missions"]) => {
                let raw = body_or_reply!(request);
                handle_add(request, state, &raw);
            }
            (Method::Post, ["missions", id, "dispatch"]) => {
                let raw = body_or_reply!(request);
                handle_dispatch(request, state, id, &raw);
            }
            (Method::Post, ["missions", id, "auto-dispatch"]) => {
                handle_auto_dispatch(request, state, id)
            }
            (Method::Post, ["missions", id, "start"]) => {
                let raw = body_or_reply!(request);
                handle_start(request, state, id, &raw);
            }
            (Method::Post, ["missions", id, "complete"]) => {
                let raw = body_or_reply!(request);
                handle_complete(request, state, id, &raw);
            }
            (Method::Post, ["missions", id, "cancel"]) => handle_cancel(request, state, id),
            (Method::Post, ["missions", id, "fail"]) => {
                let raw = body_or_reply!(request);
                handle_fail(request, state, id, &raw);
            }
            (Method::Post, ["nodes", node, "recover"]) => handle_recover(request, state, node),
            (Method::Get, ["stats"]) => handle_stats(request, state),
            _ => write_json(request, 404, &json!({"error": "not found"})),
        }
    }
}

/// Counts per state, how many closes still wait for Job-Dispatcher, and
/// where Job-Dispatcher is.
fn handle_stats(request: tiny_http::Request, state: &AppState) {
    let (count, mut by_state) = {
        let reg = state.registry.lock().unwrap();
        let mut by_state: std::collections::BTreeMap<&'static str, usize> =
            std::collections::BTreeMap::new();
        for mission in reg.all() {
            *by_state.entry(mission.state_label()).or_default() += 1;
        }
        (reg.all().count(), by_state)
    };
    by_state.entry("Pending").or_default();
    write_json(
        request,
        200,
        &json!({
            "missionCount": count,
            "missionsByState": by_state,
            "pendingRemoteCloses": state.close_outbox.pending().len(),
            "jobDispatcherUrl": state.job_dispatcher_url,
        }),
    );
}

/// The optional fencing fields of a start/complete/fail request.
#[derive(Deserialize, Default)]
struct FenceFields {
    #[serde(default)]
    attempt: Option<u32>,
    #[serde(default)]
    node: Option<String>,
}

fn parse_fence(raw: &str) -> Result<FenceFields, String> {
    if raw.trim().is_empty() {
        return Ok(FenceFields::default());
    }
    serde_json::from_str(raw).map_err(|e| format!("malformed request JSON: {e}"))
}

fn fence_status(error: &FenceError) -> u16 {
    match error {
        FenceError::AttemptRequired => 400,
        FenceError::StaleAttempt { .. } | FenceError::WrongNode { .. } => 409,
    }
}

#[derive(Deserialize)]
struct AddRequest {
    id: String,
    #[serde(default)]
    priority: i32,
    #[serde(default, rename = "requiredTool")]
    required_tool: String,
    #[serde(default)]
    location: String,
    #[serde(default, rename = "dependsOn")]
    depends_on: Vec<String>,
}

fn handle_add(request: tiny_http::Request, state: &AppState, raw: &str) {
    let req: AddRequest = match serde_json::from_str(raw) {
        Ok(r) => r,
        Err(e) => {
            write_json(
                request,
                400,
                &json!({"error": format!("malformed request JSON: {e}")}),
            );
            return;
        }
    };
    if !is_valid_mission_id(&req.id) {
        write_json(
            request,
            400,
            &json!({"error": "mission id must be 1 to 128 characters of A-Z a-z 0-9 . _ -"}),
        );
        return;
    }
    let spec = JobSpec {
        priority: req.priority,
        required_tool: req.required_tool,
        location: req.location,
        depends_on: req.depends_on,
    };

    let (mission_snapshot, spec) = {
        let mut reg = state.registry.lock().unwrap();
        // A repeated POST of a mission that already ended must not be sent
        // to Job-Dispatcher again: a resubmission of a failed job there
        // resets it to pending, and it would run work this orchestrator
        // already closed.
        if let Some(existing) = reg.get(&req.id) {
            if existing.is_terminal() {
                let body = json!({
                    "error": format!("mission {:?} already exists and is finished", req.id),
                    "mission": existing,
                });
                drop(reg);
                write_json(request, 409, &body);
                return;
            }
        }
        let mission = reg.add_with_spec(req.id, spec);
        let snapshot = serde_json::to_value(&*mission).unwrap();
        let spec = mission.spec.clone();
        reg.persist();
        (snapshot, spec)
    };
    // Real, deliberate second half of the "full chain": every mission
    // this Orchestrator now knows about is also submitted as a real job
    // to Job-Dispatcher's own queue, so its real tool-aware/fairness
    // routing has something to route - best-effort (a Job-Dispatcher
    // that's down doesn't stop a mission from existing here, this
    // registry is the source of truth for mission STATE regardless). The
    // submission is idempotent, so posting the mission again retries it.
    let job_submission = submit_to_job_dispatcher_if_configured(
        state,
        mission_snapshot["id"].as_str().unwrap_or_default(),
        &spec,
    );

    write_json(
        request,
        200,
        &json!({"mission": mission_snapshot, "jobDispatcher": job_submission}),
    );
}

/// Returns a real, honest status string for the mission's own JSON
/// response - never silently swallowed, so a caller can tell "queued
/// for real routing" apart from "Job-Dispatcher wasn't configured/was
/// unreachable" without needing to read this server's own logs.
fn submit_to_job_dispatcher_if_configured(
    state: &AppState,
    mission_id: &str,
    spec: &JobSpec,
) -> serde_json::Value {
    let Some(base_url) = &state.job_dispatcher_url else {
        return json!({"submitted": false, "reason": "no --job-dispatcher-url configured"});
    };
    match job_dispatcher::submit_job(base_url, mission_id, spec) {
        Ok(()) => json!({"submitted": true}),
        Err(e) => json!({"submitted": false, "reason": e.to_string()}),
    }
}

/// `GET /missions`, optionally narrowed with `?state=Pending` (a state
/// name, case-insensitive) and paged with `?limit=N&offset=N`.
fn handle_list(request: tiny_http::Request, state: &AppState, url: &str) {
    let query = url.split_once('?').map(|(_, q)| q).unwrap_or("");
    let mut wanted_state: Option<String> = None;
    let mut limit = usize::MAX;
    let mut offset = 0usize;
    for pair in query.split('&').filter(|p| !p.is_empty()) {
        let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
        match key {
            "state" => wanted_state = Some(percent_decode(value).to_ascii_lowercase()),
            "limit" => limit = value.parse().unwrap_or(usize::MAX),
            "offset" => offset = value.parse().unwrap_or(0),
            _ => {}
        }
    }
    let reg = state.registry.lock().unwrap();
    let missions: Vec<_> = reg
        .all()
        .filter(|m| {
            wanted_state
                .as_deref()
                .is_none_or(|s| m.state_label().to_ascii_lowercase() == s)
        })
        .skip(offset)
        .take(limit)
        .collect();
    write_json(request, 200, &json!({"missions": missions}));
}

fn handle_get(request: tiny_http::Request, state: &AppState, id: &str) {
    let reg = state.registry.lock().unwrap();
    match reg.get(id) {
        Some(m) => write_json(request, 200, &serde_json::to_value(m).unwrap()),
        None => write_json(
            request,
            404,
            &json!({"error": format!("no mission {id:?}")}),
        ),
    }
}

#[derive(Deserialize)]
struct DispatchRequest {
    node: String,
}

fn handle_dispatch(request: tiny_http::Request, state: &AppState, id: &str, raw: &str) {
    let req: DispatchRequest = match serde_json::from_str(raw) {
        Ok(r) => r,
        Err(e) => {
            write_json(
                request,
                400,
                &json!({"error": format!("malformed request JSON: {e}")}),
            );
            return;
        }
    };
    let mut reg = state.registry.lock().unwrap();
    let Some(mission) = reg.get_mut(id) else {
        write_json(
            request,
            404,
            &json!({"error": format!("no mission {id:?}")}),
        );
        return;
    };
    let outcome = mission.dispatch(req.node);
    if outcome.is_ok() {
        reg.persist();
    }
    match outcome {
        Ok(()) => write_json(
            request,
            200,
            &serde_json::to_value(reg.get(id).unwrap()).unwrap(),
        ),
        Err(e) => write_json(
            request,
            409,
            &json!({"error": e.to_string(), "transition": e}),
        ),
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
        write_json(
            request,
            503,
            &json!({"error": "no --job-dispatcher-url configured on this orchestrator"}),
        );
        return;
    };

    let pending_spec = {
        let reg = state.registry.lock().unwrap();
        match reg.get(id) {
            None => {
                write_json(
                    request,
                    404,
                    &json!({"error": format!("no mission {id:?}")}),
                );
                return;
            }
            Some(m) if matches!(m.state, crate::mission::MissionState::Pending) => {
                Some(m.spec.clone())
            }
            Some(_) => None,
        }
    };
    // A pending mission whose first submission never got through (the
    // dispatcher was down when it was added) has no job there to route.
    // The submission is idempotent, so doing it again is harmless.
    if let Some(spec) = pending_spec {
        if let Err(e) = job_dispatcher::submit_job(base_url, id, &spec) {
            write_json(
                request,
                502,
                &json!({"error": format!("job-dispatcher does not have the job and it could not be submitted: {e}")}),
            );
            return;
        }
    }

    let assignments = match job_dispatcher::run_dispatch(base_url) {
        Ok(a) => a,
        Err(e) => {
            write_json(
                request,
                502,
                &json!({"error": format!("job-dispatcher dispatch pass failed: {e}")}),
            );
            return;
        }
    };

    // a single /dispatch pass on Job-Dispatcher is its own real,
    // global scheduling algorithm - it can assign several jobs at once,
    // not just the one this caller asked about. Reconciling EVERY
    // returned assignment (not only the one matching `id`) keeps this
    // Orchestrator's own local mission state from diverging from what
    // Job-Dispatcher actually reserved: an assignment left unreconciled
    // here would show that OTHER mission as still "Queued" locally while
    // its robot is genuinely already busy on Job-Dispatcher's side.
    let mut result_for_caller: Option<Result<serde_json::Value, TransitionError>> = None;
    // Assignments of work this orchestrator already closed (a mission
    // cancelled while its job still sat in the queue, say) must not keep a
    // robot reserved: they are handed back below.
    let mut to_release: Vec<(String, bool)> = Vec::new();
    {
        let mut reg = state.registry.lock().unwrap();
        for assignment in &assignments {
            let Some(mission) = reg.get_mut(&assignment.job_id) else {
                continue; // not one of THIS orchestrator's own missions - nothing local to reconcile
            };
            let was_terminal = mission.is_terminal();
            let dispatch_result = mission.dispatch(assignment.robot_id.clone());
            if was_terminal {
                to_release.push((assignment.job_id.clone(), false));
            }
            if assignment.job_id == id {
                result_for_caller =
                    Some(dispatch_result.map(|()| serde_json::to_value(&*mission).unwrap()));
            } else if let Err(e) = dispatch_result {
                // A real state mismatch worth an operator's attention,
                // but not this caller's own request to fail on - they
                // asked about `id`, not this other mission.
                eprintln!(
                    "[orchestrator] could not reconcile job-dispatcher's assignment of {} to {}: {e}",
                    assignment.job_id, assignment.robot_id
                );
            }
        }
        reg.persist();
    }
    for (job_id, success) in to_release {
        if let Err(e) = job_dispatcher::close_job(base_url, &job_id, success) {
            eprintln!(
                "[orchestrator] could not hand back job {job_id} that job-dispatcher assigned although the mission is closed: {e}"
            );
            state.close_outbox.mark_pending(&job_id, success);
        }
    }

    match result_for_caller {
        None => write_json(
            request,
            200,
            // A real, honest "not yet" - the mission is queued at
            // Job-Dispatcher, but no robot matched this pass (none
            // available/right tool right now) - reconsidered on a future
            // /dispatch call, same as Job-Dispatcher's own README already
            // documents for its own /dispatch endpoint.
            &json!({"assigned": false, "reason": "no matching robot on this dispatch pass"}),
        ),
        Some(Ok(mission_json)) => write_json(
            request,
            200,
            &json!({"assigned": true, "mission": mission_json}),
        ),
        Some(Err(e)) => write_json(
            request,
            409,
            &json!({"error": e.to_string(), "transition": e}),
        ),
    }
}

fn handle_start(request: tiny_http::Request, state: &AppState, id: &str, raw: &str) {
    let fence = match parse_fence(raw) {
        Ok(f) => f,
        Err(message) => {
            write_json(request, 400, &json!({"error": message}));
            return;
        }
    };
    let mut reg = state.registry.lock().unwrap();
    let Some(mission) = reg.get_mut(id) else {
        write_json(
            request,
            404,
            &json!({"error": format!("no mission {id:?}")}),
        );
        return;
    };
    if let Err(e) = mission.check_fence(fence.attempt, fence.node.as_deref(), state.require_attempt)
    {
        write_json(
            request,
            fence_status(&e),
            &json!({"error": e.to_string(), "fence": e}),
        );
        return;
    }
    let outcome = mission.start();
    if outcome.is_ok() {
        reg.persist();
    }
    match outcome {
        Ok(()) => write_json(
            request,
            200,
            &serde_json::to_value(reg.get(id).unwrap()).unwrap(),
        ),
        Err(e) => write_json(
            request,
            409,
            &json!({"error": e.to_string(), "transition": e}),
        ),
    }
}

/// A mission's terminal outcome is committed locally first, and the
/// intent to confirm it to Job-Dispatcher is committed WITH it (the
/// registry is saved with `remote_close_confirmed = false` in the same
/// write, and the outbox gets its entry right after): a process that dies
/// between the outcome and the answer of Job-Dispatcher restarts with the
/// close still pending instead of forgetting it. Only once Job-Dispatcher
/// has taken the close do both records say it is confirmed.
///
/// `job_dispatcher::close_job()` is idempotent - retrying a close that
/// already went through, because only THIS side's confirmation got lost, is
/// always safe.
fn confirm_close_remotely(state: &AppState, id: &str, success: bool) {
    let Some(base_url) = &state.job_dispatcher_url else {
        return;
    };
    state.close_outbox.mark_pending(id, success);
    match job_dispatcher::close_job(base_url, id, success) {
        Ok(()) => mark_close_confirmed(&state.registry, &state.close_outbox, id),
        Err(e) => eprintln!(
            "[orchestrator] could not confirm the closing of mission {id} to job-dispatcher yet (it will be retried): {e}"
        ),
    }
}

fn mark_close_confirmed(registry: &SharedRegistry, outbox: &RemoteCloseOutbox, id: &str) {
    outbox.mark_confirmed(id);
    let mut reg = registry.lock().unwrap();
    if let Some(mission) = reg.get_mut(id) {
        mission.mark_remote_close_confirmed();
    }
    reg.persist();
}

/// Puts into the outbox every terminal mission the registry says is still
/// unconfirmed but the outbox does not know (a crash between the two writes).
fn adopt_unconfirmed_closes(registry: &SharedRegistry, outbox: &RemoteCloseOutbox) {
    let known: std::collections::HashSet<String> =
        outbox.pending().into_iter().map(|p| p.mission_id).collect();
    let reg = registry.lock().unwrap();
    for mission in reg.all() {
        if mission.is_terminal() && !mission.remote_close_confirmed && !known.contains(&mission.id)
        {
            outbox.mark_pending(
                &mission.id,
                matches!(
                    mission.state,
                    crate::mission::MissionState::Completed { .. }
                ),
            );
        }
    }
}

/// When each unconfirmed close may be tried again: a close that keeps
/// failing is retried after 30 s, 1 min, 2 min ... up to 30 min, instead of
/// every 30 s for ever.
#[derive(Default)]
struct RetrySchedule {
    entries: HashMap<String, (u32, Instant)>,
}

impl RetrySchedule {
    fn is_due(&self, id: &str, now: Instant) -> bool {
        self.entries.get(id).is_none_or(|(_, next)| now >= *next)
    }

    fn failed(&mut self, id: &str, now: Instant) {
        let attempts = self.entries.get(id).map_or(0, |(n, _)| *n) + 1;
        let wait = REMOTE_CLOSE_RETRY_INTERVAL
            .saturating_mul(1u32 << attempts.min(7))
            .min(REMOTE_CLOSE_MAX_BACKOFF);
        self.entries.insert(id.to_string(), (attempts, now + wait));
    }

    fn succeeded(&mut self, id: &str) {
        self.entries.remove(id);
    }
}

/// Tries every close still waiting for Job-Dispatcher, now.
///
/// The real worklist comes from `outbox` (see outbox.rs's own module doc), a
/// small durable JSON file independent of `MissionRegistry`'s own state, so
/// a fresh incarnation of this process still retries every mission the
/// outbox remembers, using the exact `success` value recorded when the close
/// was first attempted. `registry` is consulted only to keep a still-tracked
/// mission's own `remote_close_confirmed` field honest.
#[cfg(test)]
fn reconcile_pending_remote_closes(
    registry: &SharedRegistry,
    outbox: &RemoteCloseOutbox,
    base_url: &str,
) {
    reconcile_due(
        registry,
        outbox,
        base_url,
        &mut RetrySchedule::default(),
        Instant::now(),
    );
}

fn reconcile_due(
    registry: &SharedRegistry,
    outbox: &RemoteCloseOutbox,
    base_url: &str,
    schedule: &mut RetrySchedule,
    now: Instant,
) {
    for entry in outbox.pending() {
        if !schedule.is_due(&entry.mission_id, now) {
            continue;
        }
        match job_dispatcher::close_job(base_url, &entry.mission_id, entry.success) {
            Ok(()) => {
                schedule.succeeded(&entry.mission_id);
                mark_close_confirmed(registry, outbox, &entry.mission_id);
            }
            Err(_) => schedule.failed(&entry.mission_id, now),
        }
    }
}

fn handle_complete(request: tiny_http::Request, state: &AppState, id: &str, raw: &str) {
    let fence = match parse_fence(raw) {
        Ok(f) => f,
        Err(message) => {
            write_json(request, 400, &json!({"error": message}));
            return;
        }
    };
    let result = {
        let mut reg = state.registry.lock().unwrap();
        let Some(mission) = reg.get_mut(id) else {
            write_json(
                request,
                404,
                &json!({"error": format!("no mission {id:?}")}),
            );
            return;
        };
        if let Err(e) =
            mission.check_fence(fence.attempt, fence.node.as_deref(), state.require_attempt)
        {
            write_json(
                request,
                fence_status(&e),
                &json!({"error": e.to_string(), "fence": e}),
            );
            return;
        }
        let outcome = mission.complete();
        if outcome.is_ok() && state.job_dispatcher_url.is_some() {
            mission.mark_remote_close_pending();
        }
        if outcome.is_ok() {
            reg.persist();
        }
        outcome
    };
    match result {
        Ok(()) => {
            // tells Job-Dispatcher this job reached its real terminal
            // outcome, so it frees the robot's reservation instead of
            // continuing to believe it's still assigned to a mission this
            // Orchestrator already closed out locally. The lock is dropped
            // first: a slow Job-Dispatcher never stalls the registry.
            confirm_close_remotely(state, id, true);
            // Re-serialized AFTER the call, so the response's own
            // remote_close_confirmed field is accurate.
            let mission_json =
                serde_json::to_value(state.registry.lock().unwrap().get(id).unwrap()).unwrap();
            write_json(request, 200, &mission_json);
        }
        Err(e) => write_json(
            request,
            409,
            &json!({"error": e.to_string(), "transition": e}),
        ),
    }
}

fn handle_cancel(request: tiny_http::Request, state: &AppState, id: &str) {
    let result = {
        let mut reg = state.registry.lock().unwrap();
        let Some(mission) = reg.get_mut(id) else {
            write_json(
                request,
                404,
                &json!({"error": format!("no mission {id:?}")}),
            );
            return;
        };
        let outcome = mission.cancel();
        if outcome == Ok(CancelOutcome::Cancelled) && state.job_dispatcher_url.is_some() {
            mission.mark_remote_close_pending();
        }
        if outcome.is_ok() {
            reg.persist();
        }
        outcome
    };
    match result {
        Ok(outcome) => {
            // same real confirmation as handle_complete, but only for a
            // FRESH cancellation (CancelOutcome::Cancelled) - a no-op
            // re-cancel of an already-cancelled mission has nothing new to
            // confirm to Job-Dispatcher.
            if outcome == CancelOutcome::Cancelled {
                confirm_close_remotely(state, id, false);
            }
            let mission_json =
                serde_json::to_value(state.registry.lock().unwrap().get(id).unwrap()).unwrap();
            write_json(
                request,
                200,
                &json!({"outcome": outcome, "mission": mission_json}),
            );
        }
        Err(e) => write_json(
            request,
            409,
            &json!({"error": e.to_string(), "transition": e}),
        ),
    }
}

#[derive(Deserialize)]
struct FailRequest {
    reason: String,
    #[serde(default)]
    attempt: Option<u32>,
    #[serde(default)]
    node: Option<String>,
}

fn handle_fail(request: tiny_http::Request, state: &AppState, id: &str, raw: &str) {
    let req: FailRequest = match serde_json::from_str(raw) {
        Ok(r) => r,
        Err(e) => {
            write_json(
                request,
                400,
                &json!({"error": format!("malformed request JSON: {e}")}),
            );
            return;
        }
    };
    let outcome = {
        let mut reg = state.registry.lock().unwrap();
        let Some(mission) = reg.get_mut(id) else {
            write_json(
                request,
                404,
                &json!({"error": format!("no mission {id:?}")}),
            );
            return;
        };
        if let Err(e) = mission.check_fence(req.attempt, req.node.as_deref(), state.require_attempt)
        {
            write_json(
                request,
                fence_status(&e),
                &json!({"error": e.to_string(), "fence": e}),
            );
            return;
        }
        let outcome = mission.fail(req.reason);
        if outcome.is_ok() && state.job_dispatcher_url.is_some() {
            mission.mark_remote_close_pending();
        }
        if outcome.is_ok() {
            reg.persist();
        }
        outcome
    };
    match outcome {
        Ok(()) => {
            // fail() only ever returns Ok(()) for a FRESH transition (it
            // refuses outright - Err - once the mission is already
            // terminal), so unlike handle_cancel there is no
            // already-failed case to skip here.
            confirm_close_remotely(state, id, false);
            write_json(
                request,
                200,
                &serde_json::to_value(state.registry.lock().unwrap().get(id).unwrap()).unwrap(),
            );
        }
        Err(e) => write_json(
            request,
            409,
            &json!({"error": e.to_string(), "transition": e}),
        ),
    }
}

fn handle_recover(request: tiny_http::Request, state: &AppState, node: &str) {
    let requeued: Vec<(String, JobSpec)> = {
        let mut reg = state.registry.lock().unwrap();
        let ids = reg.recover_node_unavailable(node);
        ids.into_iter()
            .map(|id| {
                let spec = reg.get(&id).map(|m| m.spec.clone()).unwrap_or_default();
                (id, spec)
            })
            .collect()
    };
    // Job-Dispatcher is told too, after the lock is dropped, so it does not
    // keep the job assigned to the node that just went away (and holds
    // that node's robot unavailable until it reports in again). Best-effort,
    // same reasoning as every other job_dispatcher.rs call: the registry
    // above is already the real source of truth for mission state.
    let mut not_notified: Vec<String> = Vec::new();
    if let Some(base_url) = &state.job_dispatcher_url {
        for (mission_id, spec) in &requeued {
            if let Err(e) = job_dispatcher::requeue_job(base_url, mission_id, node, spec) {
                eprintln!(
                    "[orchestrator] could not notify job-dispatcher to requeue mission {mission_id}: {e}"
                );
                not_notified.push(mission_id.clone());
            }
        }
    }
    let requeued: Vec<String> = requeued.into_iter().map(|(id, _)| id).collect();
    write_json(
        request,
        200,
        &json!({"requeuedMissions": requeued, "jobDispatcherNotNotified": not_notified}),
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpStream;
    use std::thread;

    /// Real, reproducible flake found here (not a theory): every test in
    /// this module binds its own real OS-assigned TCP port(s)
    /// (`start_test_server`/`fake_job_dispatcher`) and makes real
    /// blocking socket calls against them. `cargo test`'s default
    /// multi-threaded runner fires all of them at once, and under enough
    /// concurrent raw-socket traffic on this dev machine,
    /// `add_submits_the_real_mission_to_job_dispatcher_when_configured`
    /// intermittently saw its own `ureq::post` to its own
    /// `fake_job_dispatcher` fail as a transport error (reproduced with
    /// `cargo test --all-targets`, disappeared every time under
    /// `--test-threads=1`) - real OS/scheduler contention between this
    /// file's own tests, not a production bug (the exact same
    /// `submit_job` code path is what a real, unhurried single request
    /// already exercises correctly). Serializing this file's own tests
    /// behind one lock removes the contention at its real source instead
    /// of adding a retry to production code for a problem production
    /// never actually has.
    fn net_test_lock() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: Mutex<()> = Mutex::new(());
        match LOCK.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        }
    }

    fn start_test_server() -> u16 {
        start_test_server_with_job_dispatcher(None)
    }

    fn start_test_server_with_job_dispatcher(job_dispatcher_url: Option<String>) -> u16 {
        start_test_server_with(job_dispatcher_url, ServerOptions::default())
    }

    fn start_test_server_with(job_dispatcher_url: Option<String>, options: ServerOptions) -> u16 {
        let server = bind("127.0.0.1:0").expect("bind on an OS-assigned port must succeed");
        let port = server
            .server_addr()
            .to_ip()
            .expect("tiny_http always binds a real IP socket for an http:// server")
            .port();
        // A real, per-test-unique outbox path (the bound port is already
        // unique per test) - these tests run concurrently, so sharing one
        // file would have them corrupt each other's persisted state.
        let outbox_path =
            std::env::temp_dir().join(format!("orchestrator-server-test-outbox-{port}.json"));
        // a port can come round again in a later run: never inherit its files
        let _ = std::fs::remove_file(&outbox_path);
        let close_outbox = RemoteCloseOutbox::load(outbox_path)
            .expect("a fresh test outbox path must always load cleanly");
        // Same per-port-unique real path convention as the outbox above,
        // for the same reason - concurrent tests must never share one
        // missions.json.
        let missions_path =
            std::env::temp_dir().join(format!("orchestrator-server-test-missions-{port}.json"));
        let _ = std::fs::remove_file(&missions_path);
        let registry = MissionRegistry::load(missions_path)
            .expect("a fresh test registry path must always load cleanly");
        thread::spawn(move || run(server, job_dispatcher_url, close_outbox, registry, options));
        port
    }

    /// A tiny, real fake Job-Dispatcher answering every request the same
    /// way, forever (unlike job_dispatcher.rs's own single-shot fake) -
    /// this module's own tests need to survive both the /jobs/submit
    /// call handle_add makes AND a later /dispatch call in the same test.
    ///
    /// this used to read the incoming request with one single,
    /// non-looping `stream.read()` call, then immediately write the
    /// canned response and let the connection close. `ureq`'s own
    /// `send_string()` can write a request's headers and body as
    /// separate TCP writes - a real client (Orchestrator's own
    /// `job_dispatcher::run_dispatch`/`submit_job`) racing this fake
    /// server could have its socket closed mid-write, the exact real
    /// cause behind the intermittent
    /// `add_submits_the_real_mission_to_job_dispatcher_when_configured`
    /// transport-error flake `net_test_lock` above only papered over the
    /// INTER-test-contention half of. `read_full_request` below loops
    /// until it has actually seen the full request (headers, plus every
    /// declared `Content-Length` body byte) before ever answering - the
    /// same real fix `job_dispatcher.rs`'s own `fake_server` test helper
    /// already uses for the identical race.
    fn read_full_request(stream: &mut TcpStream) -> Vec<u8> {
        let mut raw = Vec::new();
        let mut buf = [0u8; 4096];
        let header_end = loop {
            let n = stream.read(&mut buf).unwrap_or(0);
            if n == 0 {
                return raw;
            }
            raw.extend_from_slice(&buf[..n]);
            if let Some(pos) = raw.windows(4).position(|w| w == b"\r\n\r\n") {
                break pos + 4;
            }
        };

        let headers = String::from_utf8_lossy(&raw[..header_end]);
        let content_length: usize = headers
            .lines()
            .find_map(|line| {
                line.to_ascii_lowercase()
                    .strip_prefix("content-length:")
                    .map(|v| v.trim().to_string())
            })
            .and_then(|v| v.parse().ok())
            .unwrap_or(0);

        while raw.len() < header_end + content_length {
            let n = stream.read(&mut buf).unwrap_or(0);
            if n == 0 {
                break;
            }
            raw.extend_from_slice(&buf[..n]);
        }
        raw
    }

    fn fake_job_dispatcher(status: u16, body: &'static str) -> String {
        let (url, _rx) = fake_job_dispatcher_capturing(status, body);
        url
    }

    /// Same real, always-answering fake as `fake_job_dispatcher`, but also
    /// reports every real request it received (method + path + body) so a
    /// test can assert Orchestrator actually made the specific call it
    /// claims to (e.g. a real `POST /jobs/complete` with the right
    /// `id`/`success`), not just that its own local state changed.
    fn fake_job_dispatcher_capturing(
        status: u16,
        body: &'static str,
    ) -> (String, std::sync::mpsc::Receiver<String>) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let (tx, rx) = std::sync::mpsc::channel();
        thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { continue };
                let raw = read_full_request(&mut stream);
                let _ = tx.send(String::from_utf8_lossy(&raw).to_string());
                let response = format!(
                    "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = stream.write_all(response.as_bytes());
            }
        });
        (format!("http://127.0.0.1:{port}"), rx)
    }

    /// A fake Job-Dispatcher that answers each request according to what was
    /// asked (`handler` gets the whole request text and returns the status and
    /// body), and reports every request it received, in order.
    fn fake_job_dispatcher_routing(
        handler: impl Fn(&str) -> (u16, &'static str) + Send + 'static,
    ) -> (String, std::sync::mpsc::Receiver<String>) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let (tx, rx) = std::sync::mpsc::channel();
        thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { continue };
                let raw = String::from_utf8_lossy(&read_full_request(&mut stream)).to_string();
                let (status, body) = handler(&raw);
                let _ = tx.send(raw);
                let response = format!(
                    "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = stream.write_all(response.as_bytes());
            }
        });
        (format!("http://127.0.0.1:{port}"), rx)
    }

    /// What a Job-Dispatcher answers while it holds the job `m1` in `status`
    /// (assigned to `arm-3` when a robot has it).
    fn jd_holding(status: &'static str) -> impl Fn(&str) -> (u16, &'static str) + Send + 'static {
        move |req: &str| {
            if req.starts_with("GET /jobs/m1 ") {
                match status {
                    "assigned" => (
                        200,
                        r#"{"ID":"m1","Status":"assigned","AssignedRobot":"arm-3"}"#,
                    ),
                    _ => (200, r#"{"ID":"m1","Status":"pending","AssignedRobot":""}"#),
                }
            } else if req.starts_with("POST /jobs/submit") {
                (201, r#"{"ID":"m1","Status":"pending","result":"created"}"#)
            } else {
                (200, r#"{"ID":"m1"}"#)
            }
        }
    }

    /// Everything the fake received so far, once it has been quiet for a moment.
    fn drain(rx: &std::sync::mpsc::Receiver<String>) -> Vec<String> {
        let mut seen = Vec::new();
        while let Ok(r) = rx.recv_timeout(std::time::Duration::from_millis(250)) {
            seen.push(r);
        }
        seen
    }

    fn request(port: u16, method: &str, path: &str, body: &str) -> (u16, String) {
        request_with_auth(port, method, path, body, None)
    }

    fn request_with_auth(
        port: u16,
        method: &str,
        path: &str,
        body: &str,
        token: Option<&str>,
    ) -> (u16, String) {
        let mut stream = TcpStream::connect(("127.0.0.1", port)).expect("connect must succeed");
        let auth = token
            .map(|t| format!("Authorization: Bearer {t}\r\n"))
            .unwrap_or_default();
        let raw_request = format!(
            "{method} {path} HTTP/1.1\r\nHost: 127.0.0.1\r\n{auth}Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        stream.write_all(raw_request.as_bytes()).unwrap();
        let mut raw = String::new();
        stream.read_to_string(&mut raw).unwrap();
        let (headers, resp_body) = raw.split_once("\r\n\r\n").unwrap_or((raw.as_str(), ""));
        let status_line = headers.lines().next().unwrap_or("");
        let status: u16 = status_line
            .split_whitespace()
            .nth(1)
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);
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
        let _guard = net_test_lock();
        let port = start_test_server();
        let (status, body) = post(port, "/missions", r#"{"id":"m1"}"#);
        assert_eq!(status, 200);
        assert!(body.contains("\"Pending\""));
    }

    #[test]
    fn full_happy_path_via_http() {
        let _guard = net_test_lock();
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
        let _guard = net_test_lock();
        let port = start_test_server();
        post(port, "/missions", r#"{"id":"m1"}"#);
        // start before dispatch is invalid - still Pending.
        let (status, body) = post(port, "/missions/m1/start", "");
        assert_eq!(status, 409);
        assert!(body.contains("Pending"));
    }

    #[test]
    fn cancel_is_idempotent_via_http() {
        let _guard = net_test_lock();
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
        let _guard = net_test_lock();
        let port = start_test_server();
        post(port, "/missions", r#"{"id":"m1"}"#);
        let (status, body) = post(port, "/missions/m1/fail", r#"{"reason":"no healthy node"}"#);
        assert_eq!(status, 200);
        assert!(body.contains("no healthy node"));
    }

    #[test]
    fn recover_requeues_only_missions_on_the_affected_node() {
        let _guard = net_test_lock();
        let port = start_test_server();
        post(port, "/missions", r#"{"id":"m1"}"#);
        post(port, "/missions", r#"{"id":"m2"}"#);
        post(port, "/missions/m1/dispatch", r#"{"node":"node-a"}"#);
        post(port, "/missions/m2/dispatch", r#"{"node":"node-b"}"#);

        let (status, body) = post(port, "/nodes/node-a/recover", "");
        assert_eq!(status, 200);
        assert!(body.contains("m1"), "{body}");
        assert!(!body.contains("m2"));

        let (_, m1_body) = get(port, "/missions/m1");
        assert!(m1_body.contains("\"Pending\""));
    }

    #[test]
    fn recover_still_succeeds_even_when_job_dispatcher_is_unreachable() {
        // Found while auditing the code: handle_recover() now also
        // notifies Job-Dispatcher to requeue
        // (job_dispatcher::requeue_job(), see that module's own tests
        // for the exact two-request contract) - but the registry above
        // is already the real source of truth for mission state
        // regardless of whether Job-Dispatcher is configured/reachable,
        // same best-effort reasoning every other job_dispatcher.rs call
        // already follows. A real unreachable port, not a mock.
        let _guard = net_test_lock();
        let port = start_test_server_with_job_dispatcher(Some("http://127.0.0.1:1".to_string()));
        post(port, "/missions", r#"{"id":"m1"}"#);
        post(port, "/missions/m1/dispatch", r#"{"node":"node-a"}"#);

        let (status, body) = post(port, "/nodes/node-a/recover", "");
        assert_eq!(status, 200);
        assert!(body.contains("m1"));

        let (_, m1_body) = get(port, "/missions/m1");
        assert!(m1_body.contains("\"Pending\""));
    }

    #[test]
    fn recover_notifies_job_dispatcher_to_requeue_when_configured() {
        // Real, reachable fake this time - proves handle_recover asks for the
        // requeue naming the node that went away.
        let _guard = net_test_lock();
        let (jd_url, rx) = fake_job_dispatcher_routing(jd_holding("assigned"));
        let port = start_test_server_with_job_dispatcher(Some(jd_url));
        post(port, "/missions", r#"{"id":"m1"}"#);
        post(port, "/missions/m1/dispatch", r#"{"node":"node-a"}"#);
        drain(&rx);

        let (status, body) = post(port, "/nodes/node-a/recover", "");
        assert_eq!(status, 200);
        assert!(body.contains("m1"));
        let calls = drain(&rx);
        let requeue = calls
            .iter()
            .find(|c| c.starts_with("POST /jobs/requeue"))
            .unwrap_or_else(|| panic!("expected a requeue request, got {calls:?}"));
        assert!(requeue.contains("\"id\":\"m1\""));
        assert!(requeue.contains("\"robotId\":\"node-a\""));
    }

    #[test]
    fn list_and_get_missions() {
        let _guard = net_test_lock();
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
        let _guard = net_test_lock();
        let port = start_test_server();
        post(port, "/missions", r#"{"id":"m1"}"#);
        post(port, "/missions", r#"{"id":"m2"}"#);
        let (status, body) = get(port, "/stats");
        assert_eq!(status, 200);
        assert!(body.contains("\"missionCount\":2"));
    }

    #[test]
    fn unknown_path_is_404() {
        let _guard = net_test_lock();
        let port = start_test_server();
        let (status, _) = get(port, "/nope");
        assert_eq!(status, 404);
    }

    #[test]
    fn add_without_job_dispatcher_configured_reports_it_honestly() {
        let _guard = net_test_lock();
        let port = start_test_server(); // no job_dispatcher_url
        let (status, body) = post(port, "/missions", r#"{"id":"m1"}"#);
        assert_eq!(status, 200);
        assert!(body.contains("\"submitted\":false"));
        assert!(body.contains("no --job-dispatcher-url configured"));
    }

    #[test]
    fn add_submits_the_real_mission_to_job_dispatcher_when_configured() {
        let _guard = net_test_lock();
        let jd_url =
            fake_job_dispatcher(201, r#"{"ID":"m1","Status":"pending","result":"created"}"#);
        let port = start_test_server_with_job_dispatcher(Some(jd_url));
        let (status, body) = post(port, "/missions", r#"{"id":"m1"}"#);
        assert_eq!(status, 200);
        assert!(body.contains("\"submitted\":true"));
    }

    #[test]
    fn auto_dispatch_without_job_dispatcher_configured_is_503() {
        let _guard = net_test_lock();
        let port = start_test_server();
        post(port, "/missions", r#"{"id":"m1"}"#);
        let (status, _) = post(port, "/missions/m1/auto-dispatch", "");
        assert_eq!(status, 503);
    }

    #[test]
    fn auto_dispatch_unknown_mission_is_404() {
        let _guard = net_test_lock();
        let jd_url = fake_job_dispatcher(200, "[]");
        let port = start_test_server_with_job_dispatcher(Some(jd_url));
        let (status, _) = post(port, "/missions/does-not-exist/auto-dispatch", "");
        assert_eq!(status, 404);
    }

    #[test]
    fn auto_dispatch_transitions_the_mission_to_whatever_robot_job_dispatcher_assigned() {
        let _guard = net_test_lock();
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
        let _guard = net_test_lock();
        let jd_url = fake_job_dispatcher(200, "[]"); // real, empty dispatch pass
        let port = start_test_server_with_job_dispatcher(Some(jd_url));
        post(port, "/missions", r#"{"id":"m1"}"#);

        let (status, body) = post(port, "/missions/m1/auto-dispatch", "");
        assert_eq!(status, 200);
        assert!(body.contains("\"assigned\":false"));

        let (_, m1_body) = get(port, "/missions/m1");
        assert!(
            m1_body.contains("\"Pending\""),
            "an unmatched mission must stay Pending, not silently move on"
        );
    }

    // a single /dispatch pass can assign several missions at once -
    // only the requested one was ever reconciled locally.
    #[test]
    fn auto_dispatch_reconciles_every_real_assignment_the_pass_returned_not_only_the_requested_one()
    {
        let _guard = net_test_lock();
        let jd_url = fake_job_dispatcher(
            200,
            r#"[{"JobID":"m1","RobotID":"arm-1"},{"JobID":"m2","RobotID":"arm-2"}]"#,
        );
        let port = start_test_server_with_job_dispatcher(Some(jd_url));
        post(port, "/missions", r#"{"id":"m1"}"#);
        post(port, "/missions", r#"{"id":"m2"}"#);

        let (status, body) = post(port, "/missions/m1/auto-dispatch", "");
        assert_eq!(status, 200);
        assert!(body.contains("arm-1"));

        // m2 was never the requested mission, but the same real dispatch
        // pass reserved arm-2 for it on Job-Dispatcher's side - the local
        // registry must reflect that too, not leave it looking "Pending"
        // while its robot is genuinely already busy.
        let (_, m2_body) = get(port, "/missions/m2");
        assert!(
            m2_body.contains("Dispatched") && m2_body.contains("arm-2"),
            "m2's own real assignment from the same dispatch pass must be reconciled locally too, got: {m2_body}"
        );
    }

    // (found in the same review pass, P1): completing/cancelling a
    // mission never told Job-Dispatcher, which could keep believing the
    // job (and its robot's reservation) was still active.
    #[test]
    fn complete_confirms_the_real_terminal_outcome_to_job_dispatcher() {
        let _guard = net_test_lock();
        let (jd_url, rx) = fake_job_dispatcher_routing(jd_holding("assigned"));
        let port = start_test_server_with_job_dispatcher(Some(jd_url));
        post(port, "/missions", r#"{"id":"m1"}"#);
        drain(&rx); // the /jobs/submit call from add
        post(port, "/missions/m1/dispatch", r#"{"node":"arm-3"}"#);
        post(port, "/missions/m1/start", "");

        let (status, body) = post(port, "/missions/m1/complete", "");
        assert_eq!(status, 200);
        assert!(body.contains("\"remote_close_confirmed\":true"), "{body}");

        let calls = drain(&rx);
        assert!(calls[0].starts_with("GET /jobs/m1 "), "{calls:?}");
        let complete = &calls[1];
        assert!(complete.starts_with("POST /jobs/complete"), "{calls:?}");
        assert!(complete.contains("\"id\":\"m1\""));
        assert!(complete.contains("\"success\":true"));
        assert!(
            complete.contains("\"robotId\":\"arm-3\""),
            "the close must name the robot job-dispatcher says holds the job: {complete}"
        );
    }

    #[test]
    fn cancel_confirms_the_real_terminal_outcome_to_job_dispatcher() {
        let _guard = net_test_lock();
        let (jd_url, rx) = fake_job_dispatcher_routing(jd_holding("pending"));
        let port = start_test_server_with_job_dispatcher(Some(jd_url));
        post(port, "/missions", r#"{"id":"m1"}"#);
        drain(&rx); // the /jobs/submit call from add

        let (status, _) = post(port, "/missions/m1/cancel", "");
        assert_eq!(status, 200);

        let calls = drain(&rx);
        assert!(calls[0].starts_with("GET /jobs/m1 "), "{calls:?}");
        assert!(
            calls[1].starts_with("POST /jobs/cancel"),
            "a mission cancelled while its job is still queued must withdraw the job: {calls:?}"
        );
        assert!(calls[1].contains("\"id\":\"m1\""));
    }

    #[test]
    fn a_close_is_recorded_before_job_dispatcher_is_asked() {
        // The outbox entry and the unconfirmed flag exist even when the
        // answer never comes (here: nothing is listening), so a restart
        // finds the close still pending.
        let _guard = net_test_lock();
        let port = start_test_server_with_job_dispatcher(Some("http://127.0.0.1:1".to_string()));
        post(port, "/missions", r#"{"id":"m1"}"#);
        post(port, "/missions/m1/cancel", "");
        let outbox_path =
            std::env::temp_dir().join(format!("orchestrator-server-test-outbox-{port}.json"));
        let outbox = RemoteCloseOutbox::load(outbox_path).unwrap();
        assert_eq!(outbox.pending().len(), 1);
        let missions_path =
            std::env::temp_dir().join(format!("orchestrator-server-test-missions-{port}.json"));
        let on_disk = MissionRegistry::load(missions_path).unwrap();
        assert!(!on_disk.get("m1").unwrap().remote_close_confirmed);
    }

    #[test]
    fn a_terminal_mission_left_unconfirmed_by_a_crash_is_adopted_into_the_outbox() {
        let mut registry = MissionRegistry::new();
        {
            let m = registry.add("m1");
            m.cancel().unwrap();
            m.mark_remote_close_pending();
            registry.add("m2").cancel().unwrap(); // confirmed, must not be adopted
        }
        let registry: SharedRegistry = Arc::new(Mutex::new(registry));
        let outbox = test_outbox("adopt");
        adopt_unconfirmed_closes(&registry, &outbox);
        let pending = outbox.pending();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].mission_id, "m1");
        assert!(!pending[0].success);
    }

    #[test]
    fn cancel_does_not_re_notify_job_dispatcher_on_an_idempotent_re_cancel() {
        let _guard = net_test_lock();
        let (jd_url, rx) = fake_job_dispatcher_routing(jd_holding("pending"));
        let port = start_test_server_with_job_dispatcher(Some(jd_url));
        post(port, "/missions", r#"{"id":"m1"}"#);
        drain(&rx); // the /jobs/submit call from add

        post(port, "/missions/m1/cancel", "");
        assert!(
            !drain(&rx).is_empty(),
            "the first, real cancellation must notify job-dispatcher"
        );

        let (status, body) = post(port, "/missions/m1/cancel", "");
        assert_eq!(status, 200);
        assert!(body.contains("AlreadyCancelled"));
        assert!(
            drain(&rx).is_empty(),
            "a no-op re-cancel of an already-cancelled mission must not notify job-dispatcher again"
        );
    }

    // a failed
    // confirmation to Job-Dispatcher used to be invisible to any real
    // caller - the mission looked identical to a confirmed one.
    #[test]
    fn complete_marks_remote_close_pending_when_job_dispatcher_call_fails() {
        let _guard = net_test_lock();
        let port = start_test_server_with_job_dispatcher(Some("http://127.0.0.1:1".to_string()));
        post(port, "/missions", r#"{"id":"m1"}"#);
        post(port, "/missions/m1/dispatch", r#"{"node":"node-a"}"#);
        post(port, "/missions/m1/start", "");

        let (status, body) = post(port, "/missions/m1/complete", "");
        assert_eq!(status, 200);
        assert!(
            body.contains("\"remote_close_confirmed\":false"),
            "a failed job-dispatcher confirmation must be reflected honestly, got: {body}"
        );

        // GET must show the same real, current fact - not a stale
        // snapshot from before the failed confirmation attempt.
        let (_, get_body) = get(port, "/missions/m1");
        assert!(get_body.contains("\"remote_close_confirmed\":false"));
    }

    #[test]
    fn cancel_marks_remote_close_pending_when_job_dispatcher_call_fails() {
        let _guard = net_test_lock();
        let port = start_test_server_with_job_dispatcher(Some("http://127.0.0.1:1".to_string()));
        post(port, "/missions", r#"{"id":"m1"}"#);

        let (status, body) = post(port, "/missions/m1/cancel", "");
        assert_eq!(status, 200);
        assert!(body.contains("\"remote_close_confirmed\":false"));
    }

    /// A real, unique-per-test RemoteCloseOutbox backed by a real temp
    /// file - never shared between tests (which run concurrently), same
    /// reasoning as `start_test_server_with_job_dispatcher`'s own outbox
    /// path above.
    fn test_outbox(name: &str) -> RemoteCloseOutbox {
        let path = std::env::temp_dir().join(format!(
            "orchestrator-reconcile-test-outbox-{}-{name}.json",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path); // start clean even if a previous run left one behind
        RemoteCloseOutbox::load(path).expect("a fresh test outbox path must always load cleanly")
    }

    // these
    // three tests now exercise reconcile_pending_remote_closes()'s real
    // worklist source - the durable RemoteCloseOutbox, not
    // MissionRegistry's own in-memory pending_remote_closes() - proving
    // it retries correctly even when (as after a real restart)
    // MissionRegistry has never heard of the mission at all.

    #[test]
    fn reconcile_pending_remote_closes_confirms_a_previously_failed_completion() {
        // Direct unit test of the real background-retry logic, without
        // waiting on REMOTE_CLOSE_RETRY_INTERVAL's own real 30s cadence -
        // exercises the exact same function run()'s own background
        // thread calls, against a real (fast, in-test) fake
        // Job-Dispatcher.
        let registry: SharedRegistry = Arc::new(Mutex::new(MissionRegistry::new()));
        {
            let mut reg = registry.lock().unwrap();
            let mission = reg.add("m1");
            mission.dispatch("node-a").unwrap();
            mission.start().unwrap();
            mission.complete().unwrap();
            mission.mark_remote_close_pending(); // simulates the earlier failed attempt
        }
        let outbox = test_outbox("confirms-previously-failed");
        outbox.mark_pending("m1", true); // the real, durable record handle_complete would also have written

        let (jd_url, rx) = fake_job_dispatcher_routing(jd_holding("assigned"));
        reconcile_pending_remote_closes(&registry, &outbox, &jd_url);

        let calls = drain(&rx);
        let request = calls
            .iter()
            .find(|c| c.starts_with("POST /jobs/complete"))
            .expect("expected a real retried /jobs/complete call");
        assert!(request.contains("\"id\":\"m1\""));
        assert!(request.contains("\"success\":true"));

        let reg = registry.lock().unwrap();
        assert!(
            reg.get("m1").unwrap().remote_close_confirmed,
            "a successful retry must clear the pending flag"
        );
        assert!(
            outbox.pending().is_empty(),
            "a successful retry must also clear the real, durable outbox entry"
        );
    }

    #[test]
    fn reconcile_pending_remote_closes_leaves_the_flag_pending_when_the_retry_also_fails() {
        let registry: SharedRegistry = Arc::new(Mutex::new(MissionRegistry::new()));
        {
            let mut reg = registry.lock().unwrap();
            let mission = reg.add("m1");
            mission.dispatch("node-a").unwrap();
            mission.start().unwrap();
            mission.complete().unwrap();
            mission.mark_remote_close_pending();
        }
        let outbox = test_outbox("leaves-pending-on-retry-failure");
        outbox.mark_pending("m1", true);

        reconcile_pending_remote_closes(&registry, &outbox, "http://127.0.0.1:1"); // real unreachable port

        let reg = registry.lock().unwrap();
        assert!(
            !reg.get("m1").unwrap().remote_close_confirmed,
            "a retry that also fails must leave the mission pending, not silently mark it confirmed"
        );
        assert_eq!(
            outbox.pending().len(),
            1,
            "a retry that also fails must leave the real, durable outbox entry in place"
        );
    }

    #[test]
    fn reconcile_pending_remote_closes_ignores_missions_that_never_needed_one() {
        let registry: SharedRegistry = Arc::new(Mutex::new(MissionRegistry::new()));
        {
            let mut reg = registry.lock().unwrap();
            reg.add("m1"); // still Pending - not terminal at all, nothing to reconcile
        }
        let outbox = test_outbox("ignores-never-needed-one"); // real, but genuinely empty
        let (jd_url, rx) = fake_job_dispatcher_capturing(200, "{}");
        reconcile_pending_remote_closes(&registry, &outbox, &jd_url);
        assert!(
            rx.recv_timeout(std::time::Duration::from_millis(200))
                .is_err(),
            "an empty outbox has nothing to reconcile and must never be called out to"
        );
    }

    #[test]
    fn reconcile_pending_remote_closes_recovers_a_mission_a_real_restart_has_forgotten() {
        // The real point of a fresh MissionRegistry (exactly
        // what a real process restart produces - see mission.rs's own
        // module doc) has NEVER heard of "m1" at all, yet the durable
        // outbox alone is still enough to retry confirming its closure
        // to Job-Dispatcher - no orphaned reservation left behind purely
        // because this process forgot the mission ever existed.
        let registry: SharedRegistry = Arc::new(Mutex::new(MissionRegistry::new()));
        let outbox = test_outbox("recovers-after-restart");
        outbox.mark_pending("m1", true); // simulates outbox.load() at startup finding a real leftover entry

        let (jd_url, rx) = fake_job_dispatcher_routing(jd_holding("assigned"));
        reconcile_pending_remote_closes(&registry, &outbox, &jd_url);

        let calls = drain(&rx);
        let request = calls
            .iter()
            .find(|c| c.starts_with("POST /jobs/complete"))
            .expect("expected a real retried /jobs/complete call even with no matching mission in the registry");
        assert!(request.contains("\"id\":\"m1\""));
        assert!(request.contains("\"success\":true"));

        assert!(
            outbox.pending().is_empty(),
            "a successful retry must clear the outbox entry even when MissionRegistry never knew this mission"
        );
        assert!(
            registry.lock().unwrap().get("m1").is_none(),
            "this test's whole point: the registry genuinely never knew about m1"
        );
    }

    #[test]
    fn loopback_addresses_are_recognised_and_nothing_else_is() {
        for addr in [
            "127.0.0.1",
            "localhost",
            "LOCALHOST",
            "::1",
            "[::1]",
            "127.0.0.5",
            "127.0.0.1:8114",
            "localhost:8114",
            "[::1]:8114",
        ] {
            assert!(is_loopback_addr(addr), "{addr} is loopback");
        }
        for addr in [
            "0.0.0.0",
            "192.168.0.180",
            "::",
            "example.com",
            "",
            "0.0.0.0:8114",
            "192.168.0.180:8114",
            "[::]:8114",
            "example.com:8114",
        ] {
            assert!(!is_loopback_addr(addr), "{addr} is not loopback");
        }
    }

    #[test]
    fn the_process_refuses_to_serve_the_network_without_a_token() {
        assert!(startup_problem("127.0.0.1", None, false).is_none());
        // main.rs hands over "host:port", exactly as it binds
        assert!(startup_problem("127.0.0.1:8114", None, false).is_none());
        assert!(startup_problem("0.0.0.0:8114", None, false).is_some());
        assert!(startup_problem("0.0.0.0", None, false).is_some());
        assert!(startup_problem("192.168.0.180", Some(""), false).is_some());
        assert!(startup_problem("0.0.0.0", Some("a-token"), false).is_none());
        assert!(startup_problem("0.0.0.0", None, true).is_none());
    }

    #[test]
    fn with_a_token_every_route_but_health_demands_it() {
        let _guard = net_test_lock();
        let port = start_test_server_with(
            None,
            ServerOptions {
                token: Some("s3cret-token".to_string()),
                require_attempt: false,
            },
        );
        for (method, path) in [
            ("GET", "/missions"),
            ("GET", "/stats"),
            ("POST", "/missions"),
            ("POST", "/nodes/node-a/recover"),
            ("POST", "/missions/m1/complete"),
        ] {
            let (status, _) = request(port, method, path, "{}");
            assert_eq!(status, 401, "{method} {path} without a token");
            let (status, _) = request_with_auth(port, method, path, "{}", Some("wrong-token"));
            assert_eq!(status, 401, "{method} {path} with a wrong token");
        }
        let (status, body) = get(port, "/health");
        assert_eq!(status, 200, "liveness stays open");
        assert!(body.contains("HYDRA-UMC-ORCHESTRATOR"));
        let (status, _) = request_with_auth(port, "GET", "/missions", "", Some("s3cret-token"));
        assert_eq!(status, 200);
    }

    #[test]
    fn without_a_token_nothing_changes_for_a_loopback_deployment() {
        let _guard = net_test_lock();
        let port = start_test_server();
        assert_eq!(get(port, "/missions").0, 200);
        assert_eq!(get(port, "/health").0, 200);
    }

    #[test]
    fn a_body_over_the_limit_is_413_and_the_server_keeps_serving() {
        let _guard = net_test_lock();
        let port = start_test_server();
        let big = format!(
            "{{\"id\":\"{}\"}}",
            "x".repeat(MAX_BODY_BYTES as usize + 1024)
        );
        assert_eq!(post(port, "/missions", &big).0, 413);
        assert_eq!(post(port, "/missions/m1/complete", &big).0, 413);
        assert_eq!(post(port, "/missions/m1/fail", &big).0, 413);
        assert_eq!(post(port, "/missions", r#"{"id":"m1"}"#).0, 200);
    }

    #[test]
    fn a_late_report_from_the_node_that_lost_the_mission_is_refused_over_http() {
        let _guard = net_test_lock();
        let port = start_test_server();
        post(port, "/missions", r#"{"id":"m1"}"#);
        post(port, "/missions/m1/dispatch", r#"{"node":"node-a"}"#);
        post(
            port,
            "/missions/m1/start",
            r#"{"attempt":1,"node":"node-a"}"#,
        );

        // node-a is declared unavailable, the mission goes to node-b
        post(port, "/nodes/node-a/recover", "");
        let (status, body) = post(port, "/missions/m1/dispatch", r#"{"node":"node-b"}"#);
        assert_eq!(status, 200, "{body}");
        assert!(body.contains("\"attempt\":3"), "{body}");

        // node-a comes back from the partition and reports what it believes
        for path in ["/missions/m1/start", "/missions/m1/complete"] {
            let (status, body) = post(port, path, r#"{"attempt":1,"node":"node-a"}"#);
            assert_eq!(status, 409, "{path}: {body}");
            assert!(body.contains("stale"), "{path}: {body}");
        }
        let (status, _) = post(port, "/missions/m1/fail", r#"{"reason":"x","attempt":1}"#);
        assert_eq!(status, 409, "a stale node must not fail the mission either");

        // the mission is untouched, and node-b can carry on
        let (_, m) = get(port, "/missions/m1");
        assert!(m.contains("node-b") && m.contains("Dispatched"), "{m}");
        assert_eq!(
            post(
                port,
                "/missions/m1/start",
                r#"{"attempt":3,"node":"node-b"}"#
            )
            .0,
            200
        );
        assert_eq!(
            post(port, "/missions/m1/complete", r#"{"attempt":3}"#).0,
            200
        );
    }

    #[test]
    fn requests_without_fencing_fields_still_work_unless_the_attempt_is_required() {
        let _guard = net_test_lock();
        let port = start_test_server();
        post(port, "/missions", r#"{"id":"m1"}"#);
        post(port, "/missions/m1/dispatch", r#"{"node":"node-a"}"#);
        assert_eq!(post(port, "/missions/m1/start", "").0, 200);
        assert_eq!(post(port, "/missions/m1/complete", "").0, 200);

        let strict = start_test_server_with(
            None,
            ServerOptions {
                token: None,
                require_attempt: true,
            },
        );
        post(strict, "/missions", r#"{"id":"m2"}"#);
        post(strict, "/missions/m2/dispatch", r#"{"node":"node-a"}"#);
        let (status, body) = post(strict, "/missions/m2/start", "");
        assert_eq!(status, 400, "{body}");
        assert!(body.contains("attempt"));
        assert_eq!(
            post(strict, "/missions/m2/start", r#"{"attempt":1}"#).0,
            200
        );
        assert_eq!(
            post(strict, "/missions/m2/complete", r#"{"attempt":1}"#).0,
            200
        );
    }

    #[test]
    fn a_malformed_fencing_body_is_400() {
        let _guard = net_test_lock();
        let port = start_test_server();
        post(port, "/missions", r#"{"id":"m1"}"#);
        post(port, "/missions/m1/dispatch", r#"{"node":"node-a"}"#);
        assert_eq!(post(port, "/missions/m1/start", "not json").0, 400);
        assert_eq!(
            post(port, "/missions/m1/start", r#"{"attempt":"one"}"#).0,
            400
        );
    }
    #[test]
    fn a_node_name_with_escapes_is_decoded_before_it_is_matched() {
        let _guard = net_test_lock();
        let port = start_test_server();
        post(port, "/missions", r#"{"id":"m1"}"#);
        post(port, "/missions/m1/dispatch", r#"{"node":"cell a/1"}"#);
        let (status, body) = post(port, "/nodes/cell%20a%2F1/recover", "");
        assert_eq!(status, 200);
        assert!(body.contains("m1"), "{body}");
    }

    #[test]
    fn mission_ids_are_validated_and_a_finished_mission_is_not_reopened() {
        let _guard = net_test_lock();
        let (jd_url, rx) = fake_job_dispatcher_routing(jd_holding("pending"));
        let port = start_test_server_with_job_dispatcher(Some(jd_url));
        for bad in [r#"{"id":""}"#, r#"{"id":"a/b"}"#, r#"{"id":"a b"}"#] {
            assert_eq!(post(port, "/missions", bad).0, 400, "{bad}");
        }
        assert!(
            drain(&rx).is_empty(),
            "an invalid id must never reach job-dispatcher"
        );

        post(port, "/missions", r#"{"id":"m1"}"#);
        post(port, "/missions/m1/cancel", "");
        drain(&rx);
        let (status, body) = post(port, "/missions", r#"{"id":"m1"}"#);
        assert_eq!(status, 409, "{body}");
        assert!(
            drain(&rx).is_empty(),
            "a finished mission must not be submitted again: job-dispatcher would run it"
        );
        let (_, m) = get(port, "/missions/m1");
        assert!(m.contains("Cancelled"));
    }

    #[test]
    fn the_routing_hints_of_a_mission_reach_job_dispatcher() {
        let _guard = net_test_lock();
        let (jd_url, rx) = fake_job_dispatcher_routing(jd_holding("pending"));
        let port = start_test_server_with_job_dispatcher(Some(jd_url));
        let (status, body) = post(
            port,
            "/missions",
            r#"{"id":"m1","priority":9,"requiredTool":"Laser","location":"cell-2","dependsOn":["prep"]}"#,
        );
        assert_eq!(status, 200);
        assert!(body.contains("Laser"), "{body}");
        let calls = drain(&rx);
        let submit = calls
            .iter()
            .find(|c| c.starts_with("POST /jobs/submit"))
            .unwrap();
        assert!(submit.contains("\"priority\":9"));
        assert!(submit.contains("\"requiredTool\":\"Laser\""));
        assert!(submit.contains("\"location\":\"cell-2\""));
        assert!(submit.contains("\"dependsOn\":[\"prep\"]"));
    }

    #[test]
    fn auto_dispatch_submits_a_pending_mission_whose_first_submission_was_lost() {
        let _guard = net_test_lock();
        let (jd_url, rx) = fake_job_dispatcher_routing(|req| {
            if req.starts_with("POST /dispatch") {
                (200, r#"[{"JobID":"m1","RobotID":"arm-3"}]"#)
            } else {
                (201, r#"{"ID":"m1","Status":"pending"}"#)
            }
        });
        let port = start_test_server_with_job_dispatcher(Some(jd_url));
        post(port, "/missions", r#"{"id":"m1"}"#);
        drain(&rx);
        let (status, body) = post(port, "/missions/m1/auto-dispatch", "");
        assert_eq!(status, 200, "{body}");
        let calls = drain(&rx);
        assert!(calls[0].starts_with("POST /jobs/submit"), "{calls:?}");
        assert!(calls[1].starts_with("POST /dispatch"), "{calls:?}");
    }

    #[test]
    fn work_assigned_for_a_mission_that_is_already_closed_is_handed_back() {
        let _guard = net_test_lock();
        // job-dispatcher still had m1 queued when it was cancelled here, and
        // a dispatch pass for another mission hands it to arm-3
        let (jd_url, rx) = fake_job_dispatcher_routing(|req| {
            if req.starts_with("POST /dispatch") {
                (
                    200,
                    r#"[{"JobID":"m1","RobotID":"arm-3"},{"JobID":"m2","RobotID":"arm-4"}]"#,
                )
            } else if req.starts_with("GET /jobs/m1 ") {
                (
                    200,
                    r#"{"ID":"m1","Status":"assigned","AssignedRobot":"arm-3"}"#,
                )
            } else {
                (200, r#"{"ID":"x","Status":"pending"}"#)
            }
        });
        let port = start_test_server_with_job_dispatcher(Some(jd_url));
        post(port, "/missions", r#"{"id":"m1"}"#);
        post(port, "/missions", r#"{"id":"m2"}"#);
        post(port, "/missions/m1/cancel", "");
        drain(&rx);

        let (status, _) = post(port, "/missions/m2/auto-dispatch", "");
        assert_eq!(status, 200);
        let calls = drain(&rx);
        let release = calls
            .iter()
            .find(|c| c.starts_with("POST /jobs/complete"))
            .unwrap_or_else(|| panic!("the robot must be freed: {calls:?}"));
        assert!(release.contains("\"id\":\"m1\""));
        assert!(release.contains("\"success\":false"));
        assert!(release.contains("\"robotId\":\"arm-3\""));
        let (_, m1) = get(port, "/missions/m1");
        assert!(
            m1.contains("Cancelled"),
            "the mission stays cancelled: {m1}"
        );
    }

    #[test]
    fn missions_can_be_listed_by_state_and_counted() {
        let _guard = net_test_lock();
        let port = start_test_server();
        post(port, "/missions", r#"{"id":"a"}"#);
        post(port, "/missions", r#"{"id":"b"}"#);
        post(port, "/missions/b/cancel", "");
        let (_, pending) = get(port, "/missions?state=pending");
        assert!(
            pending.contains("\"a\"") && !pending.contains("\"b\""),
            "{pending}"
        );
        let (_, cancelled) = get(port, "/missions?state=Cancelled");
        assert!(cancelled.contains("\"b\"") && !cancelled.contains("\"a\""));
        let (_, page) = get(port, "/missions?limit=1&offset=1");
        assert!(page.contains("\"b\"") && !page.contains("\"a\""), "{page}");
        let (_, stats) = get(port, "/stats");
        assert!(stats.contains("\"missionCount\":2"), "{stats}");
        assert!(stats.contains("\"Cancelled\":1"), "{stats}");
        assert!(stats.contains("\"pendingRemoteCloses\":0"), "{stats}");
    }

    #[test]
    fn a_slow_upload_does_not_stop_the_other_requests() {
        let _guard = net_test_lock();
        let port = start_test_server();
        // announces 60000 bytes, sends one, and keeps the connection open
        let mut slow = TcpStream::connect(("127.0.0.1", port)).unwrap();
        slow.write_all(
            b"POST /missions HTTP/1.1\r\nHost: x\r\nContent-Type: application/json\r\nContent-Length: 60000\r\n\r\n{",
        )
        .unwrap();
        thread::sleep(std::time::Duration::from_millis(200));
        let started = std::time::Instant::now();
        assert_eq!(get(port, "/health").0, 200);
        assert!(started.elapsed() < std::time::Duration::from_secs(2));
        drop(slow);
    }

    #[test]
    fn a_body_that_announces_more_than_the_limit_is_refused_without_reading_it() {
        let _guard = net_test_lock();
        let port = start_test_server();
        let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
        stream
            .set_read_timeout(Some(std::time::Duration::from_secs(3)))
            .unwrap();
        stream
            .write_all(
                b"POST /missions HTTP/1.1\r\nHost: x\r\nContent-Length: 5000000\r\nConnection: close\r\n\r\n",
            )
            .unwrap();
        // the node answers at once but keeps the connection open while it
        // waits for the body it refused: one read is enough
        let mut buf = [0u8; 512];
        let n = stream.read(&mut buf).unwrap_or(0);
        let raw = String::from_utf8_lossy(&buf[..n]).to_string();
        assert!(raw.starts_with("HTTP/1.1 413"), "{raw}");
    }

    #[test]
    fn a_job_dispatcher_that_never_answers_does_not_freeze_the_api() {
        let _guard = net_test_lock();
        // accepts connections and never replies
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let jd_port = listener.local_addr().unwrap().port();
        thread::spawn(move || {
            let mut held = Vec::new();
            for stream in listener.incoming().flatten() {
                held.push(stream);
            }
        });
        let port =
            start_test_server_with_job_dispatcher(Some(format!("http://127.0.0.1:{jd_port}")));
        let blocked = thread::spawn(move || post(port, "/missions", r#"{"id":"m1"}"#));
        thread::sleep(std::time::Duration::from_millis(300));
        let started = std::time::Instant::now();
        assert_eq!(get(port, "/health").0, 200);
        assert!(started.elapsed() < std::time::Duration::from_secs(1));
        let (status, body) = blocked.join().unwrap();
        assert_eq!(
            status, 200,
            "the mission exists whatever job-dispatcher does"
        );
        assert!(body.contains("\"submitted\":false"), "{body}");
    }

    #[test]
    fn a_close_that_keeps_failing_is_retried_less_and_less_often() {
        let mut schedule = RetrySchedule::default();
        let t0 = Instant::now();
        assert!(schedule.is_due("m1", t0));
        schedule.failed("m1", t0);
        assert!(!schedule.is_due("m1", t0 + Duration::from_secs(10)));
        assert!(schedule.is_due("m1", t0 + Duration::from_secs(61)));
        // many failures: capped
        for _ in 0..20 {
            schedule.failed("m1", t0);
        }
        assert!(!schedule.is_due("m1", t0 + REMOTE_CLOSE_MAX_BACKOFF - Duration::from_secs(1)));
        assert!(schedule.is_due("m1", t0 + REMOTE_CLOSE_MAX_BACKOFF));
        schedule.succeeded("m1");
        assert!(schedule.is_due("m1", t0));
    }

    /// Contract test against the real Job-Dispatcher binary. Skipped unless
    /// `HYDRA_UMC_JOB_DISPATCHER_BIN` names a built `hydra-umc-job-dispatcher`;
    /// with it, the whole chain runs: add -> auto-dispatch -> start ->
    /// complete / cancel / recover, checked on the dispatcher's own state.
    #[test]
    fn the_chain_works_against_the_real_job_dispatcher() {
        let Ok(bin) = std::env::var("HYDRA_UMC_JOB_DISPATCHER_BIN") else {
            eprintln!("skipped: HYDRA_UMC_JOB_DISPATCHER_BIN is not set");
            return;
        };
        let _guard = net_test_lock();
        let jd_port = {
            let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            l.local_addr().unwrap().port()
        };
        struct Kill(std::process::Child);
        impl Drop for Kill {
            fn drop(&mut self) {
                let _ = self.0.kill();
            }
        }
        let _jd = Kill(
            std::process::Command::new(bin)
                .args([
                    "-addr",
                    &format!("127.0.0.1:{jd_port}"),
                    "-robot-timeout",
                    "0",
                ])
                .stdout(std::process::Stdio::null())
                .spawn()
                .expect("the dispatcher binary must start"),
        );
        let jd = format!("http://127.0.0.1:{jd_port}");
        for _ in 0..50 {
            if TcpStream::connect(("127.0.0.1", jd_port)).is_ok() {
                break;
            }
            thread::sleep(std::time::Duration::from_millis(100));
        }
        let jd_post = |path: &str, body: &str| {
            let (status, text) = request(jd_port, "POST", path, body);
            assert!(status < 300, "{path}: {status} {text}");
        };
        jd_post("/robots", r#"{"id":"arm-1","available":true}"#);
        jd_post("/robots", r#"{"id":"arm-2","available":true}"#);

        let port = start_test_server_with_job_dispatcher(Some(jd.clone()));
        let job_status = |id: &str| get_url(jd_port, &format!("/jobs/{id}"));
        // 1. the whole happy path frees the robot
        post(port, "/missions", r#"{"id":"m1"}"#);
        post(port, "/missions/m1/auto-dispatch", "");
        post(port, "/missions/m1/start", "");
        let (_, done) = post(port, "/missions/m1/complete", "");
        assert!(done.contains("\"remote_close_confirmed\":true"), "{done}");
        assert!(
            job_status("m1").contains("\"done\""),
            "{}",
            job_status("m1")
        );
        let robots = get_url(jd_port, "/robots");
        assert!(!robots.contains("\"Available\":false"), "{robots}");

        // 2. cancelling a queued mission withdraws its job
        post(port, "/missions", r#"{"id":"m2"}"#);
        post(port, "/missions/m2/cancel", "");
        assert!(
            job_status("m2").contains("cancelled"),
            "{}",
            job_status("m2")
        );

        // 3. a node declared down gets its job back in the queue, and the robot is held
        post(port, "/missions", r#"{"id":"m3"}"#);
        post(port, "/missions/m3/auto-dispatch", "");
        let (_, m3) = get(port, "/missions/m3");
        let node = if m3.contains("arm-1") {
            "arm-1"
        } else {
            "arm-2"
        };
        let (status, body) = post(port, &format!("/nodes/{node}/recover"), "");
        assert_eq!(status, 200, "{body}");
        assert!(
            job_status("m3").contains("\"pending\""),
            "{}",
            job_status("m3")
        );
        let (_, assigned) = post(port, "/missions/m3/auto-dispatch", "");
        assert!(
            assigned.contains("\"assigned\":true") && !assigned.contains(node),
            "the job must go to the other robot: {assigned}"
        );
    }

    fn get_url(port: u16, path: &str) -> String {
        request(port, "GET", path, "").1
    }
}
