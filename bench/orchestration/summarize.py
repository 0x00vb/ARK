#!/usr/bin/env python3
"""Summarize agent-team run logs: python3 summarize.py on-1.log on-2.log ... -- off-1.log ...

Reads what the example prints: the result line, each `model reason` line (input, output,
cached tokens and time) and the final state. Prints one row per log and the mean per group.
"""
import re
import sys

CALL = re.compile(r"model \w+ · (\S+) · (\d+) in / (\d+) out(?: \((\d+) cached\))? · ([\d.]+)s")
RESULT = re.compile(r"^== result \((\w+), ([\d.]+)s\)")


def parse(path):
    calls, state, wall = [], None, None
    for line in open(path, encoding="utf-8"):
        m = RESULT.search(line)
        if m:
            state, wall = m.group(1), float(m.group(2))
        m = CALL.search(line)
        if m:
            model, inp, out, cached = m.group(1), int(m.group(2)), int(m.group(3)), int(m.group(4) or 0)
            calls.append((model, inp, out, cached))
    return {
        "state": state,
        "wall_s": wall,
        "calls": len(calls),
        "input": sum(c[1] for c in calls),
        "uncached": sum(c[1] - c[3] for c in calls),
        "output": sum(c[2] for c in calls),
    }


def show(name, logs):
    rows = [parse(p) for p in logs]
    print(f"\n{name}")
    print("log | state | wall s | calls | input | uncached input | output")
    for p, r in zip(logs, rows):
        print(f"{p} | {r['state']} | {r['wall_s']} | {r['calls']} | {r['input']} | {r['uncached']} | {r['output']}")
    done = [r for r in rows if r["wall_s"] is not None]
    if done:
        mean = lambda k: sum(r[k] for r in done) / len(done)
        print(f"mean ({len(done)} runs) | {sum(r['state'] == 'done' for r in done)}/{len(done)} done | "
              f"{mean('wall_s'):.1f} | {mean('calls'):.1f} | {mean('input'):.0f} | {mean('uncached'):.0f} | {mean('output'):.0f}")


if __name__ == "__main__":
    args = sys.argv[1:]
    if "--" in args:
        i = args.index("--")
        show("group A", args[:i])
        show("group B", args[i + 1:])
    else:
        show("runs", args)
