# 0009. Skills: names in context, instructions on demand

Status: Accepted
Date: 2026-10-03

## Context
People install skills (Agent Skills: a folder with a `SKILL.md` that names the skill, says when
to use it, and gives instructions) with tools such as `npx skills add`, which writes them to the
project's `.agents/skills`. A skill's instructions can run to thousands of tokens. Sending every
installed skill with every request would break the bounded context (0002) and pay for text no
task uses. ARK already solves the same shape for rules: a small selection rides in the request,
the rest is one call away.

## Decision
Skills come from two places, the project folder's `.agents/skills` and the store's `skills`; a
name in the project hides the same name in the store (`ark_runtime::skills`). A turn carries only
a catalog: one line per skill it may read (name and description), under a token cap
(`CATALOG_BUDGET`), in the first message after the rules, reported as `skills` in each
`ContextReport`. `skills.read` returns a skill's instructions whole (up to a cap) and lists the
files beside them; `skills.file` reads one of those files. Both are read-only and loaded up front
when a catalog is present.

The conversation reads every skill by default (`ArkBuilder::skills`). An orchestrated agent reads
only the skills its spec names (`AgentSpec::skills`, empty by default); `agents.create` can name
them. ARK installs nothing: the person runs an installer or copies a folder. ARK does not preload
a skill's instructions; that waits for a benchmark that pays for it.

## Consequences
- A turn with no skills is unchanged (the bench baseline did not move).
- A turn with skills pays for the catalog on every request (a few tokens per skill) and for one
  `skills.read` call when it uses one.
- A skill's instructions come from whoever wrote it. They reach the model as a tool result, with
  the approval gate still in front of every state-changing action.
- Scripts a skill mentions run only through tools the app gives (ARK adds no command runner).
- Front matter is read with a small parser (top-level `key: value`, quoted values, `>` and `|`
  blocks). Nested keys are ignored.

## Check
`crates/ark-runtime/src/skills.rs` tests (discovery, precedence, catalog cap, reads stay in the
folder); `crates/ark-runtime/tests/request_profile.rs`,
`the_skills_catalog_is_counted_as_its_own_part`; `crates/ark-interact/tests/interaction.rs`,
`the_conversation_reads_a_skill_and_agents_get_the_ones_they_are_given`; the `skill` workload in
`ark-bench`.
