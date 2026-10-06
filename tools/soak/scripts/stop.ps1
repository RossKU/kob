# Stops the soak: asks the supervisor to stop every child (run/STOP), then force-kills whatever is left.
$soak = Split-Path -Parent $PSScriptRoot
$run = Join-Path $soak "run"
New-Item -ItemType File -Force (Join-Path $run "STOP") | Out-Null
$pidFile = Join-Path $run "supervisor.pid"
for ($i = 0; $i -lt 20; $i++) {
  if (-not (Test-Path $pidFile)) { break }
  Start-Sleep -Milliseconds 500
}
if (Test-Path $pidFile) {
  $sp = Get-Content $pidFile
  & taskkill /PID $sp /T /F | Out-Null
  Remove-Item $pidFile -Force
}
Remove-Item (Join-Path $run "STOP") -Force -ErrorAction SilentlyContinue
Write-Output "soak stopped"
