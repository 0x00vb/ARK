# Runs ARK for real on Windows, after `cargo build`: the things unit tests do not reach.
# Each check prints one result line and never stops the others; logs/smoke.md (and in CI the
# job summary) gets a table. Turns use a small local model through Ollama: no paid model is
# called. Claude Code and Codex are only asked whether they are signed in.
#
#   pwsh scripts/windows-smoke.ps1 [-Logs logs] [-Model qwen3:0.6b]

param(
    [string]$Logs = "logs",
    [string]$Model = "qwen3:0.6b"
)

$ErrorActionPreference = "Continue"
$Temp = if ($env:RUNNER_TEMP) { $env:RUNNER_TEMP } else { $env:TEMP }
New-Item -ItemType Directory -Force -Path $Logs | Out-Null
$Logs = (Resolve-Path $Logs).Path
$Writer = (Resolve-Path "target/debug/writer.exe").Path
$Results = @()

function Result($check, $ok, $detail) {
    $mark = if ($ok -eq $true) { "pass" } elseif ($ok -eq $false) { "FAIL" } else { "note" }
    Write-Host "[$mark] $check : $detail"
    $script:Results += "| $check | $mark | $($detail -replace '\|', '/' -replace "`r?`n", ' ') |"
}

# No credentials reach the CLIs or the runtime from this job.
foreach ($v in "ANTHROPIC_API_KEY", "OPENAI_API_KEY", "ARK_CLAUDE_BIN", "ARK_CODEX_BIN") {
    Remove-Item "Env:$v" -ErrorAction SilentlyContinue
}

Result "environment" $null ("HOME=" + $env:HOME + "; USERPROFILE=" + $env:USERPROFILE + "; LOCALAPPDATA=" + $env:LOCALAPPDATA)

# ---- Ollama and a small model: the one running here, the installed one, or a download ------
$haveModel = $false
try {
    $running = $null -ne (Invoke-RestMethod "http://localhost:11434/api/tags" -TimeoutSec 3 -ErrorAction Stop)
} catch { $running = $false }
$ollama = (Get-Command ollama -ErrorAction SilentlyContinue).Source
if (-not $ollama -and (Test-Path "$env:LOCALAPPDATA\Programs\Ollama\ollama.exe")) { $ollama = "$env:LOCALAPPDATA\Programs\Ollama\ollama.exe" }
try {
    if (-not $ollama) {
        $ollamaDir = Join-Path $Temp "ollama"
        Invoke-WebRequest "https://github.com/ollama/ollama/releases/latest/download/ollama-windows-amd64.zip" -OutFile "$ollamaDir.zip"
        Expand-Archive "$ollamaDir.zip" -DestinationPath $ollamaDir -Force
        $ollama = Join-Path $ollamaDir "ollama.exe"
    }
    if (-not $running) {
        Start-Process $ollama -ArgumentList "serve" -RedirectStandardError "$Logs/ollama.log" -RedirectStandardOutput "$Logs/ollama.out" -WindowStyle Hidden
        Start-Sleep 5
    }
    & $ollama pull $Model *> "$Logs/ollama-pull.log"
    $haveModel = $LASTEXITCODE -eq 0
    Result "ollama" $haveModel "already running: $running; pull $Model exit $LASTEXITCODE"
} catch {
    Result "ollama" $false "$_"
}

# ---- one-shot turn, no WRITER_ROOT: where does the store go without HOME? -----------------
$work = Join-Path $Temp "writer-cwd"
New-Item -ItemType Directory -Force -Path $work | Out-Null
Push-Location $work
if ($haveModel) {
    $sw = [Diagnostics.Stopwatch]::StartNew()
    & $Writer -p "How many chapters are there? Use your tools." --model "ollama:$Model" --yes --events > "$Logs/turn-events.jsonl" 2> "$Logs/turn-stderr.log"
    $code = $LASTEXITCODE
    $events = @(Get-Content "$Logs/turn-events.jsonl" -ErrorAction SilentlyContinue)
    $tools = @($events | Select-String '"tool' ).Count
    Result "one-shot turn (ollama)" ($code -eq 0) "exit $code in $([int]$sw.Elapsed.TotalSeconds)s, $($events.Count) events, $tools tool lines"
}
$appData = Join-Path $env:LOCALAPPDATA "Writer"
Result "store in %LOCALAPPDATA%\Writer, not the working folder" ((Test-Path $appData) -and -not (Test-Path (Join-Path $work ".writer"))) "app data: $(Test-Path $appData); in the working folder: $(Test-Path (Join-Path $work '.writer'))"
Pop-Location

