// =============================================================================
// HYDRA-UMC-ORCHESTRATOR - src/estop.rs
// Copyright (C) 2026 JuanenRac (Electro Hobby 3D) <electrohobby3d@gmail.com>
// GPL-3.0-or-later - see LICENSE
// =============================================================================
//! The global stop of the fleet: `POST /estop`.
//!
//! A stop is a safety function, so it is built to be the one thing that always answers:
//!
//! * it is never switched off and never waits for anything: the latch is an atomic flag set
//!   before anything else is done, the registry of missions is only looked at with `try_lock`
//!   (a busy or poisoned registry never delays the stop) and the call to the server has a
//!   short timeout on its own thread;
//! * it is idempotent: engaging an engaged latch changes nothing and only retries the call to
//!   the server when the previous attempt was not confirmed;
//! * it is audited: every trigger, repeat and release is one JSON line in the audit file and in
//!   the log, written after the stop took effect (a file that cannot be written never blocks it);
//! * it does not clear by itself: the latch is saved to disk and survives a restart, and only an
//!   explicit release by an administrator ends it. A state file that exists but cannot be read
//!   is treated as an engaged stop (fail closed).
//!
//! While the latch is engaged the orchestrator refuses to dispatch, auto-dispatch or start a
//! mission (`423`); stopping, completing, failing, cancelling and recovering stay possible.
//! The latch also asks HYDRA-UMC-SERVER to stop every robot (`POST /api/stop-all`), which is the
//! part that announces the retained stop on the MQTT bridge when that option is on there.
//! Releasing here does not release the server: its stop is rearmed explicitly on its own side.

use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Mutex, MutexGuard};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

const MAX_BY: usize = 128;
const MAX_REASON: usize = 500;

#[derive(Debug, Clone)]
pub struct EstopConfig {
    /// where the latch is saved (None: kept in memory only)
    pub state_file: Option<PathBuf>,
    /// where every stop and release is recorded, one JSON line each (None: only the log)
    pub audit_file: Option<PathBuf>,
    /// base URL of HYDRA-UMC-SERVER; the stop is forwarded to `<url>/api/stop-all` (None: not forwarded)
    pub server_url: Option<String>,
    pub server_token: Option<String>,
    pub fanout_timeout: Duration,
    /// the token that may release the stop; without it the general token (or none) decides
    pub admin_token: Option<String>,
}

