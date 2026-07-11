mod events;
mod extract;
mod indexer;
mod meta;
mod reconcile;
mod searcher;
mod status;
mod updater;
mod watch;

pub use events::{Debouncer, PendingChange, PendingOp, QUIET_WINDOW_MS, WATER_LEVEL, WatchEvent};
pub use indexer::{IndexStats, rebuild_index};
pub use meta::registered_roots;
pub use reconcile::{ReconcileStats, reconcile};
pub use searcher::{PreviewHit, SearchHit, Searcher};
pub use status::{IndexStatus, index_status};
pub use updater::{BatchOutcome, IndexUpdater};
pub use watch::{EventSource, NotifyEventSource, WatchGuard, WatchProgress, run_watch};

use tantivy::schema::{
    IndexRecordOption, STORED, STRING, Schema, SchemaBuilder, TextFieldIndexing, TextOptions,
};

/// 索引里每篇文档的字段。
/// tantivy 的 Field 只是个轻量句柄（本质是个编号），到处复制无所谓。
///
/// mtime/size 是里程碑 3 新加的 STORED 数值字段：既给启动对账做 (path, mtime, size)
/// 三元组比对，也顺手给搜索结果展示用。加了字段就是不兼容变更，schema 版本随之从
/// 里程碑 1 的隐式 v1 升到 v2（见 meta.rs）。
pub(crate) struct Fields {
    pub path: tantivy::schema::Field,
    pub name: tantivy::schema::Field,
    pub ext: tantivy::schema::Field,
    pub content: tantivy::schema::Field,
    pub mtime: tantivy::schema::Field,
    pub size: tantivy::schema::Field,
}

/// 定义 schema：path/ext 原样存（不分词），name/content 走 jieba 分词，
/// mtime/size 只存不索引（对账遍历时读回来比对）。
/// content 必须 STORED，否则搜索命中后没有原文可做摘要高亮。
pub(crate) fn build_schema() -> (Schema, Fields) {
    let mut builder: SchemaBuilder = Schema::builder();

    let jieba_indexing = TextFieldIndexing::default()
        .set_tokenizer("jieba")
        .set_index_option(IndexRecordOption::WithFreqsAndPositions);
    let jieba_text = TextOptions::default()
        .set_indexing_options(jieba_indexing)
        .set_stored();

    let fields = Fields {
        path: builder.add_text_field("path", STRING | STORED),
        name: builder.add_text_field("name", jieba_text.clone()),
        ext: builder.add_text_field("ext", STRING | STORED),
        content: builder.add_text_field("content", jieba_text),
        mtime: builder.add_i64_field("mtime", STORED),
        size: builder.add_u64_field("size", STORED),
    };
    (builder.build(), fields)
}

/// 索引和查询两侧都要注册同一个分词器：
/// tantivy 只把分词器名字("jieba")写进 schema，实现是运行时挂载的。
pub(crate) fn register_tokenizers(index: &tantivy::Index) {
    index
        .tokenizers()
        .register("jieba", tantivy_jieba::JiebaTokenizer::new());
}
