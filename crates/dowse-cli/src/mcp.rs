//! `dowse mcp`：stdio 传输的只读 MCP server，把本地索引暴露给 AI agent。
//!
//! 安全边界（见 docs/DESIGN-M5-MCP.md 第二、三节）：
//! - 这个进程绝不碰 IndexWriter，没有任何会修改索引的工具；
//! - 每次工具调用前都对 reader 做一次 reload，读到浮窗侧最新提交的段；
//! - 索引不存在/损坏时返回结构化的工具级错误（`isError: true` + 建库指引），不 panic。

use std::ops::Range;
use std::path::PathBuf;

use dowse_core::{IndexStatus, PreviewHit, SearchHit, Searcher, index_status as core_index_status};
use rmcp::handler::server::tool::IntoCallToolResult;
use rmcp::handler::server::wrapper::{Json, Parameters};
use rmcp::model::{CallToolResult, ServerCapabilities, ServerInfo};
use rmcp::{ErrorData as McpError, ServerHandler, tool, tool_handler, tool_router};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;

/// 命中片段里高亮命中词的前后缀标记（不透出字节区间，agent 直接可读）。
const HL_OPEN: &str = "«";
const HL_CLOSE: &str = "»";

/// search 工具的默认返回条数。
const DEFAULT_SEARCH_LIMIT: usize = 10;

