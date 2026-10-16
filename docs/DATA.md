# Data work

How ARK works with spreadsheets, CSV files and databases, for app builders. Why: ADR 0033.

A person gives ARK the files and says what they want ("compare these, by customer, chart the
months, give me a clean workbook"). There is no mode to turn on. The agent picks the tools:

| Tool | For | Limit |
|---|---|---|
| Files (`files.read`, `search`, `edit`, `write`; ADR 0019) | seeing a workbook (every sheet's start), changing cells in place and keeping the file's look | reads at most `ark_files::READ_CELLS` (100,000) cells per sheet and says how many rows it left out |
| Run code (`code.run`; ADR 0020, 0032) | anything over many rows or several files: profile, join, clean, total, chart, write workbooks | the project folder only, no network, a timeout; memory not limited yet |
| The `data-analysis` skill | how to do it with care: look first, check joins, keep originals, reconcile totals, plain answers | read when a task needs it |

## Giving ARK the files

| Where the runtime runs | How a file gets there |
|---|---|
| This computer | The project folder in Finder or Explorer, or a drop on the chat |
| Elsewhere (a server, ARK Cloud) | A drop on the chat, or ARK Desktop's Copy to Cloud for a whole folder (ADR 0030) |

A drop puts the files in the project folder (`upload_add`, ADR 0035). A name in use gets a
number, so nothing is overwritten. The composer then says `Added to the project: ...`.

## When a run changes a source

Run code and Files keep each file's version from before a call changes or removes it (ADR
0034). The file's card in that turn has Restore, and after a restore, Undo. Files up to 256 MiB
are kept, and up to 512 MiB in one run. When a changed file was too large to copy, the agent
reads `no copy of the version before`.

## Turning it on

The execution adapter admits two runs at once, with at most eight waiting calls. Runs in the
same project are exclusive. Waiting happens before setup and file copies; a full queue returns
a busy error. CPU usage and each program's memory remain unlimited. These limits are local to
the shared adapter, not the whole machine (ADR 0072).

```rust
let ark = Ark::builder()
    .files(true)   // Files
    .code(true)    // Run code, Python, and the data-analysis skill
    .build()?;
```

On Windows, call `ark_sdk::prelude::ark_exec::entry()` first in `main` (ADR 0032). Linux needs
`bwrap` (bubblewrap).

## What Python has

Python 3.12 with pandas, numpy, matplotlib, openpyxl, duckdb (with its `excel` extension),
python-calamine, xlsxwriter and pyarrow. The first run sets it up under the store with `uv`
(about 300 MB). The list is in `crates/ark-exec/src/python.rs` (`PACKAGES`).

| File | Read with |
|---|---|
| `.xlsx`, `.xlsm` | `pd.read_excel(path, sheet_name=None, engine="calamine")`, or DuckDB `read_xlsx` |
| `.xls`, `.xlsb`, `.ods` | `pd.read_excel(path, engine="calamine")` |
| `.csv`, `.tsv`, `.parquet` | DuckDB `read_csv`, `read_parquet`, or pandas |

## Another skill

The skill comes from the Python runtime (`Runtime::skills`) and is written to
`<store>/exec/skills/data-analysis/SKILL.md` at start. To replace it, put a skill with the same
name in `<store>/skills/` or in the project's `.agents/skills/`.

## Databases

A database is an MCP plugin (`Engine::add_plugin`, or the Plugins screen), not part of Run
code: a run has no network.

- Give the plugin a database login that can only read. That is the guarantee.
- ARK runs a plugin's tools marked `readOnlyHint` without asking. Every other tool asks first.
- For large results, have the agent save the query result to a file in the project and analyze
  the file with Run code.

## Not covered

Power BI and other BI tools: ARK reads what they export and writes workbooks they import. There
is no connector.

Programs run from the Terminal panel keep no versions of the files they change.
