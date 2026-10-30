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
HYDRA-UMC-ORCHESTRATOR v0.1.6
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
`hydra-umc-orchestrator serve [--addr HOST] [--port PORT] [--job-dispatcher-url URL] [--data-dir DIR] [--token TOKEN] [--allow-anonymous] [--require-attempt] [--job-dispatcher-token TOKEN] [--log-format text|json] [--keep-finished N]`
(`--addr` defaults to `127.0.0.1`, `--port` to `8114`, `--data-dir` to `./data`;
`--job-dispatcher-url` is optional — omitted, missions stay local-only and
`auto-dispatch` reports `503`). The access options are described under
"Access and fencing" below. Startup banner:

```
$ hydra-umc-orchestrator serve --port 8114
[orchestrator] HTTP API listening on 127.0.0.1:8114
[orchestrator] POST /missions, GET /missions, GET /missions/:id,
[orchestrator] POST /missions/:id/{dispatch,auto-dispatch,start,complete,cancel,fail},
[orchestrator] POST /nodes/:node/recover, GET /stats, GET /health
[orchestrator] --job-dispatcher-url not set - missions stay local-only, auto-dispatch disabled
[orchestrator] pending remote-close outbox: data\pending_remote_closes.json
[orchestrator] mission registry: data\missions.json
```

Real routes (`src/server.rs`'s own dispatch table):

| Method | Path | Does |
|---|---|---|
| `POST` | `/missions` | Add a mission: `{"id": "..."}` (1 to 128 of `A-Z a-z 0-9 . _ -`), optionally `priority`, `requiredTool`, `location` and `dependsOn`, which travel to JOB-DISPATCHER so its tool-aware and multi-stage routing is used. Posting a mission that already exists and is still open submits it again (harmless, and a way to retry a submission that did not get through); one that already ended is `409` and is not sent to JOB-DISPATCHER. The body may instead be the common Operation of the SDK (a body with `operation_id`, which is what the semantic planner hands over): the id of the operation is the id of the mission and `priority`, `required_tool`, `location` and `depends_on` are read from its `params`; only an operation with `schema_version` `1.0` in the state `received` is taken (anything else is a `400`) |
| `GET` | `/missions` | List every mission; `?state=Pending` narrows by state, `?limit=N&offset=N` pages |
| `GET` | `/missions/:id` | Get one mission |
| `POST` | `/missions/:id/dispatch` | Assign the mission to a node (`{"node": "..."}`); the mission's `attempt` goes up by one. Refused with 409 when a job-dispatcher is configured (it owns the assignment): use `auto-dispatch` |
| `POST` | `/missions/:id/auto-dispatch` | Submit the mission again if it is still pending (idempotent), run a real JOB-DISPATCHER dispatch pass (`run_dispatch`) and reconcile every assignment it returns, not only this mission's own. Work JOB-DISPATCHER hands out for a mission that is already closed here is handed back, so its robot is not left reserved |
| `POST` | `/missions/:id/start` | Mark `InProgress`; optional `{"attempt": n, "node": "..."}` fences the report (see below) |
| `POST` | `/missions/:id/complete` | Mark `Completed`, confirming the real terminal outcome to JOB-DISPATCHER; same optional fencing fields |
| `POST` | `/missions/:id/cancel` | Idempotent cancel, confirming to JOB-DISPATCHER on a fresh (non-repeat) cancellation: a job nobody was given is withdrawn there (`/jobs/cancel`), one a robot holds is closed as failed (`/jobs/complete`) |
| `POST` | `/missions/:id/fail` | `{"reason": "...", "attempt": n, "node": "..."}` (the last two optional), confirming to JOB-DISPATCHER the same way `cancel` does |
| `POST` | `/nodes/:node/recover` | HYDRA-UMC-NODE-HEALING's own real caller — requeues every mission dispatched to `node` (see that project's own `OrchestratorReactor`) and asks JOB-DISPATCHER (`/jobs/requeue`) to take those jobs off that robot and hold it unavailable until it reports in again. `%XX` escapes in `:node` are decoded. The node name must be the robot id JOB-DISPATCHER assigned |
| `GET` | `/stats` | `{"missionCount": ..., "missionsByState": {...}, "pendingRemoteCloses": n, "oldestPendingCloseAgeSeconds": n or null, "jobDispatcherUrl": ..., "jobDispatcher": {"lastOkAtMs": ..., "lastError": ..., "lastErrorAtMs": ...}}` |
| `GET` | `/health` | `{"status": "ok", "name": "HYDRA-UMC-ORCHESTRATOR", "version": ...}`; the only route that never asks for the token, so a watchdog can probe it |

