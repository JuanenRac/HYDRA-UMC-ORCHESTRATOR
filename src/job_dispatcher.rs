// =============================================================================
// HYDRA-UMC-ORCHESTRATOR - src/job_dispatcher.rs
// Copyright (C) 2026 JuanenRac (Electro Hobby 3D) <electrohobby3d@gmail.com>
// GPL-3.0 - see LICENSE
// =============================================================================
//! A real, minimal client for HYDRA-UMC-JOB-DISPATCHER's own real HTTP API
//! (`docs/API.md` in that repo - `POST /jobs/submit`, `POST /dispatch`),
//! the second half of the "full chain" this project's own `server.rs`
//! wires together: a mission added here is also submitted as a real job
//! there, and a real dispatch pass there is what actually decides which
//! robot a mission is assigned to - `mission.rs`'s own state machine has
//! no routing/matching logic of its own, it only ever recorded whatever
//! node a caller told it to (see that module's own docs).

use serde::{Deserialize, Serialize};

#[derive(Serialize)]
struct SubmitRequest<'a> {
    id: &'a str,
    #[serde(rename = "dedupKey")]
    dedup_key: &'a str,
}

#[derive(Serialize)]
struct CompleteRequest<'a> {
    id: &'a str,
    success: bool,
}

#[derive(Debug, Deserialize)]
pub struct Assignment {
    #[serde(rename = "JobID")]
    pub job_id: String,
    #[serde(rename = "RobotID")]
    pub robot_id: String,
}

/// V07-012 (P1): the real
/// shape `GET /jobs` reports for one job - only the two fields
/// `complete_job()`'s own 400-disambiguation below actually needs.
#[derive(Debug, Deserialize)]
struct JobStatusView {
    #[serde(rename = "ID")]
    id: String,
    #[serde(rename = "Status")]
    status: String,
}

/// Real error a caller can act on distinctly: `Unreachable` (Job-
/// Dispatcher isn't up - a mission still exists here regardless, this
/// integration is best-effort, not a hard dependency) vs. `BadResponse`
/// (Job-Dispatcher answered, but not with something this client can
/// parse - a real contract mismatch worth surfacing differently).
#[derive(Debug)]
pub enum ClientError {
    Unreachable(String),
    BadResponse(String),
}

impl std::fmt::Display for ClientError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ClientError::Unreachable(e) => write!(f, "job-dispatcher unreachable: {e}"),
            ClientError::BadResponse(e) => {
                write!(f, "job-dispatcher returned an unexpected response: {e}")
            }
        }
    }
}

/// Submits `mission_id` as a real job to Job-Dispatcher's queue, using
/// the mission id as both the job id and the dedup key - a retried
/// POST /missions call (this project's own caller, on a timeout) must
/// never double-submit the same mission as two separate jobs there.
/// Best-effort: a 200/201/409 (already submitted) all count as success
/// here, since the real intent - "this mission is now in the queue" -
/// is satisfied either way.
pub fn submit_job(base_url: &str, mission_id: &str) -> Result<(), ClientError> {
    let url = format!("{}/jobs/submit", base_url.trim_end_matches('/'));
    let body = SubmitRequest {
        id: mission_id,
        dedup_key: mission_id,
    };
    let result = ureq::post(&url)
        .set("Content-Type", "application/json")
        .send_string(
            &serde_json::to_string(&body).map_err(|e| ClientError::BadResponse(e.to_string()))?,
        );

    match result {
        Ok(_) => Ok(()),
        // ureq treats 409 as an Err(Status(..)) - Job-Dispatcher's own
        // real "job ID already exists" response for a job this exact
        // mission id already submitted successfully before.
        Err(ureq::Error::Status(409, _)) => Ok(()),
        Err(ureq::Error::Status(code, resp)) => Err(ClientError::BadResponse(format!(
            "HTTP {code}: {}",
            resp.into_string().unwrap_or_default()
        ))),
        Err(ureq::Error::Transport(t)) => Err(ClientError::Unreachable(t.to_string())),
    }
}