impl Default for EstopConfig {
    fn default() -> Self {
        EstopConfig {
            state_file: None,
            audit_file: None,
            server_url: None,
            server_token: None,
            fanout_timeout: Duration::from_secs(3),
            admin_token: None,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct Latch {
    engaged: bool,
    by: String,
    reason: String,
    since_ms: u64,
    /// the server confirmed it applied the stop
    server_confirmed: bool,
    /// how many times a stop was asked for while engaged, the first one included
    triggers: u64,
}

pub struct Estop {
    engaged: AtomicBool,
    latch: Mutex<Latch>,
    cfg: EstopConfig,
}

/// Cuts a text to a number of characters and removes control characters, so a name or a reason cannot forge a log line.
fn clean(text: &str, max: usize) -> String {
    text.chars()
        .filter(|c| !c.is_control())
        .take(max)
        .collect::<String>()
        .trim()
        .to_string()
}

impl Estop {
    pub fn new(cfg: EstopConfig) -> Estop {
        let mut latch = Latch::default();
        if let Some(path) = &cfg.state_file {
            match std::fs::read_to_string(path) {
                Ok(text) => match serde_json::from_str::<Latch>(&text) {
                    Ok(saved) => latch = saved,
                    Err(e) => {
                        // an unreadable record of a stop must not become "no stop"
                        crate::log_line!(
                            "the saved state of the global stop at {} cannot be read ({e}): treating the stop as engaged",
                            path.display()
                        );
                        latch = Latch {
                            engaged: true,
                            by: "startup".into(),
                            reason: "the saved state of the stop could not be read".into(),
                            since_ms: crate::logging::now_ms(),
                            server_confirmed: false,
                            triggers: 1,
                        };
                    }
                },
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => {
                    crate::log_line!(
                        "the saved state of the global stop at {} cannot be opened ({e}): treating the stop as engaged",
                        path.display()
                    );
                    latch = Latch {
                        engaged: true,
                        by: "startup".into(),
                        reason: "the saved state of the stop could not be opened".into(),
                        since_ms: crate::logging::now_ms(),
                        server_confirmed: false,
                        triggers: 1,
                    };
                }
            }
        }
        if latch.engaged {
            crate::log_line!(
                "the global stop is ENGAGED (since {} ms, by {}): dispatching and starting are refused until an administrator releases it",
                latch.since_ms,
                latch.by
            );
        }
        Estop {
            engaged: AtomicBool::new(latch.engaged),
            latch: Mutex::new(latch),
            cfg,
        }
    }

    pub fn admin_token(&self) -> Option<&str> {
        self.cfg.admin_token.as_deref()
    }

    /// True while the stop is engaged. One atomic read: it never waits.
    pub fn engaged(&self) -> bool {
        self.engaged.load(Ordering::SeqCst)
    }

    fn lock(&self) -> MutexGuard<'_, Latch> {
        // a panic elsewhere must not turn into a stop that cannot be asked for
        self.latch.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn save(&self, latch: &Latch) {
        let Some(path) = &self.cfg.state_file else {
            return;
        };
        let text = serde_json::to_string(latch).unwrap_or_default();
        let tmp = path.with_extension("tmp");
        let result = std::fs::write(&tmp, text).and_then(|()| std::fs::rename(&tmp, path));
        if let Err(e) = result {
            crate::log_line!(
                "could not save the state of the global stop to {}: {e}",
                path.display()
            );
        }
    }

    fn audit(&self, event: &str, by: &str, detail: Value) {
        let line =
            json!({"at_ms": crate::logging::now_ms(), "event": event, "by": by, "detail": detail})
                .to_string();
        crate::log_line!("global stop audit: {line}");
        if let Some(path) = &self.cfg.audit_file {
            let written = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)
                .and_then(|mut f| writeln!(f, "{line}"));
            if let Err(e) = written {
                crate::log_line!(
                    "could not write the audit of the global stop to {}: {e}",
                    path.display()
                );
            }
        }
    }

    pub fn status(&self) -> Value {
        let latch = self.lock();
        json!({
            "engaged": latch.engaged,
            "by": latch.by,
            "reason": latch.reason,
            "since_ms": latch.since_ms,
            "server_confirmed": latch.server_confirmed,
            "server_configured": self.cfg.server_url.is_some(),
            "triggers": latch.triggers,
        })
    }

    /// Engages the stop. `affected` is what the caller could read of the missions that were moving (informative only).
    pub fn trigger(&self, by: &str, reason: &str, affected: Option<Vec<String>>) -> Value {
        // first of all, before anything can fail or wait
        let was_engaged = self.engaged.swap(true, Ordering::SeqCst);
        let by = clean(by, MAX_BY);
        let by = if by.is_empty() {
            "unknown".to_string()
        } else {
            by
        };
        let reason = clean(reason, MAX_REASON);
        let need_forward;
        let snapshot;
        {
            let mut latch = self.lock();
            let first = !latch.engaged;
            if first {
                latch.engaged = true;
                latch.by = by.clone();
                latch.reason = reason.clone();
                latch.since_ms = crate::logging::now_ms();
                latch.server_confirmed = false;
                latch.triggers = 0;
            }
            latch.triggers += 1;
            need_forward = self.cfg.server_url.is_some() && !latch.server_confirmed;
            snapshot = latch.clone();
            self.save(&latch);
        }
        let server = if need_forward {
            let outcome = self.forward_to_server();
            if outcome.is_ok() {
                let mut latch = self.lock();
                latch.server_confirmed = true;
                self.save(&latch);
            }
            match outcome {
                Ok(()) => "confirmed".to_string(),
                Err(e) => format!("failed: {e}"),
            }
        } else if self.cfg.server_url.is_some() {
            "already_confirmed".to_string()
        } else {
            "not_configured".to_string()
        };
        let repeat = was_engaged || !snapshot.engaged || snapshot.triggers > 1;
        self.audit(
            if repeat { "estop_repeat" } else { "estop" },
            &by,
            json!({"reason": reason, "server": server, "affected_missions": affected}),
        );
        json!({
            "engaged": true,
            "already_engaged": repeat,
            "by": snapshot.by,
            "since_ms": snapshot.since_ms,
            "server": server,
            "affected_missions": affected,
        })
    }

    /// Ends the stop. The caller has already checked that the requester is an administrator.
    pub fn release(&self, by: &str) -> Value {
        let by = clean(by, MAX_BY);
        let by = if by.is_empty() {
            "unknown".to_string()
        } else {
            by
        };
        let was = {
            let mut latch = self.lock();
            let was = latch.engaged;
            if was {
                *latch = Latch::default();
                self.save(&latch);
            }
            self.engaged.store(false, Ordering::SeqCst);
            was
        };
        self.audit(
            if was { "release" } else { "release_noop" },
            &by,
            json!({"was_engaged": was, "server_released": false}),
        );
        json!({"engaged": false, "was_engaged": was, "server_released": false})
    }

    /// Records a release that was refused.
    pub fn audit_refused_release(&self, by: &str) {
        self.audit("release_refused", &clean(by, MAX_BY), json!({}));
    }

    fn forward_to_server(&self) -> Result<(), String> {
        let Some(base) = &self.cfg.server_url else {
            return Ok(());
        };
        let url = format!("{}/api/stop-all", base.trim_end_matches('/'));
        let token = self.cfg.server_token.clone();
        let timeout = self.cfg.fanout_timeout;
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let agent = ureq::AgentBuilder::new()
                .timeout_connect(timeout)
                .timeout(timeout)
                .build();
            let mut request = agent.post(&url).set("Content-Type", "application/json");
            if let Some(token) = &token {
                request = request.set("Authorization", &format!("Bearer {token}"));
            }
            let result = match request.send_string("{}") {
                Ok(_) => Ok(()),
                Err(ureq::Error::Status(code, _)) => Err(format!("the server answered {code}")),
                Err(e) => Err(format!("the server could not be reached: {e}")),
            };
            let _ = tx.send(result);
        });
        match rx.recv_timeout(timeout + Duration::from_millis(500)) {
            Ok(result) => result,
            Err(_) => Err("the server did not answer in time".to_string()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;
    use std::net::TcpListener;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("orch-estop-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn fake_server(status: &'static str) -> (String, mpsc::Receiver<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { break };
                // read the whole request (the body may arrive in a second write) before answering
                let mut received = Vec::new();
                let mut buf = [0u8; 2048];
                loop {
                    let n = stream.read(&mut buf).unwrap_or(0);
                    if n == 0 {
                        break;
                    }
                    received.extend_from_slice(&buf[..n]);
                    let text = String::from_utf8_lossy(&received);
                    if let Some((head, body)) = text.split_once("\r\n\r\n") {
                        let length = head
                            .lines()
                            .find_map(|l| {
                                l.to_ascii_lowercase()
                                    .strip_prefix("content-length:")
                                    .map(|v| v.trim().parse::<usize>().unwrap_or(0))
                            })
                            .unwrap_or(0);
                        if body.len() >= length {
                            break;
                        }
                    }
                }
                let _ = tx.send(String::from_utf8_lossy(&received).to_string());
                let _ = write!(
                    stream,
                    "HTTP/1.1 {status}\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{{}}"
                );
            }
        });
        (url, rx)
    }

