// HYDRA-UMC-ORCHESTRATOR - src/mission.rs
// Copyright (C) 2026 JuanenRac (Electro Hobby 3D) <electrohobby3d@gmail.com>
// GPL-3.0 - see LICENSE
//
// The real mission state machine: what "arbitrating which robot gets
// which mission" (main.rs's own description of this process's role)
// actually means as code. Pure in-memory logic, no gRPC/network I/O -
// the same "real logic before real transport" pattern used across this
// ecosystem's other v0 passes (see e.g. HYDRA-UMC-VISUAL-SERVOING-API's
// authorization.py or HYDRA-UMC-SAFETY-ZONES's safety_state.py). A real
// dispatcher wiring this to JOB-DISPATCHER/NODE-HEALING over gRPC lands
// once those services have something real to call.

use std::collections::BTreeMap;

/// The lifecycle a single mission moves through. `Dispatched` and
/// `InProgress` carry the node currently responsible for the mission -
/// that is exactly the information `recover_from_unavailable_node` needs
/// to decide whether a mission is affected by a given node going down.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MissionState {
    Pending,
    Dispatched { node: String },
    InProgress { node: String },
    Completed { node: String },
    Cancelled,
    Failed { reason: String },
}

impl MissionState {
    fn label(&self) -> &'static str {
        match self {
            MissionState::Pending => "Pending",
            MissionState::Dispatched { .. } => "Dispatched",
            MissionState::InProgress { .. } => "InProgress",
            MissionState::Completed { .. } => "Completed",
            MissionState::Cancelled => "Cancelled",
            MissionState::Failed { .. } => "Failed",
        }
    }

    fn is_terminal(&self) -> bool {
        matches!(
            self,
            MissionState::Completed { .. } | MissionState::Cancelled | MissionState::Failed { .. }
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransitionError {
    pub from: &'static str,
    pub attempted: &'static str,
}

impl std::fmt::Display for TransitionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "cannot {} a mission in state {}",
            self.attempted, self.from
        )
    }
}

/// What actually happened when `cancel()` was called - distinct from a
/// `TransitionError` because "already cancelled" is a successful,
/// idempotent outcome, not a failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CancelOutcome {
    Cancelled,
    AlreadyCancelled,
}

/// What happened when a mission was checked against a node that just
/// became unavailable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecoveryOutcome {
    /// The mission was on the unavailable node and has been requeued to
    /// `Pending` so a dispatcher can redispatch it to a healthy node.
    Requeued,
    /// The mission was not affected - either it was never on that node,
    /// or it was already in a terminal state (a completed/cancelled/
    /// failed mission has nothing left to recover).
    NotAffected,
}

#[derive(Debug, Clone)]
pub struct Mission {
    pub id: String,
    pub state: MissionState,
}

impl Mission {
    pub fn new(id: impl Into<String>) -> Self {
        Mission {
            id: id.into(),
            state: MissionState::Pending,
        }
    }

    /// Pending -> Dispatched. Assigns the mission to `node`, the only
    /// transition allowed from Pending.
    pub fn dispatch(&mut self, node: impl Into<String>) -> Result<(), TransitionError> {
        match &self.state {
            MissionState::Pending => {
                self.state = MissionState::Dispatched { node: node.into() };
                Ok(())
            }
            other => Err(TransitionError {
                from: other.label(),
                attempted: "dispatch",
            }),
        }
    }

    /// Dispatched -> InProgress. The node has confirmed it started
    /// executing the mission.
    pub fn start(&mut self) -> Result<(), TransitionError> {
        match &self.state {
            MissionState::Dispatched { node } => {
                self.state = MissionState::InProgress { node: node.clone() };
                Ok(())
            }
            other => Err(TransitionError {
                from: other.label(),
                attempted: "start",
            }),
        }
    }

    /// InProgress -> Completed.
    pub fn complete(&mut self) -> Result<(), TransitionError> {
        match &self.state {
            MissionState::InProgress { node } => {
                self.state = MissionState::Completed { node: node.clone() };
                Ok(())
            }
            other => Err(TransitionError {
                from: other.label(),
                attempted: "complete",
            }),
        }
    }

