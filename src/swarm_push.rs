// =============================================================================
// HYDRA-UMC-ORCHESTRATOR - src/swarm_push.rs
// Copyright (C) 2026 JuanenRac (Electro Hobby 3D) <electrohobby3d@gmail.com>
// GPL-3.0-or-later - see LICENSE
// =============================================================================
//! Pushes the mission registry to HYDRA-UMC-SWARM-SYNC, so a second orchestrator can hold the same registry.
//!
//! Every few seconds the records of `replica.rs` are compared with what was pushed last and only the ones that changed are sent, in one
//! `POST /reconcile` (one cell, the writer being a stable number taken from the name of this orchestrator). The clock of a write is a
//! counter that only goes up; at the start it is moved past the newest time the swarm-sync map already holds, so a restarted orchestrator
//! does not write with times the map already passed. The generation is `attempt + 1` as in `replica.rs`, and it is what decides which side
//! wins; the counter only breaks ties.
//!
//! Off unless `--swarm-sync-url` is given. A push that fails is tried again at the next round with the same records.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::json;

use crate::mission::MissionRegistry;
use crate::replica::{records, ReplicaRecord};

/// The default seconds between two pushes.
pub const DEFAULT_INTERVAL_S: u64 = 5;
/// The most records one push carries; a bigger change goes out in several rounds.
pub const MAX_PER_PUSH: usize = 500;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);
const CALL_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, Clone)]
pub struct PushConfig {
    pub url: String,
    pub token: Option<String>,
    /// The name of this orchestrator: the owner of a mission no robot holds, and the source of the writer number.
    pub owner: String,
    pub interval: Duration,
}

/// A stable number for the name of an orchestrator (FNV-1a), never 0.
pub fn writer_id(owner: &str) -> u64 {
    let mut hash: u64 = 0xcbf29ce484222325;
    for byte in owner.bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash.max(1)
}

#[derive(Debug, Default)]
pub struct Pusher {
    sent: BTreeMap<String, (String, u64)>,
    clock: u64,
}

impl Pusher {
    pub fn new(start_clock: u64) -> Self {
        Pusher {
            sent: BTreeMap::new(),
            clock: start_clock,
        }
    }

    /// The records that differ from what was pushed last (a new key, another value or another generation), at most `MAX_PER_PUSH`.
    pub fn changed<'a>(&self, current: &'a [ReplicaRecord]) -> Vec<&'a ReplicaRecord> {
        current
            .iter()
            .filter(|r| self.sent.get(&r.key) != Some(&(r.value.clone(), r.generation)))
            .take(MAX_PER_PUSH)
            .collect()
    }

    /// The `POST /reconcile` body for `changed`, with a clock tick for each write.
    pub fn body(&mut self, owner: &str, changed: &[&ReplicaRecord]) -> String {
        let writes: Vec<serde_json::Value> = changed
            .iter()
            .map(|r| {
                self.clock += 1;
                json!({"key": r.key, "value": r.value, "time": self.clock, "generation": r.generation})
            })
            .collect();
        json!({"cells": [{"id": owner, "writer": writer_id(owner), "writes": writes}]}).to_string()
    }

    /// Remembers that these records reached the map.
    pub fn confirm(&mut self, changed: &[&ReplicaRecord]) {
        for r in changed {
            self.sent
                .insert(r.key.clone(), (r.value.clone(), r.generation));
        }
    }

    #[cfg(test)]
    pub fn clock(&self) -> u64 {
        self.clock
    }
}

fn agent() -> ureq::Agent {
    ureq::AgentBuilder::new()
        .timeout_connect(CONNECT_TIMEOUT)
        .timeout(CALL_TIMEOUT)
        .build()
}

fn with_token(request: ureq::Request, token: &Option<String>) -> ureq::Request {
    match token {
        Some(t) => request.set("Authorization", &format!("Bearer {t}")),
        None => request,
    }
}

/// The newest Lamport time in the map of the swarm-sync node (`GET /state?full=1`), or 0 when it holds nothing.
pub fn newest_time(config: &PushConfig) -> Result<u64, String> {
    let url = format!("{}/state?full=1", config.url.trim_end_matches('/'));
    let response = with_token(agent().get(&url), &config.token)
        .call()
        .map_err(|e| e.to_string())?;
    let text = response.into_string().map_err(|e| e.to_string())?;
    let body: serde_json::Value = serde_json::from_str(&text).map_err(|e| e.to_string())?;
    Ok(body["entries"]
        .as_array()
        .map(|list| {
            list.iter()
                .filter_map(|e| e["time"].as_u64())
                .max()
                .unwrap_or(0)
        })
        .unwrap_or(0))
}

