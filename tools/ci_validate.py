#!/usr/bin/env python3
# =============================================================================
# HYDRA-UMC - ci_validate.py
# Copyright (C) 2026 JuanenRac (Electro Hobby 3D) <electrohobby3d@gmail.com>
# GPL-3.0 - see LICENSE
# =============================================================================
"""Run the dependency-free, non-destructive CI baseline for one repository."""

from __future__ import annotations

import json
import re
import sys
import subprocess
from pathlib import Path


ROOT = Path(__file__).resolve().parent.parent
MANIFEST_PATH = ROOT / "hydra-umc.project.json"
REQUIRED_DOCUMENTS = (
    "README.md",
    "README_spa.md",
    "README_fra.md",
    "README_ita.md",
    "README_deu.md",
    "README_zho.md",
    "README_jpn.md",
    "CHANGELOG.md",
    "LICENSE",
    "CONTRIBUTING.md",
    "CODE_OF_CONDUCT.md",
    "SECURITY.md",
    "SUPPORT.md",
)
REQUIRED_BUILD_TEST_FILES = (
    "build-test.bat",
    "build-test.sh",
    "tools/build_test.py",
)
REQUIRED_MANIFEST_KEYS = (
    "schema_version",
    "ecosystem",
    "name",
    "version",
    "role",
    "stack",
    "technologies",
    "deployment_target",
    "maturity",
    "family",
    "parent",
    "build",
    "notes",
    "native_version",
)
VERSION_HEADING = re.compile(r"(?im)^#{1,3}\s*\[?(\d+\.\d+\.\d+)(?:\]|\s|$)")
SEMVER = re.compile(r"^\d+\.\d+\.\d+$")

# Found while investigating a systemic pattern of stale version/test-count
# claims in README prose (2026-09-07): a version bump already updates the
# manifest, the native version file and CHANGELOG.md's own heading (all
# three already checked above) - but a README's own free prose ("Real
# today: version `X.Y.Z`", "Status: vX.Y.Z", a `<project> vX.Y.Z` CLI
# example line) is never re-derived from any of those and can silently
# go stale. This checks for a semver-shaped token sitting next to a known
# "current version" label word (in any of the 7 languages) or right
# after this project's own name, and fails if it disagrees with the real
# manifest version. It skips lines describing the odometer versioning
# scheme itself (those always show an arrow between two version numbers,
# e.g. "0.0.9 -> 0.1.0").
VERSION_PROSE_TOKEN = re.compile(r"\bv?(\d+\.\d+\.\d+)\b")
VERSION_PROSE_LABELS = (
    "Real today", "Real hoy", "Réel aujourd'hui", "Reale oggi", "Heute real",
    "目前真实的部分", "現時点で実在するもの",
    "Version:", "Versión:", "Version :", "Versione:", "版本：", "バージョン：",
    "Current version", "Versión actual", "Version actuelle",
    "Versione attuale", "Aktuelle Version", "当前版本", "現在のバージョン",
    "Status:", "Estado:", "État :", "Stato:", "状态：", "ステータス",
)
VERSION_PROSE_LABEL_PATTERN = re.compile(
    "|".join(f"(?<![A-Za-z\u00c0-\u024f])(?:{re.escape(label)})" for label in VERSION_PROSE_LABELS)
)


def validate_readme_version_prose(manifest: dict) -> None:
    real_version = manifest["version"]
    name_pattern = re.compile(re.escape(manifest["name"]) + r"\s+v(\d+\.\d+\.\d+)")
    offenders: list[str] = []
    for document_name in REQUIRED_DOCUMENTS:
        if not document_name.startswith("README"):
            continue
        path = ROOT / document_name
        if not path.is_file():
            continue
        text = path.read_text(encoding="utf-8", errors="replace")
        for line_number, line in enumerate(text.splitlines(), start=1):
            if "->" in line or "\u2192" in line:
                continue
            if VERSION_PROSE_LABEL_PATTERN.search(line) is None and name_pattern.search(line) is None:
                continue
            for match in VERSION_PROSE_TOKEN.finditer(line):
                found = match.group(1)
                if found != real_version:
                    offenders.append(f"{document_name}:{line_number}: states {found}, manifest says {real_version}")
    if offenders:
        preview = "; ".join(offenders[:10])
        suffix = "" if len(offenders) <= 10 else f" (+{len(offenders) - 10} more)"
        fail(f"README states a stale current-version number: {preview}{suffix}")

