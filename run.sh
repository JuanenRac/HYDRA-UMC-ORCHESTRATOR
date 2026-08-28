#!/usr/bin/env bash
# HYDRA_UMC_SCRIPT_STANDARD_HEADER_BEGIN
# *****************************************************************************
# Project   : HYDRA-UMC-ORCHESTRATOR
# Script    : run.sh
# Purpose   : Runtime workflow for the project entry point.
# Author    : JuanenRac (Electro Hobby 3D)
# Email     : electrohobby3d@gmail.com
# Copyright : (C) 2026 JuanenRac
# License   : GPL-3.0 - see LICENSE
# *****************************************************************************
# HYDRA_UMC_SCRIPT_STANDARD_HEADER_END
# HYDRA_UMC_SCRIPT_STANDARD_BANNER_BEGIN
printf '\n*******************************************************************************\n'
printf '%s\n' "* HYDRA-UMC-ORCHESTRATOR - run.sh"
printf '%s\n' "* Mode      : RUN WORKFLOW"
printf '%s\n' "* Author    : JuanenRac (Electro Hobby 3D)"
printf '%s\n' "* Email     : electrohobby3d@gmail.com"
printf '%s\n' "* Copyright : (C) 2026 JuanenRac"
printf '%s\n' "* License   : GPL-3.0 - see LICENSE"
printf '%s\n' "* ------------------------------------------------------------------------- *"
printf '%s\n' "* 1. Resolve the runtime prerequisites declared by this script."
printf '%s\n' "* 2. Start the project entry point and forward user arguments unchanged."
printf '%s\n' "* 3. Preserve its result and keep an interactive terminal open."
printf '%s\n' "*******************************************************************************"
printf '\n'
# HYDRA_UMC_SCRIPT_STANDARD_BANNER_END

# HYDRA_UMC_SCRIPT_STANDARD_SAFE_PAUSE
# Prompt only in an interactive terminal: CI, pipes and service launchers never block.
hydra_umc_pause_on_exit() {
    local status=$?
    if [[ -t 0 && -t 1 ]]; then
        printf '\nPress Enter to close this window...'
        read -r _
    fi
    return "$status"
}
trap 'hydra_umc_pause_on_exit' EXIT

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
