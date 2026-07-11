mod extract;
mod indexer;
mod searcher;

pub use indexer::{rebuild_index, IndexStats};
pub use searcher::{SearchHit, Searcher};

use tantivy::schema::{
    IndexRecordOption, Schema, SchemaBuilder, TextFieldIndexing, TextOptions, STORED, STRING,
};

/// 索引里每篇文档的四个字段。
/// tantivy 的 Field 只是个轻量句柄（本质是个编号），到处复制无所谓。
pub(crate) struct Fields {
    pub path: tantivy::schema::Field,
    pub name: tantivy::schema::Field,
    pub ext: tantivy::schema::Field,
    pub content: tantivy::schema::Field,
}

/// 定义 schema：path/ext 原样存（不分词），name/content 走 jieba 分词。
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
