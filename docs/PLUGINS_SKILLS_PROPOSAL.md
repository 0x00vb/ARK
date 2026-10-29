# Plugins and Skills marketplace proposal

Status: proposed, 2026-10-29. Source investigation and design brief. No new product installer or
publishing service exists in this change. Board: plugins-skills-marketplace-20261029.

## Outcome

A person finds a capability or skill, sees its source and included skills, and installs it
without opening a terminal. Installing a plugin can include its reviewed companion skills.
People can author a portable skill package, try it privately and submit a reviewed release.

The supplied screenshot establishes browsing, installed items, search and direct actions as
references. ARK keeps its own Plugins destination, tokens, chat and account model. No copied
application shell. No Popular section without actual ranking data.

## Current source, verified

| Evidence | What exists | What it does not establish |
|---|---|---|
| crates/ark-ui/web/js/marketplace.js | Global Plugins view, search, categories, provider and app entries, details, connect/disconnect | Searchable skill catalog or skill installation |
| crates/ark-ui/web/js/skills.js | Advanced Skills list and a prefilled `npx skills add` terminal action | Direct installation, progress or an installation receipt |
| crates/ark-runtime/src/skills.rs | SKILL.md discovery, project/store/builtin scope, most-specific-name precedence, bounded on-demand reads | Package ownership, updating or removal |
| crates/ark-runtime/src/plugins/catalog.rs | CatalogEntry::Package and PackageComponent::Skill metadata | A working package installer |
| crates/ark-runtime/src/plugins.rs | marketplace Add persists id and added_at | Downloading, installing or activating a package |
| docs/adr/0069-plugins-are-reviewed-capabilities-from-independent-providers.md | Existing trust, state, person-only lifecycle and portable package decision | Proof that all described operations shipped |
| ../ark-desktop-blender/src/games.rs:190-202 and skills/game-development/SKILL.md | Desktop already supplies a companion game-development skill through Engine::add_skills | A general downloadable plugin/skill bundle lifecycle |

The existing `npx skills` ecosystem is useful for discovery and format compatibility. Current
upstream supports noninteractive selection and copies, but lists no ARK-specific target in its
agent map. Do not pass an invented `--agent ark`, run an unpinned npx command, or install into
other agents' directories from ARK. A compatible source importer can sit behind ARK's existing
person-only lifecycle. Start with curated pinned, data-only skill packages. Determine whether
the upstream library has a stable usable interface before adding a runtime dependency.

The existing game-development companion is a concrete reuse case. Generalize the package
lifecycle around that capability/skill relationship, without replacing its tool contract.
Built-in skills remain marked Comes with ARK and have no misleading Install or Remove action.
A project skill can shadow a store/builtin name; surface that scope instead of claiming the
wrong content is active.

## Interface and states

Physical scene: a person interrupts daytime project work on a laptop to add one capability,
then returns to the same chat. Use the existing light/dark system choice, Manrope, paper/ink,
hairline lists and quiet blue selection. No new visual identity.

| Area | Proposed behavior |
|---|---|
| Navigation | Existing Plugins destination. A Plugins / Skills selector inside it. |
| Browse | Search plus Browse / Installed. Categories group actual available entries. |
| Rows | Icon, name, one useful sentence, source and actual state. Install, Connect or Open as appropriate. |
| Details | Included skills, publisher, pinned version, access and destination. Source details expand inline. |
| Installation | One Install action for a reviewed data-only package. The detail already shows the contents and destination. |
| Bundles | Plugin detail says which skills come with it. One package operation handles the selected plugin and its companion skills. OAuth remains the service's own Connect step. |
| Personal | Add source or import a local package. Authorship does not imply trusted public publication. |
| Narrow | One column, wrapping labels, full-width search and type selector; no second permanent sidebar. |
| Return | Search, filter and focus survive detail navigation, refresh and a failed operation. The open chat and draft remain intact. |

A plus icon may be a compact Install control only with an accessible name including the item.
Use named buttons for connection and failure recovery. Do not use the same control to install,
connect an account and grant future publish authority without showing those separate states.

