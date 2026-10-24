#!/usr/bin/env python3
# =============================================================================
# HYDRA-UMC-ORCHESTRATOR - tools/verify_dispatcher_contract.py
# Copyright (C) 2026 JuanenRac (Electro Hobby 3D) <electrohobby3d@gmail.com>
# GPL-3.0-or-later - see LICENSE
# =============================================================================
"""Run the REAL job-dispatcher and the REAL orchestrator against each other and check the contract between them.

The contract is written in docs/JOB_DISPATCHER_CONTRACT.md: the dispatcher owns the assignment of work to robots, the orchestrator
owns the mission (its life cycle, its closing and its recovery). This script starts both processes, drives a mission through them and
checks, at each step, what each one says:

1. a mission posted to the orchestrator is a pending job at the dispatcher;
2. the orchestrator refuses to assign a mission by hand (409) and lets the dispatcher choose it (`auto-dispatch`);
3. start and complete on the orchestrator close the job at the dispatcher as done and free the robot;
4. fail closes it as failed;
5. recovering a node takes its jobs back (requeued, the robot held) and a late report of the lost attempt is fenced (409).

It needs Go and a checkout of HYDRA-UMC-JOB-DISPATCHER (next to this repository, or HYDRA_UMC_JOB_DISPATCHER_ROOT) and a built
orchestrator (`cargo build`); without them it says SKIPPED and exits 0, so that a build elsewhere is not blocked. It uses free
loopback ports and a temporary directory, and stops only the two processes it started.
"""
from __future__ import annotations

import json
import os
import shutil
import socket
import subprocess
import sys
import tempfile
import time
import urllib.error
import urllib.request
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
EXE = ".exe" if os.name == "nt" else ""


def skip(reason: str) -> int:
    print(f"DISPATCHER_CONTRACT=SKIPPED {reason}")
    return 0


def fail(message: str) -> int:
    print(f"DISPATCHER_CONTRACT=FAIL {message}", file=sys.stderr)
    return 1


def free_port() -> int:
    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        return sock.getsockname()[1]


def call(method: str, url: str, body: object | None = None) -> tuple[int, object]:
    data = None if body is None else json.dumps(body).encode()
    request = urllib.request.Request(url, data=data, method=method)
    try:
        with urllib.request.urlopen(request, timeout=10) as response:
            raw, status = response.read(), response.status
    except urllib.error.HTTPError as error:
        raw, status = error.read(), error.code
    try:
        return status, json.loads(raw) if raw else None
    except json.JSONDecodeError:
        return status, raw.decode("utf-8", "replace")


def wait_for(url: str) -> None:
    for _ in range(100):
        try:
            if call("GET", url)[0] == 200:
                return
        except (urllib.error.URLError, ConnectionError, OSError):
            pass
        time.sleep(0.1)
    raise RuntimeError(f"{url} did not answer")


class Check:
    def __init__(self) -> None:
        self.count = 0

    def equal(self, label: str, actual: object, expected: object) -> None:
        self.count += 1
        if actual != expected:
            raise AssertionError(f"{label}: expected {expected!r}, got {actual!r}")


def find_orchestrator() -> Path | None:
    for profile in ("debug", "release"):
        candidate = ROOT / "target" / profile / f"hydra-umc-orchestrator{EXE}"
        if candidate.is_file():
            return candidate
    return None


