# Skill packages

Apps register reviewed `SkillPackage` values with `Engine::add_skill_package`. A package has
an id, name, summary, publisher, version and a list of `PackagedSkill` values. Each skill has a
name and a map of relative file names to UTF-8 content, including `SKILL.md`. Registration
validates metadata, skill frontmatter, paths, allowed data formats and bounded sizes.

## Person controls

| Surface | Action |
|---|---|
| Plugins › Skills | Browse, search, install, view details, remove |
| `/skills` | Show discovered skills |
| `/skills available` | Show app-registered packages and installed versions |
| `/skills install <id>` | Install a registered package on the current computer |
| `/skills remove <id>` | Deactivate a package and keep its files |

Installation is store-wide on the selected computer. It does not change a project's profile
allowlist or grant access to an account. The page identifies the destination and pins the
catalog digest. Builtin and manually installed skills remain visible without false Install
buttons. Local names take precedence; installation refuses collisions rather than replacing
files. Updating keeps the previous release.

## Durable ownership

The store owns `skill-packages/<digest>` releases. Installation writes a private staging
directory, syncs the payload, verifies it and activates a versioned `skill_package` record.
A partial stage is never discoverable. A repeated install verifies a complete release without
copying it again. An operation lock serializes mutations across processes. Restart discovers
only active receipts, including a package an app no longer registers.

Unexpected files, symlinks, missing content and edited payloads are reported. One damaged
package does not hide unrelated valid packages. Removing a receipt preserves all release
files, including local edits. Reinstall does not overwrite those edits.

## Scoped API

Project resources accept `skill_packages`, `skill_package_install` and `skill_package_remove`.
Install input is `{id, digest}`. Remove input is `{id}`. The host validates the current project
location; a remote destination never silently falls back to the local computer. The install
and remove handlers use the same lifecycle as the CLI.

This slice executes no package hooks, downloads or arbitrary commands. Data formats are
Markdown, text, JSON and CSV. Public publishing, remote package sources and general plugin
bundle transactions require a later reviewed contract. A plugin may declare `skills` metadata
for its existing companion skills. This display does not claim a new download transaction.

## Private package import and export

Import selects a local UTF-8 JSON SkillPackage of at most1MiB. Preview validates every field,
path, file type and skill. Unknown fields, including hooks, are refused. The page shows
provided publisher metadata and every file as plain text. Preview installs nothing. Install
confirms the immutable digest on the captured computer. Changing computers discards preview.
An app-registered package id cannot be substituted with different private content.

The existing scoped skill_packages read accepts source for preview, or id to export a
verified installed package. skill_package_install optionally accepts source with digest.
Import shares the existing operation lock, collision checks, immutable staging and receipt.
Export package downloads canonical JSON. Remove preserves files; a damaged payload cannot
be exported as verified content. No archive extraction, downloader or executable hook runs.
