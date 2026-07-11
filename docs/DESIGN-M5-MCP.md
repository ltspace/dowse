# 里程碑 5 设计说明：MCP server

目标一句话：AI agent 能把 dowse 的本地索引当检索工具用——"帮我找找上个月那份限流方案"这类请求，agent 调 dowse 而不是全盘 grep。

## 一、形态决策

**CLI 子命令，不另立二进制**：`dowse mcp` 启动 stdio 传输的 MCP server。
理由：分发只有一个 exe；MCP 宿主（Claude Code / Claude Desktop / Cursor）
的注册方式就是"命令 + 参数"，子命令天然合拍。

```
claude mcp add dowse -- dowse mcp
```

## 二、并发模型（关键约束）

浮窗应用（dowse-app）常驻并持有索引的**写**权；MCP server 是独立进程，
**只读**打开同一份索引。tantivy 的段文件不可变、读写可跨进程共存——
但有两条纪律：

- MCP 进程绝不碰 IndexWriter（rebuild 之类的变更操作不提供，见工具清单）；
- reader 在每次请求前 reload，保证读到浮窗侧最新 commit 的段。

## 三、工具清单（刻意少）

| 工具 | 参数 | 返回 |
|------|------|------|
| search | query，limit（默认 10），ext（可选扩展名过滤） | 命中数组：path、score、snippet（含高亮标记）、kind |
| preview | path，query | 该文件命中上下文（约 1500 字）+ 元信息（大小/mtime/kind） |
| index_status | 无 | 文档总数、已注册索引根、索引落盘体积、最近一次更新时间 |

**不提供**：rebuild_index、add_root 等一切变更操作。变更是人的决策，
留在浮窗和 CLI 里；agent 只读——这条是安全边界，不是功能取舍。

返回一律结构化 JSON；snippet 的高亮用前后缀标记（不是字节区间），
agent 直接可读。

## 四、随附一份 SKILL.md

面向 Claude Code 的技能说明放仓库 `skill/SKILL.md`：一段话说明 dowse 是什么、
三个工具怎么组合用（先 search 后 preview）、结果里 path 可直接喂给
Read/资源管理器。参照 wx-cli 用 skills CLI 分发的做法，公开后支持
`npx skills add ltspace/dowse`。

## 五、验收清单

1. `claude mcp add dowse -- dowse mcp` 注册后，在 Claude Code 里问
   "我硬盘上关于限流的笔记"，agent 调 search 并给出正确文件路径。
2. 浮窗侧新建文件、增量索引落盘后，MCP 侧下一次 search 能命中（reload 生效）。
3. 浮窗未运行时 MCP 独立可用（只依赖索引文件存在）。
4. 索引不存在时，工具返回结构化错误与一句建库指引，不 panic。
5. 三个工具的 schema 校验：漏参/错型有明确报错。

## 六、明确不做（本里程碑）

- HTTP/SSE 传输（stdio 够用，公开后有人要再说）
- 写操作类工具（安全边界，见上）
- 语义/向量检索工具（索引层还没有向量能力，不在 MCP 层造假）