fn post_reconcile(config: &PushConfig, body: &str) -> Result<(), String> {
    let url = format!("{}/reconcile", config.url.trim_end_matches('/'));
    with_token(agent().post(&url), &config.token)
        .set("Content-Type", "application/json")
        .send_string(body)
        .map(|_| ())
        .map_err(|e| match e {
            ureq::Error::Status(code, resp) => {
                let text = resp.into_string().unwrap_or_default();
                format!(
                    "swarm-sync answered {code}: {}",
                    text.chars().take(200).collect::<String>()
                )
            }
            other => other.to_string(),
        })
}

/// One round: sends what changed. Returns how many records went out.
pub fn push_round(
    pusher: &mut Pusher,
    config: &PushConfig,
    registry: &Mutex<MissionRegistry>,
) -> Result<usize, String> {
    let current = records(&registry.lock().unwrap(), &config.owner);
    let changed = pusher.changed(&current);
    if changed.is_empty() {
        return Ok(0);
    }
    let body = pusher.body(&config.owner, &changed);
    post_reconcile(config, &body)?;
    pusher.confirm(&changed);
    Ok(changed.len())
}

/// Starts the pushing thread.
pub fn spawn(registry: Arc<Mutex<MissionRegistry>>, config: PushConfig) {
    std::thread::spawn(move || {
        let mut pusher = Pusher::new(0);
        let mut synced_clock = false;
        let mut last_error: Option<String> = None;
        loop {
            if !synced_clock {
                match newest_time(&config) {
                    Ok(time) => {
                        pusher = Pusher::new(time);
                        synced_clock = true;
                    }
                    Err(e) => note_error(&mut last_error, &e),
                }
            }
            if synced_clock {
                match push_round(&mut pusher, &config, &registry) {
                    Ok(0) => last_error = None,
                    Ok(n) => {
                        last_error = None;
                        crate::log_line!("pushed {n} mission record(s) to swarm-sync");
                    }
                    Err(e) => note_error(&mut last_error, &e),
                }
            }
            std::thread::sleep(config.interval);
        }
    });
}

