// =============================================================================
// HYDRA-UMC-ORCHESTRATOR - src/replica.rs
// Copyright (C) 2026 JuanenRac (Electro Hobby 3D) <electrohobby3d@gmail.com>
// GPL-3.0-or-later - see LICENSE
// =============================================================================
//! The mission registry as records a second orchestrator can hold: the shape HYDRA-UMC-SWARM-SYNC replicates under `mission/<id>`.
//!
//! A record is `{"mission_id", "status", "owner", "attempt"}` and its generation is `attempt + 1`. The generation is compared before
//! the clock in the swarm-sync map, so the record of a newer attempt wins over any record of an older one on every node, whatever their
//! clocks say: a node that was cut off and keeps writing with its old attempt loses once the nodes meet again.
//!
//! A record holds no routing hints (priority, tool, location, `dependsOn`): the job-dispatcher owns the jobs, and a node that takes the
//! registry over finds them there. What a record carries is exactly what is needed to decide who owns the mission now.
//!
//! Taking over (`adopt`) never trusts a mission that was in flight: the new node asks the robot that held it to confirm that it stopped.
//! If it confirms, the mission goes back to `Pending` with its attempt raised, so anything the old holder still reports carries a number
//! that is no longer current. If nobody confirms, the mission stays `Unknown`: it is not dispatched a second time while it may still be
//! running somewhere.

use serde::Serialize;

use crate::mission::{Mission, MissionRegistry, MissionState};

pub const MISSION_PREFIX: &str = "mission/";

/// The value of a record, in the field order of the swarm contract.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct RecordValue<'a> {
    mission_id: &'a str,
    status: &'static str,
    owner: &'a str,
    attempt: u32,
}

/// One mission as the swarm-sync map holds it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ReplicaRecord {
    pub key: String,
    /// The JSON text of the record (the map holds strings).
    pub value: String,
    /// `attempt + 1`: what the swarm-sync map needs to rank the record.
    pub generation: u64,
}

fn status_of(state: &MissionState) -> &'static str {
    match state {
        MissionState::Pending => "queued",
        MissionState::Dispatched { .. } => "assigned",
        // an unknown mission was in flight and nobody has confirmed what became of it: it is replicated as still running
        MissionState::InProgress { .. } | MissionState::Unknown { .. } => "running",
        MissionState::Completed { .. } => "completed",
        MissionState::Failed { .. } => "failed",
        MissionState::Cancelled => "cancelled",
    }
}

fn holder_of<'a>(state: &'a MissionState, default_owner: &'a str) -> &'a str {
    match state {
        MissionState::Dispatched { node }
        | MissionState::InProgress { node }
        | MissionState::Completed { node } => node,
        MissionState::Unknown { last_node } => last_node,
        _ => default_owner,
    }
}

/// The record of `mission`. `default_owner` (the name of this orchestrator) is the owner of a mission no robot holds.
pub fn record_of(mission: &Mission, default_owner: &str) -> ReplicaRecord {
    let value = RecordValue {
        mission_id: &mission.id,
        status: status_of(&mission.state),
        owner: holder_of(&mission.state, default_owner),
        attempt: mission.attempt,
    };
    ReplicaRecord {
        key: format!("{MISSION_PREFIX}{}", mission.id),
        value: serde_json::to_string(&value).expect("a record always serializes"),
        generation: u64::from(mission.attempt) + 1,
    }
}

pub fn records(registry: &MissionRegistry, default_owner: &str) -> Vec<ReplicaRecord> {
    registry
        .all()
        .map(|m| record_of(m, default_owner))
        .collect()
}

// No route takes a registry over yet: that needs the robot to confirm that it stopped, which has no contract. The function is complete and
// tested; it is what the takeover will call.
#[allow(dead_code)]
#[derive(Debug, Default)]
pub struct Adoption {
    pub missions: Vec<Mission>,
    /// Missions that were in flight and whose holder confirmed it stopped: back to `Pending` with a higher attempt.
    pub requeued: Vec<String>,
    /// Missions that were in flight and nobody confirmed: `Unknown`, not to be dispatched again yet.
    pub unconfirmed: Vec<String>,
    /// Records that could not be read (the text of the problem).
    pub rejected: Vec<String>,
}

