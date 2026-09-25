# Mammoth

A fast, modular text editor for huge files, inspired by EmEditor and written in Rust.

- Opens a 100 GB text, CSV, JSON or log file instantly. Files over 1 GB open in **preview
  mode** showing the first 1,000 lines. Click **Load entire file** (or press **Ctrl+L**) to
  index the rest in the background with a live progress bar. You can pause it at any time.
- Find, Count and Extract always scan the **whole file**, even in preview mode. If a match
  is past the loaded part, Mammoth loads up to it and jumps there.
- Find / Replace with match case, whole word, regex (`$1` capture groups) and Replace All
  (undoable).
- **Modules**: pluggable detectors (emails, URLs, IPs, UUIDs, credit cards, phone numbers,
  dates, log levels, base64-encoded snowflake IDs, plus your own) that highlight matches.
  Each one also works as a search term, so you get Find next / Count / Extract to new tab /
  Mask (redact) for free.
- **Bookmarks**: toggle one on the current line (Ctrl+F2), jump between them with F2 /
  Shift+F2. They show as a dot in the gutter and survive closing and reopening the file.
- **Next / previous error** (F4 / Shift+F4): jumps between ERROR/FATAL lines using the
  built-in log-level detector, which also breaks logs down by level (Insights) so you can
  filter to just warnings, just errors, and so on.
- **Highlight several search terms at once** (Ctrl+F3 on a selection or the word under the
  cursor), each its own colour, with removable chips in the status bar.
- **Insights** (one click on a detector's chip in the status bar, the chart button in the
  title bar, or **Ctrl+Shift+E**): what a detector found, analysed automatically. For
  emails you get the total, unique addresses and domains, a provider split bar (Gmail,
  Outlook, Yahoo, iCloud, company domains…), and tabs for providers, domains and **top
  addresses** (duplicates at the top). Hover any row for **Filter** / Find / Extract.
  URLs group by site, IPs by subnet, cards by brand, dates by month.
- **Filter lines**: open every line that matches a search, a detector or a breakdown group
  in a new tab. The gutter keeps the original line numbers, and double-clicking one jumps
  back to it.
- **CSV table view** (Ctrl+Shift+T): a real grid with click-to-sort columns, per-column
  filters and in-place cell editing that only rewrites that one field (no Excel-style
  reformatting). Sorting and filtering cover the whole file; "Open view as tab" saves the
  result.
- **JSON inspector** (Ctrl+Shift+J): the current line's JSON, even inside a log line, as a
  collapsible tree or pretty text. It has a live **jq** filter box (click a key to get its
  path), can run jq over every line of a huge JSON Lines / log file, and can pretty-print or
  minify a document.
- **Heatmap** beside the scrollbar: see where each detector's hits (plus errors in logs,
  plus your current search) cluster across the whole file, even in preview mode. Hover for
  counts, click to jump. Files up to 256 MB are mapped automatically; bigger ones map on
  click, because that reads the whole file once.
- **Convert** (File → Convert): CSV / TSV ↔ JSON Lines / JSON array, streamed straight
  to disk, so a 50 GB CSV becomes JSON Lines without being loaded into memory. Numbers
  keep their exact text, `007` stays a string, and nested JSON can be flattened into
  `user.email` columns.
- Syntax colouring modules: rainbow CSV/TSV columns, JSON / JSON Lines, log levels.
- **Command palette** (Ctrl+Shift+P): fuzzy-search every action, e.g. "email extract",
  "font", "csv", "load".
- Custom frameless window with its own title bar: drag to move (Aero Snap works),
  double-click to maximise, resize from any edge.
- **Custom fonts**: uses Segoe UI / Cascadia Mono on Windows by default. Pick any
  installed font or any `.ttf` / `.otf` / `.ttc` file, separately for the interface and the
  editor, with a live preview. Line spacing is adjustable.
- Tabs, undo/redo, go to line (loads on demand), drag & drop, zoom, dark UI.

See [IDEAS.md](IDEAS.md) for what could come next.

## Build & run

```
cargo run --release -- path/to/file.log
```

The binary ends up in `target/release/mammoth.exe`. You can pass files on the command line,
drop them on the window, or use File → Open.

## How it stays fast

| What | How |
|---|---|
| Opening | Files above 64 MB are memory-mapped, so nothing is read up front. |
| Line index | A background thread counts newlines with SIMD and stores the offset of every 1024th line (~8 bytes per 1024 lines, so a 100 GB file needs only a few MB of index). |
| Scrolling | Only the visible lines are ever decoded or drawn. Scroll position is tracked as a line number, so it's exact even with billions of lines. |
| Editing | A line-based piece table: edits never copy the original file, and untouched lines just point back into the mapped bytes. You can edit while the file is still loading. |
| Saving | Streams pieces to a temp file, then swaps it in. Unchanged regions are copied byte-for-byte. |
| Searching | Regex over the raw mapped bytes in line-aligned 8 MB chunks, on a background thread with progress and cancel. |
| Disk I/O | A read-ahead thread prefetches upcoming chunks so disk and CPU work overlap. |

