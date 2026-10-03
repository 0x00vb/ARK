import subprocess, json, re
SCHEMA='{"type":"object","additionalProperties":false,"properties":{"action":{"type":"string","enum":["reply","call_tools"]},"text":{"type":"string"},"tool_calls":{"type":"array","items":{"type":"object","additionalProperties":false,"properties":{"name":{"type":"string"},"arguments":{"type":"string"}},"required":["name","arguments"]}}},"required":["action","text","tool_calls"]}'
T=[("load_action","load one action's input schema so you can call it",'{"type":"object","required":["name"],"properties":{"name":{"type":"string"}}}'),
   ("get_evidence","read exact rows of an earlier tool result",'{"type":"object","required":["invocation"],"properties":{"invocation":{"type":"string"},"path":{"type":"string"},"match":{"type":"string"}}}'),
   ("search_evidence","full-text search over stored artifacts",'{"type":"object","required":["query"],"properties":{"query":{"type":"string"},"limit":{"type":"integer"}}}'),
   ("record_state","add a finding, hypothesis or next step to the working state",'{"type":"object","required":["kind","text"],"properties":{"kind":{"type":"string"},"text":{"type":"string"}}}'),
   ("doc.read","read a stored document's text",'{"type":"object","required":["artifact"],"properties":{"artifact":{"type":"string"}}}'),
   ("rules.lookup","find the bound rules that apply to a topic",'{"type":"object","required":["query"],"properties":{"query":{"type":"string"}}}')]
COMMON='''Answer every turn with exactly one action:
- "call_tools": put one or more calls in tool_calls (name, and arguments as a JSON object string). text may hold a short note. The host runs the calls and sends the results back.
- "reply": your answer to the person goes in text, in Markdown. tool_calls must be empty.

Earlier results appear in the transcript as <tool_result>. Never repeat a call whose result you already have.
'''
APP='''
# Instructions from the application

You read one document and report on it. Read it with doc.read, then answer with its key figures, what went well, what did not, and the plan it states. Report only what the document says.

Capabilities: doc (doc.read), rules (rules.lookup).
Project: reader-b — reads one document and reports its facts
Artifacts, newest first (pass `art:<id>` as the `artifact` argument):
  - art:f3b31fd6e1f4 summer-review.md (text)
'''
S_OPEN='''You are the model behind a host application's agent. Your only tool is StructuredOutput: answer every turn with exactly one StructuredOutput call and nothing else. The host owns the tools listed below and runs them for you. They are not tools of yours; to use one, name it in the tool_calls field of your StructuredOutput call. Every tool listed is available now; no loading step is needed.

'''
S=S_OPEN+COMMON+"\n# Host tools (use them only inside StructuredOutput.tool_calls)\n"+"".join(f"- `{n}`: {d}. Arguments: {p}\n" for n,d,p in T)+APP
X_OPEN='''You are the model behind a host application's agent. You have no tools. The host owns the tools listed below and runs them for you when you ask in your reply. Every reply is exactly one JSON object and nothing else (no prose, no code fence), with this shape:
{"action": "call_tools" or "reply", "text": "...", "tool_calls": [{"name": "...", "arguments": "<JSON object as a string>"}]}
Every tool listed is available now; no loading step is needed.

'''
X=X_OPEN+COMMON+"\n# Host tools (ask for them in tool_calls)\n"+"".join(f"- `{n}`: {d}. Arguments: {p}\n" for n,d,p in T)+APP
task="<transcript>\n<user>\n== RULES ==\nFollow these rules (review-writing v1):\n- [W1] Quote figures exactly as the source states them, with their unit.\n- [W2] Say which document a claim comes from.\n2 more rules cover other kinds of work. Only if this task needs one that is not listed, rules.lookup(query) finds it.\n\n== TASK ==\nTask t-2 delegated by lead (from its task t-1).\n\n== OBJECTIVE ==\nRead the summer support review and report its facts.\n\n== INPUTS ==\n- art:f3b31fd6e1f4 summer-review.md (text)\n\nYour answer is returned to the agent that delegated this task as its result. Answer with the result itself.\n</user>\n<user>\n== WORKING STATE ==\n(empty)\n</user>\n</transcript>\nContinue as the assistant with one action.\n"
def parse_text(t):
    t=t.strip()
    m=re.search(r"\{.*\}", t, re.S)
    try: return json.loads(m.group(0)) if m else None
    except Exception: return None
for model in ["haiku","sonnet"]:
    for name,sysp,structured in [("structured",S,True),("text-json",X,False)]:
        for i in range(3):
            args=["claude","-p","--output-format","json","--tools","","--strict-mcp-config","--setting-sources","","--no-session-persistence","--model",model,"--system-prompt",sysp]
            if structured: args+=["--json-schema",SCHEMA]
            r=subprocess.run(args,input=task,capture_output=True,text=True,timeout=240)
            v=json.loads(r.stdout); u=v['usage']
            out=v.get('structured_output') if structured else parse_text(v.get('result',''))
            calls=[c.get('name') for c in (out or {}).get('tool_calls',[])] if out else None
            ok = bool(out) and out.get('action')=='call_tools' and 'doc.read' in (calls or [])
            print(model,name,i,"turns",v.get('num_turns'),"input",u['input_tokens']+u.get('cache_read_input_tokens',0)+u.get('cache_creation_input_tokens',0),"ok" if ok else "MISS",calls if out else ("unparsed: "+v.get('result','')[:80].replace('\n',' ')),flush=True)
