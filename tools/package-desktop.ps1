param(
    [ValidateSet("debug", "release")]
    [string]$Configuration = "debug",
    [string]$OutDir = "dist",
    [string]$RuntimeComponentCatalog = "",
    [string]$DshAsar = $env:DSHR_ASAR,
    [string]$PowerShellExe,
    [switch]$BaseSidecar
)

# M4 G1 最小 Windows 打包：编译 dshr.exe，连同 README/版本信息压成 zip。
# 不包含安装器、签名、自动更新；这些仍属后续发布工作。
$ErrorActionPreference = "Stop"
$root = Split-Path -Parent $PSScriptRoot
Set-Location $root

$nodeExe = Join-Path $root "runtime/node/node.exe"
$official = Join-Path $root "sidecar/dsh-official"
$rc5Debug = $Configuration -eq "debug" -and -not $BaseSidecar
if ($rc5Debug) {
    if (-not $DshAsar) {
        $DshAsar = Join-Path $env:LOCALAPPDATA 'Programs/DeepSeek Harness/resources/app.asar'
    }
    $DshAsar = (Resolve-Path -LiteralPath $DshAsar).Path
    $profileSource = Join-Path $official 'windows-debug-profile.json'
    $lock = Get-Content -LiteralPath (Join-Path $official 'official-rc5-packages.lock.json') -Encoding UTF8 -Raw | ConvertFrom-Json
    $packages = @($lock.packages.name | Where-Object { $_ -notin @('@deepseek-ai/dsh-tool-bash', '@deepseek-ai/dsh-tool-bash-persistent') })
    $profileNames = @((Get-Content -LiteralPath $profileSource -Encoding UTF8 -Raw | ConvertFrom-Json).plugins.name)
    $windowsTools = @($packages | Where-Object { $_ -like '@deepseek-ai/dsh-tool-*' })
    if (@($profileNames | Select-Object -Unique).Count -ne $profileNames.Count -or
        @($profileNames | Where-Object { $_ -notin $packages }).Count -gt 0 -or
        @($windowsTools | Where-Object { $_ -notin $profileNames }).Count -gt 0) {
        throw 'Windows debug profile must list each locked Windows tool exactly once and use locked support packages'
    }
    if (-not $PowerShellExe) {
        $command = Get-Command pwsh.exe -CommandType Application -ErrorAction SilentlyContinue | Select-Object -First 1
        if (-not $command) { throw 'Windows rc.5 debug packaging needs a standalone x64 PowerShell 7 installation. Pass -PowerShellExe.' }
        $PowerShellExe = $command.Source
    }
    $PowerShellExe = (Resolve-Path -LiteralPath $PowerShellExe).Path
    $powerShellHome = Split-Path -Parent $PowerShellExe
    $shellVersion = & $PowerShellExe -NoLogo -NoProfile -NonInteractive -Command '$PSVersionTable.PSVersion.ToString(); [System.Runtime.InteropServices.RuntimeInformation]::ProcessArchitecture.ToString()'
    if ($LASTEXITCODE -ne 0 -or @($shellVersion).Count -ne 2 -or
        $shellVersion[0] -notmatch '^7\.' -or $shellVersion[1] -ne 'X64') {
        throw 'Expected a standalone x64 PowerShell 7 runtime'
    }
    foreach ($file in @('pwsh.exe', 'coreclr.dll', 'pwsh.runtimeconfig.json', 'LICENSE.txt', 'ThirdPartyNotices.txt')) {
        if (-not (Test-Path -LiteralPath (Join-Path $powerShellHome $file) -PathType Leaf)) {
            throw "Missing PowerShell distribution file: $file"
        }
    }
}
$vswhere = Join-Path ([Environment]::GetEnvironmentVariable("ProgramFiles(x86)")) "Microsoft Visual Studio/Installer/vswhere.exe"
$vcRuntime = if (Test-Path -LiteralPath $vswhere -PathType Leaf) {
    & $vswhere -latest -products "*" -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -find "VC/Redist/MSVC/*/x64/Microsoft.VC*.CRT/vcruntime140.dll" |
        Select-Object -First 1
}
if (-not $vcRuntime -or -not (Test-Path -LiteralPath $vcRuntime -PathType Leaf)) {
    throw "missing x64 vcruntime140.dll in the Visual Studio redistributable directory"
}
$requiredAssets = @(
    $nodeExe,
    (Join-Path $official "sidecar-dsh.mjs")
)
if (-not $rc5Debug) {
    $requiredAssets += @('cordis', 'dsh-tool-fs', 'dsh-tool-workspace-dependencies') | ForEach-Object {
        Join-Path $official "node_modules/@deepseek-ai/$_/package.json"
    }
}
foreach ($file in $requiredAssets) {
    if (-not (Test-Path -LiteralPath $file -PathType Leaf)) { throw "missing required runtime asset: $file" }
}
if (-not $RuntimeComponentCatalog -and (Test-Path -LiteralPath "runtime/components.json")) {
    $RuntimeComponentCatalog = "runtime/components.json"
}
if ($Configuration -eq "release" -and -not $RuntimeComponentCatalog) {
    throw "Windows release packaging requires -RuntimeComponentCatalog. Run tools/build-runtime-components.py first."
}
if ($RuntimeComponentCatalog) {
    $RuntimeComponentCatalog = (Resolve-Path -LiteralPath $RuntimeComponentCatalog).Path
    $catalog = Get-Content -LiteralPath $RuntimeComponentCatalog -Raw | ConvertFrom-Json
    if ($catalog.schema_version -ne 1 -or $catalog.platform -ne "windows-x64") {
        throw "invalid Windows runtime component catalog"
    }
    foreach ($id in @("python", "creator-build")) {
        $asset = $catalog.components.$id
        if (-not $asset -or $asset.version -notmatch '^[a-zA-Z0-9][a-zA-Z0-9._-]*$' -or
            $asset.archive -notmatch '^[a-zA-Z0-9][a-zA-Z0-9._-]*\.zip$' -or
            $asset.sha256 -notmatch '^[a-fA-F0-9]{64}$' -or $asset.bytes -lt 1 -or $asset.bytes -gt 1GB) {
            throw "invalid runtime component catalog entry: $id"
        }
    }
}

