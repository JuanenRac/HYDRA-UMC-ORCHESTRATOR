// =============================================================================
// HYDRA-UMC-ORCHESTRATOR - src/simulation_bench.rs
// Copyright (C) 2026 JuanenRac (Electro Hobby 3D) <electrohobby3d@gmail.com>
// GPL-3.0-or-later - see LICENSE
// =============================================================================
//! The simulation bench of the failover: a simulated primary orchestrator whose mission registry changes while it is alive, whose network
//! drops answers in runs (a partition), whose copy of the records sometimes cannot be read, and simulated robots, some of which never confirm a
//! stop. The standby runs the real decision against them, round by round, from a fixed seed. What must hold whatever the seed says: it acts only
//! after the limit of silent answers in a row, exactly once; a mission that was in flight is requeued only on the proof of its own robot and is
//! otherwise kept `Unknown` (never dispatched twice); a mission the standby already held is not touched; a forged record is rejected and does not
//! stop the others. Nothing here touches a robot or a network: a pass means "passed in simulation".

use crate::failover::{Limits, Peer, Standby, Step, StopSource};
use crate::mission::{Mission, MissionRegistry, MissionState};
use crate::replica::{record_of, ReplicaRecord};

const ROBOTS: [&str; 3] = ["node-a", "node-b", "node-c"];
const MISSIONS: usize = 12;

/// A small generator with a fixed seed.
struct Lcg(u64);

impl Lcg {
    fn unit(&mut self) -> f64 {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        ((self.0 >> 11) as f64) / ((1u64 << 53) as f64)
    }
    fn below(&mut self, n: usize) -> usize {
        ((self.unit() * n as f64) as usize).min(n - 1)
    }
    fn chance(&mut self, p: f64) -> bool {
        self.unit() < p
    }
}

/// Moves a mission to a random state through the real transitions (a transition that is refused leaves it where it was).
fn drive(mission: &mut Mission, rng: &mut Lcg) {
    let robot = ROBOTS[rng.below(ROBOTS.len())];
    match rng.below(6) {
        0 => {}
        1 => {
            let _ = mission.dispatch(robot);
        }
        2 => {
            let _ = mission.dispatch(robot);
            let _ = mission.start();
        }
        3 => {
            let _ = mission.dispatch(robot);
            let _ = mission.start();
            let _ = mission.complete();
        }
        4 => {
            let _ = mission.dispatch(robot);
            let _ = mission.fail("simulated failure");
        }
        _ => {
            let _ = mission.cancel();
        }
    }
}

fn fleet(rng: &mut Lcg) -> Vec<Mission> {
    (0..MISSIONS)
        .map(|i| {
            let mut mission = Mission::new(format!("m-{i}"));
            drive(&mut mission, rng);
            mission
        })
        .collect()
}

fn records_of(missions: &[Mission]) -> Vec<ReplicaRecord> {
    missions.iter().map(|m| record_of(m, "orch-a")).collect()
}

fn in_flight(mission: &Mission) -> bool {
    matches!(
        mission.state,
        MissionState::Dispatched { .. } | MissionState::InProgress { .. }
    )
}

/// The primary as the standby sees it: it answers while its script says so, and its registry keeps changing while it does.
struct SimPrimary {
    alive_script: Vec<bool>,
    position: usize,
    missions: Vec<Mission>,
    rng: Lcg,
    /// Whether the next read of the records fails.
    unreadable: bool,
    /// The records of the last read that worked.
    last_read: Option<Vec<ReplicaRecord>>,
}

impl SimPrimary {
    fn new(seed: u64, alive_script: Vec<bool>) -> SimPrimary {
        let mut rng = Lcg(seed);
        let missions = fleet(&mut rng);
        SimPrimary {
            alive_script,
            position: 0,
            missions,
            rng,
            unreadable: false,
            last_read: None,
        }
    }
}

impl Peer for SimPrimary {
    fn alive(&mut self) -> bool {
        let alive = self
            .alive_script
            .get(self.position)
            .copied()
            .unwrap_or(false);
        self.position += 1;
        if alive {
            // the primary goes on working: one mission moves
            let index = self.rng.below(self.missions.len());
            drive(&mut self.missions[index], &mut self.rng);
            self.unreadable = self.rng.chance(0.2);
        }
        alive
    }

    fn records(&mut self) -> Result<Vec<ReplicaRecord>, String> {
        if self.unreadable {
            self.last_read = None;
            return Err("the answer was cut".into());
        }
        let records = records_of(&self.missions);
        self.last_read = Some(records.clone());
        Ok(records)
    }
}

/// Robots that confirm a stop only for the missions they were told about, and never for a silent robot.
struct SimRobots {
    confirm: Vec<(String, String, u32)>,
}

