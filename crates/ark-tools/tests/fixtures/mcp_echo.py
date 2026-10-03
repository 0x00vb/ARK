#!/usr/bin/env python3
"""A minimal MCP server over stdio for ARK's tests.

Tools: `echo` (read-only) returns its `text`; `stamp` (changes state) returns "stamped".
`--silent` answers nothing, to test the handshake limit. Standard library only."""
import json
import sys

TOOLS = [
    {"name": "echo", "description": "Return the text it is given",
     "inputSchema": {"type": "object", "properties": {"text": {"type": "string"}}},
     "annotations": {"readOnlyHint": True}},
    {"name": "stamp", "description": "Mark something as done",
     "inputSchema": {"type": "object", "properties": {}},
     "annotations": {"destructiveHint": True}},
]


def reply(id_, result):
    sys.stdout.write(json.dumps({"jsonrpc": "2.0", "id": id_, "result": result}) + "\n")
    sys.stdout.flush()


def main():
    silent = "--silent" in sys.argv
    for line in sys.stdin:
        if silent:
            continue
        try:
            msg = json.loads(line)
        except ValueError:
            continue
        method, id_ = msg.get("method"), msg.get("id")
        if id_ is None:
            continue  # a notification
        if method == "initialize":
            reply(id_, {"protocolVersion": "2024-11-05", "capabilities": {"tools": {}},
                        "serverInfo": {"name": "echo", "version": "1"}})
        elif method == "tools/list":
            reply(id_, {"tools": TOOLS})
        elif method == "tools/call":
            p = msg.get("params", {})
            text = p.get("arguments", {}).get("text", "") if p.get("name") == "echo" else "stamped"
            reply(id_, {"content": [{"type": "text", "text": text}]})
        else:
            sys.stdout.write(json.dumps({"jsonrpc": "2.0", "id": id_,
                                         "error": {"code": -32601, "message": "no such method"}}) + "\n")
            sys.stdout.flush()


main()