BUILD_RUN_SECTION_MARKERS = (
    "BUILD & RUN",
    "COMPILACIÓN Y EJECUCIÓN",
    "COMPILATION ET EXÉCUTION",
    "COMPILAZIONE ED ESECUZIONE",
    "ERSTELLEN UND AUSFÜHREN",
    "BUILD & AUSFÜHRUNG",
    "ビルドと実行",
    "构建与运行",
)


def has_build_run_section(text: str) -> bool:
    """Accept the translated BUILD & RUN heading in every public README."""
    return any(marker in text for marker in BUILD_RUN_SECTION_MARKERS)


def fail(message: str) -> None:
    print(f"CI_VALIDATION=FAIL {message}", file=sys.stderr)
    raise SystemExit(1)


def read_native_version(text: str, pattern: str | dict[str, str]) -> str:
    if isinstance(pattern, dict):
        values: list[str] = []
        for component in ("major", "minor", "patch"):
            match = re.search(pattern[component], text, re.MULTILINE)
            if match is None:
                raise ValueError(f"native {component} version component not found")
            values.append(match.group(1))
        return ".".join(values)
    match = re.search(pattern, text, re.MULTILINE)
    if match is None or len(match.groups()) < 3:
        raise ValueError("native version pattern did not expose major.minor.patch")
    return ".".join(match.group(index) for index in (1, 2, 3))



LOCAL_MARKDOWN_LINK = re.compile(r"(?<!!)\[[^\]]*\]\(([^)\s]+)(?:\s+[\"'][^)]*)?\)")
MARKDOWN_EXCLUDED_DIRECTORIES = {
    ".git", ".venv", "venv", "node_modules", "build", "dist", "target",
    "__pycache__", ".gradle",
}


def validate_local_markdown_links() -> None:
    """Reject broken relative file links without probing external URLs."""
    broken: list[str] = []
    for markdown_path in ROOT.rglob("*.md"):
        if any(part in MARKDOWN_EXCLUDED_DIRECTORIES for part in markdown_path.parts):
            continue
        text = markdown_path.read_text(encoding="utf-8", errors="replace")
        for line_number, line in enumerate(text.splitlines(), start=1):
            for match in LOCAL_MARKDOWN_LINK.finditer(line):
                reference = match.group(1).strip().strip("<>")
                target = reference.split("#", maxsplit=1)[0].split("?", maxsplit=1)[0]
                if not target or re.match(r"(?i)^(https?:|mailto:|tel:|data:)", target):
                    continue
                if target.startswith("/"):
                    continue
                destination = (markdown_path.parent / target).resolve()
                if not destination.exists():
                    relative = markdown_path.relative_to(ROOT)
                    broken.append(f"{relative}:{line_number} -> {reference}")
    if broken:
        preview = "; ".join(broken[:10])
        suffix = "" if len(broken) <= 10 else f" (+{len(broken) - 10} more)"
        fail(f"broken local Markdown link(s): {preview}{suffix}")
