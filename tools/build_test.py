#!/usr/bin/env python3
# =============================================================================
# HYDRA-UMC-ORCHESTRATOR - tools/build_test.py
# Copyright (C) 2026 JuanenRac (Electro Hobby 3D) <electrohobby3d@gmail.com>
# GPL-3.0 - see LICENSE
# =============================================================================
"""Run a non-versioning build check for this repository.

This command never invokes version bump scripts and never updates CHANGELOG.md.
It may create normal compiler outputs such as build/, target/ or APK artifacts.
"""
from __future__ import annotations

import json
import os
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
EXCLUDED_PARTS = {".git", ".venv", "node_modules", "build", "dist", "target", "__pycache__"}


def fail(message: str) -> None:
    print(f"BUILD_TEST=FAIL {message}", file=sys.stderr)
    raise SystemExit(1)


def run(*command: str, cwd: Path = ROOT, env: dict[str, str] | None = None) -> None:
    print("+", " ".join(command))
    try:
        subprocess.run(command, cwd=cwd, env=env, check=True)
    except FileNotFoundError:
        fail(f"required command not found: {command[0]}")
    except subprocess.CalledProcessError as exc:
        fail(f"command failed with exit code {exc.returncode}: {' '.join(command)}")


def compile_python_sources() -> None:
    files = [
        path for path in ROOT.rglob("*.py")
        if not any(part in EXCLUDED_PARTS for part in path.parts)
    ]
    for path in files:
        compile(path.read_text(encoding="utf-8", errors="replace"), str(path), "exec")
    print(f"PYTHON_COMPILE=PASS files={len(files)}")


def main() -> int:
    try:
        manifest = json.loads((ROOT / "hydra-umc.project.json").read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as exc:
        fail(f"cannot read project manifest: {exc}")

    stack = manifest.get("stack")
    if not isinstance(stack, str):
        fail("manifest stack must be a string")
    print(f"BUILD_TEST project={manifest.get('name', ROOT.name)} stack={stack}")

    if stack in {"python", "python-bare"}:
        compile_python_sources()
    elif stack == "node":
        npm = "npm.cmd" if os.name == "nt" else "npm"
        # Reuse an existing local dependency tree so a running editor/linter
        # cannot lock a native module while this non-destructive check runs.
        # CI starts without node_modules and therefore installs exactly from
        # the lockfile before exercising the same checks.
        if not (ROOT / "node_modules").is_dir():
            run(npm, "ci", "--ignore-scripts")
        else:
            print("NODE_DEPENDENCIES=REUSED existing node_modules")
        run(npm, "run", "typecheck", "--if-present")
        run(npm, "test", "--if-present")
        run(npm, "run", "lint", "--if-present")
    elif stack == "rust":
        run("cargo", "build", "--all-targets")
    elif stack == "go":
        module_root = ROOT / "src" if (ROOT / "src" / "go.mod").is_file() else ROOT
        run("go", "build", "./...", cwd=module_root)
    elif stack == "android":
        wrapper = "gradlew.bat" if os.name == "nt" else "./gradlew"
        wrapper_path = ROOT / wrapper
        if os.name != "nt":
            (ROOT / "gradlew").chmod((ROOT / "gradlew").stat().st_mode | 0o111)
        environment = dict(os.environ)
        environment["HYDRA_UMC_CI"] = "1"
        run(
            str(wrapper_path), "assembleDebug", "-PhydraUmcReadOnly=true",
            env=environment,
        )
    elif stack == "flutter":
        flutter = "flutter.bat" if os.name == "nt" else "flutter"
        run(flutter, "pub", "get")
        if ROOT.name == "HYDRA-UMC-IOS-CONTROL" and sys.platform == "darwin":
            run(flutter, "build", "ios", "--no-codesign")
        elif ROOT.name == "HYDRA-UMC-DSI" and os.name == "nt":
            run(flutter, "build", "windows")
        elif ROOT.name == "HYDRA-UMC-DSI" and sys.platform.startswith("linux"):
            run(flutter, "build", "linux")
        else:
            run(flutter, "analyze")
            print("FLUTTER_BUILD=SKIPPED no native desktop/mobile target is available on this host")
    elif stack == "firmware-c":
        script = ROOT / "build_firmware.sh"
        if not script.is_file():
            fail("firmware project is missing build_firmware.sh")
        environment = dict(os.environ)
        environment["HYDRA_UMC_CI"] = "1"
        if os.name == "nt":
            # WSL does not reliably inherit a Python child environment;
            # pass the read-only CI flag as an explicit WSL command argument.
            run(
                "wsl.exe", "--distribution", "Ubuntu-24.04", "--",
                "env", "HYDRA_UMC_CI=1", "bash", "build_firmware.sh",
            )
        else:
            run("bash", "build_firmware.sh", env=environment)
    else:
        fail(f"unsupported stack: {stack}")

    print("BUILD_TEST=PASS versioning=unchanged changelog=unchanged")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())