/// Tells Job-Dispatcher a mission's already-dispatched job needs
/// redistributing - found while auditing the code: `handle_recover()` in
/// `server.rs` only ever updated the local
/// in-memory mission registry, never this real integration, so Job-
/// Dispatcher could keep believing a job was still assigned to a node
/// that just went unreachable.
///
/// Uses Job-Dispatcher's own existing, documented contract - no new
/// endpoint needed on that side (see that repo's own `docs/API.md`):
/// `POST /jobs/complete {success: false}` marks the job "failed" (only
/// valid from the real "assigned" state - exactly the state a job whose
/// robot just went unreachable should be in), then `POST /jobs/submit`
/// with the SAME `dedupKey` `submit_job()` already used for this mission
/// (its own id) hits the real, documented "retried" path: Job-Dispatcher
/// resets the job to "pending" under its original id, making it eligible
/// for the next real `POST /dispatch` pass to assign to a different,
/// healthy robot.
///
/// Best-effort, same reasoning as `submit_job()`: a 400 from
/// `/jobs/complete` means this mission's job was never actually in the
/// "assigned" state on Job-Dispatcher's side (never submitted there at
/// all, or already finished on its own) - nothing real to requeue, not a
/// failure of this call.
/// Tells Job-Dispatcher a mission's job reached a real terminal outcome -
/// `POST /jobs/complete {id, success}`, the same documented endpoint
/// `requeue_job` below already uses for its own first step. Best-effort,
/// same reasoning as every other call here: a 400 means this mission's
/// job was never actually "assigned" on Job-Dispatcher's side (never
/// submitted at all, or already finished on its own) - nothing real to
/// close out there, not a failure of this call.
///
/// ORCH-02 (P1):
/// `server.rs`'s own `handle_complete`/`handle_cancel` used to update
/// only the local mission registry - Job-Dispatcher could keep believing
/// a job (and its robot's reservation) was still active for a mission
/// this Orchestrator had already closed out locally.
///
/// V07-012 (P1): a 400 from
/// `POST /jobs/complete` is genuinely AMBIGUOUS on Job-Dispatcher's own
/// side - `handleCompleteJob` returns the exact same status for "this
/// job id was never even submitted", "this job already reached done/
/// failed", AND (were a real bug elsewhere to ever leave a job's
/// `AssignedRobot` pointing at a robot record that no longer exists) a
/// genuine internal inconsistency - collapsing all of those into one
/// blind `Ok(())` used to let a REAL failure (the job is still sitting
/// there `assigned`, a real orphaned reservation) look identical to an
/// already-closed one. A 400 now triggers a real `GET /jobs` lookup of
/// this exact mission's own current status before deciding: still
/// `assigned` is a real, reported failure (nothing was actually
/// released); anything else found (or not found at all) is the honest
/// "nothing real left to close" this function already promised; and a
/// remote state that can't even be verified (the lookup itself fails)
/// fails closed rather than guessing.
pub fn complete_job(base_url: &str, mission_id: &str, success: bool) -> Result<(), ClientError> {
    let base_url = base_url.trim_end_matches('/');
    let complete_url = format!("{base_url}/jobs/complete");
    let complete_body = CompleteRequest {
        id: mission_id,
        success,
    };
    let complete_result = ureq::post(&complete_url)
        .set("Content-Type", "application/json")
        .send_string(
            &serde_json::to_string(&complete_body)
                .map_err(|e| ClientError::BadResponse(e.to_string()))?,
        );

    match complete_result {
        Ok(_) => Ok(()),
        Err(ureq::Error::Status(400, _)) => match fetch_job_status(base_url, mission_id)? {
            None => Ok(()), // never existed there at all - nothing real to close
            Some(status) if status == "assigned" => Err(ClientError::BadResponse(format!(
                "job {mission_id:?} is still reported 'assigned' on job-dispatcher - \
                 the 400 from /jobs/complete was NOT a benign already-closed state"
            ))),
            Some(_) => Ok(()), // done/failed/pending/blocked/unreachable - already terminal or never dispatched
        },
        Err(ureq::Error::Status(code, resp)) => Err(ClientError::BadResponse(format!(
            "HTTP {code}: {}",
            resp.into_string().unwrap_or_default()
        ))),
        Err(ureq::Error::Transport(t)) => Err(ClientError::Unreachable(t.to_string())),
    }
}

