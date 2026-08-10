param(
    [string]$EnvFile,
    [string]$DataDir,
    [string]$HostName,
    [int]$Port = 0,
    [switch]$Foreground
)

$ErrorActionPreference = 'Stop'
$Root = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
if (-not $EnvFile) { $EnvFile = Join-Path $Root '.env' }
if (-not $DataDir) { $DataDir = Join-Path $Root '.aos-data' }
$DataDir = [IO.Path]::GetFullPath($DataDir)
$RootPrefix = $Root.TrimEnd('\') + '\'
if (-not $DataDir.StartsWith($RootPrefix, [StringComparison]::OrdinalIgnoreCase)) {
    throw "AOS data directory must be inside $Root"
}

$Release = (Test-Path (Join-Path $Root 'bin\web-server.exe')) -and (Test-Path (Join-Path $Root 'web\index.html'))
if ($Release) {
    $Backend = Join-Path $Root 'bin\web-server.exe'
    $Web = Join-Path $Root 'web'
} else {
    $Backend = Join-Path $Root 'rust\target\release\web-server.exe'
    $Web = Join-Path $Root 'webui\dist'
    if (-not (Test-Path $Backend) -or -not (Test-Path (Join-Path $Web 'index.html'))) {
        throw 'Source release artifacts are missing. Build with cargo and npm, or use an AOS Offline package.'
    }
}

& (Join-Path $Root 'scripts\setup-environment.ps1')
if (-not (Test-Path $EnvFile)) {
    & (Join-Path $Root 'scripts\generate-env.ps1') -Path $EnvFile
} else {
    & (Join-Path $Root 'scripts\generate-env.ps1') -Path $EnvFile -Repair
}
foreach ($Line in Get-Content $EnvFile) {
    if ($Line -match '^\s*#' -or $Line -notmatch '=') { continue }
    $Pair = $Line.Split('=', 2)
    [Environment]::SetEnvironmentVariable($Pair[0].Trim(), $Pair[1].Trim().Trim('"', "'"), 'Process')
}
if (-not $HostName) { $HostName = if ($env:AOS_BIND_HOST) { $env:AOS_BIND_HOST } else { '127.0.0.1' } }
if ($Port -eq 0) { $Port = if ($env:AOS_WEB_PORT) { [int]$env:AOS_WEB_PORT } else { 3000 } }
if ($Port -lt 1 -or $Port -gt 65535) { throw 'Port must be between 1 and 65535.' }

$ModelRoot = if ($Release) { Join-Path $Root 'models\fastembed' } else { Join-Path $Root '.aos-runtime\models\fastembed' }
$Snapshot = Join-Path $ModelRoot 'models--Qdrant--paraphrase-multilingual-MiniLM-L12-v2-onnx-Q\snapshots\faf4aa4225822f3bc6376869cb1164e8e3feedd0'
$ModelHashes = [ordered]@{
    'model_optimized.onnx' = '634d0f66c29dc934c8fa72b8a4fe91dd4d420a22f1d82a241058d4316e659a99'
    'tokenizer.json' = 'fa685fc160bbdbab64058d4fc91b60e62d207e8dc60b9af5c002c5ab946ded00'
    'config.json' = 'c8ec081fdad2df991bf5abbf18418fec7a5cdaa421f60ffb060a30040b8c376f'
    'special_tokens_map.json' = '8c785abebea9ae3257b61681b4e6fd8365ceafde980c21970d001e834cf10835'
    'tokenizer_config.json' = '0666eebf692422757e1dddf3c9fb1ded73ba3dc726c5828671fc89e45bf3609f'
}
$ModelFiles = @($ModelHashes.Keys)
$MissingModelFiles = @($ModelFiles | Where-Object { -not (Test-Path (Join-Path $Snapshot $_)) })
if ($MissingModelFiles.Count -gt 0) {
    if ($Release) { throw "AOS Offline package is incomplete: missing local embedding files. Runtime downloads are disabled." }
    Write-Host '==> Downloading the pinned local embedding model for this source checkout'
    & (Join-Path $Root 'scripts\download-local-embedding.ps1') -Dir $ModelRoot
}
foreach ($Entry in $ModelHashes.GetEnumerator()) {
    $ActualModelHash = (Get-FileHash (Join-Path $Snapshot $Entry.Key) -Algorithm SHA256).Hash.ToLowerInvariant()
    if ($ActualModelHash -ne $Entry.Value) { throw "AOS local embedding checksum mismatch for $($Entry.Key); refusing to start." }
}
$OrtRoot = if ($Release) { Join-Path $Root 'runtime\onnxruntime' } else { Join-Path $Root '.aos-runtime\onnxruntime' }
$OrtDll = Join-Path $OrtRoot 'lib\onnxruntime.dll'
$OrtProvidersDll = Join-Path $OrtRoot 'lib\onnxruntime_providers_shared.dll'
if (-not (Test-Path $OrtDll) -or -not (Test-Path $OrtProvidersDll)) {
    if ($Release) {
        throw 'AOS Offline package is incomplete: required ONNX Runtime DLLs are missing. Runtime downloads are disabled.'
    }
    & (Join-Path $Root 'scripts\setup-onnxruntime.ps1') -Dir $OrtRoot
}
$env:AOS_LOCAL_EMBEDDING_CACHE_DIR = $ModelRoot
$env:ORT_DYLIB_PATH = $OrtDll
$env:Path = (Split-Path $OrtDll) + ';' + $env:Path
$env:BASE_URL = if ($env:BASE_URL) { $env:BASE_URL } else { "http://localhost:$Port" }

New-Item -ItemType Directory -Force $DataDir | Out-Null
$RunDir = Join-Path $Root '.run\aos'
New-Item -ItemType Directory -Force $RunDir | Out-Null
$PidFile = Join-Path $RunDir 'web-server.pid'
$LogFile = Join-Path $RunDir 'web-server.log'
$ErrorLogFile = Join-Path $RunDir 'web-server.err.log'
if (Test-Path $PidFile) {
    $OldPid = [int](Get-Content $PidFile -Raw)
    if (Get-Process -Id $OldPid -ErrorAction SilentlyContinue) { Write-Host "AOS is already running: http://localhost:$Port"; exit 0 }
    Remove-Item $PidFile -Force
}
$Args = @('--addr', "${HostName}:$Port", '--data-dir', $DataDir, '--web-dir', $Web)
if ($Foreground) {
    Write-Host "AOS is starting: http://localhost:$Port"
    & $Backend @Args
    exit $LASTEXITCODE
}
$Process = Start-Process $Backend -ArgumentList $Args -RedirectStandardOutput $LogFile -RedirectStandardError $ErrorLogFile -PassThru -WindowStyle Hidden
Set-Content $PidFile $Process.Id
for ($Attempt = 0; $Attempt -lt 120; $Attempt++) {
    if ($Process.HasExited) { break }
    try {
        Invoke-WebRequest "http://127.0.0.1:$Port/api/v1/setup/check" -UseBasicParsing -TimeoutSec 2 | Out-Null
        Write-Host "AOS is ready: http://localhost:$Port"
        Write-Host "Log: $LogFile"
        exit 0
    } catch { Start-Sleep -Seconds 1 }
}
if (-not $Process.HasExited) { Stop-Process -Id $Process.Id -Force }
Remove-Item $PidFile -Force -ErrorAction SilentlyContinue
Get-Content $LogFile -Tail 60 -ErrorAction SilentlyContinue
Get-Content $ErrorLogFile -Tail 60 -ErrorAction SilentlyContinue
throw 'AOS failed to become ready.'
