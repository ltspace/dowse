# dowse

本地文件极速全文搜索的命令行工具，基于 [tantivy](https://github.com/quickwit-oss/tantivy) + jieba 中文分词。同时可作为 [MCP](https://modelcontextprotocol.io) 服务，供 AI 客户端调用本地搜索能力。

## 安装

```sh
cargo install dowse
```

> 需要 Rust 1.85+（edition 2024）。NTFS 快速索引与 OCR 为 Windows 专属能力，
> 其他平台自动降级，全文检索核心功能仍可用。

## 功能

- 中文分词全文检索（jieba + tantivy，BM25 排序）
- 自动编码探测（GBK / UTF-8 等）
- PDF / Office / 纯文本内容抽取
- 增量索引 + 文件变更实时监听
- 作为 MCP 服务对接 AI 客户端

## License

MIT OR Apache-2.0
