[English](README.md) | 简体中文

<p align="center">
  <img src="crates/dowse-app/src-tauri/icons/128x128@2x.png" width="96" height="96" alt="dowse logo">
</p>

<h1 align="center">dowse</h1>

<p align="center">
  Windows 本地全文搜索。文件名、文档内容、截图里的文字，快捷键呼出，毫秒级返回。
</p>

<p align="center">
  <a href="#许可"><img src="https://img.shields.io/badge/license-MIT%2FApache--2.0-blue.svg" alt="License"></a>
  <a href="https://github.com/ltspace/dowse/releases/latest"><img src="https://img.shields.io/github/v/release/ltspace/dowse" alt="最新版本"></a>
  <a href="https://github.com/ltspace/dowse/actions/workflows/ci.yml"><img src="https://github.com/ltspace/dowse/actions/workflows/ci.yml/badge.svg" alt="CI 状态"></a>
  <a href="https://github.com/ltspace/dowse/stargazers"><img src="https://img.shields.io/github/stars/ltspace/dowse?style=flat" alt="GitHub stars"></a>
  <img src="https://img.shields.io/badge/platform-Windows-0078D6?logo=windows&logoColor=white" alt="平台：Windows">
  <a href="https://www.rust-lang.org"><img src="https://img.shields.io/badge/rust-2024_edition-orange?logo=rust&logoColor=white" alt="Rust 2024 edition"></a>
  <a href="https://github.com/ltspace/dowse/releases"><img src="https://img.shields.io/github/downloads/ltspace/dowse/total" alt="下载量"></a>
</p>

名字取自 dowsing rod（探水杖）。

![dowse 浮窗搜索中途，查询词"sql"，下方是排好序的文件结果，右侧是预览区](docs/screenshots/hero.png)

## 动机

Windows 上没有同时满足以下三点的工具：

- 搜索文件内容而不只是文件名（Everything 只做后者）
- 识别并索引图片中的文字（macOS Spotlight 有，Windows 没有对等实现）
- 一个快捷键呼出、键盘完成全部操作、不引入可感知的延迟

最接近的开源实现是 sist2，但它面向 Linux，Windows 下只能通过 Docker 运行，
中文按 trigram 处理，项目已停止维护。dowse 是针对这三点的 Windows 原生实现。

## 功能

| | |
|---|---|
| 🔍 **文件名搜索** | 输入即搜，秒级返回 |
| 📄 **文档内容搜索** | 纯文本、Markdown、代码，以及 Office 格式（PDF、Word、Excel、PowerPoint） |
| 🖼️ **截图 / 图片 OCR** | PNG/JPG/WebP/BMP 里的文字，全离线（Windows.Media.Ocr） |
| 🈶 **中文分词** | jieba 分词 + BM25 排序，不是 trigram；外加自动 GBK 编码探测 |
| ⚡ **增量索引** | 运行期文件监听，启动时按 mtime/size 对账补齐 |
| 🤖 **MCP server** | 通过 stdio 把本地搜索能力暴露给 AI agent |
| 🚀 **NTFS 快速层** | MFT 直读 + USN Journal，仅管理员权限下启用，否则静默退回常规路径 |

## dowse 与同类工具

| | dowse | Everything | Windows 自带搜索 | sist2 |
|---|:---:|:---:|:---:|:---:|
| 文件名搜索 | ✓ | ✓ | ✓ | ✓ |
| 文档内容搜索 | ✓ | ✗ | 部分支持，较慢 | ✓ |
| 截图 / 图片 OCR | ✓ | ✗ | ✗ | 有限（可选接 Tesseract） |
| 中文正确分词 | ✓（jieba） | — | 有限 | ✗（trigram） |
| 纯本地，不联网 | ✓ | ✓ | ✓ | ✓ |
| 全局快捷键浮窗 | ✓ | ✓ | ✓（Win 键） | ✗（网页 UI） |
| Windows 原生 | ✓ | ✓ | ✓ | ✗（面向 Linux，Windows 下走 Docker） |

## 中文处理

- jieba 分词，BM25 排序（tantivy 引擎）。不使用 trigram。
- 自动探测文件编码（chardetng）。GBK 文件正确解码后入索引。
- 多词查询默认 AND 语义。引号短语查询按位置精确匹配。
- OCR 使用 Windows 自带引擎（Windows.Media.Ocr），离线运行，
  zh-Hans 语言包同时覆盖中英混排，无需配置。

## 性能

设计目标，超出即视为缺陷。"实测"列是 `dowse 0.7.0` 从零重跑的基准测试（i7-13700K /
24 逻辑核 / 64GB 内存，单机单次会话，2026-07-12），复用了 v0.6.1 第三轮基准的同一份语料
（逐字节一致），确保可比。完整原始输出（建索引/搜索日志、JSON 结果文件）留在本次基准的
工作目录里，不进本仓库。

