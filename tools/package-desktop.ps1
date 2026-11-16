param(
    [ValidateSet("debug", "release")]
    [string]$Configuration = "debug",
    [string]$OutDir = "dist"
)

# M4 G1 最小 Windows 打包：编译 chat.exe，连同 README/版本信息压成 zip。
# 不包含安装器、签名、自动更新；这些仍属后续发布工作。
$ErrorActionPreference = "Stop"
$root = Split-Path -Parent $PSScriptRoot
Set-Location $root

$cargoArgs = @("build", "-p", "dshr-desktop-gpui", "--bin", "chat")
if ($Configuration -eq "release") { $cargoArgs += "--release" }
& (Join-Path $root "tools\build-env.cmd") @(@("cargo") + $cargoArgs)
if ($LASTEXITCODE -ne 0) { throw "cargo build failed: $LASTEXITCODE" }

$binary = Join-Path $root "target/$Configuration/chat.exe"
if (-not (Test-Path $binary)) { throw "missing $binary" }
$stage = Join-Path $root "$OutDir/dshr-$Configuration-windows-x64"
if (Test-Path $stage) { Remove-Item -Recurse -Force $stage }
New-Item -ItemType Directory -Path $stage | Out-Null
Copy-Item $binary (Join-Path $stage "chat.exe")

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

$commit = (git rev-parse --short HEAD 2>$null)
if (-not $commit) { $commit = "dirty" }
@(
    "dshr Windows package"
    "configuration: $Configuration"
    "commit: $commit"
    "built: $((Get-Item $binary).LastWriteTime.ToString('o'))"
    ""
    "Run chat.exe --selftest-adc-panel / --selftest-recovery / --selftest-crash-recovery."
    "Includes runtime/node, probe sidecar, and vendored dsh-official sidecar."
    "No installer, code signing, or auto-update in this package."
) | Set-Content -Encoding UTF8 (Join-Path $stage "README.txt")

$zip = Join-Path $root "$OutDir/dshr-$Configuration-windows-x64.zip"
if (Test-Path $zip) { Remove-Item -Force $zip }
Compress-Archive -Path (Join-Path $stage "*") -DestinationPath $zip
Write-Host "[package-desktop] $zip"
