param(
    [switch]$SkipBuild,
    [string]$OutputDir
)

$ErrorActionPreference = 'Stop'
$Root = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
if (-not $OutputDir) { $OutputDir = Join-Path $Root 'dist' }
if (-not [Environment]::Is64BitOperatingSystem) { throw 'Windows packaging requires x64 Windows.' }
& (Join-Path $Root 'scripts\setup-environment.ps1')
if (-not $SkipBuild) {
    if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) { throw 'Cargo is required to build the Windows package.' }
    Push-Location (Join-Path $Root 'rust')
    try { cargo build -p web-server --release --features full; if ($LASTEXITCODE -ne 0) { throw 'Rust release build failed.' } } finally { Pop-Location }
    Push-Location (Join-Path $Root 'webui')
    try { npm ci; npm run build:ci; if ($LASTEXITCODE -ne 0) { throw 'WebUI build failed.' } } finally { Pop-Location }
}
$Backend = Join-Path $Root 'rust\target\release\web-server.exe'
$Web = Join-Path $Root 'webui\dist'
if (-not (Test-Path $Backend) -or -not (Test-Path (Join-Path $Web 'index.html'))) { throw 'Release artifacts are missing.' }
$Version = ((Select-String (Join-Path $Root 'rust\Cargo.toml') '^version\s*=\s*"([^"]+)"').Matches.Groups[1].Value | Select-Object -First 1)
if (-not $Version) { $Version = '0.1.0' }
$Name = "aos-offline-$Version-windows-x86_64"
$Temp = Join-Path ([IO.Path]::GetTempPath()) ("aos-package-" + [guid]::NewGuid())
$Stage = Join-Path $Temp $Name
New-Item -ItemType Directory -Force (Join-Path $Stage 'bin'), (Join-Path $Stage 'web'), (Join-Path $Stage 'scripts'), (Join-Path $Stage 'docs'), (Join-Path $Stage 'docs\assets'), (Join-Path $Stage 'licenses'), (Join-Path $Stage 'models\fastembed') | Out-Null
try {
    Copy-Item $Backend (Join-Path $Stage 'bin\web-server.exe')
    Copy-Item (Join-Path $Web '*') (Join-Path $Stage 'web') -Recurse
    foreach ($NameToCopy in @('.env.example', 'README.md', 'LICENSE', 'NOTICE.md')) { Copy-Item (Join-Path $Root $NameToCopy) $Stage }
    Copy-Item (Join-Path $Root 'licenses\*.txt') (Join-Path $Stage 'licenses')
    Copy-Item (Join-Path $Root 'docs\assets\aos-hero.svg') (Join-Path $Stage 'docs\assets')
    Copy-Item (Join-Path $Root 'docs\assets\aos-menu-map.svg') (Join-Path $Stage 'docs\assets')
    foreach ($Doc in @('INSTALL.md', 'OPEN_SOURCE_DEPLOYMENT.zh-CN.md', 'OPEN_SOURCE_TEST_GUIDE.zh-CN.md')) { Copy-Item (Join-Path $Root "docs\$Doc") (Join-Path $Stage 'docs') }
    foreach ($Script in @('aos-start.ps1', 'aos-stop.ps1', 'aos-upgrade.ps1', 'generate-env.ps1', 'setup-environment.ps1', 'setup-onnxruntime.ps1')) { Copy-Item (Join-Path $Root "scripts\$Script") (Join-Path $Stage 'scripts') }
    & (Join-Path $Root 'scripts\setup-onnxruntime.ps1') -Dir (Join-Path $Stage 'runtime\onnxruntime')
    $ModelCache = Join-Path $Root '.aos-runtime\models\fastembed'
    & (Join-Path $Root 'scripts\download-local-embedding.ps1') -Dir $ModelCache
    Copy-Item (Join-Path $ModelCache '*') (Join-Path $Stage 'models\fastembed') -Recurse -Force
    $env:ORT_DYLIB_PATH = Join-Path $Stage 'runtime\onnxruntime\lib\onnxruntime.dll'
    & $Backend --warm-local-embedding (Join-Path $Stage 'models\fastembed')
    if ($LASTEXITCODE -ne 0) { throw 'Local embedding model warm-up failed.' }
    $Snapshot = Join-Path $Stage 'models\fastembed\models--Qdrant--paraphrase-multilingual-MiniLM-L12-v2-onnx-Q\snapshots\faf4aa4225822f3bc6376869cb1164e8e3feedd0'
    $ModelHashes = [ordered]@{
        'model_optimized.onnx' = '634d0f66c29dc934c8fa72b8a4fe91dd4d420a22f1d82a241058d4316e659a99'
        'tokenizer.json' = 'fa685fc160bbdbab64058d4fc91b60e62d207e8dc60b9af5c002c5ab946ded00'
        'config.json' = 'c8ec081fdad2df991bf5abbf18418fec7a5cdaa421f60ffb060a30040b8c376f'
        'special_tokens_map.json' = '8c785abebea9ae3257b61681b4e6fd8365ceafde980c21970d001e834cf10835'
        'tokenizer_config.json' = '0666eebf692422757e1dddf3c9fb1ded73ba3dc726c5828671fc89e45bf3609f'
    }
    foreach ($Entry in $ModelHashes.GetEnumerator()) {
        $ModelPath = Join-Path $Snapshot $Entry.Key
        if (-not (Test-Path $ModelPath)) { throw "Missing bundled model file: $($Entry.Key)" }
        if ((Get-FileHash $ModelPath -Algorithm SHA256).Hash.ToLowerInvariant() -ne $Entry.Value) { throw "Local embedding checksum mismatch for $($Entry.Key)." }
    }
    $ManifestPaths = @(
        (Join-Path $Stage 'bin'),
        (Join-Path $Stage 'web'),
        (Join-Path $Stage 'runtime'),
        (Join-Path $Stage 'models'),
        (Join-Path $Stage 'scripts'),
        (Join-Path $Stage 'docs'),
        (Join-Path $Stage 'licenses'),
        (Join-Path $Stage '.env.example'),
        (Join-Path $Stage 'README.md'),
        (Join-Path $Stage 'LICENSE'),
        (Join-Path $Stage 'NOTICE.md')
    )
    $ManifestLines = Get-ChildItem $ManifestPaths -File -Recurse |
        Sort-Object FullName |
        ForEach-Object {
            $Relative = $_.FullName.Substring($Stage.Length + 1).Replace('\', '/')
            $Hash = (Get-FileHash $_.FullName -Algorithm SHA256).Hash.ToLowerInvariant()
            "$Hash  $Relative"
        }
    $ManifestLines | Set-Content (Join-Path $Stage 'RELEASE-MANIFEST.sha256') -Encoding ascii
    $Forbidden = Get-ChildItem $Stage -Recurse -Force | Where-Object { $_.Name -match '^(\.env|aos\.db|aos\.db-wal|aos\.db-shm)$' -or $_.FullName -match '[\\/](\.run|\.aos-data)[\\/]' }
    if ($Forbidden) { throw 'Package contains runtime data or secrets.' }
    New-Item -ItemType Directory -Force $OutputDir | Out-Null
    $Archive = Join-Path $OutputDir "$Name.zip"
    Remove-Item $Archive -Force -ErrorAction SilentlyContinue
    Compress-Archive -Path $Stage -DestinationPath $Archive -CompressionLevel Optimal
    Write-Host "AOS Offline Windows package created: $Archive"
} finally {
    Remove-Item $Temp -Recurse -Force -ErrorAction SilentlyContinue
}
