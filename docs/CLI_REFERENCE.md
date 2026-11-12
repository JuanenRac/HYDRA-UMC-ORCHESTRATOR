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
`hydra-umc-orchestrator serve [--addr HOST] [--port PORT] [--job-dispatcher-url URL] [--data-dir DIR] [--token TOKEN] [--allow-anonymous] [--require-attempt] [--job-dispatcher-token TOKEN] [--log-format text|json] [--keep-finished N] [--takeover] [--require-healer-lease] [--lease-max-ttl-ms MS] [--swarm-sync-url URL] [--swarm-sync-token TOKEN] [--swarm-sync-interval-s SECONDS] [--orchestrator-name NAME]`
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
| `GET` | `/estop` | The state of the global stop (see below); always answers |
| `POST` | `/estop` | Engage the global stop (see below); always available, idempotent, audited |
| `POST` | `/estop/release` | End the global stop; only an administrator |
| `POST` | `/events/job-dispatcher` | A signed event of the job-dispatcher (only with the `job_events` section of the configuration file; otherwise `404`) |
| `GET` | `/nodes/:node/jobs` | `{"node": ..., "jobs": [{"id", "status", "attempt"}]}`: the jobs JOB-DISPATCHER holds for that robot, read with `GET /jobs?robot=` (read-only; `503` with no dispatcher configured, `502` when it cannot be asked) |
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
* **Identity of HYDRA-UMC-SERVER (off by default).** `--identity-key-file FILE` (or `ORCHESTRATOR_IDENTITY_KEY_FILE`) gives the signing key of the server (its `JWT_SECRET`, 32 characters or more, read from a file and never printed). A signed token of the server (HS256 with `username`, `role` and `exp`) is then accepted next to `--token`: a viewer may read but not change (and may read the state of the global stop but not engage it), `admin` and `operator` may change, and a request with no credential is refused even on loopback, `GET /health` and `GET /metrics` included. A non-loopback `--addr` no longer needs `--token`. Without the switch nothing changes.
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

### `standby`

`hydra-umc-orchestrator standby --primary-url URL [--primary-token TOKEN] [--primary-name NAME] [--misses N] [--interval-s SECONDS] [--data-dir DIR]`

A second orchestrator that watches the first one and takes its registry over when the first goes silent. Nothing is served and nothing is dispatched: every `--interval-s` seconds
(5 by default, 1 to 3600) it asks `GET /health` of the primary (any 2xx answer counts as alive) and, when it is alive, keeps the newest readable copy of `GET /replica?owner=NAME`
(`--primary-name`, the `--orchestrator-name` of the primary, `orchestrator` by default; a plain name of letters, digits and `- _ .`). The token is `--primary-token` or the
`ORCHESTRATOR_PRIMARY_TOKEN` environment variable (the environment keeps it out of the process list); without one no `Authorization` header is sent. Any answer resets the count of
silent rounds, so a primary that flaps is not replaced. After `--misses` silent rounds in a row (3 by default, 1 to 1000) the standby takes the copy into the registry of `--data-dir`
(`data/missions.json`) **once**, prints what it did (`adopted`, `requeued`, `unconfirmed`, `already held`, `rejected`) and exits 0; whoever supervises the process then starts
`serve` on the same data directory. A mission the registry already holds is never overwritten.

No robot can acknowledge a stop to the standby yet, so no proof of a stop is ever gathered: every mission that was in flight on the primary is kept as `Unknown` and is not
dispatched a second time while it may still be running; only the missions that were still queued are adopted as they were. An option that cannot be used (no `--primary-url`, an
address that does not start with `http://` or `https://`, a limit out of range, a name that is not plain, a registry file that cannot be read) is `fatal:` with exit status 1.

Any argument other than `mission-demo`, `serve` or `standby` (including no argument at all)
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

## Multi-stage plans (`POST /plans`)

A plan is a graph of stages: `{"id": "board-7", "stages": [{"id": "mill", "priority": 2, "requiredTool": "Spindle"}, {"id": "drill", "dependsOn": ["mill"]}]}`.
It is checked once, when it is accepted: the plan id and the stage ids are made of `A-Z a-z 0-9 _ -` (the dot separates them), no stage appears twice, every
`dependsOn` names a stage of the plan, there is no cycle and there are at most 64 stages; anything else is a `400` and nothing is created. Each stage becomes an
ordinary mission with the id `<plan>.<stage>` and the full ids of its predecessors as `dependsOn`, so the job-dispatcher releases a stage when the ones before it are
done; posting the same plan again keeps the stages that exist, and a stage id that already belongs to another mission is a `409`.

