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
$ ./run.sh mission-demo
```

`run.sh`/`run.bat` exec the built binary (`build/hydra-umc-orchestrator`
if present, else `target/release/hydra-umc-orchestrator`) and forward
every argument through, so `./run.sh mission-demo` and invoking the
built binary directly behave identically.

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

### `serve`

The real HTTP API — a genuine `tiny_http` server, not a demo. Started with
`hydra-umc-orchestrator serve [--addr HOST] [--port PORT] [--job-dispatcher-url URL] [--data-dir DIR]`
(`--addr` defaults to `127.0.0.1`, `--port` to `8114`, `--data-dir` to `./data`;
`--job-dispatcher-url` is optional — omitted, missions stay local-only and
`auto-dispatch` reports `503`). Real startup banner, captured from a real run:

```
$ hydra-umc-orchestrator serve --port 8114
[orchestrator] HTTP API listening on 127.0.0.1:8114
[orchestrator] POST /missions, GET /missions, GET /missions/:id,
[orchestrator] POST /missions/:id/{dispatch,auto-dispatch,start,complete,cancel,fail},
[orchestrator] POST /nodes/:node/recover, GET /stats
[orchestrator] --job-dispatcher-url not set - missions stay local-only, auto-dispatch disabled
[orchestrator] pending remote-close outbox: data\pending_remote_closes.json
[orchestrator] mission registry: data\missions.json
```

Real routes (`src/server.rs`'s own dispatch table):

| Method | Path | Does |
|---|---|---|
| `POST` | `/missions` | Add a mission (`{"id": "..."}`) |
| `GET` | `/missions` | List every mission |
| `GET` | `/missions/:id` | Get one mission |
| `POST` | `/missions/:id/dispatch` | Submit this mission's job to JOB-DISPATCHER (`submit_job`) |
| `POST` | `/missions/:id/auto-dispatch` | Run a real JOB-DISPATCHER dispatch pass (`run_dispatch`) and reconcile every assignment it returns, not only this mission's own |
| `POST` | `/missions/:id/start` | Mark `InProgress` |
| `POST` | `/missions/:id/complete` | Mark `Completed`, confirming the real terminal outcome to JOB-DISPATCHER |
| `POST` | `/missions/:id/cancel` | Idempotent cancel, confirming to JOB-DISPATCHER on a fresh (non-repeat) cancellation |
| `POST` | `/missions/:id/fail` | `{"reason": "..."}`, confirming to JOB-DISPATCHER the same way `cancel` does |
| `POST` | `/nodes/:node/recover` | HYDRA-UMC-NODE-HEALING's own real caller — requeues every mission dispatched to `node` (see that project's own `OrchestratorReactor`) |
| `GET` | `/stats` | `{"missionCount": ..., "jobDispatcherUrl": ...}` |

Any JOB-DISPATCHER confirmation call above that fails (network error, or a
job-dispatcher-side inconsistency `job_dispatcher.rs`'s own `complete_job()`
detects) never blocks or reverts the local mission transition - it is
recorded in a real, durable outbox (`outbox.rs`) and retried until it
succeeds, so a transient JOB-DISPATCHER outage can never leave a mission
permanently unreconciled.

A fatal startup failure (the data directory or mission registry can't be
read/created, or the port is already bound) prints `[orchestrator] fatal: ...`
and exits with a real nonzero status - never `0` - so a process supervisor
watching the exit code can tell a crash apart from a clean shutdown.

Any argument other than `mission-demo`/`serve` (including no argument at all)
falls through to the same identity/version output as bare invocation — there
is no usage error path for an unrecognized subcommand:

```
$ hydra-umc-orchestrator bogus
HYDRA-UMC-ORCHESTRATOR v0.0.3
Distributed swarm manager: coordinates SWARM-SYNC, PATH-PLANNER-3D, JOB-DISPATCHER and NODE-HEALING as a single unified robot fleet.
```

## Not yet wired in

`mission-demo` itself still exercises the mission state machine entirely
in-process against an in-memory `MissionRegistry`, with no real peer on the
other end - that demo is unchanged. `serve` (above) is the real thing: a
genuine HTTP/JSON network layer, with real, tested integration to
JOB-DISPATCHER (`job_dispatcher.rs`, over real HTTP via `ureq`) and a real
inbound endpoint NODE-HEALING's own `OrchestratorReactor` calls today. What
remains genuinely unimplemented: no gRPC transport of any kind (every real
integration above is plain HTTP/JSON, not `hydra.common.v1` gRPC), and no
outbound call to NODE-HEALING or SWARM-SYNC from this side.
