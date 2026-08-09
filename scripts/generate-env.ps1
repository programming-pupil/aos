param(
    [string]$Path,
    [switch]$Repair
)

$ErrorActionPreference = 'Stop'
$Root = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
if (-not $Path) { $Path = Join-Path $Root '.env' }
$Template = Join-Path $Root '.env.example'
if (-not (Test-Path $Template -PathType Leaf)) { throw "Missing environment template: $Template" }
if ((Test-Path $Path) -and -not $Repair) { throw "Refusing to overwrite existing environment file: $Path" }

if (-not (Test-Path $Path)) {
    Copy-Item $Template $Path
}
$Content = Get-Content $Path -Raw

function New-HexSecret([int]$Bytes) {
    $buffer = New-Object byte[] $Bytes
    $rng = [System.Security.Cryptography.RandomNumberGenerator]::Create()
    try { $rng.GetBytes($buffer) } finally { $rng.Dispose() }
    return (($buffer | ForEach-Object { $_.ToString('x2') }) -join '')
}

function Set-EnvValue([string]$Name, [string]$Value) {
    $script:Content = [regex]::Replace(
        $script:Content,
        "(?m)^$([regex]::Escape($Name))=.*$",
        "$Name=$Value"
    )
    if ($script:Content -notmatch "(?m)^$([regex]::Escape($Name))=") {
        $script:Content = $script:Content.TrimEnd() + "`r`n$Name=$Value`r`n"
    }
}

function Repair-Secret([string]$Name, [int]$Bytes, [int]$RequiredLength) {
    $match = [regex]::Match($script:Content, "(?m)^$([regex]::Escape($Name))=(.*)$")
    $value = if ($match.Success) { $match.Groups[1].Value.Trim('"', "'") } else { '' }
    $placeholder = $value -match '(?i)change-me|replace-me|^your-|dev-secret'
    if ($placeholder -or $value.Length -lt $RequiredLength) {
        Set-EnvValue $Name (New-HexSecret $Bytes)
    }
}

Repair-Secret 'JWT_SECRET' 32 32
Repair-Secret 'ENCRYPTION_KEY' 16 32
Repair-Secret 'TOKEN_ENCRYPTION_KEY' 32 32
if ($env:AOSD_GITHUB_TOKEN) { Set-EnvValue 'AOSD_GITHUB_TOKEN' $env:AOSD_GITHUB_TOKEN }
[System.IO.File]::WriteAllText($Path, $Content, [System.Text.UTF8Encoding]::new($false))
Write-Host "Environment file is ready: $Path"
