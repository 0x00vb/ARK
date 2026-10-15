"""Check sequencing that the original answer/file graders do not establish."""
import json
import sys
from pathlib import Path


def audit(directory):
    errors = []
    checked = {}
    for path in directory.glob("*.jsonl"):
        workload = path.stem.rsplit("-", 1)[0]
        if workload not in ["simple", "approval", "stale-read", "resume"]:
            continue
        events = [row["event"] for row in map(json.loads, path.read_text().splitlines())
                  if row["type"] == "event"]
        results = [event for event in events if event["type"] == "tool_result"]
        checked[workload] = checked.get(workload, 0) + 1
        if workload == "simple" and any(event["type"] == "tool_call" for event in events):
            errors.append(str(path) + ": tool used for greeting")
        if workload == "stale-read":
            actions = [event["action"] for event in results]
            if actions != ["settings.get", "settings.set", "settings.get"]:
                errors.append(str(path) + ": missing read/write/fresh-read sequence")
            elif "value=fast" not in results[-1]["summary"]:
                errors.append(str(path) + ": fresh read did not return fast")
        if workload in ["approval", "stale-read"]:
            action = "notes.write" if workload == "approval" else "settings.set"
            resolutions = [i for i, e in enumerate(events)
                           if e["type"] == "approval_resolved" and e.get("action") == action
                           and e.get("decision") == "allow_once"]
            mutations = [i for i, e in enumerate(events)
                         if e["type"] == "tool_result" and e.get("action") == action]
            if len(resolutions) != 1 or len(mutations) != 1 or resolutions[0] >= mutations[0]:
                errors.append(str(path) + ": mutation lacks preceding once-only approval")
        if workload == "resume":
            answers = [e["text"] for e in events if e["type"] == "answer"]
            if len(answers) != 2 or "gold" not in answers[-1].lower():
                errors.append(str(path) + ": tier not retained in the post-restart answer")
    return {"directory": str(directory), "checked": checked, "errors": errors}


if __name__ == "__main__":
    results = [audit(Path(directory)) for directory in sys.argv[1:]]
    print(json.dumps(results, indent=2))
    raise SystemExit(bool(any(result["errors"] for result in results)))