# ---- the same turn, resumed, with an explicit root ------------------------------------------
$env:WRITER_ROOT = Join-Path $Temp "writer root with spaces é"
if ($haveModel) {
    & $Writer -p "List the chapter files." --model "ollama:$Model" --yes --json > "$Logs/turn2.json" 2> "$Logs/turn2-stderr.log"
    $code = $LASTEXITCODE
    & $Writer -p "And the first one's title?" --resume --model "ollama:$Model" --yes --json > "$Logs/turn3.json" 2> "$Logs/turn3-stderr.log"
    Result "turn in a root with spaces and an accent, then --resume" (($code -eq 0) -and ($LASTEXITCODE -eq 0)) "exits $code, $LASTEXITCODE"
}

# ---- the window over HTTP, and that the server stops cleanly -------------------------------
$out = "$Logs/serve.out"
$p = Start-Process $Writer -ArgumentList "--serve", "127.0.0.1:0" -RedirectStandardOutput $out -RedirectStandardError "$Logs/serve.err" -PassThru -WindowStyle Hidden
$url = $null
for ($i = 0; $i -lt 60 -and -not $url; $i++) {
    Start-Sleep 1
    $url = Get-Content $out -ErrorAction SilentlyContinue | Where-Object { $_ -match '^https?://' } | Select-Object -First 1
}
if ($url) {
    try {
        $r = Invoke-WebRequest $url -UseBasicParsing
        Result "serve: page" ($r.StatusCode -eq 200) "status $($r.StatusCode), $($r.Content.Length) bytes"
    } catch { Result "serve: page" $false "$_" }
} else {
    Result "serve: page" $false "no URL printed in 60s"
}
Stop-Process -Id $p.Id -ErrorAction SilentlyContinue
Start-Sleep 2
Result "serve: process gone after stop" (-not (Get-Process -Id $p.Id -ErrorAction SilentlyContinue)) "pid $($p.Id)"

# ---- Claude Code and Codex: found and asked whether signed in, nothing spent ---------------
# In CI they are installed with npm first (signed out). On a person's machine whatever is
# installed is checked as it is.
if ($env:CI) { npm install -g @anthropic-ai/claude-code @openai/codex *> "$Logs/npm-cli.log" }
cargo test -p ark-model --test this_machine -- --ignored --nocapture *> "$Logs/this-machine.log"
foreach ($cli in "claude", "codex") {
    $where = ((& where.exe $cli 2>$null) -join ", ")
    $backend = Select-String -Path "$Logs/this-machine.log" -Pattern "^backend ${cli}: (.*)" | ForEach-Object { $_.Matches[0].Groups[1].Value } | Select-Object -First 1
    $path = Select-String -Path "$Logs/this-machine.log" -Pattern "^path ${cli}: (.*)" | ForEach-Object { $_.Matches[0].Groups[1].Value } | Select-Object -First 1
    $ok = if ($where) { [bool]($backend -and $backend -notmatch "^not found") } else { $null }
    Result "find $cli" $ok "where.exe: $(if ($where) { $where } else { 'none' }); ARK: $backend; path $path"
}

# ---- summary --------------------------------------------------------------------------------
$table = @("| check | result | detail |", "|---|---|---|") + $Results
$table | Out-File -Append -Encoding utf8 "$Logs/smoke.md"
if ($env:GITHUB_STEP_SUMMARY) {
    "## Runtime smoke" | Out-File -Append -Encoding utf8 $env:GITHUB_STEP_SUMMARY
    $table | Out-File -Append -Encoding utf8 $env:GITHUB_STEP_SUMMARY
}
# Stop only an Ollama this script started.
if (-not $running) { Get-Process ollama -ErrorAction SilentlyContinue | Where-Object { $_.Path -eq $ollama } | Stop-Process -ErrorAction SilentlyContinue }
exit 0
