# dowse-core

`dowse` 的核心库：本地文件全文搜索引擎，基于 [tantivy](https://github.com/quickwit-oss/tantivy) 倒排索引 + [jieba](https://github.com/messense/tantivy-jieba) 中文分词。

## 功能

- **中文分词全文检索**：jieba 分词器接入 tantivy，中英文混排正确切词，BM25 相关性排序。
- **编码探测**：自动识别 GBK / UTF-8 等编码，正确读取历史遗留文本。
- **多格式抽取**：纯文本、PDF、Office（docx/xlsx 等 zip 打包格式）内容抽取。
- **增量索引与实时监听**：文件变更后自动对账、增量更新索引。
- **NTFS 快速索引**（Windows）：通过 MFT / USN Journal 直接枚举卷，冷启动建索引大幅提速。
- **OCR**（Windows）：图片内文字经 `Windows.Media.Ocr` 抽取后一并索引。

> 注意：NTFS 快速层与 OCR 为 Windows 专属能力（`#[cfg(windows)]`），
> 非 Windows 平台自动降级为桩实现，全文检索等核心功能仍可用。

## 用法

```toml
[dependencies]
dowse-core = "0.6"
```

```rust
use dowse_core::{rebuild_index, Searcher};
```

## License

MIT OR Apache-2.0
