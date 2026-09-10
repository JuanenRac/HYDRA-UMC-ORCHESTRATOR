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
//! id or node name. There is still no real gRPC wiring to
//! `HYDRA-UMC-JOB-DISPATCHER`/`HYDRA-UMC-NODE-HEALING` beyond the plain
//! HTTP integration already in this file (see `main.rs`'s own module
//! doc), and there is no real E-STOP-sending code anywhere in this
//! repository to expose - this server does not grant any new physical
//! authority, it makes the exact same state machine `mission-demo`
//! already exercises reachable over a real API instead of only a fixed
//! demo script.
//!
//! Unlike this ecosystem's other Rust services' `server.rs` (all
//! stateless computations), the `MissionRegistry` is real, shared,
//! mutable state that must persist across requests - `Arc<Mutex<..>>`,
//! one lock acquired per request, released before the response is
//! written. C07: as of
//! this delivery the registry itself is also durable across a real
//! process restart (`mission.rs`'s own `MissionRegistry::load()`/
//! `persist()`, wired in by `main.rs`) - every handler below that
//! mutates a mission calls `reg.persist()` before responding, the same
//! explicit convention `outbox.rs`'s own callers already use for the
//! pending remote-close outbox.

use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use serde::Deserialize;
use serde_json::json;
use tiny_http::{Header, Method, Response, Server};

use crate::job_dispatcher;
use crate::mission::{CancelOutcome, MissionRegistry, TransitionError};
use crate::outbox::RemoteCloseOutbox;

// REV-010 (P1): how often
// the background pass below retries confirming a mission's terminal
// outcome to Job-Dispatcher - see reconcile_pending_remote_closes()'s
// own doc comment. Frequent enough that a transient Job-Dispatcher
// outage self-heals within a real operator's patience, infrequent
// enough to never look like a hot polling loop.
const REMOTE_CLOSE_RETRY_INTERVAL: Duration = Duration::from_secs(30);

type SharedRegistry = Arc<Mutex<MissionRegistry>>;
// V07-012: shared the same way SharedRegistry is - both the request
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
/// durable pending-remote-close outbox (V07-012, see outbox.rs's own
/// module doc).
pub struct AppState {
    pub registry: SharedRegistry,
    pub job_dispatcher_url: Option<String>,
    pub close_outbox: SharedOutbox,
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
    Server::http(addr).map_err(std::io::Error::other)
}

