param(
    [Parameter(Mandatory = $true)][string]$Target,
    [string]$DataDir,
    [string]$EnvFile,
    [int]$Port = 0
)

$ErrorActionPreference = 'Stop'
$Source = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
$Target = (Resolve-Path $Target).Path
if ($Source -eq $Target) { throw 'Extract the new package beside the old installation; source and target cannot be the same directory.' }
$VolumeRoot = [IO.Path]::GetPathRoot($Target).TrimEnd('\')
$UnsafeTargets = @($VolumeRoot, $env:ProgramFiles, ${env:ProgramFiles(x86)}) | Where-Object { $_ }
if ($Target.TrimEnd('\') -in $UnsafeTargets) { throw "Refusing unsafe target path: $Target" }
if (-not (Test-Path (Join-Path $Source 'bin\web-server.exe'))) { throw 'New package is missing bin\web-server.exe.' }
if (-not (Test-Path (Join-Path $Source 'web\index.html'))) { throw 'New package is missing web\index.html.' }
if (-not (Test-Path (Join-Path $Target 'scripts\aos-start.ps1'))) { throw 'Target is not an AOS Offline installation.' }

if (-not $DataDir) { $DataDir = Join-Path $Target '.aos-data' }
if (-not $EnvFile) { $EnvFile = Join-Path $Target '.env' }
$DataDir = [IO.Path]::GetFullPath($DataDir)
$EnvFile = [IO.Path]::GetFullPath($EnvFile)
$TargetPrefix = $Target.TrimEnd('\') + '\'
if (-not $DataDir.StartsWith($TargetPrefix, [StringComparison]::OrdinalIgnoreCase)) { throw 'DataDir must remain inside the target installation.' }
if (-not $EnvFile.StartsWith($TargetPrefix, [StringComparison]::OrdinalIgnoreCase)) { throw 'EnvFile must remain inside the target installation.' }
if (-not (Test-Path $EnvFile)) { throw "Target environment file is missing: $EnvFile" }

$Manifest = Join-Path $Source 'RELEASE-MANIFEST.sha256'
if (Test-Path $Manifest) {
    Write-Host '==> Verifying new release manifest'
    foreach ($Line in Get-Content $Manifest) {
        if (-not $Line.Trim()) { continue }
        $Parts = $Line -split '\s{2}', 2
        if ($Parts.Count -ne 2) { throw "Invalid release manifest line: $Line" }
        $Path = Join-Path $Source ($Parts[1] -replace '/', '\')
        if (-not (Test-Path $Path -PathType Leaf)) { throw "Release file is missing: $($Parts[1])" }
        $Actual = (Get-FileHash $Path -Algorithm SHA256).Hash.ToLowerInvariant()
        if ($Actual -ne $Parts[0].ToLowerInvariant()) { throw "Release checksum mismatch: $($Parts[1])" }
    }
}

$Timestamp = (Get-Date).ToUniversalTime().ToString('yyyyMMddTHHmmssZ')
$BackupRoot = Join-Path $Target ".aos-backups\upgrade-$Timestamp"
$RollbackRoot = Join-Path $BackupRoot 'release'
$FailedRoot = Join-Path $BackupRoot 'failed-new-release'
New-Item -ItemType Directory -Force $RollbackRoot, $FailedRoot | Out-Null
$DataArchive = Join-Path $BackupRoot 'data-before-upgrade.zip'
$Assets = @('bin', 'web', 'runtime', 'models', 'scripts', 'docs', 'licenses', 'examples', '.env.example', 'README.md', 'README.zh-CN.md', 'LICENSE', 'NOTICE.md', 'RELEASE-MANIFEST.sha256')
$RollbackRequired = $false

function Start-Aos([string]$Root) {
    $Arguments = @{ EnvFile = $EnvFile; DataDir = $DataDir }
    if ($Port -gt 0) { $Arguments.Port = $Port }
    & (Join-Path $Root 'scripts\aos-start.ps1') @Arguments
    if ($LASTEXITCODE -ne 0) { throw "AOS start failed with exit code $LASTEXITCODE" }
}

function Restore-PreviousRelease {
    Write-Warning 'Upgrade failed; restoring the previous AOS release.'
    $StopScript = Join-Path $Target 'scripts\aos-stop.ps1'
    if (Test-Path $StopScript) { try { & $StopScript } catch {} }
    foreach ($Asset in $Assets) {
        $Current = Join-Path $Target $Asset
        $Previous = Join-Path $RollbackRoot $Asset
        if (Test-Path $Current) {
            $Failed = Join-Path $FailedRoot $Asset
            New-Item -ItemType Directory -Force (Split-Path $Failed) | Out-Null
            Move-Item $Current $Failed -Force
        }
        if (Test-Path $Previous) {
            New-Item -ItemType Directory -Force (Split-Path $Current) | Out-Null
            Move-Item $Previous $Current -Force
        }
    }
    if (Test-Path $DataArchive) {
        if (Test-Path $DataDir) { Move-Item $DataDir (Join-Path $BackupRoot 'data-after-failed-upgrade') -Force }
        Expand-Archive $DataArchive (Split-Path $DataDir) -Force
    }
    try { Start-Aos $Target } catch { Write-Warning "Previous release was restored but did not start automatically: $_" }
    Write-Warning "Previous release restored. Backup: $BackupRoot"
}

try {
    Write-Host '==> Stopping the existing AOS instance'
    & (Join-Path $Target 'scripts\aos-stop.ps1')

    Write-Host '==> Backing up AOS data'
    Compress-Archive -Path $DataDir -DestinationPath $DataArchive -CompressionLevel Optimal
    Copy-Item $EnvFile (Join-Path $BackupRoot 'env.before-upgrade')
    $Database = Join-Path $DataDir 'aos.db'
    if (Test-Path $Database) { (Get-FileHash $Database -Algorithm SHA256).Hash.ToLowerInvariant() | Set-Content (Join-Path $BackupRoot 'aos.db.before.sha256') }
    $RollbackRequired = $true

    Write-Host '==> Installing the new release files'
    foreach ($Asset in $Assets) {
        $New = Join-Path $Source $Asset
        if (-not (Test-Path $New)) { continue }
        $Current = Join-Path $Target $Asset
        $Previous = Join-Path $RollbackRoot $Asset
        if (Test-Path $Current) {
            New-Item -ItemType Directory -Force (Split-Path $Previous) | Out-Null
            Move-Item $Current $Previous -Force
        }
        New-Item -ItemType Directory -Force (Split-Path $Current) | Out-Null
        Copy-Item $New $Current -Recurse -Force
    }

    Write-Host '==> Starting upgraded AOS'
    Start-Aos $Target
    if (-not (Test-Path (Join-Path $DataDir 'aos.db'))) { throw 'Upgraded AOS did not preserve aos.db.' }
    $RollbackRequired = $false
    Write-Host 'AOS upgrade completed. Data and configuration were preserved.'
    Write-Host "Pre-upgrade backup: $BackupRoot"
} catch {
    if ($RollbackRequired) { Restore-PreviousRelease }
    throw
}
