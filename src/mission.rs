// HYDRA-UMC-ORCHESTRATOR - src/mission.rs
// Copyright (C) 2026 JuanenRac (Electro Hobby 3D) <electrohobby3d@gmail.com>
// GPL-3.0-or-later - see LICENSE
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
// `MissionRegistry` does own real local file I/O now -
// `load()`/`persist()`, the same "one JSON file, load-or-empty,
// temp+rename persist" shape `outbox.rs` already established in this
// same crate - so a real mission this process knows about survives a
// real process restart, not just the pending remote-close intents
// outbox.rs already covered.

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
/// `Unknown` is a first-class state, not an absence of state. It
/// exists for exactly one real situation - `MissionRegistry::load()`
/// reloading a mission that was `Dispatched`/`InProgress` at the moment
/// this process last persisted its own state, before an unclean
/// shutdown or crash. That on-disk snapshot cannot say whether the
/// assigned node actually finished, failed, or is still working -
/// claiming it survived as `Dispatched`/`InProgress` would be lying
/// about a status this process no longer has any evidence for.
/// `Unknown` names that honestly instead of picking a guess, and only
/// `resolve_unknown_as_pending()` (a deliberate decision, taken by
/// `MissionRegistry::recover_unknown_missions()` right after a real
/// load) ever leaves it - the same "an interruption must never look
/// like a false success" rule HYDRA-UMC-DEV-SERVER's own task queue
/// applies to its durable jobs.
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

/// Writes `bytes` to `tmp_path`, forces them to disk, then renames over
/// `path`. A rename alone is atomic against a crash of this process but not
/// against a power cut: without the `sync_all` the new file can be renamed
/// into place while its contents are still only in the page cache, and the
/// mission registry comes back empty or truncated after the reboot.
pub(crate) fn durable_replace(
    tmp_path: &std::path::Path,
    path: &std::path::Path,
    bytes: &[u8],
) -> io::Result<()> {
    use std::io::Write;
    {
        let mut file = fs::File::create(tmp_path)?;
        file.write_all(bytes)?;
        file.sync_all()?;
    }
    fs::rename(tmp_path, path)?;
    // best effort: persist the rename itself (a no-op where a directory
    // cannot be opened, e.g. Windows)
    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
        if let Ok(handle) = fs::File::open(dir) {
            let _ = handle.sync_all();
        }
    }
    Ok(())
}

/// Why a report about a mission was refused before it could change anything.
/// A node that was declared unavailable may still be running the mission it
/// lost; the `attempt` number (bumped on every dispatch and never reused) is
/// the fencing token that lets this registry tell its late report apart from
/// the report of the node that holds the mission now.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum FenceError {
    /// the registry was told to demand an attempt number and none came
    AttemptRequired,
    /// the report names an attempt that is no longer the current one
    StaleAttempt { reported: u32, current: u32 },
    /// the report comes from a node that is not the one holding the mission
    WrongNode { reported: String },
}

impl std::fmt::Display for FenceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FenceError::AttemptRequired => {
                write!(f, "this orchestrator requires the \"attempt\" the mission was dispatched with")
            }
            FenceError::StaleAttempt { reported, current } => write!(
                f,
                "attempt {reported} is stale: the mission is on attempt {current} and was handed over since"
            ),
            FenceError::WrongNode { reported } => {
                write!(f, "node {reported:?} does not hold this mission")
            }
        }
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

/// What Job-Dispatcher needs to route the mission: the priority, the tool
/// the robot must have, the location it prefers and the jobs that must be
/// done first. All optional; the defaults mean "any robot, normal priority".
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct JobSpec {
    #[serde(default)]
    pub priority: i32,
    #[serde(default, rename = "requiredTool")]
    pub required_tool: String,
    #[serde(default)]
    pub location: String,
    #[serde(default, rename = "dependsOn")]
    pub depends_on: Vec<String>,
}

impl JobSpec {
    fn is_default(&self) -> bool {
        *self == JobSpec::default()
    }
}

