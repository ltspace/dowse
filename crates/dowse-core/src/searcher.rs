use std::ops::Range;
use std::path::Path;

use anyhow::{Context, Result};
use tantivy::collector::TopDocs;
use tantivy::query::{BooleanQuery, Occur, Query, QueryParser, TermQuery};
use tantivy::schema::{IndexRecordOption, Value};
use tantivy::snippet::SnippetGenerator;
use tantivy::{Index, IndexReader, TantivyDocument, Term};

use crate::{build_schema, register_tokenizers, Fields};

/// 预览窗口目标字符数：命中词前后共约 1500 字，比搜索结果列表里的摘要长得多。
const PREVIEW_MAX_CHARS: usize = 1500;

pub struct SearchHit {
    pub path: String,
    pub score: f32,
    /// 命中上下文片段，命中词的字节区间在 highlighted 里，渲染交给调用方
    pub snippet: String,
    /// 命中词的字节区间：已按起点排序且互不重叠，可直接顺序渲染。
    pub highlighted: Vec<Range<usize>>,
}

/// 按路径取的预览内容，字段契约和 SearchHit 的 snippet/highlighted 一致，
/// 只是窗口更大（约 1500 字而不是摘要用的 160 字）。
pub struct PreviewHit {
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
        // schema 版本对不上（旧索引缺 mtime/size 字段）直接报错、提示重建，
        // 不拿旧字段布局硬搜——搜出来的结果不可靠。
        crate::meta::ensure_schema_version(index_dir)?;
        register_tokenizers(&index);

        let (_, fields) = build_schema();
        // 不带字段前缀的查询词，默认同时查文件名和正文
        let mut parser = QueryParser::for_index(&index, vec![fields.name, fields.content]);
        // tantivy 默认词间是 OR，浮窗场景下多词应该收窄结果而不是放宽——
        // 用户敲"限流 中间件"是想要同时含两个词的文档，不是含任意一个词都行。
        // 带引号的短语查询不受影响，QueryParser 会先切出短语再对词间加 AND。
        parser.set_conjunction_by_default();
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

