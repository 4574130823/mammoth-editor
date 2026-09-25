# Ideas for Mammoth

## Done

- ✅ **Filtered view.** Show only lines that match a search, a detector or a breakdown
  group; line numbers map back to the source. This was the most-requested idea.
- ✅ **CSV table mode.** Sort, per-column filters, edit a cell without reformatting
  anything else, export the view.
- ✅ **JSON:** pretty-print JSON inside log lines, a collapsible tree, and jq (live filter,
  plus running it over the whole file).
- ✅ **Breakdowns.** Group detector hits and count them (emails by provider / domain, URLs by
  site, IPs by subnet…).
- ✅ **Heatmap** beside the scrollbar for detectors, errors and search hits.
- ✅ **Convert** between CSV / TSV / JSON Lines / JSON array, streamed.
- ✅ **Bookmarks that survive a reload.** Ctrl+F2 to toggle, F2 / Shift+F2 to jump; stored
  per file path in settings.
- ✅ **Next / previous error, filter by log level.** A built-in "Log levels" detector
  (ERROR/WARN/INFO/DEBUG/TRACE/FATAL) plus F4 / Shift+F4, with a Severity breakdown.
- ✅ **Highlight several search terms at once**, each in its own colour (Ctrl+F3), with
  removable chips in the status bar.

## Requested by users

These come from Hacker News, GitHub issues (klogg, VS Code), the Notepad++ forum and the
EmEditor forum. Reddit blocks automated access, so no Reddit threads were read.

1. **Merge several logs into one timeline** by timestamp.
   [HN](https://news.ycombinator.com/item?id=39317580) · [HN](https://news.ycombinator.com/item?id=40703892)
2. **Jump to a time / extract a time range** by binary-searching a time-sorted file, so
   "go to 14:32" is instant even at 100 GB. [HN](https://news.ycombinator.com/item?id=39317580)
3. **Multi-line entries.** Treat a stack trace as one record.
   [klogg #333](https://github.com/variar/klogg/issues/333)
4. **SQL over CSV and logs** (SQLite / DuckDB-style queries on the open file).
   [HN](https://news.ycombinator.com/item?id=14919149)
5. **Find malformed CSV rows** (wrong column count, broken quotes).
   [HN](https://news.ycombinator.com/item?id=14919149)
6. **Shared module gallery.** Download ready-made detectors from inside the app.
   [klogg #509](https://github.com/variar/klogg/issues/509)
7. **Search across all open tabs**, with results in one list.
    [klogg #41](https://github.com/variar/klogg/issues/41)
8. **Open `.gz` / `.zip` directly.** [HN](https://news.ycombinator.com/item?id=40703892)
9. **Fast de-duplicate / unique lines with counts** (`sort | uniq -c` for huge files).
    [Notepad++ forum](https://community.notepad-plus-plus.org/topic/15315/wanted-function-remove-duplicated-lines)
10. **Render ANSI colours** in logs. [klogg #338](https://github.com/variar/klogg/issues/338)

## More ideas (suggestions, not sourced)

These are my own suggestions, chosen because they build on what Mammoth already does.

**Data & breakdowns**
- **Column breakdown in table view.** Right-click a header to see "country: US 1,204 ·
  UK 873 …" in the same window as the email breakdown, with Filter per value.
- **Duplicate finder.** "Which emails appear more than once, and on which lines?" This is a
  breakdown filtered to count > 1.
- **Column statistics.** Min / max / mean / empty count per CSV column, computed over the
  whole file.
- **Random sample.** Pull 10,000 random lines from a 100 GB file into a tab to explore
  quickly.
- **Split a file** into N-sized chunks, or one file per column value (e.g. one file per
  email provider).

**Privacy & cleanup**
- **Sanitised copy.** One click masks every enabled detector (emails, cards, IPs…) and
  streams a clean copy to disk. Useful before sharing logs.
- **Disposable / role address flags.** Mark `noreply@`, `admin@` and throwaway domains in
  the email breakdown.

**Big-log navigation**
- **Group similar log lines.** Collapse millions of lines into a few hundred templates
  ("user * logged in from *" × 2,381,004) to spot the rare, weird ones.
- **Split view.** The same file at two positions side by side.

**Editing**
- **Column (box) selection and multi-cursor editing**, a classic EmEditor feature.
- **Macro recording** for repetitive edits.
- **"Make a detector from this."** Select an example (e.g. `ORD-12345`) and Mammoth
  suggests a regex and saves it as a module.

**Platform**
- **Encodings.** UTF-16, Latin-1 and Shift-JIS detection and conversion.
- **Session restore.** Reopen tabs, scroll positions, filters and table sorts on launch.
- **Light theme / custom colour themes.**
- **Scriptable modules** (Lua / Rhai / WASM) for detectors that need logic beyond a regex.

## Even more ideas (suggestions)

**Working with email lists**
- **Join two files by a column.** Match `customers.csv` with `orders.csv` on email, or list
  "emails in A but not in B". Streamed, VLOOKUP-style.
- **Email typo fixer.** Flag `gmial.com`, `yaho.com`, `hotmial.com` and suggest the fix, as a
  new group in the email breakdown.
- **Pseudonymise instead of mask.** Replace each email with a consistent fake (the same input
  always gives the same fake), so shared data stays joinable but private.
- **Compare two breakdowns.** Providers this month vs last month, with the differences
  highlighted.

**Tables & data quality**
- **Pivot / group-by with totals.** Revenue per country, average spend per plan: sum, avg or
  count of one column grouped by another.
- **Quick charts.** A bar chart of any breakdown, and events over time from a timestamp
  column.
- **Schema check.** Declare per-column rules (number, date, email, required) and highlight
  the rows that break them.
- **Bulk cleanup.** Trim spaces, lowercase emails, normalise phone numbers and dates, and
  remove duplicate rows by column.
- **Export to Excel (.xlsx)** from the table view, one sheet per filter.

**Big files**
- **Diff two huge files** side by side, line-hash based so it works on 100 GB.
- **Open a folder as one file.** Rotated logs (`app.log`, `app.log.1`, `app.log.2.gz`…) read
  as one continuous timeline.
- **Invisible-character inspector.** Reveal and fix zero-width spaces, non-breaking spaces,
  stray BOMs, mixed line endings and broken UTF-8.
- **Timeline strip.** Like the heatmap, but by timestamp instead of position: events per
  hour, click to jump.

**Tools**
- **Regex playground.** Test a pattern live against the current line with capture groups
  shown, then "Save as module".
- **Command-line mode.** `mammoth count --module email big.log` or
  `mammoth convert a.csv b.jsonl`, running the same engine in scripts.
- **Ask Claude from the palette.** "Explain this stack trace", "write a jq filter that…",
  "make a detector for order IDs".
- **Findings report.** Export the counts, breakdowns and bookmarks from a session as one
  shareable HTML page.
- **Remote files.** Open over SSH / SFTP or HTTP range requests without downloading the whole
  file first.
