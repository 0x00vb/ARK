# writer

A developmental editor for novelists on ARK. It is the "could this runtime power a novel?"
proof, and since v0.2 it is an interactive app on the same shell as the security workbench.
Nothing in ARK knows it is a writing tool.

```bash
cargo run -p writing-agent                  # open the writer's shell
writer --resume                             # continue the last session
writer -p "how long is chapter 4?" --json   # one-shot, for scripts
```

The manuscript is the current directory when it holds `.md` or `.txt` chapters. Otherwise a
four-chapter sample novel is seeded into the project workspace. The store lives in
`$WRITER_ROOT` (default `~/.writer`).

What the app adds on top of ARK:

| Piece | What |
|---|---|
| Tools | `manuscript.list/read/wordcount/save`, `craft.continuity`, `craft.voice`; `web.search`, `web.fetch` |
| Persona | an editor that reads before it judges and keeps the narrator's voice |
| Distiller | writer-facing summaries (`"locket": 2 mention(s) in chapter-01.md, chapter-02.md`) |
| Commands | `/manuscript`, `/chapter <n>`, `/style [n]`, `/review <n>` |
| Agents | `/review` runs an editor agent that delegates to a continuity reader and a voice reader at once, each with only its own tools; `/agents` shows who did what |
| Look | magenta accent, `✎` prompt, labels like `Tracing "locket" through the manuscript` |
| Approvals | `manuscript.save` asks first and shows the file and word count |

`cargo test -p writing-agent` runs it against a scripted mock model, including a gated save
and an approved save inside a session.
