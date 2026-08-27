#!/usr/bin/env bash
# HYDRA-UMC-ORCHESTRATOR - run.sh
# Copyright (C) 2026 JuanenRac (Electro Hobby 3D) <electrohobby3d@gmail.com>
# GPL-3.0 - see LICENSE
#
# Runs the already-built release binary. Run build.sh first.
set -euo pipefail
cd "$(dirname "$0")"

# build/ is checked first because it's the copy build.sh makes right after
# a version bump - the canonical "last thing actually shipped". target/
# release/ is kept as a fallback so a bare `cargo build --release` (no
# version bump, no copy step) still runs without forcing build.sh. Both
# the extension-less (Linux/macOS) and .exe (Windows, e.g. under WSL/Git
# Bash) binary names are checked since this same script is meant to work
# in both environments.
if [ -x build/hydra-umc-orchestrator ]; then
    exec build/hydra-umc-orchestrator
elif [ -x target/release/hydra-umc-orchestrator ]; then
    exec target/release/hydra-umc-orchestrator
elif [ -x build/hydra-umc-orchestrator.exe ]; then
    exec build/hydra-umc-orchestrator.exe
elif [ -x target/release/hydra-umc-orchestrator.exe ]; then
    exec target/release/hydra-umc-orchestrator.exe
else
    echo "No compiled binary found. Run build.sh first." >&2
    exit 1
fi
