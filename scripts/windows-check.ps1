# The Windows check on a real machine: what the `windows` CI job runs, from a checkout of ARK.
# Builds, runs every test, the page tests in Chromium and WebKit, and the runtime smoke
# (scripts/windows-smoke.ps1). Nothing stops at the first failure. At the end it writes
# logs/summary.md and windows-check-<time>.zip with every log; send the zip back.
#
# No paid model is called and no account is used. It installs nothing on the system: it says
# what is missing and the winget command for it.
#
#   powershell -ExecutionPolicy Bypass -File scripts\windows-check.ps1

param([string]$Logs = "logs")

$ErrorActionPreference = "Continue"
Set-Location (Split-Path $PSScriptRoot -Parent)
New-Item -ItemType Directory -Force -Path $Logs | Out-Null
$Logs = (Resolve-Path $Logs).Path
$env:CARGO_TERM_COLOR = "never"
$env:CARGO_INCREMENTAL = "0"
$env:RUST_BACKTRACE = "1"

# ---- what the machine needs ------------------------------------------------------------------
$missing = @()
if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) { $missing += "Rust: winget install Rustlang.Rustup" }
$vswhere = "${env:ProgramFiles(x86)}\Microsoft Visual Studio\Installer\vswhere.exe"
$vc = if (Test-Path $vswhere) { & $vswhere -latest -products * -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath } else { $null }
if (-not $vc) { $missing += 'C++ build tools: winget install Microsoft.VisualStudio.2022.BuildTools --override "--quiet --wait --add Microsoft.VisualStudio.Workload.VCTools --includeRecommended"' }
if (-not (Get-Command node -ErrorAction SilentlyContinue)) { $missing += "Node.js: winget install OpenJS.NodeJS.LTS" }
if (-not (Get-Command git -ErrorAction SilentlyContinue)) { $missing += "Git: winget install Git.Git" }
if ($missing) {
    Write-Host "Install these first, open a new terminal, and run this again:"
    $missing | ForEach-Object { Write-Host "  $_" }
    exit 1
}

function Step($name, [scriptblock]$run) {
    Write-Host "==> $name"
    $sw = [Diagnostics.Stopwatch]::StartNew()
    & $run
    $code = $LASTEXITCODE
    $script:steps += "| $name | $(if ($code -eq 0) { 'pass' } else { "FAIL ($code)" }) | $([int]$sw.Elapsed.TotalMinutes) min |"
}
$steps = @()

$os = Get-CimInstance Win32_OperatingSystem
"$($os.Caption) $($os.Version) $($os.OSArchitecture); user $env:USERNAME; $((Get-CimInstance Win32_Processor | Select-Object -First 1).Name)" | Out-File "$Logs/system.txt"
& rustc -vV *>> "$Logs/system.txt"
& node -v *>> "$Logs/system.txt"

Step "build" { cargo build --workspace --all-targets --locked *> "$Logs/build.log" }
Step "clippy" { cargo clippy --workspace --all-targets --locked -- -D warnings *> "$Logs/clippy.log" }
Step "test" { cargo test --workspace --locked --no-fail-fast *> "$Logs/test.log" }
Step "page tests (setup)" {
    Push-Location crates/ark-ui/tests/web
    npm ci *> "$Logs/page-setup.log"
    npx playwright-core install chromium webkit *>> "$Logs/page-setup.log"
    Pop-Location
}
$env:ARK_UI_PAGE_TESTS = "1"
$env:ARK_UI_BROWSER = "chromium"
Step "page tests (Chromium)" { cargo test -p ark-ui --test page --locked *> "$Logs/page-chromium.log" }
Remove-Item Env:ARK_UI_BROWSER
Step "page tests (WebKit)" { cargo test -p ark-ui --test page --locked *> "$Logs/page-webkit.log" }
Remove-Item Env:ARK_UI_PAGE_TESTS
Step "runtime smoke" { & "$PSScriptRoot\windows-smoke.ps1" -Logs $Logs }

# ---- summary and the zip -------------------------------------------------------------------
$failing = Get-Content "$Logs/test.log", "$Logs/page-chromium.log", "$Logs/page-webkit.log" -ErrorAction SilentlyContinue |
    Select-String -Pattern '^test .* FAILED$|^error: test failed|panicked at' | ForEach-Object { $_.Line } | Sort-Object -Unique
$summary = @("# Windows check", "", (Get-Content "$Logs/system.txt" -First 1), "", "| step | result | time |", "|---|---|---|") + $steps +
    @("", "## Failing tests", '```') + $failing + @('```', "")
if (Test-Path "$Logs/smoke.md") { $summary += @("## Runtime smoke", "") + (Get-Content "$Logs/smoke.md") }
$summary | Out-File -Encoding utf8 "$Logs/summary.md"
$zip = "windows-check-$(Get-Date -Format yyyyMMdd-HHmm).zip"
Compress-Archive -Path "$Logs/*" -DestinationPath $zip -Force
Get-Content "$Logs/summary.md"
Write-Host ""
Write-Host "Done. Send back $((Resolve-Path $zip).Path)"
