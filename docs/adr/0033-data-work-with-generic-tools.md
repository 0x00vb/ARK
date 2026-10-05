# 0033. Data work with the generic tools, no analytics mode

Status: Accepted
Date: 2026-10-05

## Context
Analysts and operators work across many workbooks, sheets and CSV files: they compare, join,
clean, total, chart, and sometimes use SQL or Power BI. The owner wants ARK to do this work for
them (2026-10-05), with these limits: no BI product, no "data mode", no choice of library put to
the person, the same code for ARK Desktop and ARK Cloud, and large data kept out of the model's
context.

ARK already had Files (ADR 0019: spreadsheets read and edited in place), Run code (ADR 0020:
Python with pandas in a sandbox) and skills (ADR 0009). What was missing:

| Gap | Seen as |
|---|---|
| Run code only on macOS | ADR 0032 |
| SQL over files, legacy `.xls`, formatted new workbooks | pandas alone: slow joins, no `.xls`, plain output |
| Files read a whole workbook into memory, and showed one sheet | 1.27 GB and 1.7 s for a 300,000-row sheet |
| The agent did not know how a careful analyst works | joins never checked, totals counted twice, sources overwritten |
| The person could not tell what a run was for | the approval showed the program |

## Decision
Small, generic additions to the existing tools. Nothing in `ark-core` or the loop knows about
data.

| Question | Decision |
|---|---|
| Where the data is processed | In a program (`code.run`), never in the model's context. Files keeps at most `READ_CELLS` (100,000) cells per sheet when it reads to show, counts the rest (`Sheet.unread`) and says a program reads all of it. An edit still sees every row |
| Python's tools | duckdb (SQL over CSV, Parquet, data frames, and `.xlsx` through its `excel` extension, installed at setup because runs have no network), python-calamine (fast reads, and `.xls`, `.xlsb`, `.ods`), xlsxwriter, pyarrow, beside pandas, numpy, matplotlib, openpyxl. Pinned in `ark-exec`, one environment on every system |
| How the agent learns the work | A skill, `data-analysis`, that comes with the Python runtime (`Runtime::skills`). The SDK writes it under the store (`exec/skills`) and registers the folder (`Engine::add_skills`, `SkillScope::Builtin`), after the project's and the store's skills, so either can replace it by name. It costs one catalog line per turn; the instructions are read when a task needs them |
| What the person sees | No mode and no setting. `code.run` takes an optional `purpose`, which the approval and the activity row lead with. The grid says when it shows part of a sheet |
| Databases | MCP plugins, as any other service. A tool without `readOnlyHint` is `Mutate` and asks first (`risk_from_annotations`), so reads run and writes ask. The guarantee is a read-only database login, which [`docs/DATA.md`](../DATA.md) tells people to use. No database code in ARK and none in the sandbox, which has no network |
| Power BI and other BI tools | Out of scope. ARK reads what they export (`.xlsx`, `.csv`) and writes workbooks they import. A connector would be an MCP plugin, built when a real user needs one |

## Consequences
- ARK Desktop and ARK Cloud get the same tools from `ark-exec`: Cloud's Linux runtime needs
  bubblewrap (ADR 0032) and nothing else.
- The first setup of Python downloads about 100 MB more.
- An app that turns Run code on gets the skill in its catalog. An app that wants other
  instructions puts a `data-analysis` skill in the store's `skills`.
- Not limited yet: memory and CPU per run (ADR 0032). A run that reads a 2 GB file can use most
  of the machine's memory.
- Not done: dropping files on the chat, the version before a run changed a file, a plain message
  when Windows Excel holds a workbook open (board cards).

## Check
- `ark-bench` workload `workbooks`: two workbooks and a CSV with 54,000 rows and the problems of
  real exports; checked on the saved files, and the sources must not change. Its program passes
  in real Python with `ARK_TEST_PYTHON=1 cargo test -p ark-bench workbooks`.
- `ark-files` tests: a large sheet reads its start and counts the rest, an edit sees all.
- `ark-sdk` test `run_code_comes_with_its_skill`; `ark-ui` page test `analysis` (the purpose in
  the approval, the grid's note).
