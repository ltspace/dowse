use std::ops::Range;
use std::path::Path;

use anyhow::{Context, Result};
use tantivy::collector::TopDocs;
use tantivy::query::QueryParser;
use tantivy::schema::Value;
use tantivy::snippet::SnippetGenerator;
use tantivy::{Index, IndexReader, TantivyDocument};

use crate::{build_schema, register_tokenizers, Fields};

pub struct SearchHit {
    pub path: String,
    pub score: f32,
    /// 命中上下文片段，命中词的字节区间在 highlighted 里，渲染交给调用方
    pub snippet: String,
    pub highlighted: Vec<Range<usize>>,
}

pub struct Searcher {
    reader: IndexReader,
    parser: QueryParser,
    fields: Fields,
}

impl Searcher {
    pub fn open(index_dir: &Path) -> Result<Self> {
        let index = Index::open_in_dir(index_dir)
            .context("打不开索引目录，先跑 `dowse index <目录>` 建一次索引")?;
        register_tokenizers(&index);

        let (_, fields) = build_schema();
        // 不带字段前缀的查询词，默认同时查文件名和正文
        let parser = QueryParser::for_index(&index, vec![fields.name, fields.content]);
        let reader = index.reader()?;

        Ok(Self {
            reader,
            parser,
            fields,
        })
    }

    pub fn search(&self, query_str: &str, limit: usize) -> Result<Vec<SearchHit>> {
        let query = self.parser.parse_query(query_str)?;
        let searcher = self.reader.searcher();

        let mut snippet_gen = SnippetGenerator::create(&searcher, &query, self.fields.content)?;
        snippet_gen.set_max_num_chars(160);

        let top_docs = searcher.search(&query, &TopDocs::with_limit(limit).order_by_score())?;

        let mut hits = Vec::with_capacity(top_docs.len());
        for (score, addr) in top_docs {
            let doc: TantivyDocument = searcher.doc(addr)?;
            let path = doc
                .get_first(self.fields.path)
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_owned();

            let snippet = snippet_gen.snippet_from_doc(&doc);
            hits.push(SearchHit {
                path,
                score,
                snippet: snippet.fragment().to_owned(),
                highlighted: snippet.highlighted().to_vec(),
            });
        }
        Ok(hits)
    }

    /// 索引里的文档总数，给 CLI 的状态输出用
    pub fn num_docs(&self) -> u64 {
        self.reader.searcher().num_docs()
    }
}
