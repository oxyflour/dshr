<#
.SYNOPSIS
  滚动帧率探针：用 GPUI profiler 的 FrameTiming 测量 10k 事件会话的滚动帧率与帧耗时。

.DESCRIPTION
  1. 用 chat --seed-perf-session 造一个 10k 事件会话；
  2. 以 DSHR_FRAME_PROBE 打开窗口，探针每帧推进滚动并请求下一帧，达到目标帧数后
     自己写报告并退出（时间戳来自渲染管线，不是脚本 sleep）；
  3. 解析报告，与 docs/m2-performance-baseline.json 比较：平均 FPS 不得低于基线的 95%，
     draw_work p95 不得高于基线的 120%。

  基线缺失时用 -UpdateBaseline 写入。

.EXAMPLE
  tools\frame-probe.ps1
  tools\frame-probe.ps1 -Frames 240 -UpdateBaseline
#>
param(
    [int]$Frames = 240,
    [int]$Rows = 0,
    [string]$SessionsDir = ".tmp/perf-sessions",
    [string]$Log = ".tmp/frame-probe.txt",
    [string]$Baseline = "docs/m2-performance-baseline.json",
    [switch]$UpdateBaseline,
    [int]$TimeoutSeconds = 180
)

$ErrorActionPreference = "Stop"
$root = Split-Path $PSScriptRoot -Parent
Push-Location $root
try {
    $exe = Join-Path $root "target\release\chat.exe"
    if (-not (Test-Path $exe)) {
        $exe = Join-Path $root "target\debug\chat.exe"
    }
    if (-not (Test-Path $exe)) {
        throw "找不到 chat.exe；先构建：tools\build-env.cmd cargo build --release -p dshr-desktop-gpui --bin chat"
    }

    if (-not (Test-Path $SessionsDir) -or -not (Get-ChildItem $SessionsDir -Filter *.jsonl -ErrorAction SilentlyContinue)) {
        & $exe --seed-perf-session $SessionsDir | Write-Host
    }

    Remove-Item -Force $Log -ErrorAction SilentlyContinue
    $env:DSHR_SESSIONS_DIR = (Resolve-Path $SessionsDir).Path
    # 默认不限制行数：虚拟化保证渲染成本与历史长度无关；-Rows >0 才回到旧窗口化行为。
    if ($Rows -gt 0) { $env:DSHR_VISIBLE_LIMIT = "$Rows" } else { Remove-Item Env:DSHR_VISIBLE_LIMIT -ErrorAction SilentlyContinue }
    $env:DSHR_FRAME_PROBE = "$Frames"
    $env:DSHR_FRAME_LOG = (Join-Path $root $Log)
    $env:DSHR_FRAME_QUIT = "1"

    $proc = Start-Process -FilePath $exe -PassThru -WindowStyle Normal
    $deadline = (Get-Date).AddSeconds($TimeoutSeconds)
    while (-not $proc.HasExited -and (Get-Date) -lt $deadline) {
        Start-Sleep -Milliseconds 200
    }
    if (-not $proc.HasExited) {
        $proc.Kill()
        throw "帧探针 $TimeoutSeconds 秒内没有退出（可能首帧无法收敛）"
    }
    if (-not (Test-Path $Log)) {
        throw "帧探针没有写报告：$Log"
    }

    $text = Get-Content $Log -Raw
    Write-Host $text

    function Get-Metric([string]$name) {
        $match = [regex]::Match($text, "(?m)^\[frame-probe\] draws=(\d+) elapsed_ms=(\d+) avg_fps=([\d.]+)")
        if (-not $match.Success) { throw "报告缺少汇总行" }
        $draws = [int]$match.Groups[1].Value
        $elapsedMs = [int]$match.Groups[2].Value
        $fps = [double]$match.Groups[3].Value
        $p95 = 0.0
        $m = [regex]::Match($text, "(?m)^\[frame-probe\] $name p50_us=([\d.]+) p95_us=([\d.]+) max_us=([\d.]+)")
        if ($m.Success) { $p95 = [double]$m.Groups[2].Value }
        return [pscustomobject]@{ draws = $draws; elapsed_ms = $elapsedMs; fps = $fps; draw_p95_us = $p95 }
    }

    $measured = Get-Metric "draw_work"
    $missed = [regex]::Match($text, "missed_ratio=([\d.]+)")
    $missedRatio = if ($missed.Success) { [double]$missed.Groups[1].Value } else { 1.0 }
    $rowsMatch = [regex]::Match($text, "rows=(\d+)")
    $measuredRows = if ($rowsMatch.Success) { [int]$rowsMatch.Groups[1].Value } else { $Rows }
    $result = [pscustomobject]@{
        frames       = $measured.draws
        elapsed_ms   = $measured.elapsed_ms
        avg_fps      = $measured.fps
        draw_p95_us  = $measured.draw_p95_us
        missed_ratio = $missedRatio
        rows         = $measuredRows
        at           = (Get-Date).ToString("s")
        host         = $env:COMPUTERNAME
    }

    if ($UpdateBaseline -or -not (Test-Path $Baseline)) {
        $result | ConvertTo-Json | Set-Content -Path $Baseline -Encoding utf8
        Write-Host "[frame-probe] 基线写入 $Baseline"
        exit 0
    }

    $base = Get-Content $Baseline -Raw | ConvertFrom-Json
    $fpsFloor = [math]::Round([double]$base.avg_fps * 0.95, 2)
    $p95Ceil = [math]::Round([double]$base.draw_p95_us * 1.20, 2)
    Write-Host ("[frame-probe] baseline_fps={0} fps_floor={1} baseline_p95_us={2} p95_ceil={3}" -f `
            $base.avg_fps, $fpsFloor, $base.draw_p95_us, $p95Ceil)

    $ok = $true
    if ($measured.fps -lt $fpsFloor) {
        Write-Host "[frame-probe] FAIL: avg_fps $($measured.fps) < $fpsFloor"
        $ok = $false
    }
    if ($measured.draw_p95_us -gt $p95Ceil) {
        Write-Host "[frame-probe] FAIL: draw_work p95 $($measured.draw_p95_us)us > $p95Ceil us"
        $ok = $false
    }
    if ($ok) {
        Write-Host "[frame-probe] PASS"
        exit 0
    }
    exit 1
}
finally {
    Remove-Item Env:DSHR_SESSIONS_DIR -ErrorAction SilentlyContinue
    Remove-Item Env:DSHR_VISIBLE_LIMIT -ErrorAction SilentlyContinue
    Remove-Item Env:DSHR_FRAME_PROBE -ErrorAction SilentlyContinue
    Remove-Item Env:DSHR_FRAME_LOG -ErrorAction SilentlyContinue
    Remove-Item Env:DSHR_FRAME_QUIT -ErrorAction SilentlyContinue
    Pop-Location
}
