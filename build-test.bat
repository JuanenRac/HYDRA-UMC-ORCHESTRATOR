@echo off
REM HYDRA_UMC_SCRIPT_STANDARD_HEADER_BEGIN
REM *****************************************************************************
REM Project   : HYDRA-UMC-ORCHESTRATOR
REM Script    : build-test.bat
REM Purpose   : Non-mutating build validation; manifest and CHANGELOG stay unchanged.
REM Author    : JuanenRac (Electro Hobby 3D)
REM Email     : electrohobby3d@gmail.com
REM Copyright : (C) 2026 JuanenRac
REM License   : GPL-3.0 - see LICENSE
REM *****************************************************************************
REM HYDRA_UMC_SCRIPT_STANDARD_HEADER_END
REM HYDRA_UMC_SCRIPT_STANDARD_BANNER_BEGIN
echo.
echo *****************************************************************************
echo * HYDRA-UMC-ORCHESTRATOR - build-test.bat
echo * Mode      : NON-MUTATING BUILD TEST
echo * Author    : JuanenRac (Electro Hobby 3D)
echo * Email     : electrohobby3d@gmail.com
echo * Copyright : (C) 2026 JuanenRac
echo * License   : GPL-3.0 - see LICENSE
echo * ------------------------------------------------------------------------- *
echo * 1. Run the project's build validation command.
echo * 2. Do not change the project version, manifest or CHANGELOG.
echo * 3. Report the result and keep an interactive terminal open.
echo *****************************************************************************
echo.
REM HYDRA_UMC_SCRIPT_STANDARD_BANNER_END
REM Runs the non-versioning build check. It does not update the manifest or CHANGELOG.
setlocal
cd /d "%~dp0"
where py >nul 2>&1
if errorlevel 1 (
    python tools\build_test.py
) else (
    py -3 tools\build_test.py
)
if errorlevel 1 (
    set "RESULT=1"
) else (
    set "RESULT=0"
)
echo.
pause
exit /b %RESULT%