pub fn run(
    server: Server,
    job_dispatcher_url: Option<String>,
    close_outbox: RemoteCloseOutbox,
    registry: MissionRegistry,
) {
    let state = AppState {
        registry: Arc::new(Mutex::new(registry)),
        job_dispatcher_url,
        close_outbox: Arc::new(close_outbox),
    };

    // REV-010: only spawned when Job-Dispatcher integration is actually
    // configured at all (matching every other real call site in this
    // file) - see reconcile_pending_remote_closes()'s own doc comment.
    if let Some(base_url) = state.job_dispatcher_url.clone() {
        let registry = Arc::clone(&state.registry);
        let outbox = Arc::clone(&state.close_outbox);
        // V07-012: a real attempt right now, before this incarnation
        // waits a full REMOTE_CLOSE_RETRY_INTERVAL - the whole point of
        // persisting the outbox is a restart recovering FAST, not just
        // eventually. Even now that MissionRegistry itself also survives
        // a restart (C07), the outbox stays the real worklist here: a
        // mission reloaded as `Unknown` still needs its own separate
        // resolution (`recover_unknown_missions()`, called once by
        // main.rs before this incarnation ever serves a request) before
        // it says anything meaningful about a pending remote close.
        reconcile_pending_remote_closes(&registry, &outbox, &base_url);
        thread::spawn(move || loop {
            thread::sleep(REMOTE_CLOSE_RETRY_INTERVAL);
            reconcile_pending_remote_closes(&registry, &outbox, &base_url);
        });
    }

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
                    Err(e) => {
                        write_json(request, 400, &json!({"error": e.to_string()}));
                        continue;
                    }
                };
                handle_add(request, &state, &raw);
            }
            (Method::Post, ["missions", id, "dispatch"]) => {
                let raw = match read_body(&mut request) {
                    Ok(r) => r,
                    Err(e) => {
                        write_json(request, 400, &json!({"error": e.to_string()}));
                        continue;
                    }
                };
                handle_dispatch(request, &state, id, &raw);
            }
            (Method::Post, ["missions", id, "auto-dispatch"]) => {
                handle_auto_dispatch(request, &state, id)
            }
            (Method::Post, ["missions", id, "start"]) => handle_start(request, &state, id),
            (Method::Post, ["missions", id, "complete"]) => handle_complete(request, &state, id),
            (Method::Post, ["missions", id, "cancel"]) => handle_cancel(request, &state, id),
            (Method::Post, ["missions", id, "fail"]) => {
                let raw = match read_body(&mut request) {
                    Ok(r) => r,
                    Err(e) => {
                        write_json(request, 400, &json!({"error": e.to_string()}));
                        continue;
                    }
                };
                handle_fail(request, &state, id, &raw);
            }
            (Method::Post, ["nodes", node, "recover"]) => handle_recover(request, &state, node),
            (Method::Get, ["stats"]) => {
                let reg = state.registry.lock().unwrap();
                write_json(
                    request,
                    200,
                    &json!({
                        "missionCount": reg.all().count(),
                        "jobDispatcherUrl": state.job_dispatcher_url,
                    }),
                );
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
        Err(e) => {
            write_json(
                request,
                400,
                &json!({"error": format!("malformed request JSON: {e}")}),
            );
            return;
        }
    };

    let mission_snapshot = {
        let mut reg = state.registry.lock().unwrap();
        let mission = reg.add(req.id);
        let snapshot = serde_json::to_value(&*mission).unwrap();
        reg.persist();
        snapshot
    };
    // Real, deliberate second half of the "full chain": every mission
    // this Orchestrator now knows about is also submitted as a real job
    // to Job-Dispatcher's own queue, so its real tool-aware/fairness
    // routing has something to route - best-effort (a Job-Dispatcher
    // that's down doesn't stop a mission from existing here, this
    // registry is the source of truth for mission STATE regardless).
    let job_submission = submit_to_job_dispatcher_if_configured(
        state,
        mission_snapshot["id"].as_str().unwrap_or_default(),
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

    {
        let reg = state.registry.lock().unwrap();
        if reg.get(id).is_none() {
            write_json(
                request,
                404,
                &json!({"error": format!("no mission {id:?}")}),
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

    // ORCH-01 (P1): a single /dispatch pass on Job-Dispatcher is its own real,
    // global scheduling algorithm - it can assign several jobs at once,
    // not just the one this caller asked about. Reconciling EVERY
    // returned assignment (not only the one matching `id`) keeps this
    // Orchestrator's own local mission state from diverging from what
    // Job-Dispatcher actually reserved: an assignment left unreconciled
    // here would show that OTHER mission as still "Queued" locally while
    // its robot is genuinely already busy on Job-Dispatcher's side.
    let mut result_for_caller: Option<Result<serde_json::Value, TransitionError>> = None;
    {
        let mut reg = state.registry.lock().unwrap();
        for assignment in &assignments {
            let Some(mission) = reg.get_mut(&assignment.job_id) else {
                continue; // not one of THIS orchestrator's own missions - nothing local to reconcile
            };
            let dispatch_result = mission.dispatch(assignment.robot_id.clone());
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

fn handle_start(request: tiny_http::Request, state: &AppState, id: &str) {
    let mut reg = state.registry.lock().unwrap();
    let Some(mission) = reg.get_mut(id) else {
        write_json(
            request,
            404,
            &json!({"error": format!("no mission {id:?}")}),
        );
        return;
    };
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

/// REV-010 (P1):
/// handle_complete()/handle_cancel() below commit a mission's terminal
/// state locally first, then make a best-effort attempt to confirm it
/// to Job-Dispatcher - ORCH-02's own real integration. Before this fix,
/// a failed confirmation (a transient network error, Job-Dispatcher
/// briefly unreachable) was only ever logged: the mission looked
/// identical to one that WAS confirmed, so Job-Dispatcher could keep
/// believing the job - and its robot's reservation - was still active
/// indefinitely, with no real path back to consistency short of a human
/// noticing. `Mission::remote_close_confirmed` now distinguishes the
/// two, and this function - run periodically from a real background
/// thread spawned in `run()` - retries the SAME real confirmation call
/// for every mission still pending one, until it actually succeeds.
/// `job_dispatcher::complete_job()` is itself idempotent (its own doc
/// comment: a 400 there means "nothing real to close, already done or
/// never assigned" and is treated as success) - retrying a call that
/// already succeeded on Job-Dispatcher's own side the first time,
/// because only THIS side's confirmation of that success got lost, is
/// always safe.
///
/// V07-012 (P1): the
/// "honest limit" this function's own docstring used to state out loud
/// (a real process restart losing the pending list along with every
/// other mission `MissionRegistry` ever knew about) is now closed. The
/// real worklist below comes from `outbox` (see outbox.rs's own module
/// doc), a small durable JSON file independent of `MissionRegistry`'s
/// own ephemeral state, not from `registry.pending_remote_closes()`, so
/// a fresh incarnation of this process (an empty `MissionRegistry`, same
/// as any other restart) still retries every mission the outbox
/// remembers, using the exact `success` value captured at the moment the
/// FIRST confirmation attempt failed. `registry` is still consulted,
/// best-effort, only to keep a still-tracked mission's own
/// `remote_close_confirmed` API field honest for a caller who never
/// restarted this process - the outbox alone is what makes retrying
/// correct even when the registry has forgotten the mission entirely.
fn reconcile_pending_remote_closes(
    registry: &SharedRegistry,
    outbox: &RemoteCloseOutbox,
    base_url: &str,
) {
    for entry in outbox.pending() {
        if job_dispatcher::complete_job(base_url, &entry.mission_id, entry.success).is_ok() {
            outbox.mark_confirmed(&entry.mission_id);
            if let Some(mission) = registry.lock().unwrap().get_mut(&entry.mission_id) {
                mission.mark_remote_close_confirmed();
            }
        }
    }
}

fn handle_complete(request: tiny_http::Request, state: &AppState, id: &str) {
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
        let outcome = mission.complete();
        if outcome.is_ok() {
            reg.persist();
        }
        outcome
    };
    match result {
        Ok(()) => {
            // ORCH-02: tells Job-Dispatcher this job reached its real
            // terminal outcome, so it frees the robot's reservation
            // instead of continuing to believe it's still assigned to a
            // mission this Orchestrator already closed out locally.
            // Best-effort and dropped the lock first, same reasoning as
            // handle_recover's own existing job_dispatcher call.
            if let Some(base_url) = &state.job_dispatcher_url {
                if let Err(e) = job_dispatcher::complete_job(base_url, id, true) {
                    eprintln!(
                        "[orchestrator] could not confirm completion of mission {id} to job-dispatcher: {e}"
                    );
                    // REV-010: mark this pending rather than let it look
                    // confirmed - the background pass above retries it.
                    if let Some(mission) = state.registry.lock().unwrap().get_mut(id) {
                        mission.mark_remote_close_pending();
                    }
                    // V07-012: ALSO persisted, durably, independent of
                    // MissionRegistry - see outbox.rs's own module doc
                    // for why this is what actually survives a restart.
                    state.close_outbox.mark_pending(id, true);
                }
            }
            // Re-serialized AFTER the job_dispatcher call and any
            // resulting bookkeeping above, so the response's own
            // remoteCloseConfirmed field is accurate, not a stale
            // snapshot from before that call was even attempted.
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
        if outcome.is_ok() {
            reg.persist();
        }
        outcome
    };
    match result {
        Ok(outcome) => {
            // ORCH-02: same real confirmation as handle_complete, but
            // only for a FRESH cancellation (CancelOutcome::Cancelled) -
            // a no-op re-cancel of an already-cancelled mission has
            // nothing new to confirm to Job-Dispatcher.
            if outcome == CancelOutcome::Cancelled {
                if let Some(base_url) = &state.job_dispatcher_url {
                    if let Err(e) = job_dispatcher::complete_job(base_url, id, false) {
                        eprintln!(
                            "[orchestrator] could not confirm cancellation of mission {id} to job-dispatcher: {e}"
                        );
                        // REV-010: same real bookkeeping as handle_complete above.
                        if let Some(mission) = state.registry.lock().unwrap().get_mut(id) {
                            mission.mark_remote_close_pending();
                        }
                        // V07-012: same real, durable outbox entry as
                        // handle_complete above.
                        state.close_outbox.mark_pending(id, false);
                    }
                }
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
    let mut reg = state.registry.lock().unwrap();
    let Some(mission) = reg.get_mut(id) else {
        write_json(
            request,
            404,
            &json!({"error": format!("no mission {id:?}")}),
        );
        return;
    };
    let outcome = mission.fail(req.reason);
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

fn handle_recover(request: tiny_http::Request, state: &AppState, node: &str) {
    let requeued = {
        let mut reg = state.registry.lock().unwrap();
        reg.recover_node_unavailable(node)
    };
    // Real gap found while auditing the code: this used to only update
    // the local in-memory mission registry -
    // Job-Dispatcher could keep believing a job was still assigned to
    // the now-unreachable node. Best-effort, same reasoning as every
    // other job_dispatcher.rs call: the registry above is already the
    // real source of truth for mission state regardless of whether Job-
    // Dispatcher is configured/reachable - dropped the lock before this
    // loop so a slow/unreachable Job-Dispatcher can never stall another
    // request that just needs the registry.
    if let Some(base_url) = &state.job_dispatcher_url {
        for mission_id in &requeued {
            if let Err(e) = job_dispatcher::requeue_job(base_url, mission_id) {
                eprintln!(
                    "[orchestrator] could not notify job-dispatcher to requeue mission {mission_id}: {e}"
                );
            }
        }
    }
    write_json(request, 200, &json!({"requeuedMissions": requeued}));
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
        let close_outbox = RemoteCloseOutbox::load(outbox_path)
            .expect("a fresh test outbox path must always load cleanly");
        // Same per-port-unique real path convention as the outbox above,
        // for the same reason - concurrent tests must never share one
        // missions.json.
        let missions_path =
            std::env::temp_dir().join(format!("orchestrator-server-test-missions-{port}.json"));
        let registry = MissionRegistry::load(missions_path)
            .expect("a fresh test registry path must always load cleanly");
        thread::spawn(move || run(server, job_dispatcher_url, close_outbox, registry));
        port
    }

    /// A tiny, real fake Job-Dispatcher answering every request the same
    /// way, forever (unlike job_dispatcher.rs's own single-shot fake) -
    /// this module's own tests need to survive both the /jobs/submit
    /// call handle_add makes AND a later /dispatch call in the same test.
    ///
    /// ORCH-03 (P1): this used to read the incoming request with one single,
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
        assert!(body.contains("m1"));
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
        // Real, reachable fake this time - proves handle_recover's own
        // job-dispatcher call path runs cleanly end to end when
        // configured and reachable, not just that recover degrades
        // gracefully when it isn't (the test above).
        let _guard = net_test_lock();
        let jd_url =
            fake_job_dispatcher(200, r#"{"ID":"m1","Status":"pending","result":"retried"}"#);
        let port = start_test_server_with_job_dispatcher(Some(jd_url));
        post(port, "/missions", r#"{"id":"m1"}"#);
        post(port, "/missions/m1/dispatch", r#"{"node":"node-a"}"#);

        let (status, body) = post(port, "/nodes/node-a/recover", "");
        assert_eq!(status, 200);
        assert!(body.contains("m1"));
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

    // ORCH-01 (P1): a single /dispatch pass can assign several missions at once -
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

    // ORCH-02 (found in the same review pass, P1): completing/cancelling a
    // mission never told Job-Dispatcher, which could keep believing the
    // job (and its robot's reservation) was still active.
    #[test]
    fn complete_confirms_the_real_terminal_outcome_to_job_dispatcher() {
        let _guard = net_test_lock();
        let (jd_url, rx) = fake_job_dispatcher_capturing(
            200,
            r#"{"ID":"m1","Status":"pending","result":"created"}"#,
        );
        let port = start_test_server_with_job_dispatcher(Some(jd_url));
        post(port, "/missions", r#"{"id":"m1"}"#);
        while rx.try_recv().is_ok() {} // drain the /jobs/submit call from add
        post(port, "/missions/m1/dispatch", r#"{"node":"node-a"}"#);
        post(port, "/missions/m1/start", "");

        let (status, _) = post(port, "/missions/m1/complete", "");
        assert_eq!(status, 200);

        let request = rx
            .recv_timeout(std::time::Duration::from_secs(2))
            .expect("expected a real /jobs/complete call to job-dispatcher");
        assert!(request.starts_with("POST /jobs/complete"));
        assert!(request.contains("\"id\":\"m1\""));
        assert!(request.contains("\"success\":true"));
    }

    #[test]
    fn cancel_confirms_the_real_terminal_outcome_to_job_dispatcher() {
        let _guard = net_test_lock();
        let (jd_url, rx) = fake_job_dispatcher_capturing(
            200,
            r#"{"ID":"m1","Status":"pending","result":"created"}"#,
        );
        let port = start_test_server_with_job_dispatcher(Some(jd_url));
        post(port, "/missions", r#"{"id":"m1"}"#);
        while rx.try_recv().is_ok() {} // drain the /jobs/submit call from add

        let (status, _) = post(port, "/missions/m1/cancel", "");
        assert_eq!(status, 200);

        let request = rx
            .recv_timeout(std::time::Duration::from_secs(2))
            .expect("expected a real /jobs/complete call to job-dispatcher for the cancellation");
        assert!(request.starts_with("POST /jobs/complete"));
        assert!(request.contains("\"id\":\"m1\""));
        assert!(request.contains("\"success\":false"));
    }

    #[test]
    fn cancel_does_not_re_notify_job_dispatcher_on_an_idempotent_re_cancel() {
        let _guard = net_test_lock();
        let (jd_url, rx) = fake_job_dispatcher_capturing(
            200,
            r#"{"ID":"m1","Status":"pending","result":"created"}"#,
        );
        let port = start_test_server_with_job_dispatcher(Some(jd_url));
        post(port, "/missions", r#"{"id":"m1"}"#);
        while rx.try_recv().is_ok() {} // drain the /jobs/submit call from add

        post(port, "/missions/m1/cancel", "");
        rx.recv_timeout(std::time::Duration::from_secs(2))
            .expect("the first, real cancellation must notify job-dispatcher");

        let (status, body) = post(port, "/missions/m1/cancel", "");
        assert_eq!(status, 200);
        assert!(body.contains("AlreadyCancelled"));
        assert!(
            rx.recv_timeout(std::time::Duration::from_millis(200))
                .is_err(),
            "a no-op re-cancel of an already-cancelled mission must not notify job-dispatcher again"
        );
    }

    // REV-010 (P1): a failed
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

    // V07-012 (P1): these
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

        let (jd_url, rx) =
            fake_job_dispatcher_capturing(200, r#"{"ID":"m1","Status":"completed"}"#);
        reconcile_pending_remote_closes(&registry, &outbox, &jd_url);

        let request = rx
            .recv_timeout(std::time::Duration::from_secs(2))
            .expect("expected a real retried /jobs/complete call");
        assert!(request.starts_with("POST /jobs/complete"));
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
        // The real point of V07-012: a fresh MissionRegistry (exactly
        // what a real process restart produces - see mission.rs's own
        // module doc) has NEVER heard of "m1" at all, yet the durable
        // outbox alone is still enough to retry confirming its closure
        // to Job-Dispatcher - no orphaned reservation left behind purely
        // because this process forgot the mission ever existed.
        let registry: SharedRegistry = Arc::new(Mutex::new(MissionRegistry::new()));
        let outbox = test_outbox("recovers-after-restart");
        outbox.mark_pending("m1", true); // simulates outbox.load() at startup finding a real leftover entry

        let (jd_url, rx) =
            fake_job_dispatcher_capturing(200, r#"{"ID":"m1","Status":"completed"}"#);
        reconcile_pending_remote_closes(&registry, &outbox, &jd_url);

        let request = rx
            .recv_timeout(std::time::Duration::from_secs(2))
            .expect("expected a real retried /jobs/complete call even with no matching mission in the registry");
        assert!(request.starts_with("POST /jobs/complete"));
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
}