$cargoArgs = @("build", "--locked", "-p", "dshr-desktop-gpui", "--bin", "dshr", "--bin", "dshr-cli")
if ($Configuration -eq "release") { $cargoArgs += "--release" }
& (Join-Path $root "tools\build-env.cmd") @(@("cargo") + $cargoArgs)
if ($LASTEXITCODE -ne 0) { throw "cargo build failed: $LASTEXITCODE" }

$binary = Join-Path $root "target/$Configuration/dshr.exe"
$console = Join-Path $root "target/$Configuration/dshr-cli.exe"
if (-not (Test-Path $binary)) { throw "missing $binary" }
if (-not (Test-Path $console)) { throw "missing $console" }
$output = [IO.Path]::GetFullPath((Join-Path $root $OutDir))
$stage = Join-Path $output "dshr-$Configuration-windows-x64"
if (Test-Path -LiteralPath $stage) {
    $target = Get-Item -LiteralPath $stage -Force
    if ($target.Parent.FullName -ne $output -or ($target.Attributes -band [IO.FileAttributes]::ReparsePoint)) {
        throw "refusing to replace package stage: $stage"
    }
    Remove-Item -LiteralPath $target.FullName -Recurse -Force
}
New-Item -ItemType Directory -Path $stage | Out-Null
Copy-Item $binary (Join-Path $stage "dshr.exe")
New-Item -ItemType Directory -Path (Join-Path $stage "bin") | Out-Null
Copy-Item -LiteralPath $console (Join-Path $stage "bin/dshr.exe")
Copy-Item -LiteralPath $vcRuntime -Destination (Join-Path $stage "vcruntime140.dll")
Copy-Item -LiteralPath $vcRuntime -Destination (Join-Path $stage "bin/vcruntime140.dll")

