#!/usr/bin/env python3
# =============================================================================
# HYDRA-UMC - ci_validate.py
# Copyright (C) 2026 JuanenRac (Electro Hobby 3D) <electrohobby3d@gmail.com>
# GPL-3.0-or-later - see LICENSE
# =============================================================================
"""Run the dependency-free, non-destructive CI baseline for one repository."""

from __future__ import annotations

import importlib.util
import json
import os
import re
import subprocess
import sys
from pathlib import Path


from _doc_policy import (
    check_build_scripts_do_not_bump,
    check_public_private_boundary,
    check_scripts_do_not_activate_venv,
)
from _readme_parity import check_readme_section_parity

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
    "README_por.md",
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
VERSION_HEADING = re.compile(r"(?im)^#{1,3}\s*\[?(\d+\.\d+\.\d+(?:\.\d+)?)(?:\]|\s|$)")
SEMVER = re.compile(r"^\d+\.\d+\.\d+(?:\.\d+)?$")

# Found while investigating a systemic pattern of stale version/test-count
# claims in README prose: a version bump already updates the
# manifest, the native version file and CHANGELOG.md's own heading (all
# three already checked above) - but a README's own free prose ("Real
# today: version `X.Y.Z`", "Status: vX.Y.Z", a `<project> vX.Y.Z` CLI
# example line) is never re-derived from any of those and can silently
# go stale. This checks for a semver-shaped token sitting next to a known
# "current version" label word (in any of the 8 languages) or right
# after this project's own name, and fails if it disagrees with the real
# manifest version. It skips lines describing the odometer versioning
# scheme itself (those always show an arrow between two version numbers,
# e.g. "0.0.9 -> 0.1.0").
VERSION_PROSE_TOKEN = re.compile(r"\bv?(\d+\.\d+\.\d+(?:\.\d+)?)\b")
VERSION_PROSE_LABELS = (
    "Real today", "Real hoy", "Réel aujourd'hui", "Reale oggi", "Heute real",
    "目前真实的部分", "現時点で実在するもの",
    "Version:", "Versión:", "Version :", "Versione:", "版本：", "バージョン：",
    "Current version", "Versión actual", "Version actuelle",
    "Versione attuale", "Aktuelle Version", "当前版本", "現在のバージョン",
    "Status:", "Estado:", "État :", "Statut :", "Statut:", "Stato:", "状态：", "ステータス",
    "状態:", "状態：",
    "Real hoje", "Versão:", "Versão atual",
)
VERSION_PROSE_LABEL_PATTERN = re.compile(
    "|".join(f"(?<![A-Za-z\u00c0-\u024f])(?:{re.escape(label)})" for label in VERSION_PROSE_LABELS)
)


def validate_readme_version_prose(manifest: dict) -> None:
    real_version = manifest["version"]
    name_pattern = re.compile(re.escape(manifest["name"]) + r"\s+v(\d+\.\d+\.\d+(?:\.\d+)?)")
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
    "INSTALL AND RUN",
    "INSTALAR Y EJECUTAR",
    "INSTALLATION ET EXÉCUTION",
    "INSTALLAZIONE ED ESECUZIONE",
    "INSTALLATION UND AUSFÜHRUNG",
    "インストールと実行",
    "安装与运行",
    "DEVELOPMENT ENVIRONMENT",
    "ENTORNO DE DESARROLLO",
    "ENVIRONNEMENT DE DÉVELOPPEMENT",
    "AMBIENTE DI SVILUPPO",
    "ENTWICKLUNGSUMGEBUNG",
    "開発環境",
    "开发环境",
)


def has_build_run_section(text: str) -> bool:
    """Accept the translated BUILD & RUN heading in every public README."""
    normalized = text.upper()
    return any(marker.upper() in normalized for marker in BUILD_RUN_SECTION_MARKERS)


def fail(message: str) -> None:
    print(f"CI_VALIDATION=FAIL {message}", file=sys.stderr)
    raise SystemExit(1)


_THREE_GROUPS = re.compile(r"(\([^()]*\))\\\.(\([^()]*\))\\\.(\([^()]*\))(?!\(\?:\\[.+])")


