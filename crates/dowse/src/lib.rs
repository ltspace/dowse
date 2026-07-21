//! Windows 本地全文搜索引擎的核心库。
//!
//! 索引层基于 tantivy 倒排索引，内容分词用 `tokenizer` 模块把文本按"汉字/
//! 非汉字"切段：汉字段接 jieba 按中文习惯分词，非汉字段按字母数字切词并统一
//! 小写。`extract` 模块负责文本抽取——纯文本用 chardetng/encoding_rs 探测
//! 编码，另外支持 PDF、Office（docx/xlsx/pptx）等常见格式。`ocr`/`ocr_worker`
//! 模块接入 Windows 系统自带的 OCR 引擎，让截图和图片里的文字也能被搜到
//! （仅 Windows；其余平台是诚实报"不可用"的桩实现）。`mft`/`usn` 模块在
//! NTFS 卷 + 管理员权限下走 MFT 快速枚举和 USN Journal 增量监听，跳过全盘
//! 目录遍历；拿不到这个前提条件就诚实降级到基于 walkdir + notify 的目录
//! 遍历/文件监听，两条路径产出的索引结果完全一致，调用方感知不到差别。
//!
//! docs.rs 在 Linux 构建机上生成本页文档，`cfg(windows)` 之后的 API（OCR、
//! NTFS MFT/USN 快速索引）不会出现在这里；要看到完整 API，在 Windows 上本地
//! 跑 `cargo doc` 即可。
//!
//! # 核心入口
//!
//! - 建索引：[`rebuild_index`] 全量重建；多根索引里"再加一个文件夹"用
//!   [`index_root_incremental`]（只补扫新根，不动其它根，跟全量重建同一套 MFT
//!   快车道 / [`IndexStats`] 口径）；根的增删见 [`add_root`]/[`remove_root`]。
//! - 搜索：[`Searcher::open`] 打开一个索引的只读句柄，[`Searcher::search`]
//!   执行查询（`search_filtered`/`search_advanced` 支持扩展名过滤和排序）。
//! - 实时监听：[`watch_roots_auto`] 按卷能力自动选快车道或慢车道，持续把
//!   文件系统变化落进索引。
//! - 启动对账：[`reconcile`]，追平程序未运行期间发生的文件变化。
//! - OCR：[`drain_ocr_queue`] 一次性处理完当前排队的图片；
//!   [`OcrPipeline::start`] 启动后台 worker 池持续处理新入队的图片。
//!
//! # 示例
//!
//! 建一次索引，然后在"文档类"扩展名里搜索、按修改时间从新到旧排，最后读出
//! 每条命中的字段：
//!
//! ```no_run
//! # fn main() -> anyhow::Result<()> {
//! use std::path::Path;
//! use dowse::{Searcher, SortMode, display_path, ext_group_by_name, rebuild_index};
//!
//! let index_dir = Path::new("./my-index");
//! let target_dir = Path::new("./my-documents");
//!
//! // 全量建一次索引，拿到收录 / 跳过 / 耗时统计。
//! let stats = rebuild_index(index_dir, target_dir)?;
//! println!(
//!     "收录 {} 个文件，跳过 {}，耗时 {:.1}s",
//!     stats.indexed, stats.skipped, stats.seconds
//! );
//!
//! // 打开只读句柄开始搜索。
//! let searcher = Searcher::open(index_dir)?;
//!
//! // 只在文档类扩展名（md/txt/pdf/docx…）里搜，按修改时间从新到旧排。
//! let hits = searcher.search_advanced(
//!     "季度 报告",
//!     20,
//!     ext_group_by_name(Some("doc")),
//!     SortMode::MtimeDesc,
//! )?;
//!
//! for hit in &hits {
//!     // display_path 剥掉 Windows 的 `\\?\` 扩展长度前缀，只用于展示。
//!     println!(
//!         "[{}] {} —— {} 处高亮",
//!         hit.ext,
//!         display_path(&hit.path),
//!         hit.highlighted.len()
//!     );
//!     println!("{}", hit.snippet);
//! }
//! # Ok(())
//! # }
//! ```
#![warn(missing_docs)]

mod cursor;
mod events;
mod ext_groups;
mod extract;
mod frn_table;
mod indexer;
mod meta;
#[cfg(windows)]
mod mft;
mod ocr;
mod ocr_queue;
mod ocr_worker;
mod query;
mod reconcile;
mod roots;
mod rules;
mod searcher;
mod status;
mod tokenizer;
mod updater;
#[cfg(windows)]
mod usn;
mod usn_translate;
mod volume;
mod watch;

