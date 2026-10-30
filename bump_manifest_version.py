#!/usr/bin/env python3
# =============================================================================
# HYDRA-UMC - Manifest-backed build version utility
# Copyright (C) 2026 JuanenRac (Electro Hobby 3D) <electrohobby3d@gmail.com>
# GPL-3.0-or-later - see LICENSE
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


_THREE_GROUPS = re.compile(r"(\([^()]*\))\\.(\([^()]*\))\\.(\([^()]*\))(?!\(\?:\\[.+])")


def with_optional_fourth_group(pattern: str, separator: str = r"\.") -> str:
    """Let a native-version pattern also match a fourth `.N` component.

    Patterns are written for `MAJOR.MINOR.PATCH`; the same text with an
    optional fourth group behind the third one reads both forms. A pattern that
    already has it is returned unchanged. Patterns that keep the components in
    separate fields (a mapping) never reach this function.
    """
    match = _THREE_GROUPS.search(pattern)
    if match is None:
        return pattern
    return pattern[: match.end()] + "(?:" + separator + match.group(3) + ")?" + pattern[match.end() :]


# Some native files cannot hold a fourth component. The manifest and the changelog always keep all four;
# the native file receives a fixed translation of them:
#   plain   a.b.c.d  ->  a.b.c.d        (Python, Go, properties files ...)
#   cargo   a.b.c.d  ->  a.b.c+d        (Cargo.toml and Cargo.lock: the fourth component is semver build metadata)
#   pubspec a.b.c.d  ->  a.b.c+N        (pubspec.yaml: the fourth component is not stored; +N is the build number,
#                                        a plain counter raised by one on every bump)
#   npm     a.b.c.d  ->  a.b.c          (package.json and package-lock.json: npm only accepts semver, so the fourth
#                                        component is not stored there either)
# Reading goes the other way, so a cargo file reads back as four components and a pubspec as three. A package.json
# that still holds all four components (written before this rule) is accepted until the next bump rewrites it.
def native_style(source: Path) -> str:
    return {"Cargo.toml": "cargo", "pubspec.yaml": "pubspec", "package.json": "npm"}.get(source.name, "plain")


def to_native(version: str, style: str) -> str:
    parts = version.split(".")
    if len(parts) < 4 or style == "plain":
        return version
    if style == "cargo":
        return ".".join(parts[:3]) + "+" + parts[3]
    if style in ("pubspec", "npm"):
        return ".".join(parts[:3])
    raise ValueError(f"unknown native version style: {style}")


def same_version(native: str, declared: str, style: str) -> bool:
    """Whether the version read from the native file is the manifest one, as that file can express it."""
    if style == "npm":
        return native == declared or native == to_native(declared, style)
    return to_native(declared, style) == to_native(native, style) if style == "pubspec" else native == declared


def style_separator(style: str) -> str:
    return r"\+" if style == "cargo" else r"\."


def bump_pubspec_build(text: str) -> str:
    """Raise the `+N` build number of `version: a.b.c+N` by one (a monotonic counter, no carry)."""
    return re.sub(r"(?m)^(version:\s*\d+\.\d+\.\d+\+)(\d+)", lambda m: m.group(1) + str(int(m.group(2)) + 1), text, count=1)


def sync_cargo_lock(version: str, cargo_text: str) -> bool:
    """Carry the package's own entry in Cargo.lock along with Cargo.toml (the lock names the package first)."""
    lock_path = ROOT / "Cargo.lock"
    name = re.search(r'(?ms)^\[package\].*?^name\s*=\s*"([^"]+)"', cargo_text)
    if not lock_path.is_file() or name is None:
        return False
    with open(lock_path, encoding="utf-8", newline="") as handle:
        raw = handle.read()
    eol = r'\r?\n'
    pattern = re.compile(r'(?m)(^name = ' + re.escape('"' + name.group(1) + '"') + eol + r'version = ")[^"]*(")')
    new_raw = pattern.sub(lambda m: m.group(1) + version + m.group(2), raw, count=1)
    if new_raw == raw:
        return False
    lock_path.write_text(new_raw, encoding="utf-8", newline="")
    return True


def sync_app_version_dart(pubspec_text: str) -> bool:
    """Regenerate lib/app_version.dart (the constants the app shows) from pubspec.yaml, when the app has that file."""
    target = ROOT / "lib" / "app_version.dart"
    match = re.search(r"(?m)^version:\s*(\d+\.\d+\.\d+)\+(\d+)", pubspec_text)
    if not target.is_file() or match is None:
        return False
    name, build = match.group(1), match.group(2)
    old = target.read_text(encoding="utf-8")
    new = re.sub(r"const String kAppVersion = '[^']*';", f"const String kAppVersion = '{name}';", old)
    new = re.sub(r"const int kAppBuildNumber = \d+;", f"const int kAppBuildNumber = {build};", new)
    new = re.sub(r"const String kAppVersionFull = '[^']*';", f"const String kAppVersionFull = '{name}+{build}';", new)
    if new == old:
        return False
    target.write_text(new, encoding="utf-8", newline="")
    return True


