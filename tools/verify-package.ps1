param(
    [string]$Archive = "dist/dshr-debug-windows-x64.zip"
)

# M4 G1：校验归档 SHA-256、解压、跑包内真实窗口和运行时自检，再清理临时目录。
$ErrorActionPreference = "Stop"
$archivePath = (Resolve-Path $Archive).Path
$hashFile = "$archivePath.sha256"
if (-not (Test-Path -LiteralPath $hashFile)) { throw "missing $hashFile" }
$expected = ((Get-Content -LiteralPath $hashFile) -split '\s+')[0].ToLowerInvariant()
$actual = (Get-FileHash -LiteralPath $archivePath -Algorithm SHA256).Hash.ToLowerInvariant()
if ($expected -ne $actual) { throw "sha256 mismatch: expected=$expected actual=$actual" }
Write-Host "[verify-package] sha256 ok $actual"

$tmp = Join-Path ([System.IO.Path]::GetTempPath()) ("dshr-verify-" + [guid]::NewGuid().ToString("N"))
New-Item -ItemType Directory -Path $tmp | Out-Null
$previousEnv = @{}
foreach ($name in @("PATH", "DSHR_HOME", "DSHR_NODE", "DSHR_RUNTIME_DIR", "DSHR_APP_ROOT")) {
    $previousEnv[$name] = [Environment]::GetEnvironmentVariable($name)
}
try {
    Expand-Archive -LiteralPath $archivePath -DestinationPath $tmp -Force
    $exe = Join-Path $tmp "dshr.exe"
    if (-not (Test-Path -LiteralPath $exe)) { throw "missing dshr.exe in archive" }
    foreach ($file in @("vcruntime140.dll", "bin/vcruntime140.dll")) {
        if (-not (Test-Path -LiteralPath (Join-Path $tmp $file) -PathType Leaf)) {
            throw "missing app-local Visual C++ runtime: $file"
        }
    }
    $node = Join-Path $tmp "runtime/node/node.exe"
    if (-not (Test-Path -LiteralPath $node -PathType Leaf)) { throw "missing bundled Node" }
    foreach ($asset in @("probe-sidecar.mjs", "probe-plugin.mjs", "cordis")) {
        if (Test-Path -LiteralPath (Join-Path $tmp "sidecar/$asset")) {
            throw "diagnostic sidecar asset in desktop package: $asset"
        }
    }
    foreach ($name in @("DSHR_NODE", "DSHR_RUNTIME_DIR", "DSHR_APP_ROOT")) {
        Remove-Item -LiteralPath "Env:$name" -ErrorAction SilentlyContinue
    }
    $env:PATH = "$env:SystemRoot\System32;$env:SystemRoot"
    $env:DSHR_HOME = Join-Path $tmp ".home"
    Push-Location $tmp
    try {
        & $node --version
        if ($LASTEXITCODE -ne 0) { throw "bundled Node failed: $LASTEXITCODE" }
        if (Test-Path -LiteralPath (Join-Path $tmp 'sidecar/dsh-official/default-profile.json') -PathType Leaf) {
            $shell = Join-Path $tmp 'runtime/powershell/pwsh.exe'
            if (-not (Test-Path -LiteralPath $shell -PathType Leaf)) { throw 'missing bundled PowerShell' }
            & $shell -NoLogo -NoProfile -NonInteractive -Command '$PSVersionTable.PSVersion.ToString()'
            if ($LASTEXITCODE -ne 0) { throw "bundled PowerShell failed: $LASTEXITCODE" }
        }
        foreach ($check in @("window", "official", "cpp-component", "cpp-install", "cpp-cli-install")) {
            $result = Start-Process -FilePath $exe -ArgumentList "--selftest-$check" -Wait -PassThru -NoNewWindow
            if ($result.ExitCode -ne 0) { throw "$check selftest failed: $($result.ExitCode)" }
        }
    } finally {
        Pop-Location
    }
    Write-Host "[verify-package] PASS $archivePath"
} finally {
    foreach ($name in $previousEnv.Keys) {
        if ($null -eq $previousEnv[$name]) {
            Remove-Item -LiteralPath "Env:$name" -ErrorAction SilentlyContinue
        } else {
            [Environment]::SetEnvironmentVariable($name, $previousEnv[$name])
        }
    }
    $target = Get-Item -LiteralPath $tmp -Force -ErrorAction SilentlyContinue
    if ($target -and $target.FullName -eq $tmp -and -not ($target.Attributes -band [IO.FileAttributes]::ReparsePoint)) {
        Remove-Item -LiteralPath $target.FullName -Recurse -Force -ErrorAction SilentlyContinue
    }
}
