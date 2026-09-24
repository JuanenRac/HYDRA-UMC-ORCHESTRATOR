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


# Versions from FOUR_PART_FROM on carry a fourth component; every digit rolls
# over past 9 into the one to its left.
FOUR_PART_FROM = (0, 8, 0)


def next_version(value: str) -> str:
    """Return the documented decimal odometer successor of a project version."""
    parts = [int(part) for part in value.split(".")]
    if len(parts) == 4:
        major, minor, patch, build = parts
        build += 1
        if build == 10:
            patch, build = patch + 1, 0
        if patch == 10:
            minor, patch = minor + 1, 0
        if minor == 10:
            major, minor = major + 1, 0
        return f"{major}.{minor}.{patch}.{build}"
    major, minor, patch = parts
    patch += 1
    if patch == 10:
        minor, patch = minor + 1, 0
    if minor == 10:
        major, minor = major + 1, 0
    if (major, minor, patch) >= FOUR_PART_FROM:
        return f"{major}.{minor}.{patch}.0"
    return f"{major}.{minor}.{patch}"


_THREE_GROUPS = re.compile(r"(\([^()]*\))\\.(\([^()]*\))\\.(\([^()]*\))(?!\(\?:\\.)")


def with_optional_fourth_group(pattern: str | dict[str, str]) -> str | dict[str, str]:
    """Let a native-version pattern also match a fourth `.N` component.

    Patterns are written for `MAJOR.MINOR.PATCH`; the same text with an
    optional fourth group behind the third one reads both forms. A pattern that
    already has it, or that keeps the components in separate fields, is
    returned unchanged.
    """
    if isinstance(pattern, dict):
        return pattern
    match = _THREE_GROUPS.search(pattern)
    if match is None:
        return pattern
    return pattern[: match.end()] + r"(?:\." + match.group(3) + ")?" + pattern[match.end() :]


def read_version(text: str, pattern: str | dict[str, str]) -> str:
    if isinstance(pattern, dict):
        parts: dict[str, str] = {}
        for key in ("major", "minor", "patch"):
            match = re.search(pattern[key], text, re.MULTILINE)
            if match is None:
                raise ValueError(f"native version {key} component was not found")
            parts[key] = match.group(1)
        version = f"{parts['major']}.{parts['minor']}.{parts['patch']}"
        # A separate field for the fourth component only counts from the
        # version that carries one; below it a stored 0 is just a placeholder.
        if "build" in pattern and tuple(int(parts[k]) for k in ("major", "minor", "patch")) >= FOUR_PART_FROM:
            build = re.search(pattern["build"], text, re.MULTILINE)
            if build is not None:
                version += "." + build.group(1)
        return version
    match = re.search(with_optional_fourth_group(pattern), text, re.MULTILINE)
    if match is None or len(match.groups()) < 3:
        raise ValueError("native version was not found")
    parts = [match.group(index) for index in (1, 2, 3)]
    if len(match.groups()) >= 4 and match.group(4) is not None:
        parts.append(match.group(4))
    return ".".join(parts)


def replace_version(text: str, pattern: str | dict[str, str], version: str) -> str:
    values = version.split(".")
    if isinstance(pattern, dict):
        keys = ("major", "minor", "patch", "build")[: len(values)]
        if len(values) == 4 and "build" not in pattern:
            raise ValueError(
                "this project keeps its version in separate fields and has no build field yet; "
                "add a build entry to native_version.pattern and a matching line to the version file"
            )
        replacements: list[tuple[int, int, str]] = []
        for key, value in zip(keys, values, strict=True):
            match = re.search(pattern[key], text, re.MULTILINE)
            if match is None:
                raise ValueError(f"native version {key} component was not found")
            replacements.append((*match.span(1), value))
        for start, end, value in sorted(replacements, reverse=True):
            text = text[:start] + value + text[end:]
        return text
    match = re.search(with_optional_fourth_group(pattern), text, re.MULTILINE)
    if match is None or len(match.groups()) < 3:
        raise ValueError("native version was not found")
    last = 4 if len(match.groups()) >= 4 and match.group(4) is not None else 3
    return text[: match.start(1)] + version + text[match.end(last) :]


def write_manifest(data: dict[str, object]) -> None:
    MANIFEST_PATH.write_text(json.dumps(data, indent=2, ensure_ascii=False) + "\n", encoding="utf-8")


