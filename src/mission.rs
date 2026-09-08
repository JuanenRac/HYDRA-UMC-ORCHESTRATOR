// HYDRA-UMC-ORCHESTRATOR - src/mission.rs
// Copyright (C) 2026 JuanenRac (Electro Hobby 3D) <electrohobby3d@gmail.com>
// GPL-3.0 - see LICENSE
//
// The real mission state machine: what "arbitrating which robot gets
// which mission" (main.rs's own description of this process's role)
// actually means as code. No gRPC/network I/O anywhere in this module -
// the same "real logic before real transport" pattern used across this
// ecosystem's other v0 passes (see e.g. HYDRA-UMC-VISUAL-SERVOING-API's
// authorization.py or HYDRA-UMC-SAFETY-ZONES's safety_state.py). A real
// dispatcher wiring this to JOB-DISPATCHER/NODE-HEALING over gRPC lands
// once those services have something real to call.
//
// C07 (this project's own private development plan): `MissionRegistry`
// does own real local file I/O now - `load()`/`persist()`, the same
// "one JSON file, load-or-empty, temp+rename persist" shape `outbox.rs`
// already established in this same crate - so a real mission this
// process knows about survives a real process restart, not just the
// pending remote-close intents outbox.rs already covered.

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// The lifecycle a single mission moves through. `Dispatched` and
/// `InProgress` carry the node currently responsible for the mission -
/// that is exactly the information `recover_from_unavailable_node` needs
/// to decide whether a mission is affected by a given node going down.
///
/// C07 (this project's own private development plan): `Unknown` is a
/// first-class state, not an absence of state. It exists for exactly one
/// real situation - `MissionRegistry::load()` reloading a mission that
/// was `Dispatched`/`InProgress` at the moment this process last
/// persisted its own state, before an unclean shutdown or crash. That
/// on-disk snapshot cannot say whether the assigned node actually
/// finished, failed, or is still working - claiming it survived as
/// `Dispatched`/`InProgress` would be lying about a status this process
/// no longer has any evidence for. `Unknown` names that honestly instead
/// of picking a guess, and only `resolve_unknown()` (a deliberate
/// decision, taken by `MissionRegistry::recover_unknown_missions()` right
/// after a real load) ever leaves it - matching the same "an interruption
/// must never look like a false success" rule this plan's own DS05
/// acceptance criterion states for HYDRA-UMC-DEV-SERVER's task queue.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum MissionState {
    Pending,
    Dispatched { node: String },
    InProgress { node: String },
    Unknown { last_node: String },
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
            MissionState::Unknown { .. } => "Unknown",
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum CancelOutcome {
    Cancelled,
    AlreadyCancelled,
}

/// What happened when a mission was checked against a node that just
/// became unavailable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum RecoveryOutcome {
    /// The mission was on the unavailable node and has been requeued to
    /// `Pending` so a dispatcher can redispatch it to a healthy node.
    Requeued,
    /// The mission was not affected - either it was never on that node,
    /// or it was already in a terminal state (a completed/cancelled/
    /// failed mission has nothing left to recover).
    NotAffected,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Mission {
    pub id: String,
    pub state: MissionState,
    // C07 (this project's own private development plan, I17 "identidad
    // de intentos"): how many times this mission has ever been
    // dispatched, across every attempt - including one that ended in
    // `Unknown` after a restart. Never reset by `recover_from_
    // unavailable_node`/`recover_unknown_missions()` requeuing it back
    // to `Pending`, precisely so a caller can tell "this is a fresh
    // mission" from "this is retry #3 of one that already had trouble" -
    // the real per-attempt generation this plan's own acceptance
    // criterion for a durable queue names, applied here to the one
    // piece of durable state this repository has (MissionRegistry),
    // ahead of a real HYDRA-UMC-DEV-SERVER task queue implementing the
    // rest of that same requirement for its own jobs.
    pub attempt: u32,
    // REV-010 (found in an independent revalidation audit, P1): whether
    // this mission's own terminal outcome has actually been confirmed to
    // Job-Dispatcher yet. Reaching a terminal `state` above is a purely
    // LOCAL fact this struct's own transition methods below already
    // guarantee; a real remote confirmation (server.rs's own
    // job_dispatcher::complete_job() call, made right after a
    // transition succeeds) is a separate network call that can fail on
    // its own - a transient error must never look identical to a real
    // confirmed close. Starts `true` (nothing to confirm yet - either
    // still non-terminal, or terminal via a path that never needed a
    // remote confirmation); only server.rs ever sets it `false` (a real
    // confirmation attempt just failed) or back to `true` (a retry
    // actually succeeded) - this module stays pure, network-free logic,
    // per its own header comment.
    pub remote_close_confirmed: bool,
}

