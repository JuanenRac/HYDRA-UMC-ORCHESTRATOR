// =============================================================================
// HYDRA-UMC-ORCHESTRATOR - src/job_events.rs
// Copyright (C) 2026 JuanenRac (Electro Hobby 3D) <electrohobby3d@gmail.com>
// GPL-3.0-or-later - see LICENSE
// =============================================================================
//! The receiving end of the events of HYDRA-UMC-JOB-DISPATCHER (`POST /events/job-dispatcher`).
//!
//! With the section `job_events` of the configuration file switched on, Job-Dispatcher can tell
//! this orchestrator when it assigned a job, so an assignment made by a pass that nobody here
//! asked for no longer goes unnoticed until someone polls. Off by default: without the section
//! the route answers like any unknown path.
//!
//! The request is accepted only when it carries a valid `X-Hydra-Signature` (HMAC-SHA256 of
//! `"<X-Hydra-Timestamp>.<body>"` with the shared secret, read from an environment variable)
//! and a timestamp within `max_skew_seconds`. Only `assigned` changes a mission (the same
//! `Pending -> Dispatched` step as `auto-dispatch`, and only for a mission that is still
//! `Pending`); `done`, `failed` and `unknown` are counted and logged, because the outcome of a
//! mission is still decided by what its node reports.

use std::sync::atomic::{AtomicU64, Ordering};

use serde::Deserialize;

use crate::hmac;

#[derive(Debug, Clone)]
pub struct JobEventsConfig {
    pub secret: Vec<u8>,
    pub max_skew_seconds: u64,
}

#[derive(Debug, Deserialize)]
pub struct JobEvent {
    pub id: u64,
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(rename = "jobId")]
    pub job_id: String,
    #[serde(rename = "robotId", default)]
    pub robot_id: String,
    #[serde(default)]
    pub attempt: u32,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Refusal {
    MissingHeader(&'static str),
    BadTimestamp,
    Stale,
    BadSignature,
    BadBody(String),
}

impl Refusal {
    pub fn status(&self) -> u16 {
        match self {
            Refusal::BadSignature | Refusal::Stale | Refusal::BadTimestamp => 401,
            Refusal::MissingHeader(_) | Refusal::BadBody(_) => 400,
        }
    }
}

impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Refusal::MissingHeader(name) => write!(f, "the header {name} is required"),
            Refusal::BadTimestamp => write!(f, "X-Hydra-Timestamp is not a number of seconds"),
            Refusal::Stale => write!(f, "the timestamp is too far from the clock of this machine"),
            Refusal::BadSignature => write!(f, "the signature does not match"),
            Refusal::BadBody(e) => write!(f, "the event is not valid: {e}"),
        }
    }
}

/// Counters of what arrived, published by `GET /stats`.
#[derive(Default)]
pub struct JobEventCounters {
    pub assigned: AtomicU64,
    pub done: AtomicU64,
    pub failed: AtomicU64,
    pub unknown: AtomicU64,
    pub refused: AtomicU64,
}

impl JobEventCounters {
    pub fn count(&self, kind: &str) {
        let counter = match kind {
            "assigned" => &self.assigned,
            "done" => &self.done,
            "failed" => &self.failed,
            "unknown" => &self.unknown,
            _ => return,
        };
        counter.fetch_add(1, Ordering::Relaxed);
    }

    pub fn snapshot(&self) -> serde_json::Value {
        serde_json::json!({
            "assigned": self.assigned.load(Ordering::Relaxed),
            "done": self.done.load(Ordering::Relaxed),
            "failed": self.failed.load(Ordering::Relaxed),
            "unknown": self.unknown.load(Ordering::Relaxed),
            "refused": self.refused.load(Ordering::Relaxed),
        })
    }
}

