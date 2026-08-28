# HYDRA-UMC-ORCHESTRATOR — CLI Reference

`hydra-umc-orchestrator` is a single Rust binary (`src/main.rs`). Bare
invocation stays a minimal skeleton — it prints identity and exits — while
the real `mission-demo` subcommand runs the project's actual mission state
machine (`src/mission.rs`) end-to-end against an in-memory
`MissionRegistry`, so the coordination logic is exercisable without a real
gRPC/network layer or any real peer service (JOB-DISPATCHER, NODE-HEALING)
to talk to yet. Every example below was captured from a real, built release
binary — the output shown is real, not illustrative.

## Usage

```
$ target/release/hydra-umc-orchestrator mission-demo
```

`run.sh` execs the built binary (`build/hydra-umc-orchestrator` if present,
else `target/release/hydra-umc-orchestrator`) but does **not** forward
arguments — it only ever runs the bare/identity path. To run `mission-demo`,
invoke the built binary directly, as shown throughout this page.

Bare invocation (no arguments, or any unrecognized argument) prints
identity/version/role and exits `0`:

```
$ hydra-umc-orchestrator
HYDRA-UMC-ORCHESTRATOR v0.0.3
Distributed swarm manager: coordinates SWARM-SYNC, PATH-PLANNER-3D, JOB-DISPATCHER and NODE-HEALING as a single unified robot fleet.
```

## Commands

### `mission-demo`

Drives four missions through the real `mission.rs` state machine — dispatch,
in-progress, a simulated node failure and recovery (missions on the failed
node are requeued to `Pending`), an idempotent cancel, a completion, and a
failure — printing every real state transition along the way.

```
$ hydra-umc-orchestrator mission-demo
[orchestrator] mission-1: dispatched -> InProgress(node=node-a)
[orchestrator] mission-2: dispatched -> Dispatched(node=node-a)
[orchestrator] mission-3: dispatched -> InProgress(node=node-b)
[orchestrator] node-a reported UNREACHABLE by NODE-HEALING - recovering its missions
[orchestrator] mission-1: requeued -> Pending
[orchestrator] mission-2: requeued -> Pending
[orchestrator] mission-3: unaffected (different node) -> InProgress(node=node-b)
[orchestrator] mission-2: cancel() -> Cancelled -> Cancelled
[orchestrator] mission-2: cancel() again (idempotent) -> AlreadyCancelled -> Cancelled
[orchestrator] mission-3: complete() -> Completed(node=node-b)
[orchestrator] mission-4: fail() -> Failed(no healthy node accepted redispatch after 3 attempts)
[orchestrator] final registry state:
  mission-1: Pending (terminal=false)
  mission-2: Cancelled (terminal=true)
  mission-3: Completed(node=node-b) (terminal=true)
  mission-4: Failed(no healthy node accepted redispatch after 3 attempts) (terminal=true)
```

Notable real behavior this demo exercises:

- **Node-failure recovery**: when `node-a` is reported unreachable, every
  mission dispatched to it (`mission-1`, `mission-2`) is requeued to
  `Pending` — `mission-3`, on `node-b`, is left untouched.
- **Idempotent cancel**: calling `cancel()` on an already-cancelled mission
  returns `AlreadyCancelled` rather than erroring or double-transitioning.
- **Terminal states**: `Cancelled`, `Completed`, and `Failed` all report
  `is_terminal() == true`; `Pending` does not.

Any argument other than `mission-demo` (including no argument at all) falls
through to the same identity/version output as bare invocation — there is no
usage error path for an unrecognized subcommand:

```
$ hydra-umc-orchestrator bogus
HYDRA-UMC-ORCHESTRATOR v0.0.3
Distributed swarm manager: coordinates SWARM-SYNC, PATH-PLANNER-3D, JOB-DISPATCHER and NODE-HEALING as a single unified robot fleet.
```

## Not yet wired in

There is no real gRPC/network layer yet — `mission-demo` exercises the
mission state machine entirely in-process, against an in-memory
`MissionRegistry`, with no real JOB-DISPATCHER or NODE-HEALING peer on the
other end. Per this project's own module docs, real logic lands as pure,
no-I/O modules first and is wired to a real transport only once there is a
real peer to talk to.
