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
- See `SONNET/13.DISENO_GRPC_NODOS_IA.txt` (private planning doc) for the
  full design rationale, alternatives considered, and the per-family
  service sketch this schema is meant to grow into.

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
