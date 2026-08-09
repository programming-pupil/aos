param([switch]$Install)

$ErrorActionPreference = 'Stop'
$Required = @(
    @{ Command = 'git'; Package = 'Git.Git'; Label = 'Git' },
    @{ Command = 'rg'; Package = 'BurntSushi.ripgrep.MSVC'; Label = 'ripgrep' },
    @{ Command = 'node'; Package = 'OpenJS.NodeJS.LTS'; Label = 'Node.js 22 LTS' },
    @{ Command = 'npm'; Package = 'OpenJS.NodeJS.LTS'; Label = 'npm' },
    @{ Command = 'npx'; Package = 'OpenJS.NodeJS.LTS'; Label = 'npx for npm MCP servers' },
    @{ Command = 'python'; Package = 'Python.Python.3.12'; Label = 'Python 3.12' },
    @{ Command = 'uv'; Package = 'astral-sh.uv'; Label = 'uv' },
    @{ Command = 'uvx'; Package = 'astral-sh.uv'; Label = 'uvx for Python MCP servers' }
)

function Get-Missing {
    return @($Required | Where-Object { -not (Get-Command $_.Command -ErrorAction SilentlyContinue) })
}

$Missing = Get-Missing
if ($Install -and $Missing.Count -gt 0) {
    if (-not (Get-Command winget -ErrorAction SilentlyContinue)) {
        throw 'winget is required for automatic installation. Install App Installer from Microsoft Store.'
    }
    $Packages = $Missing.Package | Select-Object -Unique
    foreach ($Package in $Packages) {
        & winget install --id $Package --exact --accept-package-agreements --accept-source-agreements
        if ($LASTEXITCODE -ne 0) { throw "Failed to install $Package" }
    }
    $env:Path = [Environment]::GetEnvironmentVariable('Path', 'Machine') + ';' + [Environment]::GetEnvironmentVariable('Path', 'User')
    $Missing = Get-Missing
}

foreach ($Item in $Required) {
    if (Get-Command $Item.Command -ErrorAction SilentlyContinue) {
        Write-Host "  [ok]      $($Item.Label)"
    } else {
        Write-Host "  [missing] $($Item.Label)" -ForegroundColor Red
    }
}
if ($Missing.Count -gt 0) {
    throw "Environment is incomplete. Run: .\scripts\setup-environment.ps1 -Install"
}
Write-Host 'Environment is ready.'
