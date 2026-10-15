"""Measure inherited Codex tool context with a fixed synthetic prompt. No config writes."""
import json
import os
import selectors
import re
import subprocess
import tempfile
import time
from pathlib import Path


def probe(isolated):
    proc = subprocess.Popen(
        ["codex", "app-server", "-c", 'sandbox_mode="read-only"'],
        stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL,
        text=True, bufsize=1,
    )
    selector = selectors.DefaultSelector()
    selector.register(proc.stdout, selectors.EVENT_READ)
    counter = 0
    buffered = b""

    def send(method, params):
        nonlocal counter
        counter += 1
        proc.stdin.write(json.dumps({"id": counter, "method": method, "params": params}) + "\n")
        proc.stdin.flush()
        return counter

    def receive(deadline):
        nonlocal buffered
        while time.monotonic() < deadline:
            if b"\n" in buffered:
                line, buffered = buffered.split(b"\n", 1)
                return json.loads(line)
            if selector.select(0.5):
                chunk = os.read(proc.stdout.fileno(), 65536)
                if not chunk:
                    raise RuntimeError("vendor process closed")
                buffered += chunk
        raise TimeoutError("vendor probe timed out")

    def request(method, params):
        identity = send(method, params)
        deadline = time.monotonic() + 30
        while True:
            message = receive(deadline)
            if message.get("id") == identity:
                if "error" in message:
                    raise RuntimeError("vendor RPC rejected " + method + ": " + message["error"]["message"])
                return message["result"]

    try:
        request("initialize", {"clientInfo": {"name": "ark-cost-probe", "version": "0.24.0"}})
        proc.stdin.write(json.dumps({"method": "initialized", "params": {}}) + "\n")
        proc.stdin.flush()
        config = request("config/read", {"includeLayers": False})["config"]
        overlay = {}
        if isolated:
            overlay = {
                "features.shell_tool": False,
                "features.apps": False,
                "web_search": "disabled",
            }
            for name in config.get("mcp_servers", {}):
                if not re.fullmatch(r"[A-Za-z0-9_-]+", name):
                    raise RuntimeError("probe does not accept dotted server names")
                overlay['mcp_servers.' + name + '.enabled'] = False
        with tempfile.TemporaryDirectory(prefix="ark-cost-probe-") as directory:
            thread = request("thread/start", {
                "cwd": directory, "model": "gpt-5.6-terra",
                "approvalPolicy": "never", "sandbox": "read-only",
                "baseInstructions": "Answer the user's greeting in French. Do not use tools.",
                "config": overlay,
            })["thread"]["id"]
            started = time.monotonic()
            request("turn/start", {
                "threadId": thread,
                "input": [{"type": "text", "text": "Say hello in French."}],
                "effort": "medium",
                "outputSchema": {"type": "object", "properties": {"answer": {"type": "string"}},
                                 "required": ["answer"], "additionalProperties": False},
            })
            usage = None
            answer = None
            deadline = time.monotonic() + 90
            while True:
                event = receive(deadline)
                params = event.get("params", {})
                if params.get("threadId", thread) != thread:
                    continue
                if event.get("method") == "thread/tokenUsage/updated":
                    usage = params["tokenUsage"]["last"]
                if event.get("method") == "item/completed" and params.get("item", {}).get("type") == "agentMessage":
                    answer = params["item"].get("text")
                if event.get("method") == "turn/completed":
                    if params.get("turn", {}).get("status") != "completed":
                        raise RuntimeError("probe turn failed")
                    break
            request("thread/archive", {"threadId": thread})
            return {"isolated": isolated, "usage": usage, "answer": answer,
                    "wall_seconds": time.monotonic() - started,
                    "configured_mcp_count": len(config.get("mcp_servers", {}))}
    finally:
        proc.terminate()
        try:
            proc.wait(timeout=5)
        except subprocess.TimeoutExpired:
            proc.kill()
            proc.wait()
        selector.close()


if __name__ == "__main__":
    destination = Path(__file__).with_name("vendor-probe.json")
    results = json.loads(destination.read_text()) if destination.exists() else []
    for isolated in [False, True, True, False, False, True][len(results):]:
        try:
            results.append(probe(isolated))
        except (TimeoutError, RuntimeError) as error:
            results.append({"isolated": isolated, "error": str(error)})
        destination.write_text(json.dumps(results, indent=2) + "\n")
        print(json.dumps(results[-1]), flush=True)
