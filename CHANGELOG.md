# Changelog: HYDRA-UMC-ORCHESTRATOR 🕸️

All notable changes to this project will be documented in this file.

## [0.1.0] - C07: MissionRegistry itself now survives a real restart

A revalidation pass found the one durability gap V07-012/`outbox.rs`
(0.0.9) deliberately did not cover: `MissionRegistry` itself was still
purely in-memory, so a real restart forgot every mission's own state,
history and node assignment - only the pending remote-close *intent*
survived, not the mission itself.

Added:

- `MissionRegistry::load(path)`/`persist()` - the same real "one JSON
  file, load-or-empty, temp+rename write" shape `outbox.rs` already
  established and tested for its own pending-close outbox, reused here
  rather than a second mechanism or a new dependency
  (`data/missions.json` by default, `--data-dir` to change it, same
  convention as the outbox).
- A first-class `MissionState::Unknown { last_node }` - what a mission
  that was `Dispatched`/`InProgress` at the last real persist becomes on
  reload, since this process has no fresh evidence for what actually
  happened to it while it was down. Never silently kept as
  `Dispatched`/`InProgress` (a stale claim with no evidence behind it)
  and never silently marked `Completed`/`Failed` (an equally unfounded
  guess) - `MissionRegistry::recover_unknown_missions()`, called once by
  `main.rs` right after load, requeues it to `Pending` for a fresh
  attempt instead.
- `Mission.attempt: u32` - a real per-attempt counter, incremented on
  every real `dispatch()` and never reset by a later requeue, so a
  caller can tell a fresh mission from a retried one.
- Every mutating HTTP handler (`dispatch`/`auto-dispatch`/`start`/
  `complete`/`cancel`/`fail`/`recover`) now persists the registry before
  responding - a real mission created or transitioned through this
  server's own API is durable, not only the ones `mission-demo`'s fixed
  in-memory script happens to run in one process lifetime.

No new dependency, no gRPC/network I/O added to `mission.rs` itself -
this is real local file durability for state that already existed,
following the exact pattern already proven in this same crate.

## [0.0.9] - V07-012: the pending remote-close reconciliation now survives a real restart

A second independent revalidation audit found `reconcile_pending_remote_closes()`'s
own honest, previously-documented limit was real: `MissionRegistry` is
purely in-memory, so a process restart between a mission's local
terminal transition and Job-Dispatcher's own ACK lost the pending intent
entirely - a real orphaned robot reservation with no path back to
consistency short of a human noticing. Also found: `job_dispatcher::complete_job()`
treated ANY 400 from `POST /jobs/complete` as a confirmed close, even
though Job-Dispatcher returns that exact status for both a genuinely
benign case (job never submitted, or already done/failed) and a real
failure (the job is still `assigned` there).

Fixed:

- New `outbox.rs` (`RemoteCloseOutbox`) - a small, real, crash-safe JSON
  file (`data/pending_remote_closes.json` by default, `--data-dir` to
  change it) recording exactly which missions still need a remote-close
  confirmation and which outcome (`success`) to report, independent of
  `MissionRegistry`'s own ephemeral state. `reconcile_pending_remote_closes()`
  now walks THIS as its real worklist (both once immediately at startup
  and on its usual 30s retry), not `MissionRegistry`'s own in-memory
  list - so a fresh incarnation with an empty registry still correctly
  retries confirming a mission it has otherwise completely forgotten.
- `job_dispatcher::complete_job()` now disambiguates a 400 with a real
  `GET /jobs` lookup of that job's own current status instead of
  guessing: still `assigned` is now a real, reported failure; not found,
  or found already terminal/never-dispatched, stays the honest "nothing
  real to close"; and a remote state that cannot even be verified fails
  closed rather than assuming success.

## [0.0.8] - REV-010: real regression found by independent revalidation

An independent revalidation audit reproduced a real gap in v0.0.7's own
ORCH-02 fix (against a real fake Job-Dispatcher, no mocked HTTP):