    /// Cancels the mission. Idempotent by design: calling cancel() on an
    /// already-`Cancelled` mission is a successful no-op
    /// (`AlreadyCancelled`), not an error - a caller retrying a cancel
    /// request (e.g. after a timeout on the first response) must never
    /// get a different answer the second time. Cancelling out of any
    /// OTHER terminal state (`Completed`/`Failed`) is refused: finished
    /// or already-failed work cannot be retroactively cancelled.
    pub fn cancel(&mut self) -> Result<CancelOutcome, TransitionError> {
        match &self.state {
            MissionState::Cancelled => Ok(CancelOutcome::AlreadyCancelled),
            MissionState::Completed { .. } | MissionState::Failed { .. } => Err(TransitionError {
                from: self.state.label(),
                attempted: "cancel",
            }),
            _ => {
                self.state = MissionState::Cancelled;
                Ok(CancelOutcome::Cancelled)
            }
        }
    }

    /// Recovery for a node that just became unavailable (unreachable, or
    /// reporting an invalid identity - see HYDRA-UMC-NODE-HEALING's
    /// watchdog::Status). If this mission is currently assigned to
    /// `unavailable_node` and not yet in a terminal state, it is requeued
    /// to `Pending` rather than left stuck on a node that will never
    /// report progress again. A mission already `Completed`/`Cancelled`/
    /// `Failed` is left untouched - there is nothing to recover.
    pub fn recover_from_unavailable_node(&mut self, unavailable_node: &str) -> RecoveryOutcome {
        let assigned_node = match &self.state {
            MissionState::Dispatched { node } | MissionState::InProgress { node } => Some(node),
            _ => None,
        };
        match assigned_node {
            Some(node) if node == unavailable_node => {
                self.state = MissionState::Pending;
                RecoveryOutcome::Requeued
            }
            _ => RecoveryOutcome::NotAffected,
        }
    }

    pub fn is_terminal(&self) -> bool {
        self.state.is_terminal()
    }

    /// Marks the mission permanently `Failed`, valid from any
    /// non-terminal state. Distinct from recovery
    /// (`recover_from_unavailable_node`, which requeues to `Pending` for
    /// a fresh dispatch attempt elsewhere): `fail()` is for a mission the
    /// orchestrator has given up on for good - e.g. it could not be
    /// dispatched to any healthy node after repeated attempts. Like
    /// `complete()`, this cannot be called on an already-terminal
    /// mission - a finished or already-failed mission cannot fail again.
    pub fn fail(&mut self, reason: impl Into<String>) -> Result<(), TransitionError> {
        if self.state.is_terminal() {
            return Err(TransitionError {
                from: self.state.label(),
                attempted: "fail",
            });
        }
        self.state = MissionState::Failed {
            reason: reason.into(),
        };
        Ok(())
    }
}

/// Tracks every mission the orchestrator currently knows about, keyed by
/// id. `BTreeMap` (not `HashMap`) so `all()`/iteration order is
/// deterministic - useful for both the demo CLI output and tests.
#[derive(Debug, Default)]
pub struct MissionRegistry {
    missions: BTreeMap<String, Mission>,
}

impl MissionRegistry {
    pub fn new() -> Self {
        MissionRegistry {
            missions: BTreeMap::new(),
        }
    }

    pub fn add(&mut self, id: impl Into<String>) -> &mut Mission {
        let id = id.into();
        self.missions
            .entry(id.clone())
            .or_insert_with(|| Mission::new(id))
    }

    pub fn get(&self, id: &str) -> Option<&Mission> {
        self.missions.get(id)
    }

    pub fn get_mut(&mut self, id: &str) -> Option<&mut Mission> {
        self.missions.get_mut(id)
    }

    pub fn all(&self) -> impl Iterator<Item = &Mission> {
        self.missions.values()
    }