/// V07-012: looks up `mission_id`'s own real, current status directly
/// from Job-Dispatcher's `GET /jobs` listing - the real remote-state
/// query `complete_job()` above uses to disambiguate an otherwise-opaque
/// 400 instead of blindly trusting it means "already closed". `Ok(None)`
/// means the job genuinely is not in the list at all (never submitted,
/// or Job-Dispatcher itself lost its own state) - itself a real, honest
/// fact, not an error; a real `Err` here (the lookup itself fails) is
/// what makes `complete_job()` fail closed instead of guessing.
fn fetch_job_status(base_url: &str, mission_id: &str) -> Result<Option<String>, ClientError> {
    let url = format!("{base_url}/jobs");
    let result = ureq::get(&url).call();
    match result {
        Ok(response) => {
            let text = response
                .into_string()
                .map_err(|e| ClientError::BadResponse(e.to_string()))?;
            let jobs: Vec<JobStatusView> =
                serde_json::from_str(&text).map_err(|e| ClientError::BadResponse(e.to_string()))?;
            Ok(jobs
                .into_iter()
                .find(|j| j.id == mission_id)
                .map(|j| j.status))
        }
        Err(ureq::Error::Status(code, resp)) => Err(ClientError::BadResponse(format!(
            "HTTP {code}: {}",
            resp.into_string().unwrap_or_default()
        ))),
        Err(ureq::Error::Transport(t)) => Err(ClientError::Unreachable(t.to_string())),
    }
}

/// Requeues mission_id's job onto a DIFFERENT robot: marks it Failed via
/// the same `/jobs/complete` endpoint `complete_job` above uses, then
/// re-submits it under its own dedup key so Job-Dispatcher's real
/// "retried" path resets it to `Pending` for a fresh dispatch attempt.
///
/// Deliberately NOT built on top of `complete_job` above, despite the
/// shared first request: a 400 here means "stop - there is nothing real
/// to requeue" (this mission's job was never in Job-Dispatcher's real
/// "assigned" state to begin with, so re-submitting would be a guess,
/// not a real requeue), while `complete_job`'s own callers (a mission
/// simply finishing) have no such follow-up step and want a 400 treated
/// as the same benign no-op either way. Collapsing both into one shared
/// helper previously made a 400 during `complete_job` silently continue
/// on to `submit_job` too - a real regression a dedicated test below now
/// guards against.
pub fn requeue_job(base_url: &str, mission_id: &str) -> Result<(), ClientError> {
    let base_url = base_url.trim_end_matches('/');
    let complete_url = format!("{base_url}/jobs/complete");
    let complete_body = CompleteRequest {
        id: mission_id,
        success: false,
    };
    let complete_result = ureq::post(&complete_url)
        .set("Content-Type", "application/json")
        .send_string(
            &serde_json::to_string(&complete_body)
                .map_err(|e| ClientError::BadResponse(e.to_string()))?,
        );

    match complete_result {
        Ok(_) => {}
        Err(ureq::Error::Status(400, _)) => return Ok(()),
        Err(ureq::Error::Status(code, resp)) => {
            return Err(ClientError::BadResponse(format!(
                "HTTP {code}: {}",
                resp.into_string().unwrap_or_default()
            )))
        }
        Err(ureq::Error::Transport(t)) => return Err(ClientError::Unreachable(t.to_string())),
    }

    submit_job(base_url, mission_id)
}

