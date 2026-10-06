# Restarts one child of the running soak supervisor (the others keep running): creates run/RESTART-<name>; the supervisor kills that
# child within 2 s and starts it again with its spec rebuilt from a fresh read of run/config.json (e.g. the miner after a binary swap or
# an edit of the `miner` block).
#   powershell -File tools/soak/scripts/restart-child.ps1 -Name miner
param([Parameter(Mandatory = $true)][string]$Name)
$soak = Split-Path -Parent $PSScriptRoot
$run = Join-Path $soak "run"
$pidFile = Join-Path $run "supervisor.pid"
if (-not ((Test-Path $pidFile) -and (Get-Process -Id (Get-Content $pidFile) -ErrorAction SilentlyContinue))) {
  Write-Output "supervisor not running: use scripts/start.ps1"; exit 1
}
New-Item -ItemType File -Force (Join-Path $run "RESTART-$Name") | Out-Null
Write-Output "restart of '$Name' requested (run/logs/supervisor.log shows 'restart requested' / 'started')"
