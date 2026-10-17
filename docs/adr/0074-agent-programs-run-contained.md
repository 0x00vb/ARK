# 0074. The agent programs ARK starts run contained

Status: Proposed
Date: 2026-10-16

Amends 0020 and 0032 (a sandbox's bounds name writes and network) and 0043 › Step 2 limits
("The program is not contained").

## Context
`code.run` runs inside a `Sandbox` (ADR 0020, 0032). The agent programs ARK starts for a task
(ACP workers: Claude Code, Codex, Grok; ADR 0039, 0043, 0061) do not. `ark-acp` starts them with
a clean environment and their own process group, nothing more. ADR 0043 › Step 2 limits: "It
reads anything it can without asking, and Claude Code runs commands it classifies as read-only
(`ls -la` was seen) without a question." Such a command, or the program, can write anywhere the
person can and reach any host. Source: the google/ax review
(`board/evidence/ax-research-2026-10-16.md`, R1).

Spike on macOS 26.4 with `sandbox-exec` (2026-10-16). Each connector got ACP `initialize` and
`session/new`, no prompt (no model call without the owner's OK). Denials were read with
`log stream`.

| Run | Result |
|---|---|
| claude-agent-acp 0.23.1, `code.run`'s profile as is | fails at once: it cannot read its own files |
| the same, reads open, writes to its state | opens a session. Denied: `~/.claude.json.lock`, `~/Library/Caches/claude-cli-nodejs/<project>`, the keychain |
| `claude auth status`, contained | signed out without the keychain; signed in with mach lookups of `com.apple.SecurityServer` and `com.apple.securityd.xpc` |
| claude-agent-acp, the home closed except its program, node, git's settings, global preferences, its state and the keychain; HTTPS | opens a session (18 s; 12.6 s uncontained). No denial that matters |
| codex-acp 0.16.0, writes `~/.codex` | opens a session; its login is a file and works |
| Codex's own sandbox (`codex sandbox macos -- /bin/ls`) inside ARK's | exit 71, `sandbox_apply: Operation not permitted`: macOS does not nest sandboxes, and Codex runs every command in its own (`sandbox_mode="read-only"`) |
| codex-acp with its sandbox off (`danger-full-access`, still `untrusted`) inside ARK's | opens a session. Whether its edits still ask with its sandbox off is not shown without a prompt. It still starts plugins from `~/.cache/codex-runtimes` and ChatGPT.app (denied) |
| grok 1.0.50, signed out | the same contained and not: the session asks for sign-in |
| curl, outbound TCP 443 and name lookups only | api.anthropic.com and chatgpt.com answer; port 80 does not; with no network nothing does |

`sandbox-exec` cannot allow one host and not another. Port 443 and name lookups are the closest.
Not shown: a signed-in prompt contained (a question through ARK, a file written).

## Decision
`ark_exec::sandbox::Bounds` says all a run may touch. `code.run` builds `Bounds::run` and keeps
exactly the profile it has.

| Field | `code.run` | An agent program |
|---|---|---|
| `reads` | `Only`: the system and its environment | `Except`: everything outside the person's home, ARK's store and the system's folder for temporary files. In the home: its program's install folder and its interpreter's, `~/.gitconfig`, `~/.config/git`, the global preferences, its own folders, `~/Library/Keychains` when its login is there. Never, even inside the project: the credential folders (`.ssh`, `.gnupg`, `.aws`, `.azure`, `.kube`, `.docker`, `.netrc`, `.git-credentials`, `.config/gcloud`, `.config/gh`) and ARK's store (unless the project is in it) |
| project, scratch | read and write; kept folders (`.git`, `.claude`, ...) read-only | the same |
| `writes` | none | its own state: a folder, or a file with its siblings named after it and a dot |
| `kept` | none | names inside its state folders that stay read-only: what runs later outside the sandbox |
| `network` | `None` | `Https`: outbound TCP 443 and name lookups |
| `keychain` | no | when its login is there (macOS) |

The catalog (`ark-acp`, data beside `settings_dir` and `isolated_home`):

| Program | Writes | Kept | Keychain |
|---|---|---|---|
| Claude Code | in `~/.claude` only what it writes as it works (`projects`, `todos`, `statsig`, `session-env`, `file-history`, `plans`, `debug`, `telemetry`, `cache`, `paste-cache`, `ide`, `sessions`, `history.jsonl`, `stats-cache.json`, `.credentials.json`); `~/.claude.json` and its siblings; its cache folder. Not its settings, commands, agents, skills, plugins or backups | after each run ARK puts back what it changed in `~/.claude.json` under `mcpServers` and, per project, `mcpServers`, `enabledMcpjsonServers`, `enableAllProjectMcpServers`, `allowedTools`, `hasTrustDialogAccepted`. A file ARK cannot read as JSON after the run comes back whole; one it cannot read before, refuses the start | yes |
| Grok | its private home (ADR 0061) | `node_modules` (its program), `config.toml` (its ask rules), `.npmrc`, `package.json`, `package-lock.json`, `.npm` (what ARK's own update reads) | no |
| Codex | not contained | | |

Whoever opens the store names it (`Catalog::hiding`; `ArkBuilder` passes the store root). A
contained program gets its own folder for the run: the adapter's empty home and `TMPDIR`. Its
state folders are made first when missing (0700). It does not start contained, and says why,
when its settings are moved (`CLAUDE_CONFIG_DIR` set) or the project folder holds the whole
home. Approvals keep their grants: a change and a command still ask. A command's question says
"Contained: besides its own folders, it can change files only there. It reaches the network
only on port 443." instead of "Not contained: it runs as you." Where the system has no sandbox for
programs, the program starts as before.

| System | Agent programs |
|---|---|
| macOS | contained (`Sandbox::programs`) |
| Linux | not yet: `wrap` refuses wider bounds. bubblewrap cannot allow a file's siblings by name, and `--share-net` has no port filter |
| Windows | not yet: `wrap` refuses wider bounds. An AppContainer reads only granted folders, and node, git and the program live in the person's folders |

Codex stays as it is: contained by its own sandbox for its commands, not by ARK. Turning its
sandbox off to put it in ARK's needs a prompt to show its edits still ask. `process.start` stays
Manual (ADR 0064): a dev server listens on a port, the person's tools write caches across the
home (`~/.npm`, `~/.cargo`), and an install needs the network. Neither was spiked further.

## Consequences
- On macOS a contained program, and every command it runs, writes only the project (not its kept
  folders), its run folder and its declared state, and reaches hosts on port 443 only.
- It is not isolated. It reads what is outside the home and sends it to any host on 443. Name
  lookups can carry data out (DNS tunnelling), and `system-socket` is allowed for its sockets.
- The keychain lookup is not limited to its own login: a command can run
  `security find-generic-password -w` and get any item that trusts `/usr/bin/security` (go-keyring
  tools such as `gh` store theirs that way). Hiding `.config/gh` does not stop that for Claude Code.
  The login keychain file itself is readable (the spike: no login without it), so it can be sent
  out for an offline attack on its password.
- `~/.claude.json` stays writable. A change to its MCP servers or allowed tools is put back after
  the run, under Claude Code's own lock, through a link to the file. Not covered: the person's own
  `claude` reading it before that; a change the person makes there during the run (`claude mcp
  add`), which is undone too; a process the program detached (`setsid`), which outlives the run
  and keeps its write access.
- The keychain lookup also lets it change items (`security add-generic-password -U`), for example
  the person's Claude login.
- A project's own `.mcp.json` is writable like its other files; Claude Code's trust prompt is
  what stands before it runs those servers.
- A program found through a version manager's shim (`~/.asdf/shims`, mise) opens only the shim's
  folder, so it may fail to start contained. A program kept in the home itself never opens the home.
- Claude Code's memory files (`~/.claude/projects/*/memory`) stay writable: text a later session
  reads, like the project's own files.
- A grant for a command covers it contained or not (the class is unchanged).
- A worker cannot `git add`, `commit` or branch (`.git` is kept), nor edit `.github` or `.vscode`,
  even after ARK allows it. The person does those. In a git worktree `.git` points outside the
  project, which is not writable either.
- Programs outside their install folder in the home (`~/.cargo/bin`, `~/.local/bin`) cannot be run
  by a contained worker.
- `ark-acp` depends on `ark-exec` (no new crates: both already sit on `ark-runtime` and
  `ark-tools`). The mechanism stays in one crate.
- `Bounds` and `ark_acp::Entry` gain fields (Breaking for struct literals).

## Check
`ark-exec`: `sandbox::macos` tests hold `code.run`'s profile to its exact text from 990adf3 and
check the order of a program's rules; `tests/programs.rs` (macOS) runs `sh` with agent bounds: it
writes its state folder, its state file and `.lock`, not `state.json2`, not a kept name, not
another folder, not `.git`; it reads neither a hidden file nor a `never` folder inside the
project; no network and HTTPS both refuse a port that is not 443. `ark-acp`: the bounds of a
catalog entry; a moved settings folder and a project holding the home are refused; Codex is not
contained; `guard` puts back an added MCP server or allowed tool and keeps the file's other
changes. `ark-interact`: a command's question says whether it is contained.
`ark-orchestrate/tests/worker.rs`: the scripted program runs contained on macOS; its allowed
change lands, its write outside the project and its read of the store fail. Before a release: a
signed-in prompt on Claude Code and on Grok, contained (`tests/worker_live.rs`, the owner's OK).