    /// 按路径取该文档更长的预览上下文（约 1500 字），命中词区间跟 search() 同一契约。
    /// 用于浮窗右侧预览区：用户在结果列表里选中一行后，用它的 path 和当前查询词
    /// 换一份比列表摘要（160 字）长得多的窗口。
    ///
    /// path 找不到、或者该文档已不在索引里（比如原文件被删除后索引还没重建），返回 None。
    pub fn preview(&self, path: &str, query_str: &str) -> Result<Option<PreviewHit>> {
        let searcher = self.reader.searcher();
        let path_term = Term::from_field_text(self.fields.path, path);
        let path_query = TermQuery::new(path_term, IndexRecordOption::Basic);

        // path 精确匹配 AND 用户查询词：既锁定这一篇文档，又让 SnippetGenerator
        // 拿到查询词去定位高亮位置。
        let user_query = self.parser.parse_query(query_str)?;
        let combined = BooleanQuery::new(vec![
            (Occur::Must, Box::new(path_query.clone()) as Box<dyn Query>),
            (Occur::Must, user_query),
        ]);

        let top_docs = searcher.search(&combined, &TopDocs::with_limit(1).order_by_score())?;
        if let Some((_, addr)) = top_docs.into_iter().next() {
            let mut snippet_gen = SnippetGenerator::create(&searcher, &combined, self.fields.content)?;
            snippet_gen.set_max_num_chars(PREVIEW_MAX_CHARS);

            let doc: TantivyDocument = searcher.doc(addr)?;
            let snippet = snippet_gen.snippet_from_doc(&doc);
            return Ok(Some(PreviewHit {
                snippet: snippet.fragment().to_owned(),
                highlighted: normalize_ranges(snippet.highlighted().to_vec()),
            }));
        }

        // 查询词和该文档实际不匹配（比如查询语法变了，或者调用方传了不相关的 query_str）：
        // 退回纯路径匹配，给文档开头一段没有高亮的预览，而不是彻底失败。
        let fallback = searcher.search(&path_query, &TopDocs::with_limit(1).order_by_score())?;
        let Some((_, addr)) = fallback.into_iter().next() else {
            return Ok(None);
        };
        let doc: TantivyDocument = searcher.doc(addr)?;
        let content = doc
            .get_first(self.fields.content)
            .and_then(|v| v.as_str())
            .unwrap_or_default();
        let window: String = content.chars().take(PREVIEW_MAX_CHARS).collect();
        Ok(Some(PreviewHit {
            snippet: window,
            highlighted: vec![],
        }))
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

    #[test]
    fn search_multi_word_query_defaults_to_and() -> Result<()> {
        let index_dir = tempfile::tempdir()?;
        let target_dir = tempfile::Builder::new().prefix("dowse-test-").tempdir()?;

        // 两个词都有
        std::fs::write(target_dir.path().join("both.md"), "限流中间件的实现细节")?;
        // 只有一个词
        std::fs::write(target_dir.path().join("one.md"), "限流方案对比笔记")?;

        crate::rebuild_index(index_dir.path(), target_dir.path())?;
        let searcher = Searcher::open(index_dir.path())?;

        // tantivy 默认词间是 OR，这里验证已经被 Searcher::open 显式改成 AND：
        // 两个词都出现的文档才应该命中。
        let hits = searcher.search("限流 中间件", 10)?;
        assert_eq!(hits.len(), 1, "AND 语义下只有同时含两个词的文档命中: {hits:?}", hits = hits.iter().map(|h| &h.path).collect::<Vec<_>>());
        assert!(hits[0].path.ends_with("both.md"));
        Ok(())
    }

    #[test]
    fn preview_returns_window_around_hit_with_normalized_ranges() -> Result<()> {
        let index_dir = tempfile::tempdir()?;
        let target_dir = tempfile::Builder::new().prefix("dowse-test-").tempdir()?;

        // 构造一篇比 1500 字窗口长得多的文档，命中词埋在中间。
        let filler_before = "无关内容。".repeat(400);
        let filler_after = "更多无关内容。".repeat(400);
        let content = format!("{filler_before}这里是分布式限流器的核心实现。{filler_after}");
        let doc_path = target_dir.path().join("long.md");
        std::fs::write(&doc_path, &content)?;

        crate::rebuild_index(index_dir.path(), target_dir.path())?;
        let searcher = Searcher::open(index_dir.path())?;

        let hits = searcher.search("限流器", 10)?;
        assert_eq!(hits.len(), 1);
        let full_path = &hits[0].path;

        let preview = searcher
            .preview(full_path, "限流器")?
            .expect("文档存在，preview 不应为 None");

        assert!(!preview.snippet.is_empty());
        assert!(
            preview.snippet.chars().count() < content.chars().count(),
            "预览窗口应该比全文短——验证确实做了截窗而不是整篇塞回来"
        );
        assert!(!preview.highlighted.is_empty(), "命中词应该被标记出来");
        for w in preview.highlighted.windows(2) {
            assert!(w[0].end <= w[1].start, "区间应互不重叠且按起点排序");
        }
        for r in &preview.highlighted {
            assert!(r.end <= preview.snippet.len());
            assert!(preview.snippet.is_char_boundary(r.start));
            assert!(preview.snippet.is_char_boundary(r.end));
        }
        Ok(())
    }

    #[test]
    fn preview_unknown_path_returns_none() -> Result<()> {
        let index_dir = tempfile::tempdir()?;
        let target_dir = tempfile::Builder::new().prefix("dowse-test-").tempdir()?;
        std::fs::write(target_dir.path().join("note.md"), "随便写点什么")?;
        crate::rebuild_index(index_dir.path(), target_dir.path())?;

        let searcher = Searcher::open(index_dir.path())?;
        let preview = searcher.preview("C:\\不存在\\的路径.md", "什么")?;
        assert!(preview.is_none());
        Ok(())
    }

    #[test]
    fn preview_falls_back_to_head_when_query_does_not_match_doc() -> Result<()> {
        let index_dir = tempfile::tempdir()?;
        let target_dir = tempfile::Builder::new().prefix("dowse-test-").tempdir()?;
        std::fs::write(target_dir.path().join("note.md"), "这篇笔记不含查询词")?;
        crate::rebuild_index(index_dir.path(), target_dir.path())?;

        let searcher = Searcher::open(index_dir.path())?;
        let hits = searcher.search("笔记", 10)?;
        let path = &hits[0].path;

        // 查询词和文档实际内容不匹配（调用方传了跟原查询不一致的词），
        // 应该退回文档开头预览，而不是返回 None 或报错。
        let preview = searcher
            .preview(path, "完全不相关的词汇")?
            .expect("路径存在，应该退回开头预览而不是 None");
        assert!(!preview.snippet.is_empty());
        assert!(preview.highlighted.is_empty(), "回退分支不应该产生假的高亮");
        Ok(())
    }
}