impl StopSource for SimRobots {
    fn confirms_stopped(&mut self, owner: &str, mission_id: &str, attempt: u32) -> bool {
        self.confirm
            .iter()
            .any(|(o, m, a)| o == owner && m == mission_id && *a == attempt)
    }
}

/// A script of answers: runs of silence (partitions) between runs of answers.
fn script(rng: &mut Lcg, rounds: usize) -> Vec<bool> {
    let mut out = Vec::new();
    while out.len() < rounds {
        let alive = rng.chance(0.6);
        let run = 1 + rng.below(5);
        for _ in 0..run {
            out.push(alive);
        }
    }
    out.truncate(rounds);
    out
}

/// What the standby is expected to say at each round for `script`, with a limit of `misses`.
fn expected_steps(script: &[bool], misses: u32) -> Vec<&'static str> {
    let mut out = Vec::new();
    let mut missed = 0;
    let mut done = false;
    for &alive in script {
        if done {
            out.push("already");
        } else if alive {
            missed = 0;
            out.push("alive");
        } else {
            missed += 1;
            if missed < misses {
                out.push("silent");
            } else {
                done = true;
                out.push("took");
            }
        }
    }
    out
}

fn name_of(step: &Step) -> &'static str {
    match step {
        Step::PrimaryAlive => "alive",
        Step::PrimarySilent(_) => "silent",
        Step::AlreadyTookOver => "already",
        Step::TookOver(_) => "took",
    }
}

#[test]
fn the_standby_acts_after_the_limit_of_silent_answers_in_a_row_and_exactly_once() {
    for seed in 1..=40u64 {
        let mut rng = Lcg(seed);
        let misses = 1 + rng.below(4) as u32;
        let rounds = 60;
        let answers = script(&mut rng, rounds);
        let expected = expected_steps(&answers, misses);
        let mut primary = SimPrimary::new(seed, answers);
        let mut robots = SimRobots { confirm: vec![] };
        let mut registry = MissionRegistry::new();
        let mut standby = Standby::new(Limits { misses }).unwrap();
        for (round, want) in expected.iter().enumerate() {
            let step = standby.tick(&mut primary, &mut robots, &mut registry);
            assert_eq!(name_of(&step), *want, "seed {seed}, round {round}");
            if let Step::PrimarySilent(n) = step {
                assert!(n < misses, "seed {seed}, round {round}");
            }
        }
    }
}

#[test]
fn a_mission_in_flight_is_requeued_only_on_the_proof_of_its_own_robot_and_is_otherwise_kept_unknown(
) {
    let mut took_over = 0;
    for seed in 1..=60u64 {
        let mut rng = Lcg(seed ^ 0xABCD);
        let answers = script(&mut rng, 80);
        let mut primary = SimPrimary::new(seed, answers.clone());
        let mut registry = MissionRegistry::new();
        let mut standby = Standby::new(Limits { misses: 3 }).unwrap();
        // the robots that were running something confirm a stop in about half the cases
        let mut snapshot: Option<Vec<ReplicaRecord>> = None;
        let mut robots = SimRobots { confirm: vec![] };
        for _ in 0..answers.len() {
            let step = standby.tick(&mut primary, &mut robots, &mut registry);
            if let Step::PrimaryAlive = step {
                if let Some(read) = &primary.last_read {
                    snapshot = Some(read.clone());
                    robots.confirm = primary
                        .missions
                        .iter()
                        .filter(|m| in_flight(m) && rng.chance(0.5))
                        .map(|m| (holder(m), m.id.clone(), m.attempt))
                        .collect();
                }
            }
            if let Step::TookOver(report) = step {
                took_over += 1;
                let snapshot = snapshot.clone().unwrap_or_default();
                assert_eq!(
                    report.adopted.len() + report.existing.len() + report.rejected.len(),
                    snapshot.len(),
                    "seed {seed}: every record is accounted for"
                );
                for mission in registry.all() {
                    assert!(
                        !matches!(
                            mission.state,
                            MissionState::Dispatched { .. } | MissionState::InProgress { .. }
                        ),
                        "seed {seed}: nothing taken over is trusted to be in flight: {mission:?}"
                    );
                }
                for id in &report.requeued {
                    let mission = registry.get(id).unwrap();
                    assert_eq!(mission.state, MissionState::Pending, "seed {seed}");
                    // the attempt is higher than the one the robot worked on, so what it still reports is refused
                    let was = snapshot
                        .iter()
                        .find(|r| r.key == format!("mission/{id}"))
                        .unwrap();
                    assert_eq!(u64::from(mission.attempt), was.generation, "seed {seed}");
                    assert!(
                        robots.confirm.iter().any(|(_, m, _)| m == id),
                        "seed {seed}: {id} was requeued without proof"
                    );
                }
                for id in &report.unconfirmed {
                    let mission = registry.get(id).unwrap();
                    assert!(
                        matches!(mission.state, MissionState::Unknown { .. }),
                        "seed {seed}: {id} must stay unknown, got {:?}",
                        mission.state
                    );
                }
            }
        }
    }
    assert!(
        took_over >= 20,
        "enough runs reached the takeover: {took_over}"
    );
}