    /// Applies `recover_from_unavailable_node` across every mission this
    /// registry tracks - the real fleet-wide reaction to a node health
    /// report going bad, returning exactly which missions were requeued
    /// so a caller (or a test) can assert on it precisely.
    pub fn recover_node_unavailable(&mut self, unavailable_node: &str) -> Vec<String> {
        let mut requeued = Vec::new();
        for mission in self.missions.values_mut() {
            if mission.recover_from_unavailable_node(unavailable_node) == RecoveryOutcome::Requeued
            {
                requeued.push(mission.id.clone());
            }
        }
        requeued
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_mission_starts_pending() {
        let m = Mission::new("m1");
        assert_eq!(m.state, MissionState::Pending);
    }

    #[test]
    fn full_happy_path_reaches_completed() {
        let mut m = Mission::new("m1");
        m.dispatch("node-a").unwrap();
        assert_eq!(
            m.state,
            MissionState::Dispatched {
                node: "node-a".into()
            }
        );
        m.start().unwrap();
        assert_eq!(
            m.state,
            MissionState::InProgress {
                node: "node-a".into()
            }
        );
        m.complete().unwrap();
        assert_eq!(
            m.state,
            MissionState::Completed {
                node: "node-a".into()
            }
        );
        assert!(m.is_terminal());
    }

    #[test]
    fn dispatch_from_non_pending_is_rejected() {
        let mut m = Mission::new("m1");
        m.dispatch("node-a").unwrap();
        let err = m.dispatch("node-b").unwrap_err();
        assert_eq!(err.from, "Dispatched");
        assert_eq!(err.attempted, "dispatch");
        // Must not have silently reassigned the node.
        assert_eq!(
            m.state,
            MissionState::Dispatched {
                node: "node-a".into()
            }
        );
    }

    #[test]
    fn start_from_pending_is_rejected() {
        let mut m = Mission::new("m1");
        let err = m.start().unwrap_err();
        assert_eq!(err.from, "Pending");
    }

    #[test]
    fn complete_from_dispatched_is_rejected() {
        let mut m = Mission::new("m1");
        m.dispatch("node-a").unwrap();
        let err = m.complete().unwrap_err();
        assert_eq!(err.from, "Dispatched");
    }

    #[test]
    fn cancel_from_pending_succeeds() {
        let mut m = Mission::new("m1");
        assert_eq!(m.cancel(), Ok(CancelOutcome::Cancelled));
        assert_eq!(m.state, MissionState::Cancelled);
    }

    #[test]
    fn cancel_from_in_progress_succeeds() {
        let mut m = Mission::new("m1");
        m.dispatch("node-a").unwrap();
        m.start().unwrap();
        assert_eq!(m.cancel(), Ok(CancelOutcome::Cancelled));
        assert_eq!(m.state, MissionState::Cancelled);
    }

    #[test]
    fn cancel_is_idempotent() {
        let mut m = Mission::new("m1");
        assert_eq!(m.cancel(), Ok(CancelOutcome::Cancelled));
        // Calling it again (e.g. a retried request) must succeed the
        // same way, not error - and must not change the state further.
        assert_eq!(m.cancel(), Ok(CancelOutcome::AlreadyCancelled));
        assert_eq!(m.cancel(), Ok(CancelOutcome::AlreadyCancelled));
        assert_eq!(m.state, MissionState::Cancelled);
    }

    #[test]
    fn cancel_from_completed_is_rejected() {
        let mut m = Mission::new("m1");
        m.dispatch("node-a").unwrap();
        m.start().unwrap();
        m.complete().unwrap();
        let err = m.cancel().unwrap_err();
        assert_eq!(err.from, "Completed");
        assert_eq!(err.attempted, "cancel");
        // Completion must not be retroactively undone.
        assert_eq!(
            m.state,
            MissionState::Completed {
                node: "node-a".into()
            }
        );
    }

    #[test]
    fn cancel_from_failed_is_rejected() {
        let mut m = Mission::new("m1");
        m.fail("no healthy node available").unwrap();
        let err = m.cancel().unwrap_err();
        assert_eq!(err.from, "Failed");
    }

    #[test]
    fn fail_from_pending_succeeds() {
        let mut m = Mission::new("m1");
        m.fail("no healthy node available").unwrap();
        assert_eq!(
            m.state,
            MissionState::Failed {
                reason: "no healthy node available".into()
            }
        );
        assert!(m.is_terminal());
    }

    #[test]
    fn fail_from_in_progress_succeeds() {
        let mut m = Mission::new("m1");
        m.dispatch("node-a").unwrap();
        m.start().unwrap();
        m.fail("actuator fault reported by node").unwrap();
        assert_eq!(
            m.state,
            MissionState::Failed {
                reason: "actuator fault reported by node".into()
            }
        );
    }

    #[test]
    fn fail_from_terminal_state_is_rejected() {
        let mut m = Mission::new("m1");
        m.dispatch("node-a").unwrap();
        m.start().unwrap();
        m.complete().unwrap();
        let err = m.fail("too late").unwrap_err();
        assert_eq!(err.from, "Completed");
        assert_eq!(err.attempted, "fail");
    }

    #[test]
    fn recovery_never_reopens_a_failed_mission() {
        let mut m = Mission::new("m1");
        m.dispatch("node-a").unwrap();
        m.fail("gave up".to_string()).unwrap();
        let outcome = m.recover_from_unavailable_node("node-a");
        assert_eq!(outcome, RecoveryOutcome::NotAffected);
        assert!(matches!(m.state, MissionState::Failed { .. }));
    }

    #[test]
    fn recovery_requeues_dispatched_mission_on_unavailable_node() {
        let mut m = Mission::new("m1");
        m.dispatch("node-a").unwrap();
        let outcome = m.recover_from_unavailable_node("node-a");
        assert_eq!(outcome, RecoveryOutcome::Requeued);
        assert_eq!(m.state, MissionState::Pending);
    }

    #[test]
    fn recovery_requeues_in_progress_mission_on_unavailable_node() {
        let mut m = Mission::new("m1");
        m.dispatch("node-a").unwrap();
        m.start().unwrap();
        let outcome = m.recover_from_unavailable_node("node-a");
        assert_eq!(outcome, RecoveryOutcome::Requeued);
        assert_eq!(m.state, MissionState::Pending);
    }

    #[test]
    fn recovery_ignores_missions_on_other_nodes() {
        let mut m = Mission::new("m1");
        m.dispatch("node-b").unwrap();
        let outcome = m.recover_from_unavailable_node("node-a");
        assert_eq!(outcome, RecoveryOutcome::NotAffected);
        assert_eq!(
            m.state,
            MissionState::Dispatched {
                node: "node-b".into()
            }
        );
    }

    #[test]
    fn recovery_ignores_pending_missions() {
        let mut m = Mission::new("m1");
        let outcome = m.recover_from_unavailable_node("node-a");
        assert_eq!(outcome, RecoveryOutcome::NotAffected);
        assert_eq!(m.state, MissionState::Pending);
    }

    #[test]
    fn recovery_never_reopens_a_completed_mission() {
        let mut m = Mission::new("m1");
        m.dispatch("node-a").unwrap();
        m.start().unwrap();
        m.complete().unwrap();
        let outcome = m.recover_from_unavailable_node("node-a");
        assert_eq!(outcome, RecoveryOutcome::NotAffected);
        assert_eq!(
            m.state,
            MissionState::Completed {
                node: "node-a".into()
            }
        );
    }

    #[test]
    fn recovery_never_reopens_a_cancelled_mission() {
        let mut m = Mission::new("m1");
        m.dispatch("node-a").unwrap();
        m.cancel().unwrap();
        let outcome = m.recover_from_unavailable_node("node-a");
        assert_eq!(outcome, RecoveryOutcome::NotAffected);
        assert_eq!(m.state, MissionState::Cancelled);
    }

    #[test]
    fn registry_recovers_only_missions_on_the_affected_node() {
        let mut reg = MissionRegistry::new();
        reg.add("m1").dispatch("node-a").unwrap();
        reg.add("m2").dispatch("node-a").unwrap();
        reg.add("m3").dispatch("node-b").unwrap();

        let requeued = reg.recover_node_unavailable("node-a");

        assert_eq!(requeued, vec!["m1".to_string(), "m2".to_string()]);
        assert_eq!(reg.get("m1").unwrap().state, MissionState::Pending);
        assert_eq!(reg.get("m2").unwrap().state, MissionState::Pending);
        assert_eq!(
            reg.get("m3").unwrap().state,
            MissionState::Dispatched {
                node: "node-b".into()
            }
        );
    }

    #[test]
    fn registry_recovery_is_a_safe_no_op_when_node_has_no_missions() {
        let mut reg = MissionRegistry::new();
        reg.add("m1").dispatch("node-a").unwrap();
        let requeued = reg.recover_node_unavailable("node-does-not-exist");
        assert!(requeued.is_empty());
        assert_eq!(
            reg.get("m1").unwrap().state,
            MissionState::Dispatched {
                node: "node-a".into()
            }
        );
    }
}