#### Access and fencing

* **Token.** `--token` (or the `ORCHESTRATOR_TOKEN` environment variable, which
  keeps the value out of the process list) makes every route except
  `GET /health` demand `Authorization: Bearer <token>`; anything else is `401`.
  On a non-loopback `--addr` the process refuses to start without a token
  unless `--allow-anonymous` is given on purpose. On `127.0.0.1` nothing is
  required, as before. `--job-dispatcher-token` (or `JOB_DISPATCHER_TOKEN`)
  is sent on every call to JOB-DISPATCHER, which asks for one the moment it
  listens on a network address.
* **Body size.** A request body over 64 KB is refused with `413` before it is
  read into memory (a body that announces more than that is refused without
  reading any of it).
* **Concurrency and timeouts.** Requests are served by eight worker threads,
  so a slow client or a slow JOB-DISPATCHER holds one worker and `GET /health`
  keeps answering. Every call to JOB-DISPATCHER gives up after 3 s to connect
  and 10 s in all.
* **Fencing.** `attempt` goes up on every dispatch and every time the mission
  is taken away from a node and requeued, and is never reused. A node declared
  unavailable may still be running the mission it lost; its late
  `start`/`complete`/`fail` carries the old attempt (or its own
  name, no longer the holder) and is answered `409` with
  `{"fence": {"kind": "stale_attempt", "reported": 1, "current": 2}}` (or
  `wrong_node`) without touching the mission. A request without these fields
  still works, for callers that predate them; `--require-attempt` makes the
  attempt mandatory (`400` without it) once every node sends it. The `attempt`
  a node must echo is in the response of `dispatch`/`auto-dispatch` and in
  `GET /missions/:id`.
* **Durability.** The mission registry and the outbox are written to a temp
  file, forced to disk and then renamed over the real file, so a power cut
  cannot leave a truncated registry behind.

* **Log format.** `--log-format json` (or `ORCHESTRATOR_LOG_FORMAT=json`) writes every log line as one JSON object, `{"ts_ms": ..., "level": "info|warn|error", "msg": "..."}`, instead of `[orchestrator] message`; `text` is the default.
* **Finished missions.** `missions.json` is rewritten whole on every change, so it grows with every mission ever created. `--keep-finished N` (default `0`: keep every mission) forgets, once every 30 s, the oldest finished missions beyond the `N` last ones, but only those whose close JOB-DISPATCHER confirmed; a mission still waiting for its close is never forgotten. The file carries a `schema_version`; a file written by a newer version is refused at start (the file is left untouched) instead of being read wrongly.
* **Closing a job.** When a mission is closed the orchestrator reads the job (`GET /jobs/{id}`) and completes it naming the robot and, when JOB-DISPATCHER reports one, the `attempt` it just read, so a job that was reassigned between the read and the write answers `409` and the close is simply tried again.
* **Health of the dispatcher link.** `/stats` shows `oldestPendingCloseAgeSeconds` (how long the close that has waited longest has been pending) and, under `jobDispatcher`, when a call to JOB-DISPATCHER last worked and the last error with its time.

A mission's closing is confirmed to JOB-DISPATCHER after the local transition
and never blocks or reverts it. The intent is recorded first, in the mission
registry (`remote_close_confirmed: false`) and in a durable outbox
(`outbox.rs`), before JOB-DISPATCHER is asked, so a process that dies in
between restarts with the close still pending. A close that fails is retried
after 30 s, 1 min, 2 min ... up to 30 min, and `/stats` shows how many wait.
A close that JOB-DISPATCHER keeps refusing stays in the outbox until it is
accepted (the job is gone or already over there, which counts as done): the
orchestrator never drops it silently.

A fatal startup failure (the data directory or mission registry can't be
read/created, or the port is already bound) prints `[orchestrator] fatal: ...`
and exits with a real nonzero status - never `0` - so a process supervisor
watching the exit code can tell a crash apart from a clean shutdown.

Any argument other than `mission-demo`/`serve` (including no argument at all)
falls through to the same identity/version output as bare invocation — there
is no usage error path for an unrecognized subcommand:

```
$ hydra-umc-orchestrator bogus
HYDRA-UMC-ORCHESTRATOR v0.1.6
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
