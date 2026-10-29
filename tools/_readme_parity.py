# =============================================================================
# HYDRA-UMC-ORCHESTRATOR - tools/_readme_parity.py
# Copyright (C) 2026 JuanenRac (Electro Hobby 3D) <electrohobby3d@gmail.com>
# GPL-3.0-or-later - see LICENSE
# =============================================================================
# VENDORED - do not hand-edit. This is a byte-for-byte copy of
# HYDRA-UMC-SDK's own canonical
# clients/python/src/hydra_umc_sdk/readme_parity.py, kept in sync
# by that repo's own tools/sync_readme_parity.py. Edit the rule there,
# then re-run that script to update every repo that vendors it, this one
# included.

from __future__ import annotations

import fnmatch
import re
import subprocess
from pathlib import Path
from typing import Sequence

DEFAULT_LANGUAGE_FILES: tuple[str, ...] = (
    "README.md",
    "README_spa.md",
    "README_fra.md",
    "README_ita.md",
    "README_deu.md",
    "README_zho.md",
    "README_jpn.md",
    "README_por.md",
)

# Translations whose absence is itself a violation. Portuguese is part of the
# ecosystem's eight languages (en/es/fr/it/de/zh/ja/pt), so a repository
# without README_por.md fails its own CI instead of silently falling behind.
REQUIRED_LANGUAGE_FILES: tuple[str, ...] = ("README_por.md",)

_HEADING_RE = re.compile(r"(?m)^##\s+(?:\d+\.\s+)?(\S+)")
_LINK_RE = re.compile(r"[]][(](https?://[^)\s]+)[)]")


def readme_section_signature(path: Path) -> list[str]:
    """The ordered list of `## ` heading emoji tokens in `path`, read as
    UTF-8. A file with no `## ` headings at all returns an empty list -
    a real, honest "nothing to compare", not an error."""
    text = path.read_text(encoding="utf-8")
    return _HEADING_RE.findall(text)


def readme_link_set(path: Path) -> set[str]:
    """The external (http/https) link targets in `path`."""
    return set(_LINK_RE.findall(path.read_text(encoding="utf-8")))


def check_readme_section_parity(
    root: Path,
    language_files: Sequence[str] = DEFAULT_LANGUAGE_FILES,
    required: Sequence[str] = REQUIRED_LANGUAGE_FILES,
) -> list[str]:
    """Compares `README.md`'s own heading signature (and its set of external
    links) against every other
    language file in `language_files` that actually exists under `root`.
    Returns a list of real, ready-to-report mismatch descriptions (empty
    if every present translation's structure matches the English
    original exactly). A missing translation file is not itself a
    parity violation here - `ci_validate.py`'s own REQUIRED_DOCUMENTS
    check already covers that separately - EXCEPT the files named in
    `required` (the Portuguese README by default), whose absence is
    reported here too, so the rule travels with the vendored copy.
    """
    english_path = root / language_files[0]
    if not english_path.is_file():
        return [f"{language_files[0]} is missing - cannot check section parity"]
    english_signature = readme_section_signature(english_path)
    english_links = readme_link_set(english_path)

    problems: list[str] = []
    for required_file in required:
        if required_file in language_files and not (root / required_file).is_file():
            problems.append(
                f"{required_file} is missing - the Portuguese README is required in every repository "
                f"(see the other translations for the expected structure)"
            )
    for language_file in language_files[1:]:
        path = root / language_file
        if not path.is_file():
            continue
        signature = readme_section_signature(path)
        if signature != english_signature:
            problems.append(
                f"{language_file} section structure does not match {language_files[0]}: "
                f"expected {english_signature}, got {signature}"
            )
        links = readme_link_set(path)
        if links != english_links:
            missing = sorted(english_links - links)
            extra = sorted(links - english_links)
            problems.append(
                f"{language_file} links do not match {language_files[0]}: missing {missing}, extra {extra}"
            )
    problems.extend(check_readme_tree(root, language_files))
    return problems


# The first level of a repository is described by the file tree of its README, so an entry that Git
# versions but the tree leaves out is a README that went stale. What every repository has, and the
# files of tooling and configuration that need no sentence of their own, are not asked for.
_TREE_EXEMPT = re.compile(
    r"^(README(_[a-z]{3})?\.md|LICENSE(\.md)?|CHANGELOG\.md|CODE_OF_CONDUCT\.md|CONTRIBUTING\.md|SECURITY\.md|SUPPORT\.md"
    r"|THIRD_PARTY_NOTICES\.md|.*project\.json|images|\..*|build.*|run.*|test\.(bat|sh)|dev.*|bump.*|.*\.lock|package(-lock)?\.json"
    r"|.*\.toml|mypy\.ini|pytest\.ini|tox\.ini|go\.(mod|sum)|Dockerfile.*|docker-compose.*|eslint.*|tsconfig.*|vite.*"
    r"|requirements.*|gradle.*|gradlew.*|settings\.gradle.*|local\.properties)$"
)
_TREE_FENCE = re.compile(r"```[a-z]*\r?\n(.*?)```", re.S)
_TREE_ROW = re.compile(r"^[├└]── ([^\s,]+(?:(?:, | / | \+ )[^\s,]+)*)")


def readme_tree_names(path: Path) -> set[str] | None:
    """The first-level names that the file tree of `path` lists (a pattern such as `sdkconfig.*` included; `a.sh/.bat` and `a.sh / .bat`
    list both files), or None when the README has no file tree. The tree is the code block with the most tree rows."""
    text = path.read_text(encoding="utf-8")
    best = ""
    best_rows = 0
    for fence in _TREE_FENCE.finditer(text):
        rows = len(re.findall(r"^[├└]── ", fence.group(1), re.M))
        if rows > best_rows:
            best, best_rows = fence.group(1), rows
    if not best_rows:
        return None
    names: set[str] = set()
    for line in best.splitlines():
        found = _TREE_ROW.match(line)
        if not found:
            continue
        previous = ""
        for token in re.split(r", | / | \+ ", found.group(1)):
            for part in re.split(r"/(?=\.)", token):
                part = part.rstrip("/")
                if part.startswith(".") and "." in previous.lstrip("."):
                    part = previous.rsplit(".", 1)[0] + part
                first = part.split("/")[0]
                if first:
                    names.add(first)
                previous = part
    return names


def tracked_first_level(root: Path) -> list[str] | None:
    """The first-level entries of the repository that Git versions, or None when Git cannot tell."""
    try:
        result = subprocess.run(["git", "ls-files", "-z"], cwd=root, capture_output=True, check=True)
    except (OSError, subprocess.CalledProcessError):
        return None
    entries = {item.split("/")[0] for item in result.stdout.decode("utf-8", "replace").split(chr(0)) if item}
    return sorted(entries) or None


def check_readme_tree(
    root: Path,
    language_files: Sequence[str] = DEFAULT_LANGUAGE_FILES,
    tracked: Sequence[str] | None = None,
) -> list[str]:
    """For every README that has a file tree, the versioned first-level entries that the tree leaves out."""
    entries = list(tracked) if tracked is not None else tracked_first_level(root)
    if entries is None:
        return []
    problems: list[str] = []
    for language_file in language_files:
        path = root / language_file
        if not path.is_file():
            continue
        listed = readme_tree_names(path)
        if listed is None:
            continue
        missing = [
            name for name in entries
            if not _TREE_EXEMPT.match(name) and not any(fnmatch.fnmatchcase(name, pattern) for pattern in listed)
        ]
        if missing:
            problems.append(f"{language_file} file tree does not list: {', '.join(missing)}")
    return problems
