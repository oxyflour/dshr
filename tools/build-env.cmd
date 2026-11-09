@echo off
rem Deterministic Windows build environment for this repository.
rem Why: the user's %TEMP% is not writable for linker/compiler child processes on this
rem machine (LNK1104 on lnk{GUID}.tmp, cc D8050). Redirecting TMP/TEMP into the repo
rem makes cargo/cc/link behave deterministically. See docs/m0-runbook.md and ADR-0002.
rem
rem Usage:  tools\build-env.cmd cargo test -p dshr-agent-core
setlocal
set "ROOT=%~dp0.."
set "TMP=%ROOT%\.tmp"
set "TEMP=%ROOT%\.tmp"
if not exist "%TMP%" mkdir "%TMP%" >nul 2>&1
set "PATH=%USERPROFILE%\.cargo\bin;%PATH%"

set "VCVARS=C:\Program Files\Microsoft Visual Studio\2022\Community\VC\Auxiliary\Build\vcvars64.bat"
if exist "%VCVARS%" call "%VCVARS%" >nul

%*
exit /b %ERRORLEVEL%
