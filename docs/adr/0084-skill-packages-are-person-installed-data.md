# 0084. Skill packages are person-installed data

Status: Accepted for local implementation
Date: 2026-10-29

The owner approved the marketplace plan/design and foundation preview. This amends ADR0069's
package implementation boundary, without authorizing a public catalog or social posts.

The app registers reviewed data-only packages. Core owns their validated shape, durable active
receipts and source discovery. Each release has a content-addressed owned directory. Files are
staged and synced before activating the receipt. Restart never discovers a partial stage.
Reinstall verifies an existing complete release. Changed local files are preserved and reported.
Remove deactivates the receipt and preserves payloads.

The first unit accepts no downloads, hooks, scripts or executable servers. Installing a package
does not authorize tools. Preserve project/store/builtin precedence, surface collisions, and
keep running turns on their existing skill snapshot.

Two designs were considered: a live npx wrapper, or existing-format skills with an owned local
lifecycle. Choose the latter. It avoids executable bootstrap, Node requirements and writes into
other agents' directories. A pinned upstream importer/search adapter remains a later slice.

The UI offers Plugins/Skills, search and Installed. App capabilities can disclose companion
package ids. Existing Games built-in companionship remains. External plugin payloads, catalog
publishing and account grants remain separate work. Calendar presentation reuses ADR0053.
