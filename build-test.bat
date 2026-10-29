@echo off
rem PRETTY_RUN_BEGIN
if not defined PRETTY_MODE set "PRETTY_MODE=build-test"
set "PRETTY_PY=python"
where py >nul 2>&1 && set "PRETTY_PY=py -3"
if not defined PRETTY_RUN if exist "%~dp0tools\pretty_run.py" (
    set "PRETTY_RUN=1"
    %PRETTY_PY% "%~dp0tools\pretty_run.py" --wrap --mode %PRETTY_MODE% -- cmd /d /c "%~f0" %*
    exit /b
)
rem PRETTY_RUN_END
REM HYDRA_UMC_SCRIPT_STANDARD_HEADER_BEGIN
REM *****************************************************************************
REM Project   : HYDRA-UMC-ORCHESTRATOR
REM Script    : build-test.bat
REM Purpose   : Non-mutating build validation; manifest and CHANGELOG stay unchanged.
REM Author    : JuanenRac (Electro Hobby 3D)
REM Email     : electrohobby3d@gmail.com
REM Copyright : (C) 2026 JuanenRac
REM License   : GPL-3.0-or-later - see LICENSE
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
echo * License   : GPL-3.0-or-later - see LICENSE
echo * ------------------------------------------------------------------------- *
echo * 1. Run the project's build validation command.
echo * 2. Do not change the project version, manifest or CHANGELOG.
echo * 3. Report the result and keep an interactive terminal open.
echo *****************************************************************************
echo.
REM HYDRA_UMC_SCRIPT_STANDARD_BANNER_END
REM Runs the non-versioning build check. It does not update the manifest or CHANGELOG.
setlocal
rem Keep the window open for people, never in automation (CI / HYDRA_UMC_CI).
set "KEEP_OPEN=pause"
if defined CI set "KEEP_OPEN=ver >nul"
if "%HYDRA_UMC_CI%"=="1" set "KEEP_OPEN=ver >nul"
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
%KEEP_OPEN%
exit /b %RESULT%