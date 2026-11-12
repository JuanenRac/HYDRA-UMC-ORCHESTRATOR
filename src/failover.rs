// =============================================================================
// HYDRA-UMC-ORCHESTRATOR - src/failover.rs
// Copyright (C) 2026 JuanenRac (Electro Hobby 3D) <electrohobby3d@gmail.com>
// GPL-3.0-or-later - see LICENSE
// =============================================================================
//! The decision of a standby orchestrator: when the primary has been silent long enough, and what proof of a stop to gather before the
//! registry is taken over.
//!
//! The standby polls the primary. Every answer keeps the newest copy of the records of `GET /replica`; every silence adds one to a count
//! that any answer resets. Only when the count reaches the limit does the standby act, and it acts once: it asks the robot that held each
//! in-flight mission (through a `StopSource`) whether it stopped, turns the answers into `StopEvidence` and hands them to
//! `replica::takeover`. A mission whose robot does not confirm stays `Unknown`: it is never dispatched a second time while it may still
//! be running. The network is behind two small traits so the whole decision is tested with a fake primary and a fake robot.

// The command `standby` (main.rs) drives the decision with `HttpPeer` (the primary over HTTP) and `NoStopProof`. The stop acknowledgement of a
// real robot does not exist yet, so no robot ever proves a stop and every in-flight mission stays `Unknown`. The helpers that only the tests
// use are allowed to be unused in the binary.
#![allow(dead_code)]

use std::io::Read;
use std::time::Duration;

use crate::mission::MissionRegistry;
use crate::replica::{takeover, ReplicaRecord, StopEvidence, TakeoverReport};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);
const CALL_TIMEOUT: Duration = Duration::from_secs(5);
/// The most bytes read from the answer of `GET /replica`.
const MAX_RECORDS_BYTES: u64 = 16 * 1024 * 1024;
/// Defaults and limits of the options of the `standby` command.
pub const DEFAULT_MISSES: u32 = 3;
pub const MAX_MISSES: u32 = 1000;
pub const DEFAULT_INTERVAL_S: u64 = 5;
pub const MAX_INTERVAL_S: u64 = 3600;

/// The primary orchestrator as the standby sees it.
pub trait Peer {
    /// `true` when the primary answered its health check.
    fn alive(&mut self) -> bool;
    /// The records of the primary, or why they could not be read.
    fn records(&mut self) -> Result<Vec<ReplicaRecord>, String>;
}

/// Where the proof that a robot stopped comes from: the acknowledgement of the robot to the stop request of the job-dispatcher.
pub trait StopSource {
    fn confirms_stopped(&mut self, owner: &str, mission_id: &str, attempt: u32) -> bool;
}

#[derive(Debug)]
pub enum Step {
    /// The primary answered.
    PrimaryAlive,
    /// The primary was silent; `n` answers missed in a row, fewer than the limit.
    PrimarySilent(u32),
    /// The takeover already happened; nothing more is done.
    AlreadyTookOver,
    /// The limit was reached and the registry was taken over.
    TookOver(TakeoverReport),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    /// Silent answers in a row before the standby acts; at least 1.
    pub misses: u32,
}

pub struct Standby {
    limits: Limits,
    missed: u32,
    latest: Vec<ReplicaRecord>,
    done: bool,
}

#[derive(serde::Deserialize)]
struct Shape {
    mission_id: String,
    status: String,
    owner: String,
    attempt: u32,
}

impl Standby {
    /// `None` when the limit is 0: a standby that acts on the first breath would take over on any hiccup.
    pub fn new(limits: Limits) -> Option<Self> {
        if limits.misses == 0 {
            return None;
        }
        Some(Standby {
            limits,
            missed: 0,
            latest: Vec::new(),
            done: false,
        })
    }

    pub fn missed(&self) -> u32 {
        self.missed
    }

    /// One round: look at the primary and, if it has been silent for too long, take the registry over.
    pub fn tick(
        &mut self,
        peer: &mut dyn Peer,
        robots: &mut dyn StopSource,
        registry: &mut MissionRegistry,
    ) -> Step {
        if self.done {
            return Step::AlreadyTookOver;
        }
        if peer.alive() {
            self.missed = 0;
            if let Ok(records) = peer.records() {
                self.latest = records;
            }
            return Step::PrimaryAlive;
        }
        self.missed += 1;
        if self.missed < self.limits.misses {
            return Step::PrimarySilent(self.missed);
        }
        let evidence = self.evidence(robots);
        let report = takeover(registry, &self.latest, &evidence);
        self.done = true;
        Step::TookOver(report)
    }