| 指标 | 设计目标 | 实测（v0.7.0，2026-07-12） |
|---|---|---|
| 快捷键到窗口可见 | < 50ms | 本轮未测——纯 CLI 基准，未接入浮窗应用埋点 |
| 键入到结果上屏 | < 80ms | 本轮未测——同上 |
| OCR 单张 | ~112ms / 1080p 截图 | 多次重跑稳定在约 4–27ms 孤立测（已贴近命令行启动本身的噪声下限；紧跟在全量 OCR 之后跑的第一轮受干扰测出约 170ms，已弃用，见表下说明）；批量跑满队列吞吐约 4–5ms/张（工作线程池+批量提交摊薄）——测试图是合成的 480×200 图片，不是真实 1080p 截图 |
| 常驻内存 | < 150MB（空闲态） | 空闲态未测；全量索引过程中的峰值 working set 约 327MB——这是另一个指标，不能直接对照空闲态目标判定回归 |
| 安装包体积 | < 15MB | **9.77MB**（`dowse-app_0.7.0_x64-setup.exe`，已发布版本） |
| 全文索引构建，10,000 文件 / 437MB | 秒级（规划中的纯文件名快速路径） | 10.0–10.6s——当前的全文内容 `dowse index`，不是规划中的纯文件名 MFT 路径 |
| 全文索引构建 + OCR，15,100 文件（含 5,100 张图） | — | 首次约 46.6s，同一遍里 5,100/5,100 张图全部完成 OCR——无剩余、不需要第二遍 |
| 搜索延迟 P50（任务要求的 5 类） | — | 单词/中文短语/英文短语/多词 AND/无结果五类落在 30.7–161.1ms，基于 15,100 篇文档的索引 |
| 搜索延迟 P95 | — | 同五类，39.1–172.3ms |
| `ext:` 过滤查询延迟 | — | P50 155.6ms，与非零结果的其它查询同一量级 |
| 索引体积 ÷ 语料体积 | — | 0.36（纯文本），低于 v0.6.1 那一轮的 0.54 |

全量索引相关行用的是和上面 v0.6.1 第三轮完全同一份语料：10,000 文件 / 437.66MB 文本，
加 5,100 张合成 480×200 OCR 测试图（89.8MB）——逐字节复用，没有重新生成。建索引耗时比
v0.6.1 快了约一倍，纯文本索引体积也小了约三分之一，两者都对得上新分词器（小写归一、拉丁词
按字母数字边界切分）带来的更精简的词典。无结果那类查询从 v0.6.1 的 135ms 降到贴近启动噪声
的 31ms，符合"索引更小、判定词不存在要扫的东西更少"的解释。OCR 满队列吞吐这项（从
12–19ms/张降到 4–5ms/张）测的是同一张源图复制 20 份的探针语料，只能当参考、不是精确上界——
如果操作系统 OCR 管线对图片内容做了某种缓存，这轮和 v0.6.1 那轮会同等受益，并不能单独解释
这次的提升。

## 使用

