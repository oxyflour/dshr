# 官方包 L2 gate（P0：官方包证据不可空转）
#
# 默认 `cargo test -p dshr-compat-dsh-host` 不会跑 official_packages（测试标了
# `#[ignore]`），因此无 fixture 的机器上它们显示 ignored，绝不显示 passed。
# 本脚本是 gate 的显式命令：先检查 fixture，再跑 --ignored；任何缺失/失败/SKIP 都 exit 1。
#
# 用法：
#   powershell -ExecutionPolicy Bypass -File tools/official-l2-gate.ps1
#
# 前置（fixture 获取方法）：
#   - .tmp/dsh-tools-all/  ：从本机 DSH 安装抽取的 27 个官方工具包（见 docs/m3-l2-services.md §9）
#   - sidecar/dsh-official/node_modules：node tools/dsh-vendor.mjs（需要本机 DSH 安装）
# 两者都不进仓库（gitignored/离线）。

$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
Set-Location $root

function Fail([string]$message) {
    Write-Host "[official-l2-gate] FAIL: $message" -ForegroundColor Red
    exit 1
}

Write-Host "[official-l2-gate] root=$root"

if (-not (Get-Command node -ErrorAction SilentlyContinue)) {
    Fail "PATH 上找不到 node（官方 sidecar 与 gated 测试都需要）"
}

$toolsDir = Join-Path $root '.tmp/dsh-tools-all'
foreach ($pkg in @('dsh-tool-working-directory', 'dsh-tool-todo')) {
    if (-not (Test-Path (Join-Path $toolsDir $pkg))) {
        Fail "缺少 .tmp/dsh-tools-all/$pkg。获取方法：从本机 DSH 安装抽取（docs/m3-l2-services.md §9，.tmp/ 已 gitignored）"
    }
}

$sidecar = Join-Path $root 'sidecar/dsh-official'
foreach ($rel in @('sidecar-dsh.mjs', 'node_modules', 'node_modules/@deepseek-ai/dsh-storage-domain', 'lib/official-l2-driver.mjs')) {
    if (-not (Test-Path (Join-Path $sidecar $rel))) {
        $hint = if ($rel -like 'node_modules*') { '先运行 node tools/dsh-vendor.mjs' } else { '仓库文件缺失' }
        Fail "缺少 sidecar/dsh-official/$rel（$hint）"
    }
}

Write-Host "[official-l2-gate] fixtures OK；运行 gated 官方包测试（--ignored）"
# Native stderr（cargo 的 Compiling 等）在 Windows PowerShell 下会变成 ErrorRecord；
# 这里临时放宽 EAP，避免把正常编译输出当成脚本异常。
$ErrorActionPreference = 'Continue'
$output = & (Join-Path $root 'tools\build-env.cmd') cargo test --offline -p dshr-compat-dsh-host --test official_packages -- --ignored --nocapture 2>&1
$code = $LASTEXITCODE
$ErrorActionPreference = 'Stop'
$output | ForEach-Object { $_ }
if ($code -ne 0) {
    Fail "official_packages 有测试失败（cargo exit $code）"
}

$text = ($output | Out-String)
if ($text -match 'SKIP') {
    Fail "输出包含 SKIP：官方包 gate 不允许静默跳过（测试必须 fail 或由本脚本前置失败）"
}
if ($text -notmatch '(\d+) passed') {
    Fail "无法从输出解析 passed 数量（gate 结果不可判定）"
}
$passed = [int]$Matches[1]
$expected = 6  # RPC-L2-201 + 501..505
if ($passed -lt $expected) {
    Fail "只跑了 $passed 个 gated 测试（期望 $expected：RPC-L2-201 + 501..505）"
}

Write-Host "[official-l2-gate] PASS（$passed gated tests）" -ForegroundColor Green
exit 0