def _with_optional_fourth_group(pattern: str) -> str:
    """The native-version pattern, made to accept a fourth `.N` component too."""
    match = _THREE_GROUPS.search(pattern)
    if match is None:
        return pattern
    return pattern[: match.end()] + r"(?:[.+]" + match.group(3) + ")?" + pattern[match.end() :]


def read_native_version(text: str, pattern: str | dict[str, str]) -> str:
    if isinstance(pattern, dict):
        values: list[str] = []
        for component in ("major", "minor", "patch"):
            match = re.search(pattern[component], text, re.MULTILINE)
            if match is None:
                raise ValueError(f"native {component} version component not found")
            values.append(match.group(1))
        if "build" in pattern and tuple(int(v) for v in values) >= (0, 8, 0):
            build = re.search(pattern["build"], text, re.MULTILINE)
            if build is not None:
                values.append(build.group(1))
        return ".".join(values)
    match = re.search(_with_optional_fourth_group(pattern), text, re.MULTILINE)
    if match is None or len(match.groups()) < 3:
        raise ValueError("native version pattern did not expose major.minor.patch")
    parts = [match.group(index) for index in (1, 2, 3)]
    if len(match.groups()) >= 4 and match.group(4) is not None:
        parts.append(match.group(4))
    return ".".join(parts)



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
# Test counts written by hand in the manifest `notes` go stale the moment a test is added; the README's counts are
# checked against the code by the documentation tools instead. A number in front of the word "tests" (or "tests:
# 142", "142 passing") is refused.
TEST_COUNT_IN_NOTES = re.compile(
    r"(?i)\b\d+(?:,\d{3})*\s+(?:[\w-]+\s+){0,2}(?:tests?|test cases|assertions|specs)\b"
    r"|\b(?:tests?|assertions)\s*[:=]\s*\d+"
    r"|\b\d+\s+(?:passing|passed)\b"
)


def check_notes_have_no_test_counts(manifest: dict) -> list[str]:
    notes = manifest.get("notes")
    if not isinstance(notes, str):
        return []
    found = [match.group(0).strip() for match in TEST_COUNT_IN_NOTES.finditer(notes)]
    if not found:
        return []
    shown = "; ".join(repr(text) for text in found[:3])
    return [f"manifest notes carry a hand-written test count ({shown}): drop it, the count lives in the README and is checked against the code"]


def changelog_version_section(changelog: str, version: str) -> str | None:
    """The text under the heading of `version` up to the next version heading, or None when there is no such heading."""
    headings = list(VERSION_HEADING.finditer(changelog))
    for index, heading in enumerate(headings):
        if heading.group(1) == version:
            end = headings[index + 1].start() if index + 1 < len(headings) else len(changelog)
            line_end = changelog.find("\n", heading.end())
            return changelog[(line_end if line_end != -1 else end):end]
    return None


def check_changelog_note(changelog: str, version: str, environment: dict[str, str] | None = None) -> list[str]:
    """The heading of the manifest version needs a note under it; on a tag build the tag must also name that version."""
    environment = os.environ if environment is None else environment
    section = changelog_version_section(changelog, version)
    if section is None:
        return [f"CHANGELOG.md has no heading for version {version}"]
    problems: list[str] = []
    if not any(line.strip() and not line.lstrip().startswith("#") for line in section.splitlines()):
        problems.append(f"CHANGELOG.md has no note under the heading of version {version}")
    reference = environment.get("GITHUB_REF", "")
    if environment.get("GITHUB_REF_TYPE") == "tag" or reference.startswith("refs/tags/"):
        tag = reference.removeprefix("refs/tags/") or environment.get("GITHUB_REF_NAME", "")
        if tag.removeprefix("v") != version:
            problems.append(f"the tag {tag!r} does not name the manifest version {version}")
    return problems