// ---------- 工具参数 ----------

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SearchParams {
    /// 查询词，支持空格分隔多个词（AND 语义）和 "短语查询"
    pub query: String,
    /// 最多返回几条，默认 10
    pub limit: Option<usize>,
    /// 只保留该扩展名的结果（不含点，如 "md"、"pdf"），可选
    pub ext: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct PreviewParams {
    /// 目标文件的完整路径，取自 search 结果里的 path 字段
    pub path: String,
    /// 定位高亮用的查询词，通常和 search 时用的一致
    pub query: String,
}

// ---------- 工具返回 ----------

// Deserialize 只给测试用（CallToolResult::into_typed 反序列化 structured_content
// 回一个具体类型来断言），运行时只往外序列化，不会反过来解。
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct SearchHitOut {
    /// 命中文件的完整路径，可直接喂给文件读取/资源管理器工具
    pub path: String,
    pub score: f32,
    /// 命中上下文片段，命中词用 «» 包起来
    pub snippet: String,
    /// 文件扩展名（不含点，小写），无扩展名是空串
    pub kind: String,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct SearchOutput {
    pub hits: Vec<SearchHitOut>,
    /// 索引里的文档总数（不是本次命中数），给 agent 判断索引规模用
    pub total_docs: u64,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct PreviewOutput {
    /// 命中上下文片段（约 1500 字），命中词用 «» 包起来
    pub snippet: String,
    /// 原文件体积（字节）
    pub size: u64,
    /// 原文件 mtime，Unix 毫秒时间戳
    pub mtime_unix_ms: i64,
    /// 文件扩展名（不含点，小写），无扩展名是空串
    pub kind: String,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct IndexStatusOutput {
    /// 索引里的文档总数
    pub num_docs: u64,
    /// 已注册的索引根目录
    pub roots: Vec<String>,
    /// 索引落盘体积（字节）
    pub disk_size_bytes: u64,
    /// 最近一次更新时间，Unix 毫秒时间戳；索引目录异常读不到文件 mtime 时是 null
    pub last_updated_unix_ms: Option<i64>,
}

/// 把命中区间标成 «...»。区间契约（有序、不重叠、落在 UTF-8 边界上）
/// 由 dowse-core::searcher::normalize_ranges 保证，这里游标只前进不回退。
fn mark_highlights(fragment: &str, ranges: &[Range<usize>]) -> String {
    let mut out = String::with_capacity(fragment.len() + ranges.len() * 6);
    let mut cursor = 0;
    for r in ranges {
        out.push_str(&fragment[cursor..r.start]);
        out.push_str(HL_OPEN);
        out.push_str(&fragment[r.start..r.end]);
        out.push_str(HL_CLOSE);
        cursor = r.end;
    }
    out.push_str(&fragment[cursor..]);
    out
}

fn to_search_hit_out(hit: SearchHit) -> SearchHitOut {
    SearchHitOut {
        path: hit.path,
        score: hit.score,
        snippet: mark_highlights(&hit.snippet, &hit.highlighted),
        kind: hit.ext,
    }
}

fn to_preview_output(hit: PreviewHit) -> PreviewOutput {
    PreviewOutput {
        snippet: mark_highlights(&hit.snippet, &hit.highlighted),
        size: hit.size,
        mtime_unix_ms: hit.mtime,
        kind: hit.ext,
    }
}

fn to_index_status_output(status: IndexStatus) -> IndexStatusOutput {
    let last_updated_unix_ms = status.last_updated.and_then(|t| {
        t.duration_since(std::time::UNIX_EPOCH)
            .ok()
            .map(|d| d.as_millis() as i64)
    });
    IndexStatusOutput {
        num_docs: status.num_docs,
        roots: status
            .roots
            .into_iter()
            .map(|p| p.display().to_string())
            .collect(),
        disk_size_bytes: status.disk_size_bytes,
        last_updated_unix_ms,
    }
}

/// 索引打不开（不存在/损坏/schema 版本不对）时统一的工具级错误：
/// 一句人话 + 一句建库指引。
///
/// 特意用 `CallToolResult::structured_error`（工具级错误，`isError: true`）
/// 而不是 `Err(McpError)`（协议级错误）：请求本身没问题，是索引状态的问题，
/// 协议级错误在很多 MCP 客户端里会被不透明地渲染成"internal error"，agent
/// 看不到具体文案；工具级错误的 content/structured_content 才是 agent 真正
/// 能读到、能据此决定"提示用户先建库"的地方。见 docs/DESIGN-M5-MCP.md 第五节。
fn index_unavailable_result(err: &anyhow::Error) -> CallToolResult {
    CallToolResult::structured_error(json!({
        "error": "index_unavailable",
        "message": format!("索引不可用：{err}"),
        "hint": "先用 `dowse index <目录>` 建一次索引",
    }))
}

/// 打开 searcher 并 reload 到最新提交的段。三个工具的公共开场：
/// 每次请求都重新打开+reload，不缓存 Searcher 实例——浮窗侧随时可能提交新的段，
/// 常驻一个 Searcher 会读到过期数据（见 docs/DESIGN-M5-MCP.md 第二节的并发约束）。
///
/// 返回 `Err(CallToolResult)` 而不是 `Err(McpError)`：索引不可用是工具级错误，
/// 调用方直接 `return Ok(e)` 短路成功分支即可，见上面 index_unavailable_result 的说明。
fn open_and_reload(index_dir: &std::path::Path) -> Result<Searcher, CallToolResult> {
    let searcher = Searcher::open(index_dir).map_err(|e| index_unavailable_result(&e))?;
    searcher
        .reload()
        .map_err(|e| index_unavailable_result(&e))?;
    Ok(searcher)
}

#[derive(Clone)]
pub struct DowseMcpServer {
    index_dir: PathBuf,
}

#[tool_router]
impl DowseMcpServer {
    pub fn new(index_dir: PathBuf) -> Self {
        Self { index_dir }
    }

    #[tool(
        description = "在本地全文索引里搜索，返回按相关度排序的命中列表；命中词用 «» 标出。先用这个工具定位候选文件，再用 preview 看更长的上下文。"
    )]
    async fn search(
        &self,
        Parameters(SearchParams { query, limit, ext }): Parameters<SearchParams>,
    ) -> Result<CallToolResult, McpError> {
        if query.trim().is_empty() {
            return Err(McpError::invalid_params("query 不能为空", None));
        }
        let limit = limit.unwrap_or(DEFAULT_SEARCH_LIMIT);
        if limit == 0 {
            return Err(McpError::invalid_params("limit 必须大于 0", None));
        }
        if let Some(ext) = &ext
            && ext.trim().is_empty()
        {
            return Err(McpError::invalid_params(
                "ext 传了就不能是空字符串，不需要过滤就整个字段别传",
                None,
            ));
        }

        let searcher = match open_and_reload(&self.index_dir) {
            Ok(s) => s,
            Err(tool_error) => return Ok(tool_error),
        };
        let hits = searcher
            .search_filtered(&query, limit, ext.as_deref())
            .map_err(|e| McpError::invalid_params(format!("查询语法有问题：{e}"), None))?;

        Json(SearchOutput {
            hits: hits.into_iter().map(to_search_hit_out).collect(),
            total_docs: searcher.num_docs(),
        })
        .into_call_tool_result()
    }

    #[tool(
        description = "取某个文件在索引里命中查询词的完整上下文（约 1500 字，比 search 返回的摘要长得多），附带文件大小/修改时间/类型。path 用 search 结果里的 path 字段。"
    )]
    async fn preview(
        &self,
        Parameters(PreviewParams { path, query }): Parameters<PreviewParams>,
    ) -> Result<CallToolResult, McpError> {
        if path.trim().is_empty() {
            return Err(McpError::invalid_params("path 不能为空", None));
        }
        if query.trim().is_empty() {
            return Err(McpError::invalid_params("query 不能为空", None));
        }

        let searcher = match open_and_reload(&self.index_dir) {
            Ok(s) => s,
            Err(tool_error) => return Ok(tool_error),
        };
        let hit = searcher
            .preview(&path, &query)
            .map_err(|e| McpError::invalid_params(format!("查询语法有问题：{e}"), None))?;

        match hit {
            Some(hit) => Json(to_preview_output(hit)).into_call_tool_result(),
            // 路径本身没问题（不是参数错误），是索引里当下就是找不到这篇——
            // 文件可能已被删除、或改名后索引还没重新收录。跟"索引不可用"一样，
            // 是运行时状态问题，agent 应该看得到，所以也是工具级错误。
            None => Ok(CallToolResult::structured_error(json!({
                "error": "path_not_found",
                "message": format!("索引里找不到这个路径：{path}"),
                "hint": "文件可能已被删除、改名，或改动后索引还没重新收录；可以先用 search 重新定位",
            }))),
        }
    }

    #[tool(
        description = "查看本地索引的概况：文档总数、已注册的索引根目录、索引落盘体积、最近一次更新时间。不需要参数。"
    )]
    async fn index_status(&self) -> Result<CallToolResult, McpError> {
        match core_index_status(&self.index_dir) {
            Ok(status) => Json(to_index_status_output(status)).into_call_tool_result(),
            Err(e) => Ok(index_unavailable_result(&e)),
        }
    }
}