    /// The proofs for every in-flight record of the newest copy; a record that cannot be read gets none.
    fn evidence(&self, robots: &mut dyn StopSource) -> Vec<StopEvidence> {
        let mut out = Vec::new();
        for record in &self.latest {
            let Ok(shape) = serde_json::from_str::<Shape>(&record.value) else {
                continue;
            };
            if shape.status != "assigned" && shape.status != "running" {
                continue;
            }
            if robots.confirms_stopped(&shape.owner, &shape.mission_id, shape.attempt) {
                out.push(StopEvidence {
                    owner: shape.owner,
                    mission_id: shape.mission_id,
                    attempt: shape.attempt,
                });
            }
        }
        out
    }
}

/// The primary orchestrator over HTTP: it is alive when `GET /health` answers 2xx, and its records are those of `GET /replica?owner=NAME`.
/// The token, when there is one, goes as `Authorization: Bearer` on both calls.
pub struct HttpPeer {
    base: String,
    token: Option<String>,
    owner: String,
    agent: ureq::Agent,
}

impl HttpPeer {
    /// `owner` must be a plain name (it goes into the address without escaping); the options check that before.
    pub fn new(base_url: &str, token: Option<String>, owner: &str) -> Self {
        HttpPeer {
            base: base_url.trim_end_matches('/').to_string(),
            token,
            owner: owner.to_string(),
            agent: ureq::AgentBuilder::new()
                .timeout_connect(CONNECT_TIMEOUT)
                .timeout(CALL_TIMEOUT)
                .build(),
        }
    }

    fn get(&self, path: &str) -> ureq::Request {
        let request = self.agent.get(&format!("{}{}", self.base, path));
        match &self.token {
            Some(token) => request.set("Authorization", &format!("Bearer {token}")),
            None => request,
        }
    }
}

impl Peer for HttpPeer {
    fn alive(&mut self) -> bool {
        self.get("/health").call().is_ok()
    }

    fn records(&mut self) -> Result<Vec<ReplicaRecord>, String> {
        #[derive(serde::Deserialize)]
        struct Body {
            records: Vec<ReplicaRecord>,
        }
        let response = self
            .get(&format!("/replica?owner={}", self.owner))
            .call()
            .map_err(|error| match error {
                ureq::Error::Status(code, _) => format!("the primary answered {code}"),
                other => other.to_string(),
            })?;
        let mut text = String::new();
        response
            .into_reader()
            .take(MAX_RECORDS_BYTES)
            .read_to_string(&mut text)
            .map_err(|error| error.to_string())?;
        serde_json::from_str::<Body>(&text)
            .map(|body| body.records)
            .map_err(|error| error.to_string())
    }
}

/// The stop source of a standby while no robot can acknowledge a stop: it proves nothing, so every in-flight mission stays `Unknown` and is
/// never dispatched a second time while it may still be running. The safe default.
pub struct NoStopProof;

impl StopSource for NoStopProof {
    fn confirms_stopped(&mut self, _owner: &str, _mission_id: &str, _attempt: u32) -> bool {
        false
    }
}

/// One kept stop acknowledgement of the job-dispatcher (`GET /jobs/stop-acks`, on only with `replan.persist_stop_acks`).
#[derive(Debug, Clone, serde::Deserialize)]
struct DispatcherAck {
    #[serde(rename = "jobId")]
    job_id: String,
    #[serde(rename = "robotId")]
    robot_id: String,
    attempt: u32,
}

/// The stop proof read from the job-dispatcher: a mission is proven stopped when the dispatcher kept the acknowledgement of that robot for the job of
/// the same name and the same attempt. The list is fetched once, when the first mission asks, and a failure to read it proves nothing (every mission
/// then stays `Unknown`, the safe answer).
pub struct HttpStopProof {
    base: String,
    token: Option<String>,
    agent: ureq::Agent,
    acks: Option<Vec<DispatcherAck>>,
}

impl HttpStopProof {
    pub fn new(base_url: &str, token: Option<String>) -> Self {
        HttpStopProof {
            base: base_url.trim_end_matches('/').to_string(),
            token,
            agent: ureq::AgentBuilder::new()
                .timeout_connect(CONNECT_TIMEOUT)
                .timeout(CALL_TIMEOUT)
                .build(),
            acks: None,
        }
    }

    fn fetch(&self) -> Result<Vec<DispatcherAck>, String> {
        let request = self.agent.get(&format!("{}/jobs/stop-acks", self.base));
        let request = match &self.token {
            Some(token) => request.set("Authorization", &format!("Bearer {token}")),
            None => request,
        };
        let response = request.call().map_err(|error| match error {
            ureq::Error::Status(code, _) => format!("the dispatcher answered {code}"),
            other => other.to_string(),
        })?;
        let mut text = String::new();
        response
            .into_reader()
            .take(MAX_RECORDS_BYTES)
            .read_to_string(&mut text)
            .map_err(|error| error.to_string())?;
        serde_json::from_str::<Vec<DispatcherAck>>(&text).map_err(|error| error.to_string())
    }
}

