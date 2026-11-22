param(
    [string]$PackageDir = 'dist/rc5/dshr-debug-windows-x64',
    [string]$ExistingProfile
)

$ErrorActionPreference = 'Stop'
$repo = Split-Path -Parent $PSScriptRoot
Set-Location $repo
$package = (Resolve-Path -LiteralPath $PackageDir).Path
$destination = Join-Path $repo 'target/debug'
$exe = Join-Path $destination 'dshr.exe'
if (-not (Test-Path -LiteralPath $exe -PathType Leaf)) {
    throw 'Build target/debug/dshr.exe before staging rc.5 resources'
}
if (Test-Path -LiteralPath (Join-Path $destination 'sidecar') -PathType Container) {
    throw 'target/debug already contains a desktop package layout'
}
if (Get-Process -Name dshr -ErrorAction SilentlyContinue | Where-Object Path -EQ $exe) {
    throw 'Close target/debug/dshr.exe before staging its resources'
}

$official = Join-Path $package 'sidecar/dsh-official'
$node = Join-Path $package 'runtime/node/node.exe'
$seed = Join-Path $official 'default-profile.json'
foreach ($file in @($node, $seed, (Join-Path $package 'runtime/powershell/pwsh.exe'))) {
    if (-not (Test-Path -LiteralPath $file -PathType Leaf)) { throw "Missing packaged rc.5 asset: $file" }
}
& $node tools/official-provenance.mjs --check --rc5-dir $official --windows-debug
if ($LASTEXITCODE -ne 0) { throw 'Packaged rc.5 provenance check failed' }

$profileDir = Join-Path $destination 'profile'
$profile = Join-Path $profileDir 'plugins.json'
if ($ExistingProfile) {
    $profileSource = (Resolve-Path -LiteralPath $ExistingProfile).Path
} elseif (Test-Path -LiteralPath $profile -PathType Leaf) {
    $profileSource = $profile
} else {
    $profileSource = $seed
}
$effective = Get-Content -LiteralPath $profileSource -Encoding UTF8 -Raw | ConvertFrom-Json
$defaults = Get-Content -LiteralPath $seed -Encoding UTF8 -Raw | ConvertFrom-Json
if ($null -eq $effective.plugins) { throw "Expected a plugins array in $profileSource" }
$existingNames = @($effective.plugins | ForEach-Object { $_.name })
$enabledNames = @($effective.plugins | Where-Object { $_.enabled -ne $false } | ForEach-Object { $_.name })
$alternatives = @{
    '@deepseek-ai/dsh-tool-pwsh' = '@deepseek-ai/dsh-tool-pwsh-persistent'
    '@deepseek-ai/dsh-tool-executor-native' = '@deepseek-ai/dsh-tool-executor-ptc'
}
$missing = @($defaults.plugins | Where-Object { $_.name -notin $existingNames })
foreach ($entry in $missing) {
    if ($alternatives[$entry.name] -in $enabledNames) {
        $entry | Add-Member -NotePropertyName enabled -NotePropertyValue $false -Force
    }
}
$effective.plugins = @($effective.plugins) + $missing

$assets = Join-Path $destination 'dshr-debug-assets'
New-Item -ItemType Directory -Path $assets,$profileDir -Force | Out-Null
Copy-Item -LiteralPath (Join-Path $package 'sidecar'),(Join-Path $package 'runtime') -Destination $assets -Recurse -Force
& $node tools/official-provenance.mjs --check --rc5-dir (Join-Path $assets 'sidecar/dsh-official') --windows-debug
if ($LASTEXITCODE -ne 0) { throw 'Staged rc.5 provenance check failed' }

if ($profileSource -ne $profile -or $missing.Count -gt 0) {
    $suffix = [guid]::NewGuid().ToString('N')
    $pending = "$profile.$suffix.tmp"
    $json = $effective | ConvertTo-Json -Depth 100
    [IO.File]::WriteAllText($pending, $json, [Text.UTF8Encoding]::new($false))
    if (Test-Path -LiteralPath $profile -PathType Leaf) {
        $backup = "$profile.before-rc5-$suffix.bak"
        [IO.File]::Replace($pending, $profile, $backup)
        Write-Host "[stage-windows-debug] profile backup: $backup"
    } else {
        Move-Item -LiteralPath $pending -Destination $profile
    }
}

Write-Host "[stage-windows-debug] $exe"
Write-Host "[stage-windows-debug] $($effective.plugins.Count) profile entries"