    #[test]
    fn starts_released_and_a_trigger_engages_without_a_server() {
        let estop = Estop::new(EstopConfig::default());
        assert!(!estop.engaged());
        let out = estop.trigger("ana", "smoke", None);
        assert!(estop.engaged());
        assert_eq!(out["already_engaged"], false);
        assert_eq!(out["server"], "not_configured");
    }

    #[test]
    fn a_second_trigger_is_idempotent_and_keeps_the_first_author() {
        let estop = Estop::new(EstopConfig::default());
        estop.trigger("ana", "first", None);
        let out = estop.trigger("bob", "second", None);
        assert_eq!(out["already_engaged"], true);
        assert_eq!(out["by"], "ana");
        assert!(estop.engaged());
        assert_eq!(estop.status()["triggers"], 2);
    }

    #[test]
    fn the_stop_does_not_clear_by_itself_and_survives_a_restart() {
        let dir = temp_dir("restart");
        let cfg = EstopConfig {
            state_file: Some(dir.join("estop.json")),
            audit_file: Some(dir.join("audit.jsonl")),
            ..EstopConfig::default()
        };
        Estop::new(cfg.clone()).trigger("ana", "r", None);
        let again = Estop::new(cfg.clone());
        assert!(again.engaged(), "a restart must not rearm");
        let out = again.release("admin");
        assert_eq!(out["was_engaged"], true);
        assert!(!Estop::new(cfg).engaged());
    }

    #[test]
    fn an_unreadable_state_file_is_an_engaged_stop() {
        let dir = temp_dir("corrupt");
        let path = dir.join("estop.json");
        std::fs::write(&path, "{not json").unwrap();
        let estop = Estop::new(EstopConfig {
            state_file: Some(path),
            ..EstopConfig::default()
        });
        assert!(estop.engaged());
    }

