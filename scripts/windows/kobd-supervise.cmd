@echo off
rem kobd-supervise.cmd: runs kobd.exe (next to this file) and starts it again when it exits with an error, as systemd's
rem Restart=always does on Linux (docs/ops/executor-quickstart.md, section 6). The arguments go to kobd.exe unchanged:
rem
rem     kobd-supervise.cmd --executor
rem
rem   * exit status 0 (a stop: Ctrl+C), 2 (a stop that did not finish in time, or a wrong option) or 130 (a second Ctrl+C):
rem     not started again;
rem   * any other status (1: the indexer cannot write its database or the read API stopped, and the executor exits so that it
rem     is restarted; a crash): started again after 10 s, the wait doubling to 5 min while it keeps failing within 10 min of
rem     its start;
rem   * 5 starts in a row that each ended within 30 s: not started again. That is a problem a restart does not cure (another
rem     kobd holds the data directory, the key file is refused, a configuration error): kobd.log says which;
rem   * the file kobd-supervise.stop next to this one: not started again (create it, then stop kobd: Ctrl+C, or under Task
rem     Scheduler `taskkill /f /pid <pid>` with the pid in kob-data\kob-writer.owner; `taskkill /f /im kobd.exe` would end
rem     every kobd.exe of the machine, another network's too). It is deleted when the script starts.
rem
rem Ctrl+C in this window ends kobd (status 0, not started again) and then cmd asks "Terminate batch job (Y/N)?": either
rem answer is harmless.
rem
rem Each restart is noted in kobd-supervise.log next to this file; why kobd stopped is in its own log (kobd.log).
setlocal EnableExtensions DisableDelayedExpansion
cd /d "%~dp0"
set "KOBD=%~dp0kobd.exe"
set "SUPLOG=%~dp0kobd-supervise.log"
set "STOPFILE=%~dp0kobd-supervise.stop"
if exist "%STOPFILE%" del "%STOPFILE%"
set DELAY=10
set RAPID=0
:run
call :now START
"%KOBD%" %*
set CODE=%ERRORLEVEL%
if "%CODE%"=="0" goto done
if "%CODE%"=="2" goto done
if "%CODE%"=="130" goto done
if exist "%STOPFILE%" goto stopped
call :now END
set /a RAN=END-START
rem a run of 10 minutes or more was a good one: the next failure waits 10 s again
if %RAN% GEQ 600 set DELAY=10
rem a start that ends within 30 s, five times in a row, is not cured by starting again
set FAST=0
if not "%START%"=="0" if %RAN% LSS 30 set FAST=1
if %FAST%==1 (set /a RAPID=RAPID+1) else (set RAPID=0)
if %RAPID% GEQ 5 goto rapid
echo %DATE% %TIME% kobd exited with status %CODE% after %RAN% s; starting it again in %DELAY% s>>"%SUPLOG%"
echo kobd exited with status %CODE%; starting it again in %DELAY% s (Ctrl+C to stop).
rem ping waits without a console too (timeout.exe refuses to run when its input is not one, as under Task Scheduler)
set /a PINGS=DELAY+1
ping -n %PINGS% 127.0.0.1 >nul
if exist "%STOPFILE%" goto stopped
set /a DELAY=DELAY*2
if %DELAY% GTR 300 set DELAY=300
goto run
:rapid
echo %DATE% %TIME% kobd failed 5 times in a row within 30 s of its start (last status %CODE%): a problem a restart does not cure (another kobd holds the data directory, the key file is refused, a configuration error; see kobd.log). Not started again: fix it, then start this script again>>"%SUPLOG%"
echo kobd failed 5 times in a row within 30 s of its start: not started again. See kobd.log, fix the problem, then start this script again.
rem a double-clicked window would close at once: keep it for a minute (timeout.exe refuses to wait without a console, as
rem under Task Scheduler, so a scheduled run ends at once)
echo This window closes in 60 s (press a key to close it now).
timeout /t 60 >nul 2>&1
exit /b %CODE%
:stopped
echo %DATE% %TIME% kobd exited with status %CODE% and kobd-supervise.stop is there: not started again>>"%SUPLOG%"
exit /b %CODE%
:done
echo %DATE% %TIME% kobd exited with status %CODE%: not started again>>"%SUPLOG%"
exit /b %CODE%

rem The Unix time in seconds into the variable named %1 (0 when PowerShell cannot be asked: the wait then keeps doubling, and
rem no start counts as rapid).
:now
set "%~1=0"
for /f "usebackq delims=" %%s in (`powershell -NoProfile -NonInteractive -Command "[DateTimeOffset]::UtcNow.ToUnixTimeSeconds()" 2^>nul`) do set "%~1=%%s"
exit /b 0