pub use events::{Debouncer, PendingChange, PendingOp, WatchEvent};
pub use ext_groups::by_name as ext_group_by_name;
pub use indexer::{
    IndexProgress, IndexStats, rebuild_index, rebuild_index_with_progress, remove_dir_all_retrying,
};
pub use meta::registered_roots;
pub use ocr::is_available;
pub use ocr_queue::OcrQueue;
pub use ocr_worker::{DEFAULT_WORKERS, OcrDrainStats, OcrPipeline, drain_ocr_queue};
pub use reconcile::{ReconcileStats, reconcile, reconcile_orphans};
pub use roots::{
    AddRootStats, RemoveRootStats, add_root, add_root_with_progress, index_root_incremental,
    index_root_incremental_with_progress, rebuild_root, rebuild_root_with_progress, remove_root,
};
pub use rules::{IndexRules, load_rules, save_rules};
pub use searcher::{PreviewHit, SearchHit, SearchPage, Searcher, SortMode, normalize_ranges};
pub use status::{IndexStatus, index_status};
pub use updater::{BatchOutcome, IndexUpdater};
pub use volume::ntfs_fast_path_available;
pub use watch::{
    EventSource, NotifyEventSource, WatchGuard, WatchProgress, run_watch, watch_roots_auto,
};

use tantivy::schema::{
    FAST, IndexRecordOption, STORED, STRING, Schema, SchemaBuilder, TextFieldIndexing, TextOptions,
};

/// 索引里每篇文档的字段。
/// tantivy 的 Field 只是个轻量句柄（本质是个编号），到处复制无所谓。
///
/// mtime/size 是里程碑 3 新加的数值字段，本轮（v0.5.0）补上 FAST 属性——
/// 浮窗的"排序器"要用 tantivy 的 `TopDocs::order_by_fast_field` 系列 API，
/// 这要求字段是 fast field，STORED 不够。kind 是给里程碑 4 OCR 预留的字段，
/// 本版一律写 "text"，图片管线接入后会写 "image"，让筛选/展示逻辑将来
/// 不需要再摸一遍全部索引。三个字段变化叠在一起，schema 版本从里程碑 3 的
/// v2 一次性升到 v3（见 meta.rs），只需要用户重建一次索引就把两个未来都接上。
///
/// `path_text` 是查询语法升级（schema v5）新加的字段：查询串里的 `path:关键词`
/// 要在**查询层**按路径子串命中（好让匹配总数、翻页都准确，不做拿到结果再后过滤
/// 的近似），而原有的 `path` 是 STRING（整条路径当**单个不分词的 term** 存），
/// 只能精确/前缀匹配，没法命中路径中段的目录名。于是单独加一个跟 name/content
/// 同款 jieba 分词、但**不落 STORED**（检索用，取回展示仍读 `path`）的镜像字段，
/// 让路径按与正文一致的词/子词粒度可搜。加字段是不兼容变更，schema 从 v4 升到 v5。
pub(crate) struct Fields {
    pub path: tantivy::schema::Field,
    pub name: tantivy::schema::Field,
    pub ext: tantivy::schema::Field,
    pub content: tantivy::schema::Field,
    pub mtime: tantivy::schema::Field,
    pub size: tantivy::schema::Field,
    pub kind: tantivy::schema::Field,
    /// 路径的分词镜像（jieba 分词、INDEXED 不 STORED），专供 `path:` 查询按
    /// 路径子串命中；取回展示仍用 `path`。
    pub path_text: tantivy::schema::Field,
}

