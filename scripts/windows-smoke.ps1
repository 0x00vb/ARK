# Runs ARK for real on Windows, after `cargo build`: the things unit tests do not reach.
# Each check prints one result line and never stops the others; the job summary gets a table.
# Uses a small local model through Ollama, so no paid model is called and no account is used.
#
#   pwsh scripts/windows-smoke.ps1 [-Logs logs] [-Model qwen3:0.6b]

param(
    [string]$Logs = "logs",
    [string]$Model = "qwen3:0.6b"
)

$ErrorActionPreference = "Continue"
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

# ---- Ollama and a small model ---------------------------------------------------------------
$ollamaDir = Join-Path $env:RUNNER_TEMP "ollama"
try {
    Invoke-WebRequest "https://github.com/ollama/ollama/releases/latest/download/ollama-windows-amd64.zip" -OutFile "$ollamaDir.zip"
    Expand-Archive "$ollamaDir.zip" -DestinationPath $ollamaDir -Force
    $ollama = Join-Path $ollamaDir "ollama.exe"
    Start-Process $ollama -ArgumentList "serve" -RedirectStandardError "$Logs/ollama.log" -RedirectStandardOutput "$Logs/ollama.out" -WindowStyle Hidden
    Start-Sleep 5
    & $ollama pull $Model *> "$Logs/ollama-pull.log"
    $haveModel = $LASTEXITCODE -eq 0
    Result "ollama" $haveModel "pull $Model exit $LASTEXITCODE"
} catch {
    $haveModel = $false
    Result "ollama" $false "$_"
}

# ---- one-shot turn, no WRITER_ROOT: where does the store go without HOME? -----------------
$work = Join-Path $env:RUNNER_TEMP "writer-cwd"
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
$stores = @(Get-ChildItem -Force -Recurse -Depth 2 -Directory -Filter ".writer" $work, $env:USERPROFILE -ErrorAction SilentlyContinue | ForEach-Object FullName)
Result "store location without WRITER_ROOT" $null ("found: " + ($stores -join ", "))
Pop-Location

# ---- the same turn, resumed, with an explicit root ------------------------------------------
$env:WRITER_ROOT = Join-Path $env:RUNNER_TEMP "writer root with spaces é"
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

# ---- Claude Code and Codex: installed but signed out, so nothing is spent --------------------
npm install -g @anthropic-ai/claude-code @openai/codex *> "$Logs/npm-cli.log"
foreach ($cli in "claude", "codex") {
    $where = (& where.exe $cli 2>$null) -join ", "
    $model = if ($cli -eq "claude") { "claude:sonnet" } else { "codex:default" }
    & $Writer -p "Say hi." --model $model --json > "$Logs/$cli.json" 2> "$Logs/$cli.err"
    $msg = ((Get-Content "$Logs/$cli.err", "$Logs/$cli.json" -ErrorAction SilentlyContinue) -join " ")
    if ($msg.Length -gt 300) { $msg = $msg.Substring(0, 300) }
    $found = $msg -notmatch "not installed|not found"
    Result "find $cli (on PATH as: $where)" $found "exit $LASTEXITCODE; $msg"
}

# ---- summary --------------------------------------------------------------------------------
$table = @("| check | result | detail |", "|---|---|---|") + $Results
$table | Out-File -Append -Encoding utf8 "$Logs/smoke.md"
if ($env:GITHUB_STEP_SUMMARY) {
    "## Runtime smoke" | Out-File -Append -Encoding utf8 $env:GITHUB_STEP_SUMMARY
    $table | Out-File -Append -Encoding utf8 $env:GITHUB_STEP_SUMMARY
}
Get-Process ollama* -ErrorAction SilentlyContinue | Stop-Process -ErrorAction SilentlyContinue
exit 0
