# 0015. A native browser, shared safely between agents

Status: Accepted
Date: 2026-10-04

## Context
Agents need to use sites that have no API: read a page, search a site, fill a form, use the
person's accounts. Two ways were open: Microsoft's Playwright MCP server (mature, needs
Node.js, third-party code) or driving the browser the person already has over the Chrome
DevTools protocol. The owner chose native (2026-10-04): nothing to install for anyone with
Chrome, Edge or Brave. Several agents may browse at once, and the person's signed-in browser
cannot be shared by two agents at the same time.

## Decision
A crate, `ark-browser`, drives an installed Chromium-family browser over the DevTools protocol
with five tools: `browser.open` and `browser.read` and `browser.back` (Observe), `browser.click`
and `browser.type` (Mutate, through the approval gate). A page is shown as its text and a
numbered list of the elements an agent can act on.

| Browser | Profile | Sharing |
|---|---|---|
| Fresh (default) | a headless browser, its profile wiped at each start | each agent (and the conversation) its own context: no cookies or history shared; many at once |
| Signed in (`signed_in: true`) | a visible browser on ARK's own profile (`<store>/browser/signed-in`), where the person signs in once | one agent at a time: the others wait until it has been idle for a minute ([`IDLE`]); agents never negotiate |

Chrome no longer lets a tool drive the person's everyday profile, so "signed in" is a profile
of ARK's own. Addresses on this machine or a private network are refused, as in `web.fetch`.

Tools an agent gets its own of (the web search on its model, the browser) go through one seam,
`ark_runtime::per_agent::PerAgent`: the conversation and the orchestrator ask every source for
an owner's tools at each turn or task.

## Consequences
- A person without Chrome, Edge or Brave gets no Browser (Safari has no DevTools protocol).
- A page the agent opens can link to private addresses, and the browser follows a click there;
  only `open` checks the address.
- The signed-in browser holds the person's sessions: anything an agent does there acts as the
  person, behind the approval gate like every Mutate action.
- Not in the bench: its workloads run without a browser. The browser is tested against the
  Chrome on the test machine, and skipped where there is none.

## Check
`crates/ark-browser/tests/browser.rs` (open, type and submit, click, cookies kept apart per
agent, the signed-in lease, local addresses refused); `crates/ark-orchestrate/tests/orchestration.rs`,
`an_agent_gets_its_own_browser_when_its_tools_allow_it`.
