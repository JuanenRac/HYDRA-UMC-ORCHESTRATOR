// =============================================================================
// HYDRA-UMC-ORCHESTRATOR - src/job_dispatcher.rs
// Copyright (C) 2026 JuanenRac (Electro Hobby 3D) <electrohobby3d@gmail.com>
// GPL-3.0-or-later - see LICENSE
// =============================================================================
//! A real, minimal client for HYDRA-UMC-JOB-DISPATCHER's own real HTTP API
//! (`docs/API.md` in that repo), the second half of the "full chain" this
//! project's own `server.rs` wires together: a mission added here is also
//! submitted as a real job there, and a real dispatch pass there is what
//! actually decides which robot a mission is assigned to - `mission.rs`'s
//! own state machine has no routing/matching logic of its own, it only ever
//! recorded whatever node a caller told it to (see that module's own docs).
//!
//! Every call goes through one shared agent with a connect and a total
//! timeout: a dispatcher that accepts the connection and never answers
//! cannot hang a worker for ever.

use std::sync::OnceLock;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::mission::JobSpec;

/// Bearer token sent on every call to Job-Dispatcher, set once at startup
/// from `--job-dispatcher-token` / `JOB_DISPATCHER_TOKEN`. Job-Dispatcher
/// refuses to listen on a network address without one, so an orchestrator
/// talking to a dispatcher on another host needs it.
static TOKEN: OnceLock<String> = OnceLock::new();

/// How long a connection attempt may take.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);
/// How long one whole call (connect, send, wait, read) may take.
#[cfg(not(test))]
const CALL_TIMEOUT: Duration = Duration::from_secs(10);
#[cfg(test)]
const CALL_TIMEOUT: Duration = Duration::from_secs(2);

pub fn set_token(token: String) {
    let _ = TOKEN.set(token);
}

fn agent() -> &'static ureq::Agent {
    static AGENT: OnceLock<ureq::Agent> = OnceLock::new();
    AGENT.get_or_init(|| {
        ureq::AgentBuilder::new()
            .timeout_connect(CONNECT_TIMEOUT)
            .timeout(CALL_TIMEOUT)
            .build()
    })
}

fn authed(request: ureq::Request) -> ureq::Request {
    match TOKEN.get() {
        Some(token) => request.set("Authorization", &format!("Bearer {token}")),
        None => request,
    }
}

#[derive(Serialize)]
struct SubmitRequest<'a> {
    id: &'a str,
    #[serde(rename = "dedupKey")]
    dedup_key: &'a str,
    priority: i32,
    #[serde(rename = "requiredTool")]
    required_tool: &'a str,
    location: &'a str,
    #[serde(rename = "dependsOn")]
    depends_on: &'a [String],
}

#[derive(Serialize)]
struct CompleteRequest<'a> {
    id: &'a str,
    success: bool,
    #[serde(rename = "robotId")]
    robot_id: &'a str,
}

#[derive(Serialize)]
struct CancelRequest<'a> {
    id: &'a str,
}

#[derive(Serialize)]
struct RequeueRequest<'a> {
    id: &'a str,
    #[serde(rename = "robotId")]
    robot_id: &'a str,
    reason: &'a str,
}

#[derive(Debug, Deserialize)]
pub struct Assignment {
    #[serde(rename = "JobID")]
    pub job_id: String,
    #[serde(rename = "RobotID")]
    pub robot_id: String,
}

