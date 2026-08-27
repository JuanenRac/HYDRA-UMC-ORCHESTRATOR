@echo off
REM HYDRA-UMC-ORCHESTRATOR - build.bat
REM Copyright (C) 2026 JuanenRac (Electro Hobby 3D) <electrohobby3d@gmail.com>
REM GPL-3.0 - see LICENSE
REM
REM Bumps version.json/Cargo.toml (odometer rule) then builds a real
REM release binary with cargo. Copies the resulting exe into build/.
setlocal
cd /d "%~dp0"

echo === HYDRA-UMC-ORCHESTRATOR build ===
python bump_version.py
if errorlevel 1 ( echo NATIVE VERSION BUMP FAILED. & pause & exit /b 1 )
python "%~dp0bump_manifest_version.py" --sync
if errorlevel 1 ( echo VERSION SYNCHRONIZATION FAILED. & pause & exit /b 1 )
if errorlevel 1 (
    echo WARNING: could not bump version, continuing build anyway.
)

cargo build --release
if errorlevel 1 (
    echo BUILD FAILED.
    exit /b 1
)

if not exist build mkdir build
copy /Y target\release\hydra-umc-orchestrator.exe build\hydra-umc-orchestrator.exe >nul

echo Build OK: build\hydra-umc-orchestrator.exe
endlocal