impl StopSource for HttpStopProof {
    fn confirms_stopped(&mut self, owner: &str, mission_id: &str, attempt: u32) -> bool {
        if self.acks.is_none() {
            // an unreadable list is remembered as empty: asking again for every mission would only repeat the failure
            self.acks = Some(self.fetch().unwrap_or_default());
        }
        self.acks.as_ref().is_some_and(|acks| {
            acks.iter()
                .any(|a| a.robot_id == owner && a.job_id == mission_id && a.attempt == attempt)
        })
    }
}

/// Polls until the takeover happens and returns its report. `wait` is called between two rounds that did not end in a takeover, `seen`
/// is told what each round found.
pub fn run_until_takeover(
    standby: &mut Standby,
    peer: &mut dyn Peer,
    robots: &mut dyn StopSource,
    registry: &mut MissionRegistry,
    mut wait: impl FnMut(),
    mut seen: impl FnMut(&Step),
) -> TakeoverReport {
    loop {
        let step = standby.tick(peer, robots, registry);
        seen(&step);
        match step {
            Step::TookOver(report) => return report,
            // `done` is only set together with a takeover, which already returned
            Step::AlreadyTookOver => return TakeoverReport::default(),
            Step::PrimaryAlive | Step::PrimarySilent(_) => wait(),
        }
    }
}

/// What the `standby` command was asked for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StandbyOptions {
    pub primary_url: String,
    pub token: Option<String>,
    /// The name the primary replicates under (its `--orchestrator-name`).
    pub primary_name: String,
    pub misses: u32,
    pub interval: Duration,
    pub data_dir: String,
    /// The job-dispatcher whose kept stop acknowledgements prove a stop (`--dispatcher-url`); none by default, and then no stop is ever proven.
    pub dispatcher_url: Option<String>,
    pub dispatcher_token: Option<String>,
}

