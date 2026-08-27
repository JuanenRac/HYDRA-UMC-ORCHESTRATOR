#!/usr/bin/env bash
# HYDRA-UMC-ORCHESTRATOR - build.sh
# Copyright (C) 2026 JuanenRac (Electro Hobby 3D) <electrohobby3d@gmail.com>
# GPL-3.0 - see LICENSE
#
# Bumps Cargo.toml (odometer rule) then builds a real release binary
# with cargo. Copies the resulting binary into build/.
set -euo pipefail
python3 "$(dirname "$0")/bump_manifest_version.py" || exit 1
cd "$(dirname "$0")"

echo "=== HYDRA-UMC-ORCHESTRATOR build ==="
python3 bump_version.py || echo "WARNING: could not bump version, continuing build anyway."

cargo build --release

mkdir -p build
cp -f target/release/hydra-umc-orchestrator build/hydra-umc-orchestrator 2>/dev/null || \
    cp -f target/release/hydra-umc-orchestrator.exe build/hydra-umc-orchestrator.exe

echo "Build OK: build/hydra-umc-orchestrator"
