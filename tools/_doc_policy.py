# =============================================================================
# HYDRA-UMC-ORCHESTRATOR - tools/_doc_policy.py
# Copyright (C) 2026 JuanenRac (Electro Hobby 3D) <electrohobby3d@gmail.com>
# GPL-3.0-or-later - see LICENSE
# =============================================================================
# VENDORED - do not hand-edit. This is a byte-for-byte copy of
# HYDRA-UMC-SDK's own canonical
# clients/python/src/hydra_umc_sdk/doc_policy.py, kept in sync by
# that repo's own tools/sync_doc_policy.py. Edit the rule there, then
# re-run that script to update every repo that vendors it, this one
# included.

from __future__ import annotations

import json
import re
import subprocess
import warnings
from pathlib import Path

# Split across string concatenation so this module's own source is never
# itself flagged by the very check it defines.
PRIVATE_MARKERS: tuple[str, ...] = ("SON" + "NET", "BIB" + "LIA")

# Prose that names this ecosystem's private planning or audit material,
# beyond the bare marker names above.
PRIVATE_PHRASES: tuple[str, ...] = (
    "BIB" + "LIA HYDRA" + "-UMC",
    "private development" + " plan",
    "plan de desarrollo" + " privado",
    "internal work" + " log",
    "registro de trabajo" + " interno",
)

# Internal audit/tracking code shapes that must never appear in a public
# file (source comments, tests, docs, manifests). Only the shapes that are
# unambiguous are matched - a bare "C08" or "I42" could be a part number, so
# those stay a review matter. Written as character classes so this module's
# own source never contains a literal code and never trips its own check.
TRACKING_CODE_REGEX = (
    r"(V07-[0-9]{3}|REV-[0-9]{3}|PROM-[A-Z]+-[A-Z]?[0-9]+|(DOC|CODE)-BUG-[0-9]+|[[][[](project|feedback|reference|user)_[a-z0-9_]+[]][]])"
)


# Directories that hold no versioned text: a checkout without Git (a source archive, a copy) has no `.gitignore`
# to say so, so the fallback below skips them by name.
_UNVERSIONED_DIRECTORIES = frozenset({
    ".git", ".venv", "venv", "node_modules", "build", "dist", "target", "__pycache__", ".gradle", ".idea",
    ".pytest_cache", ".mypy_cache", ".ruff_cache", ".dart_tool", "out",
})


def _is_git_work_tree(root: Path) -> bool:
    """True when `root` is inside a Git working tree and Git can be run."""
    try:
        result = subprocess.run(
            ("git", "rev-parse", "--is-inside-work-tree"),
            cwd=root,
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.DEVNULL,
            check=False,
        )
    except OSError:
        return False
    return result.returncode == 0 and result.stdout.strip() == "true"


def _text_files(root: Path):
    """The text files under `root` (what `git grep -I` would search) when there is no Git to ask: every file outside
    the unversioned directories whose first block holds no NUL byte."""
    for path in sorted(root.rglob("*")):
        relative = path.relative_to(root)
        if any(part in _UNVERSIONED_DIRECTORIES for part in relative.parts[:-1]) or not path.is_file():
            continue
        try:
            with open(path, "rb") as handle:
                head = handle.read(8192)
        except OSError:
            continue
        if b"\0" in head:
            continue
        yield path


def _boundary_without_git(root: Path) -> str | None:
    """The same three rules as `check_public_private_boundary`, by reading the files (no Git in the checkout)."""
    with warnings.catch_warnings():
        warnings.simplefilter("ignore")  # the tracking-code expression contains a bracket pair that looks nested
        tracking = re.compile(TRACKING_CODE_REGEX)
    phrases = tuple(phrase.lower() for phrase in PRIVATE_PHRASES)
    found_marker = found_phrase = found_code = False
    for path in _text_files(root):
        try:
            text = path.read_text(encoding="utf-8", errors="replace")
        except OSError:
            return "could not check public/private documentation boundary"
        lowered = text.lower()
        found_marker = found_marker or any(marker in text for marker in PRIVATE_MARKERS)
        found_phrase = found_phrase or any(phrase in lowered for phrase in phrases)
        found_code = found_code or tracking.search(text) is not None
        if found_marker:
            return "public files must not reference private documentation"
    if found_phrase:
        return "public files must not reference private planning or audit documents"
    if found_code:
        return "public files must not carry internal tracking codes"
    return None


