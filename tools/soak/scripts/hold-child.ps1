# Holds one child of the running soak supervisor down (the others keep running): creates run/HOLD-<name>; the supervisor stops that child
# within 2 s and does not start it again until the hold is released (-Release removes the file; the child then starts with its spec rebuilt
# on the next restart request, or as it was). A deploy step's way to keep a child down (a database carried, a bundle or binary swapped),
# instead of renaming its bundle or pointing its binary at one that exits at once (each of those is a crash loop to the supervisor).
#   powershell -File tools/soak/scripts/hold-child.ps1 -Name exec-b
#   powershell -File tools/soak/scripts/hold-child.ps1 -Name exec-b -Release
# Never hold both executors at once while the soak trades (one indexer and matcher must keep running).
param([Parameter(Mandatory = $true)][string]$Name, [switch]$Release)
$soak = Split-Path -Parent $PSScriptRoot
$run = Join-Path $soak "run"
$pidFile = Join-Path $run "supervisor.pid"
if (-not ((Test-Path $pidFile) -and (Get-Process -Id (Get-Content $pidFile) -ErrorAction SilentlyContinue))) {
  Write-Output "supervisor not running: use scripts/start.ps1"; exit 1
}
$hold = Join-Path $run "HOLD-$Name"
if ($Release) {
  Remove-Item -Force -ErrorAction SilentlyContinue $hold
  Write-Output "hold of '$Name' released (run/logs/supervisor.log shows 'started')"
  exit 0
}
if ($Name -like 'exec-*') {
  $other = Get-ChildItem -Path $run -Filter "HOLD-exec-*" -ErrorAction SilentlyContinue | Where-Object { $_.Name -ne "HOLD-$Name" }
  if ($other) { Write-Output "refused: $($other.Name) is held; release it first (never both executors down)"; exit 1 }
}
New-Item -ItemType File -Force $hold | Out-Null
Write-Output "hold of '$Name' requested (run/logs/supervisor.log shows 'hold' / 'held')"