**下载安装包** — 从[最新版本](https://github.com/ltspace/dowse/releases/latest)下载
`dowse-app_*_x64-setup.exe`，运行安装，然后 `Alt+\`` 呼出。

安装包未做代码签名，首次运行时 Windows SmartScreen 会拦截。点击**更多信息**再点**仍要运行**
即可继续。代码签名证书对一个独立项目是一笔实打实的经常性开销，暂时难以负担；未来版本可能会
重新考虑。

**源码构建：**

```powershell
git clone https://github.com/ltspace/dowse && cd dowse

# CLI
cargo run -p dowse -- index D:\docs      # 建索引
cargo run -p dowse -- search 限流         # 搜索
cargo run -p dowse -- search "精确短语"   # 短语查询

# 浮窗应用（Tauri 2 + Svelte 5）
cd crates/dowse-app
npm install
cargo tauri build      # 安装包产出在 target/release/bundle 下
```

浮窗应用：Alt+` 呼出，`↑↓` 选择，`Enter` 打开，
`Ctrl+Enter` 在资源管理器中定位，`Ctrl+C` 复制路径，`Esc` 隐藏。输入条右侧有两个幽灵态下拉：
类型筛选（`Ctrl+P`，全部/文档/代码/图片）和排序器（`Ctrl+S`，相关性/最新优先/最旧优先/
最大优先），默认态几乎不占视觉存在感，选中非默认值才会显形。结果行右键弹出 Windows
原生上下文菜单（打开/打开所在文件夹/复制完整路径/复制文件名）。输入条最右端的图钉按钮
可以固定窗口。固定后失焦不再自动隐藏，会话级状态，重启应用后回到未固定。

![预览区展示一条图片类结果：原图内嵌显示，旁边是 OCR 识别出的文字及命中词高亮](docs/screenshots/ocr-preview.png)

## MCP 接入

`dowse mcp` 启动一个只读的 [MCP](https://modelcontextprotocol.io) server，走 stdio 传输，
把本地索引暴露给 AI agent：

```
claude mcp add dowse -- dowse mcp
```

三个工具：`search`（查询词、条数上限、可选 `ext` 过滤）、`preview`（单条命中的完整摘要+
元信息）、`index_status`（文档总数、索引健康状态）。这个 server 绝不碰索引写入端——每次
调用前只做一次 reader reload，所以可以和浮窗应用或正在跑的 `dowse watch` 同时存在，
不会有写入冲突。

![浮窗空闲态，后台正在对截图做 OCR 索引，底部是进度条](docs/screenshots/actions.png)

## 架构

```
                 ┌─────────────────────────────────────────┐
                 │              dowse-core                  │
                 │  tantivy 索引 · jieba 分词 · 编码探测      │
                 │  文本抽取(txt/md/pdf/代码/docx/xlsx/pptx)  │
                 │  · OCR 管线                                │
                 └──────┬──────────┬──────────┬────────────┘
                        │          │          │
                 ┌──────┴───┐ ┌────┴─────┐ ┌──┴───────────┐
                 │ dowse-app │ │ dowse-cli │ │ MCP server   │
                 └──────────┘ └──────────┘ └──────────────┘
```

单一索引核心，三个消费端。dowse-app 是 Tauri 2 + Svelte 5 的常驻浮窗；
CLI 用于脚本和调试；MCP server 将本地索引暴露给 AI agent。

索引更新采用监听 + 对账两级机制：运行期间由文件系统事件驱动增量更新
（500ms 防抖窗口合并，批量提交）；启动时按 mtime/size 比对补齐停机期间的变更。
在 NTFS 卷且有管理员权限时，这两级机制改由 MFT 直读和 USN Journal 提供，
不再走目录遍历和文件系统事件监听；两条路径产出完全一致，上层无法区分正在跑哪一条。

## 路线图

| # | 内容 | 状态 |
|---|------|------|
| 1 | CLI 索引与搜索：中文分词、GBK 探测、高亮 | ✅ 完成 |
| 2 | 浮窗：全局快捷键、Acrylic 材质、键盘导航 | ✅ 完成 |
| 3 | 增量索引：文件监听、启动对账 | ✅ 完成 |
| 4 | OCR 管线：截图文字入索引 | ✅ 完成 |
| 5 | MCP server | ✅ 完成 |
| 6 | NTFS MFT / USN Journal 快速路径 | ✅ 完成（管理员权限下的快车道未经真机验证，见设计文档"实施备注"） |
| 7 | 语义搜索（向量召回、混合排序） | 🔍 探索中 |

## 技术栈

Rust · [tantivy](https://github.com/quickwit-oss/tantivy) · jieba ·
Tauri 2 · Svelte 5 · Windows.Media.Ocr · notify · Win32（MFT/USN Journal）

## 设计文档

- [docs/DESIGN-M2-浮窗.md](docs/DESIGN-M2-浮窗.md)
- [docs/DESIGN-M3-增量索引.md](docs/DESIGN-M3-增量索引.md)
- [docs/DESIGN-M4-OCR管线.md](docs/DESIGN-M4-OCR管线.md)
- [docs/DESIGN-M5-MCP.md](docs/DESIGN-M5-MCP.md)
- [docs/DESIGN-M6-NTFS快速层.md](docs/DESIGN-M6-NTFS快速层.md)

## 隐私

索引存储在本地（`%LOCALAPPDATA%\dowse`）。不联网，无遥测。这一点你可以自行验证：用资源监视器
或防火墙工具观察进程，确认它不建立任何对外连接。发布版还会随安装包提供 SHA-256 校验和，
供你核验下载文件的完整性。

## 许可

双许可协议 [MIT](LICENSE-MIT) 或 [Apache-2.0](LICENSE-APACHE)，任选其一。

## 一点想法

童年时我只有一部酷派手机。没有网络的大把时间里，我会点开文件管理器，一个个研究里面的文件和它们的构成，却总被那些散落各处、又不知内容为何的文件弄得一头雾水。

大学时买了台威联通 NAS，才发现世界上有 Qsirch 这么好用的东西，可它只活在 NAS 里，没有 Windows 版。

screenpipe 珠玉在前，像是黑镜第一季第三集《The Entire History of You》里记忆颗粒的初级版本。它很未来，很后现代，几乎是人在本地搜索这件事上的终极形态，但对当下的世界来说太重了。

所以我做了 dowse。

科幻电影《她》像一则预言，不远的将来，AI 会接管我们的个人电脑。dowse 受此启发，也提供 MCP 接口供 AI 调用，只不过检索的是你自己的文件，在你自己的机器上。

如果你也有点强迫症、喜欢整理、对自己的文件系统有很强的掌控欲，那这个软件很适合你。性能和美感，是我同样在意的东西。
