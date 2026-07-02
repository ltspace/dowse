mod events;
mod ext_groups;
mod extract;
mod indexer;
mod meta;
mod reconcile;
mod searcher;
mod status;
mod updater;
mod watch;

pub use events::{Debouncer, PendingChange, PendingOp, QUIET_WINDOW_MS, WATER_LEVEL, WatchEvent};
pub use ext_groups::by_name as ext_group_by_name;
pub use indexer::{IndexStats, rebuild_index};
pub use meta::registered_roots;
pub use reconcile::{ReconcileStats, reconcile};
pub use searcher::{PreviewHit, SearchHit, Searcher, SortMode};
pub use status::{IndexStatus, index_status};
pub use updater::{BatchOutcome, IndexUpdater};
pub use watch::{EventSource, NotifyEventSource, WatchGuard, WatchProgress, run_watch};

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
pub(crate) struct Fields {
    pub path: tantivy::schema::Field,
    pub name: tantivy::schema::Field,
    pub ext: tantivy::schema::Field,
    pub content: tantivy::schema::Field,
    pub mtime: tantivy::schema::Field,
    pub size: tantivy::schema::Field,
    pub kind: tantivy::schema::Field,
}

/// 定义 schema：path/ext/kind 原样存（不分词），name/content 走 jieba 分词，
/// mtime/size 是 FAST + STORED 的数值字段——FAST 供排序器用列式存储直接扫，
/// STORED 供对账遍历读回来比对、也顺手给搜索结果展示用。
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
        mtime: builder.add_i64_field("mtime", STORED | FAST),
        size: builder.add_u64_field("size", STORED | FAST),
        kind: builder.add_text_field("kind", STRING | STORED),
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
