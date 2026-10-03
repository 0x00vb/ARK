import subprocess, json, uuid, sys
SCHEMA='{"type":"object","additionalProperties":false,"properties":{"action":{"type":"string","enum":["reply","call_tools"]},"text":{"type":"string"},"tool_calls":{"type":"array","items":{"type":"object","additionalProperties":false,"properties":{"name":{"type":"string"},"arguments":{"type":"string"}},"required":["name","arguments"]}}},"required":["action","text","tool_calls"]}'
tools='''
# Host tools

## agents.delegate
hand a self-contained task to another agent and get its result
Parameters (JSON Schema): {"type":"object","required":["to","objective"],"properties":{"to":{"type":"string","enum":["reader-a","reader-b"]},"objective":{"type":"string"}}}
'''
A='''You are the model behind a host application's agent. The host owns the tools listed below and runs them. You have no tools of your own and must not try to run anything yourself. The host tools are not tools you can call directly: calling one by name fails. You use them only by putting them in tool_calls of your structured answer. Every tool listed below is available now; no loading step is needed for it.

Answer every turn with exactly one action:
- "call_tools": put one or more calls in tool_calls (name, and arguments as a JSON object string). The host runs the calls and sends the results back.
- "reply": your answer goes in text.
'''+tools
B='''You are the model behind a host application's agent. Your only tool is StructuredOutput: answer every turn with exactly one StructuredOutput call and nothing else. The host owns the actions listed below and runs them for you. They are not tools of yours; to use one, name it in the tool_calls field of your StructuredOutput call. Every action listed is available now; no loading step is needed.

Each StructuredOutput call has one action:
- "call_tools": put one or more calls in tool_calls (name, and arguments as a JSON object string). The host runs the calls and sends the results back.
- "reply": your answer goes in text.
'''+tools.replace('# Host tools','# Host actions')
task="<transcript>\n<user>\nDelegate 'read doc A' to reader-a and 'read doc B' to reader-b, in parallel.\n</user>\n</transcript>\nContinue as the assistant with one action.\n"
for name,sysp in [("A",A),("B",B)]:
    for i in range(2):
        r=subprocess.run(["claude","-p","--output-format","json","--tools","","--strict-mcp-config","--setting-sources","","--no-session-persistence","--model","sonnet","--system-prompt",sysp,"--json-schema",SCHEMA],input=task,capture_output=True,text=True,timeout=180)
        v=json.loads(r.stdout); u=v['usage']
        print(name,i,"input",u['input_tokens']+u.get('cache_read_input_tokens',0)+u.get('cache_creation_input_tokens',0),"turns",v.get('num_turns'),json.dumps(v.get('structured_output'))[:100],flush=True)
