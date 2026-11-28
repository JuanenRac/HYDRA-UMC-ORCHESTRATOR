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
//! * it does not give up: while the latch is engaged and the server has not confirmed, the
//!   call is repeated in the background with a growing wait (`estop.retry_server`, on by
//!   default), also for a stop that was found engaged at the start;
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
//!
//! With `estop.hold_dispatcher_url` (not set by default) the latch also asks HYDRA-UMC-JOB-DISPATCHER to hold its assignments
//! (`POST /dispatch/hold`), at the same time as the server is called, so the queue hands no job to a robot either. It is
//! repeated like the call to the server until the dispatcher confirms it. Releasing here never releases the dispatcher.

use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Mutex, MutexGuard};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::job_dispatcher::http;
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
    /// base URL of HYDRA-UMC-JOB-DISPATCHER; its assignments are held at `<url>/dispatch/hold` (None: not asked)
    pub hold_url: Option<String>,
    pub hold_token: Option<String>,
    pub fanout_timeout: Duration,
    /// the token that may release the stop; without it the general token (or none) decides
    pub admin_token: Option<String>,
    /// repeat the call to the server in the background while the stop is engaged and not confirmed
    pub retry_server: bool,
    /// the first wait of that repetition; it doubles after each failure
    pub retry_initial: Duration,
    /// the longest wait between two repetitions
    pub retry_max: Duration,
}

impl Default for EstopConfig {
    fn default() -> Self {
        EstopConfig {
            state_file: None,
            audit_file: None,
            server_url: None,
            server_token: None,
            hold_url: None,
            hold_token: None,
            fanout_timeout: Duration::from_secs(3),
            admin_token: None,
            retry_server: true,
            retry_initial: Duration::from_secs(1),
            retry_max: Duration::from_secs(30),
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
    /// the job dispatcher confirmed it holds its assignments (only asked with `hold_url`)
    #[serde(default)]
    dispatcher_held: bool,
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
                            dispatcher_held: false,
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
                        dispatcher_held: false,
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
        let mut status = json!({
            "engaged": latch.engaged,
            "by": latch.by,
            "reason": latch.reason,
            "since_ms": latch.since_ms,
            "server_confirmed": latch.server_confirmed,
            "server_configured": self.cfg.server_url.is_some(),
            "server_retry": self.retries_server(),
            "triggers": latch.triggers,
        });
        if self.cfg.hold_url.is_some() {
            status["dispatcher_held"] = Value::Bool(latch.dispatcher_held);
        }
        status
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
        let need_hold;
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
                latch.dispatcher_held = false;
                latch.triggers = 0;
            }
            latch.triggers += 1;
            need_forward = self.cfg.server_url.is_some() && !latch.server_confirmed;
            need_hold = self.cfg.hold_url.is_some() && !latch.dispatcher_held;
            snapshot = latch.clone();
            self.save(&latch);
        }
        // asked before the server is waited for, so neither of the two delays the other
        let hold = if need_hold { self.ask_hold() } else { None };
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
        let dispatcher = self.cfg.hold_url.as_ref().map(|_| match hold {
            Some(pending) => match self.wait_for(pending, "the job dispatcher") {
                Ok(()) => {
                    let mut latch = self.lock();
                    latch.dispatcher_held = true;
                    self.save(&latch);
                    "held".to_string()
                }
                Err(e) => format!("failed: {e}"),
            },
            None => "already_held".to_string(),
        });
        let repeat = was_engaged || !snapshot.engaged || snapshot.triggers > 1;
        let mut record = json!({"reason": reason, "server": server, "affected_missions": affected});
        let mut answer = json!({
            "engaged": true,
            "already_engaged": repeat,
            "by": snapshot.by,
            "since_ms": snapshot.since_ms,
            "server": server,
            "affected_missions": affected,
        });
        if let Some(dispatcher) = dispatcher {
            record["dispatcher"] = Value::String(dispatcher.clone());
            answer["dispatcher"] = Value::String(dispatcher);
        }
        self.audit(if repeat { "estop_repeat" } else { "estop" }, &by, record);
        answer
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