fn holder(mission: &Mission) -> String {
    match &mission.state {
        MissionState::Dispatched { node } | MissionState::InProgress { node } => node.clone(),
        _ => String::new(),
    }
}

#[test]
fn a_mission_the_standby_already_holds_is_never_touched_by_a_takeover() {
    for seed in 1..=20u64 {
        let mut rng = Lcg(seed);
        let missions = fleet(&mut rng);
        let records = records_of(&missions);
        let mut registry = MissionRegistry::new();
        // the standby already holds m-3, cancelled
        let mut held = Mission::new("m-3");
        let _ = held.cancel();
        let before = held.state.clone();
        assert!(registry.insert_adopted(held));
        let report = crate::replica::takeover(&mut registry, &records, &[]);
        assert_eq!(registry.get("m-3").unwrap().state, before, "seed {seed}");
        assert!(report.existing.contains(&"m-3".to_string()), "seed {seed}");
        assert_eq!(report.adopted.len(), MISSIONS - 1, "seed {seed}");
        // taking over twice changes nothing more
        let again = crate::replica::takeover(&mut registry, &records, &[]);
        assert!(again.adopted.is_empty(), "seed {seed}");
        assert_eq!(again.existing.len(), MISSIONS, "seed {seed}");
    }
}

#[test]
fn a_forged_record_is_rejected_and_the_other_records_are_still_taken_over() {
    for seed in 1..=20u64 {
        let mut rng = Lcg(seed + 500);
        let missions = fleet(&mut rng);
        let mut records = records_of(&missions);
        let good = records.len();
        records.push(ReplicaRecord {
            key: "mission/forged-1".into(),
            value: "not json".into(),
            generation: 1,
        });
        records.push(ReplicaRecord {
            key: "mission/forged-2".into(),
            value: r#"{"mission_id":"forged-2","status":"queued","owner":"x","attempt":3}"#.into(),
            generation: 99,
        });
        records.push(ReplicaRecord {
            key: "mission/forged-3".into(),
            value: r#"{"mission_id":"other","status":"queued","owner":"x","attempt":0}"#.into(),
            generation: 1,
        });
        records.push(ReplicaRecord {
            key: "mission/forged-4".into(),
            value: r#"{"mission_id":"forged-4","status":"teleporting","owner":"x","attempt":0}"#
                .into(),
            generation: 1,
        });
        let mut registry = MissionRegistry::new();
        let report = crate::replica::takeover(&mut registry, &records, &[]);
        assert_eq!(
            report.rejected.len(),
            4,
            "seed {seed}: {:?}",
            report.rejected
        );
        assert_eq!(report.adopted.len(), good, "seed {seed}");
        assert!(registry.get("forged-1").is_none());
    }
}

#[test]
fn a_record_that_cannot_be_read_from_the_primary_leaves_the_last_good_copy_in_use() {
    // the primary answers its health check but the records are cut every time; the standby keeps what it read before
    let mut rng = Lcg(77);
    let missions = fleet(&mut rng);
    struct Cut {
        answers: Vec<bool>,
        position: usize,
        good: Vec<ReplicaRecord>,
    }
    impl Peer for Cut {
        fn alive(&mut self) -> bool {
            self.position += 1;
            self.answers[self.position - 1]
        }
        fn records(&mut self) -> Result<Vec<ReplicaRecord>, String> {
            if self.position == 1 {
                Ok(self.good.clone())
            } else {
                Err("cut".into())
            }
        }
    }
    let good = records_of(&missions);
    let mut primary = Cut {
        answers: vec![true, true, true, false, false, false],
        position: 0,
        good: good.clone(),
    };
    let mut robots = SimRobots { confirm: vec![] };
    let mut registry = MissionRegistry::new();
    let mut standby = Standby::new(Limits { misses: 3 }).unwrap();
    let mut report = None;
    for _ in 0..6 {
        if let Step::TookOver(r) = standby.tick(&mut primary, &mut robots, &mut registry) {
            report = Some(r);
        }
    }
    let report = report.expect("the standby took over");
    assert_eq!(report.adopted.len(), good.len());
    assert_eq!(standby.missed(), 3);
}

#[test]
fn a_standby_with_no_limit_cannot_exist() {
    assert!(Standby::new(Limits { misses: 0 }).is_none());
}
