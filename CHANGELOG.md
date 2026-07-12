# Changelog

All notable changes to dowse are recorded here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.0.0/), and the project adheres
to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.6.1] - 2026-07-10

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

## [0.6.0] - 2026-07-09

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

## [0.5.0] - 2026-07-04

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

## [0.4.2] - 2026-07-01

### Added

- MCP server. `dowse mcp` starts a read-only server over stdio that exposes three
  tools — search, preview, and index_status — to AI agents.

### Fixed

- Fixed a duplicate window frame, a non-responsive transparency knob, and a
  path-prefix leak in result paths.

## [0.4.1] - 2026-06-29

### Added

- Office document extraction: DOCX, XLSX, and PPTX contents are indexed and
  searchable.

### Changed

- The transparency control was consolidated into a single entry point with a
  three-step knob.

### Performance

- Phrase-query latency dropped by an order of magnitude by bounding the
  snippet-generation scan window.

## [0.4.0] - 2026-06-28

### Changed

- The interface was rebuilt around the Raycast layout language — an input bar,
  result list, preview pane, and bottom action bar — with reworked spacing,
  corner radii, and type scale. Result rows gained a right-aligned type hint and
  a solid-block selection state. Three fixes came from real-use feedback: the
  window grew from 720×480 to 750×500; the dark-glass material was made genuinely
  translucent instead of a muddy gray; and the row-title weight and placeholder
  opacity were reduced. dowse keeps its own icon, aqua accent, and Inter + MiSans
  font stack; none of Raycast's logo, icons, or brand color are used.

## [0.3.0] - 2026-06-27

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

## [0.2.1] - 2026-06-20

### Changed

- Overlay transparency and corner radius were tuned toward the system-native
  Acrylic material, and the Inter + MiSans font stack was adopted.
- Result-list and preview fonts were adjusted so line height and size align with
  system conventions in mixed Chinese and English text.

## [0.2.0] - 2026-06-19

### Added

- CLI indexing and search: Chinese word segmentation (jieba), automatic GBK
  encoding detection, BM25 ranking, and search-result highlighting. Multi-term
  queries default to AND semantics.
- Overlay prototype: a global hotkey (Alt+`), an Acrylic-material window, and
  full keyboard navigation (↑↓ / Enter / Ctrl+Enter / Ctrl+C / Esc).

### Fixed

- Fixed a slice panic caused by overlapping jieba segments in highlight ranges.
- The index root directory is no longer skipped by exclusion rules.

[Unreleased]: https://github.com/ltspace/dowse/compare/v0.6.1...HEAD
[0.6.1]: https://github.com/ltspace/dowse/compare/v0.6.0...v0.6.1
[0.6.0]: https://github.com/ltspace/dowse/compare/v0.5.0...v0.6.0
[0.5.0]: https://github.com/ltspace/dowse/compare/v0.4.2...v0.5.0
[0.4.2]: https://github.com/ltspace/dowse/compare/v0.4.1...v0.4.2
[0.4.1]: https://github.com/ltspace/dowse/compare/v0.4.0...v0.4.1
[0.4.0]: https://github.com/ltspace/dowse/compare/v0.3.0...v0.4.0
[0.3.0]: https://github.com/ltspace/dowse/compare/v0.2.1...v0.3.0
[0.2.1]: https://github.com/ltspace/dowse/compare/v0.2.0...v0.2.1
[0.2.0]: https://github.com/ltspace/dowse/releases/tag/v0.2.0
