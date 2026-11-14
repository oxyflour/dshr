#!/usr/bin/env pwsh
# G2 外部作者套件 E2E：生成 → 构建 → 打包 → 安装 → 升级 → 降级/篡改/未确认反例。
#
# 运行（Windows PowerShell 5.1 / pwsh；默认执行策略会拦截未签名脚本，需显式 Bypass）：
#   powershell -NoProfile -ExecutionPolicy Bypass -File tools\authoring-check.ps1
#   （可选 -SkipBuild 跳过 cargo build，复用 .tmp/authoring-target 缓存）
#
# 所有生成物都在 .tmp/authoring（gitignored），脚本可重复执行；任一步失败则非零退出。
# 依赖：node、target\debug\chat.exe（不存在时尝试 cargo build 一次）、crates/plugin-sdk。

param(
    [switch]$SkipBuild,
    [switch]$KeepWork
)

# 说明：native 命令（cargo/node/chat）会把 stderr 变成 ErrorRecord；
# 本脚本以显式退出码/断言判定失败，因此用 Continue 而不是 Stop（Stop 会在 cargo 打日志时中断）。
$ErrorActionPreference = 'Continue'
Set-StrictMode -Version Latest

$repo = Split-Path -Parent $PSScriptRoot
$work = Join-Path $repo '.tmp\authoring'
$sample = Join-Path $work 'sample'
$targetCache = Join-Path $repo '.tmp\authoring-target'
$profileDir = Join-Path $work 'profile'
$sidecarDir = Join-Path $work 'sidecar'
$homeDir = Join-Path $work 'home'
$pluginId = 'demo.authoring'
$crateName = 'demo-authoring'
$binName = "$crateName.exe"
$failures = 0

function Write-Step([string]$message) {
    Write-Host "`n=== $message ===" -ForegroundColor Cyan
}

function Assert-True([bool]$condition, [string]$message) {
    if ($condition) {
        Write-Host "  [ok] $message" -ForegroundColor Green
    } else {
        Write-Host "  [FAIL] $message" -ForegroundColor Red
        $script:failures += 1
    }
}

function Assert-Contains([string]$text, [string]$needle, [string]$message) {
    Assert-True ($text -like "*$needle*") $message
}

function Get-ChangedText([string]$path, [string]$pattern, [string]$replacement) {
    # 用 .NET IO 读写：Windows PowerShell 5.1 的 Get-Content 会把无 BOM 的 UTF-8 当 ANSI 解码。
    $text = [System.IO.File]::ReadAllText($path)
    if ($text -notmatch $pattern) {
        throw "在 $path 中找不到要替换的内容: $pattern"
    }
    $updated = $text -replace $pattern, $replacement
    [System.IO.File]::WriteAllText($path, $updated, (New-Object System.Text.UTF8Encoding($false)))
}

function Set-PluginVersion([string]$version) {
    Get-ChangedText (Join-Path $sample 'plugin.toml') '(?m)^version = "[0-9]+\.[0-9]+\.[0-9]+"$' "version = `"$version`""
    Get-ChangedText (Join-Path $sample 'package.json') '"version": "[0-9]+\.[0-9]+\.[0-9]+"' "`"version`": `"$version`""
}

function Invoke-Pack([string]$outDir) {
    if (Test-Path $outDir) { Remove-Item -LiteralPath $outDir -Recurse -Force }
    $output = & node (Join-Path $repo 'tools\plugin-pack.mjs') --plugin $sample --out $outDir --note 'authoring-check' 2>&1
    $code = $LASTEXITCODE
    $output | ForEach-Object { Write-Host "    $_" }
    if ($code -ne 0) { throw "plugin-pack 失败（退出码 $code）" }
    $digestLine = ($output | Select-String -Pattern 'digest_sha256=([0-9a-f]{64})' | Select-Object -First 1)
    if (-not $digestLine) { throw 'plugin-pack 输出里没有摘要' }
    return ($digestLine.Matches[0].Groups[1].Value)
}

function Invoke-ChatInstall([string]$pkgDir, [string[]]$extraArgs = @()) {
    $chat = Join-Path $repo 'target\debug\chat.exe'
    $argsList = @('--install-plugin', $pkgDir, '--yes-install') + $extraArgs
    $output = & $chat @argsList 2>&1
    $code = $LASTEXITCODE
    $output | ForEach-Object { Write-Host "    $_" }
    return @{ Code = $code; Output = ($output -join "`n") }
}

function Read-AuditRecords {
    $path = Join-Path $profileDir 'install-audit.jsonl'
    if (-not (Test-Path $path)) { return @() }
    return @(Get-Content -LiteralPath $path | Where-Object { $_.Trim() -ne '' } | ForEach-Object { $_ | ConvertFrom-Json })
}

