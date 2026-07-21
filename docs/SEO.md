# Dowse SEO 执行手册

最后更新：2026-07-21

## 当前判断

Dowse 已经能通过品牌词被找到，docs.rs 也已收录 crate 描述；但用“Windows 本地搜索文件内容软件”“Windows 全文搜索工具”等非品牌需求词搜索时，Dowse 尚未进入可见结果。现阶段的目标不是堆砌关键词，而是让搜索引擎明确识别：

> Dowse 是开源、全程本地的 Windows 文件内容全文搜索工具，覆盖 PDF / Office / 代码 / 图片 OCR，并可通过只读 MCP 给 AI 使用。

## 关键词与页面分工

| 页面 | 搜索意图 | 主关键词 | 辅助词 |
|---|---|---|---|
| `/dowse/` | 找产品、下载软件 | Windows 本地文件全文搜索工具 | 搜索文件内容、中文全文搜索、图片 OCR 搜索、Everything 替代 |
| `/dowse/windows-file-content-search/` | 解决“怎么搜内容” | Windows 搜索文件内容 | PDF 全文搜索、Word 内容搜索、Everything content、Windows 内容索引 |
| `/dowse/en/` | 英文产品发现 | Windows file content search | local full-text search, OCR file search, desktop search, MCP file search |
| GitHub README | 技术评估、开源可信度 | open-source Windows file content search | Rust, tantivy, jieba, OCR, MCP |
| docs.rs / crates.io | Rust 与 CLI 用户 | Windows full-text search crate | tantivy, Chinese tokenization, MCP server |

同一个搜索意图只由一个页面主攻，避免多个页面标题和正文高度重复。新增内容必须先判断属于哪个意图。

## 已在仓库实施

- 主页标题、描述、H1 与正文对齐产品主关键词。
- Open Graph、Twitter Card、canonical、hreflang 与大图预览。
- `SoftwareApplication`、`FAQPage`、`TechArticle`、`BreadcrumbList` 结构化数据。
- 中文需求型指南与英文产品页。
- `sitemap.xml`、`robots.txt`、PWA manifest、`llms.txt`。
- README、Cargo、MCPB 和 MCP Registry 元数据反向链接官网。
- Pages 部署前运行 `.github/scripts/check_site.py`，检查 canonical、描述、H1、JSON-LD、内部链接和 sitemap。
- 修正“Everything 完全不能搜索内容”等不准确比较，避免损害可信度。

## 发布后需要人工完成

这些操作涉及站点或第三方账户，不能仅靠提交仓库文件完成。

1. 在 GitHub 仓库右侧 About 中把 Website 设置为 `https://lter.space/dowse/`。
2. 推送并确认 GitHub Pages 上三个 URL 均返回 200。
3. 在 Google Search Console、Bing Webmaster Tools 提交 `https://lter.space/dowse/sitemap.xml`。
4. 如果维护 `lter.space` 根站，在域名根目录的 `/robots.txt` 中也声明该 sitemap。搜索引擎只把主机根目录的 robots 文件视为正式规则；项目子路径里的副本主要用于人和诊断工具发现。
5. 在百度搜索资源平台验证 `lter.space` 并提交三个页面。页面正文已经使用简体中文和明确的 Windows 需求词，不需要再堆词。
6. 下一次发布 crate 时确认 crates.io 的 homepage 已变为产品官网；旧版本的元数据不会自动回写。
7. 保持 MCP Registry、Glama、Smithery 等目录中的 website 指向官网，repository 指向 GitHub。

## 合理的外部发现渠道

优先争取能带来真实用户和独立页面的渠道：

- 提交 WinGet、Scoop、Chocolatey 安装清单。
- 提交 AlternativeTo 等软件目录，分类使用 Desktop Search / File Search。
- 在正式发布帖中解决具体问题，而不是只贴项目链接：例如“如何搜索截图里的文字”“Everything 内容搜索慢时怎么办”“让 Claude 只读搜索本地资料”。
- 发布版本说明时链接对应指南或功能章节，让 GitHub Releases、MCP 目录、包管理器与官网形成稳定的互链。
- 如果写对比文章，使用可复现的场景和准确表述；不要把竞品描述成不具备其实际拥有的功能。

## 内容节奏

只有在能提供独立价值时再新增页面。前四个值得写的主题：

1. Windows 如何搜索截图和图片里的文字。
2. Everything 的 `content:` 为什么会慢，以及何时应该建立全文索引。
3. 中文文件全文搜索为什么需要分词与 GBK 编码探测。
4. 如何让 Claude / Cursor 通过只读 MCP 搜索本地文件。

每篇应包含：问题定义、原理、可执行步骤、限制、真实截图、相关产品功能链接。避免把同一段产品介绍复制成多篇“关键词页面”。

## 衡量方式

每两周查看一次，不需要每天追排名：

- 非品牌查询曝光：包含 `Windows` + `文件内容/全文搜索/OCR` 的查询数。
- 品牌查询：`dowse`、`问渠 dowse` 的展示与点击。
- 三个页面的已收录状态、canonical 选择和抓取错误。
- 官网到 GitHub Release 的点击，以及 Release 下载量。
- 外部引用域名数量；只计真实软件目录、文章和社区讨论，不购买垃圾链接。

前三个月的合理里程碑是“开始获得非品牌长尾曝光”，不是立即超过 Everything、AnyTXT 等多年积累的网站。

## 发布维护规则

- 功能变化时同步更新主页、英文页、README、`llms.txt` 与 JSON-LD 的能力描述。
- 只有正文发生实质更新时才修改 sitemap 的 `lastmod`。
- 新页面必须有唯一 title、description、H1 和 canonical，并加入 sitemap 与至少一个站内链接。
- 不使用隐藏文字、关键词堆砌、批量生成近似页面、虚假评分或未经验证的性能对比。