def check_public_private_boundary(root: Path) -> str | None:
    """Runs this ecosystem's real, two-part public/private documentation
    boundary check against `root`'s own git-tracked files: no PRIVATE_MARKERS
    name anywhere, and no PRIVATE_PHRASES prose anywhere, and no internal tracking code
    (TRACKING_CODE_REGEX) anywhere. Returns a real,
    ready-to-`fail()` error message on the first violation or check failure,
    or `None` if the boundary genuinely holds.

    In a Git working tree this shells out to `git grep` there - the same boundary this repo and every sibling repo's own CI
    already enforced individually before this module existed.

    A checkout that is not a Git working tree (a source archive, a plain
    copy) or has no Git at hand gets the same three rules by reading its
    files, skipping the directories that never hold versioned text.
    """
    if not _is_git_work_tree(root):
        return _boundary_without_git(root)
    for marker in PRIVATE_MARKERS:
        result = subprocess.run(
            ("git", "grep", "-n", "-I", "--", marker),
            cwd=root,
            text=True,
            encoding="utf-8",
            errors="replace",
            stdout=subprocess.PIPE,
            stderr=subprocess.DEVNULL,
            check=False,
        )
        if result.returncode == 0:
            return "public files must not reference private documentation"
        if result.returncode not in (0, 1):
            return "could not check public/private documentation boundary"

    phrase_cmd = ["git", "grep", "-n", "-I", "-i", "-F"]
    for phrase in PRIVATE_PHRASES:
        phrase_cmd += ["-e", phrase]
    prose_result = subprocess.run(
        tuple(phrase_cmd),
        cwd=root,
        text=True,
        encoding="utf-8",
        errors="replace",
        stdout=subprocess.PIPE,
        stderr=subprocess.DEVNULL,
        check=False,
    )
    if prose_result.returncode == 0:
        return "public files must not reference private planning or audit documents"
    if prose_result.returncode not in (0, 1):
        return "could not check public/private documentation boundary"

    code_result = subprocess.run(
        ("git", "grep", "-n", "-I", "-E", "-e", TRACKING_CODE_REGEX),
        cwd=root,
        text=True,
        encoding="utf-8",
        errors="replace",
        stdout=subprocess.PIPE,
        stderr=subprocess.DEVNULL,
        check=False,
    )
    if code_result.returncode == 0:
        return "public files must not carry internal tracking codes"
    if code_result.returncode not in (0, 1):
        return "could not check public/private documentation boundary"

    return None


# Building never changes a version: raising it is the explicit `bump` command
# of tools/hydra_project_tool.py. A shell, batch or PowerShell script at the
# top of the repository (or in scripts/) that runs one of the bump scripts is
# therefore a mistake, and so is an npm script other than `bump*` that does.
_BUMP_CALL = re.compile(
    r"bump[_-]version|bump_manifest_version|bump_version_code|bump\.py|hydra_project_tool(?:\.py)?[\"']?\s+bump"
)
_NOT_CODE = ("#", "rem ", "::", "echo", "printf", "write-host", "write-output", "//")


def _bump_calls_in_script(path: Path) -> list[str]:
    found: list[str] = []
    for number, line in enumerate(path.read_text(encoding="utf-8", errors="replace").splitlines(), start=1):
        code = line.strip()
        if not code or code.lower().startswith(_NOT_CODE):
            continue
        if _BUMP_CALL.search(code):
            found.append(f"{path.name}:{number}")
    return found


_ACTIVATE_CALL = re.compile(r"activate(?:\.bat)?\b", re.I)


def check_scripts_do_not_activate_venv(root: Path) -> str | None:
    """Fail when a script activates a virtual environment.

    The python that is first on a machine's PATH can be a stripped one (for
    example the one of ESP-IDF) whose venv has no activate script, so the
    scripts run `.venv/.../python` directly instead. Comment lines and lines
    that only print text are ignored.
    """
    offenders: list[str] = []
    for folder in (root, root / "scripts"):
        if not folder.is_dir():
            continue
        for script in sorted([*folder.glob("*.sh"), *folder.glob("*.bat"), *folder.glob("*.cmd"), *folder.glob("*.ps1")]):
            for number, line in enumerate(script.read_text(encoding="utf-8", errors="replace").splitlines(), start=1):
                code = line.strip()
                if not code or code.lower().startswith(_NOT_CODE):
                    continue
                if "venv" in code.lower() and _ACTIVATE_CALL.search(code):
                    offenders.append(f"{script.name}:{number}")
    if offenders:
        return "scripts must run the python of the venv directly, never an activate script: " + ", ".join(offenders[:5])
    return None


def check_build_scripts_do_not_bump(root: Path) -> str | None:
    """Fail when a build script runs a version bump.

    Looks at the shell, batch and PowerShell scripts in `root` and in
    `root/scripts`, ignoring comment lines and the lines that only print text,
    and at the `scripts` of `package.json` (a script whose own name does not
    start with `bump`). Returns a ready-to-`fail()` message naming the first
    offenders, or `None` when no build path changes a version.
    """
    offenders: list[str] = []
    for folder in (root, root / "scripts"):
        if not folder.is_dir():
            continue
        for script in sorted([*folder.glob("*.sh"), *folder.glob("*.bat"), *folder.glob("*.cmd"), *folder.glob("*.ps1")]):
            offenders += _bump_calls_in_script(script)
    package = root / "package.json"
    if package.is_file():
        try:
            scripts = json.loads(package.read_text(encoding="utf-8")).get("scripts", {})
        except ValueError:
            scripts = {}
        for name, command in scripts.items():
            if not name.startswith("bump") and _BUMP_CALL.search(str(command)):
                offenders.append(f"package.json:{name}")
    if offenders:
        return "building must never change the version; run bump explicitly (tools/hydra_project_tool.py bump): " + ", ".join(offenders[:5])
    return None