def main() -> int:
    try:
        manifest = json.loads(MANIFEST_PATH.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as exc:
        fail(f"cannot read manifest: {exc}")

    missing = [key for key in REQUIRED_MANIFEST_KEYS if key not in manifest]
    if missing:
        fail(f"manifest missing required keys: {', '.join(missing)}")
    if manifest["ecosystem"] != "HYDRA-UMC":
        fail("manifest ecosystem must be HYDRA-UMC")
    if manifest["name"] != ROOT.name:
        fail(f"manifest name {manifest['name']!r} must match repository directory {ROOT.name!r}")
    if not isinstance(manifest["version"], str) or not SEMVER.fullmatch(manifest["version"]):
        fail("manifest version must be MAJOR.MINOR.PATCH")
    if not isinstance(manifest["technologies"], list) or not manifest["technologies"]:
        fail("manifest technologies must be a non-empty list")
    if not isinstance(manifest["native_version"], dict):
        fail("manifest native_version must be an object")

    missing_documents = [name for name in REQUIRED_DOCUMENTS if not (ROOT / name).is_file()]
    if missing_documents:
        fail(f"required documentation missing: {', '.join(missing_documents)}")
    if not (ROOT / "bump_manifest_version.py").is_file():
        fail("bump_manifest_version.py is required")

    missing_build_test_files = [name for name in REQUIRED_BUILD_TEST_FILES if not (ROOT / name).is_file()]
    if missing_build_test_files:
        fail(f"build-test files missing: {', '.join(missing_build_test_files)}")
    # Build and run instructions belong beside each project's relevant
    # workflows.  A duplicate trailing heading is intentionally not required.
    native = manifest["native_version"]
    try:
        native_path = ROOT / str(native["file"])
        native_text = native_path.read_text(encoding="utf-8", errors="replace")
        native_version = read_native_version(native_text, native["pattern"])
    except (KeyError, OSError, TypeError, ValueError, re.error) as exc:
        fail(f"cannot validate native version: {exc}")
    if native_version != manifest["version"]:
        fail(f"native version {native_version} differs from manifest {manifest['version']}")

    changelog = (ROOT / "CHANGELOG.md").read_text(encoding="utf-8", errors="replace")
    heading = VERSION_HEADING.search(changelog)
    if heading is None or heading.group(1) != manifest["version"]:
        found = heading.group(1) if heading else "<none>"
        fail(f"latest changelog version {found} differs from manifest {manifest['version']}")

    validate_readme_version_prose(manifest)

    gitignore = ROOT / ".gitignore"
    if not gitignore.is_file():
        fail(".gitignore is required for secret protection")
    ignored = gitignore.read_text(encoding="utf-8", errors="replace")
    if not re.search(r"(?m)^\.env(?:\.|$|\*)", ignored):
        fail(".gitignore must exclude .env files")
    if not re.search(r"(?m)^!\.env\.example$", ignored):
        fail(".gitignore must explicitly retain .env.example")

    validate_local_markdown_links()

    private_marker = "SON" + "NET"
    private_references = subprocess.run(
        ("git", "grep", "-n", "-I", "--", private_marker),
        cwd=ROOT,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.DEVNULL,
        check=False,
    )
    if private_references.returncode == 0:
        fail("public files must not reference private documentation")
    if private_references.returncode not in (0, 1):
        fail("could not check public/private documentation boundary")

    # Prose that names this ecosystem's private planning or audit material.
    # Each phrase below is split across a "+" so this validator's own source
    # never contains the literal string it is searching for.
    private_phrases = (
        "BIB" + "LIA HYDRA" + "-UMC",
        "private development" + " plan",
        "plan de desarrollo" + " privado",
        "internal work" + " log",
        "registro de trabajo" + " interno",
    )
    _phrase_cmd = ["git", "grep", "-n", "-I", "-i", "-F"]
    for _phrase in private_phrases:
        _phrase_cmd += ["-e", _phrase]
    private_prose = subprocess.run(
        tuple(_phrase_cmd),
        cwd=ROOT,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.DEVNULL,
        check=False,
    )
    if private_prose.returncode == 0:
        fail("public files must not reference private planning or audit documents")
    if private_prose.returncode not in (0, 1):
        fail("could not check public/private documentation boundary")

    print(f"CI_VALIDATION=PASS project={manifest['name']} version={manifest['version']}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
