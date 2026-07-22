# dowse（问渠）

**Windows 本地文件内容全文搜索**的命令行工具，搜索文件名、PDF / Office 正文与图片 OCR 文字，基于 [tantivy](https://github.com/quickwit-oss/tantivy) + jieba 中文分词。同时可作为 [MCP](https://modelcontextprotocol.io) 服务，供 AI 客户端调用本地搜索能力。

[产品官网](https://lter.space/dowse/) · [Windows 搜索文件内容指南](https://lter.space/dowse/windows-file-content-search/) · [GitHub](https://github.com/ltspace/dowse)

> ⚙️ **平台**：主要面向 **Windows**（NTFS/USN 快速索引、OCR 均为 Windows 专属能力）。
> 其他平台可编译安装，核心全文检索仍可用，但上述加速/OCR 功能会自动降级停用。
> **需要 Rust 1.85+**（edition 2024）。

## 安装

```sh
cargo install dowse
```

## 用法

```sh
# 建索引（全量）
dowse index D:\notes

# 搜索
dowse search 分布式 限流器          # 多词默认 AND
dowse search "rate limiter"         # 引号短语
dowse search 报告 -n 20             # 最多 20 条
dowse search 笔记 --ext md,txt      # 只搜指定扩展名
dowse search 笔记 --sort mtime_desc # 按修改时间排序（relevance/mtime_desc/mtime_asc/size_desc）

# 查看索引概况：位置、文档数、落盘体积、已注册根、最近更新时间
dowse status

# 前台监听文件变更、实时增量更新（调试用），Ctrl+C 退出
dowse watch

# 作为只读 MCP 服务对接 AI 客户端，例：
#   claude mcp add --scope user dowse -- dowse mcp
dowse mcp
```

## 功能

- 中文分词全文检索（jieba + tantivy，BM25 相关性排序）
- 多词 AND、引号短语、扩展名过滤、多种排序
- 自动编码探测（GBK / UTF-8 等）
- PDF / Office / 纯文本内容抽取
- 增量索引 + 文件变更实时监听
- NTFS/USN 快速索引与图片 OCR（Windows）
- 作为 MCP 服务对接 AI 客户端

## MCP Registry

Listed on the official MCP Registry as `mcp-name: io.github.ltspace/dowse`.

## License

MIT OR Apache-2.0
