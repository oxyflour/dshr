<#
.SYNOPSIS
  滚动帧率探针：用 GPUI profiler 的 FrameTiming 测量 10k 事件会话的滚动帧率与帧耗时。

.DESCRIPTION
  1. 用 chat --seed-perf-session 造一个 10k 事件会话；
  2. 以 DSHR_FRAME_PROBE 打开窗口，探针每帧推进滚动并请求下一帧，达到目标帧数后
     自己写报告并退出（时间戳来自渲染管线，不是脚本 sleep）；
  3. 记录并输出完整测量元数据（host / 会话名 / GPU / 二进制路径 / git commit 等），
     元数据与数字一起写进 baseline，避免手工维护；
  4. 判定分两层（环境诚实）：
     - 硬性（与基线无关）：missed_ratio 必须为 0，且 draw_work p95 不超过 refresh budget
       （budget_us = refresh_ns / 1000，取自探针报告；报告缺 refresh_ns 时回退 31250us，
       即 32fps 预算）；
     - 相对基线：仅当 baseline.host 与当前主机一致（或显式 -CompareAcrossHosts）时，
       要求 avg_fps >= 基线 95% 且 draw_work p95 <= 基线 120%；主机不一致时打印 WARN
       并跳过相对判定，数字仍完整记录。
  5. -UpdateBaseline：硬性条件通过后写入带完整元数据的基线（不会用失败的数字覆盖基线）。

.PARAMETER CompareAcrossHosts
  显式允许跨主机比较相对基线。不同机器/会话（例如本地控制台 vs RDP）的 draw 数字
  受合成器影响，跨主机比较仅作参考。

.EXAMPLE
  tools\frame-probe.ps1
  tools\frame-probe.ps1 -Frames 240 -UpdateBaseline
  tools\frame-probe.ps1 -Frames 240 -CompareAcrossHosts
#>
param(
    [int]$Frames = 240,
    [int]$Rows = 0,
    [string]$SessionsDir = ".tmp/perf-sessions",
    [string]$Log = ".tmp/frame-probe.txt",
    [string]$Baseline = "docs/m2-performance-baseline.json",
    [switch]$UpdateBaseline,
    [switch]$CompareAcrossHosts,
    [int]$TimeoutSeconds = 180
)

$ErrorActionPreference = "Stop"
$root = Split-Path $PSScriptRoot -Parent

