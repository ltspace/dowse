[English](README.md) | [简体中文](README.zh-CN.md) | [日本語](README.ja.md) | [한국어](README.ko.md) | Español | [Italiano](README.it.md)

<p align="center">
  <img src="crates/dowse-app/src-tauri/icons/128x128@2x.png" width="96" height="96" alt="Logotipo de dowse">
</p>

<h1 align="center">dowse</h1>

<p align="center">
  Buscador local de código abierto para Windows. Encuentra nombres de archivo, contenido de PDF y Office, código fuente y texto dentro de capturas de pantalla con un solo atajo.
</p>

<p align="center">
  <a href="https://lter.space/dowse/">Sitio web</a> ·
  <a href="https://github.com/ltspace/dowse/releases/latest">Descargar la última versión</a>
</p>

![Dowse buscando en el espacio de trabajo ficticio Northstar, con resultados a la izquierda y una vista previa a la derecha](docs/screenshots/hero.png)

## Características

| | |
|---|---|
| 🔍 **Búsqueda por nombre** | Resultados instantáneos mientras escribes |
| 📄 **Búsqueda en documentos** | Texto, Markdown, código, PDF, Word, Excel y PowerPoint |
| 🖼️ **OCR de imágenes** | Texto de PNG / JPG / WebP / BMP mediante Windows.Media.Ocr, completamente sin conexión |
| 🈶 **Segmentación de chino** | jieba + BM25 en lugar de trigramas, con detección automática de GBK |
| ⚡ **Indexación incremental** | Vigilancia de archivos durante la ejecución y conciliación al iniciar |
| 🤖 **Servidor MCP** | Expone la búsqueda local a agentes de IA mediante stdio |
| 🚀 **Ruta rápida NTFS** | MFT + USN Journal con permisos de administrador y alternativa automática cuando no están disponibles |

Los datos y el índice permanecen en tu PC. No hay telemetría ni tráfico de red.

## Inicio rápido

Descarga `dowse-app_*_x64-setup.exe` desde la [última versión](https://github.com/ltspace/dowse/releases/latest), instálalo y pulsa `Alt+\`` para abrir el buscador.

El instalador no está firmado. Si Windows SmartScreen muestra una advertencia, elige **Más información** y después **Ejecutar de todas formas**.

Compilar desde el código fuente:

```powershell
git clone https://github.com/ltspace/dowse && cd dowse

# CLI
cargo run -p dowse -- index D:\docs
cargo run -p dowse -- search palabra

# Interfaz Tauri
cd crates/dowse-app
npm install
cargo tauri build
```

## Controles de la interfaz

- `Alt+\``: mostrar u ocultar
- `↑` / `↓`: seleccionar resultados y cambiar de página al llegar a un extremo
- `Enter`: abrir el archivo
- `Ctrl+Enter`: mostrarlo en el Explorador
- `Ctrl+C`: copiar la ruta
- `Esc`: ocultar
- `Ctrl+P`: filtrar por tipo
- `Ctrl+S`: ordenar
- `Ctrl+,`: abrir ajustes

Los resultados se dividen en páginas de 50. El discreto control `‹ 1 / N ›` solo aparece cuando hay más de una página.

## Servidor MCP

`dowse mcp` inicia por stdio un servidor MCP de solo lectura sobre el índice local.

```text
claude mcp add --scope user dowse -- dowse mcp
```

Ofrece cuatro herramientas: `search`, `preview`, `read_file_chunk` e `index_status`. `search` admite paginación con `limit` / `offset`, total de resultados, filtro por extensión y ordenación; `read_file_chunk` pagina de forma acotada el texto ya almacenado en el índice.

## Tecnologías

Rust · tantivy · jieba · Tauri 2 · Svelte 5 · Windows.Media.Ocr · Win32 (MFT / USN Journal)

## Licencia

Licencia dual [MIT](LICENSE-MIT) o [Apache-2.0](LICENSE-APACHE), a tu elección.
