// =============================================================================
// HYDRA-UMC-ORCHESTRATOR - src/plan.rs
// Copyright (C) 2026 JuanenRac (Electro Hobby 3D) <electrohobby3d@gmail.com>
// GPL-3.0-or-later - see LICENSE
// =============================================================================
//! Multi-stage missions: a plan is a graph of stages, each stage is a mission, and an edge is a `dependsOn` of the job.
//!
//! The orchestrator does not schedule the stages: it turns each one into an ordinary mission whose id is `<plan>.<stage>` and whose
//! `dependsOn` lists the full ids of the stages it waits for, so the job-dispatcher (which already understands `dependsOn`) releases a
//! stage when its predecessors are done. Without a job-dispatcher the manual dispatch of a stage is refused until its predecessors
//! completed. What a plan adds is the check done once, when it is accepted (ids, no stage twice, every predecessor exists, no cycle, at
//! most `MAX_STAGES` stages) and a view of the whole: the state of every stage, which ones are held back by a stage that failed or was
//! cancelled, and the state of the plan.
//!
//! A stage that fails does not fail the mission graph: the stages after it stay `blocked` and the plan reports `failed`, so an operator
//! can retry or cancel; nothing after it runs on its own.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::mission::{is_valid_mission_id, Mission, MissionState};

/// A plan may have this many stages: a pick-and-place job is a handful, not hundreds.
pub const MAX_STAGES: usize = 64;