/// Runs one real dispatch pass on Job-Dispatcher and returns every
/// `Assignment` it made this call - the real routing decision (tool-
/// aware matching, fairness by `Load`) this project's own `mission.rs`
/// has no logic for itself.
pub fn run_dispatch(base_url: &str) -> Result<Vec<Assignment>, ClientError> {
    let url = format!("{}/dispatch", base_url.trim_end_matches('/'));
    let result = ureq::post(&url).call();

    match result {
        // into_string() (always available) + serde_json::from_str(),
        // not Response::into_json() - that helper needs ureq's own
        // optional "json" feature, which this crate deliberately leaves
        // off (see Cargo.toml's own comment: default-features = false,
        // no TLS needed for loopback-only HTTP) - real compile error
        // found live, not guessed at.
        Ok(response) => {
            let text = response
                .into_string()
                .map_err(|e| ClientError::BadResponse(e.to_string()))?;
            serde_json::from_str::<Vec<Assignment>>(&text)
                .map_err(|e| ClientError::BadResponse(e.to_string()))
        }
        Err(ureq::Error::Status(code, resp)) => Err(ClientError::BadResponse(format!(
            "HTTP {code}: {}",
            resp.into_string().unwrap_or_default()
        ))),
        Err(ureq::Error::Transport(t)) => Err(ClientError::Unreachable(t.to_string())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::thread;

    /// A tiny, real, single-request fake Job-Dispatcher: accepts one
    /// real HTTP connection, reads the real request, replies with a
    /// fixed `status`+`body`, and reports back the raw request text it
    /// received so a test can assert on the real method/path/body this
    /// client actually sent - not just that "something" was sent.
    ///
    /// Reads in a loop rather than trusting one `stream.read()` call to
    /// return the whole request: `ureq`'s `send_string()` can write
    /// headers and body as separate TCP writes, and a loopback stack is
    /// free to deliver those as separate readable chunks - a single
    /// non-looping read intermittently captured only the headers here,
    /// with `Content-Length` correctly declared but the body itself not
    /// there yet. Reads until it has seen `\r\n\r\n` and, if a
    /// `Content-Length` header is present, that many body bytes past it.
    fn fake_server(status: u16, body: &'static str) -> (String, std::sync::mpsc::Receiver<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let (tx, rx) = std::sync::mpsc::channel();

        thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                let request = read_full_request(&mut stream);
                let response = format!(
                    "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = stream.write_all(response.as_bytes());
                let _ = tx.send(request);
            }
        });

        (format!("http://127.0.0.1:{port}"), rx)
    }

    /// Reads one full HTTP/1.1 request (headers + declared body, if any)
    /// off `stream`, tolerating the headers and body arriving as separate
    /// reads - see `fake_server`'s own doc comment for why a single
    /// `read()` isn't reliable here.
    fn read_full_request(stream: &mut std::net::TcpStream) -> String {
        let mut raw = Vec::new();
        let mut buf = [0u8; 4096];
        let header_end = loop {
            let n = stream.read(&mut buf).unwrap_or(0);
            if n == 0 {
                return String::from_utf8_lossy(&raw).to_string();
            }
            raw.extend_from_slice(&buf[..n]);
            if let Some(pos) = find_subslice(&raw, b"\r\n\r\n") {
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

        String::from_utf8_lossy(&raw).to_string()
    }

    fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
        haystack.windows(needle.len()).position(|w| w == needle)
    }

    #[test]
    fn submit_job_sends_a_real_request_with_matching_id_and_dedup_key() {
        let (base_url, rx) =
            fake_server(201, r#"{"ID":"m1","Status":"pending","result":"created"}"#);
        let result = submit_job(&base_url, "m1");
        assert!(result.is_ok());

        let request = rx.recv_timeout(std::time::Duration::from_secs(2)).unwrap();
        assert!(request.starts_with("POST /jobs/submit"));
        assert!(request.contains("\"id\":\"m1\""));
        assert!(request.contains("\"dedupKey\":\"m1\""));
    }

    #[test]
    fn submit_job_treats_409_as_success() {
        let (base_url, _rx) = fake_server(409, r#"{"error":"job ID already exists: \"m1\""}"#);
        let result = submit_job(&base_url, "m1");
        assert!(
            result.is_ok(),
            "a 409 (already submitted) must not be treated as a real failure"
        );
    }

    #[test]
    fn submit_job_reports_unreachable_when_nothing_is_listening() {
        let result = submit_job("http://127.0.0.1:1", "m1");
        assert!(matches!(result, Err(ClientError::Unreachable(_))));
    }

    #[test]
    fn complete_job_succeeds_on_a_real_200() {
        let (base_url, rx) = fake_server(200, r#"{"ID":"m1","Status":"done"}"#);
        let result = complete_job(&base_url, "m1", true);
        assert!(result.is_ok());
        let request = rx.recv_timeout(std::time::Duration::from_secs(2)).unwrap();
        assert!(request.starts_with("POST /jobs/complete"));
        assert!(request.contains("\"success\":true"));
    }

    // V07-012 (P1): a 400
    // from /jobs/complete used to be blindly treated as "already
    // closed" - these four prove it is now disambiguated against
    // Job-Dispatcher's own real GET /jobs status instead of guessed at.

    #[test]
    fn complete_job_treats_a_400_as_benign_when_the_job_does_not_exist_remotely() {
        let (base_url, rx) = fake_server_two_requests(400, r#"{"error":"unknown job"}"#, 200, "[]");
        let result = complete_job(&base_url, "m1", true);
        assert!(
            result.is_ok(),
            "a job job-dispatcher has never heard of has nothing real to close"
        );

        let first = rx.recv_timeout(std::time::Duration::from_secs(2)).unwrap();
        assert!(first.starts_with("POST /jobs/complete"));
        let second = rx.recv_timeout(std::time::Duration::from_secs(2)).unwrap();
        assert!(
            second.starts_with("GET /jobs"),
            "expected a real remote-state lookup, got: {second}"
        );
    }

    #[test]
    fn complete_job_treats_a_400_as_benign_when_the_job_is_already_terminal_remotely() {
        let (base_url, _rx) = fake_server_two_requests(
            400,
            r#"{"error":"job is not assigned"}"#,
            200,
            r#"[{"ID":"m1","Status":"done"}]"#,
        );
        let result = complete_job(&base_url, "m1", true);
        assert!(
            result.is_ok(),
            "a job already done/failed remotely has nothing real to close"
        );
    }

    #[test]
    fn complete_job_refuses_to_treat_a_400_as_success_when_the_job_is_still_assigned_remotely() {
        let (base_url, _rx) = fake_server_two_requests(
            400,
            r#"{"error":"something unexpected"}"#,
            200,
            r#"[{"ID":"m1","Status":"assigned"}]"#,
        );
        let result = complete_job(&base_url, "m1", true);
        assert!(
            result.is_err(),
            "a job job-dispatcher still reports as 'assigned' is a REAL orphaned reservation, not a benign 400"
        );
    }

    #[test]
    fn complete_job_fails_closed_when_the_remote_state_cannot_even_be_verified() {
        // `fake_server` (single-request) closes its listener right after
        // the first request - the follow-up GET /jobs this triggers has
        // nothing left to connect to, a real unreachable failure.
        let (base_url, _rx) = fake_server(400, r#"{"error":"unexpected"}"#);
        let result = complete_job(&base_url, "m1", true);
        assert!(
            result.is_err(),
            "an unverifiable remote state must never be silently treated as a confirmed close"
        );
    }

    /// A tiny, real fake Job-Dispatcher answering TWO sequential requests
    /// (unlike `fake_server`'s own single-request shape) - `requeue_job()`
    /// makes a real `/jobs/complete` call followed by a real
    /// `/jobs/submit` call, and this test needs to observe both.
    fn fake_server_two_requests(
        first_status: u16,
        first_body: &'static str,
        second_status: u16,
        second_body: &'static str,
    ) -> (String, std::sync::mpsc::Receiver<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let (tx, rx) = std::sync::mpsc::channel();

        thread::spawn(move || {
            for (status, body) in [(first_status, first_body), (second_status, second_body)] {
                if let Ok((mut stream, _)) = listener.accept() {
                    let request = read_full_request(&mut stream);
                    let response = format!(
                        "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    );
                    let _ = stream.write_all(response.as_bytes());
                    let _ = tx.send(request);
                }
            }
        });

        (format!("http://127.0.0.1:{port}"), rx)
    }

    #[test]
    fn requeue_job_completes_as_failed_then_resubmits_with_the_same_dedup_key() {
        let (base_url, rx) = fake_server_two_requests(
            200,
            r#"{"ID":"m1","Status":"failed"}"#,
            200,
            r#"{"ID":"m1","Status":"pending","result":"retried"}"#,
        );

        let result = requeue_job(&base_url, "m1");
        assert!(result.is_ok());

        let complete_request = rx.recv_timeout(std::time::Duration::from_secs(2)).unwrap();
        assert!(complete_request.starts_with("POST /jobs/complete"));
        assert!(complete_request.contains("\"id\":\"m1\""));
        assert!(complete_request.contains("\"success\":false"));

        let submit_request = rx.recv_timeout(std::time::Duration::from_secs(2)).unwrap();
        assert!(submit_request.starts_with("POST /jobs/submit"));
        assert!(submit_request.contains("\"id\":\"m1\""));
        assert!(submit_request.contains("\"dedupKey\":\"m1\""));
    }

    #[test]
    fn requeue_job_treats_a_job_not_in_the_assigned_state_as_a_benign_no_op() {
        // Real, expected outcome when this mission's job was never
        // actually dispatched on Job-Dispatcher's side (or already
        // finished on its own) - not a failure of THIS call.
        let (base_url, _rx) = fake_server(400, r#"{"error":"job is not in the assigned state"}"#);
        let result = requeue_job(&base_url, "m1");
        assert!(result.is_ok());
    }

    #[test]
    fn requeue_job_reports_unreachable_when_nothing_is_listening() {
        let result = requeue_job("http://127.0.0.1:1", "m1");
        assert!(matches!(result, Err(ClientError::Unreachable(_))));
    }

    #[test]
    fn run_dispatch_parses_real_assignments() {
        let (base_url, rx) = fake_server(200, r#"[{"JobID":"m1","RobotID":"arm-3"}]"#);
        let result = run_dispatch(&base_url).unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].job_id, "m1");
        assert_eq!(result[0].robot_id, "arm-3");

        let request = rx.recv_timeout(std::time::Duration::from_secs(2)).unwrap();
        assert!(request.starts_with("POST /dispatch"));
    }

    #[test]
    fn run_dispatch_handles_a_real_empty_pass() {
        let (base_url, _rx) = fake_server(200, "[]");
        let result = run_dispatch(&base_url).unwrap();
        assert!(result.is_empty());
    }
}