| State | Behavior |
|---|---|
| Loading | Local installed entries remain usable. Announce the current catalog fetch. |
| Empty or no match | Distinct empty catalog and no matching results; clear search restores the list. |
| Offline | Installed items remain visible. Cached offers disclose staleness; unavailable downloads cannot pretend to install. |
| Installing or cancelling | One durable operation, duplicate clicks join it, relevant row action disabled. |
| Partial | Show the component that failed. A copied skill is not proof that its plugin connected. |
| Collision or modified local skill | Preserve existing bytes. Offer a different name or explicit replacement review. |
| Update | Show a candidate separately. Running turns keep their current skill snapshot; next turn uses accepted content. |
| Remove | Remove only package-owned, unchanged files. Shared or independently installed skills survive. Account revocation is separate. |
| Refused, failed, interrupted | Keep exact outcome and progress. Retry reconciles the old operation before repeating effects. |

Keyboard acceptance includes selector arrows, search focus, row access, detail return and focus
restoration after failure. Verify light/dark, 390px and desktop, reduced motion, long names,
non-Latin text, duplicate skill names and large catalogs. Use existing brand tokens.

## Data before logic

Reuse the existing catalog and typed operation contracts. Do not create a second connector
registry or put service names into generic runtime code. Proposed additions need an ADR0069
amendment and backwards-compatible protocol review before implementation.

| Record | Required data |
|---|---|
| Package identity | Publisher namespace, package id, immutable release/revision and content digest |
| Source | Catalog identity and verified source locator; resolved immutable revision and license provenance |
| Components | Current typed Skill component names/digests; companion relationships, requirements and optional components |
| Install plan | Destination computer/project/store identity, expected catalog revision, exact selected components, conflicts and bytes |
| Ownership receipt | Operation id, package release, each installed relative path/digest and shared ownership references |
| Update | Old accepted release, staged candidate, changed access/components and rollback receipt |
| Publication candidate | Local package inventory, tests/review, publisher identity, immutable version and license; unsigned draft remains private |

Execution outline: read inert metadata, prepare exact plan, validate source and bounded archive,
stage without hooks, validate SKILL.md and local references, then publish accepted owned files
and receipt with interruption recovery. No package scripts execute on install. Catalog trust
is not code safety. Skill instructions cannot approve tools, reveal credentials or bypass an
ordinary action approval. Avoid broad installation to every coding agent.

For bundle failure, durable component state distinguishes package preparation from connection
and activation. On restart, inspect the exact staged/published paths before resuming. Removal
consults the ownership receipt and preserves a locally edited file. A duplicate user install
must not result in a second copy or delete another package's shared skill later.

## Delivery slices

| Slice | Acceptance |
|---|---|
| 1. Skill discovery and curated local install | Search Skills in Plugins; install one reviewed local package; restart; real skills.read returns its exact content; no terminal or other-agent writes. |
| 2. Plugin companion bundle | One plugin plus its skill from one install; denied OAuth produces Needs connection; reinstall/restart does not duplicate; remove preserves shared/edited skills. |
| 3. Pinned external sources | Download/search cancel, digest mismatch, path traversal, symlink, oversize, collision, interrupted write and concurrent operation tests; old accepted release retained. |
| 4. Author and publish | Export a portable package; privately import on a fresh profile; review immutable release; public publish only with owner authorization. |
| 5. Social publishing adapters | Shared editorial calendar and per-channel capabilities; durable verified publish receipts. See SOCIAL_PUBLISHING_PROPOSAL.md. |

First plan/design review covers slices1-2, the state matrix, current protocol compatibility and
ownership/recovery. The preview foundation pack is prepared separately in board evidence. No
public marketplace, catalog counts, paid offer or author account is claimed by this proposal.

## Checks and limits

This is documentation and data preparation. No app code, installed skills, accounts or remote
services change. Structural validation of preview skills cannot establish model behavior or
installer correctness. Product implementation must pass repository CI, protocol parity, actual
served UI and current native app proof. Independent plan/design review remains before code.

Current primary ecosystem references, checked 2026-10-29:
- https://agentskills.io/specification
- https://github.com/vercel-labs/skills
- https://raw.githubusercontent.com/vercel-labs/skills/main/docs/commands.md
- https://raw.githubusercontent.com/vercel-labs/skills/main/src/agents.ts
