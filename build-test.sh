#!/usr/bin/env bash
# =============================================================================
# HYDRA-UMC-ORCHESTRATOR - build-test.sh
# Copyright (C) 2026 JuanenRac (Electro Hobby 3D) <electrohobby3d@gmail.com>
# GPL-3.0 - see LICENSE
# =============================================================================
# Runs the non-versioning build check. It does not update the manifest or CHANGELOG.
set -uo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
python3 "$ROOT/tools/build_test.py"
status=$?
echo
if [ -t 0 ]; then
    read -r -p "Press Enter to close this window..." _
fi
exit "$status"