Write-Host "[package-desktop] node=$nodeExe"
New-Item -ItemType Directory -Path (Join-Path $stage "runtime/node") -Force | Out-Null
Copy-Item -LiteralPath $nodeExe (Join-Path $stage "runtime/node/node.exe")
if ($rc5Debug) {
    Write-Host "[package-desktop] PowerShell $($shellVersion[0])"
    Copy-Item -LiteralPath $powerShellHome -Destination (Join-Path $stage 'runtime/powershell') -Recurse
}
if ($RuntimeComponentCatalog) {
    Copy-Item -LiteralPath $RuntimeComponentCatalog (Join-Path $stage "runtime/components.json")
}

$sidecarStage = Join-Path $stage "sidecar"
New-Item -ItemType Directory -Path $sidecarStage -Force | Out-Null
$bundledOfficial = Join-Path $sidecarStage 'dsh-official'
if ($rc5Debug) {
    New-Item -ItemType Directory -Path $bundledOfficial | Out-Null
    $previousAsar = $env:DSHR_ASAR
    try {
        $env:DSHR_ASAR = $DshAsar
        & $nodeExe tools/dsh-vendor.mjs (Join-Path $bundledOfficial 'node_modules') @packages
        if ($LASTEXITCODE -ne 0) { throw 'Could not extract the Windows rc.5 sidecar dependency tree' }
        & $nodeExe tools/official-provenance.mjs --check --rc5-dir $bundledOfficial --windows-debug
        if ($LASTEXITCODE -ne 0) { throw 'Windows rc.5 package provenance check failed' }
    } finally {
        $env:DSHR_ASAR = $previousAsar
    }
    Copy-Item -LiteralPath (Join-Path $official 'sidecar-dsh.mjs'),(Join-Path $official 'lib') -Recurse -Destination $bundledOfficial
    Copy-Item -LiteralPath $profileSource -Destination (Join-Path $bundledOfficial 'default-profile.json')
} else {
    Copy-Item -LiteralPath $official -Recurse -Force -Destination $bundledOfficial
}

$componentStage = Join-Path $stage "components/cpp-component"
New-Item -ItemType Directory -Path $componentStage -Force | Out-Null
$componentWasm = Join-Path $root "crates/plugin-wasm/tests/fixtures/cpp-component-guest.wasm"
if (Test-Path -LiteralPath $componentWasm) {
    Copy-Item -LiteralPath $componentWasm (Join-Path $componentStage "plugin.wasm")
    $utf8 = New-Object System.Text.UTF8Encoding($false)
    [IO.File]::WriteAllText((Join-Path $componentStage "plugin.toml"), @"
[plugin]
id = "dshr.selftest.component"
version = "0.1.0"
api = "cordis-portable/v1"
description = "cpp-v1 wasm component selftest"

[runtime]
kind = "wasm-component"
entry = "plugin.wasm"
timeout_ms = 30000
"@, $utf8)
    [IO.File]::WriteAllText((Join-Path $componentStage "package.json"), @"
{
  "name": "dshr.selftest.component",
  "version": "0.1.0",
  "private": true
}
"@, $utf8)
}
$commit = (git rev-parse --short HEAD 2>$null)
if (-not $commit) { $commit = "dirty" }
@(
    "dshr Windows package"
    "configuration: $Configuration"
    "commit: $commit"
    "built: $((Get-Item $binary).LastWriteTime.ToString('o'))"
    ""
    "Run dshr.exe --selftest-adc-panel / --selftest-recovery / --selftest-crash-recovery / --selftest-cpp-component."
    "Run bin/dshr.exe --profile headless --help for the console entry."
    "Includes runtime/node, vendored dsh-official sidecar, and components/cpp-component."
    "Windows executables use the included app-local Visual C++ runtime."
    "No installer, code signing, or auto-update in this package."
) | Set-Content -Encoding UTF8 (Join-Path $stage "README.txt")

$zip = Join-Path $output "dshr-$Configuration-windows-x64.zip"
if (Test-Path $zip) { Remove-Item -Force $zip }
Compress-Archive -Path (Join-Path $stage "*") -DestinationPath $zip
$hash = (Get-FileHash -LiteralPath $zip -Algorithm SHA256).Hash.ToLowerInvariant()
$leaf = Split-Path $zip -Leaf
"$hash  $leaf" | Set-Content -Encoding ASCII "$zip.sha256"
Write-Host "[package-desktop] $zip"
Write-Host "[package-desktop] sha256=$hash"
