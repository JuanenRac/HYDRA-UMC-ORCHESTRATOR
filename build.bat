@echo off
REM HYDRA_UMC_SCRIPT_STANDARD_HEADER_BEGIN
REM *****************************************************************************
REM Project   : HYDRA-UMC-ORCHESTRATOR
REM Script    : build.bat
REM Purpose   : Incremental project build, verification and packaging workflow.
REM Author    : JuanenRac (Electro Hobby 3D)
REM Email     : electrohobby3d@gmail.com
REM Copyright : (C) 2026 JuanenRac
REM License   : GPL-3.0-or-later - see LICENSE
REM *****************************************************************************
REM HYDRA_UMC_SCRIPT_STANDARD_HEADER_END
REM HYDRA_UMC_SCRIPT_STANDARD_BANNER_BEGIN
echo.
echo *****************************************************************************
echo * HYDRA-UMC-ORCHESTRATOR - build.bat
echo * Mode      : INCREMENTAL BUILD
echo * Author    : JuanenRac (Electro Hobby 3D)
echo * Email     : electrohobby3d@gmail.com
echo * Copyright : (C) 2026 JuanenRac
echo * License   : GPL-3.0-or-later - see LICENSE
echo * ------------------------------------------------------------------------- *
echo * 1. Read the project version (building never changes it; use the explicit bump command).
echo * 2. Run this project's declared build, verification and packaging commands.
echo * 3. Report the result and keep an interactive terminal open.
echo *****************************************************************************
echo.
REM HYDRA_UMC_SCRIPT_STANDARD_BANNER_END
REM
REM Builds a real
REM release binary with cargo. Copies the resulting exe into build/.
setlocal
rem Keep the window open for people, never in automation (CI / HYDRA_UMC_CI).
set "KEEP_OPEN=pause"
if defined CI set "KEEP_OPEN=ver >nul"
if "%HYDRA_UMC_CI%"=="1" set "KEEP_OPEN=ver >nul"
cd /d "%~dp0"

echo === HYDRA-UMC-ORCHESTRATOR build ===
REM HYDRA_UMC_SCRIPT_STANDARD_VERSION_STEP
echo [1/3] Reading the project version (building never changes it)...
for /f "usebackq delims=" %%V in (`python -c "import json; print(json.load(open(r'%~dp0hydra-umc.project.json', encoding='utf-8'))['version'])"`) do set "HYDRA_UMC_VERSION_BUILT=%%V"
if not defined HYDRA_UMC_VERSION_BUILT set "HYDRA_UMC_VERSION_BUILT=unknown"
echo       Building v%HYDRA_UMC_VERSION_BUILT% (building never changes the version)
echo.
echo.
cargo build --release
if errorlevel 1 (
    echo BUILD FAILED.
    %KEEP_OPEN%
    exit /b 1
)

if not exist build mkdir build
copy /Y target\release\hydra-umc-orchestrator.exe build\hydra-umc-orchestrator.exe >nul

echo Build OK: build\hydra-umc-orchestrator.exe
echo Built v%HYDRA_UMC_VERSION_BUILT%. Building never changes the version; to raise it run: python tools\hydra_project_tool.py bump
echo.
endlocal

REM HYDRA_UMC_SCRIPT_STANDARD_SAFE_PAUSE
set "HYDRA_UMC_SCRIPT_RESULT=%ERRORLEVEL%"
echo.
echo [INFO] Script completed. Exit code: %HYDRA_UMC_SCRIPT_RESULT%.
%KEEP_OPEN%
exit /b %HYDRA_UMC_SCRIPT_RESULT%
