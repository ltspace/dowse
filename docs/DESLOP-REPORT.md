# DESLOP 报告

对 v0.6.x 全仓做的一轮减脂。基线 commit `7ff22b1`，四个改动 commit 落在其后。
每刀独立成 commit，可单独回滚；每个 commit 落地前 `cargo test --workspace` /
`clippy -D warnings` / `fmt` / `npm run check` / `npm run build` 全绿。

## 概览

| 项 | 前 | 后 |
|----|----|----|
| workspace 测试 | 162 通过 | 162 通过 |
| 改动文件 | — | 10 |
| 行数 | — | +39 / −57（净 −18） |
| core 顶层公开符号（re-export + `pub fn`） | 48 | 44 |
| core `windows` crate feature 数 | 12 | 10 |

四个改动 commit：

| commit | 刀 | shortstat |
|--------|----|-----------|
| `2a7628b` refactor(highlight): 复用 core 的 normalize_ranges | 2 | +6 / −22 |
| `c78489d` refactor(core): 未外部消费的调参常量降为 pub(crate) | 4 | +10 / −12 |
| `6ac3479` build(core): 移除未使用的 windows feature | 1 | +1 / −1 |
| `0912674` refactor(cli): 高亮区间切片逻辑合并为一处 | 2 | +22 / −22 |

---

## 刀 1 — 死码

排查手段与结论：

| 面 | 手段 | 结果 |
|----|------|------|
| 非 pub Rust 死码 | 全量 `cargo check --all-targets` 带 `-D dead_code`/`unused` | 0 处（编译器无告警） |
| pub 死导出 | 逐个 pub 项跨 cli/app/mcp + `tests/` 反查引用 | 0 处可删（`ntfs_fast_path_available` 一度误判，实为 `tests/ntfs_fast_path` 的权限护栏，已保留） |
| app.css 变量 | 27 个 `--var` 逐个反查 `var(...)` 引用 | 全部至少 1 处引用，0 死变量 |
| app.css / 组件 CSS 选择器 | `svelte-check`（171 文件，0 warning） | 0 个未用选择器 |
| Cargo 依赖 | 三个 crate 每个直接依赖反查 use | 全部在用，0 可删 |
| `windows` crate feature | 12 个 feature 逐个对照实际 API 调用 | 2 个无任何调用点（下表） |
| 前端 TS 导出 / Tauri 插件 | 31 个导出 + 5 个 tauri 插件反查 | 全部在用 |

删减：`dowse-core` 的 `windows` feature 去掉 `Globalization`、`Foundation_Collections`。
OCR 走 `TryCreateFromUserProfileLanguages`（不碰 `Globalization::Language`）、只读
`OcrResult::Text()`（不碰 `Foundation_Collections` 的 `Lines()`），两个 feature 无调用点。
`Storage_Streams`（`OpenAsync` 产出的 `IRandomAccessStream`）、`Win32_Security`
（`CreateFileW` 的 `SECURITY_ATTRIBUTES` 形参）为传递依赖，保留。

> 说明：`data-highlight='underline'` 这套备用高亮样式当前没有代码去 set 该属性，
> 属"未触发"而非"死"，且带明确设计注释（两套高亮 A/B 保留），按"拿不准就留"保留。

## 刀 2 — 重复轮子合并

合并两处：

| 轮子 | 前 | 后 |
|------|----|----|
| `normalize_ranges` | core `searcher` 与 app `highlight` 各一份（12 行逐字重复，评审已点名） | core 版升为 `pub` 单一来源，app 删本地副本改调 `dowse_core::normalize_ranges` |
| 命中区间切片重组 | cli `render_snippet`（终端 ANSI 染色）与 mcp `mark_highlights`（`«»` 标记）两份同形控制流 | 抽出 `wrap_highlight_ranges(fragment, ranges, open, close)`，两处各留一层薄封装只带各自的包裹串 |

