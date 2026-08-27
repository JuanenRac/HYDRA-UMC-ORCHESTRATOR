#!/usr/bin/env bash
# HYDRA-UMC-ORCHESTRATOR - build.sh
# Copyright (C) 2026 JuanenRac (Electro Hobby 3D) <electrohobby3d@gmail.com>
# GPL-3.0 - see LICENSE
#
# Bumps Cargo.toml (odometer rule) then builds a real release binary
# with cargo. Copies the resulting binary into build/.
set -euo pipefail
cd "$(dirname "$0")"

echo "=== HYDRA-UMC-ORCHESTRATOR build ==="
# Bump the real, native version FIRST, then sync the manifest to match
# (--sync) - never the other way around, or bump_manifest_version.py's
# own no-flag path bumps native+manifest together and this next line
# bumps native a second time, leaving it one step ahead of the manifest
# (same fix already applied to HYDRA-UMC-HIL-BRIDGE and
# HYDRA-UMC-NODE-HEALING's build.sh).
python3 bump_version.py || exit 1
python3 bump_manifest_version.py --sync || exit 1

cargo build --release

mkdir -p build
cp -f target/release/hydra-umc-orchestrator build/hydra-umc-orchestrator 2>/dev/null || \
    cp -f target/release/hydra-umc-orchestrator.exe build/hydra-umc-orchestrator.exe

echo "Build OK: build/hydra-umc-orchestrator"
