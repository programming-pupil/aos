param([string]$Dir)

$ErrorActionPreference = 'Stop'
$Root = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
if (-not $Dir) { $Dir = Join-Path $Root '.aos-runtime\onnxruntime' }
$Version = '1.23.2'
$Dll = Join-Path $Dir 'lib\onnxruntime.dll'
$ProvidersDll = Join-Path $Dir 'lib\onnxruntime_providers_shared.dll'
$VersionFile = Join-Path $Dir 'VERSION_NUMBER'
$InstalledVersion = if (Test-Path $VersionFile -PathType Leaf) { (Get-Content $VersionFile -Raw).Trim() } else { '' }
if ((Test-Path $Dll -PathType Leaf) -and (Test-Path $ProvidersDll -PathType Leaf) -and $InstalledVersion -eq $Version) { Write-Host "ONNX Runtime is ready: $Dll"; exit 0 }
if (Test-Path $Dll -PathType Leaf) { Write-Host "Replacing ONNX Runtime $InstalledVersion with $Version" }
if (-not [Environment]::Is64BitOperatingSystem) { throw 'AOS Offline for Windows requires 64-bit Windows.' }

$Temp = Join-Path ([IO.Path]::GetTempPath()) ("aos-onnxruntime-" + [guid]::NewGuid())
New-Item -ItemType Directory -Force $Temp | Out-Null
try {
    $Archive = Join-Path $Temp 'onnxruntime.zip'
    $Url = "https://github.com/microsoft/onnxruntime/releases/download/v$Version/onnxruntime-win-x64-$Version.zip"
    $ExpectedArchiveHash = '0b38df9af21834e41e73d602d90db5cb06dbd1ca618948b8f1d66d607ac9f3cd'
    Write-Host "Downloading ONNX Runtime $Version for Windows x64"
    Invoke-WebRequest -Uri $Url -OutFile $Archive
    $ActualArchiveHash = (Get-FileHash $Archive -Algorithm SHA256).Hash.ToLowerInvariant()
    if ($ActualArchiveHash -ne $ExpectedArchiveHash) { throw 'ONNX Runtime archive checksum mismatch for Windows x64.' }
    Expand-Archive $Archive $Temp -Force
    $Source = Join-Path $Temp "onnxruntime-win-x64-$Version"
    New-Item -ItemType Directory -Force (Join-Path $Dir 'lib') | Out-Null
    Copy-Item (Join-Path $Source 'lib\onnxruntime.dll') $Dll
    Copy-Item (Join-Path $Source 'lib\onnxruntime_providers_shared.dll') $ProvidersDll
    foreach ($Name in @('LICENSE', 'README.md', 'ThirdPartyNotices.txt', 'VERSION_NUMBER')) {
        $File = Join-Path $Source $Name
        if (Test-Path $File) { Copy-Item $File (Join-Path $Dir $Name) }
    }
} finally {
    Remove-Item $Temp -Recurse -Force -ErrorAction SilentlyContinue
}
if (-not (Test-Path $Dll) -or -not (Test-Path $ProvidersDll)) { throw 'ONNX Runtime installation failed: required DLLs are missing.' }
$InstalledVersion = if (Test-Path $VersionFile -PathType Leaf) { (Get-Content $VersionFile -Raw).Trim() } else { '' }
if ($InstalledVersion -ne $Version) { throw "ONNX Runtime installation reported version $InstalledVersion; expected $Version." }
Write-Host "ONNX Runtime is ready: $Dll"