# ---------------------------------------------------------------------------
Write-Step '0) 准备'
if (Test-Path $work) { Remove-Item -LiteralPath $work -Recurse -Force }
New-Item -ItemType Directory -Path $work, $targetCache, $homeDir -Force | Out-Null
$env:DSHR_PROFILE_DIR = $profileDir
$env:DSHR_SIDECAR_DIR = $sidecarDir
$env:DSHR_HOME = $homeDir
$env:CARGO_TARGET_DIR = $targetCache
Write-Host "  repo=$repo"
Write-Host "  work=$work"

$chat = Join-Path $repo 'target\debug\chat.exe'
if (-not (Test-Path $chat)) {
    Write-Step '0b) 构建 chat.exe'
    & (Join-Path $repo 'tools\build-env.cmd') cargo build --offline -p dshr-desktop-gpui --bin chat
    if ($LASTEXITCODE -ne 0) { Write-Host '[FAIL] 无法构建 chat.exe' -ForegroundColor Red; exit 1 }
}

Write-Step '1) new-plugin 生成仓库外样例'
$genOutput = & node (Join-Path $repo 'tools\new-plugin.mjs') --out $sample --id $pluginId --name 'Authoring Check' 2>&1
$genOutput | ForEach-Object { Write-Host "    $_" }
if ($LASTEXITCODE -ne 0) { Write-Host '[FAIL] new-plugin 失败' -ForegroundColor Red; exit 1 }
foreach ($relative in @('Cargo.toml', 'plugin.toml', 'package.json', 'src\main.rs', 'README.md', '.gitignore')) {
    Assert-True (Test-Path (Join-Path $sample $relative)) "生成文件存在: $relative"
}
$allText = (Get-ChildItem $sample -Recurse -File | Where-Object { $_.Extension -in '.toml', '.json', '.rs', '.md' } | ForEach-Object { Get-Content $_.FullName -Raw }) -join "`n"
Assert-True (-not $allText.Contains('__DSHR_REPO__')) '模板占位符已全部替换'
Assert-True ($allText.Contains($pluginId)) "生成内容包含 plugin_id=$pluginId"

Write-Step '2) cargo build --release（独立 workspace，SDK path 依赖）'
if (-not $SkipBuild) {
    Push-Location $sample
    try {
        $buildOutput = & (Join-Path $repo 'tools\build-env.cmd') cargo build --release --offline 2>&1
        $buildCode = $LASTEXITCODE
    } finally {
        Pop-Location
    }
    ($buildOutput | Select-Object -Last 8) | ForEach-Object { Write-Host "    $_" }
    if ($buildCode -ne 0) { Write-Host '[FAIL] 样例构建失败' -ForegroundColor Red; exit 1 }
} else {
    Write-Host '  (跳过构建，复用缓存)'
}
$builtExe = Join-Path $targetCache "release\$binName"
Assert-True (Test-Path $builtExe) "构建产物存在: target/release/$binName"
Copy-Item -LiteralPath $builtExe -Destination (Join-Path $sample "bin\$binName") -Force
Assert-True (Test-Path (Join-Path $sample "bin\$binName")) "可执行文件已复制到 bin/$binName"

Write-Step '3) plugin-pack 打包 0.1.0 并校验摘要可复现'
$pkg1 = Join-Path $work 'pkg-0.1.0'
$digest1 = Invoke-Pack $pkg1
Assert-True (Test-Path (Join-Path $pkg1 'dshr-trust.json')) '已生成 dshr-trust.json'
$trust1 = Get-Content (Join-Path $pkg1 'dshr-trust.json') -Raw | ConvertFrom-Json
Assert-True ($trust1.plugin_id -eq $pluginId) "trust plugin_id=$pluginId"
Assert-True ($trust1.version -eq '0.1.0') 'trust version=0.1.0'
Assert-True ($trust1.package.digest_sha256 -eq $digest1) 'trust 摘要与打印摘要一致'
$verify = & node (Join-Path $repo 'tools\plugin-pack.mjs') --verify $pkg1 --digest $digest1 2>&1
Assert-True ($LASTEXITCODE -eq 0) 'plugin-pack --verify 摘要自校验通过'