/// 定义 schema：path/ext/kind 原样存（不分词），name/content 走 jieba 分词，
/// mtime/size 是 FAST + STORED 的数值字段——FAST 供排序器用列式存储直接扫，
/// STORED 供对账遍历读回来比对、也顺手给搜索结果展示用。
/// content 必须 STORED，否则搜索命中后没有原文可做摘要高亮。
/// path_text 是 path 的 jieba 分词镜像，只 INDEXED 不 STORED——它只服务
/// `path:` 查询的路径子串匹配，取回展示照旧读 STORED 的 path，没必要再存一份。
pub(crate) fn build_schema() -> (Schema, Fields) {
    let mut builder: SchemaBuilder = Schema::builder();

    let jieba_indexing = TextFieldIndexing::default()
        .set_tokenizer("jieba")
        .set_index_option(IndexRecordOption::WithFreqsAndPositions);
    // name/content 要分词 + STORED（命中后取原文做摘要高亮）；path_text 同一套
    // 分词但只 INDEXED 不 STORED（见上：取回展示读 path）。共用同一份 indexing 配置。
    let jieba_text = TextOptions::default()
        .set_indexing_options(jieba_indexing.clone())
        .set_stored();
    let jieba_indexed_only = TextOptions::default().set_indexing_options(jieba_indexing);

    let fields = Fields {
        path: builder.add_text_field("path", STRING | STORED),
        name: builder.add_text_field("name", jieba_text.clone()),
        ext: builder.add_text_field("ext", STRING | STORED),
        content: builder.add_text_field("content", jieba_text),
        mtime: builder.add_i64_field("mtime", STORED | FAST),
        size: builder.add_u64_field("size", STORED | FAST),
        kind: builder.add_text_field("kind", STRING | STORED),
        path_text: builder.add_text_field("path_text", jieba_indexed_only),
    };
    (builder.build(), fields)
}

/// 索引和查询两侧都要注册同一个分词器：
/// tantivy 只把分词器名字("jieba")写进 schema，实现是运行时挂载的。
pub(crate) fn register_tokenizers(index: &tantivy::Index) {
    index
        .tokenizers()
        .register("jieba", tokenizer::MixedTokenizer::new());
}

/// 剥掉 Windows 扩展长度路径语法（`\\?\`/`\\?\UNC\`）的前缀，只给**展示层**用。
///
/// `PathBuf::canonicalize()` 在 Windows 上返回的路径天生带这个前缀——这是
/// Rust 标准库刻意保留的行为，为的是让后续的文件 I/O（打开、监听、在
/// 资源管理器里定位）自动绕开 Win32 的 `MAX_PATH`（260 字符）限制，对深层
/// 路径也能正常工作。索引里存的 `path` 字段就是 canonicalize 之后的原样值，
/// 所以搜索结果、预览区拿到的路径字符串都带着这个前缀——直接渲染给用户看
/// 会露出 `\\?\E:\...` 这种内部实现细节。
///
/// 这个函数只用来生成给用户看的文本；真正参与文件操作（`open_file`/
/// `reveal_in_folder`）的路径必须继续用没剥过的原始值，否则长路径场景会
/// 重新触发 `MAX_PATH` 限制。
///
/// # Examples
///
/// ```
/// use dowse::display_path;
///
/// // 普通 `\\?\` 前缀被剥掉。
/// assert_eq!(display_path(r"\\?\E:\notes\a.md"), r"E:\notes\a.md");
/// // UNC 形式还原成 `\\server\share\...`。
/// assert_eq!(display_path(r"\\?\UNC\server\share\a.txt"), r"\\server\share\a.txt");
/// // 没有前缀的路径原样返回。
/// assert_eq!(display_path(r"E:\notes\a.md"), r"E:\notes\a.md");
/// ```
pub fn display_path(path: &str) -> String {
    if let Some(rest) = path.strip_prefix(r"\\?\UNC\") {
        format!(r"\\{rest}")
    } else if let Some(rest) = path.strip_prefix(r"\\?\") {
        rest.to_string()
    } else {
        path.to_string()
    }
}

#[cfg(test)]
mod display_path_tests {
    use super::display_path;

    #[test]
    fn strips_plain_verbatim_prefix() {
        assert_eq!(display_path(r"\\?\E:\BLOG\post.md"), r"E:\BLOG\post.md");
    }

    #[test]
    fn strips_unc_verbatim_prefix() {
        assert_eq!(
            display_path(r"\\?\UNC\server\share\file.txt"),
            r"\\server\share\file.txt"
        );
    }

    #[test]
    fn leaves_normal_windows_path_untouched() {
        assert_eq!(display_path(r"E:\BLOG\post.md"), r"E:\BLOG\post.md");
    }

    #[test]
    fn leaves_unix_style_path_untouched() {
        assert_eq!(display_path("/home/user/file.txt"), "/home/user/file.txt");
    }

    #[test]
    fn leaves_empty_string_untouched() {
        assert_eq!(display_path(""), "");
    }
}