`GET /plans/{id}` answers the state of the plan (`pending`, `running`, `completed`, `failed` or `cancelled`) and, for each stage, its state, its `dependsOn`
and `blocked_by`: the stages that failed or were cancelled and hold it back, directly or through other stages (a pending stage held back shows the state
`Blocked`). A failed stage does not fail the rest: the stages after it wait, the plan reports `failed`, and nothing after it runs on its own. `GET /plans` lists the plans.
Without a job-dispatcher, `POST /missions/{id}/dispatch` of a stage answers `409` (`waitingFor`) until every predecessor is completed.

## Replicating the registry (`GET /replica`)

`GET /replica` (`?owner=NAME`, default `orchestrator`) exports every mission as the record HYDRA-UMC-SWARM-SYNC replicates under `mission/<id>`:
`{"mission_id", "status", "owner", "attempt"}` with the generation `attempt + 1`, which the swarm-sync map compares before the clock, so a newer attempt wins on every node
whatever the clocks say. The record carries no routing hints: the job-dispatcher holds the jobs.

## Healer leases (`/leases`)

Two watchdogs on the same cell would both ask for the recovery of the same node. A watchdog takes the lease `healer-of-<cell>` first and shows it with every recovery request.

* `POST /leases/{name}/acquire` with `{"holder": "cm5", "ttlMs": 15000}` answers `200` with `{"lease", "holder", "token", "expiresAtMs", "expiresInMs"}`, or `409` with the holder and the expiry when someone else holds it. The holder asking again keeps its token and gets a longer lease. A name and a holder are 1 to 128 letters, digits or `. _ : -`; the time is kept between 1 second and `--lease-max-ttl-ms` (60000 by default).
* `POST /leases/{name}/renew` and `/release` take `{"holder", "token"}`; a wrong holder, a wrong token or an expired lease is `409`. A release frees the lease at once, so the next watchdog does not wait for it to expire.
* `GET /leases` lists the leases that have not expired. The table is in memory on purpose (a lease lasts seconds; after a restart the watchdogs acquire it again), and a new token is never lower than the clock in milliseconds, so it cannot go back.
* `POST /nodes/{node}/recover` with the headers `X-Healer-Lease: healer-of-<cell>`, `X-Healer-Id` and `X-Healer-Token` is accepted only from the current holder (`409`, `rejected: not_lease_holder`, otherwise). Without the headers the request is refused while any healer lease is held, and always when the process starts with `--require-healer-lease` (or `ORCHESTRATOR_REQUIRE_HEALER_LEASE=1`); with no lease held and the option off, a recovery works as it always did.

HYDRA-UMC-NODE-HEALING takes and renews the lease with `--healer-cell`.

## Pushing the registry to swarm-sync (`--swarm-sync-url`)

Off unless `--swarm-sync-url` is given. Every `--swarm-sync-interval-s` seconds (5 by default) the records of `GET /replica` that differ from what was pushed last go out in one `POST /reconcile` of the swarm-sync node (one cell named after `--orchestrator-name`, a writer number taken from that name). The token is `--swarm-sync-token` or `SWARM_SYNC_TOKEN`. The clock of the writes starts past the newest time in the map (`GET /state?full=1`) and goes up with each write; the generation is `attempt + 1`, which decides which side wins. A push that fails is logged once and tried again with the same records. The second orchestrator reads the map and takes the registry over with `POST /replica/takeover`, which still needs the proof that the robot stopped.

## Taking the registry over (`POST /replica/takeover`)

Off by default: the route exists only when the process starts with `--takeover`; otherwise it answers the same `404` as any unknown path.
The body is `{"records": [...], "confirmed_stopped": [{"owner": "node-a", "mission_id": "m1", "attempt": 0}]}` (the records exactly as `GET /replica` of the other orchestrator
gave them; at most 5000 records and 5000 proofs; an unknown field is a `400`). A mission that was in flight goes back to `Pending` with its attempt raised only when a proof
names its holder, its id and the attempt it had; with no such proof it stays `Unknown`, so it is not dispatched a second time while it may still be running. The proof is the
acknowledgement that the robot gave to the stop request of the job-dispatcher (`POST /jobs/stop-ack`, delivered by HYDRA-UMC-SERVER, see its `docs/JOB_STOP_RELAY.md`);
nothing else counts. A mission this registry already holds is never overwritten. The answer lists `adopted`, `requeued`, `unconfirmed`, `existing` and `rejected`.
The records carry no routing hints, so a taken-over mission is not resubmitted to the job-dispatcher by this route.