/// The fields of `GET /jobs/{id}` this client needs.
#[derive(Debug, Deserialize)]
struct JobView {
    #[serde(rename = "Status", default)]
    status: String,
    #[serde(rename = "AssignedRobot", default)]
    assigned_robot: String,
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

fn bad_status(code: u16, resp: ureq::Response) -> ClientError {
    ClientError::BadResponse(format!(
        "HTTP {code}: {}",
        resp.into_string().unwrap_or_default()
    ))
}

fn transport(t: ureq::Transport) -> ClientError {
    ClientError::Unreachable(t.to_string())
}

/// Percent-encodes a path segment (a mission id is restricted to
/// `[A-Za-z0-9._-]`, but a stored id from an older file might not be).
fn path_segment(id: &str) -> String {
    let mut out = String::with_capacity(id.len());
    for b in id.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// True when the body of a `404` is Job-Dispatcher's own JSON error
/// (`{"error": ...}`) about an unknown job, as opposed to the plain
/// `404 page not found` an older dispatcher without the route answers.
fn is_json_error(body: &str) -> bool {
    serde_json::from_str::<serde_json::Value>(body)
        .ok()
        .is_some_and(|v| v.get("error").is_some())
}

fn json_body<T: Serialize>(body: &T) -> Result<String, ClientError> {
    serde_json::to_string(body).map_err(|e| ClientError::BadResponse(e.to_string()))
}

/// Submits the mission as a real job to Job-Dispatcher's queue, using
/// the mission id as both the job id and the dedup key - a retried
/// POST /missions call (this project's own caller, on a timeout) must
/// never double-submit the same mission as two separate jobs there.
/// The priority, required tool, preferred location and dependencies the
/// mission was created with travel along, so Job-Dispatcher's tool-aware
/// and multi-stage routing has something to work with.
/// Best-effort: a 200/201/409 (already submitted) all count as success
/// here, since the real intent - "this mission is now in the queue" -
/// is satisfied either way.
pub fn submit_job(base_url: &str, mission_id: &str, spec: &JobSpec) -> Result<(), ClientError> {
    let url = format!("{}/jobs/submit", base_url.trim_end_matches('/'));
    let body = SubmitRequest {
        id: mission_id,
        dedup_key: mission_id,
        priority: spec.priority,
        required_tool: &spec.required_tool,
        location: &spec.location,
        depends_on: &spec.depends_on,
    };
    let result = authed(agent().post(&url))
        .set("Content-Type", "application/json")
        .send_string(&json_body(&body)?);

    match result {
        Ok(_) => Ok(()),
        // ureq treats 409 as an Err(Status(..)) - Job-Dispatcher's own
        // real "job ID already exists" response for a job this exact
        // mission id already submitted successfully before.
        Err(ureq::Error::Status(409, _)) => Ok(()),
        Err(ureq::Error::Status(code, resp)) => Err(bad_status(code, resp)),
        Err(ureq::Error::Transport(t)) => Err(transport(t)),
    }
}

/// Looks up one job on Job-Dispatcher (`GET /jobs/{id}`). `Ok(None)` means
/// the job genuinely is not there (never submitted, or the dispatcher lost
/// its state); a dispatcher too old to have the route is reported as a
/// `BadResponse`, never mistaken for "no such job".
fn fetch_job(base_url: &str, mission_id: &str) -> Result<Option<JobView>, ClientError> {
    let url = format!("{base_url}/jobs/{}", path_segment(mission_id));
    match authed(agent().get(&url)).call() {
        Ok(response) => {
            let text = response
                .into_string()
                .map_err(|e| ClientError::BadResponse(e.to_string()))?;
            serde_json::from_str::<JobView>(&text)
                .map(Some)
                .map_err(|e| ClientError::BadResponse(e.to_string()))
        }
        Err(ureq::Error::Status(404, resp)) => {
            let text = resp.into_string().unwrap_or_default();
            if is_json_error(&text) {
                Ok(None)
            } else {
                Err(ClientError::BadResponse(
                    "GET /jobs/{id} is not served: this job-dispatcher is older than the orchestrator expects"
                        .to_string(),
                ))
            }
        }
        Err(ureq::Error::Status(code, resp)) => Err(bad_status(code, resp)),
        Err(ureq::Error::Transport(t)) => Err(transport(t)),
    }
}

/// Tells Job-Dispatcher a mission reached a real terminal outcome, in the
/// way that matches what Job-Dispatcher holds for that job right now:
///
/// * not there, or already `done`/`failed`/`cancelled`: nothing to close;
/// * `pending`/`blocked`/`unreachable` (nobody was given it): withdrawn
///   with `POST /jobs/cancel`, so a later dispatch pass cannot hand
///   cancelled work to a robot;
/// * `assigned`/`unknown` (a robot holds it): closed with
///   `POST /jobs/complete`, naming the robot Job-Dispatcher itself says
///   holds it, which frees that robot's reservation.
///
/// A conflict from either write (the job moved between the lookup and the
/// write) is an error: the caller keeps the close pending and the next
/// attempt sees the new state. Idempotent, so retrying a close that already
/// went through is always safe.
pub fn close_job(base_url: &str, mission_id: &str, success: bool) -> Result<(), ClientError> {
    let base_url = base_url.trim_end_matches('/');
    let Some(job) = fetch_job(base_url, mission_id)? else {
        return Ok(());
    };
    match job.status.as_str() {
        "done" | "failed" | "cancelled" => Ok(()),
        "assigned" | "unknown" => {
            let body = CompleteRequest {
                id: mission_id,
                success,
                robot_id: &job.assigned_robot,
            };
            let url = format!("{base_url}/jobs/complete");
            match authed(agent().post(&url))
                .set("Content-Type", "application/json")
                .send_string(&json_body(&body)?)
            {
                Ok(_) => Ok(()),
                // the job (or its robot) is gone: nothing left to close
                Err(ureq::Error::Status(404, _)) => Ok(()),
                Err(ureq::Error::Status(code, resp)) => Err(bad_status(code, resp)),
                Err(ureq::Error::Transport(t)) => Err(transport(t)),
            }
        }
        "pending" | "blocked" | "unreachable" => {
            let url = format!("{base_url}/jobs/cancel");
            match authed(agent().post(&url))
                .set("Content-Type", "application/json")
                .send_string(&json_body(&CancelRequest { id: mission_id })?)
            {
                Ok(_) => Ok(()),
                Err(ureq::Error::Status(404, _)) => Ok(()),
                Err(ureq::Error::Status(code, resp)) => Err(bad_status(code, resp)),
                Err(ureq::Error::Transport(t)) => Err(transport(t)),
            }
        }
        other => Err(ClientError::BadResponse(format!(
            "job {mission_id:?} has a status this orchestrator does not know: {other:?}"
        ))),
    }
}

/// Requeues the mission's job onto a DIFFERENT robot after `node` was
/// declared unavailable: `POST /jobs/requeue` puts the job back to
/// `pending` and holds `node`'s robot unavailable until it reports in
/// again, so the same dead robot is not handed the job right back. If
/// Job-Dispatcher no longer knows the job (it restarted without a
/// database), it is submitted again. A job that is not assigned any more
/// is not an error: there is nothing to take away from anybody.
pub fn requeue_job(
    base_url: &str,
    mission_id: &str,
    node: &str,
    spec: &JobSpec,
) -> Result<(), ClientError> {
    let base_url = base_url.trim_end_matches('/');
    let url = format!("{base_url}/jobs/requeue");
    let body = RequeueRequest {
        id: mission_id,
        robot_id: node,
        reason: "node declared unavailable",
    };
    match authed(agent().post(&url))
        .set("Content-Type", "application/json")
        .send_string(&json_body(&body)?)
    {
        Ok(_) => Ok(()),
        Err(ureq::Error::Status(404, resp)) => {
            let text = resp.into_string().unwrap_or_default();
            if is_json_error(&text) {
                // unknown job there: put it back in the queue
                submit_job(base_url, mission_id, spec)
            } else {
                Err(ClientError::BadResponse(
                    "POST /jobs/requeue is not served: this job-dispatcher is older than the orchestrator expects"
                        .to_string(),
                ))
            }
        }
        Err(ureq::Error::Status(code, resp)) => Err(bad_status(code, resp)),
        Err(ureq::Error::Transport(t)) => Err(transport(t)),
    }
}

/// Runs one real dispatch pass on Job-Dispatcher and returns every
/// `Assignment` it made this call - the real routing decision (tool-
/// aware matching, fairness by `Load`) this project's own `mission.rs`
/// has no logic for itself.
pub fn run_dispatch(base_url: &str) -> Result<Vec<Assignment>, ClientError> {
    let url = format!("{}/dispatch", base_url.trim_end_matches('/'));
    let result = authed(agent().post(&url)).call();

    match result {
        // into_string() (always available) + serde_json::from_str(),
        // not Response::into_json() - that helper needs ureq's own
        // optional "json" feature, which this crate deliberately leaves
        // off (see Cargo.toml's own comment: default-features = false,
        // no TLS needed for loopback-only HTTP).
        Ok(response) => {
            let text = response
                .into_string()
                .map_err(|e| ClientError::BadResponse(e.to_string()))?;
            serde_json::from_str::<Vec<Assignment>>(&text)
                .map_err(|e| ClientError::BadResponse(e.to_string()))
        }
        Err(ureq::Error::Status(code, resp)) => Err(bad_status(code, resp)),
        Err(ureq::Error::Transport(t)) => Err(transport(t)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::thread;

    /// A tiny, real fake Job-Dispatcher: accepts one connection per scripted
    /// reply, in order, reads the whole request, answers with the scripted
    /// `(status, body)`, and reports back the raw request text so a test can
    /// assert on the real method/path/body this client actually sent.
    ///
    /// Reads in a loop rather than trusting one `stream.read()` call to
    /// return the whole request: `ureq`'s `send_string()` can write
    /// headers and body as separate TCP writes, and a loopback stack is
    /// free to deliver those as separate readable chunks. Reads until it
    /// has seen `\r\n\r\n` and, if a `Content-Length` header is present,
    /// that many body bytes past it.
    fn fake_sequence(
        replies: Vec<(u16, &'static str)>,
    ) -> (String, std::sync::mpsc::Receiver<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let (tx, rx) = std::sync::mpsc::channel();

        thread::spawn(move || {
            for (status, body) in replies {
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

    fn fake_server(status: u16, body: &'static str) -> (String, std::sync::mpsc::Receiver<String>) {
        fake_sequence(vec![(status, body)])
    }

    /// Reads one full HTTP/1.1 request (headers + declared body, if any)
    /// off `stream`, tolerating the headers and body arriving as separate
    /// reads.
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

    fn next(rx: &std::sync::mpsc::Receiver<String>) -> String {
        rx.recv_timeout(Duration::from_secs(2)).unwrap()
    }

    #[test]
    fn submit_job_sends_the_id_the_dedup_key_and_the_mission_spec() {
        let (base_url, rx) =
            fake_server(201, r#"{"ID":"m1","Status":"pending","result":"created"}"#);
        let spec = JobSpec {
            priority: 7,
            required_tool: "Laser".into(),
            location: "cell-2".into(),
            depends_on: vec!["prep".into()],
        };
        assert!(submit_job(&base_url, "m1", &spec).is_ok());

        let request = next(&rx);
        assert!(request.starts_with("POST /jobs/submit"));
        assert!(request.contains("\"id\":\"m1\""));
        assert!(request.contains("\"dedupKey\":\"m1\""));
        assert!(request.contains("\"priority\":7"));
        assert!(request.contains("\"requiredTool\":\"Laser\""));
        assert!(request.contains("\"location\":\"cell-2\""));
        assert!(request.contains("\"dependsOn\":[\"prep\"]"));
    }

    #[test]
    fn submit_job_treats_409_as_success() {
        let (base_url, _rx) = fake_server(409, r#"{"error":"job ID already exists: \"m1\""}"#);
        assert!(
            submit_job(&base_url, "m1", &JobSpec::default()).is_ok(),
            "a 409 (already submitted) must not be treated as a real failure"
        );
    }

    #[test]
    fn submit_job_reports_unreachable_when_nothing_is_listening() {
        let result = submit_job("http://127.0.0.1:1", "m1", &JobSpec::default());
        assert!(matches!(result, Err(ClientError::Unreachable(_))));
    }

    // The close depends on what Job-Dispatcher holds for the job.

    #[test]
    fn closing_a_job_a_robot_holds_completes_it_naming_that_robot() {
        let (base_url, rx) = fake_sequence(vec![
            (
                200,
                r#"{"ID":"m1","Status":"assigned","AssignedRobot":"arm-3"}"#,
            ),
            (200, r#"{"ID":"m1","Status":"done"}"#),
        ]);
        assert!(close_job(&base_url, "m1", true).is_ok());

        assert!(next(&rx).starts_with("GET /jobs/m1 "));
        let complete = next(&rx);
        assert!(complete.starts_with("POST /jobs/complete"));
        assert!(complete.contains("\"id\":\"m1\""));
        assert!(complete.contains("\"success\":true"));
        assert!(complete.contains("\"robotId\":\"arm-3\""));
    }

    #[test]
    fn closing_a_job_nobody_holds_withdraws_it_instead_of_leaving_it_to_be_dispatched() {
        for status in ["pending", "blocked", "unreachable"] {
            let body: &'static str = Box::leak(
                format!(r#"{{"ID":"m1","Status":"{status}","AssignedRobot":""}}"#).into_boxed_str(),
            );
            let (base_url, rx) = fake_sequence(vec![
                (200, body),
                (200, r#"{"ID":"m1","Status":"cancelled"}"#),
            ]);
            assert!(close_job(&base_url, "m1", false).is_ok(), "{status}");
            next(&rx);
            let cancel = next(&rx);
            assert!(
                cancel.starts_with("POST /jobs/cancel"),
                "{status}: {cancel}"
            );
            assert!(cancel.contains("\"id\":\"m1\""));
        }
    }

    #[test]
    fn closing_a_job_that_is_already_over_or_unknown_there_does_nothing() {
        for status in ["done", "failed", "cancelled"] {
            let body: &'static str =
                Box::leak(format!(r#"{{"ID":"m1","Status":"{status}"}}"#).into_boxed_str());
            let (base_url, rx) = fake_server(200, body);
            assert!(close_job(&base_url, "m1", true).is_ok(), "{status}");
            next(&rx);
            assert!(
                rx.recv_timeout(Duration::from_millis(150)).is_err(),
                "{status}: no write expected"
            );
        }
        let (base_url, _rx) = fake_server(404, r#"{"error":"unknown job \"m1\""}"#);
        assert!(close_job(&base_url, "m1", true).is_ok());
    }

    #[test]
    fn a_conflict_while_closing_is_an_error_so_the_close_stays_pending() {
        let (base_url, _rx) = fake_sequence(vec![
            (
                200,
                r#"{"ID":"m1","Status":"assigned","AssignedRobot":"arm-3"}"#,
            ),
            (409, r#"{"error":"robot mismatch"}"#),
        ]);
        assert!(matches!(
            close_job(&base_url, "m1", true),
            Err(ClientError::BadResponse(_))
        ));
    }

    #[test]
    fn an_older_dispatcher_without_the_job_route_is_not_taken_for_an_unknown_job() {
        let (base_url, _rx) = fake_server(404, "404 page not found\n");
        assert!(
            matches!(
                close_job(&base_url, "m1", true),
                Err(ClientError::BadResponse(_))
            ),
            "a plain 404 means the route is missing, not that the job never existed"
        );
    }

    #[test]
    fn close_job_reports_unreachable_when_nothing_is_listening() {
        let result = close_job("http://127.0.0.1:1", "m1", true);
        assert!(matches!(result, Err(ClientError::Unreachable(_))));
    }

    #[test]
    fn a_dispatcher_that_never_answers_does_not_hang_the_caller() {
        // accepts the connection and then says nothing for longer than the timeout
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        thread::spawn(move || {
            let held = listener.accept();
            thread::sleep(Duration::from_secs(CALL_TIMEOUT.as_secs() + 5));
            drop(held);
        });
        let started = std::time::Instant::now();
        let result = run_dispatch(&format!("http://127.0.0.1:{port}"));
        assert!(matches!(result, Err(ClientError::Unreachable(_))));
        assert!(
            started.elapsed() < CALL_TIMEOUT + Duration::from_secs(3),
            "the call must give up at the timeout"
        );
    }

    #[test]
    fn requeue_job_asks_for_a_requeue_naming_the_lost_robot() {
        let (base_url, rx) = fake_server(200, r#"{"ID":"m1","Status":"pending"}"#);
        assert!(requeue_job(&base_url, "m1", "arm-3", &JobSpec::default()).is_ok());
        let request = next(&rx);
        assert!(request.starts_with("POST /jobs/requeue"));
        assert!(request.contains("\"id\":\"m1\""));
        assert!(request.contains("\"robotId\":\"arm-3\""));
    }

    #[test]
    fn requeue_job_submits_again_when_the_dispatcher_lost_the_job() {
        let (base_url, rx) = fake_sequence(vec![
            (404, r#"{"error":"unknown job \"m1\""}"#),
            (201, r#"{"ID":"m1","Status":"pending","result":"created"}"#),
        ]);
        assert!(requeue_job(&base_url, "m1", "arm-3", &JobSpec::default()).is_ok());
        assert!(next(&rx).starts_with("POST /jobs/requeue"));
        assert!(next(&rx).starts_with("POST /jobs/submit"));
    }

    #[test]
    fn requeue_job_reports_a_conflict_instead_of_hiding_it() {
        let (base_url, _rx) = fake_server(409, r#"{"error":"robot mismatch"}"#);
        assert!(requeue_job(&base_url, "m1", "arm-3", &JobSpec::default()).is_err());
    }

    #[test]
    fn requeue_job_reports_unreachable_when_nothing_is_listening() {
        let result = requeue_job("http://127.0.0.1:1", "m1", "arm-3", &JobSpec::default());
        assert!(matches!(result, Err(ClientError::Unreachable(_))));
    }

    #[test]
    fn run_dispatch_parses_real_assignments() {
        let (base_url, rx) = fake_server(200, r#"[{"JobID":"m1","RobotID":"arm-3"}]"#);
        let result = run_dispatch(&base_url).unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].job_id, "m1");
        assert_eq!(result[0].robot_id, "arm-3");
        assert!(next(&rx).starts_with("POST /dispatch"));
    }

    #[test]
    fn run_dispatch_handles_a_real_empty_pass() {
        let (base_url, _rx) = fake_server(200, "[]");
        let result = run_dispatch(&base_url).unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn path_segments_are_percent_encoded() {
        assert_eq!(path_segment("m-1.a_b"), "m-1.a_b");
        assert_eq!(path_segment("a b/c"), "a%20b%2Fc");
    }
}
