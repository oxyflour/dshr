# Build a development distribution zip: dist/dshr-dev.zip
#
# Contents:
#   chat.exe            Rust desktop binary (chat / selftest entry point, GUI subsystem in release)
#   sidecar/            Node DSH sidecar + vendored official plugins (node_modules)
#   runtime/node        Vendored Node runtime (from tools/vendor-node.mjs)
#   docs/               M0-M2 documents
#   README.md
#   RUNNING.txt         double-click instructions, data root, unsigned-build notes
#   MANIFEST.txt        build metadata (git commit, node version, timestamp)
#
# Usage:
#   pwsh tools/package-dev.ps1                 # build release chat.exe + package
#   pwsh tools/package-dev.ps1 -SkipBuild      # reuse target/release/chat.exe
#   pwsh tools/package-dev.ps1 -Configuration debug
#
# Prerequisites: run `node tools/vendor-node.mjs` and `node tools/dsh-vendor.mjs`
# once (both are local-only; no network).
#
# The script never publishes anything: it only writes dist/ (gitignored).

[CmdletBinding()]
param(
  [ValidateSet('release', 'debug')][string]$Configuration = 'release',
  [switch]$SkipBuild
)

$ErrorActionPreference = 'Stop'

$root = Split-Path -Parent $PSScriptRoot
$dist = Join-Path $root 'dist'
$stage = Join-Path $dist 'dshr-dev'
$runtimeNode = Join-Path $root 'runtime/node/node.exe'
$sidecarModules = Join-Path $root 'sidecar/dsh-official/node_modules'

function Write-Step([string]$Message) {
  Write-Host "[package] $Message"
}

# --- prerequisites ---------------------------------------------------------

if (-not (Test-Path $runtimeNode)) {
  throw "missing runtime/node/node.exe; run: node tools/vendor-node.mjs"
}
if (-not (Test-Path $sidecarModules)) {
  throw "missing sidecar/dsh-official/node_modules; run: node tools/dsh-vendor.mjs (needs local DSH install)"
}

# --- build ----------------------------------------------------------------

if (-not $SkipBuild) {
  Write-Step "cargo build -p dshr-desktop-gpui --bin chat ($Configuration)"
  $cargoArgs = @('cargo', 'build', '-p', 'dshr-desktop-gpui', '--bin', 'chat')
  if ($Configuration -eq 'release') { $cargoArgs += '--release' }
  & (Join-Path $PSScriptRoot 'build-env.cmd') @cargoArgs
  if ($LASTEXITCODE -ne 0) { throw "cargo build failed (exit $LASTEXITCODE)" }
}

$exe = Join-Path $root "target/$Configuration/chat.exe"
if (-not (Test-Path $exe)) {
  throw "missing $exe; run again without -SkipBuild"
}
if ($Configuration -eq 'debug') {
  Write-Step "note: debug build uses the Console subsystem; double-click will show a console window (release is GUI-only)"
}

# --- stage ----------------------------------------------------------------

if (Test-Path $stage) { Remove-Item -Recurse -Force $stage }
New-Item -ItemType Directory -Force -Path $stage | Out-Null
Write-Step "staging -> $stage"

Copy-Item $exe (Join-Path $stage 'chat.exe')
Copy-Item (Join-Path $root 'sidecar') (Join-Path $stage 'sidecar') -Recurse
Copy-Item (Join-Path $root 'runtime') (Join-Path $stage 'runtime') -Recurse
Copy-Item (Join-Path $root 'docs') (Join-Path $stage 'docs') -Recurse
Copy-Item (Join-Path $root 'README.md') (Join-Path $stage 'README.md')

# Keep the probe sidecar scripts (two small .mjs files): package verification uses
# `chat.exe --selftest`, which exercises the probe sidecar. Removing them makes the
# smoke test fail (Node cannot find probe-sidecar.mjs).

# --- manifest -------------------------------------------------------------

$gitCommit = '<unknown>'
try {
  $gitCommit = (& git -C $root rev-parse --short HEAD 2>$null).Trim()
} catch {
  $gitCommit = '<not a git checkout>'
}
$nodeVersion = '<unknown>'
try {
  $nodeVersion = (Get-Content -Raw (Join-Path $root 'runtime/node/RUNTIME.json') | ConvertFrom-Json).node
} catch {
  $nodeVersion = '<RUNTIME.json missing>'
}

$manifest = @(
  "dshr dev package",
  "builtAt: $((Get-Date).ToUniversalTime().ToString('o'))",
  "configuration: $Configuration",
  "gitCommit: $gitCommit",
  "node: $nodeVersion",
  "",
  "notes:",
  "- sidecar needs the vendored Node runtime under runtime/node (DSHR_NODE can override).",
  "- release chat.exe is a Windows GUI-subsystem binary: double-click opens no console window.",
  "- state is portable: written next to the exe when that directory is writable, otherwise under %LOCALAPPDATA%\dshr; DSHR_HOME overrides. It does not depend on the launch CWD.",
  "- plugins installed into sidecar/dsh-official/node_modules; profile lives under the data root (profile/plugins.json).",
  "- this is a development build: no signature, no installer, no auto-update."
)
Set-Content -Path (Join-Path $stage 'MANIFEST.txt') -Value $manifest -Encoding UTF8

# --- running notes (user-facing, Chinese) ---------------------------------

$running = @(
  "dshr 开发构建：启动与数据目录",
  "",
  "1. 双击 chat.exe 启动（release 构建是 Windows GUI 子系统，不会弹出命令行窗口）。",
  "2. 应用数据默认写在 exe 目录旁（便携包布局：sessions/ profile/ spill/ crash.log）；",
  "   该目录不可写时退回 %LOCALAPPDATA%\dshr。与快捷方式的「起始位置」无关，",
  "   可用环境变量 DSHR_HOME 指定其它位置。",
  "3. 本包是未签名的开发构建：SmartScreen / Defender 可能提示风险。",
  "   确认来源可信后，可右键文件属性勾选「解除锁定」，或在 PowerShell 执行 Unblock-File .\chat.exe 后重试。",
  "   不要关闭系统的安全防护。",
  "4. 命令行自检：在 PowerShell 中进入本目录，运行 .\chat.exe --selftest，",
  "   应看到 [selftest] PASS（CLI 用法会挂到当前控制台输出）。"
)
Set-Content -Path (Join-Path $stage 'RUNNING.txt') -Value $running -Encoding UTF8

# --- zip ------------------------------------------------------------------

New-Item -ItemType Directory -Force -Path $dist | Out-Null
$zip = Join-Path $dist 'dshr-dev.zip'
if (Test-Path $zip) { Remove-Item $zip -Force }
Write-Step "compressing -> $zip (Node runtime is ~90 MB, this can take a while)"
Compress-Archive -Path (Join-Path $stage '*') -DestinationPath $zip -CompressionLevel Optimal

$sizeMb = [math]::Round((Get-Item $zip).Length / 1MB, 1)
$stagedMb = [math]::Round(((Get-ChildItem $stage -Recurse -File | Measure-Object -Property Length -Sum).Sum) / 1MB, 1)
Write-Step "done: $zip ($sizeMb MiB, staged $stagedMb MiB)"
Write-Step "verify: Expand-Archive $zip, read RUNNING.txt, then double-click chat.exe (or run chat.exe --selftest)"