#[tool_handler]
impl ServerHandler for DowseMcpServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build()).with_instructions(
            "dowse 是本地全盘全文搜索索引的只读查询接口。典型用法：先调 search 定位候选文件，\
             再用命中的 path 调 preview 看更长上下文；也可以先调 index_status 看看索引里有多少东西。\
             这里没有任何会修改索引的工具——索引的建立/重建由用户在 dowse-app 浮窗或 `dowse index` \
             CLI 里手动触发。",
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mark_highlights_wraps_ranges_with_marks() {
        // 特意只标一个区间：clippy 的 single_range_in_vec_init 是给 vec![a..b] 这种
        // "大概率想要 0..b 里每个数"的误用场景提的，这里就是想要单个 Range，不是误用。
        #[allow(clippy::single_range_in_vec_init)]
        let ranges = [0..5];
        let out = mark_highlights("hello world", &ranges);
        assert_eq!(out, "«hello» world");
    }

    #[test]
    fn mark_highlights_multiple_ranges() {
        let out = mark_highlights("限流中间件的实现", &[0..6, 12..15]);
        assert_eq!(out, "«限流»中间«件»的实现");
    }

    #[test]
    fn mark_highlights_no_ranges_returns_original() {
        let out = mark_highlights("nothing highlighted", &[]);
        assert_eq!(out, "nothing highlighted");
    }

    #[tokio::test]
    async fn search_rejects_empty_query() {
        let server = DowseMcpServer::new(PathBuf::from("does-not-matter"));
        let err = server
            .search(Parameters(SearchParams {
                query: "   ".to_owned(),
                limit: None,
                ext: None,
            }))
            .await
            .expect_err("空查询词应该报参数错误");
        assert!(err.message.contains("query"));
    }

    #[tokio::test]
    async fn search_rejects_zero_limit() {
        let server = DowseMcpServer::new(PathBuf::from("does-not-matter"));
        let err = server
            .search(Parameters(SearchParams {
                query: "笔记".to_owned(),
                limit: Some(0),
                ext: None,
            }))
            .await
            .expect_err("limit=0 应该报参数错误");
        assert!(err.message.contains("limit"));
    }

    #[tokio::test]
    async fn search_rejects_empty_ext_string() {
        let server = DowseMcpServer::new(PathBuf::from("does-not-matter"));
        let err = server
            .search(Parameters(SearchParams {
                query: "笔记".to_owned(),
                limit: None,
                ext: Some(String::new()),
            }))
            .await
            .expect_err("空字符串的 ext 应该报参数错误");
        assert!(err.message.contains("ext"));
    }

    #[tokio::test]
    async fn search_on_missing_index_returns_tool_level_error_with_build_hint() {
        let tmp = tempfile::tempdir().unwrap();
        let missing_index = tmp.path().join("no-such-index");
        let server = DowseMcpServer::new(missing_index);

        // 索引不存在是工具级错误（Ok(CallToolResult{is_error: true, ..})），
        // 不是协议级 Err——这样 agent 才能读到具体错误文案，见 index_unavailable_result 的说明。
        let result = server
            .search(Parameters(SearchParams {
                query: "笔记".to_owned(),
                limit: None,
                ext: None,
            }))
            .await
            .expect("索引不存在不应该让 tools/call 本身失败，而是回一个 isError:true 的结果");
        assert_eq!(result.is_error, Some(true));
        let structured = result.structured_content.expect("应该带结构化错误内容");
        let message = structured["message"].as_str().unwrap_or_default();
        let hint = structured["hint"].as_str().unwrap_or_default();
        assert!(
            hint.contains("dowse index"),
            "hint 字段应带建库指引: {structured:#?}"
        );
        assert!(!message.is_empty());
    }

    #[tokio::test]
    async fn preview_rejects_empty_path() {
        let server = DowseMcpServer::new(PathBuf::from("does-not-matter"));
        let err = server
            .preview(Parameters(PreviewParams {
                path: "".to_owned(),
                query: "笔记".to_owned(),
            }))
            .await
            .expect_err("空 path 应该报参数错误");
        assert!(err.message.contains("path"));
    }

    #[tokio::test]
    async fn preview_on_missing_index_returns_tool_level_error_with_build_hint() {
        let tmp = tempfile::tempdir().unwrap();
        let missing_index = tmp.path().join("no-such-index");
        let server = DowseMcpServer::new(missing_index);

        let result = server
            .preview(Parameters(PreviewParams {
                path: "C:\\somewhere\\note.md".to_owned(),
                query: "笔记".to_owned(),
            }))
            .await
            .expect("索引不存在不应该让 tools/call 本身失败，而是回一个 isError:true 的结果");
        assert_eq!(result.is_error, Some(true));
        let structured = result.structured_content.expect("应该带结构化错误内容");
        let hint = structured["hint"].as_str().unwrap_or_default();
        assert!(
            hint.contains("dowse index"),
            "hint 字段应带建库指引: {structured:#?}"
        );
    }

    #[tokio::test]
    async fn index_status_on_missing_index_returns_tool_level_error_with_build_hint() {
        let tmp = tempfile::tempdir().unwrap();
        let missing_index = tmp.path().join("no-such-index");
        let server = DowseMcpServer::new(missing_index);

        let result = server
            .index_status()
            .await
            .expect("索引不存在不应该让 tools/call 本身失败，而是回一个 isError:true 的结果");
        assert_eq!(result.is_error, Some(true));
        let structured = result.structured_content.expect("应该带结构化错误内容");
        let hint = structured["hint"].as_str().unwrap_or_default();
        assert!(
            hint.contains("dowse index"),
            "hint 字段应带建库指引: {structured:#?}"
        );
    }

    #[tokio::test]
    async fn search_and_preview_round_trip_on_real_index() {
        let index_dir = tempfile::tempdir().unwrap();
        let target_dir = tempfile::Builder::new()
            .prefix("dowse-mcp-test-")
            .tempdir()
            .unwrap();
        std::fs::write(
            target_dir.path().join("note.md"),
            "系统采用分布式限流器保护后端服务。",
        )
        .unwrap();
        dowse_core::rebuild_index(index_dir.path(), target_dir.path()).unwrap();

        let server = DowseMcpServer::new(index_dir.path().to_path_buf());

        let search_result = server
            .search(Parameters(SearchParams {
                query: "限流器".to_owned(),
                limit: None,
                ext: None,
            }))
            .await
            .expect("索引存在，search 不应该报错");
        assert_ne!(search_result.is_error, Some(true));
        let search_out: SearchOutput = search_result
            .into_typed()
            .expect("search 的 structured_content 应该能解回 SearchOutput");
        assert_eq!(search_out.hits.len(), 1);
        assert!(search_out.hits[0].snippet.contains(HL_OPEN));
        assert_eq!(search_out.hits[0].kind, "md");
        assert_eq!(search_out.total_docs, 1);

        let path = search_out.hits[0].path.clone();
        let preview_result = server
            .preview(Parameters(PreviewParams {
                path,
                query: "限流器".to_owned(),
            }))
            .await
            .expect("文件存在，preview 不应该报错");
        assert_ne!(preview_result.is_error, Some(true));
        let preview_out: PreviewOutput = preview_result
            .into_typed()
            .expect("preview 的 structured_content 应该能解回 PreviewOutput");
        assert!(preview_out.snippet.contains(HL_OPEN));
        assert_eq!(preview_out.kind, "md");
        assert!(preview_out.size > 0);

        let status_result = server
            .index_status()
            .await
            .expect("索引存在，index_status 不应该报错");
        assert_ne!(status_result.is_error, Some(true));
        let status_out: IndexStatusOutput = status_result
            .into_typed()
            .expect("index_status 的 structured_content 应该能解回 IndexStatusOutput");
        assert_eq!(status_out.num_docs, 1);
        assert_eq!(status_out.roots.len(), 1);
        assert!(status_out.disk_size_bytes > 0);
        assert!(status_out.last_updated_unix_ms.is_some());
    }
}
