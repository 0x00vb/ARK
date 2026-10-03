#!/usr/bin/env python3
"""Put several reports of the same suite side by side, per workload (Markdown).

    bench/harness.py bench/baseline.json bench/baseline-naive.json
    bench/harness.py bench/p1/haiku-ark.json bench/p1/haiku-naive.json --skip long-session

Per workload and report: successes over runs, model calls per run, tokens per run and cost per
successful run (total cost over successes; "failed" when nothing succeeded). Then the totals
over the workloads every report has, and, for the first report against each other one, the
workloads where it is cheaper per success, dearer, or the only one to succeed.
"""
import json, sys

def load(p):
    r = json.load(open(p))
    r["_path"] = p
    return r

def by_name(r):
    return {w["name"]: w["runs"] for w in r["workloads"]}

def stats(runs):
    n = len(runs)
    ok = sum(x["success"] for x in runs)
    cost = sum(x["cost_usd"] for x in runs)
    return {
        "ok": ok,
        "n": n,
        "calls": sum(x["model_calls"] for x in runs) / max(1, n),
        "tokens": sum(x["total_tokens"] for x in runs) / max(1, n),
        "cps": cost / ok if ok else None,
    }

def cell(s):
    cps = f"{s['cps']:.4f}" if s["cps"] is not None else "failed"
    return f"{s['ok']}/{s['n']} · {s['calls']:.1f} · {s['tokens']:,.0f} · {cps}"

def main(argv):
    skip = set()
    paths = []
    it = iter(argv)
    for a in it:
        if a == "--skip":
            skip.update(next(it).split(","))
        else:
            paths.append(a)
    if len(paths) < 2:
        sys.exit(__doc__)
    reports = [load(p) for p in paths]
    names = [by_name(r) for r in reports]
    common = [w["name"] for w in reports[0]["workloads"]
              if w["name"] not in skip and all(w["name"] in n for n in names)]

    print("Each cell: successes/runs · model calls/run · tokens/run · cost/success (USD)\n")
    print("| Workload | " + " | ".join(r["label"] for r in reports) + " |")
    print("|---|" + "---:|" * len(reports))
    for w in common:
        print(f"| {w} | " + " | ".join(cell(stats(n[w])) for n in names) + " |")
    totals = [stats([x for w in common for x in n[w]]) for n in names]
    print("| **total** | " + " | ".join(cell(t) for t in totals) + " |")
    if skip:
        print(f"\nLeft out of the totals: {', '.join(sorted(skip))}.")

    first = names[0]
    for r, n in zip(reports[1:], names[1:]):
        cheaper, dearer, only_first, only_other = [], [], [], []
        for w in common:
            a, b = stats(first[w]), stats(n[w])
            if a["cps"] is None and b["cps"] is None:
                continue
            if b["cps"] is None:
                only_first.append(w)
            elif a["cps"] is None:
                only_other.append(w)
            elif a["cps"] < b["cps"]:
                cheaper.append(f"{w} ({a['cps'] / b['cps']:.2f}x)")
            else:
                dearer.append(f"{w} ({a['cps'] / b['cps']:.2f}x)")
        print(f"\n{reports[0]['label']} against {r['label']}:")
        print(f"- cheaper per success: {', '.join(cheaper) or 'none'}")
        print(f"- dearer per success: {', '.join(dearer) or 'none'}")
        print(f"- only {reports[0]['label']} succeeded: {', '.join(only_first) or 'none'}")
        print(f"- only {r['label']} succeeded: {', '.join(only_other) or 'none'}")

if __name__ == "__main__":
    main(sys.argv[1:])