    /// True when a stop the server has not confirmed is repeated in the background.
    pub fn retries_server(&self) -> bool {
        self.cfg.retry_server && self.cfg.server_url.is_some()
    }

    /// One more call to the server for a stop that is engaged and not confirmed by it; `None` when there is nothing to repeat.
    pub fn retry_unconfirmed(&self) -> Option<Result<(), String>> {
        if !self.retries_server() {
            return None;
        }
        let since = {
            let latch = self.lock();
            if !latch.engaged || latch.server_confirmed {
                return None;
            }
            latch.since_ms
        };
        let outcome = self.forward_to_server();
        if outcome.is_ok() {
            let confirmed = {
                let mut latch = self.lock();
                // the confirmation belongs to the stop it was asked for, not to one released in the meantime
                let same = latch.engaged && latch.since_ms == since && !latch.server_confirmed;
                if same {
                    latch.server_confirmed = true;
                    self.save(&latch);
                }
                same
            };
            if confirmed {
                self.audit("server_confirmed", "retry", json!({}));
            }
        }
        Some(outcome)
    }

    /// True when a hold the job dispatcher has not confirmed is repeated in the background.
    pub fn retries_hold(&self) -> bool {
        self.cfg.retry_server && self.cfg.hold_url.is_some()
    }

    /// One more hold for a stop that is engaged and that the dispatcher has not confirmed; `None` when there is nothing to repeat.
    pub fn retry_hold(&self) -> Option<Result<(), String>> {
        if !self.retries_hold() {
            return None;
        }
        let since = {
            let latch = self.lock();
            if !latch.engaged || latch.dispatcher_held {
                return None;
            }
            latch.since_ms
        };
        let outcome = self.wait_for(self.ask_hold()?, "the job dispatcher");
        if outcome.is_ok() {
            let held = {
                let mut latch = self.lock();
                let same = latch.engaged && latch.since_ms == since && !latch.dispatcher_held;
                if same {
                    latch.dispatcher_held = true;
                    self.save(&latch);
                }
                same
            };
            if held {
                self.audit("dispatcher_held", "retry", json!({}));
            }
        }
        Some(outcome)
    }

    /// The wait before the next repetition: the first wait again once there is nothing pending or the server answered, twice the last one (up to the longest) after a failure.
    fn next_wait(&self, last: Duration, outcome: Option<&Result<(), String>>) -> Duration {
        match outcome {
            Some(Err(_)) => last.saturating_mul(2).min(self.cfg.retry_max),
            _ => self.cfg.retry_initial,
        }
    }

    /// Repeats the call for as long as the process lives. Run it on its own thread: it never holds the latch while it waits.
    pub fn run_retries(&self) {
        let mut wait = self.cfg.retry_initial;
        let mut failing = false;
        loop {
            std::thread::sleep(wait);
            let held = self.retry_hold();
            let outcome = self.retry_unconfirmed();
            match &outcome {
                Some(Err(e)) => {
                    if !failing {
                        crate::log_line!("the server has not confirmed the global stop ({e}): it is asked again until it does");
                    }
                    failing = true;
                }
                _ => failing = false,
            }
            // a hold that keeps failing is asked again with the same growing wait as the server
            wait = match (&outcome, &held) {
                (None, Some(held)) => self.next_wait(wait, Some(held)),
                _ => self.next_wait(wait, outcome.as_ref()),
            };
        }
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
        let pending = self.post_aside(url, self.cfg.server_token.clone(), "the server");
        self.wait_for(pending, "the server")
    }

    /// Starts the hold of the job dispatcher; `None` when none is configured.
    fn ask_hold(&self) -> Option<mpsc::Receiver<Result<(), String>>> {
        let base = self.cfg.hold_url.as_ref()?;
        let url = format!("{}/dispatch/hold", base.trim_end_matches('/'));
        Some(self.post_aside(url, self.cfg.hold_token.clone(), "the job dispatcher"))
    }

