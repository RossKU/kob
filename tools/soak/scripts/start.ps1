# Starts the soak supervisor detached (hidden window): miner, both executors, bots, checker.
#   powershell -File tools/soak/scripts/start.ps1 [-Only miner,exec-a,exec-b]
param([string]$Only = "")
$soak = Split-Path -Parent $PSScriptRoot
$run = Join-Path $soak "run"
$pidFile = Join-Path $run "supervisor.pid"
if (Test-Path $pidFile) {
  $old = Get-Content $pidFile
  if (Get-Process -Id $old -ErrorAction SilentlyContinue) { Write-Output "supervisor already running (pid $old)"; exit 0 }
}
$argList = @("$soak\scripts\supervisor.mjs", "--config", "$run\config.json")
if ($Only) { $argList += @("--only", $Only) }
New-Item -ItemType Directory -Force (Join-Path $run "logs") | Out-Null
$p = Start-Process -FilePath "node" -ArgumentList $argList -WindowStyle Hidden -PassThru `
  -RedirectStandardOutput (Join-Path $run "logs\supervisor.out.log") -RedirectStandardError (Join-Path $run "logs\supervisor.err.log")
Write-Output "supervisor started (pid $($p.Id)); status: powershell -File $soak\scripts\status.ps1"
