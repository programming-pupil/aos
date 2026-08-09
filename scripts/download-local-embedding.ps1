param([Parameter(Mandatory = $true)][string]$Dir)

$ErrorActionPreference = 'Stop'
$Revision = 'faf4aa4225822f3bc6376869cb1164e8e3feedd0'
$Repository = 'Qdrant/paraphrase-multilingual-MiniLM-L12-v2-onnx-Q'
$ModelDirName = 'models--Qdrant--paraphrase-multilingual-MiniLM-L12-v2-onnx-Q'
$Snapshot = Join-Path $Dir "$ModelDirName\snapshots\$Revision"
$BaseUrl = "https://huggingface.co/$Repository/resolve/$Revision"
$Hashes = [ordered]@{
    'model_optimized.onnx' = '634d0f66c29dc934c8fa72b8a4fe91dd4d420a22f1d82a241058d4316e659a99'
    'tokenizer.json' = 'fa685fc160bbdbab64058d4fc91b60e62d207e8dc60b9af5c002c5ab946ded00'
    'config.json' = 'c8ec081fdad2df991bf5abbf18418fec7a5cdaa421f60ffb060a30040b8c376f'
    'special_tokens_map.json' = '8c785abebea9ae3257b61681b4e6fd8365ceafde980c21970d001e834cf10835'
    'tokenizer_config.json' = '0666eebf692422757e1dddf3c9fb1ded73ba3dc726c5828671fc89e45bf3609f'
}

New-Item -ItemType Directory -Force $Snapshot | Out-Null
Write-Host "==> Preparing pinned AOS local embedding model ($Revision)"
foreach ($Entry in $Hashes.GetEnumerator()) {
    $Name = $Entry.Key
    $Expected = $Entry.Value
    $Destination = Join-Path $Snapshot $Name
    $Valid = (Test-Path $Destination) -and ((Get-FileHash $Destination -Algorithm SHA256).Hash.ToLowerInvariant() -eq $Expected)
    if ($Valid) {
        Write-Host "  [cached] $Name"
        continue
    }
    Write-Host "  [download] $Name"
    $Partial = "$Destination.partial"
    $Downloaded = $false
    for ($Attempt = 1; $Attempt -le 5; $Attempt++) {
        try {
            Invoke-WebRequest "$BaseUrl/$Name`?download=true" -OutFile $Partial -UseBasicParsing -TimeoutSec 1800
            $Downloaded = $true
            break
        } catch {
            if ($Attempt -eq 5) { throw }
            Start-Sleep -Seconds ([Math]::Min(2 * $Attempt, 10))
        }
    }
    if (-not $Downloaded) { throw "Failed to download $Name" }
    $Actual = (Get-FileHash $Partial -Algorithm SHA256).Hash.ToLowerInvariant()
    if ($Actual -ne $Expected) { throw "Checksum mismatch for $Name; expected $Expected, got $Actual" }
    Move-Item $Partial $Destination -Force
}
Write-Host "Pinned local embedding model is ready: $Snapshot"
