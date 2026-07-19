# Changelog

All notable changes to dowse are recorded here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.0.0/), and the project adheres
to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- Index rules are now configurable instead of hardcoded, persisted as a
  `-rules.json` file next to the index: excluded directory names, extra
  plain-text extensions, and the per-file size cap (default unchanged: skip
  node_modules/target/.git/.venv/__pycache__, 20MB cap). All three indexing
  paths honor them — full rebuild, live file-watch, and startup
  reconciliation — and the NTFS MFT fast path now applies directory
  exclusions, which it previously skipped. New `dowse rules show` and
  `dowse rules set --exclude a,b --add-ext rst,adoc --max-file-mb 50`
  commands; `dowse status` prints the active rules, and rebuild reports call
  out how many files were skipped for exceeding the size cap.
- The MCP `search` tool is now on par with the CLI: `sort`
  (relevance / mtime / size), comma-separated multi-extension `ext`, and
  `offset` pagination with a `total_hits` count in the response.
  Non-relevance sorts omit the meaningless BM25 score, matching CLI
  behavior. `index_status` also reports the active index rules.
- The overlay remembers recent searches: a query is recorded when a result
  is actually opened (not on every keystroke), the last 10 are kept locally,
  and an empty input shows them — ↑↓/Enter to reuse, Delete to remove one,
  plus a clear-all action. Fully keyboard-driven and bilingual.
- `dowse add <dir>` incrementally indexes an additional root into the
  existing index — only the new directory is scanned and upserted, documents
  from other roots stay untouched (unlike `dowse index`, which rebuilds from
  scratch). The incremental path uses the same NTFS MFT fast enumeration and
  honors the same index rules as a full rebuild, and re-scanning an
  already-registered root is idempotent.
- The overlay gained an index-rules panel (`Ctrl+,`): view and edit excluded
  directories, extra text extensions, and the per-file size cap without
  touching the CLI, with a rebuild reminder and a one-click rebuild for
  single-root setups. Rebuild completion reports now call out how many files
  were skipped for exceeding the size cap.
- The overlay now measures its two core latency targets — hotkey-to-visible
  and keystroke-to-results-rendered — and writes them to the rotating log
  (`perf` lines), so the README's design targets are finally measurable on
  real usage rather than "not measured".

## [0.8.3] - 2026-07-14

### Added