impl Mission {
    pub fn new(id: impl Into<String>) -> Self {
        Mission {
            id: id.into(),
            state: MissionState::Pending,
            attempt: 0,
            remote_close_confirmed: true,
        }
    }

    /// REV-010: called by server.rs right after a terminal transition
    /// (complete()/cancel()) when confirming it to Job-Dispatcher failed.
    pub fn mark_remote_close_pending(&mut self) {
        self.remote_close_confirmed = false;
    }

    /// REV-010: called by server.rs once a retried confirmation to
    /// Job-Dispatcher actually succeeds.
    pub fn mark_remote_close_confirmed(&mut self) {
        self.remote_close_confirmed = true;
    }

    /// Pending -> Dispatched. Assigns the mission to `node`, the only
    /// transition allowed from Pending. Bumps `attempt` - every real
    /// dispatch, successful or not, is one more attempt at this mission,
    /// and that count is never reset by a later requeue.
    pub fn dispatch(&mut self, node: impl Into<String>) -> Result<(), TransitionError> {
        match &self.state {
            MissionState::Pending => {
                self.attempt += 1;
                self.state = MissionState::Dispatched { node: node.into() };
                Ok(())
            }
            other => Err(TransitionError {
                from: other.label(),
                attempted: "dispatch",
            }),
        }
    }

    /// C07: marks this mission `Unknown` - called only by
    /// `MissionRegistry::load()` for a mission that was `Dispatched`/
    /// `InProgress` at the moment this process last persisted its own
    /// state. `last_node` preserves which node it was last assigned to,
    /// purely for a human/log to see - it carries no operational meaning
    /// once `Unknown`, since that node's real status for this mission is
    /// exactly what is no longer known.
    fn mark_unknown(&mut self, last_node: String) {
        self.state = MissionState::Unknown { last_node };
    }