#[derive(Debug, Clone, Deserialize)]
pub struct StageRequest {
    pub id: String,
    #[serde(default)]
    pub priority: i32,
    #[serde(default, rename = "requiredTool")]
    pub required_tool: String,
    #[serde(default)]
    pub location: String,
    /// The ids of the stages (of this plan) that must be completed first.
    #[serde(default, rename = "dependsOn")]
    pub depends_on: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PlanRequest {
    pub id: String,
    pub stages: Vec<StageRequest>,
}

/// The id of the mission that is the stage `stage` of plan `plan`.
pub fn stage_mission_id(plan: &str, stage: &str) -> String {
    format!("{plan}.{stage}")
}

/// Checks a plan and returns its stages in an order in which every stage comes after the ones it depends on (ties broken by the order of
/// the request, so the result is the same every time).
pub fn validate(plan: &PlanRequest) -> Result<Vec<usize>, String> {
    if !is_valid_mission_id(&plan.id) || plan.id.contains('.') {
        return Err(
            "plan id must be 1 to 128 characters of A-Z a-z 0-9 _ - (no dot: the dot separates the stage)"
                .to_string(),
        );
    }
    if plan.stages.is_empty() {
        return Err("a plan needs at least one stage".to_string());
    }
    if plan.stages.len() > MAX_STAGES {
        return Err(format!("a plan has at most {MAX_STAGES} stages"));
    }
    let mut index: BTreeMap<&str, usize> = BTreeMap::new();
    for (i, stage) in plan.stages.iter().enumerate() {
        if stage.id.contains('.') || !is_valid_mission_id(&stage_mission_id(&plan.id, &stage.id)) {
            return Err(format!(
                "stage id {:?} must be characters of A-Z a-z 0-9 _ - and the mission id <plan>.<stage> at most 128 long",
                stage.id
            ));
        }
        if index.insert(stage.id.as_str(), i).is_some() {
            return Err(format!("stage {:?} appears twice", stage.id));
        }
    }
    let mut waiting_for: Vec<BTreeSet<usize>> = Vec::with_capacity(plan.stages.len());
    for stage in &plan.stages {
        let mut deps = BTreeSet::new();
        for dep in &stage.depends_on {
            if dep == &stage.id {
                return Err(format!("stage {:?} depends on itself", stage.id));
            }
            match index.get(dep.as_str()) {
                Some(&j) => {
                    deps.insert(j);
                }
                None => {
                    return Err(format!(
                        "stage {:?} depends on {dep:?}, which is not a stage of the plan",
                        stage.id
                    ))
                }
            }
        }
        waiting_for.push(deps);
    }
    // Kahn's algorithm
    let mut order = Vec::with_capacity(plan.stages.len());
    let mut done = vec![false; plan.stages.len()];
    while order.len() < plan.stages.len() {
        let next =
            (0..plan.stages.len()).find(|&i| !done[i] && waiting_for[i].iter().all(|&d| done[d]));
        match next {
            Some(i) => {
                done[i] = true;
                order.push(i);
            }
            None => {
                let stuck: Vec<&str> = (0..plan.stages.len())
                    .filter(|&i| !done[i])
                    .map(|i| plan.stages[i].id.as_str())
                    .collect();
                return Err(format!(
                    "the stages {stuck:?} depend on each other in a cycle"
                ));
            }
        }
    }
    Ok(order)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PlanStatus {
    Pending,
    Running,
    Completed,
    Failed,
    Cancelled,
}

#[derive(Debug, Clone, Serialize)]
pub struct StageView {
    pub stage: String,
    pub mission_id: String,
    pub state: &'static str,
    #[serde(rename = "dependsOn")]
    pub depends_on: Vec<String>,
    /// The stages (failed or cancelled) that hold this one back, directly or through other stages; empty when nothing does.
    pub blocked_by: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct PlanView {
    pub plan: String,
    pub status: PlanStatus,
    pub stages: Vec<StageView>,
}

fn stage_name<'a>(plan: &str, mission: &'a Mission) -> &'a str {
    mission
        .id
        .strip_prefix(plan)
        .and_then(|rest| rest.strip_prefix('.'))
        .unwrap_or(&mission.id)
}

/// The view of plan `plan` from its stage missions (the ones with `plan` set to it). None when the plan has none.
pub fn view(plan: &str, missions: &[&Mission]) -> Option<PlanView> {
    if missions.is_empty() {
        return None;
    }
    let names: BTreeMap<&str, &Mission> =
        missions.iter().map(|m| (stage_name(plan, m), *m)).collect();
    let bad = |m: &Mission| {
        matches!(
            m.state,
            MissionState::Failed { .. } | MissionState::Cancelled
        )
    };
    let mut stages = Vec::with_capacity(missions.len());
    for (name, mission) in &names {
        // walk the predecessors: every failed or cancelled stage on the way holds this one back
        let mut blocked_by = BTreeSet::new();
        let mut seen = BTreeSet::new();
        let mut pending: Vec<&str> = mission
            .spec
            .depends_on
            .iter()
            .filter_map(|d| d.strip_prefix(plan).and_then(|r| r.strip_prefix('.')))
            .collect();
        while let Some(dep) = pending.pop() {
            if !seen.insert(dep) {
                continue;
            }
            if let Some(dep_mission) = names.get(dep) {
                if bad(dep_mission) {
                    blocked_by.insert(dep.to_string());
                }
                pending.extend(
                    dep_mission
                        .spec
                        .depends_on
                        .iter()
                        .filter_map(|d| d.strip_prefix(plan).and_then(|r| r.strip_prefix('.'))),
                );
            }
        }
        let held = !blocked_by.is_empty() && matches!(mission.state, MissionState::Pending);
        stages.push(StageView {
            stage: (*name).to_string(),
            mission_id: mission.id.clone(),
            state: if held {
                "Blocked"
            } else {
                mission.state_label()
            },
            depends_on: mission
                .spec
                .depends_on
                .iter()
                .map(|d| {
                    d.strip_prefix(plan)
                        .and_then(|r| r.strip_prefix('.'))
                        .unwrap_or(d)
                        .to_string()
                })
                .collect(),
            blocked_by: blocked_by.into_iter().collect(),
        });
    }
    let all: Vec<&Mission> = names.values().copied().collect();
    let status = if all
        .iter()
        .any(|m| matches!(m.state, MissionState::Failed { .. }))
    {
        PlanStatus::Failed
    } else if all.iter().any(|m| m.state == MissionState::Cancelled) {
        PlanStatus::Cancelled
    } else if all
        .iter()
        .all(|m| matches!(m.state, MissionState::Completed { .. }))
    {
        PlanStatus::Completed
    } else if all.iter().all(|m| m.state == MissionState::Pending) {
        PlanStatus::Pending
    } else {
        PlanStatus::Running
    };
    Some(PlanView {
        plan: plan.to_string(),
        status,
        stages,
    })
}

/// Why the stage `mission` cannot be dispatched by hand yet: the first predecessor (in the registry `lookup`) that is not completed.
pub fn waiting_for(
    mission: &Mission,
    lookup: impl Fn(&str) -> Option<MissionState>,
) -> Option<String> {
    mission.plan.as_ref()?;
    mission
        .spec
        .depends_on
        .iter()
        .find_map(|dep| match lookup(dep) {
            Some(MissionState::Completed { .. }) => None,
            _ => Some(dep.clone()),
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mission::JobSpec;

    fn stage(id: &str, deps: &[&str]) -> StageRequest {
        StageRequest {
            id: id.to_string(),
            priority: 0,
            required_tool: String::new(),
            location: String::new(),
            depends_on: deps.iter().map(|d| d.to_string()).collect(),
        }
    }

    fn plan(stages: Vec<StageRequest>) -> PlanRequest {
        PlanRequest {
            id: "board-7".to_string(),
            stages,
        }
    }

    #[test]
    fn a_valid_plan_is_ordered_so_that_every_stage_follows_its_predecessors() {
        let p = plan(vec![
            stage("inspect", &["mill", "drill"]),
            stage("mill", &[]),
            stage("drill", &["mill"]),
        ]);
        let order = validate(&p).unwrap();
        let names: Vec<&str> = order.iter().map(|&i| p.stages[i].id.as_str()).collect();
        assert_eq!(names, vec!["mill", "drill", "inspect"]);
        // the same input gives the same order
        assert_eq!(validate(&p).unwrap(), order);
    }

    #[test]
    fn a_plan_with_a_cycle_a_missing_predecessor_or_a_repeated_stage_is_refused() {
        let cycle = plan(vec![
            stage("a", &["b"]),
            stage("b", &["c"]),
            stage("c", &["a"]),
        ]);
        assert!(validate(&cycle).unwrap_err().contains("cycle"));
        let itself = plan(vec![stage("a", &["a"])]);
        assert!(validate(&itself).unwrap_err().contains("itself"));
        let missing = plan(vec![stage("a", &["ghost"])]);
        assert!(validate(&missing).unwrap_err().contains("not a stage"));
        let twice = plan(vec![stage("a", &[]), stage("a", &[])]);
        assert!(validate(&twice).unwrap_err().contains("twice"));
        assert!(validate(&plan(vec![])).is_err());
    }

    #[test]
    fn ids_are_checked_and_the_dot_belongs_to_the_mission_id_only() {
        assert!(validate(&plan(vec![stage("a.b", &[])])).is_err());
        assert!(validate(&plan(vec![stage("bad id", &[])])).is_err());
        let mut p = plan(vec![stage("a", &[])]);
        p.id = "has.dot".to_string();
        assert!(validate(&p).is_err());
        p.id = "x".repeat(128);
        assert!(
            validate(&p).is_err(),
            "<plan>.<stage> would be longer than 128"
        );
        let big = plan(
            (0..=MAX_STAGES)
                .map(|i| stage(&format!("s{i}"), &[]))
                .collect(),
        );
        assert!(validate(&big).unwrap_err().contains("at most"));
    }

    fn mission(plan: &str, stage: &str, deps: &[&str], state: MissionState) -> Mission {
        let mut m = Mission::new(stage_mission_id(plan, stage));
        m.plan = Some(plan.to_string());
        m.spec = JobSpec {
            depends_on: deps.iter().map(|d| stage_mission_id(plan, d)).collect(),
            ..JobSpec::default()
        };
        m.state = state;
        m
    }

    fn completed() -> MissionState {
        MissionState::Completed { node: "n".into() }
    }

    #[test]
    fn a_stage_after_a_failed_one_is_blocked_and_the_plan_failed() {
        let ms = [
            mission("p", "a", &[], completed()),
            mission(
                "p",
                "b",
                &["a"],
                MissionState::Failed {
                    reason: "jam".into(),
                },
            ),
            mission("p", "c", &["b"], MissionState::Pending),
            mission("p", "d", &["c"], MissionState::Pending),
            mission("p", "e", &["a"], MissionState::Pending),
        ];
        let refs: Vec<&Mission> = ms.iter().collect();
        let v = view("p", &refs).unwrap();
        assert_eq!(v.status, PlanStatus::Failed);
        let by = |name: &str| v.stages.iter().find(|s| s.stage == name).unwrap();
        assert_eq!(by("c").state, "Blocked");
        assert_eq!(by("c").blocked_by, vec!["b"]);
        // through another stage
        assert_eq!(by("d").state, "Blocked");
        assert_eq!(by("d").blocked_by, vec!["b"]);
        // a stage that does not depend on the failed one is untouched
        assert_eq!(by("e").state, "Pending");
        assert!(by("e").blocked_by.is_empty());
        assert_eq!(by("b").depends_on, vec!["a"]);
    }

    #[test]
    fn the_status_of_a_plan_follows_its_stages() {
        let status = |states: Vec<MissionState>| {
            let ms: Vec<Mission> = states
                .into_iter()
                .enumerate()
                .map(|(i, s)| mission("p", &format!("s{i}"), &[], s))
                .collect();
            let refs: Vec<&Mission> = ms.iter().collect();
            view("p", &refs).unwrap().status
        };
        assert_eq!(
            status(vec![MissionState::Pending, MissionState::Pending]),
            PlanStatus::Pending
        );
        assert_eq!(
            status(vec![completed(), MissionState::Pending]),
            PlanStatus::Running
        );
        assert_eq!(
            status(vec![
                MissionState::InProgress { node: "n".into() },
                MissionState::Pending
            ]),
            PlanStatus::Running
        );
        assert_eq!(
            status(vec![completed(), completed()]),
            PlanStatus::Completed
        );
        assert_eq!(
            status(vec![completed(), MissionState::Cancelled]),
            PlanStatus::Cancelled
        );
        assert!(view("none", &[]).is_none());
    }

    #[test]
    fn a_stage_waits_for_its_predecessors_but_a_plain_mission_does_not() {
        let b = mission("p", "b", &["a"], MissionState::Pending);
        assert_eq!(
            waiting_for(&b, |_| Some(MissionState::Pending)),
            Some("p.a".to_string())
        );
        assert_eq!(waiting_for(&b, |_| None), Some("p.a".to_string()));
        assert_eq!(waiting_for(&b, |_| Some(completed())), None);
        let mut plain = Mission::new("m1");
        plain.spec.depends_on = vec!["other".to_string()];
        assert_eq!(waiting_for(&plain, |_| None), None);
    }
}