    #[test]
    fn every_trigger_and_release_is_audited() {
        let dir = temp_dir("audit");
        let audit = dir.join("audit.jsonl");
        let estop = Estop::new(EstopConfig {
            audit_file: Some(audit.clone()),
            ..EstopConfig::default()
        });
        estop.trigger("ana", "smoke\nforged line", None);
        estop.trigger("bob", "again", None);
        estop.audit_refused_release("carl");
        estop.release("admin");
        let text = std::fs::read_to_string(audit).unwrap();
        let events: Vec<String> = text
            .lines()
            .map(|l| {
                serde_json::from_str::<Value>(l).unwrap()["event"]
                    .as_str()
                    .unwrap()
                    .to_string()
            })
            .collect();
        assert_eq!(
            events,
            ["estop", "estop_repeat", "release_refused", "release"]
        );
        assert_eq!(
            text.lines().count(),
            4,
            "a reason with a newline must not forge a line"
        );
    }

    #[test]
    fn the_stop_is_forwarded_to_the_server_until_it_is_confirmed() {
        let (url, rx) = fake_server("200 OK");
        let estop = Estop::new(EstopConfig {
            server_url: Some(url),
            server_token: Some("tok".into()),
            ..EstopConfig::default()
        });
        let out = estop.trigger("ana", "r", None);
        assert_eq!(out["server"], "confirmed");
        let request = rx.recv_timeout(Duration::from_secs(2)).unwrap();
        assert!(request.starts_with("POST /api/stop-all"), "{request}");
        assert!(request
            .to_ascii_lowercase()
            .contains("authorization: bearer tok"));
        // already confirmed: a repeat does not call again
        let again = estop.trigger("ana", "r", None);
        assert_eq!(again["server"], "already_confirmed");
        assert!(rx.recv_timeout(Duration::from_millis(300)).is_err());
    }

    #[test]
    fn an_unreachable_or_failing_server_never_stops_the_stop_and_is_retried() {
        let (url, _rx) = fake_server("500 Internal Server Error");
        let estop = Estop::new(EstopConfig {
            server_url: Some(url),
            ..EstopConfig::default()
        });
        let out = estop.trigger("ana", "r", None);
        assert!(estop.engaged());
        assert!(
            out["server"].as_str().unwrap().starts_with("failed"),
            "{out}"
        );
        assert_eq!(estop.status()["server_confirmed"], false);
        // nothing listens here
        let dead = Estop::new(EstopConfig {
            server_url: Some("http://127.0.0.1:1".into()),
            fanout_timeout: Duration::from_millis(300),
            ..EstopConfig::default()
        });
        let out = dead.trigger("ana", "r", None);
        assert!(dead.engaged());
        assert!(out["server"].as_str().unwrap().starts_with("failed"));
    }

    #[test]
    fn the_stop_answers_even_when_the_latch_lock_was_poisoned() {
        let estop = std::sync::Arc::new(Estop::new(EstopConfig::default()));
        let clone = std::sync::Arc::clone(&estop);
        let _ = std::thread::spawn(move || {
            let _guard = clone.latch.lock().unwrap();
            panic!("poison");
        })
        .join();
        let out = estop.trigger("ana", "r", None);
        assert_eq!(out["engaged"], true);
        assert!(estop.engaged());
    }

    #[test]
    fn release_of_a_released_stop_changes_nothing() {
        let estop = Estop::new(EstopConfig::default());
        let out = estop.release("admin");
        assert_eq!(out["was_engaged"], false);
        assert!(!estop.engaged());
    }

    #[test]
    fn names_and_reasons_are_cleaned_and_bounded() {
        assert_eq!(clean("a\nb\u{7}c", 10), "abc");
        assert_eq!(clean(&"x".repeat(1000), MAX_REASON).len(), MAX_REASON);
    }

    #[test]
    fn a_state_file_that_cannot_be_opened_is_an_engaged_stop() {
        // a directory where the file should be: the error is not "not found", so the stop must not be taken as released
        let dir = temp_dir("unopenable");
        let path = dir.join("estop.json");
        std::fs::create_dir_all(&path).unwrap();
        let estop = Estop::new(EstopConfig {
            state_file: Some(path),
            ..EstopConfig::default()
        });
        assert!(estop.engaged());
        assert_eq!(estop.status()["by"], "startup");
        assert_eq!(estop.status()["triggers"], 1);
    }

    #[test]
    fn a_saved_released_state_starts_released_and_a_missing_file_too() {
        let dir = temp_dir("released");
        let path = dir.join("estop.json");
        let cfg = EstopConfig {
            state_file: Some(path.clone()),
            ..EstopConfig::default()
        };
        assert!(!Estop::new(cfg.clone()).engaged(), "no file at all");
        let estop = Estop::new(cfg.clone());
        estop.trigger("ana", "r", None);
        estop.release("admin");
        let saved: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(saved["engaged"], false);
        assert_eq!(saved["triggers"], 0);
        assert!(!Estop::new(cfg).engaged());
    }