    /// C07: the only way an `Unknown` mission ever leaves that state -
    /// see `MissionRegistry::recover_unknown_missions()`'s own doc
    /// comment for why "requeue to Pending" is this registry's one real
    /// policy today (never "assume it completed", never "assume it
    /// failed"). A no-op, successful `Ok(())` on a mission that was
    /// never `Unknown` in the first place - the caller doesn't need its
    /// own branch for "nothing to resolve here."
    pub fn resolve_unknown_as_pending(&mut self) {
        if matches!(self.state, MissionState::Unknown { .. }) {
            self.state = MissionState::Pending;
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

#[derive(Serialize, Deserialize, Default)]
struct PersistedRegistry {
    missions: Vec<Mission>,
}

/// Tracks every mission the orchestrator currently knows about, keyed by
/// id. `BTreeMap` (not `HashMap`) so `all()`/iteration order is
/// deterministic - useful for both the demo CLI output and tests.
///
/// C07 (this project's own private development plan): `path` is `None`
/// for every existing caller of `new()` (the demo CLI, and every test in
/// this module) - pure in-memory, exactly as before. Only `load()`
/// attaches a real path, following the same "one JSON file, `Mutex`-
/// guarded by the caller, load-or-empty, temp+rename persist" shape
/// `outbox.rs` already established and tests for its own pending-close
/// outbox - reused here rather than a second, different persistence
/// mechanism (or a new dependency like `rusqlite`/`sled`, neither of
/// which this crate needs yet for a single small snapshot file).
#[derive(Debug, Default)]
pub struct MissionRegistry {
    missions: BTreeMap<String, Mission>,
    path: Option<PathBuf>,
}

impl MissionRegistry {
    pub fn new() -> Self {
        MissionRegistry {
            missions: BTreeMap::new(),
            path: None,
        }
    }

    /// Loads whatever this process (or an earlier incarnation of it)
    /// already persisted at `path` - a missing file means no missions
    /// are known yet, not an error (same convention as
    /// `RemoteCloseOutbox::load()`). Every other I/O or parse failure is
    /// real and propagated.
    ///
    /// Any reloaded mission that was `Dispatched`/`InProgress` becomes
    /// `Unknown` immediately (see `Mission::mark_unknown()`'s own doc
    /// comment) - this snapshot can only be as fresh as the last
    /// successful `persist()` before an unclean shutdown, so a mission
    /// that LOOKED still in flight at that moment has no real evidence
    /// behind it any more. Call `recover_unknown_missions()` right after
    /// this, before serving any real request, to decide what happens to
    /// them (today: requeue to `Pending`) - `load()` itself only ever
    /// names the honest uncertainty, it never resolves it.
    pub fn load(path: PathBuf) -> io::Result<Self> {
        let raw = match fs::read_to_string(&path) {
            Ok(raw) => raw,
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                return Ok(MissionRegistry {
                    missions: BTreeMap::new(),
                    path: Some(path),
                });
            }
            Err(e) => return Err(e),
        };
        let state: PersistedRegistry = serde_json::from_str(&raw)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        let mut missions = BTreeMap::new();
        for mut mission in state.missions {
            let stale_node = match &mission.state {
                MissionState::Dispatched { node } | MissionState::InProgress { node } => {
                    Some(node.clone())
                }
                _ => None,
            };
            if let Some(node) = stale_node {
                mission.mark_unknown(node);
            }
            missions.insert(mission.id.clone(), mission);
        }
        Ok(MissionRegistry {
            missions,
            path: Some(path),
        })
    }

    fn persist_or_warn(&self) {
        let Some(path) = &self.path else {
            return; // pure in-memory instance (new()) - nothing to persist
        };
        let state = PersistedRegistry {
            missions: self.missions.values().cloned().collect(),
        };
        let result = (|| -> io::Result<()> {
            let json = serde_json::to_string_pretty(&state).map_err(io::Error::other)?;
            let tmp_path = path.with_extension(format!(
                "{}.{}.tmp",
                path.extension().and_then(|e| e.to_str()).unwrap_or("json"),
                std::process::id()
            ));
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::write(&tmp_path, json)?;
            fs::rename(&tmp_path, path)
        })();
        if let Err(e) = result {
            eprintln!(
                "[orchestrator] could not persist mission registry at {}: {e}",
                path.display()
            );
        }
    }

    /// C07: this registry's one real recovery policy for a mission
    /// reloaded as `Unknown` - requeue it to `Pending` so the next real
    /// dispatch pass gives it a fresh attempt. Never "assume it
    /// completed" (a false success is exactly what this plan's own
    /// DS05 acceptance criterion forbids for a durable queue: "una
    /// interrupcion no produce exito falso") and never "assume it
    /// failed" (the work may well still be running on a node that just
    /// hasn't been recontacted yet - failing it outright would be an
    /// equally unfounded guess in the other direction). A future phase
    /// with real node reachability at startup could resolve `Unknown`
    /// more precisely; until then, honest resubmission is the only
    /// choice this module makes without evidence.
    pub fn recover_unknown_missions(&mut self) -> Vec<String> {
        let mut requeued = Vec::new();
        for mission in self.missions.values_mut() {
            if matches!(mission.state, MissionState::Unknown { .. }) {
                mission.resolve_unknown_as_pending();
                requeued.push(mission.id.clone());
            }
        }
        if !requeued.is_empty() {
            self.persist_or_warn();
        }
        requeued
    }

    pub fn add(&mut self, id: impl Into<String>) -> &mut Mission {
        let id = id.into();
        let mission = self
            .missions
            .entry(id.clone())
            .or_insert_with(|| Mission::new(id));
        mission
    }

    /// Persists the current snapshot - call after mutating a `Mission`
    /// obtained from `get_mut()`/`add()` (a direct `&mut Mission` method
    /// call, e.g. `.dispatch(...)`, bypasses this registry's own methods
    /// entirely, so it cannot persist itself). A no-op for a pure
    /// in-memory registry (`new()`, no `path`) - existing callers that
    /// never load from a path pay nothing for this.
    pub fn persist(&self) {
        self.persist_or_warn();
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
        if !requeued.is_empty() {
            self.persist_or_warn();
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

    #[test]
    fn dispatch_increments_attempt_and_requeue_never_resets_it() {
        let mut m = Mission::new("m1");
        assert_eq!(m.attempt, 0);
        m.dispatch("node-a").unwrap();
        assert_eq!(m.attempt, 1);
        // A requeue (node went unavailable) does not reset the count -
        // this is real attempt HISTORY, not "attempts since last Pending".
        m.recover_from_unavailable_node("node-a");
        assert_eq!(m.state, MissionState::Pending);
        assert_eq!(m.attempt, 1);
        m.dispatch("node-b").unwrap();
        assert_eq!(m.attempt, 2);
    }

    fn temp_registry_path(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "orchestrator-mission-registry-test-{}-{name}.json",
            std::process::id()
        ))
    }

    #[test]
    fn load_of_a_missing_file_is_a_real_empty_registry_not_an_error() {
        let path = temp_registry_path("missing");
        let reg = MissionRegistry::load(path).expect("a missing registry file must load as empty");
        assert!(reg.all().next().is_none());
    }

    #[test]
    fn a_pure_in_memory_registry_never_touches_disk() {
        // new() (no path) must be a real no-op on persist() - existing
        // callers (mission-demo, every other test in this module) must
        // pay nothing for a persistence feature they never opted into.
        let mut reg = MissionRegistry::new();
        reg.add("m1");
        reg.persist(); // must not panic, and there is no path to write to
    }

    #[test]
    fn dispatched_mission_survives_a_real_restart_as_pending_after_recovery() {
        let path = temp_registry_path("survive-restart");
        {
            let mut reg = MissionRegistry::load(path.clone()).unwrap();
            reg.add("m1").dispatch("node-a").unwrap();
            reg.add("m2"); // stays Pending - never dispatched
            reg.persist();
        } // dropped here - simulates the process exiting uncleanly

        let mut reloaded = MissionRegistry::load(path.clone()).expect("reload must succeed");
        // Immediately after load, a mission that WAS Dispatched is
        // honestly Unknown - not silently still "Dispatched" (this
        // process has no fresh evidence that node-a is even still
        // trying), and not silently lost either (attempt count and id
        // survive).
        assert_eq!(
            reloaded.get("m1").unwrap().state,
            MissionState::Unknown {
                last_node: "node-a".into()
            }
        );
        assert_eq!(reloaded.get("m1").unwrap().attempt, 1);
        // A mission that was already Pending is unaffected by the reload.
        assert_eq!(reloaded.get("m2").unwrap().state, MissionState::Pending);

        let requeued = reloaded.recover_unknown_missions();
        assert_eq!(requeued, vec!["m1".to_string()]);
        assert_eq!(reloaded.get("m1").unwrap().state, MissionState::Pending);
        // The requeue itself is also durable - a SECOND restart right
        // after must not see "Dispatched" again from a stale snapshot.
        let reloaded_again =
            MissionRegistry::load(path.clone()).expect("second reload must succeed");
        assert_eq!(
            reloaded_again.get("m1").unwrap().state,
            MissionState::Pending
        );
        fs::remove_file(&path).ok();
    }

    #[test]
    fn in_progress_mission_also_becomes_unknown_on_reload() {
        let path = temp_registry_path("in-progress-unknown");
        {
            let mut reg = MissionRegistry::load(path.clone()).unwrap();
            let m = reg.add("m1");
            m.dispatch("node-a").unwrap();
            m.start().unwrap();
            reg.persist();
        }
        let reloaded = MissionRegistry::load(path.clone()).expect("reload must succeed");
        assert_eq!(
            reloaded.get("m1").unwrap().state,
            MissionState::Unknown {
                last_node: "node-a".into()
            }
        );
        fs::remove_file(&path).ok();
    }

    #[test]
    fn a_completed_mission_reloads_unchanged_never_becomes_unknown() {
        let path = temp_registry_path("completed-stays-completed");
        {
            let mut reg = MissionRegistry::load(path.clone()).unwrap();
            let m = reg.add("m1");
            m.dispatch("node-a").unwrap();
            m.start().unwrap();
            m.complete().unwrap();
            reg.persist();
        }
        let reloaded = MissionRegistry::load(path.clone()).expect("reload must succeed");
        // A real, confirmed terminal outcome is not an "interruption" -
        // only a mission that was genuinely still in flight at the last
        // persist is honestly uncertain after a restart.
        assert_eq!(
            reloaded.get("m1").unwrap().state,
            MissionState::Completed {
                node: "node-a".into()
            }
        );
        let requeued = MissionRegistry::load(path.clone())
            .unwrap()
            .recover_unknown_missions();
        assert!(requeued.is_empty());
        fs::remove_file(&path).ok();
    }

    #[test]
    fn save_is_atomic_and_leaves_no_temp_file_behind() {
        let path = temp_registry_path("atomic");
        let mut reg = MissionRegistry::load(path.clone()).unwrap();
        reg.add("m1").dispatch("node-a").unwrap();
        reg.persist();
        let dir = path.parent().unwrap();
        let stem = path.file_name().unwrap().to_string_lossy().to_string();
        let leftover: Vec<_> = fs::read_dir(dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| {
                let name = e.file_name().to_string_lossy().to_string();
                name.starts_with(&stem) && name != stem
            })
            .collect();
        assert!(
            leftover.is_empty(),
            "expected no leftover temp file, found: {leftover:?}"
        );
        fs::remove_file(&path).ok();
    }
}
