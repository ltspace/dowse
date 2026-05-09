# dowse

Windows 本地全文搜索。索引文件名、文档内容和截图中的文字，快捷键呼出，毫秒级返回。

名字取自 dowsing rod（探水杖）。

<!-- 【图位 1：浮窗操作 GIF——呼出、输入、结果上屏、跳转文件夹】 -->

## 动机

Windows 上没有同时满足以下三点的工具：

- 搜索文件内容而不只是文件名（Everything 只做后者）
- 识别并索引图片中的文字（macOS Spotlight 有，Windows 没有对等实现）
- 一个快捷键呼出、键盘完成全部操作、不引入可感知的延迟

最接近的开源实现是 sist2，但它面向 Linux，Windows 下只能通过 Docker 运行，
中文按 trigram 处理，项目已停止维护。dowse 是针对这三点的 Windows 原生实现。

## 中文处理

- jieba 分词，BM25 排序（tantivy 引擎）。不使用 trigram。
- 自动探测文件编码（chardetng）。GBK 文件正确解码后入索引。
- 多词查询默认 AND 语义。引号短语查询按位置精确匹配。
- OCR 使用 Windows 自带引擎（Windows.Media.Ocr），离线运行，
  zh-Hans 语言包同时覆盖中英混排，无需配置。

## 性能

设计目标，超出即视为缺陷：

| 指标 | 目标 | 说明 |
|------|------|------|
| 快捷键到窗口可见 | < 50ms | 进程常驻，呼出为 show + focus |
| 键入到结果上屏 | < 80ms | UI 与索引同进程，无 IPC |
| OCR | ~112ms / 1080p 截图 | 实测中位数，后台线程池处理 |
| 常驻内存 | < 150MB | 含索引 reader |
| 安装包 | < 15MB | Tauri，非 Electron |
| 文件名索引构建（规划） | 秒级 | NTFS MFT 直读，同 Everything |

## 使用

```powershell
git clone https://github.com/ltspace/dowse && cd dowse

cargo run -p dowse -- index D:\docs      # 建索引
cargo run -p dowse -- search 限流         # 搜索
cargo run -p dowse -- search "精确短语"   # 短语查询
```

浮窗应用（开发中）：Alt+` 呼出，`↑↓` 选择，`Enter` 打开，
`Ctrl+Enter` 在资源管理器中定位，`Ctrl+C` 复制路径，`Esc` 隐藏。

<!-- 【图位 2：亮/暗主题 + 预览区截图】 -->

## 架构

```
                 ┌─────────────────────────────────────────┐
                 │              dowse-core                  │
                 │  tantivy 索引 · jieba 分词 · 编码探测      │
                 │  文本抽取(txt/md/pdf/代码) · OCR 管线*     │
                 └──────┬──────────┬──────────┬────────────┘
                        │          │          │
                 ┌──────┴───┐ ┌────┴─────┐ ┌──┴───────────┐
                 │ dowse-app │ │ dowse-cli │ │ MCP server*  │
                 └──────────┘ └──────────┘ └──────────────┘
                                               * 规划中
```

单一索引核心，三个消费端。dowse-app 是 Tauri 2 + Svelte 5 的常驻浮窗；
CLI 用于脚本和调试；MCP server 将本地索引暴露给 AI agent。

索引更新采用监听 + 对账两级机制：运行期间由文件系统事件驱动增量更新
（500ms 防抖窗口合并，批量提交）；启动时按 mtime/size 比对补齐停机期间的变更。

## 路线图

| # | 内容 | 状态 |
|---|------|------|
| 1 | CLI 索引与搜索：中文分词、GBK 探测、高亮 | 完成 |
| 2 | 浮窗：全局快捷键、Acrylic 材质、键盘导航 | 开发中 |
| 3 | 增量索引：文件监听、启动对账 | 完成 |
| 4 | OCR 管线：截图文字入索引 | 技术验证完成 |
| 5 | MCP server | 规划 |
| 6 | NTFS MFT / USN Journal 快速路径 | 规划 |

## 技术栈

Rust · [tantivy](https://github.com/quickwit-oss/tantivy) · jieba ·
Tauri 2 · Svelte 5 · Windows.Media.Ocr · notify

## 设计文档

- [docs/DESIGN-M2-浮窗.md](docs/DESIGN-M2-浮窗.md)
- [docs/DESIGN-M3-增量索引.md](docs/DESIGN-M3-增量索引.md)

## 隐私

索引存储在本地（`%LOCALAPPDATA%\dowse`）。不联网，无遥测。