## The global stop (`/estop`)

`POST /estop` stops the dispatching of the whole fleet from the orchestrator. It is a safety function and is built so that nothing gets in its way:

* **It is never switched off and never waits.** The latch is an atomic flag set before anything else happens; the list of the missions that were moving is read with `try_lock` (a busy registry does not delay the stop; the list is then `null`); a body that is empty, not JSON or has strange fields still engages it; the call to the server has its own thread and a short timeout.
* **Who may.** The general bearer token (an operator) or the administrator token. No confirmation is asked: a stop is safe. `POST /estop/release` needs the administrator token (`estop.admin_token_env`); with a single token, or none (a loopback laboratory), that one is the administrator. A release by an operator is a `403` and is written to the audit.
* **What it does.** It engages the latch, forwards the stop to HYDRA-UMC-SERVER (`POST <estop.server_url>/api/stop-all`, with the bearer token of `estop.server_token_env`), which stops every robot and, when its own option is on, announces the retained stop on the MQTT bridge; the answer says `server`: `confirmed`, `failed: <why>`, `already_confirmed` or `not_configured`. A repeat retries the call only while it was not confirmed. The answer also lists `affected_missions`.
* **While it is engaged** `POST /missions/:id/dispatch`, `auto-dispatch` and `start` answer `423`, and an `assigned` event of the job-dispatcher is not applied. Cancelling, completing, failing and recovering stay possible.
* **Idempotent and audited.** Engaging it again changes nothing (`already_engaged`, the first author is kept). Every stop, repeat, release and refused release is one JSON line in `data/estop_audit.jsonl` and in the log.
* **It does not clear by itself.** The latch is saved to `data/estop.json` and survives a restart; a saved state that exists but cannot be read counts as engaged. Only `POST /estop/release` ends it. Releasing here does not release the stop of the server: that one is rearmed explicitly on its own side.
* **What it is not.** It is software on the network. The controller and the actuator keep the last word, and the MQTT announcement depends on the option of the server and on the broker. Marking the moving missions as paused is not done: the registry has no such state yet.

## Configuration file (`--config`)

`--config FILE` (or `ORCHESTRATOR_CONFIG`) reads the tuned values of the global stop, of the events of the job-dispatcher and of the health service over gRPC. An unknown key stops the start. Secrets are never in the file: each one is named by an environment variable.

```json
{
  "estop": {"server_url": "http://127.0.0.1:18080", "server_token_env": "ORCHESTRATOR_ESTOP_SERVER_TOKEN", "fanout_timeout_seconds": 3, "admin_token_env": "ORCHESTRATOR_ADMIN_TOKEN"},
  "job_events": {"enabled": true, "secret_env": "ORCHESTRATOR_JD_WEBHOOK_SECRET", "max_skew_seconds": 300},
  "health_grpc": {"enabled": true, "listen": "127.0.0.1:50114"},
  "metrics": {"enabled": true}
}
```

* **`estop`:** where the stop is forwarded and who administers it. Without the section the stop still works, kept in `data/estop.json`, and is not forwarded.
* **`job_events`:** off unless `enabled` is `true`. `POST /events/job-dispatcher` accepts an event only when `X-Hydra-Signature` is `sha256=` plus the HMAC-SHA256 of `"<X-Hydra-Timestamp>.<body>"` with the secret (the one the job-dispatcher signs with, `events.webhooks[].secret_env` there), compared in constant time, and the timestamp is within `max_skew_seconds` (1 to 86400, default 300) of the clock of this machine. The bearer token of the API is not what lets an event in. An `assigned` event moves a `Pending` mission to `Dispatched` on the robot it names (the same step as `auto-dispatch`); `done`, `failed` and `unknown` are only counted, because the outcome of a mission is decided by what its node reports. `GET /stats` shows the counters under `jobEvents`.
* **`health_grpc`:** off unless `enabled` is `true`. Serves `Check()` and `Watch()` of the shared `HealthService` on `listen` (default `127.0.0.1:50114`); an address other than loopback needs a token (`token_env`, or the token of the API). Every key, the states it reports and how to point the watchdog at it are in [`HEALTH_GRPC.md`](HEALTH_GRPC.md).
* **`metrics`:** `GET /metrics` in the text format of Prometheus, off unless `enabled` is `true` (closed, the route answers `404` like any unknown path, even with the token); read at start. Open, it answers a caller on this machine, or any caller that presents the bearer token when the API has one; a caller that is neither gets `401`. It publishes numbers only (version, uptime, responses by class of status, health and the `registry` check) and no label carries a path, an address, a mission or a node.
