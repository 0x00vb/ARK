# 0010. Web search: the model's own first, keyless after, no key

Status: Accepted
Date: 2026-10-03

## Context
Agents need to search the web. A search needs an index; ARK cannot build one. The options
were a search API with a key (reliable, but the person sets it up first), keyless scraping of
result pages (no setup, unreliable), or the search some model providers include (no key, but
not every model has one). The person asked for no key: the model's search when it has one,
keyless otherwise.

Measured on 2026-10-03 from one machine:

| Source | Result |
|---|---|
| Claude Code `WebSearch` (`claude -p`, haiku) | relevant, 14 s per search |
| Codex `--search` | relevant, 41 s and about 45,000 input tokens per search |
| Brave's result page | relevant, then 429 after about ten queries, for at least several minutes |
| DuckDuckGo, Mojeek result pages | a challenge page |
| Bing result page | unrelated pages |
| Wikipedia search API | relevant for encyclopedic questions, documented for automated use |

## Decision
One tool, `web.search`, for every model. It asks sources in order and takes the first that
finds something: the turn's own model (`Provider::web_search`: Anthropic's `web_search`
server tool, Claude Code's `WebSearch`, Codex `--search`), then Brave's result page, then
Wikipedia's API. Results are stored and cited like any tool output. No key is needed and
none is asked for. `web.fetch` reads a page as text and refuses this machine and private
networks. Both are `Observe`. An app turns the web on (`ArkBuilder::web`); an app with a
search API of its own adds it as a backend (`WebAdapter::search_with`).

## Consequences
- A turn on Claude or Codex searches well but slowly (one extra model call per search; it
  draws on the same account and its capacity).
- A turn on a local model depends on Brave's page and Wikipedia: general questions fail when
  Brave is rate limiting; the tool then says what each source answered.
- Scraping Brave's page is fragile by nature: a markup change breaks it, and a test of a
  trimmed real page catches that only when the copy is refreshed.
- A search is not metered as a model call in `ark-bench`.

## Check
`crates/ark-tools/src/web.rs` and `web/pages.rs` tests (order of sources, refused addresses,
parsers); `crates/ark-model/src/cli.rs`,
`a_search_turns_on_the_clis_own_web_search_and_nothing_else`;
`crates/ark-orchestrate/tests/orchestration.rs`,
`each_agent_searches_the_web_on_its_own_model_when_its_tools_allow_it`; the `web` workload
in `ark-bench`.