#[allow(dead_code)]
#[derive(serde::Deserialize)]
struct ParsedValue {
    mission_id: String,
    status: String,
    owner: String,
    attempt: u32,
}

/// Rebuilds missions from replicated records. `confirm_stopped(owner, mission_id, attempt)` asks the robot that held an in-flight mission
/// whether it stopped working on it.
#[allow(dead_code)]
pub fn adopt(
    records: &[ReplicaRecord],
    confirm_stopped: impl Fn(&str, &str, u32) -> bool,
) -> Adoption {
    let mut out = Adoption::default();
    for record in records {
        let parsed: ParsedValue = match serde_json::from_str(&record.value) {
            Ok(p) => p,
            Err(e) => {
                out.rejected.push(format!("{}: {e}", record.key));
                continue;
            }
        };
        if record.key != format!("{MISSION_PREFIX}{}", parsed.mission_id)
            || record.generation != u64::from(parsed.attempt) + 1
            || !crate::mission::is_valid_mission_id(&parsed.mission_id)
        {
            out.rejected.push(format!(
                "{}: the key, the id and the generation do not agree",
                record.key
            ));
            continue;
        }
        let mut mission = Mission::new(parsed.mission_id.clone());
        mission.attempt = parsed.attempt;
        match parsed.status.as_str() {
            "queued" => {}
            "assigned" | "running" => {
                if confirm_stopped(&parsed.owner, &parsed.mission_id, parsed.attempt) {
                    // whoever held it no longer does: a report with the old attempt is refused from now on
                    mission.attempt += 1;
                    out.requeued.push(parsed.mission_id.clone());
                } else {
                    mission.state = MissionState::Unknown {
                        last_node: parsed.owner.clone(),
                    };
                    out.unconfirmed.push(parsed.mission_id.clone());
                }
            }
            "completed" => {
                mission.state = MissionState::Completed {
                    node: parsed.owner.clone(),
                }
            }
            "failed" => {
                mission.state = MissionState::Failed {
                    reason: "replicated as failed".to_string(),
                }
            }
            "cancelled" => mission.state = MissionState::Cancelled,
            other => {
                out.rejected
                    .push(format!("{}: unknown status {other:?}", record.key));
                continue;
            }
        }
        out.missions.push(mission);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dispatched(id: &str, node: &str) -> Mission {
        let mut m = Mission::new(id);
        m.dispatch(node).unwrap();
        m
    }

    #[test]
    fn the_record_has_the_shape_and_the_generation_of_the_swarm_contract() {
        // The same text is checked in HYDRA-UMC-SWARM-SYNC: the two repositories agree on it.
        let mut m = dispatched("m-1", "node-a");
        m.start().unwrap();
        let r = record_of(&m, "orch-a");
        assert_eq!(r.key, "mission/m-1");
        assert_eq!(
            r.value,
            r#"{"mission_id":"m-1","status":"running","owner":"node-a","attempt":1}"#
        );
        assert_eq!(r.generation, 2);
        // a mission nobody holds belongs to this orchestrator
        let queued = record_of(&Mission::new("m-2"), "orch-a");
        assert_eq!(
            queued.value,
            r#"{"mission_id":"m-2","status":"queued","owner":"orch-a","attempt":0}"#
        );
        assert_eq!(queued.generation, 1);
    }

    #[test]
    fn every_state_has_a_status_and_an_owner() {
        let mut done = dispatched("c", "node-a");
        done.start().unwrap();
        done.complete().unwrap();
        let mut failed = dispatched("f", "node-a");
        failed.fail("jam").unwrap();
        let mut cancelled = Mission::new("x");
        cancelled.cancel().unwrap();
        for (mission, status, owner) in [
            (&done, "completed", "node-a"),
            (&failed, "failed", "orch"),
            (&cancelled, "cancelled", "orch"),
            (&dispatched("d", "node-b"), "assigned", "node-b"),
        ] {
            let r = record_of(mission, "orch");
            assert!(
                r.value.contains(&format!(r#""status":"{status}""#)),
                "{}",
                r.value
            );
            assert!(
                r.value.contains(&format!(r#""owner":"{owner}""#)),
                "{}",
                r.value
            );
        }
        let mut unknown = dispatched("u", "node-c");
        unknown.state = MissionState::Unknown {
            last_node: "node-c".into(),
        };
        assert!(record_of(&unknown, "orch")
            .value
            .contains(r#""status":"running""#));
    }

    #[test]
    fn a_registry_is_exported_as_one_record_per_mission() {
        let mut reg = MissionRegistry::new();
        reg.add("m1");
        reg.add("m2").dispatch("node-a").unwrap();
        let rs = records(&reg, "orch");
        assert_eq!(rs.len(), 2);
        assert_eq!(rs[0].key, "mission/m1");
        assert_eq!(rs[1].generation, 2);
    }

    #[test]
    fn taking_over_requeues_what_the_old_holder_confirmed_stopped_and_keeps_the_rest_unknown() {
        let mut reg = MissionRegistry::new();
        reg.add("queued");
        reg.add("stopped").dispatch("node-a").unwrap();
        reg.add("silent").dispatch("node-b").unwrap();
        let mut done = Mission::new("done");
        done.dispatch("node-a").unwrap();
        done.start().unwrap();
        done.complete().unwrap();
        let mut rs = records(&reg, "orch");
        rs.push(record_of(&done, "orch"));
        let adoption = adopt(&rs, |owner, _, _| owner == "node-a");
        assert_eq!(adoption.requeued, vec!["stopped"]);
        assert_eq!(adoption.unconfirmed, vec!["silent"]);
        assert!(adoption.rejected.is_empty());
        let get = |id: &str| adoption.missions.iter().find(|m| m.id == id).unwrap();
        assert_eq!(get("queued").state, MissionState::Pending);
        assert_eq!(get("stopped").state, MissionState::Pending);
        // the attempt went up: a late report of the old holder (attempt 1) is stale
        assert_eq!(get("stopped").attempt, 2);
        assert_eq!(
            get("silent").state,
            MissionState::Unknown {
                last_node: "node-b".into()
            }
        );
        assert_eq!(get("silent").attempt, 1);
        assert!(matches!(get("done").state, MissionState::Completed { .. }));
    }

    #[test]
    fn a_requeued_mission_refuses_the_late_report_of_its_old_holder() {
        let m = dispatched("m", "node-a");
        let adoption = adopt(&[record_of(&m, "orch")], |_, _, _| true);
        let adopted = &adoption.missions[0];
        assert_eq!(
            adopted.check_fence(Some(1), None, false),
            Err(crate::mission::FenceError::StaleAttempt {
                reported: 1,
                current: 2
            })
        );
    }

    #[test]
    fn records_that_disagree_with_themselves_or_cannot_be_read_are_rejected() {
        let good = record_of(&Mission::new("m"), "orch");
        let mut wrong_key = good.clone();
        wrong_key.key = "mission/other".into();
        let mut wrong_generation = good.clone();
        wrong_generation.generation = 5;
        let mut bad_status = good.clone();
        bad_status.value = good.value.replace("queued", "sleeping");
        let mut not_json = good.clone();
        not_json.value = "queued".into();
        let adoption = adopt(
            &[good, wrong_key, wrong_generation, bad_status, not_json],
            |_, _, _| true,
        );
        assert_eq!(adoption.missions.len(), 1);
        assert_eq!(adoption.rejected.len(), 4);
    }
}
