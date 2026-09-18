[English](README.md) | [简体中文](README.zh-CN.md) | [日本語](README.ja.md) | [한국어](README.ko.md) | [Español](README.es.md) | Italiano

<p align="center">
  <img src="crates/dowse-app/src-tauri/icons/128x128@2x.png" width="96" height="96" alt="Logo di dowse">
</p>

<h1 align="center">dowse</h1>

<p align="center">
  Motore di ricerca locale open source per Windows. Trova nomi di file, contenuti PDF e Office, codice sorgente e testo nelle schermate con una sola scorciatoia.
</p>

<p align="center">
  <a href="https://lter.space/dowse/">Sito web</a> ·
  <a href="https://github.com/ltspace/dowse/releases/latest">Scarica l'ultima versione</a>
</p>

![Dowse cerca nell'area di lavoro fittizia Northstar, con i risultati a sinistra e l'anteprima a destra](docs/screenshots/hero.png)

## Funzionalità

| | |
|---|---|
| 🔍 **Ricerca per nome** | Risultati immediati durante la digitazione |
| 📄 **Ricerca nel contenuto** | Testo, Markdown, codice, PDF, Word, Excel e PowerPoint |
| 🖼️ **OCR delle immagini** | Testo in PNG / JPG / WebP / BMP tramite Windows.Media.Ocr, completamente offline |
| 🈶 **Segmentazione del cinese** | jieba + BM25 invece dei trigrammi, con rilevamento automatico di GBK |
| ⚡ **Indicizzazione incrementale** | Monitoraggio dei file durante l'esecuzione e riconciliazione all'avvio |
| 🤖 **Server MCP** | Espone la ricerca locale agli agenti IA tramite stdio |
| 🚀 **Percorso rapido NTFS** | MFT + USN Journal con privilegi di amministratore e fallback automatico |

Dati e indice restano sul PC. Non ci sono telemetria né traffico di rete.

## Avvio rapido

Scarica `dowse-app_*_x64-setup.exe` dall'[ultima versione](https://github.com/ltspace/dowse/releases/latest), installalo e premi `Alt+\`` per aprire la ricerca.

Il programma di installazione non è firmato. Se Windows SmartScreen mostra un avviso, scegli **Ulteriori informazioni** e poi **Esegui comunque**.

Compilazione dai sorgenti:

```powershell
git clone https://github.com/ltspace/dowse && cd dowse

# CLI
cargo run -p dowse -- index D:\docs
cargo run -p dowse -- search parola

# Interfaccia Tauri
cd crates/dowse-app
npm install
cargo tauri build
```

## Comandi dell'interfaccia

- `Alt+\``: mostra o nasconde
- `↑` / `↓`: seleziona i risultati e cambia pagina ai bordi dell'elenco
- `Enter`: apre il file
- `Ctrl+Enter`: mostra il file in Esplora file
- `Ctrl+C`: copia il percorso
- `Esc`: nasconde la finestra
- `Ctrl+P`: filtra per tipo
- `Ctrl+S`: ordina
- `Ctrl+,`: apre le impostazioni

I risultati sono suddivisi in pagine da 50. Il discreto controllo `‹ 1 / N ›` appare soltanto quando serve.

## Server MCP

`dowse mcp` avvia su stdio un server MCP di sola lettura per l'indice locale.

```text
claude mcp add --scope user dowse -- dowse mcp
```

Fornisce quattro strumenti: `search`, `preview`, `read_file_chunk` e `index_status`. `search` supporta paginazione `limit` / `offset`, conteggio totale, filtro per estensione e ordinamento; `read_file_chunk` pagina in modo limitato il testo già presente nell'indice.

## Tecnologie

Rust · tantivy · jieba · Tauri 2 · Svelte 5 · Windows.Media.Ocr · Win32 (MFT / USN Journal)

## Licenza

Doppia licenza [MIT](LICENSE-MIT) o [Apache-2.0](LICENSE-APACHE), a scelta.
