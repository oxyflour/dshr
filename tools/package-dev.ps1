# Build a development distribution zip: dist/dshr-dev.zip
#
# Contents:
#   chat.exe            Rust desktop binary (chat / selftest entry point)
#   sidecar/            Node DSH sidecar + vendored official plugins (node_modules)
#   runtime/node        Vendored Node runtime (from tools/vendor-node.mjs)
#   docs/               M0-M2 documents
#   README.md
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

# --- stage ----------------------------------------------------------------

if (Test-Path $stage) { Remove-Item -Recurse -Force $stage }
New-Item -ItemType Directory -Force -Path $stage | Out-Null
Write-Step "staging -> $stage"

Copy-Item $exe (Join-Path $stage 'chat.exe')
Copy-Item (Join-Path $root 'sidecar') (Join-Path $stage 'sidecar') -Recurse
Copy-Item (Join-Path $root 'runtime') (Join-Path $stage 'runtime') -Recurse
Copy-Item (Join-Path $root 'docs') (Join-Path $stage 'docs') -Recurse
Copy-Item (Join-Path $root 'README.md') (Join-Path $stage 'README.md')

# Probe scripts are developer-only; keep the package lean.
foreach ($relative in @('sidecar/probe-sidecar.mjs', 'sidecar/probe-plugin.mjs')) {
  $path = Join-Path $stage $relative
  if (Test-Path $path) { Remove-Item $path -Force }
}

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
  "- plugins installed into sidecar/dsh-official/node_modules; profile lives in .tmp/profile by default.",
  "- this is a development build: no signature, no installer, no auto-update."
)
Set-Content -Path (Join-Path $stage 'MANIFEST.txt') -Value $manifest -Encoding UTF8

# --- zip ------------------------------------------------------------------

New-Item -ItemType Directory -Force -Path $dist | Out-Null
$zip = Join-Path $dist 'dshr-dev.zip'
if (Test-Path $zip) { Remove-Item $zip -Force }
Write-Step "compressing -> $zip (Node runtime is ~90 MB, this can take a while)"
Compress-Archive -Path (Join-Path $stage '*') -DestinationPath $zip -CompressionLevel Optimal

$sizeMb = [math]::Round((Get-Item $zip).Length / 1MB, 1)
$stagedMb = [math]::Round(((Get-ChildItem $stage -Recurse -File | Measure-Object -Property Length -Sum).Sum) / 1MB, 1)
Write-Step "done: $zip ($sizeMb MiB, staged $stagedMb MiB)"
Write-Step "verify: Expand-Archive $zip, then run chat.exe --selftest"
