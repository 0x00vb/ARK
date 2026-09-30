# minimal-agent

The smallest useful ARK agent: two in-process tools (`calc.eval`, `clock.now`) and a
one-line persona. No files, no network, no domain. It exists to show how little
boilerplate a new agent needs.

```bash
ARK_PROVIDER=ollama ARK_MODEL=qwen3:4b-instruct \
  cargo run -p minimal-agent -- "what is 6 * 7?"
```

The interesting part is `build_ark` in `src/main.rs`: register two `Adapter`s, set an
`AgentProfile`, pick a provider, `.build()`. That's the entire integration surface.

`cargo test -p minimal-agent` runs it end-to-end against a scripted mock model — no live
model required.
