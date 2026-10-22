#!/usr/bin/env python3
# =============================================================================
# HYDRA-UMC - Project tool: validate, test, version and bump, one way everywhere
# Copyright (C) 2026 JuanenRac (Electro Hobby 3D) <electrohobby3d@gmail.com>
# GPL-3.0-or-later - see LICENSE
# =============================================================================
"""The same commands in every repository, locally and in CI.

    python tools/hydra_project_tool.py validate   # the CI baseline (tools/ci_validate.py)
    python tools/hydra_project_tool.py test       # the build and tests (tools/build_test.py)
    python tools/hydra_project_tool.py ci         # validate, then test: what CI runs
    python tools/hydra_project_tool.py version    # the declared version and the next one
    python tools/hydra_project_tool.py status     # clean tree? version ahead of the last tag?
    python tools/hydra_project_tool.py bump       # the ONLY command that changes the version

`validate`, `test` and `ci` never change the version: HYDRA_UMC_CI=1 is set
for them and the manifest version is compared before and after, so a build
that bumps by itself fails here instead of leaving a dirty tree.
`bump` is explicit. It refuses a dirty working tree (use `--allow-dirty`
to override), runs the repository's own bump script when it has one and then
synchronises the manifest; an Android project (`bump_version_code.py`) also
raises its versionCode. The build scripts never do any of this: building does
not change a version. `bump --dry-run` only prints the version it would produce.

This file is vendored into every repository from HYDRA-UMC-SDK
(`tools/canonical/hydra_project_tool.py`, kept identical by
`tools/sync_repo_tools.py`); edit it there.
"""
from __future__ import annotations

import argparse
import importlib.util
import json
import os
import shutil
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
MANIFEST = ROOT / "hydra-umc.project.json"


def declared_version(root: Path = ROOT) -> str:
    return json.loads((root / "hydra-umc.project.json").read_text(encoding="utf-8"))["version"]


def _load_bump_module(root: Path):
    path = root / "bump_manifest_version.py"
    if not path.is_file():
        return None
    spec = importlib.util.spec_from_file_location("_hydra_bump", path)
    module = importlib.util.module_from_spec(spec)
    previous, sys.dont_write_bytecode = sys.dont_write_bytecode, True   # leave no __pycache__ in the tree
    try:
        spec.loader.exec_module(module)
    finally:
        sys.dont_write_bytecode = previous
    return module


def next_version(root: Path = ROOT) -> str | None:
    """The odometer successor, or None for a repository that has no shared bump utility."""
    module = _load_bump_module(root)
    return None if module is None else module.next_version(declared_version(root))


def _run(command: list[str], root: Path, env: dict[str, str] | None = None) -> int:
    print("+", " ".join(command), flush=True)
    return subprocess.run(command, cwd=root, env=env).returncode


def _git(root: Path, *args: str) -> subprocess.CompletedProcess[str]:
    return subprocess.run(["git", *args], cwd=root, capture_output=True, text=True)


def non_mutating(script: str, root: Path = ROOT) -> int:
    """Run `tools/<script>` with HYDRA_UMC_CI=1 and fail when the version moved."""
    path = root / "tools" / script
    if not path.is_file():
        print(f"PROJECT_TOOL=FAIL tools/{script} is missing", file=sys.stderr)
        return 1
    before = declared_version(root)
    env = dict(os.environ, HYDRA_UMC_CI="1")
    code = _run([sys.executable, str(path)], root, env)
    after = declared_version(root)
    if after != before:
        print(f"PROJECT_TOOL=FAIL {script} changed the version {before} -> {after}; a check must not bump", file=sys.stderr)
        return 1
    return code


def dirty_files(root: Path = ROOT) -> list[str]:
    result = _git(root, "status", "--porcelain")
    if result.returncode != 0:
        raise SystemExit("not a git checkout: cannot tell whether the tree is clean")
    return [line for line in result.stdout.splitlines() if line.strip()]


