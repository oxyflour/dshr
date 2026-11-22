param(
    [string] $AsarPath = $env:DSHR_ASAR,
    [string] $PrimaryRuntime,
    [string] $PackagedSidecar
)

$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
if (-not $AsarPath) {
    $AsarPath = Join-Path $env:LOCALAPPDATA 'Programs/DeepSeek Harness/resources/app.asar'
}
$AsarPath = (Resolve-Path -LiteralPath $AsarPath).Path
if (-not $PrimaryRuntime) {
    $PrimaryRuntime = Join-Path (Split-Path -Parent $AsarPath) 'runtime/primary-runtime'
}
$PrimaryRuntime = (Resolve-Path -LiteralPath $PrimaryRuntime).Path
if (-not (Test-Path -LiteralPath (Join-Path $PrimaryRuntime 'runtime.json') -PathType Leaf)) {
    throw "Missing primary-runtime manifest in $PrimaryRuntime"
}

function Invoke-GateTests([string] $Package, [string] $Target, [string] $Suite, [string] $Filter, [int] $Expected) {
    $ErrorActionPreference = 'Continue'
    $output = & (Join-Path $PSScriptRoot 'build-env.cmd') cargo test --locked --offline -p $Package $Target $Suite $Filter -- --ignored --nocapture 2>&1
    $status = $LASTEXITCODE
    $output | ForEach-Object { [string] $_ }
    $summary = $output | Out-String
    if ($status -ne 0 -or $summary -match '\bSKIP\b' -or $summary -notmatch "\b$Expected passed; 0 failed\b") {
        throw "$Suite must run all $Expected rc.5 tests without skipping (exit $status)"
    }
}

if ($PackagedSidecar) {
    $sidecar = (Resolve-Path -LiteralPath $PackagedSidecar).Path
} else {
    $fixtureParent = Join-Path $root '.tmp/dsh-official-rc5'
    New-Item -ItemType Directory -Path $fixtureParent -Force | Out-Null
    $fixtureParent = (Resolve-Path -LiteralPath $fixtureParent).Path
    $resources = Join-Path $fixtureParent ([guid]::NewGuid().ToString('N'))
    $sidecar = Join-Path $resources 'sidecar/dsh-official'
    New-Item -ItemType Directory -Path $sidecar -Force | Out-Null
}
$previousAsar = $env:DSHR_ASAR
$previousSidecar = $env:DSHR_OFFICIAL_RC5_DIR
$previousRuntime = $env:DSHR_OFFICIAL_RC5_RUNTIME

Push-Location $root
try {
    $env:DSHR_ASAR = $AsarPath
    $env:DSHR_OFFICIAL_RC5_DIR = $sidecar
    $env:DSHR_OFFICIAL_RC5_RUNTIME = $PrimaryRuntime
    if ($PackagedSidecar) {
        & node tools/official-provenance.mjs --check --rc5-dir $sidecar --windows-debug
        if ($LASTEXITCODE -ne 0) { throw 'Packaged rc.5 official package provenance check failed' }
    } else {
        $source = Join-Path $root 'sidecar/dsh-official'
        $lock = Get-Content -Encoding UTF8 -Raw -LiteralPath (Join-Path $source 'official-rc5-packages.lock.json') | ConvertFrom-Json
        $packages = @($lock.packages | ForEach-Object { $_.name })
        & node tools/dsh-vendor.mjs (Join-Path $sidecar 'node_modules') @packages
        if ($LASTEXITCODE -ne 0) { throw 'Could not extract rc.5 official packages' }
        & node tools/official-provenance.mjs --check --rc5-dir $sidecar
        if ($LASTEXITCODE -ne 0) { throw 'rc.5 official package provenance check failed' }
        Copy-Item -LiteralPath (Join-Path $source 'sidecar-dsh.mjs'),(Join-Path $source 'lib') -Destination $sidecar -Recurse
    }

    Invoke-GateTests 'dshr-compat-dsh-host' '--test' 'official_packages' 'rc5_files_' 1
    Invoke-GateTests 'dshr-desktop-gpui' '--test' 'headless_cli' 'rc5_' 4
    Invoke-GateTests 'dshr-desktop-gpui' '--test' 'headless_cli' 'stage2_headless_' 1
    Invoke-GateTests 'dshr-desktop-gpui' '--bin' 'dshr' 'stage2_official_shell_' 1
    Invoke-GateTests 'dshr-desktop-gpui' '--test' 'headless_cli' 'stage3_headless_questions_' 1
    Invoke-GateTests 'dshr-desktop-gpui' '--bin' 'dshr' 'stage3_official_questions_' 1
    Invoke-GateTests 'dshr-desktop-gpui' '--test' 'headless_cli' 'stage3_headless_deliverables_' 1
    Invoke-GateTests 'dshr-desktop-gpui' '--test' 'headless_cli' 'stage3_headless_skill_' 1
    Invoke-GateTests 'dshr-desktop-gpui' '--test' 'headless_cli' 'stage3_headless_web_' 1
    Invoke-GateTests 'dshr-desktop-gpui' '--bin' 'dshr' 'stage3_official_office_' 1
    Invoke-GateTests 'dshr-desktop-gpui' '--bin' 'dshr' 'stage4_official_jobs_' 1
    Invoke-GateTests 'dshr-desktop-gpui' '--bin' 'dshr' 'stage4_official_terminals_' 1
    Invoke-GateTests 'dshr-desktop-gpui' '--bin' 'dshr' 'stage5_official_goals_' 1
    Invoke-GateTests 'dshr-desktop-gpui' '--bin' 'dshr' 'stage5_official_query_' 1
    Invoke-GateTests 'dshr-desktop-gpui' '--bin' 'dshr' 'stage5_official_subagents_' 1
    Invoke-GateTests 'dshr-desktop-gpui' '--bin' 'dshr' 'stage5_official_teams_' 1
    Invoke-GateTests 'dshr-desktop-gpui' '--bin' 'dshr' 'stage5_official_workflow_runs_' 1
    Invoke-GateTests 'dshr-desktop-gpui' '--bin' 'dshr' 'stage5_official_workflow_cancels_' 1
    Invoke-GateTests 'dshr-desktop-gpui' '--bin' 'dshr' 'stage5_official_ralph_' 1
    Invoke-GateTests 'dshr-desktop-gpui' '--bin' 'dshr' 'stage5_official_schedules_' 1
    Invoke-GateTests 'dshr-desktop-gpui' '--bin' 'dshr' 'stage6_official_native_' 1
    Invoke-GateTests 'dshr-desktop-gpui' '--bin' 'dshr' 'stage6_official_inspection_' 1
    Invoke-GateTests 'dshr-desktop-gpui' '--bin' 'dshr' 'stage6_official_ptc_' 1
    Invoke-GateTests 'dshr-desktop-gpui' '--bin' 'dshr' 'stage6_official_preset_' 1
} finally {
    $env:DSHR_ASAR = $previousAsar
    $env:DSHR_OFFICIAL_RC5_DIR = $previousSidecar
    $env:DSHR_OFFICIAL_RC5_RUNTIME = $previousRuntime
    Pop-Location
    if ($resources) {
        $resolved = (Resolve-Path -LiteralPath $resources).Path
        if ((Split-Path -Parent $resolved) -eq $fixtureParent) {
            Remove-Item -LiteralPath $resolved -Recurse -Force
        }
    }
}

Write-Host '[official-rc5-gate] PASS'
