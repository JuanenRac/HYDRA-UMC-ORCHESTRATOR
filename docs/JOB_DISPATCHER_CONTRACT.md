<!-- =============================================================================
HYDRA-UMC-ORCHESTRATOR - Contract with the job-dispatcher
Copyright (C) JuanenRac (Electro Hobby 3D) <electrohobby3d@gmail.com>
GPL-3.0-or-later - see LICENSE
============================================================================= -->

# Contract between the orchestrator and the job-dispatcher

Two processes touch the same work: HYDRA-UMC-JOB-DISPATCHER (a queue of jobs and a registry of robots) and this orchestrator (missions). Without a rule about who
decides what, both can decide, and a robot gets two answers. The rule:

| | Owner | What it means |
|---|---|---|
| **The assignment** (which robot gets which job, and when) | the job-dispatcher | Only a dispatch pass (`POST /dispatch`) assigns. The orchestrator never chooses a robot when a dispatcher is configured. |
| **The mission** (its life cycle, its closing, its recovery) | the orchestrator | Pending, Dispatched, InProgress, Completed, Failed, Cancelled, and the attempt counter, live here. The dispatcher knows a job, not a mission. |

Without `--job-dispatcher-url` the orchestrator is on its own and assigns by hand, as it always did; everything below applies when a dispatcher is configured.

## What each side promises

**The orchestrator promises the dispatcher**

1. A mission posted to it becomes a job at the dispatcher (`POST /jobs/submit`, with the mission's `priority`, `requiredTool`, `location` and `dependsOn`, and the mission id as the idempotency key,
   so posting it twice is one job).
2. It never assigns by hand: `POST /missions/:id/dispatch` is refused with `409` and `{"assignmentOwner": "job-dispatcher"}`; `POST /missions/:id/auto-dispatch` runs a dispatch pass and
   reconciles EVERY assignment the pass returned, not only the mission asked about.
3. Every real end of a mission is confirmed to the dispatcher: complete closes the job as `done` (naming the robot the dispatcher itself reports, and its `attempt`), fail closes it as `failed`,
   cancel withdraws a job nobody holds or closes the one a robot holds as failed. The intent is written to a durable outbox BEFORE the dispatcher is asked, a close that fails is retried (30 s, 1 min, 2 min... up to 30 min)
   and is never dropped; the local transition never waits for, nor is undone by, the dispatcher.
4. `POST /nodes/:node/recover` (NODE-HEALING's call) requeues the missions of that node (a new `attempt`, never reused) and asks the dispatcher to take those jobs off the robot and hold it
   (`POST /jobs/requeue`), so the robot that was declared lost is not offered work again until it checks in.
5. A late report from the lost attempt is fenced: `start`, `complete` and `fail` naming a stale `attempt` (or a node that no longer holds the mission) are `409` with a `fence` object and change nothing, here or at the dispatcher.

**The dispatcher promises the orchestrator**

1. A job submitted twice with the same key is one job.
2. It assigns only in a dispatch pass, to a robot that is available, has the required tool and is not silent longer than `-robot-timeout`; it answers each assignment with the job and the robot.
3. `GET /jobs/{id}` tells the truth about a job (`Status`, `AssignedRobot`, `Attempt`), so the orchestrator can close it against what the dispatcher holds.
4. It accepts the close of a job that is already over (the answer is a conflict the orchestrator treats as done), and refuses the close of a job that was reassigned since (the orchestrator reads the job first and names its attempt).
5. `POST /jobs/requeue` returns a job to the queue and keeps the robot unavailable until it sends a heartbeat.

## A dispatcher that is down

The orchestrator keeps serving. Missions are created locally (`jobDispatcher.submitted: false`), `auto-dispatch` answers an error naming the dispatcher, and every close waits in the outbox. `/stats` shows
`pendingRemoteCloses`, the age of the oldest one and, under `jobDispatcher`, when a call last worked and the last error. When the dispatcher is back, the outbox drains by itself.

## Checking it with both real processes

`python tools/verify_dispatcher_contract.py` builds the dispatcher (Go, from `HYDRA_UMC_JOB_DISPATCHER_ROOT` or the sibling checkout), starts it and a built orchestrator on free loopback ports, and drives
missions through both, checking what each side says at each step: the mission is a pending job; a manual dispatch is `409`; `auto-dispatch` makes the robot the dispatcher chose the node of the mission;
`start` and `complete` leave the job `done` and the robot free; `fail` leaves it `failed`; `recover` returns the job to `pending`, keeps the robot unavailable and makes a late `complete` of the lost attempt a
`409` that changes nothing. Without Go, the dispatcher checkout or the built orchestrator it prints `DISPATCHER_CONTRACT=SKIPPED` and exits 0.
