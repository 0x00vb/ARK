# agent-team

A coordinator and two readers built from ARK's orchestration primitives. The coordinator
delegates one document to each reader in a single reply, so they work in parallel, each in
its own context and its own model session. Then it combines their results and stores a
diagram of the shared themes. A rule set bound to the project shapes every task.

```bash
cargo run -p agent-team                                  # Claude and ChatGPT subscriptions
ARK_TEAM_MODEL=ollama:qwen3:4b-instruct cargo run -p agent-team   # one local model for all three
```

| Variable | Default |
|---|---|
| `ARK_LEAD_MODEL` | `claude:sonnet` |
| `ARK_READER_A_MODEL` | `codex` |
| `ARK_READER_B_MODEL` | `claude:haiku` |
| `ARK_TEAM_MODEL` | unset; one model for all three |
| `ARK_TEAM_STORE` | a temporary directory |

It prints each event as it happens, the result, the agent and task graph, each agent's model
session, the stored diagram, and the orchestration as a Mermaid diagram, and writes the graph
to `<store>/graph.json`. Two recorded runs are in `bench/orchestration/`. An excerpt of one
(2026-10-03):

```text
       lead │ started (attempt 1) on claude:sonnet with a 67-token brief
       lead │ delegated t-c5bb3955 to reader-a
       lead │ delegated t-dbdf6499 to reader-b
   reader-a │ started (attempt 1) on codex:default with a 173-token brief
   reader-b │ started (attempt 1) on claude:haiku with a 173-token brief
   reader-b │ done
   reader-a │ done
       lead │ continues
       lead │ ✓ stored flowchart diagram "Themes shared by spring and summer support reviews" v1 (19 line…
```

What to read in `src/main.rs`: the three `AgentSpec`s (who may delegate to whom, which tools
each may use, which model each runs on) and the one `TaskSpec`. Nothing in it is specific to
a team: the same calls build a chain, a hierarchy or a review loop. Guide:
[`docs/ORCHESTRATION.md`](../../docs/ORCHESTRATION.md).
