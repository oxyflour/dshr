param(
    [Parameter(Mandatory)][string]$Archive,
    [Parameter(Mandatory)][string]$RuntimePackages,
    [Parameter(Mandatory)][string]$AcceptanceExe,
    [Parameter(Mandatory)][string]$Evidence,
    [string]$DeepSeekEnv,
    [switch]$HostSmoke
)

$ErrorActionPreference = "Stop"
$archivePath = (Resolve-Path -LiteralPath $Archive).Path
$packages = (Resolve-Path -LiteralPath $RuntimePackages).Path
$acceptance = (Resolve-Path -LiteralPath $AcceptanceExe).Path
$run = Join-Path $env:TEMP ("dshr-acceptance-" + [guid]::NewGuid().ToString("N"))
$out = (New-Item -ItemType Directory -Path $Evidence -Force).FullName
$base = Join-Path $run "app"
$workspace = Join-Path $run "workspace"
New-Item -ItemType Directory -Path $base, $workspace | Out-Null

$os = Get-CimInstance Win32_OperatingSystem
$system = Get-CimInstance Win32_ComputerSystem
$label = if ($HostSmoke) { "HOST_SMOKE" } else { "CLEAN_WINDOWS" }
@("$label $($os.Caption) build $($os.BuildNumber)", "Machine model $($system.Model)") |
    Set-Content -LiteralPath (Join-Path $out "baseline.txt")
if (-not $HostSmoke) {
    if ($system.Model -notmatch 'Virtual|VMware|KVM') {
        throw "run in a clean Windows virtual machine"
    }
    foreach ($name in @("dsh", "node", "python", "py", "cargo", "rustc")) {
        $found = Get-Command $name -CommandType Application -All -ErrorAction SilentlyContinue |
            Where-Object { $_.Source -notlike "*\Microsoft\WindowsApps\*" }
        if ($found) { throw "preinstalled command detected: $name" }
        "No preinstalled $name on PATH" | Add-Content -LiteralPath (Join-Path $out "baseline.txt")
    }
    foreach ($path in @(
        "$env:USERPROFILE\.cargo",
        "$env:USERPROFILE\.rustup",
        "$env:ProgramFiles\nodejs",
        "$env:ProgramFiles\DeepSeek Harness",
        "$env:LOCALAPPDATA\Programs\DeepSeek Harness",
        "$env:LOCALAPPDATA\Programs\Python",
        "$env:ProgramFiles\Python*",
        "$env:SystemDrive\Python*"
    )) {
        if (Test-Path -Path $path) { throw "preinstalled runtime detected: $path" }
    }
    "No preinstalled runtimes in standard system or user directories" |
        Add-Content -LiteralPath (Join-Path $out "baseline.txt")
    $systemCrt = Test-Path -LiteralPath (Join-Path $env:SystemRoot "System32/vcruntime140.dll") -PathType Leaf
    "System32 vcruntime140.dll present before extracting app: $systemCrt" |
        Add-Content -LiteralPath (Join-Path $out "baseline.txt")
}

$actual = (Get-FileHash -LiteralPath $archivePath -Algorithm SHA256).Hash
$expected = ((Get-Content -LiteralPath "$archivePath.sha256" -Raw).Trim() -split '\s+')[0]
if ($actual -ne $expected) { throw "base archive checksum mismatch" }
Expand-Archive -LiteralPath $archivePath -DestinationPath $base
Copy-Item -LiteralPath $packages -Destination (Join-Path $base "runtime-packages") -Recurse
Copy-Item -LiteralPath $acceptance -Destination (Join-Path $base "dshr-acceptance.exe")
Copy-Item -LiteralPath (Join-Path $base "runtime/components.json") -Destination (Join-Path $out "components.json")
"Base SHA-256 $actual" | Add-Content -LiteralPath (Join-Path $out "baseline.txt")