def last_tag_version(root: Path = ROOT) -> str | None:
    result = _git(root, "tag", "--list", "v*", "--sort=-v:refname")
    tags = [t for t in result.stdout.split() if t[1:].replace(".", "").isdigit()]
    return tags[0][1:] if tags else None


def _version_key(value: str) -> tuple[int, ...]:
    return tuple(int(part) for part in value.split("."))


def bump(root: Path = ROOT, allow_dirty: bool = False, dry_run: bool = False) -> int:
    before = declared_version(root)
    wanted = next_version(root)
    if dry_run:
        print(f"{before} -> {wanted or 'decided by the repository own bump script'} (dry run, nothing written)")
        return 0
    dirty = dirty_files(root)
    if dirty and not allow_dirty:
        print("PROJECT_TOOL=FAIL the working tree is not clean; commit or stash first (or --allow-dirty):", file=sys.stderr)
        for line in dirty[:10]:
            print("  " + line, file=sys.stderr)
        return 1
    env = {key: value for key, value in os.environ.items() if key != "HYDRA_UMC_CI"}
    own = next((p for p in (root / "bump_version.py", root / "tools" / "bump_version.py") if p.is_file()), None)
    dart_bump = root / "tool" / "bump_version.dart"
    if own is not None:
        steps = [[sys.executable, str(own)]]
        if wanted is not None:
            steps.append([sys.executable, str(root / "bump_manifest_version.py"), "--sync"])
    elif dart_bump.is_file():
        # a Flutter app: its own Dart script moves pubspec.yaml (version and build number), then the manifest follows
        steps = [[shutil.which("dart") or "dart", "run", "tool/bump_version.dart"]]
        if wanted is not None:
            steps.append([sys.executable, str(root / "bump_manifest_version.py"), "--sync"])
    else:
        if wanted is None:
            print("PROJECT_TOOL=FAIL this repository has neither bump_version.py nor bump_manifest_version.py", file=sys.stderr)
            return 1
        steps = [[sys.executable, str(root / "bump_manifest_version.py")]]
    if (root / "bump_version_code.py").is_file():
        steps.append([sys.executable, str(root / "bump_version_code.py")])
    for step in steps:
        if _run(step, root, env) != 0:
            print("PROJECT_TOOL=FAIL the bump step failed; check git status before retrying", file=sys.stderr)
            return 1
    after = declared_version(root)
    if wanted is not None and after != wanted:
        print(f"PROJECT_TOOL=FAIL expected {wanted} after the bump but the manifest says {after}", file=sys.stderr)
        return 1
    print(f"PROJECT_TOOL=BUMPED {before} -> {after}")
    return 0


def status(root: Path = ROOT) -> int:
    version = declared_version(root)
    dirty = dirty_files(root)
    tag = last_tag_version(root)
    print(f"version {version}; last tag {tag or 'none'}; working tree {'dirty (' + str(len(dirty)) + ' files)' if dirty else 'clean'}")
    problems = []
    if tag and _version_key(version) < _version_key(tag):
        problems.append(f"the manifest version {version} is older than the last tag {tag}")
    if dirty:
        problems.append("the working tree is not clean")
    print("PROJECT_STATUS=" + ("FAIL " + "; ".join(problems) if problems else "PASS"))
    return 1 if problems else 0


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    parser.add_argument("command", choices=("validate", "test", "ci", "version", "status", "bump"))
    parser.add_argument("--allow-dirty", action="store_true", help="bump: do not require a clean tree")
    parser.add_argument("--dry-run", action="store_true", help="bump: print the next version and stop")
    args = parser.parse_args(argv)
    if args.command == "validate":
        return non_mutating("ci_validate.py")
    if args.command == "test":
        return non_mutating("build_test.py")
    if args.command == "ci":
        return non_mutating("ci_validate.py") or non_mutating("build_test.py")
    if args.command == "version":
        print(f"{declared_version()} (next {next_version() or 'decided by the repository own bump script'})")
        return 0
    if args.command == "status":
        return status()
    return bump(allow_dirty=args.allow_dirty, dry_run=args.dry_run)


if __name__ == "__main__":
    raise SystemExit(main())