impl StandbyOptions {
    /// `flag` gives the value of a command-line flag, `env` that of an environment variable. The token comes from `--primary-token` or
    /// `ORCHESTRATOR_PRIMARY_TOKEN`; an empty one is no token.
    pub fn from_flags(
        flag: &dyn Fn(&str) -> Option<String>,
        env: &dyn Fn(&str) -> Option<String>,
    ) -> Result<Self, String> {
        let url = flag("--primary-url")
            .filter(|u| !u.is_empty())
            .ok_or("--primary-url is required (the address of the primary orchestrator)")?;
        if !(url.starts_with("http://") || url.starts_with("https://")) {
            return Err(format!(
                "--primary-url must start with http:// or https://, not {url:?}"
            ));
        }
        let misses = match flag("--misses") {
            None => DEFAULT_MISSES,
            Some(value) => match value.parse::<u32>() {
                Ok(n) if (1..=MAX_MISSES).contains(&n) => n,
                _ => {
                    return Err(format!(
                        "--misses must be a whole number from 1 to {MAX_MISSES}, not {value:?}"
                    ))
                }
            },
        };
        let interval_s = match flag("--interval-s") {
            None => DEFAULT_INTERVAL_S,
            Some(value) => match value.parse::<u64>() {
                Ok(n) if (1..=MAX_INTERVAL_S).contains(&n) => n,
                _ => {
                    return Err(format!(
                    "--interval-s must be a whole number from 1 to {MAX_INTERVAL_S}, not {value:?}"
                ))
                }
            },
        };
        let primary_name = flag("--primary-name").unwrap_or_else(|| "orchestrator".to_string());
        if !crate::mission::is_valid_mission_id(&primary_name) {
            return Err(format!(
                "--primary-name must be a plain name (letters, digits, - _ .), not {primary_name:?}"
            ));
        }
        let token = flag("--primary-token")
            .or_else(|| env("ORCHESTRATOR_PRIMARY_TOKEN"))
            .filter(|t| !t.is_empty());
        let dispatcher_url = flag("--dispatcher-url").filter(|u| !u.is_empty());
        if let Some(u) = &dispatcher_url {
            if !(u.starts_with("http://") || u.starts_with("https://")) {
                return Err(format!(
                    "--dispatcher-url must start with http:// or https://, not {u:?}"
                ));
            }
        }
        let dispatcher_token = flag("--dispatcher-token")
            .or_else(|| env("ORCHESTRATOR_DISPATCHER_TOKEN"))
            .filter(|t| !t.is_empty());
        Ok(StandbyOptions {
            dispatcher_url,
            dispatcher_token,
            primary_url: url,
            token,
            primary_name,
            misses,
            interval: Duration::from_secs(interval_s),
            data_dir: flag("--data-dir").unwrap_or_else(|| "data".to_string()),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mission::{Mission, MissionState};
    use crate::replica::record_of;

    struct FakePrimary {
        script: Vec<bool>,
        pos: usize,
        records: Vec<ReplicaRecord>,
        records_fail: bool,
    }
    impl FakePrimary {
        fn new(script: Vec<bool>, records: Vec<ReplicaRecord>) -> Self {
            FakePrimary {
                script,
                pos: 0,
                records,
                records_fail: false,
            }
        }
    }
    impl Peer for FakePrimary {
        fn alive(&mut self) -> bool {
            let a = self.script.get(self.pos).copied().unwrap_or(false);
            self.pos += 1;
            a
        }
        fn records(&mut self) -> Result<Vec<ReplicaRecord>, String> {
            if self.records_fail {
                Err("unreadable".into())
            } else {
                Ok(self.records.clone())
            }
        }
    }

    /// A robot that confirms only the missions it was told to, and remembers what it was asked.
    struct FakeRobot {
        stops: Vec<(String, String, u32)>,
        asked: Vec<(String, String, u32)>,
    }
    impl FakeRobot {
        fn new(stops: Vec<(String, String, u32)>) -> Self {
            FakeRobot {
                stops,
                asked: vec![],
            }
        }
    }
    impl StopSource for FakeRobot {
        fn confirms_stopped(&mut self, owner: &str, mission_id: &str, attempt: u32) -> bool {
            let key = (owner.to_string(), mission_id.to_string(), attempt);
            self.asked.push(key.clone());
            self.stops.contains(&key)
        }
    }

    fn running(id: &str, node: &str) -> Mission {
        let mut m = Mission::new(id);
        m.dispatch(node).unwrap();
        m.start().unwrap();
        m
    }

    fn three_records() -> Vec<ReplicaRecord> {
        vec![
            record_of(&running("m-1", "node-a"), "orch-a"),
            record_of(&running("m-2", "node-b"), "orch-a"),
            record_of(&Mission::new("m-3"), "orch-a"),
        ]
    }

    fn proof(owner: &str, id: &str, attempt: u32) -> (String, String, u32) {
        (owner.to_string(), id.to_string(), attempt)
    }

    #[test]
    fn a_limit_of_zero_is_refused() {
        assert!(Standby::new(Limits { misses: 0 }).is_none());
        assert!(Standby::new(Limits { misses: 1 }).is_some());
    }

    #[test]
    fn it_waits_for_the_limit_and_then_takes_over_once() {
        let mut standby = Standby::new(Limits { misses: 3 }).unwrap();
        let mut primary = FakePrimary::new(vec![true, false, false, false, false], three_records());
        let mut robot = FakeRobot::new(vec![proof("node-a", "m-1", 1)]);
        let mut reg = MissionRegistry::new();
        assert!(matches!(
            standby.tick(&mut primary, &mut robot, &mut reg),
            Step::PrimaryAlive
        ));
        assert!(matches!(
            standby.tick(&mut primary, &mut robot, &mut reg),
            Step::PrimarySilent(1)
        ));
        assert!(matches!(
            standby.tick(&mut primary, &mut robot, &mut reg),
            Step::PrimarySilent(2)
        ));
        assert!(
            robot.asked.is_empty(),
            "no robot is asked before the limit is reached"
        );
        let Step::TookOver(report) = standby.tick(&mut primary, &mut robot, &mut reg) else {
            panic!("the third silence must take over");
        };
        assert_eq!(report.adopted.len(), 3);
        assert_eq!(report.requeued, vec!["m-1".to_string()]);
        assert_eq!(report.unconfirmed, vec!["m-2".to_string()]);
        // the queued mission needs no proof and is not asked about
        assert_eq!(robot.asked.len(), 2);
        assert!(matches!(
            standby.tick(&mut primary, &mut robot, &mut reg),
            Step::AlreadyTookOver
        ));
        assert_eq!(robot.asked.len(), 2, "a second takeover asks nobody");
    }

    #[test]
    fn a_confirmed_mission_goes_back_to_pending_with_a_newer_attempt_and_the_other_stays_unknown() {
        let mut standby = Standby::new(Limits { misses: 1 }).unwrap();
        let mut primary = FakePrimary::new(vec![true, false], three_records());
        let mut robot = FakeRobot::new(vec![proof("node-a", "m-1", 1)]);
        let mut reg = MissionRegistry::new();
        standby.tick(&mut primary, &mut robot, &mut reg);
        standby.tick(&mut primary, &mut robot, &mut reg);
        let m1 = reg.get("m-1").unwrap();
        assert!(matches!(m1.state, MissionState::Pending));
        assert_eq!(m1.attempt, 2);
        let m2 = reg.get("m-2").unwrap();
        assert!(matches!(m2.state, MissionState::Unknown { .. }));
    }

    #[test]
    fn a_robot_that_confirms_another_attempt_proves_nothing() {
        let mut standby = Standby::new(Limits { misses: 1 }).unwrap();
        let mut primary = FakePrimary::new(vec![true, false], three_records());
        let mut robot = FakeRobot::new(vec![proof("node-a", "m-1", 0)]);
        let mut reg = MissionRegistry::new();
        standby.tick(&mut primary, &mut robot, &mut reg);
        let Step::TookOver(report) = standby.tick(&mut primary, &mut robot, &mut reg) else {
            panic!("must take over");
        };
        assert!(report.requeued.is_empty());
        assert_eq!(report.unconfirmed.len(), 2);
    }

    #[test]
    fn an_answer_resets_the_count_so_a_flapping_primary_is_not_replaced() {
        let mut standby = Standby::new(Limits { misses: 3 }).unwrap();
        let mut primary = FakePrimary::new(
            vec![false, false, true, false, false, true, false],
            three_records(),
        );
        let mut robot = FakeRobot::new(vec![]);
        let mut reg = MissionRegistry::new();
        for _ in 0..7 {
            let step = standby.tick(&mut primary, &mut robot, &mut reg);
            assert!(!matches!(step, Step::TookOver(_)));
        }
        assert_eq!(standby.missed(), 1);
        assert!(robot.asked.is_empty());
        assert!(reg.get("m-1").is_none());
    }

    #[test]
    fn nothing_is_adopted_when_the_primary_never_answered() {
        let mut standby = Standby::new(Limits { misses: 1 }).unwrap();
        let mut primary = FakePrimary::new(vec![false], three_records());
        let mut robot = FakeRobot::new(vec![]);
        let mut reg = MissionRegistry::new();
        let Step::TookOver(report) = standby.tick(&mut primary, &mut robot, &mut reg) else {
            panic!("must act");
        };
        assert!(
            report.adopted.is_empty(),
            "no copy of the records was ever read"
        );
    }

    #[test]
    fn the_newest_readable_copy_is_kept_when_a_later_read_fails() {
        let mut standby = Standby::new(Limits { misses: 1 }).unwrap();
        let mut primary = FakePrimary::new(vec![true, true, false], three_records());
        let mut robot = FakeRobot::new(vec![]);
        let mut reg = MissionRegistry::new();
        standby.tick(&mut primary, &mut robot, &mut reg);
        primary.records_fail = true;
        standby.tick(&mut primary, &mut robot, &mut reg);
        let Step::TookOver(report) = standby.tick(&mut primary, &mut robot, &mut reg) else {
            panic!("must act");
        };
        assert_eq!(report.adopted.len(), 3);
    }

    #[test]
    fn a_record_that_cannot_be_read_is_not_asked_about() {
        let mut standby = Standby::new(Limits { misses: 1 }).unwrap();
        let broken = ReplicaRecord {
            key: "mission/x".into(),
            value: "not json".into(),
            generation: 1,
        };
        let mut primary = FakePrimary::new(vec![true, false], vec![broken]);
        let mut robot = FakeRobot::new(vec![]);
        let mut reg = MissionRegistry::new();
        standby.tick(&mut primary, &mut robot, &mut reg);
        let Step::TookOver(report) = standby.tick(&mut primary, &mut robot, &mut reg) else {
            panic!("must act");
        };
        assert!(robot.asked.is_empty());
        assert_eq!(report.rejected.len(), 1);
    }

    #[test]
    fn a_mission_the_standby_already_holds_is_left_alone() {
        let mut standby = Standby::new(Limits { misses: 1 }).unwrap();
        let mut primary = FakePrimary::new(vec![true, false], three_records());
        let mut robot = FakeRobot::new(vec![proof("node-a", "m-1", 1)]);
        let mut reg = MissionRegistry::new();
        reg.insert_adopted(Mission::new("m-1"));
        standby.tick(&mut primary, &mut robot, &mut reg);
        let Step::TookOver(report) = standby.tick(&mut primary, &mut robot, &mut reg) else {
            panic!("must act");
        };
        assert_eq!(report.existing, vec!["m-1".to_string()]);
        assert_eq!(reg.get("m-1").unwrap().attempt, 0);
    }

    // ---- the primary over HTTP ----

    use std::collections::HashMap;
    use std::io::Write as _;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{mpsc, Arc};

    /// A fake primary: `GET /health` answers 200 while `up` is set and 503 after; `GET /replica...` answers `replica` (status, body). Each request
    /// line and header block it receives goes to the channel.
    fn fake_primary(
        up: Arc<AtomicBool>,
        replica: (u16, String),
    ) -> (String, mpsc::Receiver<String>) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { break };
                let mut raw = Vec::new();
                let mut buf = [0u8; 4096];
                loop {
                    let n = stream.read(&mut buf).unwrap_or(0);
                    raw.extend_from_slice(&buf[..n]);
                    if n == 0 || raw.windows(4).any(|w| w == b"\r\n\r\n") {
                        break;
                    }
                }
                let text = String::from_utf8_lossy(&raw).to_string();
                let (status, body) = if text.starts_with("GET /health") {
                    if up.load(Ordering::SeqCst) {
                        (200, "{}".to_string())
                    } else {
                        (503, "{}".to_string())
                    }
                } else if text.starts_with("GET /replica") {
                    replica.clone()
                } else {
                    (404, "{}".to_string())
                };
                let _ = tx.send(text);
                let _ = write!(
                    stream,
                    "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
            }
        });
        (url, rx)
    }