/// True for an id that can be addressed in a URL path and that
/// Job-Dispatcher will take: 1 to 128 of `A-Z a-z 0-9 . _ -`.
pub fn is_valid_mission_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Mission {
    pub id: String,
    pub state: MissionState,
    // The fencing generation of this mission: it goes up on every dispatch
    // AND every time the mission is taken away from a node and requeued
    // (a node declared unavailable, or a restart that left it `Unknown`),
    // and is never reset or reused. A node reports the attempt it was given
    // at dispatch, so everything it says after the mission was taken from
    // it - even while the mission waits in `Pending` for its next dispatch -
    // carries a number that is no longer the current one and is refused.
    #[serde(default)]
    pub attempt: u32,
    // whether
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
    #[serde(default = "default_true")]
    pub remote_close_confirmed: bool,
    // routing hints forwarded to Job-Dispatcher; absent from the JSON when
    // they are all default
    #[serde(default, skip_serializing_if = "JobSpec::is_default")]
    pub spec: JobSpec,
    // unix milliseconds of the moment the mission reached a terminal state (0:
    // not finished, or finished before this was recorded); only used to decide
    // which finished missions are the oldest when they are compacted
    #[serde(default, skip_serializing_if = "is_zero_u64")]
    pub finished_at_ms: u64,
    // the plan this mission is a stage of (see plan.rs); absent for a plain mission
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan: Option<String>,
}

fn is_zero_u64(value: &u64) -> bool {
    *value == 0
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn default_true() -> bool {
    true
}

impl Mission {
    pub fn new(id: impl Into<String>) -> Self {
        Mission {
            id: id.into(),
            state: MissionState::Pending,
            attempt: 0,
            remote_close_confirmed: true,
            spec: JobSpec::default(),
            finished_at_ms: 0,
            plan: None,
        }
    }

    /// called by server.rs right after a terminal transition
    /// (complete()/cancel()) when confirming it to Job-Dispatcher failed.
    pub fn mark_remote_close_pending(&mut self) {
        self.remote_close_confirmed = false;
    }

    /// called by server.rs once a retried confirmation to
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

    /// marks this mission `Unknown` - called only by
    /// `MissionRegistry::load()` for a mission that was `Dispatched`/
    /// `InProgress` at the moment this process last persisted its own
    /// state. `last_node` preserves which node it was last assigned to,
    /// purely for a human/log to see - it carries no operational meaning
    /// once `Unknown`, since that node's real status for this mission is
    /// exactly what is no longer known.
    fn mark_unknown(&mut self, last_node: String) {
        self.state = MissionState::Unknown { last_node };
    }

    /// the only way an `Unknown` mission ever leaves that state -
    /// see `MissionRegistry::recover_unknown_missions()`'s own doc
    /// comment for why "requeue to Pending" is this registry's one real
    /// policy today (never "assume it completed", never "assume it
    /// failed"). A no-op, successful `Ok(())` on a mission that was
    /// never `Unknown` in the first place - the caller doesn't need its
    /// own branch for "nothing to resolve here."
    pub fn resolve_unknown_as_pending(&mut self) {
        if matches!(self.state, MissionState::Unknown { .. }) {
            self.state = MissionState::Pending;
            // whoever held it before the restart no longer does
            self.attempt += 1;
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
                self.finished_at_ms = now_ms();
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
                self.finished_at_ms = now_ms();
                Ok(CancelOutcome::Cancelled)
            }
        }
    }

    /// Checks a start/complete/fail report against the dispatch that is
    /// current. `attempt` and `node` are optional in the request for
    /// compatibility with callers that predate them; a supplied value is
    /// always checked, and `require_attempt` makes the attempt mandatory.
    pub fn check_fence(
        &self,
        attempt: Option<u32>,
        node: Option<&str>,
        require_attempt: bool,
    ) -> Result<(), FenceError> {
        match attempt {
            Some(reported) if reported != self.attempt => {
                return Err(FenceError::StaleAttempt {
                    reported,
                    current: self.attempt,
                })
            }
            None if require_attempt => return Err(FenceError::AttemptRequired),
            _ => {}
        }
        if let Some(reported) = node {
            let holder = match &self.state {
                MissionState::Dispatched { node } | MissionState::InProgress { node } => {
                    Some(node.as_str())
                }
                _ => None,
            };
            if holder != Some(reported) {
                return Err(FenceError::WrongNode {
                    reported: reported.to_string(),
                });
            }
        }
        Ok(())
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
                // revoke the attempt that node was given: its late reports
                // now name a number that is no longer current
                self.attempt += 1;
                RecoveryOutcome::Requeued
            }
            _ => RecoveryOutcome::NotAffected,
        }
    }

    pub fn is_terminal(&self) -> bool {
        self.state.is_terminal()
    }

    /// The name of the state (`Pending`, `Dispatched`, ...), without its data.
    pub fn state_label(&self) -> &'static str {
        self.state.label()
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
        self.finished_at_ms = now_ms();
        Ok(())
    }
}