$processEnv = @("DSHR_NODE", "DSHR_RUNTIME_DIR", "DSHR_APP_ROOT", "DSHR_DSH_SOURCE",
    "DSHR_DSH_PLUGINS", "DSHR_SIDECAR", "DSHR_CREATOR_CARGO", "DSHR_FS_WRITE",
    "DSHR_LOADER_BUNDLE", "DSHR_LOADER_PATCH",
    "DSHR_HOST_SERVICES", "DSHR_FORCE_MOCK", "CARGO_HOME", "RUSTUP_HOME", "RUSTC",
    "PATH", "USERPROFILE", "LOCALAPPDATA", "DSHR_HOME", "DSHR_LIVE_CREATOR_DIR",
    "DSHR_LIVE_CREATOR_MANAGED_COMPONENTS", "DEEPSEEK_API_KEY", "DEEPSEEK_BASE_URL", "DEEPSEEK_MODEL")
$previous = @{}
foreach ($name in $processEnv) { $previous[$name] = [Environment]::GetEnvironmentVariable($name) }
try {
    foreach ($name in $processEnv) {
        if ($name -notlike 'DEEPSEEK_*' -and $name -notin @('PATH', 'USERPROFILE', 'LOCALAPPDATA')) {
            Remove-Item -LiteralPath "Env:$name" -ErrorAction SilentlyContinue
        }
    }
    if ($DeepSeekEnv) {
        foreach ($line in (Get-Content -LiteralPath $DeepSeekEnv)) {
            if ($line -match '^(DEEPSEEK_API_KEY|DEEPSEEK_BASE_URL|DEEPSEEK_MODEL)=(.+)$') {
                [Environment]::SetEnvironmentVariable($Matches[1], $Matches[2].Trim().Trim('"', "'"))
            }
        }
    }
    if (-not $env:DEEPSEEK_API_KEY) { throw "provide DEEPSEEK_API_KEY or -DeepSeekEnv" }
    $env:PATH = "$env:SystemRoot\System32;$env:SystemRoot"
    if ($HostSmoke) {
        $env:USERPROFILE = Join-Path $run "user"
        $env:LOCALAPPDATA = Join-Path $run "local"
        New-Item -ItemType Directory -Path $env:USERPROFILE, $env:LOCALAPPDATA | Out-Null
    }
    $env:DSHR_HOME = Join-Path $run "data"
    $env:DSHR_LIVE_CREATOR_DIR = $workspace
    $env:DSHR_LIVE_CREATOR_MANAGED_COMPONENTS = "1"
    $console = Start-Process -FilePath (Join-Path $base "bin/dshr.exe") -WorkingDirectory $base -ArgumentList @("--profile", "headless", "--help") -Wait -PassThru -WindowStyle Hidden -RedirectStandardOutput (Join-Path $out "console.stdout.txt") -RedirectStandardError (Join-Path $out "console.stderr.txt")
    if ($console.ExitCode -ne 0) {
        throw ("packaged console startup failed ({0}, 0x{0:X8})" -f $console.ExitCode)
    }
    $launch = @{
        FilePath = Join-Path $base "dshr-acceptance.exe"
        WorkingDirectory = $base
        ArgumentList = @("--ignored", "--nocapture", "--exact", "creator::live_test::live_creator_authors_and_invokes_hello_world")
        Wait = $true
        PassThru = $true
        WindowStyle = "Hidden"
        RedirectStandardOutput = Join-Path $out "creator.stdout.txt"
        RedirectStandardError = Join-Path $out "creator.stderr.txt"
    }
    $test = Start-Process @launch
    $trace = Get-Content -LiteralPath (Join-Path $out "creator.stdout.txt") -Raw
    $sessions = Join-Path $workspace "sessions"
    if (Test-Path -LiteralPath $sessions) {
        Get-ChildItem -LiteralPath $sessions -Filter "*.jsonl" -File |
            Copy-Item -Destination $out
    }
    if ($test.ExitCode -ne 0 -or $trace -notmatch 'LIVE_CREATOR_PASS') {
        throw ("creator acceptance failed ({0}, 0x{0:X8}); see {1}" -f $test.ExitCode, $out)
    }
    Write-Host "$label PASS $out"
} finally {
    foreach ($name in $previous.Keys) {
        if ($null -eq $previous[$name]) {
            Remove-Item -LiteralPath "Env:$name" -ErrorAction SilentlyContinue
        } else {
            [Environment]::SetEnvironmentVariable($name, $previous[$name])
        }
    }
}
