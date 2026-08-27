@echo off
REM HYDRA-UMC-ORCHESTRATOR - run.bat
REM Copyright (C) 2026 JuanenRac (Electro Hobby 3D) <electrohobby3d@gmail.com>
REM GPL-3.0 - see LICENSE
REM
REM Runs the already-built release binary. Run build.bat first.
setlocal
cd /d "%~dp0"

REM build\ is checked first because it's the copy build.bat makes right
REM after a version bump - the canonical "last thing actually shipped".
REM target\release\ is kept as a fallback so a bare `cargo build --release`
REM (no version bump, no copy step) still runs without forcing build.bat.
if exist build\hydra-umc-orchestrator.exe (
    build\hydra-umc-orchestrator.exe
) else if exist target\release\hydra-umc-orchestrator.exe (
    target\release\hydra-umc-orchestrator.exe
) else (
    echo No compiled binary found. Run build.bat first.
    exit /b 1
)
endlocal
