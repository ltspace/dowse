---
name: dowse
description: Search the user's local files with dowse, a Windows full-text search index (Chinese + English, PDF/txt/md/code). Use this when the user asks you to find something on their disk — a note, a doc, a config, "that file about X" — instead of grepping the whole filesystem.
---

# dowse：本地全盘搜索

dowse 是一个常驻 Windows 的本地全文搜索索引（tantivy + jieba 分词），浮窗应用
负责建索引、增量更新；这个 MCP server 是它的只读查询接口，不提供任何会修改
索引的工具。用户问"我硬盘上关于 XXX 的笔记/文档/配置在哪"这类问题时，优先用
这里的工具而不是让 agent 自己去遍历文件系统或调用 grep 类工具。dowse 的索引
覆盖了用户机器上所有已收录的目录，一次查询就能跨全盘命中，比现场扫描快得多、
也不会漏掉不在当前工作目录下的文件。

## 四个工具怎么组合用

1. **search**：先用它定位候选文件。查询支持多词 AND、`"短语"`、大写 `OR`、
   `NOT`/`-词`，以及 `path:`、`mtime:`、`size:` 操作符；还可按多个扩展名过滤、
   按相关度/修改时间/体积排序，并用 `limit`/`offset` 翻页。返回中的 `total_hits`
   是总命中数，`path` 是后续工具使用的精确路径。

2. **preview**：从 search 结果里挑一条，把它的 `path` 和当时用的 `query` 传
   给 preview，拿到一段长得多的上下文（约 1500 字，也带 `«»` 高亮），外加
   文件大小、修改时间。search 的 snippet 只有 160 字左右，判断"是不是这篇"
   经常不够，需要看更多上下文时用 preview，不用自己去读整个文件。

3. **read_file_chunk**：preview 仍不够时，用 search 返回的 `path` 分页读取已索引
   正文。默认 4000 字，单页最多 8000 字；如果 `next_offset_chars` 非空，把它作为
   下一次的 `offset_chars`。内容来自索引快照，不要把它描述为刚从原文件读取。

4. **index_status**：不需要参数。想知道索引里有多少文档、覆盖了哪些根目录、
   索引落盘多大、最近一次更新是什么时候，调这个。比如 search 一直没命中时，
   可以先查一下 index_status 确认索引没坏、确实收录过东西。

典型顺序：**search → preview → read_file_chunk（仅在需要更多正文时）**。如果要用
别的工具进一步处理原文件，直接使用返回的 `path`，不需要转换或拼接。

## 用不到的时候

- 索引里没有的目录（用户从没让 dowse-app 收录过）搜不到，这不是 bug，是
  dowse 的设计边界，它只覆盖用户主动收录的目录。index_status 的 `roots`
  字段能看到当前覆盖了哪些目录。
- 索引不存在或损坏时，工具会返回结构化错误加一句建库指引（`dowse index <目录>`），
  不会崩溃；看到这个提示时告诉用户去 dowse-app 浮窗或命令行建一次索引，
  不要自己瞎猜路径重试。
- 这里没有任何写操作：建索引、重建索引、增删收录目录都不在这个工具集里，
  需要用户自己在 dowse-app 浮窗或 `dowse index` 命令行里做。
- read_file_chunk 只能返回已经写入索引的正文，不是通用文件读取器；路径不在索引里
  会返回结构化的 `path_not_found`。