def check_glance(root: Path, environment: dict[str, str] | None = None) -> list[str]:
    """Run the documentation repository's `make_glance.py --check` for this repository when that checkout is at hand.

    The data and the tool live in HYDRA-UMC-DOCS: it is looked for in HYDRA_UMC_DOCS_DIR or next to this repository, and
    the check is skipped when it is absent or does not list this repository (a plain CI checkout has no sibling).
    """
    environment = os.environ if environment is None else environment
    docs = Path(environment["HYDRA_UMC_DOCS_DIR"]) if environment.get("HYDRA_UMC_DOCS_DIR") else root.parent / "HYDRA-UMC-DOCS"
    script, data = docs / "tools" / "make_glance.py", docs / "data" / "glance.json"
    if not script.is_file() or not data.is_file():
        return []
    try:
        listed = json.loads(data.read_text(encoding="utf-8")).get("repos", {})
    except (OSError, json.JSONDecodeError):
        return ["the glance data file of the documentation repository cannot be read"]
    if root.name not in listed:
        return []
    result = subprocess.run(
        [sys.executable, "-I", str(script), "--check", "--repo", root.name, "--workspace", str(root.parent)],
        cwd=docs, text=True, encoding="utf-8", errors="replace", capture_output=True, check=False,
    )
    if result.returncode == 0:
        return []
    lines = [line for line in (result.stdout + result.stderr).splitlines() if line.strip()]
    return ["make_glance.py --check: " + "; ".join(lines[:3])[:400]]


def check_ecosystem_release(root: Path, environment: dict[str, str] | None = None) -> list[str]:
    """Check this repository against `ecosystem-release.json` (the versions of the family that go together).

    The file is written by the SDK tool `tools/ecosystem_release.py` and published in the documentation repository
    (`data/ecosystem-release.json`): it is looked for in HYDRA_UMC_DOCS_DIR or next to this repository, and the check is
    skipped when it is absent or does not list this repository (a plain CI checkout has no sibling). The listed version
    must not be ahead of the manifest and the listed commit must be in this repository's history.
    """
    environment = os.environ if environment is None else environment
    docs = Path(environment["HYDRA_UMC_DOCS_DIR"]) if environment.get("HYDRA_UMC_DOCS_DIR") else root.parent / "HYDRA-UMC-DOCS"
    data = docs / "data" / "ecosystem-release.json"
    if not data.is_file():
        return []
    try:
        listed = json.loads(data.read_text(encoding="utf-8")).get("repositories", [])
        entry = next((item for item in listed if isinstance(item, dict) and item.get("name") == root.name), None)
    except (OSError, json.JSONDecodeError, AttributeError):
        return ["ecosystem-release.json of the documentation repository cannot be read"]
    if entry is None:
        return []
    problems: list[str] = []
    try:
        manifest_version = json.loads((root / "hydra-umc.project.json").read_text(encoding="utf-8"))["version"]
        wanted = tuple(int(part) for part in str(entry["version"]).split("."))
        have = tuple(int(part) for part in manifest_version.split("."))
        width = max(len(wanted), len(have))
        if wanted + (0,) * (width - len(wanted)) > have + (0,) * (width - len(have)):
            problems.append(f"ecosystem-release.json lists version {entry['version']}, ahead of the manifest ({manifest_version})")
    except (OSError, KeyError, ValueError, json.JSONDecodeError):
        problems.append("ecosystem-release.json has an entry for this repository that cannot be compared with the manifest")
    try:
        commit = str(entry.get("commit", ""))
        shallow = subprocess.run(["git", "rev-parse", "--is-shallow-repository"], cwd=root, text=True, capture_output=True, check=False)
        if not re.fullmatch(r"[0-9a-f]{40}", commit):
            problems.append("ecosystem-release.json lists a commit that is not 40 hexadecimal characters")
        elif shallow.returncode == 0 and shallow.stdout.strip() == "false":
            known = subprocess.run(["git", "cat-file", "-e", commit + "^{commit}"], cwd=root, capture_output=True, check=False)
            if known.returncode != 0:
                problems.append(f"ecosystem-release.json lists commit {commit[:12]}, which is not in the history of this repository")
    except OSError:
        pass
    return problems