/// The layout of `missions.json` this version writes. A file without the field was
/// written before it existed and is layout 1; a file that says a higher number was
/// written by a newer version and is refused instead of being read wrongly (and
/// overwritten on the next change).
pub const REGISTRY_SCHEMA_VERSION: u32 = 1;

#[derive(Serialize, Deserialize)]
struct PersistedRegistry {
    #[serde(default = "first_schema_version")]
    schema_version: u32,
    missions: Vec<Mission>,
}

fn first_schema_version() -> u32 {
    1
}

/// Tracks every mission the orchestrator currently knows about, keyed by
/// id. `BTreeMap` (not `HashMap`) so `all()`/iteration order is
/// deterministic - useful for both the demo CLI output and tests.
///
/// `path` is `None`
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
        if state.schema_version > REGISTRY_SCHEMA_VERSION {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "{} has schema_version {}, newer than the {} this version reads",
                    path.display(),
                    state.schema_version,
                    REGISTRY_SCHEMA_VERSION
                ),
            ));
        }
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
            schema_version: REGISTRY_SCHEMA_VERSION,
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
            durable_replace(&tmp_path, path, json.as_bytes())
        })();
        if let Err(e) = result {
            crate::log_line!(
                "could not persist mission registry at {}: {e}",
                path.display()
            );
        }
    }

    /// this registry's one real recovery policy for a mission
    /// reloaded as `Unknown` - requeue it to `Pending` so the next real
    /// dispatch pass gives it a fresh attempt. Never "assume it
    /// completed" (an interruption must never masquerade as a real
    /// success for a durable queue) and never "assume it failed" (the
    /// work may well still be running on a node that just hasn't been
    /// recontacted yet - failing it outright would be an equally
    /// unfounded guess in the other direction). A future phase with real
    /// node reachability at startup could resolve `Unknown` more
    /// precisely; until then, honest resubmission is the only choice
    /// this module makes without evidence.
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

    /// Forgets the oldest finished missions so `missions.json` (rewritten whole on every
    /// change) does not grow for ever: of the missions that are over AND whose close was
    /// confirmed to Job-Dispatcher, only the `keep` that finished last stay. A mission
    /// whose close is still pending is never forgotten. Returns how many were removed.
    pub fn compact_finished(&mut self, keep: usize) -> usize {
        let mut finished: Vec<(u64, String)> = self
            .missions
            .values()
            .filter(|m| m.is_terminal() && m.remote_close_confirmed)
            .map(|m| (m.finished_at_ms, m.id.clone()))
            .collect();
        if finished.len() <= keep {
            return 0;
        }
        finished.sort();
        let excess = finished.len() - keep;
        for (_, id) in finished.into_iter().take(excess) {
            self.missions.remove(&id);
        }
        self.persist_or_warn();
        excess
    }

    pub fn add(&mut self, id: impl Into<String>) -> &mut Mission {
        let id = id.into();
        let mission = self
            .missions
            .entry(id.clone())
            .or_insert_with(|| Mission::new(id));
        mission
    }

    /// Like `add`, and when the mission is new it carries `spec`. An
    /// existing mission is returned as it is (its spec is never rewritten).
    pub fn add_with_spec(&mut self, id: impl Into<String>, spec: JobSpec) -> &mut Mission {
        let id = id.into();
        self.missions.entry(id.clone()).or_insert_with(|| {
            let mut mission = Mission::new(id);
            mission.spec = spec;
            mission
        })
    }

    /// Like `add_with_spec`, for a stage of `plan`. An existing mission is returned as it is.
    pub fn add_stage(&mut self, id: impl Into<String>, spec: JobSpec, plan: &str) -> &mut Mission {
        let id = id.into();
        self.missions.entry(id.clone()).or_insert_with(|| {
            let mut mission = Mission::new(id);
            mission.spec = spec;
            mission.plan = Some(plan.to_string());
            mission
        })
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
        // A requeue (node went unavailable) never resets the count; it moves
        // it forward, so what the node that lost the mission says next is
        // already stale while the mission waits in Pending.
        m.recover_from_unavailable_node("node-a");
        assert_eq!(m.state, MissionState::Pending);
        assert_eq!(m.attempt, 2);
        m.dispatch("node-b").unwrap();
        assert_eq!(m.attempt, 3);
    }

    #[test]
    fn a_late_report_from_the_node_that_lost_the_mission_is_stale_even_before_the_next_dispatch() {
        let mut m = Mission::new("m1");
        m.dispatch("node-a").unwrap();
        m.start().unwrap();
        m.recover_from_unavailable_node("node-a");
        // the mission is back in the queue; node-a (partitioned) reports late
        assert_eq!(
            m.check_fence(Some(1), None, true),
            Err(FenceError::StaleAttempt {
                reported: 1,
                current: 2
            })
        );
    }

    #[test]
    fn a_mission_file_without_the_newer_fields_still_loads() {
        let old = r#"{"missions":[{"id":"m1","state":"Pending"}]}"#;
        let path = temp_registry_path("old-format");
        fs::write(&path, old).unwrap();
        let reg = MissionRegistry::load(path.clone()).unwrap();
        let m = reg.get("m1").unwrap();
        assert_eq!(m.attempt, 0);
        assert!(m.remote_close_confirmed);
        assert_eq!(m.spec, JobSpec::default());
        fs::remove_file(&path).ok();
    }

    #[test]
    fn mission_ids_are_validated() {
        for ok in ["m1", "a.b_c-d", &"x".repeat(128)] {
            assert!(is_valid_mission_id(ok), "{ok}");
        }
        for bad in ["", "a/b", "a b", "ñ", &"x".repeat(129)] {
            assert!(!is_valid_mission_id(bad), "{bad}");
        }
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
    fn a_cancelled_mission_stays_cancelled_across_restarts_and_recovery() {
        let path = temp_registry_path("cancelled-stays-cancelled");
        {
            let mut reg = MissionRegistry::load(path.clone()).unwrap();
            let m = reg.add("m1");
            m.dispatch("node-a").unwrap();
            m.cancel().unwrap();
            reg.persist();
        }
        let mut reloaded = MissionRegistry::load(path.clone()).unwrap();
        assert_eq!(reloaded.get("m1").unwrap().state, MissionState::Cancelled);
        // A cancelled mission is a deliberate decision, not an
        // interruption: recovery must never turn it back into work.
        assert!(reloaded.recover_unknown_missions().is_empty());
        assert!(reloaded.recover_node_unavailable("node-a").is_empty());
        assert_eq!(
            MissionRegistry::load(path.clone())
                .unwrap()
                .get("m1")
                .unwrap()
                .state,
            MissionState::Cancelled
        );
        fs::remove_file(&path).ok();
    }

    #[test]
    fn a_failed_mission_keeps_its_reason_across_a_restart() {
        let path = temp_registry_path("failed-stays-failed");
        {
            let mut reg = MissionRegistry::load(path.clone()).unwrap();
            let m = reg.add("m1");
            m.dispatch("node-a").unwrap();
            m.fail("node lost power").unwrap();
            reg.persist();
        }
        let mut reloaded = MissionRegistry::load(path.clone()).unwrap();
        assert_eq!(
            reloaded.get("m1").unwrap().state,
            MissionState::Failed {
                reason: "node lost power".into()
            }
        );
        assert!(reloaded.recover_unknown_missions().is_empty());
        fs::remove_file(&path).ok();
    }

    #[test]
    fn cancelling_right_after_a_restart_is_durable_too() {
        let path = temp_registry_path("cancel-after-restart");
        {
            let mut reg = MissionRegistry::load(path.clone()).unwrap();
            reg.add("m1").dispatch("node-a").unwrap();
            reg.persist();
        }
        let mut reloaded = MissionRegistry::load(path.clone()).unwrap();
        reloaded.get_mut("m1").unwrap().cancel().unwrap();
        reloaded.persist();
        let mut again = MissionRegistry::load(path.clone()).unwrap();
        assert_eq!(again.get("m1").unwrap().state, MissionState::Cancelled);
        assert!(again.recover_unknown_missions().is_empty());
        fs::remove_file(&path).ok();
    }

    #[test]
    fn an_unknown_mission_stays_unknown_until_recovery_decides() {
        let path = temp_registry_path("unknown-twice");
        {
            let mut reg = MissionRegistry::load(path.clone()).unwrap();
            reg.add("m1").dispatch("node-a").unwrap();
            reg.persist();
        }
        // First restart marks it unknown; a second restart before anyone
        // recovers it must not lose or upgrade that uncertainty.
        let first = MissionRegistry::load(path.clone()).unwrap();
        first.persist();
        let second = MissionRegistry::load(path.clone()).unwrap();
        assert_eq!(
            second.get("m1").unwrap().state,
            MissionState::Unknown {
                last_node: "node-a".into()
            }
        );
        assert_eq!(second.get("m1").unwrap().attempt, 1);
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

    #[test]
    fn a_report_from_the_node_that_lost_the_mission_is_fenced_out() {
        let mut m = Mission::new("m1");
        m.dispatch("node-a").unwrap();
        m.start().unwrap();
        assert_eq!(m.attempt, 1);
        // node-a is declared unavailable, the mission is handed to node-b
        m.recover_from_unavailable_node("node-a");
        m.dispatch("node-b").unwrap();
        assert_eq!(m.attempt, 3);

        // node-a comes back and reports with the attempt it was given
        assert_eq!(
            m.check_fence(Some(1), Some("node-a"), true),
            Err(FenceError::StaleAttempt {
                reported: 1,
                current: 3
            })
        );
        // even without an attempt, node-a is not the holder
        assert_eq!(
            m.check_fence(None, Some("node-a"), false),
            Err(FenceError::WrongNode {
                reported: "node-a".to_string()
            })
        );
        // node-b, the holder, passes
        assert_eq!(m.check_fence(Some(3), Some("node-b"), true), Ok(()));
    }

    #[test]
    fn the_attempt_is_only_demanded_when_the_orchestrator_is_told_to() {
        let mut m = Mission::new("m1");
        m.dispatch("node-a").unwrap();
        assert_eq!(m.check_fence(None, None, false), Ok(()));
        assert_eq!(
            m.check_fence(None, None, true),
            Err(FenceError::AttemptRequired)
        );
        assert_eq!(m.check_fence(Some(1), None, true), Ok(()));
    }

    #[test]
    fn a_mission_nobody_holds_has_no_node_to_report_from() {
        let m = Mission::new("m1");
        assert_eq!(
            m.check_fence(None, Some("node-a"), false),
            Err(FenceError::WrongNode {
                reported: "node-a".to_string()
            })
        );
    }

    #[test]
    fn durable_replace_leaves_the_new_content_and_no_temp_file() {
        let dir = std::env::temp_dir().join(format!("orchestrator-durable-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("missions.json");
        let tmp = dir.join("missions.json.tmp");
        fs::write(&path, b"old").unwrap();
        durable_replace(&tmp, &path, b"new").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"new");
        assert!(!tmp.exists());
        let _ = fs::remove_dir_all(&dir);
    }
}

#[cfg(test)]
mod edge_tests {
    use super::*;

    fn temp_file(name: &str, content: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "orchestrator-edge-{}-{name}.json",
            std::process::id()
        ));
        fs::write(&path, content).unwrap();
        path
    }

    #[test]
    fn mission_id_length_and_alphabet_boundaries() {
        assert!(is_valid_mission_id(&"a".repeat(128)));
        assert!(!is_valid_mission_id(&"a".repeat(129)));
        for bad in [
            "",
            "a b",
            "a/b",
            "a\\b",
            "..%2F",
            "a?b",
            "a#b",
            "mision-\u{f1}",
            "a\nb",
            "a\0b",
        ] {
            assert!(!is_valid_mission_id(bad), "{bad:?} must be refused");
        }
        for good in [".", "..", "_", "-", "A.b_C-9"] {
            assert!(is_valid_mission_id(good), "{good:?} must be accepted");
        }
    }

    #[test]
    fn a_corrupt_registry_file_is_refused_not_silently_replaced_by_an_empty_one() {
        for (name, content) in [
            ("garbage", "not json"),
            ("truncated", "{\"missions\":[{\"id\":\"m1\""),
            ("empty", ""),
        ] {
            let path = temp_file(name, content);
            let err = MissionRegistry::load(path.clone())
                .map(|_| ())
                .expect_err("a corrupt file must not load");
            assert_eq!(err.kind(), io::ErrorKind::InvalidData, "{name}");
            // The bytes on disk are untouched, so an operator can still inspect them.
            assert_eq!(fs::read_to_string(&path).unwrap(), content);
            fs::remove_file(&path).ok();
        }
    }

    #[test]
    fn resolve_unknown_as_pending_only_touches_unknown_missions() {
        let mut m = Mission::new("m1");
        m.dispatch("node-a").unwrap();
        let attempt = m.attempt;
        m.resolve_unknown_as_pending();
        assert_eq!(
            m.state,
            MissionState::Dispatched {
                node: "node-a".into()
            }
        );
        assert_eq!(
            m.attempt, attempt,
            "a mission that was never unknown keeps its attempt"
        );

        m.mark_unknown("node-a".into());
        m.resolve_unknown_as_pending();
        assert_eq!(m.state, MissionState::Pending);
        assert_eq!(
            m.attempt,
            attempt + 1,
            "the holder before the restart is fenced out"
        );
        m.resolve_unknown_as_pending();
        assert_eq!(
            m.attempt,
            attempt + 1,
            "resolving twice must not bump twice"
        );
    }

    #[test]
    fn the_stale_attempt_is_reported_before_the_wrong_node() {
        let mut m = Mission::new("m1");
        m.dispatch("node-a").unwrap();
        assert_eq!(
            m.check_fence(Some(99), Some("other"), true),
            Err(FenceError::StaleAttempt {
                reported: 99,
                current: 1
            })
        );
    }

    #[test]
    fn nobody_can_report_on_a_mission_that_is_over_or_unknown() {
        let mut done = Mission::new("done");
        done.dispatch("node-a").unwrap();
        done.start().unwrap();
        done.complete().unwrap();
        assert_eq!(
            done.check_fence(Some(1), Some("node-a"), true),
            Err(FenceError::WrongNode {
                reported: "node-a".into()
            })
        );
        let mut unknown = Mission::new("unknown");
        unknown.dispatch("node-a").unwrap();
        unknown.mark_unknown("node-a".into());
        assert_eq!(
            unknown.check_fence(None, Some("node-a"), false),
            Err(FenceError::WrongNode {
                reported: "node-a".into()
            })
        );
    }

    #[test]
    fn the_remote_close_flag_defaults_to_confirmed_and_survives_a_round_trip() {
        let mut m = Mission::new("m1");
        assert!(m.remote_close_confirmed);
        m.mark_remote_close_pending();
        let json = serde_json::to_string(&m).unwrap();
        let back: Mission = serde_json::from_str(&json).unwrap();
        assert!(!back.remote_close_confirmed);
        m.mark_remote_close_confirmed();
        assert!(m.remote_close_confirmed);

        // A record written before the flag existed means "nothing to confirm".
        let mut value: serde_json::Value = serde_json::from_str(&json).unwrap();
        value
            .as_object_mut()
            .unwrap()
            .remove("remote_close_confirmed");
        let old: Mission =
            serde_json::from_value(value).expect("an old mission record still loads");
        assert!(old.remote_close_confirmed);
    }

    #[test]
    fn recovery_for_a_node_does_not_touch_a_pending_mission() {
        let mut reg = MissionRegistry::new();
        reg.add("p");
        let affected = reg.recover_node_unavailable("node-a");
        assert!(affected.is_empty());
        assert_eq!(reg.get("p").unwrap().attempt, 0);
        assert!(reg.get("missing").is_none());
        assert!(reg.get_mut("missing").is_none());
    }

    #[test]
    fn adding_the_same_id_twice_does_not_reset_a_running_mission() {
        let mut reg = MissionRegistry::new();
        reg.add("m1").dispatch("node-a").unwrap();
        let again = reg.add("m1");
        assert_eq!(
            again.state,
            MissionState::Dispatched {
                node: "node-a".into()
            }
        );
        assert_eq!(again.attempt, 1);
    }
}