- **REV-010 [P1]:** `handle_complete()`/`handle_cancel()` commit a
  mission's terminal state locally first, then make a best-effort
  attempt to confirm it to Job-Dispatcher (ORCH-02's own real
  integration) - a failed confirmation (a transient network error,
  Job-Dispatcher briefly unreachable) used to be only logged, with the
  mission looking identical to a genuinely confirmed one. Job-Dispatcher
  could then keep believing the job - and its robot's reservation - was
  still active indefinitely, with no real path back to consistency.
  Fixed: `Mission` gained a real `remote_close_confirmed` field,
  distinguishing "terminal locally" from "terminal AND confirmed
  remotely"; a new background thread (`reconcile_pending_remote_closes`,
  spawned in `run()` only when Job-Dispatcher integration is configured)
  retries the exact same confirmation call every 30s for every mission
  still pending one, until it actually succeeds - safe because
  `job_dispatcher::complete_job()` is itself idempotent.
  **Honest limit, stated explicitly:** this closes the gap within this
  Orchestrator process's own lifetime only - `MissionRegistry` is still
  purely in-memory, so a real process restart loses the pending list
  along with every other mission this process knew about. Real crash/
  restart durability needs a persistence layer this project does not
  have yet - real, separate future work, not attempted here.
- 5 new regression tests, `cargo fmt`/`cargo clippy -D warnings` clean.

## [0.0.7] - ORCH-01/02/03: reconcile every assignment, confirm completion, fix a real test race

- **ORCH-01 (found in an ecosystem-wide software-improvements audit, P1):**
  `handle_auto_dispatch()` ran a real, global `/dispatch` pass on
  Job-Dispatcher, but only reconciled the ONE assignment matching the
  caller's own requested mission id - any OTHER mission the same pass
  assigned (Job-Dispatcher's own algorithm can assign several jobs in one
  pass) stayed looking "Pending" locally while its robot was genuinely
  already reserved on Job-Dispatcher's side. Every returned assignment is
  now reconciled against the local registry, not only the requested one.