def main() -> int:
    jd_root = Path(os.environ.get("HYDRA_UMC_JOB_DISPATCHER_ROOT", ROOT.parent / "HYDRA-UMC-JOB-DISPATCHER"))
    if not (jd_root / "main.go").is_file():
        return skip(f"no checkout of HYDRA-UMC-JOB-DISPATCHER at {jd_root}")
    if shutil.which("go") is None:
        return skip("Go is not installed")
    orchestrator = find_orchestrator()
    if orchestrator is None:
        return skip("the orchestrator is not built (cargo build)")

    work = Path(tempfile.mkdtemp(prefix="hydra-orchestrator-contract-"))
    processes: list[subprocess.Popen] = []
    try:
        dispatcher_exe = work / f"job-dispatcher{EXE}"
        build = subprocess.run(["go", "build", "-o", str(dispatcher_exe), "."], cwd=jd_root, capture_output=True, text=True)
        if build.returncode != 0:
            return fail(f"the dispatcher does not build: {build.stderr.strip()[:300]}")
        jd_port, orch_port = free_port(), free_port()
        jd, orch = f"http://127.0.0.1:{jd_port}", f"http://127.0.0.1:{orch_port}"
        processes.append(subprocess.Popen([str(dispatcher_exe), "-addr", f"127.0.0.1:{jd_port}"], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL))
        wait_for(f"{jd}/health")
        data_dir = work / "data"
        data_dir.mkdir()
        processes.append(
            subprocess.Popen(
                [str(orchestrator), "serve", "--port", str(orch_port), "--job-dispatcher-url", jd, "--data-dir", str(data_dir)],
                stdout=subprocess.DEVNULL,
                stderr=subprocess.DEVNULL,
            )
        )
        wait_for(f"{orch}/health")
        check = Check()

        # a robot checks in with the dispatcher
        status, robots = call("POST", f"{jd}/robots", {"id": "arm-1", "location": "cell-1", "tool": "Laser", "available": True})
        check.equal("robot registered", status, 200)

        # 1. a mission is a pending job
        status, body = call("POST", f"{orch}/missions", {"id": "m1", "requiredTool": "Laser"})
        check.equal("mission created", status in (200, 201), True)
        check.equal("mission submitted to the dispatcher", body["jobDispatcher"]["submitted"], True)
        status, job = call("GET", f"{jd}/jobs/m1")
        check.equal("job status after the mission", (status, job["Status"], job["RequiredTool"]), (200, "pending", "Laser"))

        # 2. the dispatcher owns the assignment
        status, body = call("POST", f"{orch}/missions/m1/dispatch", {"node": "arm-1"})
        check.equal("manual dispatch refused", status, 409)
        check.equal("the refusal names the owner", body["assignmentOwner"], "job-dispatcher")
        status, body = call("POST", f"{orch}/missions/m1/auto-dispatch")
        check.equal("auto-dispatch", (status, body["assigned"]), (200, True))
        check.equal("mission dispatched to the robot the dispatcher chose", body["mission"]["state"], {"Dispatched": {"node": "arm-1"}})
        check.equal("attempt goes up on a dispatch", body["mission"]["attempt"], 1)
        status, job = call("GET", f"{jd}/jobs/m1")
        check.equal("job assigned to the same robot", (job["Status"], job["AssignedRobot"]), ("assigned", "arm-1"))

        # 3. start and complete close the job as done and free the robot
        status, body = call("POST", f"{orch}/missions/m1/start", {"attempt": 1, "node": "arm-1"})
        check.equal("start", (status, body["state"]), (200, {"InProgress": {"node": "arm-1"}}))
        status, body = call("POST", f"{orch}/missions/m1/complete", {"attempt": 1, "node": "arm-1"})
        check.equal("complete", (status, body["state"]), (200, {"Completed": {"node": "arm-1"}}))
        status, job = call("GET", f"{jd}/jobs/m1")
        check.equal("job done at the dispatcher", job["Status"], "done")
        status, robots = call("GET", f"{jd}/robots")
        check.equal("robot free again", [r["Available"] for r in robots], [True])
        check.equal("the close was confirmed, nothing waits in the outbox", call("GET", f"{orch}/stats")[1]["pendingRemoteCloses"], 0)

        # 4. fail closes the job as failed
        call("POST", f"{orch}/missions", {"id": "m2", "requiredTool": "Laser"})
        call("POST", f"{orch}/missions/m2/auto-dispatch")
        status, body = call("POST", f"{orch}/missions/m2/fail", {"reason": "jam", "attempt": 1, "node": "arm-1"})
        check.equal("fail", (status, body["state"]), (200, {"Failed": {"reason": "jam"}}))
        check.equal("job failed at the dispatcher", call("GET", f"{jd}/jobs/m2")[1]["Status"], "failed")

        # 5. recovering a node takes its work back, and a late report of the lost attempt is fenced
        call("POST", f"{orch}/missions", {"id": "m3", "requiredTool": "Laser"})
        call("POST", f"{orch}/missions/m3/auto-dispatch")
        status, body = call("POST", f"{orch}/nodes/arm-1/recover")
        check.equal("recover", (status, body["requeuedMissions"], body["jobDispatcherNotNotified"]), (200, ["m3"], []))
        status, mission = call("GET", f"{orch}/missions/m3")
        check.equal("mission requeued with a new attempt", (mission["state"], mission["attempt"]), ("Pending", 2))
        status, job = call("GET", f"{jd}/jobs/m3")
        check.equal("job back in the queue", (job["Status"], job["AssignedRobot"]), ("pending", ""))
        check.equal("the robot is held, not offered work", [r["Available"] for r in call("GET", f"{jd}/robots")[1]], [False])
        status, body = call("POST", f"{orch}/missions/m3/complete", {"attempt": 1, "node": "arm-1"})
        check.equal("late report fenced", (status, body["fence"]["kind"], body["fence"]["current"]), (409, "stale_attempt", 2))
        check.equal("the fenced report changed nothing at the dispatcher", call("GET", f"{jd}/jobs/m3")[1]["Status"], "pending")

        print(f"DISPATCHER_CONTRACT=PASS checks={check.count} (real dispatcher and real orchestrator)")
        return 0
    except (AssertionError, RuntimeError, KeyError, TypeError) as error:
        return fail(f"{type(error).__name__}: {error}")
    finally:
        for process in processes:
            if process.poll() is None:
                process.terminate()
                try:
                    process.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    process.kill()
        shutil.rmtree(work, ignore_errors=True)


if __name__ == "__main__":
    raise SystemExit(main())
