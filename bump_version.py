#!/usr/bin/env python3
# =============================================================================
# HYDRA-UMC-ORCHESTRATOR - bump_version.py
# Copyright (C) 2026 JuanenRac (Electro Hobby 3D) <electrohobby3d@gmail.com>
# GPL-3.0 - see LICENSE
#
# Ecosystem-wide versioning policy: the version bumps automatically on
# every REAL build (every run of build.bat/build.sh - the only thing
# that counts as a "build" for a Rust binary; just running
# `cargo run` directly does NOT bump anything), using an odometer/
# mileage-counter rule in base 10: the last component (patch) goes up
# by 1; if it would roll past 9, it resets to 0 and carries 1 into the
# component to its left (minor), which repeats the same rule leftward
# (major has no component further left, so it just keeps counting past
# 9 with no reset - same as a real odometer's leftmost digit never
# wrapping back to 0 mid-drive).
# Example: 0.1.9 -> 0.2.0. Another: 0.9.9 -> 0.0.0.
#
# Called by build.bat/build.sh BEFORE `cargo build --release` runs, so
# every packaged binary carries a version strictly newer than the last
# one actually shipped (Cargo picks it up automatically via
# env!("CARGO_PKG_VERSION") in src/main.rs). Standalone-runnable too
# (invoked twice in a row is how this script's own bump logic gets
# verified without needing a full cargo build each time).
# =============================================================================
from __future__ import annotations

import re
import sys
from pathlib import Path

CARGO_TOML = Path(__file__).resolve().parent / "Cargo.toml"
VERSION_RE = re.compile(r'^version\s*=\s*"([^"]+)"\s*$', re.MULTILINE)


def bump(version: str) -> str:
    """Applies the base-10 odometer/carry rule to a MAJOR.MINOR.PATCH
    string and returns the next version string."""
    parts = [int(p) for p in version.split(".")]
    i = len(parts) - 1
    parts[i] += 1
    # Carry: any component past the leftmost one wraps at 10 -> 0 and
    # bumps its left neighbor. The leftmost component (major) never
    # wraps - it just keeps counting, matching a real odometer.
    while i > 0 and parts[i] > 9:
        parts[i] = 0
        parts[i - 1] += 1
        i -= 1
    return ".".join(str(p) for p in parts)


def main() -> int:
    if not CARGO_TOML.is_file():
        print(f"ERROR: {CARGO_TOML} not found - cannot bump version.", file=sys.stderr)
        return 1

    text = CARGO_TOML.read_text(encoding="utf-8")
    match = VERSION_RE.search(text)
    if not match:
        print(f'ERROR: no version = "X.Y.Z" line found in {CARGO_TOML}.', file=sys.stderr)
        return 1

    old_version = match.group(1)
    new_version = bump(old_version)
    new_text = text[: match.start()] + f'version = "{new_version}"' + text[match.end() :]
    CARGO_TOML.write_text(new_text, encoding="utf-8")

    print(f"Version bumped: {old_version} -> {new_version}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
