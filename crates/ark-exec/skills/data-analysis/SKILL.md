---
name: data-analysis
description: Work with spreadsheets, CSV files and tables like a careful analyst: compare, join, clean, total, chart, or make a new workbook. Read it before work over several files or sheets, many rows, or numbers that must be right.
---

# Data analysis

The person sees your answer and the files you write, not your programs. The numbers must be
right, and the person must be able to check them.

## 1. Look before you compute

- `files.list`, then `files.read` each source without a sheet: you get the start of every sheet
  and how many rows were not read. Do not read a large sheet piece by piece; a program reads all
  of it.
- With several or large files, one `code.run` prints a profile of each: sheets, row count,
  columns and types, empty cells, distinct values of the key columns, smallest and largest
  numbers and dates, duplicate keys. Print summaries, never whole tables.
- Write down what is wrong before fixing it: the header not on the first row, title rows, total
  or subtotal rows, numbers stored as text (`1.234,50`, `$1,200`, `(300)`, `12%`), dates as text
  or as Excel serial numbers, IDs with spaces, other case or lost leading zeros, duplicate rows,
  empty rows and columns.

## 2. Use the right tool

In `code.run` with Python:

| To | Use |
|---|---|
| Read workbooks | `pd.read_excel(path, sheet_name=None, engine="calamine")` reads every sheet at once; also `.xls`, `.xlsb`, `.ods` |
| Read CSV | `pd.read_csv(path, dtype={"id": str})`: IDs as text keep their leading zeros |
| Join, group, filter many rows or several files | DuckDB SQL: `duckdb.sql("select ... from read_csv('a.csv') join df using (id)").df()`. A data frame is a table by its variable name; `read_xlsx('f.xlsx', sheet='S')` reads a sheet |
| Chart | matplotlib; `plt.savefig("chart.png", dpi=150, bbox_inches="tight")` |
| New workbook | `pd.ExcelWriter(path, engine="xlsxwriter")`, with formats |
| Change some cells of an existing workbook and keep its look | `files.edit` |

Data stays in the program: never paste rows into the conversation or into a program's source.
A database comes through a connected plugin: query it, save the result to a file, analyze the
file.

## 3. Join with care

Before a join, make the keys the same on both sides (strip, same case, text, leading zeros),
then print:

- whether each side's key is unique (`count(*)` against `count(distinct key)`)
- how many keys match, how many on each side do not, and a few that do not
- the row count before and after: a left join that gains rows has duplicate keys on the right

Say in the answer what did not match and what that means for the result.

## 4. Clean without losing anything

- Never change a source file unless the person asked. Write results to a new file
  (`sales cleaned.xlsx`, `summary.xlsx`).
- Take total and subtotal rows out before you aggregate.
- Keep what you remove or doubt: a sheet `Removed` or `Check`, with a reason per row.
- Convert types on purpose, count the values that did not convert, and show some of them.
  Never drop them without saying so.

## 5. Check the numbers

Before you answer:

- Reconcile: rows in = rows out + rows removed; the total before = the total after + what you
  removed.
- Get one key figure a second way (the group totals add up to the grand total; the SQL and the
  pandas result agree).
- Open every file you wrote and print its sheets, row counts and a few values.

If a check fails, find out why and fix it before you answer.

## 6. Hand back something people understand

- Workbook: a sheet per table, headers in plain words, numbers stored as numbers with a number
  format, the header row frozen, columns wide enough. With more than a few steps, a `Notes`
  sheet: the sources, what was done, what was left out.
- Chart: a title that says what it shows, labeled axes with units, bars sorted, one point per
  chart.
- Each `code.run` gets a `purpose`: one plain sentence on what it does.
- The answer: the result first, in a few lines with the key numbers; then what you assumed,
  what you left out and why; then the files you wrote. No code and no library names unless the
  person asks.
