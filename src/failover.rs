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

// The production `Peer` and `StopSource` (HTTP to the primary, the stop acknowledgement of a real robot) are not wired yet: only the
// decision and its tests exist, driven by fakes.
#![allow(dead_code)]

use crate::mission::MissionRegistry;
use crate::replica::{takeover, ReplicaRecord, StopEvidence, TakeoverReport};

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
}
