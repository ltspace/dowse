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
    /// 命中词的字节区间：已按起点排序且互不重叠，可直接顺序渲染。
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
                highlighted: normalize_ranges(snippet.highlighted().to_vec()),
            });
        }
        Ok(hits)
    }

    /// 索引里的文档总数，给 CLI 的状态输出用
    pub fn num_docs(&self) -> u64 {
        self.reader.searcher().num_docs()
    }
}

/// 把 tantivy SnippetGenerator 吐出的命中区间整理成有序且互不重叠的序列。
///
/// jieba 分词器会产出重叠 token（比如"分布式"同时切出"分布"和"分布式"），
/// SnippetGenerator 按 token 逐个给区间，顺序和是否重叠都不保证。
/// 这里按起点排序，再把重叠或相邻（下一个的 start <= 当前的 end）的区间合并，
/// 让调用方可以假设区间有序不重叠、按顺序切片渲染。
fn normalize_ranges(mut ranges: Vec<Range<usize>>) -> Vec<Range<usize>> {
    ranges.sort_by_key(|r| r.start);

    let mut merged: Vec<Range<usize>> = Vec::with_capacity(ranges.len());
    for r in ranges {
        match merged.last_mut() {
            Some(last) if r.start <= last.end => {
                if r.end > last.end {
                    last.end = r.end;
                }
            }
            _ => merged.push(r),
        }
    }
    merged
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_ranges_empty() {
        assert_eq!(normalize_ranges(vec![]), Vec::<Range<usize>>::new());
    }

    #[test]
    fn normalize_ranges_out_of_order() {
        let input = vec![10..13, 0..2, 5..7];
        assert_eq!(normalize_ranges(input), vec![0..2, 5..7, 10..13]);
    }

    #[test]
    fn normalize_ranges_fully_overlapping() {
        // "分布式" 和 "分布" 起点相同，"分布式" 覆盖更长
        let input = vec![0..9, 0..6];
        assert_eq!(normalize_ranges(input), vec![0..9]);
    }

    #[test]
    fn normalize_ranges_partially_overlapping() {
        let input = vec![0..6, 3..9];
        assert_eq!(normalize_ranges(input), vec![0..9]);
    }

    #[test]
    fn normalize_ranges_adjacent() {
        let input = vec![0..3, 3..6];
        assert_eq!(normalize_ranges(input), vec![0..6]);
    }

    #[test]
    fn search_jieba_overlapping_tokens_yields_sorted_nonoverlapping_ranges() -> Result<()> {
        let index_dir = tempfile::tempdir()?;
        // rebuild_index 会整棵跳过以 "." 开头的目录，tempfile 默认给临时目录
        // 起 ".tmpXXXXXX" 这样的名字，所以待索引目录要用不带点前缀的名字。
        let target_dir = tempfile::Builder::new().prefix("dowse-test-").tempdir()?;

        std::fs::write(
            target_dir.path().join("note.md"),
            "系统采用分布式限流器保护后端服务。",
        )?;

        crate::rebuild_index(index_dir.path(), target_dir.path())?;

        let searcher = Searcher::open(index_dir.path())?;
        let hits = searcher.search("分布式", 10)?;

        assert!(!hits.is_empty(), "应该能搜到刚建的文档");
        for hit in &hits {
            for w in hit.highlighted.windows(2) {
                assert!(
                    w[0].end <= w[1].start,
                    "区间应互不重叠且按起点排序: {:?}",
                    hit.highlighted
                );
            }
            for r in &hit.highlighted {
                assert!(r.start <= r.end);
                assert!(r.end <= hit.snippet.len());
            }
        }
        Ok(())
    }
}
