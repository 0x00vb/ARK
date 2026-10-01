#!/usr/bin/env python3
"""Print the before/after tables in RESULTS.md from two reports (M4 and M5).

    bench/results.py bench/m4/live-qwen3-4b.json bench/m5/live-qwen3-4b-final.json
"""
import json, sys

def load(p):
    return json.load(open(p))

def runs(r, skip=()):
    return [x for w in r["workloads"] if w["name"] not in skip for x in w["runs"]]

def mean(v):
    return sum(v) / len(v) if v else 0.0

def pct(v, p):
    v = sorted(v)
    if not v:
        return 0
    i = max(1, min(len(v), int(-(-p * len(v) // 1))))
    return v[i - 1]

def row(name, a, b, fmt="{:.0f}", lower_better=True):
    fa, fb = fmt.format(a), fmt.format(b)
    d = "" if a == 0 else f"{(b - a) / a * 100:+.0f}%"
    print(f"| {name} | {fa} | {fb} | {d} |")

def table(a, b, skip=()):
    ra, rb = runs(a, skip), runs(b, skip)
    succ = lambda r: sum(x["success"] for x in r) / len(r)
    cost = lambda r: sum(x["cost_usd"] for x in r) / max(1, sum(x["success"] for x in r))
    print(f"| Metric | {a['label']} | {b['label']} | Delta |")
    print("|---|---:|---:|---:|")
    row("Task success", succ(ra), succ(rb), "{:.2f}")
    row("Cost / successful task (USD)", cost(ra), cost(rb), "{:.4f}")
    for key, name in [("input_tokens", "Input tokens / run"), ("uncached_input_tokens", "Uncached input tokens / run"),
                      ("cached_input_tokens", "Cached input tokens / run"), ("output_tokens", "Output tokens / run"),
                      ("model_calls", "Model calls / run"), ("tool_calls", "Tool calls / run"),
                      ("deduplicated", "Duplicate calls served from reuse / run"), ("repeated_executions", "Repeated identical executions / run")]:
        row(name, mean([x[key] for x in ra]), mean([x[key] for x in rb]), "{:.1f}" if key in ("model_calls", "tool_calls", "deduplicated", "repeated_executions") else "{:.0f}")
    row("Peak context (tokens)", max(x["peak_context_tokens"] for x in ra), max(x["peak_context_tokens"] for x in rb))
    ta = [t for x in ra for t in x["turn_ms"]]; tb = [t for x in rb for t in x["turn_ms"]]
    row("Median turn latency (ms)", pct(ta, .5), pct(tb, .5))
    row("P95 turn latency (ms)", pct(ta, .95), pct(tb, .95))
    fa = [t for x in ra for t in x["first_token_ms"]]; fb = [t for x in rb for t in x["first_token_ms"]]
    if fa and fb:
        row("Median time to first token (ms)", pct(fa, .5), pct(fb, .5))
    row("Failed provider calls / run", mean([x["provider_errors"] for x in ra]), mean([x["provider_errors"] for x in rb]), "{:.2f}")
    row("Retried provider calls / run", mean([x["provider_retries"] for x in ra]), mean([x["provider_retries"] for x in rb]), "{:.2f}")
    row("Turns that hit the 300 s timeout", sum("timed out" in x["verdict"] for x in ra), sum("timed out" in x["verdict"] for x in rb))
    print()
    print("| Workload | Success | Model calls | Tokens / run |")
    print("|---|---:|---:|---:|")
    wa = {w["name"]: w for w in a["workloads"]}
    for w in b["workloads"]:
        if w["name"] in skip or w["name"] not in wa:
            continue
        sa, sb = wa[w["name"]]["summary"], w["summary"]
        print(f"| {w['name']} | {sa['success_rate']:.2f} -> {sb['success_rate']:.2f} | {sa['model_calls']:.1f} -> {sb['model_calls']:.1f} | {sa['total_tokens']:.0f} -> {sb['total_tokens']:.0f} |")

if __name__ == "__main__":
    a, b = load(sys.argv[1]), load(sys.argv[2])
    skip = tuple(sys.argv[3].split(",")) if len(sys.argv) > 3 else ()
    table(a, b, skip)
