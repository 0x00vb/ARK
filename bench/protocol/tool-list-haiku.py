import subprocess, json, sys
SCHEMA='{"type":"object","additionalProperties":false,"properties":{"action":{"type":"string","enum":["reply","call_tools"]},"text":{"type":"string"},"tool_calls":{"type":"array","items":{"type":"object","additionalProperties":false,"properties":{"name":{"type":"string"},"arguments":{"type":"string"}},"required":["name","arguments"]}}},"required":["action","text","tool_calls"]}'
OPEN='''You are the model behind a host application's agent. Your only tool is StructuredOutput: answer every turn with exactly one StructuredOutput call and nothing else. The host owns the tools listed below and runs them for you. They are not tools of yours; to use one, name it in the tool_calls field of your StructuredOutput call. Every tool listed is available now; no loading step is needed.

Answer every turn with exactly one action:
- "call_tools": put one or more calls in tool_calls (name, and arguments as a JSON object string). text may hold a short note. The host runs the calls and sends the results back.
- "reply": your answer to the person goes in text, in Markdown. tool_calls must be empty.
'''
T=[("load_action","load an action's input schema by name",'{"type":"object","properties":{"name":{"type":"string"}}}'),
   ("doc.read","read a stored document's text",'{"type":"object","required":["artifact"],"properties":{"artifact":{"type":"string"}}}')]
heads="\n# Host tools\n"+"".join(f"\n## {n}\n{d}\nParameters (JSON Schema): {p}\n" for n,d,p in T)
bullets="\n# Host tools (use them only inside StructuredOutput.tool_calls)\n"+"".join(f"- `{n}`: {d}. Arguments: {p}\n" for n,d,p in T)
example='''
Example. To read art:abc, call StructuredOutput with {"action":"call_tools","text":"","tool_calls":[{"name":"doc.read","arguments":"{\\"artifact\\":\\"art:abc\\"}"}]}.
'''
V={"current":OPEN+heads,"bullets":OPEN+bullets,"example":OPEN+example+heads}
task="<transcript>\n<user>\nTask t-1.\n== OBJECTIVE ==\nRead the document and report its key figures.\n== INPUTS ==\n- art:74ae89d31c6e spring-review.md (text)\n</user>\n</transcript>\nContinue as the assistant with one action.\n"
for name,sysp in V.items():
    for i in range(2):
        r=subprocess.run(["claude","-p","--output-format","json","--tools","","--strict-mcp-config","--setting-sources","","--no-session-persistence","--model","haiku","--system-prompt",sysp,"--json-schema",SCHEMA],input=task,capture_output=True,text=True,timeout=180)
        v=json.loads(r.stdout); u=v['usage']
        print(name,i,"turns",v.get('num_turns'),"input",u['input_tokens']+u.get('cache_read_input_tokens',0)+u.get('cache_creation_input_tokens',0),json.dumps(v.get('structured_output'))[:110],flush=True)
