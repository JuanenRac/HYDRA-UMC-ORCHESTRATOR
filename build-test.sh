#!/usr/bin/env bash
# HYDRA_UMC_SCRIPT_STANDARD_HEADER_BEGIN
# *****************************************************************************
# Project   : HYDRA-UMC-ORCHESTRATOR
# Script    : build-test.sh
# Purpose   : Non-mutating build validation; manifest and CHANGELOG stay unchanged.
# Author    : JuanenRac (Electro Hobby 3D)
# Email     : electrohobby3d@gmail.com
# Copyright : (C) 2026 JuanenRac
# License   : GPL-3.0 - see LICENSE
# *****************************************************************************
# HYDRA_UMC_SCRIPT_STANDARD_HEADER_END
# HYDRA_UMC_SCRIPT_STANDARD_BANNER_BEGIN
printf '\n*******************************************************************************\n'
printf '%s\n' "* HYDRA-UMC-ORCHESTRATOR - build-test.sh"
printf '%s\n' "* Mode      : NON-MUTATING BUILD TEST"
printf '%s\n' "* Author    : JuanenRac (Electro Hobby 3D)"
printf '%s\n' "* Email     : electrohobby3d@gmail.com"
printf '%s\n' "* Copyright : (C) 2026 JuanenRac"
printf '%s\n' "* License   : GPL-3.0 - see LICENSE"
printf '%s\n' "* ------------------------------------------------------------------------- *"
printf '%s\n' "* 1. Run the project's build validation command."
printf '%s\n' "* 2. Do not change the project version, manifest or CHANGELOG."
printf '%s\n' "* 3. Report the result and keep an interactive terminal open."
printf '%s\n' "*******************************************************************************"
printf '\n'
# HYDRA_UMC_SCRIPT_STANDARD_BANNER_END
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