    fn replica_body(records: &[ReplicaRecord]) -> String {
        serde_json::json!({ "records": records }).to_string()
    }

    #[test]
    fn the_http_peer_asks_health_and_the_records_of_the_named_owner_with_the_token() {
        let up = Arc::new(AtomicBool::new(true));
        let (url, rx) = fake_primary(up, (200, replica_body(&three_records())));
        let mut peer = HttpPeer::new(&format!("{url}/"), Some("s3cret".into()), "orch-a");
        assert!(peer.alive());
        let health = rx.recv_timeout(Duration::from_secs(2)).unwrap();
        assert!(health.starts_with("GET /health"), "{health}");
        assert!(health.contains("Bearer s3cret"));
        let records = peer.records().unwrap();
        assert_eq!(records.len(), 3);
        let asked = rx.recv_timeout(Duration::from_secs(2)).unwrap();
        assert!(asked.starts_with("GET /replica?owner=orch-a "), "{asked}");
        assert!(asked.contains("Bearer s3cret"));
    }

    #[test]
    fn the_http_peer_sends_no_authorization_without_a_token() {
        let up = Arc::new(AtomicBool::new(true));
        let (url, rx) = fake_primary(up, (200, replica_body(&[])));
        let mut peer = HttpPeer::new(&url, None, "orchestrator");
        assert!(peer.alive());
        let health = rx.recv_timeout(Duration::from_secs(2)).unwrap();
        assert!(!health.to_ascii_lowercase().contains("authorization"));
    }