- **ORCH-02 (found in the same audit, P1):** `handle_complete()`/
  `handle_cancel()` only ever updated the local mission registry - Job-
  Dispatcher could keep believing a job (and its robot's reservation) was
  still active for a mission this Orchestrator had already closed out
  locally. New `job_dispatcher::complete_job()` confirms the real terminal
  outcome (`POST /jobs/complete {id, success}`) right after each local
  transition succeeds; cancel only confirms on a FRESH cancellation
  (`CancelOutcome::Cancelled`), never on a no-op re-cancel of an
  already-cancelled mission. Best-effort, same reasoning as every other
  job_dispatcher.rs call - dropped the registry lock first.
- **ORCH-03 (found in the same audit, P1):** `server.rs`'s own
  `fake_job_dispatcher` test helper captured an incoming request with a
  single, non-looping `stream.read()` call - the identical race
  `job_dispatcher.rs`'s own `fake_server` helper was already fixed for in
  0.0.6, reintroduced in this sibling helper that never got the same fix.
  `read_full_request()` now loops until it has actually seen the full
  request (headers, plus every declared `Content-Length` body byte)
  before answering. Verified with 8 consecutive full-suite runs (default
  and `--test-threads=16`), not just one green run.
- 5 new regression tests reproduce each finding's own exact scenario:
  reconciling a second mission's assignment from the same dispatch pass,
  confirming a real completion/cancellation to Job-Dispatcher (captured
  via a request-capturing fake), and a no-op re-cancel NOT re-notifying
  it. `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`,
  and `cargo test --all-targets` (51/51) all pass.

- **New `job_dispatcher::requeue_job()`, called from `handle_recover()`**
  - found in an ecosystem-wide software-improvements audit:
  `POST /nodes/:node/recover` only ever updated the local in-memory
  mission registry, never Job-Dispatcher - so Job-Dispatcher could keep
  believing a job was still assigned to a node that just went
  unreachable. Uses Job-Dispatcher's own existing, documented contract,
  no new endpoint needed on that side: `POST /jobs/complete
  {success: false}` marks the job "failed" (only valid from its real
  "assigned" state - exactly the state a job whose robot just went
  unreachable should be in), then `POST /jobs/submit` with the same
  `dedupKey` `submit_job()` already used (the mission's own id) hits the
  real, documented "retried" path - resetting the job to "pending" under
  its original id, eligible for the next real `POST /dispatch` pass.
  Best-effort, same reasoning as `submit_job()`: a mission with no
  matching job on Job-Dispatcher's side (never submitted there, or
  already finished on its own) is a real, benign no-op, not a failure.
  Dropped the mission-registry lock before making this network call, so
  a slow/unreachable Job-Dispatcher can never stall another request that
  only needs the registry. New tests cover the real two-request sequence
  in isolation and `handle_recover()`'s own graceful degradation when
  Job-Dispatcher is configured but unreachable. 47/47 tests pass
  (cargo fmt/clippy/test, 3 consecutive runs).

## [0.0.6]

- **Fixed CI**: `cargo fmt --check` was failing on `src/job_dispatcher.rs`
  (unwrapped lines), and `cargo clippy -- -D warnings` was failing on
  `std::io::Error::new(ErrorKind::Other, e)` (now `std::io::Error::other`,
  clippy's own suggested idiom).
- **Fixed a real flaky-test bug found while verifying the above**:
  `job_dispatcher.rs`'s own test-only `fake_server` captured an incoming
  HTTP request with a single, non-looping `stream.read()` call. `ureq`'s
  `send_string()` can write request headers and body as separate TCP
  writes, and a loopback stack is free to deliver those as separate
  readable chunks - the single read intermittently captured only the
  headers (`Content-Length` correctly declared, body not there yet),
  reproducing deterministically in this environment. `fake_server` now
  reads in a loop until it has seen the full header block and, when
  `Content-Length` is present, that many declared body bytes too. No
  behavior change to `submit_job()`/`run_dispatch()` themselves - `cargo
  test`: 42/42 passing throughout, including 5/5 repeat runs of the
  affected test to confirm the fix is not itself flaky.

## [0.0.5] - The real "full chain": Job-Dispatcher wired in

- **`src/job_dispatcher.rs`** (new) - a real, minimal client for
  `HYDRA-UMC-JOB-DISPATCHER`'s own real HTTP API (`docs/API.md` in that
  repo): `submit_job()` (`POST /jobs/submit`, using the mission id as
  both id and `dedupKey` - a retried `POST /missions` call here must
  never double-submit) and `run_dispatch()` (`POST /dispatch`, that
  project's own real tool-aware/fairness routing pass - `mission.rs`
  never had any matching logic of its own, it only ever recorded
  whatever node a caller told it to). Uses `ureq` (new dependency,
  `default-features = false` - no TLS needed for loopback-only HTTP),
  the real HTTP *client* counterpart to `tiny_http` (server-only).
- **`server.rs`** - `POST /missions` now also submits the new mission to
  Job-Dispatcher when `--job-dispatcher-url` is configured (best-effort:
  a down Job-Dispatcher doesn't stop a mission existing here, this
  registry is the source of truth for mission STATE either way; the
  real outcome is reported back in the response, never silently
  swallowed). New `POST /missions/:id/auto-dispatch` runs one real
  Job-Dispatcher dispatch pass and adopts whichever robot it actually
  assigned into this mission's local state - the manual
  `POST /missions/:id/dispatch {node}` (caller-supplied node) stays
  unchanged as a direct-assignment override.
- **`main.rs`** - new `--job-dispatcher-url` flag for `serve`; omitted,
  behavior is unchanged from before this existed (missions stay
  local-only, `auto-dispatch` answers `503`).
- **`systemd/hydra-umc-orchestrator.service`** - wired to the real
  Job-Dispatcher instance already on this CM5 (`127.0.0.1:8090`), soft-
  ordered `After=` it (not `Requires=` - this integration degrades
  honestly, it doesn't need Job-Dispatcher to be up to start).
- 11 new tests (`job_dispatcher.rs`'s own `#[cfg(test)]` module against
  a real raw-socket fake server, plus 6 new `server.rs` tests covering
  both the best-effort submit and the full auto-dispatch path,
  including the honest "no robot matched this pass" outcome) - 42 total.

## [0.0.4] - Real v0: JSON/HTTP server mode, plus CM5 deployment

- **`mission.rs`** - `MissionState`/`TransitionError`/`CancelOutcome`/
  `RecoveryOutcome`/`Mission` gained a `Serialize` derive (behavior-
  preserving, additive only) so `server.rs` can hand them straight to
  `serde_json` without a second, parallel JSON shape.
- **`server.rs`** (new) - `POST /missions`, `GET /missions`,
  `GET /missions/:id`, `POST /missions/:id/{dispatch,start,complete,
  cancel,fail}`, and `POST /nodes/:node/recover` reach the exact same
  `MissionRegistry`/`Mission` methods `mission-demo` already exercised
  against its own fixed, hardcoded scenario - now reachable with a real
  caller-supplied mission id and node name, over a real `tiny_http`
  server (blocking, no async runtime - same convention as
  `HYDRA-UMC-TWIN`'s own `server.rs`). Unlike this ecosystem's other
  Rust services' `server.rs` (all stateless computations), the
  `MissionRegistry` is real shared, mutable state that must persist
  across requests - `Arc<Mutex<MissionRegistry>>`, one lock per request.
  Still purely in-memory bookkeeping: no real gRPC wiring to
  `HYDRA-UMC-JOB-DISPATCHER`/`HYDRA-UMC-NODE-HEALING` exists, and there
  is no real E-STOP-sending code anywhere in this repository to expose -
  this does not grant any new physical authority, it makes the exact
  same state machine reachable over a real API instead of only a fixed
  demo script.
- **`main.rs`** - new `serve` subcommand (`--addr`/`--port`, default
  `127.0.0.1:8114`).
- **`systemd/hydra-umc-orchestrator.service`** (new) - loopback-only
  unit for `HYDRA-UMC-OS/provisioning/install_orchestrator.sh` (new,
  that repo), compiled as a release binary, same pattern as
  `install_twin.sh`. State resets on every restart (no persistence yet)
  - a real, known limitation, documented in the unit itself, not
  silently hidden.
- 9 new tests (`server.rs`'s own `#[cfg(test)]` module, real end-to-end
  HTTP over a raw `TcpStream`) - 31 total.

## [0.0.3] - Real v0: mission state machine, idempotent cancellation, node-failure recovery

- **`src/mission.rs`** (new) - the real logic behind "arbitrating which robot gets which mission": `Mission` (`Pending -> Dispatched -> InProgress -> Completed`, with `Cancelled`/`Failed` as separate terminal states) and `MissionRegistry` (tracks every mission by id, `BTreeMap`-backed for deterministic iteration). Pure in-memory state machine, no gRPC/network I/O yet - the same "real logic before real transport" sequencing already used by this ecosystem's other v0 passes.
- `Mission::cancel()` is idempotent by design: cancelling an already-`Cancelled` mission returns `CancelOutcome::AlreadyCancelled` (success, not an error) so a retried cancel request never gets a different answer the second time. Cancelling out of `Completed`/`Failed` is refused - finished or already-failed work cannot be retroactively cancelled.
- `Mission::recover_from_unavailable_node()` / `MissionRegistry::recover_node_unavailable()` - the real reaction to a node health report going bad (see `HYDRA-UMC-NODE-HEALING`'s `watchdog::Status::Unreachable`/`Invalid`): a `Dispatched`/`InProgress` mission on the affected node is requeued to `Pending` for redispatch elsewhere; a mission already in a terminal state is left untouched.
- `Mission::fail()` - the real way a mission reaches the `Failed` terminal state (e.g. no healthy node accepted redispatch after repeated recovery attempts), valid from any non-terminal state.
- **`main.rs`** - new `mission-demo` subcommand runs the full scenario end-to-end (dispatch 3 missions across 2 nodes, one node goes `UNREACHABLE`, recovery requeues its missions, one requeued mission is cancelled twice to demonstrate idempotency, the unaffected mission completes, a fourth mission is marked `Failed`) against a real `MissionRegistry`, printing every real transition.
- 22 tests covering every transition (including every invalid-transition rejection) and both `MissionRegistry::recover_node_unavailable` paths (only the affected node's missions requeue; a node with no missions is a safe no-op).
- Fixed `build.sh`: called `bump_manifest_version.py` (no `--sync`) before `bump_version.py`, double-bumping the native version one step ahead of the manifest - reordered to match `build.bat`'s already-correct native-bump-then-sync sequence (same fix already applied to `HYDRA-UMC-HIL-BRIDGE` and `HYDRA-UMC-NODE-HEALING`).

## [0.0.2]
### Added
- New `proto/hydra_common.proto` - the shared gRPC schema for node-to-node
  traffic across the ecosystem's Vision AI Node, Cognitive AI Node,
  Orchestration & Swarm, and Digital Twin & Simulation families. Defines
  `NodeIdentity`, `HealthReport`, and `HealthService` - the one contract
  every node is expected to implement, so HYDRA-UMC-NODE-HEALING can probe
  any node in the ecosystem uniformly instead of each family inventing its
  own ad-hoc health check. Per-family business services (detections,
  intents, job dispatch, physics stepping, ...) are deliberately not
  defined yet - see `proto/README.md` for why.
- New `proto/README.md` documenting the file, why it lives here instead of
  a dedicated repo, and how each language (Python/Rust/Go/Node) generates
  its own bindings from it.
- `README.md` (all 7 languages) directory-structure section updated to
  list `proto/`.
- Verified for real: compiled with `grpc_tools.protoc` (Python target,
  `python -m grpc_tools.protoc -I proto --python_out=... proto/hydra_common.proto`)
  with a clean exit code, then used the generated stub to build a real
  `HealthReport` message, serialize it to bytes, and parse it back -
  `identity.name` and a custom `metrics` entry both round-tripped exactly.
  Not just written to look plausible - genuinely valid protobuf3.
- Copyright headers on `run.bat` and `run.sh`, matching the header already
  present on `src/main.rs`, `bump_version.py`, `build.bat` and `build.sh`.
- Inline "why" comments across `src/main.rs`, `bump_version.py`, `build.bat`,
  `build.sh`, `run.bat` and `run.sh` explaining non-obvious decisions: why
  Rust for this specific orchestrator, why the entry point is a deliberately
  inert skeleton for now, why the odometer-style version bump runs before
  every real build, and why `run.*` checks `build/` before `target/release/`.
- Expanded `README.md` (and its 4 translations) with an advanced technical
  section (internal architecture, Rust rationale, design decisions), a
  detailed build/run walkthrough with a troubleshooting subsection, and a
  new "🔗 Related Projects" section (directly related repos plus the rest
  of the ecosystem grouped by category).
- This `CHANGELOG.md`.

### Changed
- Roadmap section reworded from calendar quarters to phase labels
  (Phase 1-4), across all 5 README languages.

## [0.0.0]
### Added
- Initial Rust skeleton: `Cargo.toml`, `src/main.rs` (prints identity and
  role, exits 0).
- GPL-3.0 copyright headers on source and build scripts.
- Odometer-style version bump (`bump_version.py`), wired into `build.bat`
  and `build.sh` ahead of `cargo build --release`.
- `run.bat` / `run.sh` to launch the compiled binary.
- Multi-language `README.md` (English, Spanish, French, Italian, German).
- `docker-compose.yml` integrating this repository with its 4 children
  (SWARM-SYNC, PATH-PLANNER-3D, JOB-DISPATCHER, NODE-HEALING) as sibling
  checkouts on one shared network.
