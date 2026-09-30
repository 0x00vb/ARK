# writing-agent

A novelist's developmental editor, built on the **same** ARK runtime as the security
workstation ARK was extracted from — with zero changes to any ARK crate. This is the
concrete answer to the test ARK holds itself to:

> Could this exact harness power an agent whose only job is helping an author write a novel?

It registers manuscript tools (`manuscript.list/read/wordcount/save`, `continuity.search`),
a writer's persona, writing-domain capabilities (`structure-analysis`, `continuity-check`,
`revision`, `drafting`), and a **custom result distiller** that turns tool output into
writer-facing evidence. The runtime's context budget, model routing, evidence model,
working-state graph, approval gate (note `manuscript.save` is `Mutate` and needs approval),
and reproducible notebook all apply unchanged.

```bash
ARK_PROVIDER=anthropic ARK_MODEL=claude-sonnet-5-5 \
  cargo run -p writing-agent -- "is chapter 1 too long, and does it mention the locket?"
```

`cargo test -p writing-agent` runs it end-to-end against a scripted mock model, and asserts
the approval gate blocks an unapproved save.
