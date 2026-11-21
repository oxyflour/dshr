param(
    [ValidateSet("debug", "release")]
    [string]$Configuration = "debug",
    [string]$OutDir = "dist"
)

# M4 G1 最小 Windows 打包：编译 dshr.exe，连同 README/版本信息压成 zip。
# 不包含安装器、签名、自动更新；这些仍属后续发布工作。
$ErrorActionPreference = "Stop"
$root = Split-Path -Parent $PSScriptRoot
Set-Location $root

$cargoArgs = @("build", "-p", "dshr-desktop-gpui", "--bin", "dshr", "--bin", "dshr-cli")
if ($Configuration -eq "release") { $cargoArgs += "--release" }
& (Join-Path $root "tools\build-env.cmd") @(@("cargo") + $cargoArgs)
if ($LASTEXITCODE -ne 0) { throw "cargo build failed: $LASTEXITCODE" }

$binary = Join-Path $root "target/$Configuration/dshr.exe"
$console = Join-Path $root "target/$Configuration/dshr-cli.exe"
if (-not (Test-Path $binary)) { throw "missing $binary" }
if (-not (Test-Path $console)) { throw "missing $console" }
$stage = Join-Path $root "$OutDir/dshr-$Configuration-windows-x64"
if (Test-Path $stage) { Remove-Item -Recurse -Force $stage }
New-Item -ItemType Directory -Path $stage | Out-Null
Copy-Item $binary (Join-Path $stage "dshr.exe")
New-Item -ItemType Directory -Path (Join-Path $stage "bin") | Out-Null
Copy-Item -LiteralPath $console (Join-Path $stage "bin/dshr.exe")

$nodeExe = "$root\runtime\node\node.exe"
Write-Host "[package-desktop] node=$nodeExe"
if (Test-Path -LiteralPath $nodeExe) {
    New-Item -ItemType Directory -Path (Join-Path $stage "runtime\node") -Force | Out-Null
    Copy-Item -LiteralPath $nodeExe (Join-Path $stage "runtime\node\node.exe")
} else {
    Write-Warning "[package-desktop] 缺少 node runtime，包将进入 Native-only 模式"
}

$sidecarStage = Join-Path $stage "sidecar"
New-Item -ItemType Directory -Path $sidecarStage -Force | Out-Null
foreach ($name in @("probe-sidecar.mjs", "probe-plugin.mjs")) {
    $src = Join-Path $root "sidecar/$name"
    if (Test-Path $src) { Copy-Item $src (Join-Path $sidecarStage $name) }
}
$official = Join-Path $root "sidecar/dsh-official"
if (Test-Path $official) {
    Copy-Item -Recurse -Force $official (Join-Path $sidecarStage "dsh-official")
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
    "Includes runtime/node, probe sidecar, vendored dsh-official sidecar, and components/cpp-component."
    "No installer, code signing, or auto-update in this package."
) | Set-Content -Encoding UTF8 (Join-Path $stage "README.txt")

$zip = Join-Path $root "$OutDir/dshr-$Configuration-windows-x64.zip"
if (Test-Path $zip) { Remove-Item -Force $zip }
Compress-Archive -Path (Join-Path $stage "*") -DestinationPath $zip
$hash = (Get-FileHash -LiteralPath $zip -Algorithm SHA256).Hash.ToLowerInvariant()
$leaf = Split-Path $zip -Leaf
"$hash  $leaf" | Set-Content -Encoding ASCII "$zip.sha256"
Write-Host "[package-desktop] $zip"
Write-Host "[package-desktop] sha256=$hash"