/// Logs a problem once while it lasts, not at every round.
fn note_error(last: &mut Option<String>, error: &str) {
    if last.as_deref() != Some(error) {
        crate::log_line!("could not push the registry to swarm-sync: {error}");
        *last = Some(error.to_string());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(id: &str, attempt: u32, status: &str) -> ReplicaRecord {
        ReplicaRecord {
            key: format!("mission/{id}"),
            value: format!(
                r#"{{"mission_id":"{id}","status":"{status}","owner":"orch-a","attempt":{attempt}}}"#
            ),
            generation: u64::from(attempt) + 1,
        }
    }

    #[test]
    fn the_writer_number_is_stable_and_never_zero() {
        assert_eq!(writer_id("orch-a"), writer_id("orch-a"));
        assert_ne!(writer_id("orch-a"), writer_id("orch-b"));
        assert_ne!(writer_id(""), 0);
    }

    #[test]
    fn only_new_or_changed_records_go_out() {
        let mut pusher = Pusher::new(10);
        let first = vec![record("m1", 0, "queued"), record("m2", 0, "queued")];
        let changed = pusher.changed(&first);
        assert_eq!(changed.len(), 2);
        pusher.confirm(&changed);
        assert!(pusher.changed(&first).is_empty());
        let later = vec![record("m1", 0, "running"), record("m2", 0, "queued")];
        let changed = pusher.changed(&later);
        assert_eq!(changed.len(), 1);
        assert_eq!(changed[0].key, "mission/m1");
    }

    #[test]
    fn a_push_that_failed_is_sent_again_because_it_was_never_confirmed() {
        let mut pusher = Pusher::new(0);
        let list = vec![record("m1", 0, "queued")];
        let changed = pusher.changed(&list);
        let _ = pusher.body("orch-a", &changed);
        // no confirm: the push failed
        assert_eq!(pusher.changed(&list).len(), 1);
    }

    #[test]
    fn the_clock_goes_up_with_every_write_and_starts_past_the_map() {
        let mut pusher = Pusher::new(100);
        let list = vec![record("m1", 0, "queued"), record("m2", 2, "running")];
        let changed = pusher.changed(&list);
        let body: serde_json::Value =
            serde_json::from_str(&pusher.body("orch-a", &changed)).unwrap();
        let writes = body["cells"][0]["writes"].as_array().unwrap();
        assert_eq!(writes[0]["time"], 101);
        assert_eq!(writes[1]["time"], 102);
        assert_eq!(writes[1]["generation"], 3);
        assert_eq!(body["cells"][0]["writer"], writer_id("orch-a"));
        assert_eq!(pusher.clock(), 102);
    }

    #[test]
    fn a_big_change_goes_out_in_rounds() {
        let pusher = Pusher::new(0);
        let list: Vec<ReplicaRecord> = (0..MAX_PER_PUSH + 10)
            .map(|i| record(&format!("m{i}"), 0, "queued"))
            .collect();
        assert_eq!(pusher.changed(&list).len(), MAX_PER_PUSH);
    }

    /// A fake swarm-sync node: answers every request with 200 and hands each request it received (head and body) to the test.
    fn fake_swarm_sync(state_body: &'static str) -> (String, std::sync::mpsc::Receiver<String>) {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { break };
                let mut raw = Vec::new();
                let mut buf = [0u8; 4096];
                loop {
                    let n = stream.read(&mut buf).unwrap_or(0);
                    raw.extend_from_slice(&buf[..n]);
                    let text = String::from_utf8_lossy(&raw).to_string();
                    if let Some((head, body)) = text.split_once("\r\n\r\n") {
                        let length = head
                            .lines()
                            .find_map(|l| {
                                l.to_ascii_lowercase()
                                    .strip_prefix("content-length:")
                                    .and_then(|v| v.trim().parse::<usize>().ok())
                            })
                            .unwrap_or(0);
                        if body.len() >= length {
                            break;
                        }
                    }
                    if n == 0 {
                        break;
                    }
                }
                let text = String::from_utf8_lossy(&raw).to_string();
                let answer = if text.starts_with("GET") {
                    state_body
                } else {
                    "{}"
                };
                let _ = tx.send(text);
                let _ = write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{answer}",
                    answer.len()
                );
            }
        });
        (url, rx)
    }

    #[test]
    fn a_round_sends_the_changed_missions_with_the_token_and_then_nothing() {
        let (url, rx) = fake_swarm_sync(
            r#"{"entries":[{"key":"mission/old","value":"x","time":40,"writer":1,"generation":1}]}"#,
        );
        let config = PushConfig {
            url,
            token: Some("s3cret".into()),
            owner: "orch-a".into(),
            interval: Duration::from_secs(1),
        };
        assert_eq!(newest_time(&config).unwrap(), 40);
        let _ = rx.recv_timeout(Duration::from_secs(2)).unwrap();
        let path = std::env::temp_dir().join(format!(
            "orchestrator-swarm-push-test-{}.json",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);
        let mut reg = MissionRegistry::load(path.clone()).unwrap();
        reg.add("m1");
        let registry = Mutex::new(reg);
        let mut pusher = Pusher::new(40);
        assert_eq!(push_round(&mut pusher, &config, &registry).unwrap(), 1);
        let sent = rx.recv_timeout(Duration::from_secs(2)).unwrap();
        assert!(sent.starts_with("POST /reconcile"), "{sent}");
        assert!(sent.contains("Bearer s3cret"));
        assert!(sent.contains("mission/m1"));
        assert!(sent.contains("\"time\":41"));
        // nothing changed: nothing is sent
        assert_eq!(push_round(&mut pusher, &config, &registry).unwrap(), 0);
        assert!(rx.recv_timeout(Duration::from_millis(300)).is_err());
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn a_swarm_sync_that_is_down_is_an_error_and_nothing_is_marked_as_sent() {
        let config = PushConfig {
            url: "http://127.0.0.1:1".into(),
            token: None,
            owner: "orch-a".into(),
            interval: Duration::from_secs(1),
        };
        let path = std::env::temp_dir().join(format!(
            "orchestrator-swarm-push-down-{}.json",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);
        let mut reg = MissionRegistry::load(path.clone()).unwrap();
        reg.add("m1");
        let registry = Mutex::new(reg);
        let mut pusher = Pusher::new(0);
        assert!(push_round(&mut pusher, &config, &registry).is_err());
        assert_eq!(
            pusher
                .changed(&records(&registry.lock().unwrap(), "orch-a"))
                .len(),
            1
        );
        let _ = std::fs::remove_file(path);
    }

    use std::io::{Read, Write};
    use std::net::TcpListener;

    /// A stand-in swarm-sync node: answers the given (status, body) pairs in order, one connection each, and hands back the requests it saw.
    fn stand_in(
        answers: Vec<(u16, &'static str)>,
    ) -> (String, std::thread::JoinHandle<Vec<String>>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let handle = std::thread::spawn(move || {
            let mut seen = Vec::new();
            for (status, body) in answers {
                let (mut stream, _) = listener.accept().unwrap();
                let mut raw = Vec::new();
                let mut chunk = [0u8; 1024];
                let mut wanted: Option<usize> = None;
                loop {
                    let n = stream.read(&mut chunk).unwrap_or(0);
                    if n == 0 {
                        break;
                    }
                    raw.extend_from_slice(&chunk[..n]);
                    let text = String::from_utf8_lossy(&raw).to_string();
                    if let Some(end) = text.find("\r\n\r\n") {
                        let length = wanted.get_or_insert_with(|| {
                            text[..end]
                                .lines()
                                .find_map(|l| {
                                    l.to_ascii_lowercase()
                                        .strip_prefix("content-length:")
                                        .and_then(|v| v.trim().parse::<usize>().ok())
                                })
                                .unwrap_or(0)
                        });
                        if raw.len() >= end + 4 + *length {
                            break;
                        }
                    }
                }
                seen.push(String::from_utf8_lossy(&raw).to_string());
                let reply = format!(
                    "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = stream.write_all(reply.as_bytes());
            }
            seen
        });
        (url, handle)
    }

    fn config(url: &str, token: Option<&str>) -> PushConfig {
        PushConfig {
            url: url.to_string(),
            token: token.map(str::to_string),
            owner: "orch-a".into(),
            interval: Duration::from_millis(10),
        }
    }

    #[test]
    fn the_newest_time_is_the_largest_one_of_the_map_and_zero_when_it_holds_nothing() {
        let (url, server) = stand_in(vec![
            (
                200,
                r#"{"entries":[{"time":5},{"time":9},{"key":"no time"},{"time":"x"}]}"#,
            ),
            (200, r#"{"entries":[]}"#),
            (200, r#"{"other":1}"#),
        ]);
        let cfg = config(&format!("{url}/"), Some("t0k"));
        assert_eq!(newest_time(&cfg), Ok(9));
        assert_eq!(newest_time(&cfg), Ok(0));
        assert_eq!(newest_time(&cfg), Ok(0));
        let seen = server.join().unwrap();
        assert!(seen[0].starts_with("GET /state?full=1 "), "{}", seen[0]);
        assert!(
            seen[0]
                .to_ascii_lowercase()
                .contains("authorization: bearer t0k"),
            "{}",
            seen[0]
        );
    }

    #[test]
    fn a_map_that_cannot_be_read_is_an_error_and_so_is_a_node_that_is_not_there() {
        let (url, server) = stand_in(vec![(200, "not json"), (500, "{}")]);
        let cfg = config(&url, None);
        assert!(newest_time(&cfg).is_err());
        assert!(newest_time(&cfg).is_err());
        server.join().unwrap();
        let closed = TcpListener::bind("127.0.0.1:0").unwrap();
        let gone = format!("http://{}", closed.local_addr().unwrap());
        drop(closed);
        assert!(newest_time(&config(&gone, None)).is_err());
    }

    #[test]
    fn a_round_sends_what_changed_once_and_a_refusal_keeps_it_for_the_next_round() {
        let registry = Mutex::new(MissionRegistry::new());
        let mut pusher = Pusher::new(0);
        // nothing to send: the node is not even asked
        let idle = config("http://127.0.0.1:1", None);
        assert_eq!(push_round(&mut pusher, &idle, &registry), Ok(0));

        registry.lock().unwrap().add("m1");
        registry
            .lock()
            .unwrap()
            .add("m2")
            .dispatch("node-a")
            .unwrap();
        let (url, server) = stand_in(vec![(409, r#"{"error":"stale seal"}"#), (200, "{}")]);
        let cfg = config(&url, Some("t0k"));
        let refused = push_round(&mut pusher, &cfg, &registry).unwrap_err();
        assert!(refused.contains("swarm-sync answered 409"), "{refused}");
        assert!(refused.contains("stale seal"), "{refused}");
        // the refusal confirmed nothing: both records go out again, and then nothing is left
        assert_eq!(push_round(&mut pusher, &cfg, &registry), Ok(2));
        let seen = server.join().unwrap();
        assert!(seen[0].starts_with("POST /reconcile "), "{}", seen[0]);
        assert!(
            seen[1].contains("mission/m1") && seen[1].contains("mission/m2"),
            "{}",
            seen[1]
        );
        assert_eq!(push_round(&mut pusher, &idle, &registry), Ok(0));
    }

    #[test]
    fn a_problem_is_logged_once_while_it_lasts() {
        let mut last = None;
        note_error(&mut last, "down");
        assert_eq!(last.as_deref(), Some("down"));
        note_error(&mut last, "down");
        note_error(&mut last, "other");
        assert_eq!(last.as_deref(), Some("other"));
    }
}
