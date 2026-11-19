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
try {
    Expand-Archive -LiteralPath $archivePath -DestinationPath $tmp -Force
    $exe = Join-Path $tmp "chat.exe"
    if (-not (Test-Path -LiteralPath $exe)) { throw "missing chat.exe in archive" }
    $env:DSHR_HOME = Join-Path $tmp ".home"
    Push-Location $tmp
    try {
        & .\chat.exe --selftest-window
        if ($LASTEXITCODE -ne 0) { throw "window selftest failed: $LASTEXITCODE" }
        & .\chat.exe --selftest-sidecar-recovery
        if ($LASTEXITCODE -ne 0) { throw "sidecar selftest failed: $LASTEXITCODE" }
        & .\chat.exe --selftest-official
        if ($LASTEXITCODE -ne 0) { throw "official selftest failed: $LASTEXITCODE" }
        & .\chat.exe --selftest-cpp-component
        if ($LASTEXITCODE -ne 0) { throw "cpp component selftest failed: $LASTEXITCODE" }
        & .\chat.exe --selftest-cpp-install
        if ($LASTEXITCODE -ne 0) { throw "cpp install selftest failed: $LASTEXITCODE" }
        & .\chat.exe --selftest-cpp-cli-install
        if ($LASTEXITCODE -ne 0) { throw "cpp cli install selftest failed: $LASTEXITCODE" }
    } finally {
        Pop-Location
    }
    Write-Host "[verify-package] PASS $archivePath"
} finally {
    Remove-Item Env:\DSHR_HOME -ErrorAction SilentlyContinue
    Remove-Item -Recurse -Force $tmp -ErrorAction SilentlyContinue
}
