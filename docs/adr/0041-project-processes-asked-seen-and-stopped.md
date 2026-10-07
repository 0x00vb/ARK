# 0041. Project processes: asked, seen and stopped

Status: Accepted, amended by 0046 (the agent's browser opens the pages a process serves itself)
Date: 2026-10-06

## Context
A person asked ARK Desktop to "run the dev server and show me the /home page". The agent
could not start it and told the person to run `npm run dev` themselves (2026-10-06). What
ARK had:

| Piece | Why it did not serve |
|---|---|
| Run code (`code.run`, ADR 0020, 0032) | Python only, contained, no network, ends at its timeout. A server needs the person's toolchain, the network, a port and no end |
| The window's terminal (`ark-ui/src/terminal.rs`) | The person's own shell. The agent may type a command at its prompt, never press Enter |
| The agent's browser (`browser.open`, ADR 0015) | Refuses this machine's addresses, so a page on `localhost` cannot be opened, by design |
| The person's Browser view (`present {view: "browser"}`) | Opens any address, `localhost` too. The `present` tool did not say that `locator` is the address, so the agent never used it (fixed with this ADR) |

Node was installed (nvm), but in the person's shell setup, which an app started from the Dock
does not read.

The owner's decision (2026-10-06): agents start and stop long-running processes in the
project, as a capability of their own, not through the person's terminal. Generic, not about
npm or web servers. The agent asks before it starts one and shows exactly what runs and where;
"Always in this project" is allowed; the person always sees a process and its output and can
stop it. A missing program goes through the existing request flow (ADR 0013), not "run this
yourself".

## Decision
A crate, `ark-proc`, with one service per runtime (`Processes`) and one tool, `process`.

| Question | Decision |
|---|---|
| The actions | `process.start {command, folder?, port?, open?, wait?}` (Mutate), `process.list {id?}` (Observe: all processes, or one with the end of its output), `process.stop {id}` (Observe: it only ends what ARK started, which the person allowed) |
| What runs | `command` through the person's shell (`$SHELL -c`), in the project folder or `folder` inside it, with the environment of the person's login shell (read once per runtime with `$SHELL -i -l -c env`, as editors do) so their PATH, version managers and settings apply. `BROWSER=none`, so a server does not open a browser of its own. No standard input |
| Containment | None, and the approval says so. A process runs as the person, with their files and the network. This is the difference from Run code, which stays as it is |
| Approval | Every start asks unless granted. The question names the command, the folder and that it is not contained. The approval class is the command and its folder (`ActionDescriptor::grant_per`), so "Always in this project" covers `npm run dev` in `web/`, not every command |
| Seen and stopped | The window has a Processes panel: each process with its command, folder, state, address and live output, and Stop. It opens when a process starts. Both front-ends have `/processes` (list, `stop <id>`) |
| Ready | Code decides, not the model: a process is ready when an address on this machine (`http://localhost:…`, `127.0.0.1`, `0.0.0.0`, `[::1]`) appears in its output and its port accepts a connection, or when `port` accepts one. `start` waits up to `wait` seconds (default 30, at most 300) for ready or exit, then answers with the state, the address and the end of the output. A process still starting keeps running |
| Opening the page | `open` (a path such as `/home`): once ready, the window opens the address with that path in its Browser view. Without `open` nothing opens |
| Several at once | Any number of processes per project, up to 8 running per runtime. Each has a short id (`p1`). A start with the same command and folder as a running process returns that process instead of a second copy |
| Stopping | SIGTERM to the process group (the shell and everything it started), SIGKILL after 5 s. Exited processes stay listed with their exit code (the last 8) |
| Cleanup | A runtime stops its processes when it stops (`Host::stop`, ARK quitting) and when the service is dropped. Each running process is recorded in `<store>/processes/running.json` (process group, start time, command). On the next start the service ends any recorded group whose leader still runs and started when recorded, then clears the file. A group whose leader died before the sweep is left alone: its id may belong to someone else |
| A missing program | `start` checks the command's program in the person's PATH before it runs anything. When it is missing and ARK knows how to install it (Node.js, pnpm, Yarn, Bun, Deno, uv, Rust), the answer tells the agent to call `acquire_capability` with its name. The person sees "ARK needs this to finish: Install Node.js" with the exact install command, and Install or Not now. The install runs as a process (seen in the panel), then the shell environment is read again. Installers are data (`tools.json`), not code |
| Two acquisition sources | `ark_runtime::acquire::Sources` puts several sources behind the profile's one slot: each need goes to the first source that knows it. A source may word its own question (`ask`): the yes button is the first word of its line ("Connect Google", "Install Node.js") |
| Where it is offered | macOS and Linux (`ArkBuilder::processes(true)`), off by default. Not on Windows yet: no process groups there; a job object is the equivalent |

## Consequences
- An agent can run a project's app, test server, watcher or worker and show it, in any
  language, with the person's own tools. The person's terminal stays theirs.
- "Always in this project" for a command is an uncontained grant. It is per command and folder,
  listed and removed in `/grants` like any other, and never covers another command.
- A process the agent leaves running keeps running until stopped, ARK quits, or the person
  stops it. It is not restarted after ARK restarts; the agent starts it again when it needs it.
- On a runtime elsewhere (ARK Cloud, `--serve`), the address is the runtime's machine, not the
  window's. Opening it in the Browser view needs the runtime's server to forward the port.
  Later.
- The agent's own browser still refuses this machine's addresses. Letting it read the pages of
  processes ARK started is a later, narrow exception (ADR 0046).
- Output is kept per process in memory (the last 256 KB). Logs are not written to the store.
- Installs use Homebrew where it is installed, else the tool's official installer. A machine
  without either for a tool gets the reason, and the agent says what is missing.

## Check
`crates/ark-proc/tests/processes.rs`: a server becomes ready at the address it prints, a port
given by the agent, a process that exits early, stop ends the whole group, the same command
returns the running process, the limit, a missing program asks for its install, the sweep ends
a recorded group and leaves an unknown one. `crates/ark-interact/src/labels.rs`:
`a_process_start_asks_with_its_command_and_folder`. `crates/ark-runtime/src/acquire.rs`:
`sources_send_a_need_to_the_first_that_knows_it`. `crates/ark-ui/tests/page.rs` (WebKit):
`a_process_shows_its_output_and_opens_its_page`.