    /// One POST on its own thread, with the short timeout of the stop.
    fn post_aside(
        &self,
        url: String,
        token: Option<String>,
        who: &'static str,
    ) -> mpsc::Receiver<Result<(), String>> {
        let timeout = self.cfg.fanout_timeout;
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let agent = http::AgentBuilder::new()
                .timeout_connect(timeout)
                .timeout(timeout)
                .build();
            let mut request = agent.post(&url).set("Content-Type", "application/json");
            if let Some(token) = &token {
                request = request.set("Authorization", &format!("Bearer {token}"));
            }
            let result = match request.send_string("{}") {
                Ok(_) => Ok(()),
                Err(http::Error::Status(code, _)) => Err(format!("{who} answered {code}")),
                Err(e) => Err(format!("{who} could not be reached: {e}")),
            };
            let _ = tx.send(result);
        });
        rx
    }

    fn wait_for(
        &self,
        pending: mpsc::Receiver<Result<(), String>>,
        who: &str,
    ) -> Result<(), String> {
        match pending.recv_timeout(self.cfg.fanout_timeout + Duration::from_millis(500)) {
            Ok(result) => result,
            Err(_) => Err(format!("{who} did not answer in time")),
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
    fn the_dispatcher_is_asked_to_hold_as_the_shared_fixture_says() {
        let fixture: Value =
            serde_json::from_str(include_str!("../tests/fixtures/job-dispatcher-hold.json"))
                .unwrap();
        let (url, rx) = fake_server("200 OK");
        let estop = Estop::new(EstopConfig {
            hold_url: Some(url),
            hold_token: Some("tok".into()),
            ..EstopConfig::default()
        });
        let out = estop.trigger("ana", "r", None);
        assert_eq!(out["dispatcher"], "held");
        assert_eq!(out["server"], "not_configured");
        let request = rx.recv_timeout(Duration::from_secs(2)).unwrap();
        let line = format!(
            "{} {}",
            fixture["hold"]["method"].as_str().unwrap(),
            fixture["hold"]["path"].as_str().unwrap()
        );
        assert!(request.starts_with(&line), "{request}");
        assert!(request
            .to_ascii_lowercase()
            .contains("authorization: bearer tok"));
        assert_eq!(estop.status()["dispatcher_held"], true);
        // already held: a repeat does not call again, and releasing here releases nothing there
        assert_eq!(
            estop.trigger("ana", "r", None)["dispatcher"],
            "already_held"
        );
        estop.release("admin");
        assert!(rx.recv_timeout(Duration::from_millis(300)).is_err());
    }

    #[test]
    fn a_dispatcher_that_does_not_hold_never_stops_the_stop() {
        let (url, _rx) = fake_server("404 Not Found");
        let estop = Estop::new(EstopConfig {
            hold_url: Some(url),
            fanout_timeout: Duration::from_millis(500),
            ..EstopConfig::default()
        });
        let out = estop.trigger("ana", "r", None);
        assert!(estop.engaged());
        assert!(
            out["dispatcher"].as_str().unwrap().starts_with("failed"),
            "{out}"
        );
        assert_eq!(estop.status()["dispatcher_held"], false);
    }

    #[test]
    fn a_hold_that_was_not_confirmed_is_asked_again_until_it_is() {
        let dir = std::env::temp_dir().join(format!("orch-estop-hold-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("estop.json");
        // a stop saved before the dispatcher confirmed anything (a file of an earlier version has no such field)
        std::fs::write(
            &path,
            r#"{"engaged":true,"by":"ana","reason":"r","since_ms":1,"server_confirmed":true,"triggers":1}"#,
        )
        .unwrap();
        let down = Estop::new(EstopConfig {
            state_file: Some(path.clone()),
            hold_url: Some("http://127.0.0.1:1".into()),
            fanout_timeout: Duration::from_millis(300),
            ..EstopConfig::default()
        });
        assert!(down.retry_hold().unwrap().is_err());
        assert_eq!(down.status()["dispatcher_held"], false);
        let (url, _rx) = fake_server("200 OK");
        let up = Estop::new(EstopConfig {
            state_file: Some(path),
            hold_url: Some(url),
            ..EstopConfig::default()
        });
        assert!(up.retry_hold().unwrap().is_ok());
        assert_eq!(up.status()["dispatcher_held"], true);
        assert!(up.retry_hold().is_none(), "nothing left to repeat");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn without_a_dispatcher_address_the_answers_are_the_ones_of_before() {
        let estop = Estop::new(EstopConfig::default());
        let out = estop.trigger("ana", "r", None);
        assert!(out.get("dispatcher").is_none());
        assert!(estop.status().get("dispatcher_held").is_none());
        assert!(estop.retry_hold().is_none());
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
    fn a_stop_the_server_did_not_confirm_is_repeated_until_it_does() {
        let dir = temp_dir("background-retry");
        let (url, rx) = server_with_statuses(vec![
            "503 Service Unavailable",
            "503 Service Unavailable",
            "200 OK",
        ]);
        let cfg = EstopConfig {
            state_file: Some(dir.join("estop.json")),
            server_url: Some(url),
            ..EstopConfig::default()
        };
        let estop = Estop::new(cfg.clone());
        assert!(estop.retry_unconfirmed().is_none(), "nothing is engaged");
        estop.trigger("ana", "r", None);
        // a restart finds the unconfirmed latch and goes on asking
        let restarted = Estop::new(cfg);
        assert!(restarted.retry_unconfirmed().unwrap().is_err());
        assert_eq!(restarted.status()["server_confirmed"], false);
        assert!(restarted.retry_unconfirmed().unwrap().is_ok());
        assert_eq!(restarted.status()["server_confirmed"], true);
        assert!(restarted.retry_unconfirmed().is_none(), "confirmed once");
        for _ in 0..3 {
            assert!(rx.recv_timeout(Duration::from_secs(2)).is_ok());
        }
    }

    #[test]
    fn nothing_is_repeated_when_it_is_off_released_or_not_configured() {
        let off = Estop::new(EstopConfig {
            server_url: Some("http://127.0.0.1:1".into()),
            retry_server: false,
            fanout_timeout: Duration::from_millis(300),
            ..EstopConfig::default()
        });
        off.trigger("ana", "r", None);
        assert!(off.retry_unconfirmed().is_none());
        assert_eq!(off.status()["server_retry"], false);

        let alone = Estop::new(EstopConfig::default());
        alone.trigger("ana", "r", None);
        assert!(alone.retry_unconfirmed().is_none());

        let released = Estop::new(EstopConfig {
            server_url: Some("http://127.0.0.1:1".into()),
            fanout_timeout: Duration::from_millis(300),
            ..EstopConfig::default()
        });
        released.trigger("ana", "r", None);
        assert_eq!(released.status()["server_retry"], true);
        released.release("admin");
        assert!(released.retry_unconfirmed().is_none());
    }

    #[test]
    fn the_wait_doubles_up_to_the_longest_and_starts_again() {
        let estop = Estop::new(EstopConfig::default());
        let failed: Result<(), String> = Err("down".into());
        let mut wait = Duration::from_secs(1);
        let mut seen = Vec::new();
        for _ in 0..7 {
            wait = estop.next_wait(wait, Some(&failed));
            seen.push(wait.as_secs());
        }
        assert_eq!(seen, [2, 4, 8, 16, 30, 30, 30]);
        assert_eq!(estop.next_wait(wait, Some(&Ok(()))).as_secs(), 1);
        assert_eq!(estop.next_wait(wait, None).as_secs(), 1);
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