def widen_pattern(pattern: str | dict[str, str], style: str = "plain") -> str | dict[str, str]:
    """The pattern to store once the version has four components (a pubspec keeps three: see above)."""
    if isinstance(pattern, dict) or style == "pubspec":
        return pattern
    return with_optional_fourth_group(pattern, style_separator(style))


def read_version(text: str, pattern: str | dict[str, str], style: str = "plain") -> str:
    if isinstance(pattern, dict):
        fields: dict[str, str] = {}
        for key in ("major", "minor", "patch"):
            match = re.search(pattern[key], text, re.MULTILINE)
            if match is None:
                raise ValueError(f"native version {key} component was not found")
            fields[key] = match.group(1)
        version = f"{fields['major']}.{fields['minor']}.{fields['patch']}"
        # A separate field for the fourth component only counts from the
        # version that carries one; below it a stored 0 is just a placeholder.
        if "build" in pattern and tuple(int(fields[k]) for k in ("major", "minor", "patch")) >= FOUR_PART_FROM:
            build = re.search(pattern["build"], text, re.MULTILINE)
            if build is not None:
                version += "." + build.group(1)
        return version
    widened = pattern if style == "pubspec" else with_optional_fourth_group(pattern, style_separator(style))
    match = re.search(widened, text, re.MULTILINE)
    if match is None or len(match.groups()) < 3:
        raise ValueError("native version was not found")
    parts = [match.group(index) for index in (1, 2, 3)]
    if len(match.groups()) >= 4 and match.group(4) is not None:
        parts.append(match.group(4))
    return ".".join(parts)


def replace_version(text: str, pattern: str | dict[str, str], version: str, style: str = "plain") -> str:
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
    widened = pattern if style == "pubspec" else with_optional_fourth_group(pattern, style_separator(style))
    match = re.search(widened, text, re.MULTILINE)
    if match is None or len(match.groups()) < 3:
        raise ValueError("native version was not found")
    version = to_native(version, style)
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
    version = to_native(version, "npm")  # npm accepts three components only
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


def finish(source: Path, style: str, version: str, text: str) -> None:
    """Everything that follows the native file: the lock file and the generated constants."""
    if style == "cargo":
        sync_cargo_lock(to_native(version, style), text)
    elif style == "pubspec":
        sync_app_version_dart(text)


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
        style = native_style(source)
        source_text = source.read_text(encoding="utf-8", errors="replace")
        current = read_version(source_text, pattern, style)
    except (OSError, KeyError, TypeError, json.JSONDecodeError, ValueError) as exc:
        print(f"ERROR: {exc}")
        return 1

    if args.sync:
        if same_version(current, declared, style):
            added = ensure_changelog_version(declared)
            if added:
                print(f"Changelog synchronized: {declared}")
            sync_package_lock(declared)
            finish(source, style, declared, source_text)
            print(f"HYDRA-UMC version: v{declared} -> v{declared} (already synchronized)")
            return 0
        expected = next_version(declared)
        if not same_version(current, expected, style):
            if expected != current + ".0":
                print(f"ERROR: native version {current} is not the next version after manifest {declared}")
                return 1
            # A per-project bump script produced the last three-part version;
            # the ecosystem rule appends the fourth component.
            source_text = replace_version(source_text, pattern, expected, style)
            source.write_text(source_text, encoding="utf-8")
            manifest["native_version"]["pattern"] = widen_pattern(pattern, style)
            sync_version_mirrors(current, expected)
        current = expected
        manifest["version"] = current
        write_manifest(manifest)
        ensure_changelog_version(current)
        sync_package_lock(current)
        finish(source, style, current, source_text)
        print(f"HYDRA-UMC version: v{declared} -> v{current}")
        return 0

    if not same_version(current, declared, style):
        print(f"ERROR: native version {current} differs from manifest {declared}; run validation or use --sync after one native bump")
        return 1
    new = next_version(declared)
    try:
        updated_text = replace_version(source_text, pattern, new, style)
    except ValueError as exc:
        print(f"ERROR: cannot write {new}: {exc}")
        return 1
    if style == "pubspec":
        updated_text = bump_pubspec_build(updated_text)
    source.write_text(updated_text, encoding="utf-8")
    sync_version_mirrors(current, new)
    if new.count(".") == 3:
        manifest["native_version"]["pattern"] = widen_pattern(pattern, style)
    manifest["version"] = new
    write_manifest(manifest)
    ensure_changelog_version(new)
    sync_package_lock(new)
    finish(source, style, new, updated_text)
    print(f"HYDRA-UMC version: v{declared} -> v{new}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
