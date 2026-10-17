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

import re
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
    return problems
