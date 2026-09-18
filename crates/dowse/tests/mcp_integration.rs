//! `dowse mcp` 的集成测试：真的起一个子进程跑编译好的 dowse 二进制，
//! 通过 stdio 走一遍 MCP 握手（initialize → tools/list → tools/call），
//! 用官方 rmcp client SDK 当测试客户端——这样握手细节（协议版本协商、
//! JSON-RPC 帧格式）交给 SDK 自己保证一致，不用我们手搓协议帧。
//!
//! 用 `DOWSE_INDEX_DIR` 环境变量把子进程指向一个 tempdir 索引，不碰
//! 用户机器上 `%LOCALAPPDATA%\dowse\index` 那份真索引。

use rmcp::ServiceExt;
use rmcp::model::CallToolRequestParams;
use rmcp::transport::{ConfigureCommandExt, TokioChildProcess};
use tokio::process::Command;

/// CI 默认测本次 cargo 构建的二进制；本机安装验收可用环境变量指向实际安装产物。
fn mcp_binary() -> std::ffi::OsString {
    std::env::var_os("DOWSE_MCP_BIN")
        .unwrap_or_else(|| std::ffi::OsString::from(env!("CARGO_BIN_EXE_dowse")))
}

/// 建一个只有一篇文档的小索引，供子进程查询。
fn build_test_index() -> (tempfile::TempDir, tempfile::TempDir) {
    let index_dir = tempfile::tempdir().expect("创建索引临时目录失败");
    let target_dir = tempfile::Builder::new()
        .prefix("dowse-mcp-it-")
        .tempdir()
        .expect("创建被索引临时目录失败");

    std::fs::write(
        target_dir.path().join("note.md"),
        "系统采用分布式限流器保护后端服务。",
    )
    .expect("写测试文档失败");

    dowse::rebuild_index(index_dir.path(), target_dir.path()).expect("建索引失败");

    (index_dir, target_dir)
}