系统性反查过任务点名的其它形状（路径处理、错误包装、遍历、防抖测试辅助），未发现
额外重复：路径剥前缀已由 core `display_path` 单点提供（ocr 的 `strip_extended_prefix`
早已委托过去）；错误包装、目录遍历、测试辅助各只有单一实现。

## 刀 3 — 过度抽象拆除

逐项审计，未发现可安全拆除的抽象：

- 单实现 trait：core 仅 `EventSource`/`WatchGuard` 两个 trait，各有 2 个实现
  （`NotifyEventSource` + `UsnEventSource`），非单实现；app 无 trait。
- 纯转发包装：`X` / `X_with_progress` 成对函数（`apply`、`rebuild_index`、
  `add_root`、`rebuild_root`）是对外 API 且有各自测试与多调用点，非单点转发，保留。
- 单调用点"通用"工具：`render_snippet` 保留为命名薄封装（点明"终端染色 + 压行"
  意图，比内联清楚），其复用部分已按刀 2 抽出。

## 刀 4 — API 表面收缩

对 `dowse-core` 约 50 个 pub 项逐个审计（cli/app/mcp + `tests/` 消费面 + 公有签名
可达性）。可安全下沉的只有 5 个纯调参常量，降为 `pub(crate)`：

| 符号 | 内部使用者 |
|------|-----------|
| `QUIET_WINDOW_MS` | `watch` 静默窗口 |
| `WATER_LEVEL` | `events::Debouncer` 水位 |
| `PROGRESS_INTERVAL` | `indexer`/`updater` 进度频率 |
| `MAX_WORKERS` / `MIN_WORKERS` | `ocr_worker` 线程数钳制 |

其余 pub 项未下沉的原因分两类，均为编译器/测试强约束：
- **公有签名可达**：如 `IndexStats`/`IndexProgress`/`BatchOutcome`/`AddRootStats` 等是
  被消费的 pub 函数的返回/回调类型，`WatchEvent` 是 pub `WatchProgress::Received` 的负载，
  降级即触发 E0446。
- **被 `dowse-core/tests/` 集成测试消费**：`reconcile*`、`run_watch`、`EventSource`/
  `WatchGuard`/`NotifyEventSource`、`is_available`、`remove_dir_all_retrying`、
  `ntfs_fast_path_available` 等由集成测试 crate（外部消费者）直接调用，降级会破坏测试。
  归位这些测试为 crate 内单元测试属独立重构，不在本轮零行为改动范围内。

## 刀 5 — 一致性

- 日志入口：core 32 处 `eprintln!` 已是统一形态（中文诊断 + `: {err}`），且 core 无
  日志框架；app `logging.rs` 在进程启动时把 stdout/stderr 整体重定向到轮转日志文件，
  `eprintln!` 本身就是被设计成的统一出口。无散点可收编。
- 错误 context / 命名：抽查 `.context(...)` 与命名，措辞已一致（统一中文），无成规模偏差。

## 刀 6 — 注释

按"删叙述型/辩护型、留一切约束型"扫描。本仓注释绝大多数是约束型（为什么必须这样 /
坑 / 不变量 / 顺序契约），少量含"评审/验收/spike"字样的经核查均承载需求或坑位理由
（如 `reveal_in_folder` 的 raw_arg 注入安全不变量、OCR 语言标签匹配坑），非纯复述或
对审查者喊话。按"拿不准就留"，本刀无删减。

## 验证

| 项 | 结果 |
|----|------|
| `cargo test --workspace` | 162 通过 / 0 失败 |
| `cargo clippy --workspace --all-targets -- -D warnings` | 0 |
| `cargo fmt --all --check` | 干净 |
| `npm run check` | 171 文件 / 0 error / 0 warning |
| `npm run build` | 成功 |

对外接口（CLI 参数、MCP 工具 schema、Tauri command 签名、配置文件格式）零改动。