# 相对仓库根的路径（baseline 里记录的是可移植的相对路径）。
function ConvertTo-RepoRelativePath([string]$path) {
    $relative = $path
    if ($relative.StartsWith($root, [StringComparison]::OrdinalIgnoreCase)) {
        $relative = $relative.Substring($root.Length)
    }
    return ($relative.TrimStart('\', '/') -replace '\\', '/')
}

# 当前会话实际使用的显示适配器 + 全部适配器清单（RDP 下会看到虚拟显示适配器）。
function Get-GpuMetadata {
    try {
        $adapters = @(Get-CimInstance Win32_VideoController -ErrorAction Stop)
    }
    catch {
        return $null
    }
    if ($adapters.Count -eq 0) {
        return $null
    }
    $withDisplay = @($adapters | Where-Object { $_.CurrentHorizontalResolution -gt 0 })
    $active = $adapters[0]
    if ($withDisplay.Count -gt 0) {
        $active = $withDisplay[0]
    }
    $resolution = "unknown"
    if ($null -ne $active.CurrentHorizontalResolution -and $null -ne $active.CurrentVerticalResolution) {
        $resolution = "{0}x{1}" -f $active.CurrentHorizontalResolution, $active.CurrentVerticalResolution
    }
    return [pscustomobject][ordered]@{
        name          = [string]$active.Name
        driverVersion = [string]$active.DriverVersion
        resolution    = $resolution
        adapters      = @($adapters | ForEach-Object {
                [pscustomobject][ordered]@{
                    name          = [string]$_.Name
                    driverVersion = [string]$_.DriverVersion
                }
            })
    }
}

# 解析报告里某一组阶段耗时（p50/p95/max 微秒）。
function Get-PhaseMetrics([string]$text, [string]$name) {
    $pattern = "(?m)^\[frame-probe\] " + [regex]::Escape($name) + " p50_us=([\d.]+) p95_us=([\d.]+) max_us=([\d.]+)"
    $match = [regex]::Match($text, $pattern)
    if (-not $match.Success) {
        return [pscustomobject]@{ p50_us = 0.0; p95_us = 0.0; max_us = 0.0 }
    }
    return [pscustomobject]@{
        p50_us = [double]$match.Groups[1].Value
        p95_us = [double]$match.Groups[2].Value
        max_us = [double]$match.Groups[3].Value
    }
}

Push-Location $root
try {
    $exe = Join-Path $root "target\release\chat.exe"
    if (-not (Test-Path $exe)) {
        $exe = Join-Path $root "target\debug\chat.exe"
    }
    if (-not (Test-Path $exe)) {
        throw "找不到 chat.exe；先构建：tools\build-env.cmd cargo build --release -p dshr-desktop-gpui --bin chat"
    }

    # ---- 测量环境元数据：每次运行自动采集，随 baseline 落盘 ----
    $hostName = [string]$env:COMPUTERNAME
    if (-not $hostName) {
        $hostName = [System.Net.Dns]::GetHostName()
    }
    $sessionName = [string]$env:SESSIONNAME
    if (-not $sessionName) {
        $sessionName = "unknown"
    }
    $remoteSession = $sessionName -match '^(RDP|ICA|parsec)'
    if ($remoteSession) {
        Write-Host "[frame-probe] WARN: 当前是远程会话（session=$sessionName，host=$hostName）；draw_work 受远端合成器/虚拟显示器影响，不能与本地控制台基线的 draw 数字直接比较。"
    }
    $gpu = Get-GpuMetadata
    $binaryRel = ConvertTo-RepoRelativePath $exe
    $binaryMtime = (Get-Item $exe).LastWriteTime.ToString("yyyy-MM-ddTHH:mm:sszzz")
    $gitCommit = "unknown"
    $treeDirty = $false
    try {
        $commitOutput = & git -C $root rev-parse --short HEAD 2>$null
        if ($LASTEXITCODE -eq 0 -and $commitOutput) {
            $gitCommit = ([string]$commitOutput).Trim()
        }
        $dirtyLines = @(& git -C $root status --porcelain --untracked-files=no 2>$null | Where-Object { $_ -match '\S' })
        if ($dirtyLines.Count -gt 0) {
            $treeDirty = $true
        }
    }
    catch {
        $gitCommit = "unknown"
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

    # 用 CreateProcess 直接启动（UseShellExecute=false）：Start-Process 默认走 ShellExecute，
    # 在本机触发 "Open File - Security Warning"（发布者未验证）对话框，无人值守时会永久卡住。
    $startInfo = New-Object System.Diagnostics.ProcessStartInfo
    $startInfo.FileName = $exe
    $startInfo.WorkingDirectory = $root
    $startInfo.UseShellExecute = $false
    $proc = [System.Diagnostics.Process]::Start($startInfo)
    if ($null -eq $proc) {
        throw "无法启动 chat.exe：$exe"
    }
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

    $text = [System.IO.File]::ReadAllText((Join-Path $root $Log))
    Write-Host $text

    $summary = [regex]::Match($text, "(?m)^\[frame-probe\] draws=(\d+) elapsed_ms=(\d+) avg_fps=([\d.]+) rows=(\d+)")
    if (-not $summary.Success) {
        throw "报告缺少汇总行"
    }
    $draws = [int]$summary.Groups[1].Value
    $elapsedMs = [int]$summary.Groups[2].Value
    $fps = [double]$summary.Groups[3].Value
    $measuredRows = [int]$summary.Groups[4].Value
    if ($draws -lt $Frames) {
        Write-Host "[frame-probe] WARN: 报告 draws=$draws < 目标帧数 $Frames（探针可能被安全阀提前结束）"
    }

    $draw = Get-PhaseMetrics $text "draw_work"
    if ($draw.p95_us -le 0.0) {
        throw "报告缺少 draw_work p95 分位行"
    }

    $missedMatch = [regex]::Match($text, "missed_refresh=(\d+)")
    $missedRefresh = $null
    if ($missedMatch.Success) {
        $missedRefresh = [int]$missedMatch.Groups[1].Value
    }
    $ratioMatch = [regex]::Match($text, "missed_ratio=([\d.]+)")
    $missedRatio = 1.0
    if ($ratioMatch.Success) {
        $missedRatio = [double]$ratioMatch.Groups[1].Value
    }
    $refreshMatch = [regex]::Match($text, "refresh_ns=Some\((\d+)\)")
    $refreshNs = $null
    if ($refreshMatch.Success) {
        $refreshNs = [long]$refreshMatch.Groups[1].Value
    }
    # 32fps 预算兜底：与 docs/m2-performance.md 的口径一致，也与远端 32Hz 显示匹配。
    $budgetUs = 31250.0
    $budgetSource = "fallback_32fps"
    if ($null -ne $refreshNs -and $refreshNs -gt 0) {
        $budgetUs = [math]::Round([double]$refreshNs / 1000.0, 2)
        $budgetSource = "refresh_ns"
    }

    $at = (Get-Date).ToString("yyyy-MM-ddTHH:mm:sszzz")
    $noteParts = @("自动生成：$draws 帧滚动、10k 事件会话（7500 行全部虚拟化）、release build；Gate 2 判定口径见 docs/m2-performance.md。")
    if ($remoteSession) {
        $noteParts += "当前为远程会话（$sessionName），draw 数字不代表本地控制台基线。"
    }
    $note = $noteParts -join ""

    $measurement = [ordered]@{
        frames         = $draws
        elapsed_ms     = $elapsedMs
        avg_fps        = $fps
        draw_p50_us    = $draw.p50_us
        draw_p95_us    = $draw.p95_us
        missed_refresh = $missedRefresh
        missed_ratio   = $missedRatio
        rows           = $measuredRows
        refresh_ns     = $refreshNs
        budget_us      = $budgetUs
        at             = $at
        host           = $hostName
        sessionName    = $sessionName
        remoteSession  = [bool]$remoteSession
        gpu            = $gpu
        binary         = $binaryRel
        binaryMtime    = $binaryMtime
        gitCommit      = $gitCommit
        treeDirty      = [bool]$treeDirty
        note           = $note
    }

    $gpuLabel = "unknown"
    if ($null -ne $gpu) {
        $gpuLabel = "{0} (driver {1}, {2})" -f $gpu.name, $gpu.driverVersion, $gpu.resolution
    }
    Write-Host ("[frame-probe] meta host={0} session={1} remote={2} gpu={3} binary={4} built={5} commit={6} tree_dirty={7} frames={8} rows={9} refresh_ns={10} budget_us={11}" -f `
            $hostName, $sessionName, $remoteSession, $gpuLabel, $binaryRel, $binaryMtime, $gitCommit, $treeDirty, $draws, $measuredRows, $refreshNs, $budgetUs)
    Write-Host ("[frame-probe] measured avg_fps={0} draw_p50_us={1} draw_p95_us={2} missed_refresh={3} missed_ratio={4}" -f `
            $fps, $draw.p50_us, $draw.p95_us, $missedRefresh, $missedRatio)

    # ---- 第一层：硬性条件（与基线无关，任何环境都必须满足） ----
    $hardOk = $true
    if ($missedRatio -gt 0.0) {
        Write-Host "[frame-probe] FAIL: missed_ratio $missedRatio > 0（有帧超过 refresh budget）"
        $hardOk = $false
    }
    if ($draw.p95_us -gt $budgetUs) {
        Write-Host "[frame-probe] FAIL: draw_work p95 $($draw.p95_us)us > refresh budget $budgetUs us（预算来源：$budgetSource）"
        $hardOk = $false
    }
    if (-not $hardOk) {
        if ($UpdateBaseline) {
            Write-Host "[frame-probe] 硬性条件未通过，不覆盖基线 $Baseline"
        }
        exit 1
    }
    Write-Host "[frame-probe] HARD PASS: missed_ratio=0, draw_p95_us=$($draw.p95_us) <= budget_us=$budgetUs"

    $baselinePath = $Baseline
    if (-not [System.IO.Path]::IsPathRooted($baselinePath)) {
        $baselinePath = Join-Path $root $baselinePath
    }

    # ---- 写基线：只有硬性条件通过才写，避免把失败数字落成基线 ----
    if ($UpdateBaseline -or -not (Test-Path $baselinePath)) {
        if (-not $UpdateBaseline) {
            Write-Host "[frame-probe] WARN: 基线不存在（$Baseline），自动创建"
        }
        $json = $measurement | ConvertTo-Json -Depth 6
        [System.IO.File]::WriteAllText($baselinePath, $json, (New-Object System.Text.UTF8Encoding($false)))
        Write-Host "[frame-probe] 基线写入 $Baseline（host=$hostName session=$sessionName frames=$draws avg_fps=$fps draw_p95_us=$($draw.p95_us) commit=$gitCommit）"
        exit 0
    }

    # ---- 第二层：相对基线（仅在相同 host，或显式跨主机时判定） ----
    # 用 .NET 读取（UTF-8 自动识别）：Windows PowerShell 5.1 的 Get-Content 默认按 ANSI
    # 解码无 BOM 文件，会把基线里的中文 note 读成乱码。
    $base = [System.IO.File]::ReadAllText($baselinePath) | ConvertFrom-Json
    if ($null -eq $base -or $null -eq $base.avg_fps -or $null -eq $base.draw_p95_us) {
        throw "基线缺少 avg_fps/draw_p95_us 字段：$Baseline"
    }
    $baseHost = [string]$base.host
    $sameHost = ($baseHost -ne "") -and ($baseHost -ieq $hostName)
    Write-Host ("[frame-probe] baseline host={0} session={1} at={2} commit={3} avg_fps={4} draw_p95_us={5}" -f `
            $baseHost, [string]$base.sessionName, [string]$base.at, [string]$base.gitCommit, $base.avg_fps, $base.draw_p95_us)

    if (-not ($sameHost -or $CompareAcrossHosts)) {
        Write-Host "[frame-probe] WARN: 基线 host=$baseHost 与当前 host=$hostName 不一致；跳过相对基线判定（仅记录数字）。确需跨主机比较时加 -CompareAcrossHosts。"
        Write-Host "[frame-probe] PASS (hard gate: missed_ratio=0, draw_p95_us <= budget_us)"
        exit 0
    }

    $fpsFloor = [math]::Round([double]$base.avg_fps * 0.95, 2)
    $p95Ceil = [math]::Round([double]$base.draw_p95_us * 1.20, 2)
    Write-Host ("[frame-probe] baseline_fps={0} fps_floor={1} baseline_p95_us={2} p95_ceil={3}" -f `
            $base.avg_fps, $fpsFloor, $base.draw_p95_us, $p95Ceil)
    if (-not $sameHost) {
        Write-Host "[frame-probe] WARN: -CompareAcrossHosts 跨主机比较（当前 $hostName，基线 $baseHost），环境差异可能主导，仅作参考"
    }

    $relativeOk = $true
    if ($fps -lt $fpsFloor) {
        Write-Host "[frame-probe] FAIL: avg_fps $fps < $fpsFloor"
        $relativeOk = $false
    }
    if ($draw.p95_us -gt $p95Ceil) {
        Write-Host "[frame-probe] FAIL: draw_work p95 $($draw.p95_us)us > $p95Ceil us"
        $relativeOk = $false
    }
    if (-not $relativeOk) {
        exit 1
    }
    Write-Host "[frame-probe] PASS"
    exit 0
}
finally {
    Remove-Item Env:DSHR_SESSIONS_DIR -ErrorAction SilentlyContinue
    Remove-Item Env:DSHR_VISIBLE_LIMIT -ErrorAction SilentlyContinue
    Remove-Item Env:DSHR_FRAME_PROBE -ErrorAction SilentlyContinue
    Remove-Item Env:DSHR_FRAME_LOG -ErrorAction SilentlyContinue
    Remove-Item Env:DSHR_FRAME_QUIT -ErrorAction SilentlyContinue
    Pop-Location
}
