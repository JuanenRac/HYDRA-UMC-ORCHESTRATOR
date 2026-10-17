#!/usr/bin/env python3
# HYDRA-UMC-ORCHESTRATOR - tools/verify_compose.py
# Copyright (C) 2026 JuanenRac (Electro Hobby 3D) <electrohobby3d@gmail.com>
# GPL-3.0 - see LICENSE
"""Static checks of docker-compose.yml, runnable without Docker or PyYAML.

A compose file that builds but starts nothing useful, or starts an open API,
passes `docker compose config` just as well as a correct one, so the things
that went wrong before are checked here:

  * every service that is not behind a profile says what to run (`command:`),
    because a binary whose bare invocation prints its identity and exits is
    not a service;
  * no image is floating (`latest`, or a major-only tag such as `rust:1-slim`);
  * a published port is bound to the loopback interface of the host;
  * a token the stack cannot run safely without is `${VAR:?message}`, never
    `${VAR:-default}` and never a literal;
  * every host a node registry mounted into the stack probes is a service of
    the stack.

Exit status 0 and `COMPOSE_CHECK=PASS` when everything holds.
"""
from __future__ import annotations

import json
import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
COMPOSE = ROOT / "docker-compose.yml"


def service_blocks(text: str) -> dict[str, list[str]]:
    blocks: dict[str, list[str]] = {}
    in_services = False
    current: str | None = None
    for line in text.splitlines():
        if re.match(r"^\S", line):
            in_services = line.startswith("services:")
            current = None
            continue
        if not in_services:
            continue
        m = re.match(r"^  ([A-Za-z0-9_-]+):\s*$", line)
        if m:
            current = m.group(1)
            blocks[current] = []
        elif current is not None:
            blocks[current].append(line)
    return blocks


def main() -> int:
    text = COMPOSE.read_text(encoding="utf-8")
    problems: list[str] = []
    blocks = service_blocks(text)
    if not blocks:
        problems.append("no services found")

    for name, lines in blocks.items():
        body = "\n".join(lines)
        if not re.search(r"^    profiles:", body, re.M) and not re.search(r"^    command:", body, re.M):
            problems.append(f"{name}: no command: - a bare binary prints its identity and exits")
        in_ports = False
        for line in lines:
            if re.match(r"^    ports:", line):
                in_ports = True
                continue
            if in_ports:
                item = re.match(r"^      - \"?([^\"]+)\"?\s*$", line)
                if not item:
                    in_ports = False
                    continue
                if not item.group(1).startswith("127.0.0.1:"):
                    problems.append(f"{name}: port {item.group(1)} is not bound to 127.0.0.1")
        for m in re.finditer(r"^\s+([A-Z_]*(?:TOKEN|SECRET|PASSWORD)[A-Z_]*):\s*(.*)$", body, re.M):
            value = m.group(2).strip()
            if not value.startswith("${") or ":?" not in value:
                problems.append(f"{name}: {m.group(1)} must be required with ${{VAR:?message}}, got {value!r}")

    for image in re.findall(r"^\s*FROM\s+(\S+)", text, re.M):
        ref = image.split("@")[0]
        tag = ref.split(":", 1)[1] if ":" in ref else "latest"
        if tag == "latest" or re.fullmatch(r"\d+(-[A-Za-z].*)?", tag):
            problems.append(f"image {image} is floating - pin at least major.minor")

    for mount in re.findall(r"^\s+-\s+(\./[^:\s]+):[^\s]+", text, re.M):
        path = ROOT / mount
        if not path.exists():
            problems.append(f"mounted file {mount} does not exist")
        elif path.suffix == ".json":
            for node in json.loads(path.read_text(encoding="utf-8")):
                url = node.get("healthUrl", "")
                host = re.match(r"https?://([^:/]+)", url)
                if host and host.group(1) not in blocks:
                    problems.append(f"{mount}: {node.get('name')} probes host {host.group(1)}, which is not a service")

    if problems:
        for problem in problems:
            print(f"COMPOSE_CHECK problem: {problem}")
        print("COMPOSE_CHECK=FAIL")
        return 1
    print(f"COMPOSE_CHECK=PASS services={len(blocks)}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