def run_extra_checks() -> None:
    """The checks a repository has of its own live in tools/extra_checks.py, so this file stays identical everywhere.

    The module defines `check(root: Path) -> list[str]` and returns one sentence per problem it found (an empty
    list when all is well). It is loaded from its path, so it needs no package and no change to sys.path.
    """
    path = ROOT / "tools" / "extra_checks.py"
    if not path.is_file():
        return
    spec = importlib.util.spec_from_file_location("hydra_extra_checks", path)
    if spec is None or spec.loader is None:
        fail("tools/extra_checks.py cannot be loaded")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    checker = getattr(module, "check", None)
    if not callable(checker):
        fail("tools/extra_checks.py must define check(root) returning a list of problems")
    problems = checker(ROOT)
    if problems:
        fail("; ".join(str(problem) for problem in problems))


def _universal_readme_ci(root: Path) -> list[str]:
    """The layout of the README (22 sections in a fixed order, in the eight languages): a warning until the family has migrated; see tools/universal_readme_check.py."""
    try:
        sys.path.insert(0, str(Path(__file__).resolve().parent))
        import universal_readme_check
    except ImportError:
        return []
    return universal_readme_check.run_in_ci(root)


def _status_lights_ci(root: Path) -> list[str]:
    """The lights of the Project status table stay inside what the manifest (maturity, evidence) allows; see tools/status_lights.py."""
    try:
        sys.path.insert(0, str(Path(__file__).resolve().parent))
        import status_lights
    except ImportError:
        return []
    return status_lights.run_in_ci(root)


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
    # The bridges keep their bump script under tools/.
    if not (ROOT / "bump_manifest_version.py").is_file() and not (ROOT / "tools" / "bump_version.py").is_file():
        fail("bump_manifest_version.py (or tools/bump_version.py) is required")

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
    if Path(str(native["file"])).name == "pubspec.yaml":
        # a Flutter version name has three components (the build number follows the plus sign)
        same = native_version == ".".join(manifest["version"].split(".")[:3])
    elif Path(str(native["file"])).name == "package.json":
        # npm only accepts three components; a file written before that rule may still hold all four
        same = native_version in (manifest["version"], ".".join(manifest["version"].split(".")[:3]))
    else:
        same = native_version == manifest["version"]
    if not same:
        fail(f"native version {native_version} differs from manifest {manifest['version']}")

    changelog = (ROOT / "CHANGELOG.md").read_text(encoding="utf-8", errors="replace")
    heading = VERSION_HEADING.search(changelog)
    if heading is None or heading.group(1) != manifest["version"]:
        found = heading.group(1) if heading else "<none>"
        fail(f"latest changelog version {found} differs from manifest {manifest['version']}")
    for note_problem in check_changelog_note(changelog, manifest["version"]):
        fail(note_problem)

    validate_readme_version_prose(manifest)
    for notes_problem in check_notes_have_no_test_counts(manifest):
        fail(notes_problem)

    gitignore = ROOT / ".gitignore"
    if not gitignore.is_file():
        fail(".gitignore is required for secret protection")
    ignored = gitignore.read_text(encoding="utf-8", errors="replace")
    if not re.search(r"(?m)^\.env(?:\.|$|\*)", ignored):
        fail(".gitignore must exclude .env files")
    if not re.search(r"(?m)^!\.env\.example$", ignored):
        fail(".gitignore must explicitly retain .env.example")

    validate_local_markdown_links()

    doc_policy_error = check_public_private_boundary(ROOT)
    if doc_policy_error:
        fail(doc_policy_error)
    build_bump_error = check_build_scripts_do_not_bump(ROOT)
    if build_bump_error:
        fail(build_bump_error)
    venv_activate_error = check_scripts_do_not_activate_venv(ROOT)
    if venv_activate_error:
        fail(venv_activate_error)
    for readme_problem in check_readme_section_parity(ROOT):
        fail(readme_problem)
    for _universal_problem in _universal_readme_ci(ROOT):
        fail(_universal_problem)
    for _lights_problem in _status_lights_ci(ROOT):
        fail(_lights_problem)
    for glance_problem in check_glance(ROOT):
        fail(glance_problem)
    for release_problem in check_ecosystem_release(ROOT):
        fail(release_problem)
    run_extra_checks()

    print(f"CI_VALIDATION=PASS project={manifest['name']} version={manifest['version']}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
