# Changelog: HYDRA-UMC-ORCHESTRATOR 🕸️

All notable changes to this project will be documented in this file.

## [Unreleased] - Shared gRPC contract (proto/)

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
- The public protocol comments describe the current contract and its
  compatibility boundaries.

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
