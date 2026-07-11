English | [简体中文](README.zh-CN.md)

<!-- [![License](https://img.shields.io/badge/license-MIT%2FApache--2.0-blue.svg)](#license) -->
<!-- CI badge: enable once .github/workflows/ci.yml has run on the default branch -->
<!-- [![CI](https://github.com/ltspace/dowse/actions/workflows/ci.yml/badge.svg)](https://github.com/ltspace/dowse/actions/workflows/ci.yml) -->

# dowse

Native full-text search for Windows. Indexes file names, document contents, and text embedded in screenshots. Summoned by a hotkey, returns results in milliseconds.

The name comes from a dowsing rod.

<!-- [Image slot 1: overlay in action GIF — summon, type, results appear, jump to folder] -->

## Motivation

No Windows tool satisfies all three of the following at once:

- Search file contents, not just file names (Everything only does the latter)
- Recognize and index text inside images (macOS Spotlight has this; there is no equivalent on Windows)
- One hotkey to summon, full keyboard operation, no perceptible latency

The closest open-source implementation is sist2, but it targets Linux — on Windows it only runs via Docker — treats Chinese text as trigrams, and the project is no longer maintained. dowse is a Windows-native implementation built around these three points.

## Chinese text handling

- Word segmentation via jieba, ranking via BM25 (tantivy engine). No trigrams.
- Automatic file encoding detection (chardetng). GBK-encoded files are decoded correctly before indexing — this matters because a large share of Chinese-language documents on Windows, especially older ones, are still saved in GBK rather than UTF-8, and a search tool that assumes UTF-8 silently mis-indexes or garbles them.
- Multi-term queries default to AND semantics. Quoted phrase queries match on exact position.
- OCR runs on the Windows-native engine (Windows.Media.Ocr), fully offline. The zh-Hans language pack also covers mixed Chinese/English text, no extra configuration required.

## Performance

Design targets; exceeding them is treated as a defect:

| Metric | Target | Note |
|---|---|---|
| Hotkey to window visible | < 50ms | Process stays resident; summon is show + focus |
| Keystroke to results rendered | < 80ms | UI and index share a process, no IPC |
| OCR | ~112ms / 1080p screenshot | Measured median, processed on a background thread pool |
| Resident memory | < 150MB | Includes index reader |
| Installer size | < 15MB | Tauri, not Electron |
| File name index build (planned) | seconds | Direct NTFS MFT read, same approach as Everything |

## Usage

```powershell
git clone https://github.com/ltspace/dowse && cd dowse

cargo run -p dowse -- index D:\docs      # build the index
cargo run -p dowse -- search 限流         # search
cargo run -p dowse -- search "精确短语"   # phrase query
```

Overlay app (released; current v0.2.1, v0.3.0 coming soon): `Alt+\`` to summon, `↑↓` to select, `Enter` to open, `Ctrl+Enter` to reveal in Explorer, `Ctrl+C` to copy path, `Esc` to hide.

<!-- [Image slot 2: light/dark theme + preview pane screenshot] -->

## Architecture

```
                 ┌─────────────────────────────────────────┐
                 │              dowse-core                  │
                 │  tantivy index · jieba segmentation ·     │
                 │  encoding detection · text extraction     │
                 │  (txt/md/pdf/code/docx/xlsx/pptx) ·       │
                 │  OCR pipeline*                            │
                 └──────┬──────────┬──────────┬────────────┘
                        │          │          │
                 ┌──────┴───┐ ┌────┴─────┐ ┌──┴───────────┐
                 │ dowse-app │ │ dowse-cli │ │ MCP server   │
                 └──────────┘ └──────────┘ └──────────────┘
```

One index core, three consumers. dowse-app is a Tauri 2 + Svelte 5 resident overlay; the CLI is for scripting and debugging; the MCP server exposes the local index to AI agents.

Index updates run on a two-tier scheme: while running, file system events drive incremental updates (500ms debounce window, batched commits); at startup, an mtime/size comparison reconciles changes made while the app was not running.

## Roadmap

| # | Scope | Status |
|---|---|---|
| 1 | CLI indexing and search: Chinese segmentation, GBK detection, highlighting | Done |
| 2 | Overlay: global hotkey, Acrylic material, keyboard navigation | In progress |
| 3 | Incremental indexing: file watching, startup reconciliation | Done |
| 4 | OCR pipeline: screenshot text into the index | Done |
| 5 | MCP server | Done |
| 6 | NTFS MFT / USN Journal fast path | Planned |

## Stack

Rust · [tantivy](https://github.com/quickwit-oss/tantivy) · jieba · Tauri 2 · Svelte 5 · Windows.Media.Ocr · notify

## Design docs

- [docs/DESIGN-M2-浮窗.md](docs/DESIGN-M2-浮窗.md) (overlay design, Chinese)
- [docs/DESIGN-M3-增量索引.md](docs/DESIGN-M3-增量索引.md) (incremental indexing design, Chinese)
- [docs/DESIGN-M4-OCR管线.md](docs/DESIGN-M4-OCR管线.md) (OCR pipeline design, Chinese)
- [docs/DESIGN-M5-MCP.md](docs/DESIGN-M5-MCP.md) (MCP server design, Chinese)

## Privacy

The index is stored locally (`%LOCALAPPDATA%\dowse`). No network access, no telemetry.

## License

Dual-licensed under [MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE), at your option.
