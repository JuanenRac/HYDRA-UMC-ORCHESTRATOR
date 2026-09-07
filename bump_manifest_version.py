#!/usr/bin/env python3
# =============================================================================
# HYDRA-UMC - Manifest-backed build version utility
# Copyright (C) 2026 JuanenRac (Electro Hobby 3D) <electrohobby3d@gmail.com>
# GPL-3.0 - see LICENSE
# =============================================================================
"""Increment or synchronize a repository-native version and its manifest.

This file is intentionally copied to each HYDRA-UMC/URTC repository root so
its build scripts work from a normal standalone checkout.  It reads only the
repository's own ``hydra-umc.project.json``; there is no project catalogue or
per-project conditional logic in this utility.
"""

from __future__ import annotations

import argparse
import json
import re
from pathlib import Path


ROOT = Path(__file__).resolve().parent
MANIFEST_PATH = ROOT / "hydra-umc.project.json"
CHANGELOG_PATH = ROOT / "CHANGELOG.md"


def next_version(value: str) -> str:
    """Return the documented decimal odometer successor of MAJOR.MINOR.PATCH."""
    major, minor, patch = (int(part) for part in value.split("."))
    patch += 1
    if patch == 10:
        minor, patch = minor + 1, 0
    if minor == 10:
        major, minor = major + 1, 0
    return f"{major}.{minor}.{patch}"


def read_version(text: str, pattern: str | dict[str, str]) -> str:
    if isinstance(pattern, dict):
        parts: dict[str, str] = {}
        for key in ("major", "minor", "patch"):
            match = re.search(pattern[key], text, re.MULTILINE)
            if match is None:
                raise ValueError(f"native version {key} component was not found")
            parts[key] = match.group(1)
        return f"{parts['major']}.{parts['minor']}.{parts['patch']}"
    match = re.search(pattern, text, re.MULTILINE)
    if match is None or len(match.groups()) < 3:
        raise ValueError("native version was not found")
    return ".".join(match.group(index) for index in (1, 2, 3))


def replace_version(text: str, pattern: str | dict[str, str], version: str) -> str:
    values = version.split(".")
    replacements: list[tuple[int, int, str]] = []
    if isinstance(pattern, dict):
        for key, value in zip(("major", "minor", "patch"), values, strict=True):
            match = re.search(pattern[key], text, re.MULTILINE)
            if match is None:
                raise ValueError(f"native version {key} component was not found")
            replacements.append((*match.span(1), value))
    else:
        match = re.search(pattern, text, re.MULTILINE)
        if match is None or len(match.groups()) < 3:
            raise ValueError("native version was not found")
        replacements.extend((*match.span(index), value) for index, value in zip((1, 2, 3), values, strict=True))
    for start, end, value in sorted(replacements, reverse=True):
        text = text[:start] + value + text[end:]
    return text


def write_manifest(data: dict[str, object]) -> None:
    MANIFEST_PATH.write_text(json.dumps(data, indent=2, ensure_ascii=False) + "\n", encoding="utf-8")


def ensure_changelog_version(version: str) -> bool:
    """Record a version only when the public changelog does not yet contain it.

    Build scripts may be responsible only for a routine version increment.  The
    entry deliberately stays factual instead of inventing feature notes; people
    can expand it later with the actual release details.
    """
    if not CHANGELOG_PATH.is_file():
        raise ValueError("CHANGELOG.md is required for a versioned project")
    text = CHANGELOG_PATH.read_text(encoding="utf-8", errors="replace")
    heading = re.compile(rf"(?m)^##+\s+\[?{re.escape(version)}(?:\]|\s|$)")
    if heading.search(text):
        return False

    entry = (
        f"## [{version}]\n\n"
        "- Build version synchronized with `hydra-umc.project.json` and the "
        "repository-native version source.\n\n"
    )
    previous = re.search(r"(?m)^##+\s+\[?\d+\.\d+\.\d+", text)
    if previous:
        text = text[: previous.start()] + entry + text[previous.start() :]
    else:
        first_line_end = text.find("\n")
        insertion = first_line_end + 1 if first_line_end >= 0 else len(text)
        text = text[:insertion] + "\n" + entry + text[insertion:]
    CHANGELOG_PATH.write_text(text, encoding="utf-8")
    return True


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--sync",
        action="store_true",
        help="accept exactly one preceding native increment and synchronize the manifest",
    )
    args = parser.parse_args()

    try:
        manifest = json.loads(MANIFEST_PATH.read_text(encoding="utf-8"))
        declared = manifest["version"]
        native = manifest["native_version"]
        if not isinstance(declared, str) or not isinstance(native, dict):
            raise ValueError("manifest lacks version or native_version")
        source = ROOT / str(native["file"])
        pattern = native["pattern"]
        source_text = source.read_text(encoding="utf-8", errors="replace")
        current = read_version(source_text, pattern)
    except (OSError, KeyError, TypeError, json.JSONDecodeError, ValueError) as exc:
        print(f"ERROR: {exc}")
        return 1

    if args.sync:
        if current == declared:
            added = ensure_changelog_version(current)
            if added:
                print(f"Changelog synchronized: {current}")
            print(f"HYDRA-UMC version: v{current} -> v{current} (already synchronized)")
            return 0
        if current != next_version(declared):
            print(f"ERROR: native version {current} is not the next version after manifest {declared}")
            return 1
        manifest["version"] = current
        write_manifest(manifest)
        ensure_changelog_version(current)
        print(f"HYDRA-UMC version: v{declared} -> v{current}")
        return 0

    if current != declared:
        print(f"ERROR: native version {current} differs from manifest {declared}; run validation or use --sync after one native bump")
        return 1
    new = next_version(current)
    source.write_text(replace_version(source_text, pattern, new), encoding="utf-8")
    manifest["version"] = new
    write_manifest(manifest)
    ensure_changelog_version(new)
    print(f"HYDRA-UMC version: v{current} -> v{new}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