Write-Step '4) chat --install-plugin 安装 0.1.0（摘要匹配 = JS/Rust 算法一致）'
$install1 = Invoke-ChatInstall $pkg1
Assert-True ($install1.Code -eq 0) "安装退出码 0（实际 $($install1.Code)）"
$destination = Join-Path $sidecarDir "node_modules\$pluginId"
Assert-True (Test-Path (Join-Path $destination 'plugin.toml')) "包已落盘: node_modules\$pluginId"
Assert-True (Test-Path (Join-Path $destination "bin\$binName")) "包内含 $binName"
$metaPath = Join-Path $destination '.dshr-install.json'
$meta = Get-Content $metaPath -Raw | ConvertFrom-Json
Assert-True ($meta.version -eq '0.1.0') '已安装元数据 version=0.1.0'
Assert-True ($meta.digest_sha256 -eq $digest1) '已安装元数据摘要与包摘要一致（Rust 重算 = JS 计算）'
$profileText = Get-Content (Join-Path $profileDir 'plugins.json') -Raw
Assert-Contains $profileText $pluginId 'plugins.json 含插件条目'
$audit1 = Read-AuditRecords
Assert-True ((@($audit1 | Where-Object { $_.decision -eq 'accepted' -and $_.name -eq $pluginId })).Count -ge 1) '审计含 accepted'

Write-Step '5) 升级到 0.2.0'
Set-PluginVersion '0.2.0'
$pkg2 = Join-Path $work 'pkg-0.2.0'
$digest2 = Invoke-Pack $pkg2
$install2 = Invoke-ChatInstall $pkg2
Assert-True ($install2.Code -eq 0) "升级安装退出码 0（实际 $($install2.Code)）"
$meta2 = Get-Content $metaPath -Raw | ConvertFrom-Json
Assert-True ($meta2.version -eq '0.2.0') '升级后元数据 version=0.2.0'
$audit2 = Read-AuditRecords
Assert-True ((@($audit2 | Where-Object { $_.decision -eq 'accepted' })).Count -ge 2) '审计 accepted ≥ 2（首装 + 升级）'

Write-Step '6) 降级反例：重装 0.1.0 必须被拒（E_TRUST_DOWNGRADE）'
$downgrade = Invoke-ChatInstall $pkg1
Assert-True ($downgrade.Code -ne 0) "降级安装非零退出（实际 $($downgrade.Code)）"
Assert-Contains $downgrade.Output 'E_TRUST_DOWNGRADE' '输出含 E_TRUST_DOWNGRADE'
$metaAfterDowngrade = Get-Content $metaPath -Raw | ConvertFrom-Json
Assert-True ($metaAfterDowngrade.version -eq '0.2.0') '降级被拒后已安装版本仍是 0.2.0'
$audit3 = Read-AuditRecords
Assert-True ((@($audit3 | Where-Object { $_.decision -eq 'rejected' -and $_.version -eq '0.1.0' })).Count -ge 1) '审计含 rejected（0.1.0 降级）'

Write-Step '7) 篡改反例：改包内容后重装必须被拒（E_INTEGRITY_MISMATCH）'
$pkgTampered = Join-Path $work 'pkg-tampered'
Copy-Item -LiteralPath $pkg2 -Destination $pkgTampered -Recurse -Force
Add-Content -LiteralPath (Join-Path $pkgTampered 'README.md') -Value "`n<!-- tampered -->"
$tamper = Invoke-ChatInstall $pkgTampered
Assert-True ($tamper.Code -ne 0) "篡改包安装非零退出（实际 $($tamper.Code)）"
Assert-Contains $tamper.Output 'E_INTEGRITY_MISMATCH' '输出含 E_INTEGRITY_MISMATCH'

Write-Step '8) 未确认反例：未签名包不带 --yes-install 必须拒绝（退出码 2）'
Set-PluginVersion '0.3.0'
$pkg3 = Join-Path $work 'pkg-0.3.0'
$null = Invoke-Pack $pkg3
$chat = Join-Path $repo 'target\debug\chat.exe'
$unconfirmedOutput = & $chat --install-plugin $pkg3 2>&1
$unconfirmedCode = $LASTEXITCODE
$unconfirmedOutput | ForEach-Object { Write-Host "    $_" }
Assert-True ($unconfirmedCode -eq 2) "未确认安装退出码 2（实际 $($unconfirmedCode)）"
$metaAfterUnconfirmed = Get-Content $metaPath -Raw | ConvertFrom-Json
Assert-True ($metaAfterUnconfirmed.version -eq '0.2.0') '未确认安装未改变已安装版本'

# ---------------------------------------------------------------------------
Write-Step '结果'
if ($failures -eq 0) {
    Write-Host "[authoring-check] PASS（生成/构建/打包/安装/升级/降级拒绝/篡改拒绝/未确认拒绝）" -ForegroundColor Green
    Write-Host "  产物目录: $work"
    exit 0
} else {
    Write-Host "[authoring-check] FAIL: $failures 项断言失败" -ForegroundColor Red
    exit 1
}
