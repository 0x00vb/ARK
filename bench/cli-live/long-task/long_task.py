"""Drive `writer --serve` through the window protocol for one live CLI task. Prints JSON lines.

usage: python3 long_task.py <provider:model> <CLI process name> <empty store dir>
e.g.   python3 long_task.py claude:haiku "input-format stream-json" /tmp/w > claude-haiku.jsonl
       python3 long_task.py codex:default app-server /tmp/w > codex-default.jsonl
Build the writer first (`cargo build -p writing-agent`); WRITER_BIN overrides its path."""
import json, os, re, subprocess, sys, threading, time, urllib.request
spec, pname, root = sys.argv[1], sys.argv[2], sys.argv[3]
writer = os.environ.get("WRITER_BIN") or os.path.join(os.path.dirname(os.path.abspath(__file__)), "../../../target/debug/writer")
env = dict(os.environ, WRITER_ROOT=root)
proc = subprocess.Popen([writer, "--serve", "127.0.0.1:0", "--model", spec], env=env,
                        stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True, cwd=root)
url = None
for line in proc.stdout:
    m = re.search(r"http://127\.0\.0\.1:(\d+)/\?token=([0-9a-f]+)", line)
    if m: url = m; break
threading.Thread(target=lambda: [None for _ in proc.stdout], daemon=True).start()
PORT, TOK = url.group(1), url.group(2); BASE = f"http://127.0.0.1:{PORT}"
def call(m, p=None):
    r = urllib.request.Request(BASE + "/rpc", method="POST", data=json.dumps({"method": m, "params": p or {}}).encode(),
                               headers={"content-type": "application/json", "x-ark-token": TOK})
    return json.load(urllib.request.urlopen(r, timeout=1800))
events = []
def listen():
    with urllib.request.urlopen(BASE + "/events?token=" + TOK, timeout=3600) as r:
        for line in r:
            line = line.decode().strip()
            if line.startswith("data:"):
                events.append((time.time(), json.loads(line[5:])))
threading.Thread(target=listen, daemon=True).start(); time.sleep(0.5)
def procs():
    # CLI processes under this writer only (other sessions on the machine run the CLIs too).
    rows = subprocess.run(["ps", "-A", "-o", "pid=,ppid=,command="], capture_output=True, text=True).stdout.splitlines()
    kids, tree = {}, {proc.pid}
    for r in rows:
        pid, ppid, cmd = r.strip().split(None, 2) + [""] * (3 - len(r.strip().split(None, 2)))
        kids.setdefault(int(ppid), []).append((int(pid), cmd))
    todo, found = [proc.pid], 0
    while todo:
        for pid, cmd in kids.get(todo.pop(), []):
            todo.append(pid)
            if pname in cmd: found += 1
    return found
def out(d): print(json.dumps(d), flush=True)
def tools_in(evs):
    names = []
    for _, e in evs:
        if e.get("kind") == "event":
            s = json.dumps(e["event"])
            for n in re.findall(r'"(?:action|name)": "([a-z_]+\.[a-z_.]+)"', s):
                names.append(n)
    return sorted(set(names))
def turn(label, text, cancel_after=None):
    start = len(events); t0 = time.time(); r = call("send", {"text": text})
    first = None; cancelled_at = None
    while True:
        time.sleep(0.05)
        evs = events[start:]
        texts = [(t, e) for t, e in evs if e.get("kind") == "text"]
        if texts and first is None: first = round(texts[0][0] - t0, 2)
        if cancel_after and len(texts) >= cancel_after and cancelled_at is None:
            cancelled_at = len(texts); call("cancel"); time.sleep(1.5)
            out({"label": label, "cancel_sent_after_text_events": cancelled_at, "cli_procs_after_cancel": procs()})
        fin = [e for t, e in evs if e.get("kind") == "turn_finished"]
        if fin:
            f = fin[0]
            streamed = "".join(e.get("text", "") for _, e in texts)
            out({"label": label, "secs": round(time.time() - t0, 1), "first_text_s": first, "text_events": len(texts),
                 "tools": tools_in(evs), "ok": f.get("ok"), "cancelled": f.get("cancelled"), "error": f.get("error"),
                 "steps": f.get("steps"), "answer": (f.get("answer") or "")[:400],
                 "streamed_matches_answer": bool(streamed) and streamed.strip()[:60] in (f.get("answer") or ""),
                 "cli_procs": procs()})
            return f
try:
    out({"model": spec, "hello": call("hello").get("model") if isinstance(call("hello"), dict) else None})
    turn("1 tools", "Where does the locket appear across the manuscript, and is it described consistently from chapter to chapter? Check the chapters with your tools before answering.")
    turn("2 follow-up", "Of the inconsistencies you just found, which one would you fix first? Quote the exact sentence and say why in two lines.")
    turn("3 cancel", "Write a 400-word scene in which the narrator finally opens the locket.", cancel_after=4)
    turn("4 recovery", "Forget the scene. In one sentence: which sentence did you say you would fix first, and in which chapter?")
    t0 = time.time(); r = call("command", {"line": "/review 2"})
    # /review runs the team synchronously or as a submitted turn; wait for the graph to settle.
    g = call("graph").get("result")
    out({"label": "5 review", "secs": round(time.time() - t0, 1), "reply": json.dumps(r)[:600],
         "agents": [(a.get("name") or a.get("id"), a.get("model"), a.get("state"), (a.get("session") or {}).get("opens"), (a.get("session") or {}).get("turns")) for a in (g or {}).get("agents", [])],
         "tasks": [(t.get("owner"), t.get("state"), (t.get("error") or "")[:200]) for t in (g or {}).get("tasks", [])],
         "accounts": (g or {}).get("accounts"), "cli_procs": procs()})
    turn("6 reuse", "Thanks. In one line: how many chapters does the manuscript have?")
    out({"label": "usage", "subscription_usage": call("subscription_usage")})
finally:
    proc.terminate(); proc.wait(timeout=20); time.sleep(1)
    out({"label": "after exit", "cli_procs": procs()})