#[cfg(test)]
mod schema_and_compaction_tests {
    use super::*;

    fn temp(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("orc-missions-{}-{name}.json", std::process::id()))
    }

    fn finished(registry: &mut MissionRegistry, id: &str, finished_at_ms: u64, confirmed: bool) {
        let m = registry.add(id);
        m.dispatch("n").unwrap();
        m.start().unwrap();
        m.complete().unwrap();
        m.finished_at_ms = finished_at_ms;
        m.remote_close_confirmed = confirmed;
    }

    #[test]
    fn the_file_carries_its_schema_version_and_loads_back() {
        let path = temp("version");
        fs::remove_file(&path).ok();
        let mut registry = MissionRegistry::load(path.clone()).unwrap();
        registry.add("m1");
        registry.persist();
        let raw = fs::read_to_string(&path).unwrap();
        assert!(
            raw.contains(&format!("\"schema_version\": {REGISTRY_SCHEMA_VERSION}")),
            "{raw}"
        );
        assert!(MissionRegistry::load(path.clone())
            .unwrap()
            .get("m1")
            .is_some());
        fs::remove_file(&path).ok();
    }

    #[test]
    fn a_file_without_a_version_is_the_first_layout_and_still_loads() {
        let path = temp("legacy");
        fs::write(&path, r#"{"missions":[{"id":"old","state":"Pending"}]}"#).unwrap();
        let registry = MissionRegistry::load(path.clone()).unwrap();
        assert!(registry.get("old").is_some());
        fs::remove_file(&path).ok();
    }

    #[test]
    fn a_file_from_a_newer_version_is_refused_and_left_alone() {
        let path = temp("newer");
        let content = r#"{"schema_version":99,"missions":[]}"#;
        fs::write(&path, content).unwrap();
        let err = MissionRegistry::load(path.clone()).map(|_| ()).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
        assert!(err.to_string().contains("99"), "{err}");
        assert_eq!(fs::read_to_string(&path).unwrap(), content);
        fs::remove_file(&path).ok();
    }

    #[test]
    fn terminal_transitions_record_when_they_happened() {
        let mut registry = MissionRegistry::new();
        let m = registry.add("m");
        assert_eq!(m.finished_at_ms, 0);
        m.cancel().unwrap();
        assert!(m.finished_at_ms > 0);
        let f = registry.add("f");
        f.fail("boom").unwrap();
        assert!(f.finished_at_ms > 0);
    }

    #[test]
    fn compaction_forgets_the_oldest_confirmed_finished_missions_only() {
        let mut registry = MissionRegistry::new();
        finished(&mut registry, "old", 100, true);
        finished(&mut registry, "mid", 200, true);
        finished(&mut registry, "new", 300, true);
        finished(&mut registry, "unconfirmed-oldest", 1, false);
        registry.add("still-pending");
        assert_eq!(registry.compact_finished(2), 1);
        assert!(registry.get("old").is_none());
        for kept in ["mid", "new", "unconfirmed-oldest", "still-pending"] {
            assert!(registry.get(kept).is_some(), "{kept} must stay");
        }
        assert_eq!(registry.compact_finished(2), 0, "nothing more to forget");
        assert_eq!(registry.compact_finished(1), 1);
        assert!(registry.get("mid").is_none() && registry.get("new").is_some());
    }

    #[test]
    fn compaction_is_written_to_the_file() {
        let path = temp("compact");
        fs::remove_file(&path).ok();
        let mut registry = MissionRegistry::load(path.clone()).unwrap();
        finished(&mut registry, "a", 1, true);
        finished(&mut registry, "b", 2, true);
        registry.persist();
        registry.compact_finished(1);
        let reloaded = MissionRegistry::load(path.clone()).unwrap();
        assert!(reloaded.get("a").is_none() && reloaded.get("b").is_some());
        fs::remove_file(&path).ok();
    }
}