    #[test]
    fn what_is_saved_while_engaged_names_the_first_author_and_the_cleaned_reason() {
        let dir = temp_dir("saved");
        let path = dir.join("estop.json");
        let estop = Estop::new(EstopConfig {
            state_file: Some(path.clone()),
            ..EstopConfig::default()
        });
        estop.trigger("ana\nroot", "smoke\u{7} in cell 2", Some(vec!["m1".into()]));
        estop.trigger("bob", "second", None);
        let saved: Value = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
        assert_eq!(saved["engaged"], true);
        assert_eq!(saved["by"], "anaroot");
        assert_eq!(saved["reason"], "smoke in cell 2");
        assert_eq!(saved["triggers"], 2);
        assert!(saved["since_ms"].as_u64().unwrap() > 0);
    }

    #[test]
    fn an_empty_author_is_recorded_as_unknown_and_a_file_that_cannot_be_written_does_not_stop_the_stop(
    ) {
        let dir = temp_dir("unwritable");
        // the audit file and the state file are inside a folder that does not exist: nothing can be written
        let missing = dir.join("no-such-folder");
        let estop = Estop::new(EstopConfig {
            state_file: Some(missing.join("estop.json")),
            audit_file: Some(missing.join("audit.jsonl")),
            ..EstopConfig::default()
        });
        let out = estop.trigger("\n\t", "r", None);
        assert!(estop.engaged());
        assert_eq!(out["engaged"], true);
        assert_eq!(out["by"], "unknown");
        let released = estop.release("   ");
        assert_eq!(released["was_engaged"], true);
        assert!(!estop.engaged());
    }

    /// A server that answers the requests it gets with the statuses in `statuses`, one each, and lets the test count them.
    fn server_with_statuses(statuses: Vec<&'static str>) -> (String, mpsc::Receiver<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            for status in statuses {
                let Ok((mut stream, _)) = listener.accept() else {
                    return;
                };
                let mut received = Vec::new();
                let mut buf = [0u8; 2048];
                loop {
                    let n = stream.read(&mut buf).unwrap_or(0);
                    if n == 0 {
                        break;
                    }
                    received.extend_from_slice(&buf[..n]);
                    if String::from_utf8_lossy(&received).contains("\r\n\r\n{}") {
                        break;
                    }
                }
                let _ = tx.send(String::from_utf8_lossy(&received).to_string());
                let _ = write!(
                    stream,
                    "HTTP/1.1 {status}\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{{}}"
                );
            }
        });
        (url, rx)
    }

    #[test]
    fn a_failed_forward_is_tried_again_by_the_next_trigger_and_a_confirmed_one_survives_a_restart()
    {
        let dir = temp_dir("retry");
        let (url, rx) = server_with_statuses(vec!["503 Service Unavailable", "200 OK"]);
        let cfg = EstopConfig {
            state_file: Some(dir.join("estop.json")),
            server_url: Some(url),
            ..EstopConfig::default()
        };
        let estop = Estop::new(cfg.clone());
        let first = estop.trigger("ana", "r", None);
        assert!(first["server"].as_str().unwrap().contains("503"), "{first}");
        assert_eq!(estop.status()["server_confirmed"], false);
        // the second ask tries again and this time the server confirms
        let second = estop.trigger("ana", "r", None);
        assert_eq!(second["server"], "confirmed");
        assert_eq!(second["already_engaged"], true);
        assert_eq!(estop.status()["server_confirmed"], true);
        assert!(rx.recv_timeout(Duration::from_secs(2)).is_ok());
        assert!(rx.recv_timeout(Duration::from_secs(2)).is_ok());
        // after a restart the confirmation is remembered: no third call is made
        let again = Estop::new(cfg);
        assert!(again.engaged());
        assert_eq!(
            again.trigger("bob", "r", None)["server"],
            "already_confirmed"
        );
        assert!(rx.recv_timeout(Duration::from_millis(300)).is_err());
    }

    #[test]
    fn the_status_of_a_released_stop_is_empty() {
        let estop = Estop::new(EstopConfig::default());
        estop.trigger("ana", "r", None);
        estop.release("admin");
        let status = estop.status();
        assert_eq!(status["engaged"], false);
        assert_eq!(status["by"], "");
        assert_eq!(status["triggers"], 0);
        assert_eq!(status["server_configured"], false);
        // and it can be engaged again from scratch, with the author of the new stop
        let out = estop.trigger("bob", "again", None);
        assert_eq!(out["already_engaged"], false);
        assert_eq!(out["by"], "bob");
    }
}
