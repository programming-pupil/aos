$ErrorActionPreference = 'Stop'
$Root = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
$PidFile = Join-Path $Root '.run\aos\web-server.pid'
if (-not (Test-Path $PidFile)) { Write-Host 'AOS is not running.'; exit 0 }
$PidValue = [int](Get-Content $PidFile -Raw)
$Process = Get-Process -Id $PidValue -ErrorAction SilentlyContinue
if (-not $Process) { Remove-Item $PidFile -Force; Write-Host 'Removed stale PID file.'; exit 0 }
if ($Process.ProcessName -notlike '*web-server*') { throw "Refusing to stop non-AOS process $PidValue" }
Stop-Process -Id $PidValue
try { Wait-Process -Id $PidValue -Timeout 30 -ErrorAction Stop } catch { Stop-Process -Id $PidValue -Force }
Remove-Item $PidFile -Force
Write-Host 'AOS stopped.'