def sync_package_lock(version: str) -> bool:
    """This utility used to only ever patch the ONE `native_version.file`
    (typically `package.json`, via a plain text regex - see
    `replace_version` above), leaving `package-lock.json`'s own separate
    copy of the same root version - both its own top-level `"version"`
    and `packages[""].version` (npm's own real lockfile-v2/v3 shape) -
    silently behind.

    A real, empirically-verified no-op for every OTHER repository this
    same shared script is copied into: `json.dumps(data, indent=2,
    ensure_ascii=False) + "\\n"` round-trips every one of this
    ecosystem's real npm lockfiles byte-for-byte when nothing in it
    actually changed, so the only real diff this ever produces is
    exactly the two version fields below - never a reformatted file,
    and never a resolved dependency's own version (this never touches
    any `packages["node_modules/..."]` entry), so `npm ci` keeps
    installing exactly what it already did.
    """
    lock_path = ROOT / "package-lock.json"
    if not lock_path.is_file():
        return False
    try:
        raw = lock_path.read_text(encoding="utf-8")
        lock = json.loads(raw)
    except (OSError, json.JSONDecodeError):
        return False
    changed = False
    if lock.get("version") != version:
        lock["version"] = version
        changed = True
    packages = lock.get("packages")
    if isinstance(packages, dict) and isinstance(packages.get(""), dict):
        if packages[""].get("version") != version:
            packages[""]["version"] = version
            changed = True
    if not changed:
        return False
    new_raw = json.dumps(lock, indent=2, ensure_ascii=False) + "\n"
    lock_path.write_text(new_raw, encoding="utf-8")
    return True


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


MIRROR_DIRECTORY_GLOBS = ("src/*/__init__.py", "*/__init__.py", "clients/python/src/*/__init__.py")


def sync_version_mirrors(old: str, new: str) -> list[str]:
    """Carry a package's own `__version__` line along with the native version.

    Only a line that still holds exactly the previous version is rewritten, so
    an unrelated `__version__` (a vendored copy, a test fixture) is never
    touched.
    """
    line = re.compile(r'(?m)^(__version__' + r'\s*=\s*")' + re.escape(old) + r'(")')
    changed: list[str] = []
    seen: set[Path] = set()
    for glob in MIRROR_DIRECTORY_GLOBS:
        for path in sorted(ROOT.glob(glob)):
            if path in seen or not path.is_file():
                continue
            seen.add(path)
            text = path.read_text(encoding="utf-8", errors="replace")
            if line.search(text):
                path.write_text(line.sub(lambda m: m.group(1) + new + m.group(2), text), encoding="utf-8")
                changed.append(str(path.relative_to(ROOT)))
    return changed


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
            sync_package_lock(current)
            print(f"HYDRA-UMC version: v{current} -> v{current} (already synchronized)")
            return 0
        expected = next_version(declared)
        if current != expected:
            if expected != current + ".0":
                print(f"ERROR: native version {current} is not the next version after manifest {declared}")
                return 1
            # A per-project bump script produced the last three-part version;
            # the ecosystem rule appends the fourth component.
            source.write_text(replace_version(source_text, pattern, expected), encoding="utf-8")
            manifest["native_version"]["pattern"] = with_optional_fourth_group(pattern)
            sync_version_mirrors(current, expected)
            current = expected
        manifest["version"] = current
        write_manifest(manifest)
        ensure_changelog_version(current)
        sync_package_lock(current)
        print(f"HYDRA-UMC version: v{declared} -> v{current}")
        return 0

    if current != declared:
        print(f"ERROR: native version {current} differs from manifest {declared}; run validation or use --sync after one native bump")
        return 1
    new = next_version(current)
    try:
        updated_text = replace_version(source_text, pattern, new)
    except ValueError as exc:
        print(f"ERROR: cannot write {new}: {exc}")
        return 1
    source.write_text(updated_text, encoding="utf-8")
    sync_version_mirrors(current, new)
    if new.count(".") == 3:
        manifest["native_version"]["pattern"] = with_optional_fourth_group(pattern)
    manifest["version"] = new
    write_manifest(manifest)
    ensure_changelog_version(new)
    sync_package_lock(new)
    print(f"HYDRA-UMC version: v{current} -> v{new}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
