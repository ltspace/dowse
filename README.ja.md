[English](README.md) | [简体中文](README.zh-CN.md) | 日本語 | [한국어](README.ko.md) | [Español](README.es.md) | [Italiano](README.it.md)

<p align="center">
  <img src="crates/dowse-app/src-tauri/icons/128x128@2x.png" width="96" height="96" alt="dowse ロゴ">
</p>

<h1 align="center">dowse</h1>

<p align="center">
  Windows 向けのオープンソース・ローカル全文検索。ファイル名、PDF / Office 文書、ソースコード、スクリーンショット内の文字を、ひとつのホットキーから検索できます。
</p>

<p align="center">
  <a href="https://lter.space/dowse/">公式サイト</a> ·
  <a href="https://github.com/ltspace/dowse/releases/latest">最新版をダウンロード</a>
</p>

![架空の Northstar ワークスペースを検索する Dowse。左側に検索結果、右側にプレビューを表示](docs/screenshots/hero.png)

## 特長

| | |
|---|---|
| 🔍 **ファイル名検索** | 入力と同時に高速検索 |
| 📄 **文書全文検索** | テキスト、Markdown、コード、PDF、Word、Excel、PowerPoint |
| 🖼️ **画像 OCR** | PNG / JPG / WebP / BMP 内の文字を Windows.Media.Ocr でオフライン認識 |
| 🈶 **中国語の単語分割** | jieba + BM25。trigram ではなく、GBK エンコーディングも自動検出 |
| ⚡ **増分インデックス** | 実行中のファイル監視と、起動時の差分照合 |
| 🤖 **MCP サーバー** | ローカル検索を stdio 経由で AI エージェントに提供 |
| 🚀 **NTFS 高速パス** | 管理者権限では MFT + USN Journal を利用し、利用できない場合は自動フォールバック |

データとインデックスは PC 内に保存されます。ネットワーク送信やテレメトリはありません。

## クイックスタート

[最新リリース](https://github.com/ltspace/dowse/releases/latest)から `dowse-app_*_x64-setup.exe` をダウンロードして実行し、`Alt+\`` で検索ウィンドウを呼び出します。

インストーラーは未署名です。Windows SmartScreen が表示された場合は、**詳細情報** → **実行**を選択してください。

ソースからビルドする場合：

```powershell
git clone https://github.com/ltspace/dowse && cd dowse

# CLI
cargo run -p dowse -- index D:\docs
cargo run -p dowse -- search keyword

# Tauri オーバーレイ
cd crates/dowse-app
npm install
cargo tauri build
```

## オーバーレイ操作

- `Alt+\``：表示 / 非表示
- `↑` / `↓`：結果を選択、ページ端では前後のページへ移動
- `Enter`：ファイルを開く
- `Ctrl+Enter`：エクスプローラーで表示
- `Ctrl+C`：パスをコピー
- `Esc`：非表示
- `Ctrl+P`：ファイル種類の絞り込み
- `Ctrl+S`：並べ替え
- `Ctrl+,`：設定

検索結果は 50 件ずつ表示されます。複数ページがある場合だけ、結果見出しに控えめな `‹ 1 / N ›` が現れます。

## MCP サーバー

`dowse mcp` はローカルインデックスを読み取るだけの MCP サーバーを stdio で起動します。

```text
claude mcp add --scope user dowse -- dowse mcp
```

`search`、`preview`、`index_status` の 3 ツールを提供します。`search` は `limit` / `offset` ページング、合計件数、拡張子フィルター、並べ替えに対応しています。

## 技術スタック

Rust · tantivy · jieba · Tauri 2 · Svelte 5 · Windows.Media.Ocr · Win32 (MFT / USN Journal)

## ライセンス

[MIT](LICENSE-MIT) または [Apache-2.0](LICENSE-APACHE) のデュアルライセンスです。
