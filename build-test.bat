@echo off
REM =============================================================================
REM HYDRA-UMC-ORCHESTRATOR - build-test.bat
REM Copyright (C) 2026 JuanenRac (Electro Hobby 3D) ^<electrohobby3d@gmail.com^>
REM GPL-3.0 - see LICENSE
REM =============================================================================
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