/// Checks the signature and the age of a delivery and parses it.
pub fn verify(
    config: &JobEventsConfig,
    timestamp: Option<&str>,
    signature: Option<&str>,
    body: &str,
    now_s: u64,
) -> Result<JobEvent, Refusal> {
    let timestamp = timestamp.ok_or(Refusal::MissingHeader("X-Hydra-Timestamp"))?;
    let signature = signature.ok_or(Refusal::MissingHeader("X-Hydra-Signature"))?;
    let sent: u64 = timestamp.parse().map_err(|_| Refusal::BadTimestamp)?;
    let expected = format!(
        "sha256={}",
        hmac::sign(&config.secret, timestamp, body.as_bytes())
    );
    // the signature first: nothing about the clock is revealed to a caller without the secret
    if !hmac::constant_time_eq(expected.as_bytes(), signature.as_bytes()) {
        return Err(Refusal::BadSignature);
    }
    if now_s.abs_diff(sent) > config.max_skew_seconds {
        return Err(Refusal::Stale);
    }
    let event: JobEvent =
        serde_json::from_str(body).map_err(|e| Refusal::BadBody(e.to_string()))?;
    if !crate::mission::is_valid_mission_id(&event.job_id) {
        return Err(Refusal::BadBody("jobId is not a plain identifier".into()));
    }
    Ok(event)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> JobEventsConfig {
        JobEventsConfig {
            secret: b"s3cret".to_vec(),
            max_skew_seconds: 300,
        }
    }

    const BODY: &str =
        r#"{"id":7,"type":"assigned","jobId":"m1","robotId":"r1","attempt":1,"atMs":1}"#;

    fn signed(ts: &str, body: &str) -> String {
        format!("sha256={}", hmac::sign(b"s3cret", ts, body.as_bytes()))
    }

    #[test]
    fn a_signed_fresh_event_is_accepted() {
        let event = verify(
            &config(),
            Some("1000"),
            Some(&signed("1000", BODY)),
            BODY,
            1100,
        )
        .unwrap();
        assert_eq!(
            (
                event.kind.as_str(),
                event.job_id.as_str(),
                event.robot_id.as_str()
            ),
            ("assigned", "m1", "r1")
        );
    }

    #[test]
    fn a_wrong_signature_or_body_is_refused() {
        assert_eq!(
            verify(&config(), Some("1000"), Some("sha256=00"), BODY, 1000).unwrap_err(),
            Refusal::BadSignature
        );
        let tampered = BODY.replace("m1", "m2");
        assert_eq!(
            verify(
                &config(),
                Some("1000"),
                Some(&signed("1000", BODY)),
                &tampered,
                1000
            )
            .unwrap_err(),
            Refusal::BadSignature
        );
        let other = format!("sha256={}", hmac::sign(b"other", "1000", BODY.as_bytes()));
        assert_eq!(
            verify(&config(), Some("1000"), Some(&other), BODY, 1000).unwrap_err(),
            Refusal::BadSignature
        );
    }

    #[test]
    fn an_old_or_future_delivery_is_refused() {
        let sig = signed("1000", BODY);
        assert_eq!(
            verify(&config(), Some("1000"), Some(&sig), BODY, 1301).unwrap_err(),
            Refusal::Stale
        );
        assert_eq!(
            verify(&config(), Some("1000"), Some(&sig), BODY, 699).unwrap_err(),
            Refusal::Stale
        );
        assert!(verify(&config(), Some("1000"), Some(&sig), BODY, 1300).is_ok());
    }

    #[test]
    fn missing_headers_and_bad_bodies_are_refused() {
        assert_eq!(
            verify(&config(), None, Some("x"), BODY, 0)
                .unwrap_err()
                .status(),
            400
        );
        assert_eq!(
            verify(&config(), Some("1"), None, BODY, 0)
                .unwrap_err()
                .status(),
            400
        );
        assert_eq!(
            verify(&config(), Some("abc"), Some("x"), BODY, 0).unwrap_err(),
            Refusal::BadTimestamp
        );
        let bad = r#"{"id":1,"type":"assigned","jobId":"../x"}"#;
        assert!(matches!(
            verify(&config(), Some("5"), Some(&signed("5", bad)), bad, 5).unwrap_err(),
            Refusal::BadBody(_)
        ));
        let junk = "not json";
        assert!(matches!(
            verify(&config(), Some("5"), Some(&signed("5", junk)), junk, 5).unwrap_err(),
            Refusal::BadBody(_)
        ));
    }

    #[test]
    fn counters_count_each_type() {
        let c = JobEventCounters::default();
        c.count("assigned");
        c.count("done");
        c.count("done");
        c.count("nonsense");
        let s = c.snapshot();
        assert_eq!(
            (
                s["assigned"].as_u64(),
                s["done"].as_u64(),
                s["failed"].as_u64()
            ),
            (Some(1), Some(2), Some(0))
        );
    }
}
