# Soak status: supervisor children, executor health, the latest report.
$soak = Split-Path -Parent $PSScriptRoot
$run = Join-Path $soak "run"
$pidFile = Join-Path $run "supervisor.pid"
if ((Test-Path $pidFile) -and (Get-Process -Id (Get-Content $pidFile) -ErrorAction SilentlyContinue)) {
  Write-Output "supervisor: running (pid $(Get-Content $pidFile))"
} else { Write-Output "supervisor: NOT running" }
if (Test-Path "$run\supervisor.json") {
  $s = Get-Content "$run\supervisor.json" | ConvertFrom-Json
  # restarts split by cause (scripts/restarts.mjs over supervisor.log): requested / held, start failures (a deploy step: missing bundle or
  # binary), crashes (an exit nobody asked for)
  $why = $null
  try { $why = node (Join-Path $PSScriptRoot "restarts.mjs") (Join-Path $run "logs\supervisor.log") | ConvertFrom-Json } catch { }
  $s.children | ForEach-Object {
    $w = if ($why) { $why.($_.name) } else { $null }
    [pscustomobject]@{
      name = $_.name; running = $_.running; pid = $_.pid; restarts = $_.restarts
      requested = if ($w) { $w.requested } else { '' }
      startFail = if ($w) { $w.startFailures } else { '' }
      crashes = if ($w) { $w.crashes } else { '' }
      lastCrash = if ($w -and $w.lastCrash) { "$($w.lastCrash.at) up $($w.lastCrash.upSec)s" } else { '' }
    }
  } | Format-Table -AutoSize | Out-String | Write-Output
}
if (Test-Path "$run\stats\resources.json") {
  $r = Get-Content "$run\stats\resources.json" | ConvertFrom-Json
  Write-Output "memory (working set, MB): total $($r.totalWsMb)"
}
foreach ($u in @("http://127.0.0.1:8091", "http://127.0.0.1:8092")) {
  try { $h = Invoke-RestMethod "$u/v1/health" -TimeoutSec 5; Write-Output "$u : $($h.state) lag_daa=$($h.lag_daa)" } catch { Write-Output "$u : unreachable" }
}
if (Test-Path "$run\reports\latest.txt") { Get-Content "$run\reports\latest.txt" | Write-Output }
