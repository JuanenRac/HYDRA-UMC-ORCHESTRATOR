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

#[derive(Debug, Deserialize)]
pub struct Assignment {
    #[serde(rename = "JobID")]
    pub job_id: String,
    #[serde(rename = "RobotID")]
    pub robot_id: String,
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
            ClientError::BadResponse(e) => write!(f, "job-dispatcher returned an unexpected response: {e}"),
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
    let body = SubmitRequest { id: mission_id, dedup_key: mission_id };
    let result = ureq::post(&url)
        .set("Content-Type", "application/json")
        .send_string(&serde_json::to_string(&body).map_err(|e| ClientError::BadResponse(e.to_string()))?);

    match result {
        Ok(_) => Ok(()),
        // ureq treats 409 as an Err(Status(..)) - Job-Dispatcher's own
        // real "job ID already exists" response for a job this exact
        // mission id already submitted successfully before.
        Err(ureq::Error::Status(409, _)) => Ok(()),
        Err(ureq::Error::Status(code, resp)) => {
            Err(ClientError::BadResponse(format!("HTTP {code}: {}", resp.into_string().unwrap_or_default())))
        }
        Err(ureq::Error::Transport(t)) => Err(ClientError::Unreachable(t.to_string())),
    }
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
            let text = response.into_string().map_err(|e| ClientError::BadResponse(e.to_string()))?;
            serde_json::from_str::<Vec<Assignment>>(&text).map_err(|e| ClientError::BadResponse(e.to_string()))
        }
        Err(ureq::Error::Status(code, resp)) => {
            Err(ClientError::BadResponse(format!("HTTP {code}: {}", resp.into_string().unwrap_or_default())))
        }
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
    fn fake_server(status: u16, body: &'static str) -> (String, std::sync::mpsc::Receiver<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let (tx, rx) = std::sync::mpsc::channel();

        thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                let mut buf = [0u8; 4096];
                let n = stream.read(&mut buf).unwrap_or(0);
                let request = String::from_utf8_lossy(&buf[..n]).to_string();
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

    #[test]
    fn submit_job_sends_a_real_request_with_matching_id_and_dedup_key() {
        let (base_url, rx) = fake_server(201, r#"{"ID":"m1","Status":"pending","result":"created"}"#);
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
        assert!(result.is_ok(), "a 409 (already submitted) must not be treated as a real failure");
    }

    #[test]
    fn submit_job_reports_unreachable_when_nothing_is_listening() {
        let result = submit_job("http://127.0.0.1:1", "m1");
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