Measured on this machine (release build). "Cold" is a 21.5 GB file, bigger than RAM, so
everything comes from disk. "Warm" is an 8.6 GB file already cached in memory.

| Operation | Cold (21.5 GB, 363 M lines) | Warm (8.6 GB, 145 M lines) |
|---|---|---|
| Open + show first 1,000 lines | 2 ms | < 1 ms |
| Index the whole file ("Load entire file") | 16 s (1.33 GB/s) | 4 s (2.1 GB/s) |
| Find text at the very end, in preview mode | 14 s (1.57 GB/s) | — |
| Count every email address in the file | 27 s (0.81 GB/s) | 4.1 s (2.1 GB/s) |
| Jump to a random line | ~10 µs | ~10 µs |

Cold numbers are limited by the disk. A helper thread asks the OS to prefetch the next
chunks (`PrefetchVirtualMemory` on Windows, `madvise` elsewhere) so disk reads overlap with
scanning.

Run the benchmark yourself:

```
set MAMMOTH_BENCH_FILE=D:\big.log
cargo test --release bench_big_file -- --ignored --nocapture
```

## Keyboard shortcuts

| Keys | Action |
|---|---|
| Ctrl+O / Ctrl+N | Open / new |
| Ctrl+S / Ctrl+Shift+S | Save / save as |
| Ctrl+W, Ctrl+Tab | Close tab, next tab |
| **Ctrl+L** | Load entire file (preview mode) |
| Ctrl+F / Ctrl+H | Find / replace |
| F3 / Shift+F3 | Find next / previous |
| **Ctrl+F3** / Ctrl+Shift+F3 | Highlight word/selection (toggle) / clear all highlights |
| **Ctrl+F2** / F2 / Shift+F2 | Toggle bookmark / next / previous bookmark |
| **F4** / Shift+F4 | Next / previous error |
| Ctrl+G | Go to line (`line` or `line:col`) |
| Ctrl+Z / Ctrl+Y | Undo / redo |
| Ctrl+D, Ctrl+Shift+K | Duplicate line, delete line |
| Alt+Up / Alt+Down | Move line |
| Tab / Shift+Tab | Indent / outdent selection |
| Ctrl+wheel, Ctrl+= / Ctrl+- | Zoom |
| **Ctrl+Shift+P** | Command palette |
| Ctrl+Shift+M | Show / hide modules |
| **Ctrl+Shift+E** | Insights (email providers, domains, top addresses) |
| **Ctrl+Shift+T** | CSV table view |
| **Ctrl+Shift+J** | JSON inspector |
| Ctrl+, | Settings (fonts, spacing, preview size) |
| F1 | Shortcut list |

Preview size and threshold are in View → Settings.

## Using modules

Modules stay out of the way until you want them:

- The **modules button** in the title bar (the badge shows how many detectors are on) opens
  a drawer that floats over the editor. Esc or a click in the editor closes it. The pin
  button docks it to the side instead.
- Every active detector shows as a **chip in the status bar**. Click a chip for Find next,
  Count, Extract to new tab, Mask all, or to turn it off.
- Every module action is also in the **command palette**, e.g. type `email mask`.
- In the find bar, the "Text" dropdown can switch the search to any detector.
- **Click a detector's chip** (e.g. "Email addresses 6,000") to open its **Insights**:
  counts per group across the whole file. From any row, **Filter** opens all those lines in
  a new tab, **Find** jumps to the next one, and **Extract** lists the matches. Right-click
  a chip for the other actions.

## Table view filters

Type into the box under a column header:

| Filter | Meaning |
|---|---|
| `gmail` | contains (any case) |
| `!gmail` | doesn't contain |
| `=pro` | equals · `=` alone means empty · `!` alone means not empty |
| `>100` `>=100` `<100` `<=100` | numeric comparisons |
| `/^\d{3}-/` | regular expression |

Click a header to sort (ascending → descending → off). Double-click a cell, press Enter or
F2, or just start typing to edit it. Enter or Tab commits, Esc cancels, and Ctrl+Z undoes.

## Modules

### Option 1: a TOML file (no Rust needed)

Drop a `.toml` file into any of these folders, then press the reload button in the Modules
panel:

- `modules/` next to `mammoth.exe`
- `modules/` in the current working directory (this repo ships two examples there)
- `%APPDATA%\Mammoth\modules\` (the folder button in the Modules panel opens it)

```toml
name        = "JWT tokens"
description = "JSON Web Tokens"
pattern     = '''eyJ[A-Za-z0-9_-]{5,}\.eyJ[A-Za-z0-9_-]{5,}\.[A-Za-z0-9_-]{10,}'''
color       = "#e5c07b"
replacement = "[JWT]"       # optional: used by "Mask…"
enabled     = true          # optional: highlight by default
# case_insensitive = true   # optional
# group_label = "Status"    # optional: name a (?P<group>…) capture to get a Breakdown
```

Add a named capture called `group` and the module gets a **Breakdown** too. For example,
`pattern = 'HTTP/1\.[01]" (?P<group>\d{3})'` with `group_label = "Status"` counts
requests per HTTP status code.

`pattern` uses [Rust regex syntax](https://docs.rs/regex). Prefer `(?-u:\b)` over `\b` for
word boundaries: it keeps the regex engine on its fast path for multi-GB scans.

### Option 2: a Rust module

Implement `modules::Module` and add it to `builtin()` in `src/modules/mod.rs`. A detector
only needs a pattern. `validate` lets you reject false positives, like the Luhn check in the
credit card detector:

```rust
pub struct Ssn;

impl Module for Ssn {
    fn id(&self) -> &str { "us-ssn" }
    fn name(&self) -> &str { "US social security numbers" }
    fn kind(&self) -> ModuleKind { ModuleKind::Detector }
    fn color(&self) -> Color32 { Color32::from_rgb(0xff, 0x8c, 0x42) }
    fn pattern(&self) -> Option<String> { Some(r"(?-u:\b)\d{3}-\d{2}-\d{4}(?-u:\b)".into()) }
    fn validate(&self, s: &str) -> bool { !s.starts_with("000") && !s.starts_with("666") }
    fn replacement(&self) -> Option<String> { Some("[SSN]".into()) }
    // Optional breakdown: group hits, e.g. by area number.
    fn group_label(&self) -> Option<String> { Some("Area".into()) }
    fn group_key(&self, s: &str, _: Option<&str>) -> Option<String> { s.get(..3).map(Into::into) }
}
```

Syntax modules implement `highlight(line, out)` instead and push coloured byte spans. See
`src/modules/syntax.rs` for CSV, JSON and log examples.

## Limitations

- Text is treated as UTF-8 (invalid bytes show as `�`). UTF-16 files are not decoded.
- No word wrap. Lines longer than 64 KB are shown as read-only segments (marked in the
  gutter) so a single gigantic line can't freeze the UI.
- Saving over a file that is memory-mapped (> 64 MB) reloads it afterwards, which clears
  undo history for that tab. Smaller files keep their undo history across saves.
- Replace All, Filter, table sorting/filtering and "run jq on file" need the whole file
  indexed, so they load the rest of the file first if you're in preview mode.
- Table view treats each line as one row: quoted fields that contain line breaks aren't
  supported.
- The JSON tree and pretty-print work on documents up to 64 MB. Bigger files work through
  "Run jq on file", which streams line by line.

## Project layout

```
build.rs               embeds the generated icon into the .exe
src/
  main.rs              entry point, frameless window setup
  app/mod.rs           tabs, find bar, status bar, background jobs
  app/titlebar.rs      custom title bar, window controls, edge resizing
  app/commands.rs      every action (menus, shortcuts and palette share it)
  app/palette.rs       command palette
  app/modules_ui.rs    modules drawer + status-bar chips
  app/settings_ui.rs   settings window + font picker
  app/breakdown_ui.rs  insights panel (e.g. emails by provider / domain / address)
  app/table_ui.rs      table-view plumbing (debounced sort/filter jobs, export)
  app/json_ui.rs       JSON inspector, jq on the whole file, pretty-print / minify
  app/heatmap_ui.rs    heatmap strip beside the scrollbar
  app/convert_ui.rs    convert dialog
  convert.rs           streaming CSV / TSV ↔ JSON Lines / JSON array conversion
  table.rs             the CSV grid widget + whole-file sort/filter
  csv.rs               lossless CSV field parsing and editing
  json_tools.rs        find JSON in a line, pretty-print, jq (via jaq)
  fonts.rs             installed-font scanning, custom font loading
  logo.rs              the logo, rasterised in code (also used for the .ico)
  editor.rs            the virtualised editor widget (render, mouse, keyboard)
  search.rs            find / count / extract / replace-all over huge files
  document/source.rs   memory-mapped file + background sparse line index
  document/mod.rs      piece table, undo/redo, streaming save
  modules/             module trait, detectors (+ email providers), syntaxes, TOML loader
  icons.rs, theme.rs   vector icons, colours
modules/               example user modules (TOML)
```

Run the tests with `cargo test`. They cover indexing (CRLF, huge lines, preview), edit/save
round-trips (including saving over a memory-mapped file), search/replace, and keyboard
flows driven through the real UI (find, palette, modules drawer).

To see the UI without opening a window, this renders screenshots offscreen:

```
set MAMMOTH_PREVIEW_DIR=C:\temp\shots
cargo test render_previews -- --ignored
```
