@echo off
REM HYDRA-UMC-ORCHESTRATOR test launcher: the tests and the checks of build-test, shown in a live panel; the version is never changed. GPL-3.0-or-later.
set "PRETTY_MODE=test"
call "%~dp0build-test.bat" %*
exit /b %ERRORLEVEL%
