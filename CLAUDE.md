# ARK

Domain-agnostic agent runtime (Rust workspace). Read `CONTRIBUTING.md` for the one hard rule:
no tool or domain name in `ark-core`.

## Working here

- `cargo test --workspace` must stay green. No live model needed; use `MockProvider`, or
  `ARK_PROVIDER=mock ARK_MOCK_SCRIPT=<file>` to drive a real app binary.
- The runtime never knows about terminals. Anything visual belongs in `ark-cli` (or a
  future `ark-ui`), fed by `HarnessEvent`, `ApprovalGate` and `CancelToken`.
- Terminal history is not LLM context. Do not add anything that replays the transcript into
  a prompt. `crates/ark-sdk/tests/long_session.rs` guards this.
- User-facing docs live in `docs/`. Update `docs/CLI.md` when shell behavior changes and
  `CHANGELOG.md` for any public API change.

## Writing style (docs, comments, commit messages, reports)

No AI-generated slop. Write like an engineer explaining something to a colleague.

- No em dashes or en dashes. Use a period, a comma, a colon or parentheses.
- Short sentences, plain words. Lead with what matters.
- Keep it short. Prefer a table or a short list to paragraphs. Cut anything the reader
  already knows from the code or the heading.
- Say what a thing does, not how good it is. No marketing words ("seamless", "robust",
  "powerful", "leverage", "delve", "comprehensive", "cutting-edge").
- No filler openers or closers ("It's worth noting", "In summary", "Overall").
- Never invent output. Examples in docs come from real runs or tests, and say so.
