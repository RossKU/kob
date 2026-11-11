@echo off
rem kobd-supervise.cmd: runs kobd.exe (next to this file) and starts it again when it exits with an error, as systemd's
rem Restart=always does on Linux (docs/ops/executor-quickstart.md, section 6). The arguments go to kobd.exe unchanged:
rem
rem     kobd-supervise.cmd --executor
rem
rem   * exit status 0 (a stop: Ctrl+C) or 2 (a stop that did not finish in time, or a wrong option): not started again;
rem   * any other status (1: the indexer cannot write its database or the read API stopped, and the executor exits so that it
rem     is restarted; a crash): started again after 10 s, the wait doubling to 5 min while it keeps failing within 10 min of
rem     its start.
rem
rem Each restart is noted in kobd-supervise.log next to this file; why kobd stopped is in its own log (kobd.log).
setlocal EnableExtensions DisableDelayedExpansion
cd /d "%~dp0"
set "KOBD=%~dp0kobd.exe"
set "SUPLOG=%~dp0kobd-supervise.log"
set DELAY=10
:run
call :now START
"%KOBD%" %*
set CODE=%ERRORLEVEL%
if "%CODE%"=="0" goto done
if "%CODE%"=="2" goto done
call :now END
set /a RAN=END-START
rem a run of 10 minutes or more was a good one: the next failure waits 10 s again
if %RAN% GEQ 600 set DELAY=10
echo %DATE% %TIME% kobd exited with status %CODE% after %RAN% s; starting it again in %DELAY% s>>"%SUPLOG%"
echo kobd exited with status %CODE%; starting it again in %DELAY% s (Ctrl+C to stop).
rem ping waits without a console too (timeout.exe refuses to run when its input is not one, as under Task Scheduler)
set /a PINGS=DELAY+1
ping -n %PINGS% 127.0.0.1 >nul
set /a DELAY=DELAY*2
if %DELAY% GTR 300 set DELAY=300
goto run
:done
echo %DATE% %TIME% kobd exited with status %CODE%: not started again>>"%SUPLOG%"
exit /b %CODE%

rem The Unix time in seconds into the variable named %1 (0 when PowerShell cannot be asked: the wait then keeps doubling).
:now
set "%~1=0"
for /f "usebackq delims=" %%s in (`powershell -NoProfile -NonInteractive -Command "[DateTimeOffset]::UtcNow.ToUnixTimeSeconds()" 2^>nul`) do set "%~1=%%s"
exit /b 0
