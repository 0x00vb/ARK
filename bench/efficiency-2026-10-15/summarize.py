"""Summarize all attempts, including failures, without changing the raw reports."""
import argparse
import json
from pathlib import Path


def load(paths):
    rows = []
    seen = set()
    for path in paths:
        report = json.loads(path.read_text())
        for workload in report["workloads"]:
            for row in workload["runs"]:
                identity = (row["workload"], row["rep"])
                if identity in seen:
                    raise ValueError("duplicate workload/repetition across reports")
                seen.add(identity)
                rows.append(row)
    return rows


def totals(rows):
    count = len(rows)
    successes = sum(row["success"] for row in rows)
    result = {"attempts": count, "successes": successes}
    for field in ["model_calls", "input_tokens", "output_tokens", "cached_input_tokens",
                  "uncached_input_tokens", "total_tokens", "cost_usd", "provider_errors",
                  "provider_retries", "approvals_requested", "wall_ms"]:
        result[field] = sum(row[field] for row in rows)
    result["reference_cost_per_success"] = result["cost_usd"] / successes if successes else None
    result["state_updates"] = None  # Read event traces, not unavailable metadata-call counts.
    return result


def trace_steps(directory):
    result = {"state_updates": 0, "state_steps_without_adapter_or_retrieval": 0}
    for path in directory.glob("*.jsonl"):
        step = []

        def finish():
            states = sum(event["type"] == "state_update" for event in step)
            result["state_updates"] += states
            if states and not any(event["type"] in ["tool_call", "retrieval"] for event in step):
                result["state_steps_without_adapter_or_retrieval"] += 1

        for line in path.read_text().splitlines():
            row = json.loads(line)
            if row["type"] != "event":
                continue
            event = row["event"]
            if event["type"] == "step":
                finish()
                step = []
            step.append(event)
        finish()
    return result


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument("reports", nargs="+", type=Path)
    parser.add_argument("--traces", nargs="*", type=Path, default=[])
    args = parser.parse_args()
    rows = load(args.reports)
    output = {"totals": totals(rows), "workloads": {}}
    for name in sorted({row["workload"] for row in rows}):
        output["workloads"][name] = totals([row for row in rows if row["workload"] == name])
    output["trace_counts"] = [trace_steps(directory) for directory in args.traces]
    print(json.dumps(output, indent=2))