    #[test]
    fn a_primary_that_answers_an_error_or_nothing_is_not_alive() {
        let up = Arc::new(AtomicBool::new(false));
        let (url, _rx) = fake_primary(up, (200, replica_body(&[])));
        assert!(!HttpPeer::new(&url, None, "orchestrator").alive());
        // nobody listens on port 1
        assert!(!HttpPeer::new("http://127.0.0.1:1", None, "orchestrator").alive());
    }

    #[test]
    fn records_that_cannot_be_read_are_an_error_with_the_reason() {
        let up = Arc::new(AtomicBool::new(true));
        let (url, _rx) = fake_primary(up.clone(), (500, "{}".to_string()));
        let error = HttpPeer::new(&url, None, "orchestrator")
            .records()
            .unwrap_err();
        assert!(error.contains("500"), "{error}");
        let (url, _rx) = fake_primary(up.clone(), (200, "not json".to_string()));
        assert!(HttpPeer::new(&url, None, "orchestrator").records().is_err());
        let (url, _rx) = fake_primary(up, (200, r#"{"records":[{"key":1}]}"#.to_string()));
        assert!(HttpPeer::new(&url, None, "orchestrator").records().is_err());
        assert!(HttpPeer::new("http://127.0.0.1:1", None, "orchestrator")
            .records()
            .is_err());
    }

    #[test]
    fn a_primary_that_goes_silent_is_taken_over_with_every_mission_in_flight_left_unknown() {
        let up = Arc::new(AtomicBool::new(true));
        let (url, _rx) = fake_primary(up.clone(), (200, replica_body(&three_records())));
        let mut peer = HttpPeer::new(&url, None, "orch-a");
        let mut standby = Standby::new(Limits { misses: 2 }).unwrap();
        let mut reg = MissionRegistry::new();
        let flip = up.clone();
        let mut rounds = 0;
        let mut kinds = Vec::new();
        let report = run_until_takeover(
            &mut standby,
            &mut peer,
            &mut NoStopProof,
            &mut reg,
            || {
                rounds += 1;
                if rounds == 1 {
                    flip.store(false, Ordering::SeqCst); // the primary dies after the first round
                }
            },
            |step| {
                kinds.push(match step {
                    Step::PrimaryAlive => "alive",
                    Step::PrimarySilent(_) => "silent",
                    Step::AlreadyTookOver => "done",
                    Step::TookOver(_) => "took over",
                })
            },
        );
        assert_eq!(kinds, vec!["alive", "silent", "took over"]);
        assert_eq!(report.adopted.len(), 3);
        // nobody proved a stop: the two missions in flight stay Unknown and are not requeued
        assert!(report.requeued.is_empty());
        assert_eq!(report.unconfirmed.len(), 2);
        assert!(matches!(
            reg.get("m-1").unwrap().state,
            MissionState::Unknown { .. }
        ));
        assert!(matches!(
            reg.get("m-3").unwrap().state,
            MissionState::Pending
        ));
    }

    #[test]
    fn the_runner_waits_only_between_rounds_that_did_not_take_over() {
        let mut standby = Standby::new(Limits { misses: 2 }).unwrap();
        let mut primary = FakePrimary::new(vec![false, false], three_records());
        let mut reg = MissionRegistry::new();
        let mut waits = 0;
        let report = run_until_takeover(
            &mut standby,
            &mut primary,
            &mut NoStopProof,
            &mut reg,
            || waits += 1,
            |_| {},
        );
        assert_eq!(waits, 1);
        // the primary never answered, so there was no copy to adopt
        assert!(report.adopted.is_empty());
        // asked again after the takeover, it does nothing and returns at once
        let again = run_until_takeover(
            &mut standby,
            &mut primary,
            &mut NoStopProof,
            &mut reg,
            || waits += 1,
            |_| {},
        );
        assert_eq!(waits, 1);
        assert!(again.adopted.is_empty());
    }

    #[test]
    fn no_stop_proof_confirms_nothing() {
        assert!(!NoStopProof.confirms_stopped("node-a", "m-1", 1));
    }

    // ---- the options of the command ----

    fn options(flags: &[(&str, &str)], env: &[(&str, &str)]) -> Result<StandbyOptions, String> {
        let flags: HashMap<String, String> = flags
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        let env: HashMap<String, String> = env
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        StandbyOptions::from_flags(&|k| flags.get(k).cloned(), &|k| env.get(k).cloned())
    }

    #[test]
    fn the_options_have_conservative_defaults() {
        let o = options(&[("--primary-url", "http://10.0.0.2:8114")], &[]).unwrap();
        assert_eq!(o.primary_url, "http://10.0.0.2:8114");
        assert_eq!(o.misses, DEFAULT_MISSES);
        assert_eq!(o.interval, Duration::from_secs(DEFAULT_INTERVAL_S));
        assert_eq!(o.primary_name, "orchestrator");
        assert_eq!(o.data_dir, "data");
        assert!(o.token.is_none());
    }

    #[test]
    fn the_primary_address_is_required_and_must_be_http() {
        assert!(options(&[], &[])
            .unwrap_err()
            .contains("--primary-url is required"));
        assert!(options(&[("--primary-url", "")], &[]).is_err());
        for bad in ["ftp://x", "10.0.0.2:8114", "ws://x"] {
            let error = options(&[("--primary-url", bad)], &[]).unwrap_err();
            assert!(error.contains("http:// or https://"), "{error}");
        }
        assert!(options(&[("--primary-url", "https://primary.local")], &[]).is_ok());
    }

    #[test]
    fn the_limit_and_the_interval_are_checked() {
        let base = ("--primary-url", "http://x");
        for bad in ["0", "1001", "-1", "abc", "2.5", ""] {
            let error = options(&[base, ("--misses", bad)], &[]).unwrap_err();
            assert!(error.contains("--misses"), "{error}");
        }
        for bad in ["0", "3601", "abc", "1.5"] {
            let error = options(&[base, ("--interval-s", bad)], &[]).unwrap_err();
            assert!(error.contains("--interval-s"), "{error}");
        }
        let o = options(&[base, ("--misses", "1000"), ("--interval-s", "3600")], &[]).unwrap();
        assert_eq!(o.misses, 1000);
        assert_eq!(o.interval, Duration::from_secs(3600));
        let o = options(&[base, ("--misses", "1"), ("--interval-s", "1")], &[]).unwrap();
        assert_eq!((o.misses, o.interval), (1, Duration::from_secs(1)));
    }

    #[test]
    fn the_primary_name_must_be_plain_because_it_goes_into_the_address() {
        let base = ("--primary-url", "http://x");
        assert_eq!(
            options(&[base, ("--primary-name", "orch-a.1")], &[])
                .unwrap()
                .primary_name,
            "orch-a.1"
        );
        for bad in ["a b", "a&b=1", "a/b", "", "a?x"] {
            assert!(
                options(&[base, ("--primary-name", bad)], &[]).is_err(),
                "{bad}"
            );
        }
    }

    #[test]
    fn the_token_comes_from_the_flag_or_the_environment_and_empty_is_none() {
        let base = ("--primary-url", "http://x");
        assert_eq!(
            options(&[base], &[("ORCHESTRATOR_PRIMARY_TOKEN", "from-env")])
                .unwrap()
                .token
                .as_deref(),
            Some("from-env")
        );
        assert_eq!(
            options(
                &[base, ("--primary-token", "from-flag")],
                &[("ORCHESTRATOR_PRIMARY_TOKEN", "from-env")]
            )
            .unwrap()
            .token
            .as_deref(),
            Some("from-flag")
        );
        assert!(options(&[base], &[("ORCHESTRATOR_PRIMARY_TOKEN", "")])
            .unwrap()
            .token
            .is_none());
        assert!(options(&[base, ("--primary-token", "")], &[])
            .unwrap()
            .token
            .is_none());
    }

    // ---- the stop proof kept by the job-dispatcher ----

    /// A fake dispatcher: `GET /jobs/stop-acks` answers `(status, body)`; the request text goes to the channel.
    fn fake_dispatcher(answer: (u16, String)) -> (String, mpsc::Receiver<String>) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { break };
                let mut raw = Vec::new();
                let mut buf = [0u8; 4096];
                loop {
                    let n = stream.read(&mut buf).unwrap_or(0);
                    raw.extend_from_slice(&buf[..n]);
                    if n == 0
                        || raw.windows(4).any(|w| w == b"\r\n\r\n")
                    {
                        break;
                    }
                }
                let text = String::from_utf8_lossy(&raw).to_string();
                let (status, body) = if text.starts_with("GET /jobs/stop-acks") {
                    answer.clone()
                } else {
                    (404, "{}".to_string())
                };
                let _ = tx.send(text);
                let _ = write!(
                    stream,
                    "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
            }
        });
        (url, rx)
    }

    const ACKS: &str = r#"[{"jobId":"m-1","robotId":"node-a","attempt":1,"ackedAtMs":5}]"#;

    #[test]
    fn a_kept_acknowledgement_proves_the_stop_of_that_robot_job_and_attempt() {
        let (url, rx) = fake_dispatcher((200, ACKS.to_string()));
        let mut proof = HttpStopProof::new(&url, Some("secret".to_string()));
        assert!(proof.confirms_stopped("node-a", "m-1", 1));
        // another robot, another job and another attempt are not proven
        assert!(!proof.confirms_stopped("node-b", "m-1", 1));
        assert!(!proof.confirms_stopped("node-a", "m-2", 1));
        assert!(!proof.confirms_stopped("node-a", "m-1", 2));
        let asked = rx.recv().unwrap();
        assert!(asked.starts_with("GET /jobs/stop-acks "), "{asked}");
        assert!(asked.contains("Bearer secret"), "{asked}");
        // the list is fetched once, whatever the number of missions
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn an_unreadable_list_of_acknowledgements_proves_nothing() {
        for answer in [
            (404, "{}".to_string()),
            (200, "not json".to_string()),
            (200, r#"{"jobId":"m-1"}"#.to_string()),
        ] {
            let (url, _rx) = fake_dispatcher(answer);
            let mut proof = HttpStopProof::new(&url, None);
            assert!(!proof.confirms_stopped("node-a", "m-1", 1));
        }
        // nobody listens
        let mut proof = HttpStopProof::new("http://127.0.0.1:1", None);
        assert!(!proof.confirms_stopped("node-a", "m-1", 1));
    }

    #[test]
    fn the_standby_takes_over_with_the_proof_of_the_dispatcher() {
        let (url, _rx) = fake_dispatcher((200, ACKS.to_string()));
        let mut proof = HttpStopProof::new(&url, None);
        let mut standby = Standby::new(Limits { misses: 1 }).unwrap();
        let mut primary = FakePrimary::new(vec![true, false], three_records());
        let mut reg = MissionRegistry::new();
        standby.tick(&mut primary, &mut proof, &mut reg);
        let Step::TookOver(report) = standby.tick(&mut primary, &mut proof, &mut reg) else {
            panic!("must take over");
        };
        // m-1 has an acknowledgement kept by the dispatcher and goes back to pending; m-2 has none and stays unknown
        assert_eq!(report.requeued, vec!["m-1".to_string()]);
        assert_eq!(report.unconfirmed, vec!["m-2".to_string()]);
    }

    #[test]
    fn the_dispatcher_options_are_off_by_default_and_checked() {
        let base = ("--primary-url", "http://x");
        let o = options(&[base], &[]).unwrap();
        assert!(o.dispatcher_url.is_none() && o.dispatcher_token.is_none());
        let error = options(&[base, ("--dispatcher-url", "ftp://d")], &[]).unwrap_err();
        assert!(error.contains("--dispatcher-url"), "{error}");
        let o = options(
            &[base, ("--dispatcher-url", "http://d:8080")],
            &[("ORCHESTRATOR_DISPATCHER_TOKEN", "t")],
        )
        .unwrap();
        assert_eq!(o.dispatcher_url.as_deref(), Some("http://d:8080"));
        assert_eq!(o.dispatcher_token.as_deref(), Some("t"));
        assert!(options(&[base, ("--dispatcher-url", "")], &[])
            .unwrap()
            .dispatcher_url
            .is_none());
    }
}
