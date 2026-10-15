"""Run the real writing app against two synthetic chapters. Never use a private manuscript."""
import hashlib
import json
import os
from pathlib import Path
import subprocess
import sys

folder = Path(__file__).resolve().parent
fixture = json.loads((folder / "dogfood-fixture.json").read_text())
root = Path(fixture["manuscript"])
binary = Path(sys.argv[1]).resolve()
model = sys.argv[2] if len(sys.argv) > 2 else "gpt-5.6-terra@medium"
env = os.environ.copy()
env.update(WRITER_ROOT=fixture["store"], ARK_PROVIDER="codex", ARK_MODEL=model)
command = [str(binary), "--events", "-p",
           "List the chapter filenames with their word counts, and give the total word count. Do not edit the manuscript."]
try:
    run = subprocess.run(command, cwd=root, env=env, capture_output=True, text=True, timeout=120)
    output, error, code = run.stdout, run.stderr, run.returncode
except subprocess.TimeoutExpired as failure:
    output = failure.stdout or b""
    error = failure.stderr or b""
    output = output.decode() if isinstance(output, bytes) else output
    error = error.decode() if isinstance(error, bytes) else error
    code = None
(folder / "dogfood-events.jsonl").write_text(output)
(folder / "dogfood-stderr.log").write_text(error)
events = [json.loads(line) for line in output.splitlines() if line.strip()]
results = [event for event in events if event.get("type") == "result"]
answer = results[-1].get("answer", "") if results else ""
calls = [event for event in events if event.get("type") == "tool_call"]
unchanged = all(hashlib.sha256((root / name).read_bytes()).hexdigest() == digest
                for name, digest in fixture["sha256"].items())
checks = {
    "process_succeeded": code == 0,
    "result_succeeded": bool(results and results[-1].get("ok")),
    "listed_both_files": all(name in answer for name in fixture["expected_words"]),
    "correct_word_counts": all(str(value) in answer for value in [4, 3, 7]),
    "read_actual_manuscript": any(event.get("action", "").startswith("manuscript.") for event in calls),
    "no_save_call": all(event.get("action") != "manuscript.save" for event in calls),
    "manuscript_hashes_unchanged": unchanged,
}
report = {"binary_sha256": hashlib.sha256(binary.read_bytes()).hexdigest(),
          "model": "codex:" + model, "effort_forced": "@" in model, "returncode": code, "checks": checks,
          "success": all(checks.values()), "answer": answer,
          "model_calls": sum(event.get("type") == "model_call" for event in events)}
(folder / "dogfood-result.json").write_text(json.dumps(report, indent=2) + "\n")
print(json.dumps(report, indent=2))
raise SystemExit(not report["success"])
