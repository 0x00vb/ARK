# 0083. A board per goal opens beside the chat

Status: Accepted
Date: 2026-10-26

## Context
ADR 0023 gave each project one board, and ADR 0065 put it in the fixed destination rail. Most
chats never plan work, so the rail kept a place for a view most people did not open. A project
that pursues several goals mixed all of them in one set of columns. The owner asked
(2026-10-25) for the board to leave the fixed rail, to appear when the agent plans work, and to
hold different boards for different goals.

## Decision
A **board** (`ark_orchestrate::Board`) is the work toward one goal: `id` (`b-` and eight hex
digits), `goal`, who made it, and the chat whose conversation made it. Each `WorkItem` carries
its `board`. Boards are stored as `orch_board` records and traced as `EventKind::Board` and
`BoardRemoved`.

| Question | Decision |
|---|---|
| Which board an item goes on | `board.add` needs `board`: an existing board's goal or id, or a new goal, which makes a board. The same goal in other case or spacing is the same board. `WorkChange::board` places a new item or moves one; without it a new item goes on the board with the latest activity |
| What reaches the model | `board.list`'s description names the 6 latest boards by goal with their counts; `board.list` returns each with its open items (40 at most in all) and its latest 5 done |
| Where the person sees it | A panel in the side pane (`board`, `presents: ["board"]`), not a rail destination. An item opens in the board's place, with Back. The current project in the sidebar lists its boards with progress and what waits for the person |
| When it opens | Code, not the model: a board made by this window's chat opens beside it (the board keeps the chat, taken from the turn's work id). The agent can also open one with `present {view: "board", locator: <board id>}`, and the person from the project's list |
| Items made before boards | On load, items whose board does not exist join one board named "Board", saved once |
| Removing | Deleting a board removes its items; their tasks stay. Two boards never share a goal |

## Consequences
- The rail is one destination shorter, and nothing about boards shows until a project has one.
- A person who wants a board without an agent asks the chat or uses `/board add`; the window has
  no "New board" control.
- A board opens in every window of its chat, not in other chats of the project.
- Ranks are kept per board; moving an item to another board puts it at the end of its column.
- The window's methods gain `board_rename` and `board_delete`; `board` also returns `boards`.

## Check
- `crates/ark-orchestrate/tests/board.rs`: `each_goal_has_its_own_board`,
  `items_made_before_boards_had_goals_join_one_board`, and the conversation planning on a goal's
  board.
- `crates/ark-interact/tests/interaction.rs`, `board_command_shows_and_changes_the_board`.
- `crates/ark-ui/tests/page.rs` (WebKit), `the_board_is_worked_by_the_person_and_the_agents`:
  no rail destination, the chat's board opens beside it, the project lists it, rename, reopen
  and delete.