- The Windows installer now bundles the `dowse` CLI as a Tauri sidecar and
  adds the install directory to the user PATH, so `dowse` — and
  `claude mcp add dowse -- dowse mcp` — work out of the box for GUI installs.
  The uninstaller removes the PATH entry. (#13)
- MCP tools now carry `title` and `readOnlyHint` annotations, as required by
  the Claude connectors directory and used by MCP clients for auto-permission
  decisions.
- A privacy policy (PRIVACY.md), an MCPB desktop-extension package for Claude
  Desktop (`mcpb/`), and a landing page at <https://lter.space/dowse/>.

### Changed

- dowse now has a Chinese name: 问渠 ("ask the channel", from Zhu Xi's poem
  that also explains the index: clear water comes from a living source).
- The MSI bundle is paused for this release: with the bundled CLI it exceeds
  the 15MB installer budget (16.96MB) and WiX does not run the NSIS PATH
  hooks, which would leave MSI installs with an inconsistent `dowse` command
  experience. NSIS setup.exe (14.31MB, within budget) is the sole installer
  until proper WiX Environment support lands.

## [0.8.2] - 2026-07-13

### Changed

- Crate README now carries the MCP Registry ownership marker (mcp-name)
  required for listing on the official registry.

## [0.8.1] - 2026-07-13

### Fixed

- Fixed a docs.rs build failure: the crate pinned a Windows documentation
  target, which docs.rs's Linux build machine cannot cross-compile a C
  dependency for. Documentation now builds with the default Linux target;
  the Windows-only API surface is visible in a local build instead.

## [0.8.0] - 2026-07-13

### Added

- `dowse status` reports the index location, document count, on-disk size,
  root directory, and last-updated time. `dowse search` gained `--ext`
  (comma-separated extension filter) and `--sort` (relevance / mtime / size);
  non-relevance sort orders hide the otherwise meaningless BM25 score, and an
  empty query now returns a clear error instead of an empty result set.

### Changed

- The `dowse-core` and `dowse-cli` crates are merged into a single `dowse`
  package that is both the search library and the command-line tool. The CLI is
  installable with `cargo install dowse`; consumers that want only the search
  engine can depend on the crate with `default-features = false` to leave out
  the CLI and its dependencies.
- Rustdoc coverage was completed across the crate: the crate root and public
  items are documented, several with runnable examples, and `missing_docs` is
  now enforced as a lint.
- CI gained a dependency security audit and a non-Windows compile check.

### Fixed

- Malformed-file panic protection, previously scoped to PDF extraction only,
  now wraps every format the extractor handles. A panic while extracting any
  one file is caught and downgraded to "no text" for that file, rather than
  risking a poisoned shared index-updater lock that could stall the watch and
  OCR pipelines.

## [0.7.0] - 2026-07-08

### Added

- Interface text follows the system language in Chinese or English. The overlay
  dropdowns, search placeholder, result count, empty-state guidance, preview
  hints, pin tooltip, shortcut bar, and shortcut overlay, together with the tray
  tooltip, the tray and right-click menus, and the folder-selection dialog, are
  drawn from a dictionary. A Chinese system UI shows Chinese, everything else
  shows English. The choice is made once at startup; there is no runtime switch.

### Changed

- Text is now tokenized by script: CJK runs go to jieba, Latin/digit runs split
  on alphanumeric boundaries, and every token is lowercased. Searches are now
  case-insensitive (`api` matches `API`), and hyphenated or mixed terms are
  found by their parts (`covid`, `19`, and `covid-19` all match `covid-19`).
  Token positions are sequential, which makes quoted phrase queries more
  accurate. Chinese segmentation is unchanged.
- The index schema is now version 4. Existing indexes must be rebuilt once
  after upgrading; opening an older index reports a clear error asking for a
  rebuild.

### Fixed

- Fixed a bug where incremental indexing could stop permanently after the index
  writer collided with a real-time anti-virus scan. Reopening the writer hit a
  lock-ordering deadlock and never recovered, so files renamed or created after
  the collision were not indexed until the next full rebuild. Reopening now
  reuses the existing write lock and recovers cleanly.

## [0.6.1] - 2026-06-25

### Added

- Indexing progress is now shown live. During the text phase a running counter
  and the current file name are displayed (long paths elided in the middle, no
  progress bar or percentage). During the OCR backfill pass a single progress bar
  reports images recognized out of the known total, without covering search
  results. Hiding and re-summoning the window resumes from a fresh snapshot
  rather than a blank or stale state, and the tray tooltip mirrors the same
  status.
- The tray menu shows the active index folder and its document count, plus a
  "Change index folder…" action that triggers a full rebuild; the overlay empty
  state shows the current index root and offers a change-folder entry.
- Process stdout and stderr are redirected to a size-rotating log file under
  `%LOCALAPPDATA%\dowse\logs`, backed by a panic hook that records the crashing
  thread, location, and message. Release GUI builds previously had no console and
  lost all diagnostics silently.

### Changed

- OCR results are written back to the index in batches (32 images or a 5-second
  window, whichever comes first) instead of one commit per image. The
  anti-virus-contention retry-and-backoff that previously guarded only full
  rebuilds now also covers incremental updates and OCR write-back.
- Remote Desktop sessions are no longer unconditionally downgraded to a
  solid-color material. Recent Windows RDP pipelines usually render Acrylic and
  Mica correctly; when they do not, the tray transparency toggle remains
  available as a fallback.

### Fixed

- Fixed an I/O storm during large image-corpus indexing, where committing after
  every recognized image was the root cause of intermittent crashes,
  multi-second window-summon lag, a frozen background-OCR counter, and searches
  returning nothing while indexing was in progress. Under a stress retest of
  roughly 5,100 images plus over ten thousand text documents, indexing completed
  with zero crashes, every image recognized, and search available throughout.

## [0.6.0] - 2026-06-22

### Added

- NTFS fast path. On NTFS volumes with administrator rights, the initial index is
  built by enumerating the Master File Table, real-time monitoring is driven by
  the USN Journal instead of file-system-event watching, and startup
  reconciliation replays from a persisted journal cursor. Without a volume handle
  (non-administrator or non-NTFS) the tool falls back silently to the directory
  walk plus file-watch path; both paths produce identical results, and one
  machine can serve one drive over the fast path and another over the slow path.
- A `Ctrl+/` overlay lists the keyboard shortcuts.
- The footer reports the elapsed time of the current search, and the result count
  animates when it changes.

### Changed

- Indexing streams its progress instead of blocking with no feedback until it
  finishes.

### Fixed

- Fixed a durability bug where a reversed OCR-queue persistence order could
  permanently lose recognized image text.
- Hardened index-directory deletion on Windows with a handle-release
  retry-and-backoff; the missing backoff was also the root cause of several
  intermittent integration-test failures.
- Fixed a crash when a full rebuild collided with real-time anti-virus scanning,
  using whole-run retry plus dynamic concurrency reduction.
- Several robustness fixes: the OCR queue no longer accumulates stale historical
  entries or builds duplicate queues for the same directory; a failed fast-path
  startup now rejoins the slow path for the current run instead of silently
  stalling; and large-directory walks were moved off the file-watch callback
  thread.

## [0.5.0] - 2026-06-08

### Added

- OCR pipeline. Text inside screenshots and images is recognized and indexed,
  using the Windows-native OCR engine (Windows.Media.Ocr), fully offline, on a
  low-priority queue served by a worker pool. Scope is PNG/JPG/JPEG/WebP/BMP up
  to 20MB. The queue is persisted, so restarting mid-pass does not re-recognize
  already-processed images. Each image is indexed in two forms — the raw OCR
  output and a CJK-space-collapsed variant — to hedge against segmentation gaps.
  Without an OCR language pack installed, the pipeline disables itself and logs a
  single line instead of crashing. The preview pane now renders the source image
  alongside the matched OCR text.
- Type and sort controls in the overlay: a type filter (All / Documents / Code /
  Images, `Ctrl+P`) and a sort order (relevance / newest / oldest / largest,
  `Ctrl+S`), presented as two ghost-style dropdowns that stay near-invisible
  until a non-default value is chosen; changing either re-runs the search.
- A native Windows context menu on result rows: open, open containing folder,
  copy full path, and copy file name.
- A pin toggle on the input bar that keeps the overlay open when it loses focus.
  The state is session-only and not persisted; Esc and the global hotkey still
  hide the window.

### Changed

- The index schema was upgraded to v3: mtime and size gained FAST attributes
  (a prerequisite for sorting), and a new `kind` field distinguishes text
  documents from OCR'd images. **The index must be rebuilt after upgrading.** An
  old index is detected and you are guided to rebuild it rather than kept
  silently compatible.

## [0.4.2] - 2026-05-27

### Added

- MCP server. `dowse mcp` starts a read-only server over stdio that exposes three
  tools — search, preview, and index_status — to AI agents.

### Fixed

- Fixed a duplicate window frame, a non-responsive transparency knob, and a
  path-prefix leak in result paths.

## [0.4.1] - 2026-05-24

### Added

- Office document extraction: DOCX, XLSX, and PPTX contents are indexed and
  searchable.

### Changed

- The transparency control was consolidated into a single entry point with a
  three-step knob.

### Performance

- Phrase-query latency dropped by an order of magnitude by bounding the
  snippet-generation scan window.

## [0.4.0] - 2026-05-20

### Changed

- The interface was rebuilt around the Raycast layout language — an input bar,
  result list, preview pane, and bottom action bar — with reworked spacing,
  corner radii, and type scale. Result rows gained a right-aligned type hint and
  a solid-block selection state. Three fixes came from real-use feedback: the
  window grew from 720×480 to 750×500; the dark-glass material was made genuinely
  translucent instead of a muddy gray; and the row-title weight and placeholder
  opacity were reduced. dowse keeps its own icon, aqua accent, and Inter + MiSans
  font stack; none of Raycast's logo, icons, or brand color are used.

## [0.3.0] - 2026-05-17

### Added

- Incremental indexing. File-system events drive incremental updates while the
  app is running, and an mtime/size comparison reconciles changes made while it
  was not. The same mechanism backs both the resident app and a `dowse watch`
  command.
- Visual polish: a monospace font, aqua highlighting, a floating scrollbar, a
  summon cursor animation, and system-associated file-type icons in result rows
  and the preview pane.
- A finalized application icon and a tray silhouette that switches between light
  and dark with the taskbar theme.
- Public-release scaffolding: a bilingual README, dual MIT / Apache-2.0
  licensing, contribution and security guides, and continuous integration.

### Changed

- Empty-state copy was rewritten as terse declarative text.

## [0.2.1] - 2026-05-02

### Changed

- Overlay transparency and corner radius were tuned toward the system-native
  Acrylic material, and the Inter + MiSans font stack was adopted.
- Result-list and preview fonts were adjusted so line height and size align with
  system conventions in mixed Chinese and English text.

## [0.2.0] - 2026-04-30

### Added

- CLI indexing and search: Chinese word segmentation (jieba), automatic GBK
  encoding detection, BM25 ranking, and search-result highlighting. Multi-term
  queries default to AND semantics.
- Overlay prototype: a global hotkey (Alt+`), an Acrylic-material window, and
  full keyboard navigation (↑↓ / Enter / Ctrl+Enter / Ctrl+C / Esc).

### Fixed

- Fixed a slice panic caused by overlapping jieba segments in highlight ranges.
- The index root directory is no longer skipped by exclusion rules.

[Unreleased]: https://github.com/ltspace/dowse/compare/v0.8.2...HEAD
[0.8.2]: https://github.com/ltspace/dowse/compare/v0.8.1...v0.8.2
[0.8.1]: https://github.com/ltspace/dowse/compare/v0.8.0...v0.8.1
[0.8.0]: https://github.com/ltspace/dowse/compare/v0.7.0...v0.8.0
[0.7.0]: https://github.com/ltspace/dowse/compare/v0.6.1...v0.7.0
[0.6.1]: https://github.com/ltspace/dowse/compare/v0.6.0...v0.6.1
[0.6.0]: https://github.com/ltspace/dowse/compare/v0.5.0...v0.6.0
[0.5.0]: https://github.com/ltspace/dowse/compare/v0.4.2...v0.5.0
[0.4.2]: https://github.com/ltspace/dowse/compare/v0.4.1...v0.4.2
[0.4.1]: https://github.com/ltspace/dowse/compare/v0.4.0...v0.4.1
[0.4.0]: https://github.com/ltspace/dowse/compare/v0.3.0...v0.4.0
[0.3.0]: https://github.com/ltspace/dowse/compare/v0.2.1...v0.3.0
[0.2.1]: https://github.com/ltspace/dowse/compare/v0.2.0...v0.2.1
[0.2.0]: https://github.com/ltspace/dowse/releases/tag/v0.2.0
