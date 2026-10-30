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
    python tools/hydra_project_tool.py doctor     # read-only: what is wrong here and what to do about it
    python tools/hydra_project_tool.py coverage   # measure the tests against tools/coverage_floor.json (a warning)

`validate`, `test` and `ci` never change the version: HYDRA_UMC_CI=1 is set
for them and the manifest version is compared before and after, so a build
that bumps by itself fails here instead of leaving a dirty tree.
`bump` is explicit. It refuses a dirty working tree (use `--allow-dirty`
to override), runs the repository's own bump script when it has one and then
synchronises the manifest; an Android project (`bump_version_code.py`) also
raises its versionCode. The build scripts never do any of this: building does
not change a version. `bump --dry-run` only prints the version it would produce.

`doctor` writes nothing. It checks the manifest (keys, name, version, valid enums), the universal README,
the At a glance section (needs HYDRA-UMC-DOCS next to the repository), the CHANGELOG note of the
version, the shared tools (needs HYDRA-UMC-SDK next to the repository) and the SDK pin, prints one line
per problem with the fix under it and ends with DOCTOR=PASS or DOCTOR=FAIL <n>. A check that needs a
sibling checkout that is not there prints SKIP.

This file is vendored into every repository from HYDRA-UMC-SDK
(`tools/canonical/hydra_project_tool.py`, kept identical by
`tools/sync_repo_tools.py`); edit it there.
"""
from __future__ import annotations

import argparse
import importlib.util
import json
import os
import re
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


# What each toolchain accepts in its own version field. The manifest, the CHANGELOG and the tags always keep the four
# components from 0.8.0; these files get a fixed translation of them (see bump_manifest_version.py): Cargo
# "a.b.c+d" (semver build metadata, the same in Cargo.lock) and Flutter "a.b.c+N" (N is the build number).
_NATIVE_FORMATS = {
    "Cargo.toml": (re.compile(r'(?m)^version\s*=\s*"([^"]*)"'), re.compile(r"^\d+\.\d+\.\d+(?:\+\d+)?$"), "a.b.c or a.b.c+d"),
    "pubspec.yaml": (re.compile(r"(?m)^version:\s*(\S+)"), re.compile(r"^\d+\.\d+\.\d+\+\d+$"), "a.b.c+N"),
}


def native_version_problem(root: Path = ROOT) -> str | None:
    """A message when the version in the native file is one its toolchain would reject, else None.

    Cargo and Flutter do not accept four dotted components; writing one breaks the build without warning, so the bump
    and `doctor` refuse it with a clear message instead.
    """
    try:
        native = json.loads((root / "hydra-umc.project.json").read_text(encoding="utf-8")).get("native_version") or {}
        name = Path(str(native.get("file", ""))).name
    except (OSError, json.JSONDecodeError):
        return None
    if name not in _NATIVE_FORMATS:
        return None
    finder, accepted, shape = _NATIVE_FORMATS[name]
    path = root / str(native["file"])
    match = finder.search(path.read_text(encoding="utf-8", errors="replace")) if path.is_file() else None
    if match is None:
        return f"{native['file']} has no version line"
    if not accepted.match(match.group(1)):
        return f"{native['file']} holds {match.group(1)!r}, which {name.split('.')[0]} cannot build; it must look like {shape}"
    return None


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
    if own is not None:
        steps = [[sys.executable, str(own)]]
        if wanted is not None:
            steps.append([sys.executable, str(root / "bump_manifest_version.py"), "--sync"])
    else:
        # no bump script of its own (a Flutter app too): the shared utility moves the native file and the manifest
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
    problem = native_version_problem(root)
    if problem:
        print(f"PROJECT_TOOL=FAIL {problem}; restore it with git and report this", file=sys.stderr)
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


REQUIRED_MANIFEST_KEYS = (
    "schema_version", "ecosystem", "name", "version", "role", "stack", "technologies",
    "deployment_target", "maturity", "family", "parent", "build", "notes", "native_version",
)
# The values of project-manifest.schema.json (a test of the SDK compares them with the schema).
MANIFEST_ENUMS = {
    "role": ("api", "ui", "cli", "firmware", "library", "service", "tool"),
    "deployment_target": ("cm5", "user-pc", "mobile", "wearable", "dev-server", "mcu"),
    "maturity": ("scaffolding", "functional", "established", "production"),
}
_VERSION = re.compile(r"^\d+\.\d+\.\d+(?:\.\d+)?$")
_CHANGELOG_HEADING = re.compile(r"(?im)^#{1,3}\s*\[?(\d+\.\d+\.\d+(?:\.\d+)?)(?:\]|\s|$)")
_SDK_TAG = re.compile(r"^v\d+\.\d+\.\d+(?:\.\d+)?$")
_SDK_REFERENCES = (
    re.compile(r"HYDRA-UMC-SDK\.git@([^#\s\"']+)"),
    re.compile(r"repository:\s*JuanenRac/HYDRA-UMC-SDK\b[^\n}]*?\bref:\s*([^\s,}]+)"),
    re.compile(r"repository:\s*JuanenRac/HYDRA-UMC-SDK[ \t]*\r?\n(?:[ \t]+[^\n]*\r?\n)*?[ \t]+ref:\s*([^\s#]+)"),
)
_SDK_FILES = ("pyproject.toml", "requirements.txt", "requirements-dev.txt", ".github/workflows/ci.yml")

# Each finding is (problem, what to do about it); a check that cannot run says so with Skipped.
Finding = tuple[str, str]


class Skipped(Exception):
    """A check that needs something this checkout does not have (a sibling repository)."""


def _load_module(path: Path, name: str):
    spec = importlib.util.spec_from_file_location(name, path)
    module = importlib.util.module_from_spec(spec)
    previous, sys.dont_write_bytecode = sys.dont_write_bytecode, True   # a read-only command leaves no __pycache__
    try:
        spec.loader.exec_module(module)
    finally:
        sys.dont_write_bytecode = previous
    return module


def _sibling(root: Path, name: str, variable: str, environment: dict[str, str]) -> Path | None:
    path = Path(environment[variable]) if environment.get(variable) else root.parent / name
    return path if path.is_dir() else None


def doctor_manifest(root: Path, environment: dict[str, str]) -> list[Finding]:
    try:
        manifest = json.loads((root / "hydra-umc.project.json").read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as exc:
        return [(f"hydra-umc.project.json cannot be read ({exc})", "restore the manifest from git")]
    findings: list[Finding] = []
    missing = [key for key in REQUIRED_MANIFEST_KEYS if key not in manifest]
    if missing:
        findings.append(("the manifest lacks " + ", ".join(missing), "add the keys (see docs/PROJECT_MANIFEST.md in the SDK)"))
    if manifest.get("ecosystem") != "HYDRA-UMC":
        findings.append((f"ecosystem is {manifest.get('ecosystem')!r}", 'set "ecosystem": "HYDRA-UMC"'))
    if manifest.get("name") != root.name:
        findings.append((f"name {manifest.get('name')!r} differs from the directory {root.name!r}", "make the manifest name the repository name"))
    if not isinstance(manifest.get("version"), str) or not _VERSION.match(manifest["version"]):
        findings.append((f"version {manifest.get('version')!r} is not MAJOR.MINOR.PATCH", "write the version as numbers separated by dots"))
    problem = native_version_problem(root)
    if problem:
        findings.append((problem, "restore the native version file from git; the toolchain only accepts its own form"))
    for key, allowed in MANIFEST_ENUMS.items():
        if key in manifest and manifest[key] not in allowed:
            findings.append((f"{key} {manifest[key]!r} is not one of {', '.join(allowed)}", f"choose a valid {key}"))
    return findings


def doctor_readme(root: Path, environment: dict[str, str]) -> list[Finding]:
    checker = root / "tools" / "universal_readme_check.py"
    if not checker.is_file():
        return [("tools/universal_readme_check.py is missing", "run tools/sync_repo_tools.py from the SDK")]
    problems = _load_module(checker, "_hydra_readme_check").check_repository(root)
    findings = [(problem, "fix it in the README source and regenerate the eight files") for problem in problems[:6]]
    if len(problems) > 6:
        findings.append((f"... and {len(problems) - 6} more README problems", "python tools/universal_readme_check.py --all lists them"))
    return findings


def doctor_glance(root: Path, environment: dict[str, str]) -> list[Finding]:
    docs = _sibling(root, "HYDRA-UMC-DOCS", "HYDRA_UMC_DOCS_DIR", environment)
    if docs is None or not (docs / "tools" / "make_glance.py").is_file():
        raise Skipped("no HYDRA-UMC-DOCS checkout next to this repository (set HYDRA_UMC_DOCS_DIR)")
    try:
        listed = json.loads((docs / "data" / "glance.json").read_text(encoding="utf-8")).get("repos", {})
    except (OSError, json.JSONDecodeError):
        return [("the glance data of HYDRA-UMC-DOCS cannot be read", "restore data/glance.json there")]
    if root.name not in listed:
        raise Skipped("this repository is not listed in the glance data")
    result = subprocess.run(
        [sys.executable, "-I", str(docs / "tools" / "make_glance.py"), "--check", "--repo", root.name, "--workspace", str(root.parent)],
        cwd=docs, text=True, encoding="utf-8", errors="replace", capture_output=True, check=False,
    )
    if result.returncode == 0:
        return []
    lines = [line for line in (result.stdout + result.stderr).splitlines() if line.strip()]
    return [("the At a glance section is stale: " + "; ".join(lines[:2])[:300],
             f"python make_glance.py --apply --repo {root.name} in HYDRA-UMC-DOCS, then commit the READMEs")]


def doctor_changelog(root: Path, environment: dict[str, str]) -> list[Finding]:
    try:
        version = declared_version(root)
        text = (root / "CHANGELOG.md").read_text(encoding="utf-8", errors="replace")
    except (OSError, ValueError, KeyError):
        return [("CHANGELOG.md or the manifest version cannot be read", "restore them from git")]
    headings = list(_CHANGELOG_HEADING.finditer(text))
    if not headings:
        return [("CHANGELOG.md has no version heading", "add a heading with the version and its notes")]
    if headings[0].group(1) != version:
        return [(f"the newest CHANGELOG heading is {headings[0].group(1)} but the manifest says {version}",
                 "promote the unreleased notes under a heading for the manifest version")]
    end = headings[1].start() if len(headings) > 1 else len(text)
    body = text[headings[0].end():end].split("\n", 1)[-1]
    if not any(line.strip() and not line.lstrip().startswith("#") for line in body.splitlines()):
        return [(f"the CHANGELOG heading of {version} has no note under it", "write what changed in this version")]
    return []


def doctor_shared_tools(root: Path, environment: dict[str, str]) -> list[Finding]:
    sdk = root if root.name == "HYDRA-UMC-SDK" else _sibling(root, "HYDRA-UMC-SDK", "HYDRA_UMC_SDK_DIR", environment)
    if sdk is None or not (sdk / "tools" / "sync_repo_tools.py").is_file():
        raise Skipped("no HYDRA-UMC-SDK checkout next to this repository (set HYDRA_UMC_SDK_DIR)")
    module = _load_module(sdk / "tools" / "sync_repo_tools.py", "_hydra_sync_tools")
    if not hasattr(module, "drift_of_repository"):
        raise Skipped("the SDK checkout is older than the doctor command (update it)")
    return [(f"{path} differs from the shared copy", "python tools/sync_repo_tools.py in HYDRA-UMC-SDK, then commit here")
            for path in module.drift_of_repository(root)]


def sdk_references(root: Path) -> list[tuple[str, str]]:
    """(file, ref) for every reference to the SDK in the dependency and workflow files."""
    found: list[tuple[str, str]] = []
    for relative in _SDK_FILES:
        path = root / relative
        if path.is_file():
            text = path.read_text(encoding="utf-8", errors="replace")
            for pattern in _SDK_REFERENCES:
                found += [(relative, match.group(1)) for match in pattern.finditer(text)]
    return found


def _newest_sdk_tag(sdk: Path) -> str | None:
    result = subprocess.run(["git", "tag", "--list", "v*"], cwd=sdk, capture_output=True, text=True)
    tags = [t for t in result.stdout.split() if _SDK_TAG.match(t)]
    return max(tags, key=lambda tag: tuple(int(part) for part in tag[1:].split("."))) if tags else None


def doctor_sdk_pin(root: Path, environment: dict[str, str]) -> list[Finding]:
    references = sdk_references(root)
    if root.name == "HYDRA-UMC-SDK" or not references:
        return []
    findings: list[Finding] = []
    for relative, ref in sorted(set(references)):
        if not _SDK_TAG.match(ref):
            findings.append((f"{relative} names the SDK by {ref!r}, not by a release tag", "python tools/pin_sdk.py in HYDRA-UMC-SDK"))
    sdk = _sibling(root, "HYDRA-UMC-SDK", "HYDRA_UMC_SDK_DIR", environment)
    newest = _newest_sdk_tag(sdk) if sdk is not None else None
    if newest is None:
        if not findings:
            raise Skipped("no HYDRA-UMC-SDK checkout to compare the pin with")
        return findings
    for relative, ref in sorted(set(references)):
        if _SDK_TAG.match(ref) and ref != newest:
            findings.append((f"{relative} pins the SDK at {ref}; the newest release is {newest}", "python tools/pin_sdk.py in HYDRA-UMC-SDK"))
    return findings


DOCTOR_CHECKS = (
    ("manifest", doctor_manifest),
    ("readme", doctor_readme),
    ("glance", doctor_glance),
    ("changelog", doctor_changelog),
    ("shared tools", doctor_shared_tools),
    ("sdk pin", doctor_sdk_pin),
)


def doctor(root: Path = ROOT, environment: dict[str, str] | None = None) -> int:
    """Read-only: check the repository against the shared rules and say what to do about each problem."""
    environment = dict(os.environ) if environment is None else environment
    problems = 0
    for label, check in DOCTOR_CHECKS:
        try:
            findings = check(root, environment)
        except Skipped as why:
            print(f"SKIP  {label}: {why}")
            continue
        if not findings:
            print(f"OK    {label}")
            continue
        for problem, fix in findings:
            print(f"FIX   {label}: {problem}\n        -> {fix}")
        problems += len(findings)
    print("DOCTOR=" + (f"FAIL {problems}" if problems else "PASS"))
    return 1 if problems else 0


COVERAGE_FLOOR_FILE = Path("tools") / "coverage_floor.json"


def parse_go_profile(text: str) -> float | None:
    """Percent of statements covered, from a `go test -coverprofile` file (None for an empty profile)."""
    total = covered = 0
    for line in text.splitlines()[1:]:
        parts = line.rsplit(" ", 2)
        if len(parts) == 3 and parts[1].isdigit() and parts[2].isdigit():
            total += int(parts[1])
            covered += int(parts[1]) if int(parts[2]) > 0 else 0
    return None if total == 0 else 100.0 * covered / total


def parse_node_total(text: str) -> float | None:
    """The line coverage of the `all files` row of `node --test --experimental-test-coverage`."""
    match = re.search(r"all files\s*\|\s*([\d.]+)", text)
    return float(match.group(1)) if match else None


def _capture(command: list[str], cwd: Path, env: dict[str, str] | None = None) -> subprocess.CompletedProcess[str]:
    return subprocess.run(command, cwd=cwd, env=env, capture_output=True, text=True, encoding="utf-8", errors="replace")


def measure_coverage(entry: dict, root: Path) -> float | None:
    """Run the tests of one stack under its coverage tool; None when the tool is not installed or gives no figure."""
    import tempfile
    stack = entry.get("stack")
    workdir = root / entry.get("workdir", ".")
    with tempfile.TemporaryDirectory() as tmp:
        if stack == "python":
            env = dict(os.environ, COVERAGE_FILE=str(Path(tmp) / ".coverage"), HYDRA_UMC_CI="1")
            if entry.get("pythonpath"):
                env["PYTHONPATH"] = os.pathsep.join([str(root / entry["pythonpath"]), env.get("PYTHONPATH", "")]).rstrip(os.pathsep)
            run = _capture([sys.executable, "-m", "coverage", "run", "--branch", f"--source={entry['source']}", "-m", "pytest",
                            entry.get("tests", "tests"), "-q", "-p", "no:cacheprovider"], workdir, env)
            if run.returncode != 0 and "No module named coverage" in run.stderr:
                return None
            report = _capture([sys.executable, "-m", "coverage", "json", "-o", "-"], workdir, env)
            try:
                return float(json.loads(report.stdout)["totals"]["percent_covered"])
            except (ValueError, KeyError, TypeError):
                return None
        if stack == "go":
            profile = Path(tmp) / "cover.out"
            _capture(["go", "test", "-count=1", f"-coverprofile={profile}", "./..."], workdir)
            return parse_go_profile(profile.read_text(encoding="utf-8")) if profile.is_file() else None
        if stack == "node":
            run = _capture(["node", "--test", "--experimental-test-coverage", *entry.get("tests", ["tests/*.test.mjs"])], workdir)
            return parse_node_total(run.stdout + run.stderr)
    return None


def coverage(root: Path = ROOT, strict: bool = False, measure=measure_coverage) -> int:
    """Measure the coverage of the stacks listed in tools/coverage_floor.json and compare it with each floor.

    The file is a list of {"stack": "python"|"go"|"node", "floor": N, ...} entries (the keys each stack reads are in
    docs/REPOSITORY_TOOLING.md). Below the floor it is a WARNING (exit 0); with --strict it fails. Nothing here runs in CI yet.
    """
    path = root / COVERAGE_FLOOR_FILE
    if not path.is_file():
        print(f"COVERAGE=SKIP no {COVERAGE_FLOOR_FILE.as_posix()} in this repository")
        return 0
    try:
        entries = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as exc:
        print(f"COVERAGE=FAIL {COVERAGE_FLOOR_FILE.as_posix()} cannot be read ({exc})", file=sys.stderr)
        return 1
    below = 0
    for entry in entries:
        label = f"{entry.get('stack')}:{entry.get('workdir', '.')}"
        value = measure(entry, root)
        if value is None:
            print(f"SKIP  {label}: no figure (coverage tool not installed or the tests did not run)")
        elif value + 1e-9 < float(entry["floor"]):
            below += 1
            print(f"WARN  {label}: {value:.1f}% is below the floor of {entry['floor']}%")
        else:
            print(f"OK    {label}: {value:.1f}% (floor {entry['floor']}%)")
    print("COVERAGE=" + ("PASS" if not below else f"WARN {below} below the floor"))
    return 1 if below and strict else 0


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    parser.add_argument("command", choices=("validate", "test", "ci", "version", "status", "bump", "doctor", "coverage"))
    parser.add_argument("--allow-dirty", action="store_true", help="bump: do not require a clean tree")
    parser.add_argument("--dry-run", action="store_true", help="bump: print the next version and stop")
    parser.add_argument("--strict", action="store_true", help="coverage: fail below the floor instead of warning")
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
    if args.command == "doctor":
        return doctor()
    if args.command == "coverage":
        return coverage(strict=args.strict)
    return bump(allow_dirty=args.allow_dirty, dry_run=args.dry_run)


if __name__ == "__main__":
    raise SystemExit(main())