#[tokio::test]
async fn stdio_handshake_lists_tools_and_search_returns_structured_hit() {
    let (index_dir, _target_dir) = build_test_index();

    let index_dir_arg = index_dir.path().to_string_lossy().into_owned();

    let client = ()
        .serve(
            TokioChildProcess::new(Command::new(mcp_binary()).configure(|cmd| {
                cmd.arg("mcp").env("DOWSE_INDEX_DIR", &index_dir_arg);
            }))
            .expect("构造子进程 transport 失败"),
        )
        .await
        .expect("MCP initialize 握手失败");

    let server_info = client.peer_info();
    println!("[handshake] initialize 返回的 server info: {server_info:#?}");
    let implementation = server_info
        .as_ref()
        .and_then(|info| info.server_info.as_ref())
        .expect("initialize 应返回 serverInfo");
    assert_eq!(implementation.name, "dowse");
    assert_eq!(implementation.version, env!("CARGO_PKG_VERSION"));

    // tools/list：四个只读工具都应该在，且不应该有任何变更类工具。
    let tools = client
        .list_tools(Default::default())
        .await
        .expect("tools/list 失败");
    println!("[handshake] tools/list 返回: {tools:#?}");

    let names: Vec<String> = tools.tools.iter().map(|t| t.name.to_string()).collect();
    assert!(
        names.contains(&"search".to_string()),
        "缺 search 工具: {names:?}"
    );
    assert!(
        names.contains(&"preview".to_string()),
        "缺 preview 工具: {names:?}"
    );
    assert!(
        names.contains(&"read_file_chunk".to_string()),
        "缺 read_file_chunk 工具: {names:?}"
    );
    assert!(
        names.contains(&"index_status".to_string()),
        "缺 index_status 工具: {names:?}"
    );
    assert_eq!(names.len(), 4, "工具清单应该刻意少，只有这四个: {names:?}");
    assert!(
        tools.tools.iter().all(|tool| tool.output_schema.is_some()),
        "所有工具都应该声明 outputSchema: {:#?}",
        tools.tools
    );
    let search_tool = tools
        .tools
        .iter()
        .find(|tool| tool.name == "search")
        .unwrap();
    assert_eq!(
        search_tool.input_schema["properties"]["limit"]["minimum"],
        1
    );
    assert_eq!(
        search_tool.input_schema["properties"]["limit"]["maximum"],
        100
    );
    assert_eq!(
        search_tool.input_schema["properties"]["offset"]["maximum"],
        10000
    );
    let read_tool = tools
        .tools
        .iter()
        .find(|tool| tool.name == "read_file_chunk")
        .unwrap();
    assert_eq!(
        read_tool.input_schema["properties"]["max_chars"]["maximum"],
        8000
    );

    // tools/call search：真查一次，断言返回是结构化 JSON，snippet 带高亮标记。
    let result = client
        .call_tool(
            CallToolRequestParams::new("search").with_arguments(
                serde_json::json!({"query": "限流器"})
                    .as_object()
                    .unwrap()
                    .clone(),
            ),
        )
        .await
        .expect("tools/call search 失败");
    println!("[handshake] tools/call search 返回: {result:#?}");

    assert_ne!(
        result.is_error,
        Some(true),
        "search 不应该报错: {result:#?}"
    );
    let structured = result
        .structured_content
        .expect("search 应该把结果放进 structured_content");
    let hits = structured["hits"].as_array().expect("hits 应该是数组");
    assert_eq!(hits.len(), 1, "应该命中刚建的那篇文档: {structured:#?}");
    assert!(
        hits[0]["path"].as_str().unwrap().ends_with("note.md"),
        "命中路径应该是 note.md: {structured:#?}"
    );
    assert!(
        hits[0]["snippet"].as_str().unwrap().contains('«'),
        "snippet 应该带 «» 高亮标记: {structured:#?}"
    );
    assert_eq!(structured["total_docs"], 1);

    let path = hits[0]["path"].as_str().unwrap().to_owned();
    let chunk = client
        .call_tool(
            CallToolRequestParams::new("read_file_chunk").with_arguments(
                serde_json::json!({"path": path, "max_chars": 5})
                    .as_object()
                    .unwrap()
                    .clone(),
            ),
        )
        .await
        .expect("tools/call read_file_chunk 失败");
    assert_ne!(chunk.is_error, Some(true));
    assert_eq!(chunk.structured_content.unwrap()["returned_chars"], 5);

    client.cancel().await.expect("关闭 MCP client 失败");
}

#[tokio::test]
async fn tools_call_index_status_on_missing_index_returns_tool_level_error_with_build_hint() {
    // 不建索引，直接指一个空目录：应该拿到 isError:true 的结构化错误，
    // 而不是子进程崩溃或协议层报错——见 docs/DESIGN-M5-MCP.md 第五节验收清单第 4 条。
    let empty_dir = tempfile::tempdir().expect("创建空临时目录失败");

    let index_dir_arg = empty_dir.path().to_string_lossy().into_owned();

    let client = ()
        .serve(
            TokioChildProcess::new(Command::new(mcp_binary()).configure(|cmd| {
                cmd.arg("mcp").env("DOWSE_INDEX_DIR", &index_dir_arg);
            }))
            .expect("构造子进程 transport 失败"),
        )
        .await
        .expect("MCP initialize 握手失败");

    let result = client
        .call_tool(
            CallToolRequestParams::new("index_status")
                .with_arguments(serde_json::json!({}).as_object().unwrap().clone()),
        )
        .await
        .expect("tools/call 本身不应该失败——索引缺失是工具级错误，不是协议级错误");
    println!("[handshake] index_status 在缺索引时的返回: {result:#?}");

    // 索引不可用是"工具跑了但没跑成"，走 isError:true 的 CallToolResult，
    // 不是拒绝整个 tools/call 请求——这样 agent 能读到具体错误文案和建库指引。
    assert_eq!(result.is_error, Some(true), "应该是工具级错误: {result:#?}");
    let structured = result.structured_content.expect("应该带结构化错误内容");
    let hint = structured["hint"].as_str().unwrap_or_default();
    assert!(
        hint.contains("dowse index"),
        "hint 字段应带建库指引: {structured:#?}"
    );

    client.cancel().await.expect("关闭 MCP client 失败");
}
