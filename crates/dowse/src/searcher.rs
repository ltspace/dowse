//! 只读搜索入口。[`Searcher::open`] 打开一个索引的只读句柄，
//! [`Searcher::search`]/[`Searcher::search_filtered`]/[`Searcher::search_advanced`]
//! 执行查询并返回 [`SearchHit`]（含 BM25 分数、命中片段、高亮区间），
//! [`SortMode`] 控制按相关性还是按 mtime/size 排序。也提供按路径取更大窗口
//! 预览内容的能力（[`PreviewHit`]）。

use std::ops::{Bound, Range};
use std::path::Path;

use anyhow::{Context, Result, bail};
use tantivy::collector::{Count, TopDocs};
use tantivy::query::{
    AllQuery, BooleanQuery, Occur, PhraseQuery, Query, QueryParser, RangeQuery, TermQuery,
};
use tantivy::schema::{IndexRecordOption, Value};
use tantivy::snippet::SnippetGenerator;
use tantivy::tokenizer::{TokenStream, Tokenizer};
use tantivy::{DocAddress, Index, IndexReader, Order, TantivyDocument, Term};

use crate::query::{self, Cmp};
use crate::{Fields, build_schema, register_tokenizers};

/// 结果排序方式。相关性是默认值——不传排序参数、或者从前端传来的字符串
/// 没解析出已知档位时都落回这里。其余三档对应浮窗"排序器"下拉的三个非默认项，
/// 底层用 tantivy `TopDocs::order_by_fast_field` 系列 API 在 mtime/size 这两个
/// v0.5.0 补了 FAST 属性的字段上排（见 lib.rs 的 build_schema）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SortMode {
    /// 按 BM25 相关性分数排序（默认）。
    #[default]
    Relevance,
    /// 按修改时间降序：最新的排在前面。
    MtimeDesc,
    /// 按修改时间升序：最旧的排在前面。
    MtimeAsc,
    /// 按文件体积降序：最大的排在前面。
    SizeDesc,
}

impl SortMode {
    /// 从字符串解析——浮窗前端和未来的 CLI 参数都用这个入口。未知值/None
    /// 一律落回相关性排序，不报错：排序档位是体验层面的偏好，不值得因为
    /// 一个拼错的字符串让整次搜索失败。
    ///
    /// # Examples
    ///
    /// ```
    /// use dowse::SortMode;
    ///
    /// assert_eq!(SortMode::parse(Some("mtime_desc")), SortMode::MtimeDesc);
    /// assert_eq!(SortMode::parse(Some("size_desc")), SortMode::SizeDesc);
    /// // 未知字符串和 None 都落回默认的相关性排序。
    /// assert_eq!(SortMode::parse(Some("bogus")), SortMode::Relevance);
    /// assert_eq!(SortMode::parse(None), SortMode::Relevance);
    /// ```
    pub fn parse(name: Option<&str>) -> Self {
        match name {
            Some("mtime_desc") => Self::MtimeDesc,
            Some("mtime_asc") => Self::MtimeAsc,
            Some("size_desc") => Self::SizeDesc,
            _ => Self::Relevance,
        }
    }
}

/// 预览窗口目标字符数：命中词前后共约 1500 字，比搜索结果列表里的摘要长得多。
const PREVIEW_MAX_CHARS: usize = 1500;

/// 搜索结果列表里单条摘要的目标字符数（跟 tantivy `SnippetGenerator::set_max_num_chars` 对齐）。
const SNIPPET_MAX_CHARS: usize = 160;

/// 摘要生成的扫描窗口上限（字节）。
///
/// tantivy `SnippetGenerator` 的 `max_num_chars` 只决定“挑多长的片段来展示”，
/// 不决定“喂给分词器扫描的输入范围”——`search_fragments` 内部会把传入的整段
/// 文本从头到尾分词一遍，不会因为已经凑够展示长度就提前退出。如果直接把
/// 整篇 STORED content（可能几 MB）交给它，扫描耗时会随文档体积线性增长；
/// 短语/多词 AND 查询命中巨型文档时，单条摘要可能要花几百毫秒。更麻烦的是
/// BM25 打分偏爱词频更高的大文档，短语/多词查询恰好更容易把这类大文档挤进
/// Top-10，于是这个开销会稳定复现在整页结果上，不是个例。这里在调用
/// `snippet()`/`snippet_from_doc()` 之前手动把扫描输入截到这个字节数以内，
/// 把“分词扫描量”和“最终展示长度”解耦，从根上避免整篇重新分词。
const SNIPPET_SCAN_MAX_BYTES: usize = 128 * 1024;

/// 一条搜索命中。
///
/// [`Searcher::search`] 及其变体返回一批 `SearchHit`。`score` 只有在结果按相关性
/// 排序（[`SortMode::Relevance`]，也是默认排序）时才有意义——按 mtime/size 排序
/// 时它固定是 0.0，不要拿它做展示或二次排序。
pub struct SearchHit {
    /// 命中文件的绝对路径（Windows 上带 `\\?\` 扩展长度前缀，展示前用
    /// [`crate::display_path`] 剥掉）。
    pub path: String,
    /// BM25 相关性分数。只在 `SortMode::Relevance`（默认排序）下有意义；
    /// 按 mtime/size 排序时这里固定是 0.0，不要拿它做展示或二次排序。
    pub score: f32,
    /// 命中上下文片段，命中词的字节区间在 highlighted 里，渲染交给调用方
    pub snippet: String,
    /// 命中词的字节区间：已按起点排序且互不重叠，可直接顺序渲染。
    pub highlighted: Vec<Range<usize>>,
    /// 文件扩展名（不含点，小写），无扩展名时是空串。给 MCP 等消费方标注文件类型用。
    pub ext: String,
}

/// 按路径取的预览内容，字段契约和 SearchHit 的 snippet/highlighted 一致，
/// 只是窗口更大（约 1500 字而不是摘要用的 160 字）。
pub struct PreviewHit {
    /// 命中词周围的预览文本（约 1500 字），命中词区间见 `highlighted`。
    pub snippet: String,
    /// 命中词在 `snippet` 里的字节区间：已按起点排序且互不重叠，可直接顺序渲染。
    pub highlighted: Vec<Range<usize>>,
    /// 原文件体积（字节），来自建索引时存的 size 字段。
    pub size: u64,
    /// 原文件 mtime（毫秒级 Unix 时间戳），来自建索引时存的 mtime 字段。
    pub mtime: i64,
    /// 文件扩展名（不含点，小写），无扩展名时是空串。
    pub ext: String,
}

/// 一页搜索结果：当前页命中列表加上"匹配总数"。
///
/// `hits` 是按 `sort` 排序、再按 `limit`/`offset` 截出的当前页；`total` 是整个查询
/// 匹配的文档数，不受 `limit`/`offset` 截断。分页调用方（MCP 的 search 工具）靠
/// `total` 判断"还有没有下一页"——`offset + hits.len() < total` 就说明后面还有，
/// 否则不必再翻，省得 agent 盲翻空页。
pub struct SearchPage {
    /// 当前页的命中列表，已按 `sort` 排序并按 `limit`/`offset` 截取。
    pub hits: Vec<SearchHit>,
    /// 满足查询的文档总数，不受 `limit`/`offset` 影响。
    pub total: usize,
}

/// 一个索引的只读搜索句柄。
///
/// `Searcher` 只读打开索引，可以和一个并发的写入端（[`crate::IndexUpdater`]、
/// 实时监听、全量重建）共存于同一份索引之上——tantivy 的段文件不可变，读写能
/// 跨线程甚至跨进程并行。代价是 reader 持有的是打开那一刻的段快照，不会自动感知
/// 之后别处提交的新写入；要读到最新一次 commit 的结果，调用方必须显式调用
/// [`Searcher::reload`]。
pub struct Searcher {
    reader: IndexReader,
    parser: QueryParser,
    fields: Fields,
}

impl Searcher {
    /// 打开一个已建好的索引的只读句柄。
    ///
    /// 索引目录不存在、或 schema 版本和当前库对不上（旧索引缺 mtime/size 字段）
    /// 时返回 `Err`，错误信息会提示先重建索引。
    ///
    /// # Examples
    ///
    /// ```no_run
    /// # fn main() -> anyhow::Result<()> {
    /// use std::path::Path;
    /// use dowse::Searcher;
    ///
    /// let searcher = Searcher::open(Path::new("./my-index"))?;
    /// for hit in searcher.search("关键词", 20)? {
    ///     println!("{}\t{}", hit.path, hit.snippet);
    /// }
    /// # Ok(())
    /// # }
    /// ```
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

    /// 执行一次全文搜索，返回至多 `limit` 条命中。
    ///
    /// 默认同时查 `name`（文件名）和 `content`（正文）两个字段。查询串里的多个词
    /// **默认按 AND 合取**：`"限流 中间件"` 只命中同时含这两个词的文档，不是含任
    /// 意一个即可（带引号的短语查询照常按相邻位置匹配）。结果按 BM25 相关性分数
    /// 排序。需要扩展名过滤或按 mtime/size 排序，改用
    /// [`search_filtered`](Self::search_filtered) / [`search_advanced`](Self::search_advanced)。
    ///
    /// # Examples
    ///
    /// ```no_run
    /// # fn main() -> anyhow::Result<()> {
    /// use std::path::Path;
    /// use dowse::Searcher;
    ///
    /// let searcher = Searcher::open(Path::new("./my-index"))?;
    /// let hits = searcher.search("分布式 限流", 10)?;
    /// for hit in &hits {
    ///     println!("{} ({:.3})", hit.path, hit.score);
    /// }
    /// # Ok(())
    /// # }
    /// ```
    pub fn search(&self, query_str: &str, limit: usize) -> Result<Vec<SearchHit>> {
        self.search_advanced(query_str, limit, None, SortMode::Relevance)
    }

    /// 同 search()，多一个可选的扩展名过滤（不含点，大小写不敏感）。
    /// 给 MCP 的 search 工具用；单个扩展名是分组过滤的特例（长度为 1 的集合）。
    pub fn search_filtered(
        &self,
        query_str: &str,
        limit: usize,
        ext: Option<&str>,
    ) -> Result<Vec<SearchHit>> {
        let group: Option<&[&str]> = ext.as_ref().map(std::slice::from_ref);
        self.search_advanced(query_str, limit, group, SortMode::Relevance)
    }

    /// 浮窗"筛选/排序器"两件套的核心入口：query AND ext 分组过滤在 tantivy
    /// 查询层合取（分组内部是 Should 并集，跟 query 之间是 Must），排序按
    /// `sort` 选相关性打分或者 mtime/size 的 fast field 排序——都在查询层
    /// 完成，不是先拿结果再筛/再排，否则筛剩/排后的数量会少于调用方要求的 limit。
    ///
    /// # Examples
    ///
    /// ```no_run
    /// # fn main() -> anyhow::Result<()> {
    /// use std::path::Path;
    /// use dowse::{Searcher, SortMode, ext_group_by_name};
    ///
    /// let searcher = Searcher::open(Path::new("./my-index"))?;
    /// // 只在文档类扩展名里搜，按修改时间从新到旧排。
    /// let hits = searcher.search_advanced(
    ///     "季度 报告",
    ///     20,
    ///     ext_group_by_name(Some("doc")),
    ///     SortMode::MtimeDesc,
    /// )?;
    /// println!("命中 {} 条", hits.len());
    /// # Ok(())
    /// # }
    /// ```
    pub fn search_advanced(
        &self,
        query_str: &str,
        limit: usize,
        ext_group: Option<&[&str]>,
        sort: SortMode,
    ) -> Result<Vec<SearchHit>> {
        // 复用分页实现取第一页（offset=0），丢掉总数——不给这条老入口加参数，
        // 避免动到浮窗/CLI 等既有调用方的签名。多算的那次 Count 和 TopDocs 共用
        // 同一次检索（见 search_paged 里的 tuple 收集器），几乎不额外花代价。
        Ok(self
            .search_paged(query_str, limit, 0, ext_group, sort)?
            .hits)
    }

    /// 分页版 [`search_advanced`](Self::search_advanced)：多一个 `offset`（先跳过
    /// 多少条命中）并额外返回匹配总数（[`SearchPage::total`]）。query 与 ext 分组
    /// 过滤、排序都在 tantivy 查询层完成，`TopDocs` 和 `Count` 用同一次检索的
    /// tuple 收集器一起跑——两者本就都要遍历全部命中文档，合成一次遍历，总数几乎
    /// 是白捡的。`offset` 传 0 即取第一页。
    ///
    /// # Examples
    ///
    /// ```no_run
    /// # fn main() -> anyhow::Result<()> {
    /// use std::path::Path;
    /// use dowse::{Searcher, SortMode};
    ///
    /// let searcher = Searcher::open(Path::new("./my-index"))?;
    /// // 取第 2 页（每页 20 条）。
    /// let page = searcher.search_paged("季度 报告", 20, 20, None, SortMode::Relevance)?;
    /// println!("本页 {} 条，共 {} 条", page.hits.len(), page.total);
    /// # Ok(())
    /// # }
    /// ```
    pub fn search_paged(
        &self,
        query_str: &str,
        limit: usize,
        offset: usize,
        ext_group: Option<&[&str]>,
        sort: SortMode,
    ) -> Result<SearchPage> {
        // 把查询串解析成（检索用查询, 只含内容词的高亮查询）：无操作符时两者都退回
        // 老的整串 parse_query，逐字节向后兼容；有操作符时按结构化规则拼。摘要高亮只
        // 认内容词那份，path:/mtime:/size: 这些操作符不进高亮（它们的 term 落在别的
        // 字段上，SnippetGenerator 绑定在 content 字段，本就不会拿它们去标原文，这里
        // 再用纯内容查询喂它是双保险，也让意图更清楚）。
        let (retrieval_query, snippet_query) = self.build_queries(query_str)?;
        let query: Box<dyn Query> = match ext_group {
            Some(exts) if !exts.is_empty() => {
                let ext_should: Vec<(Occur, Box<dyn Query>)> = exts
                    .iter()
                    .map(|ext| {
                        let term =
                            Term::from_field_text(self.fields.ext, &ext.to_ascii_lowercase());
                        (
                            Occur::Should,
                            Box::new(TermQuery::new(term, IndexRecordOption::Basic))
                                as Box<dyn Query>,
                        )
                    })
                    .collect();
                Box::new(BooleanQuery::new(vec![
                    (Occur::Must, retrieval_query),
                    (
                        Occur::Must,
                        Box::new(BooleanQuery::new(ext_should)) as Box<dyn Query>,
                    ),
                ]))
            }
            _ => retrieval_query,
        };
        let searcher = self.reader.searcher();

        let mut snippet_gen =
            SnippetGenerator::create(&searcher, &snippet_query, self.fields.content)?;
        snippet_gen.set_max_num_chars(SNIPPET_MAX_CHARS);

        // 相关性排序保留真实 BM25 分数；其余三档按 fast field 排，doc 顺序
        // 就是最终顺序，score 字段填 0.0——非相关性排序下这个分数没有意义，
        // 调用方（前端/MCP）不应该拿它做展示或二次排序。
        //
        // 每档都把 `TopDocs`（带 offset）和 `Count` 打成 tuple 收集器一次跑完：
        // TopDocs 和 Count 本来都要遍历全部命中文档，合成一次遍历顺带拿到总数，
        // 供分页调用方判断还有没有下一页。
        let (addrs, total): (Vec<(f32, DocAddress)>, usize) = match sort {
            SortMode::Relevance => {
                let (top, total) = searcher.search(
                    &query,
                    &(
                        TopDocs::with_limit(limit)
                            .and_offset(offset)
                            .order_by_score(),
                        Count,
                    ),
                )?;
                (top, total)
            }
            SortMode::MtimeDesc => {
                let (top, total) = searcher.search(
                    &query,
                    &(
                        TopDocs::with_limit(limit)
                            .and_offset(offset)
                            .order_by_fast_field::<i64>("mtime", Order::Desc),
                        Count,
                    ),
                )?;
                (
                    top.into_iter().map(|(_, addr)| (0.0, addr)).collect(),
                    total,
                )
            }
            SortMode::MtimeAsc => {
                let (top, total) = searcher.search(
                    &query,
                    &(
                        TopDocs::with_limit(limit)
                            .and_offset(offset)
                            .order_by_fast_field::<i64>("mtime", Order::Asc),
                        Count,
                    ),
                )?;
                (
                    top.into_iter().map(|(_, addr)| (0.0, addr)).collect(),
                    total,
                )
            }
            SortMode::SizeDesc => {
                let (top, total) = searcher.search(
                    &query,
                    &(
                        TopDocs::with_limit(limit)
                            .and_offset(offset)
                            .order_by_fast_field::<u64>("size", Order::Desc),
                        Count,
                    ),
                )?;
                (
                    top.into_iter().map(|(_, addr)| (0.0, addr)).collect(),
                    total,
                )
            }
        };

        let mut hits = Vec::with_capacity(addrs.len());
        for (score, addr) in addrs {
            let doc: TantivyDocument = searcher.doc(addr)?;
            let path = doc
                .get_first(self.fields.path)
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_owned();
            let content = doc
                .get_first(self.fields.content)
                .and_then(|v| v.as_str())
                .unwrap_or_default();
            let ext = doc
                .get_first(self.fields.ext)
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_owned();

            let (snippet, highlighted) =
                snippet_with_fallback(&snippet_gen, content, SNIPPET_MAX_CHARS);
            hits.push(SearchHit {
                path,
                score,
                snippet,
                highlighted,
                ext,
            });
        }
        Ok(SearchPage { hits, total })
    }

    /// 把查询串翻成两份 tantivy 查询：`.0` 用于检索（含全部操作符条件），`.1` 只含
    /// 内容词、供摘要/预览高亮用。
    ///
    /// 向后兼容的关键分叉在这里：一旦 [`query::parse`] 判定整串**没有**任何操作符，
    /// 就把原始查询串原样交回老的 `QueryParser`（两份都是），跟从前逐字节一致——
    /// 短语、`ext:`、多词 AND 等既有行为一个字节都不动。只有确实出现 `path:` /
    /// `mtime:` / `size:` / `OR` / `NOT` / `-` 时才走下面的结构化拼装。
    fn build_queries(&self, query_str: &str) -> Result<(Box<dyn Query>, Box<dyn Query>)> {
        let parsed = query::parse(query_str)?;
        if !parsed.has_operators {
            return Ok((
                self.parser.parse_query(query_str)?,
                self.parser.parse_query(query_str)?,
            ));
        }
        self.build_from_parsed(&parsed)
    }

    /// 把结构化的 [`query::Parsed`] 拼成（检索查询, 纯内容高亮查询）。
    ///
    /// 组内各条件 AND（`Occur::Must`，排除词走 `Occur::MustNot`），组间 OR
    /// （`Occur::Should`），落实"AND 优先级高于 OR"。纯内容查询把所有组的正内容词
    /// 以 `Should` 并起来喂给 SnippetGenerator——高亮只关心"哪些词出现在原文里"，
    /// 不需要复刻 AND/OR 的结构。
    fn build_from_parsed(
        &self,
        parsed: &query::Parsed,
    ) -> Result<(Box<dyn Query>, Box<dyn Query>)> {
        let mut group_queries: Vec<Box<dyn Query>> = Vec::new();
        // 所有组的正内容词并集，只给高亮用。
        let mut content_only: Vec<(Occur, Box<dyn Query>)> = Vec::new();

        for group in &parsed.groups {
            let mut clauses: Vec<(Occur, Box<dyn Query>)> = Vec::new();

            for term in &group.content {
                // 内容词原样交给主 QueryParser（同时查 name+content，短语/子词都由它处理）。
                clauses.push((Occur::Must, self.parser.parse_query(term)?));
                // 再解析一份进高亮集合——Box<dyn Query> 不能克隆，只能重解析一次。
                content_only.push((Occur::Should, self.parser.parse_query(term)?));
            }
            for term in &group.excluded {
                clauses.push((Occur::MustNot, self.parser.parse_query(term)?));
            }
            for path in &group.paths {
                clauses.push((Occur::Must, self.build_path_query(path)?));
            }
            for bound in &group.mtimes {
                clauses.push((Occur::Must, self.build_mtime_query(bound)));
            }
            for bound in &group.sizes {
                clauses.push((Occur::Must, self.build_size_query(bound)));
            }

            // 只有排除条件、没有任何正向条件的组（如 `-草稿` 单独成组）：tantivy 的
            // BooleanQuery 只有 MustNot 会匹配空集，补一个 AllQuery 当正向底座，
            // 语义才是"全部里排掉这些"。
            let has_positive = clauses.iter().any(|(occ, _)| *occ != Occur::MustNot);
            if !has_positive {
                clauses.push((Occur::Must, Box::new(AllQuery)));
            }
            group_queries.push(Box::new(BooleanQuery::new(clauses)));
        }

        let retrieval: Box<dyn Query> = if group_queries.len() == 1 {
            // 单组不套 OR 外壳，省一层 BooleanQuery。
            group_queries.pop().expect("已判定非空")
        } else {
            let shoulds = group_queries
                .into_iter()
                .map(|q| (Occur::Should, q))
                .collect();
            Box::new(BooleanQuery::new(shoulds))
        };

        // 纯内容查询可能为空（整串只有操作符、没有内容词）：空 BooleanQuery 不匹配任何
        // term，SnippetGenerator 拿不到高亮词，摘要走"文档开头无高亮"兜底，符合预期。
        let snippet: Box<dyn Query> = Box::new(BooleanQuery::new(content_only));
        Ok((retrieval, snippet))
    }

    /// 把一条 `path:` 操作数拼成 path_text 字段上的查询。
    ///
    /// 用跟索引侧同一个 [`crate::tokenizer::MixedTokenizer`] 把操作数切成 token，
    /// 保证"查"和"建"两侧口径一致（否则大小写/子词边界对不上就搜不到）。带引号的
    /// 操作数（`path:"my docs"`）且切出多个 token 时按短语（相邻位置）匹配，落实
    /// "按整体处理"；否则多个 token 之间 AND、单 token 直接一个 TermQuery。
    fn build_path_query(&self, path: &query::PathTerm) -> Result<Box<dyn Query>> {
        let mut tokenizer = crate::tokenizer::MixedTokenizer::new();
        let mut stream = tokenizer.token_stream(&path.operand);
        let mut terms: Vec<Term> = Vec::new();
        while stream.advance() {
            terms.push(Term::from_field_text(
                self.fields.path_text,
                &stream.token().text,
            ));
        }
        if terms.is_empty() {
            bail!(
                "path: 操作数 \"{}\" 里没有可检索的内容（全是标点/空白？）",
                path.operand
            );
        }
        if path.phrase && terms.len() >= 2 {
            return Ok(Box::new(PhraseQuery::new(terms)));
        }
        if terms.len() == 1 {
            let term = terms.pop().expect("刚判过长度为 1");
            return Ok(Box::new(TermQuery::new(term, IndexRecordOption::Basic)));
        }
        let clauses: Vec<(Occur, Box<dyn Query>)> = terms
            .into_iter()
            .map(|t| {
                (
                    Occur::Must,
                    Box::new(TermQuery::new(t, IndexRecordOption::Basic)) as Box<dyn Query>,
                )
            })
            .collect();
        Ok(Box::new(BooleanQuery::new(clauses)))
    }

    /// 把一条 `mtime:` 过滤拼成 mtime 字段上的 [`RangeQuery`]。mtime 是 i64 FAST 字段，
    /// tantivy 0.26 的 RangeQuery 直接走 fast field 扫描，不需要额外索引属性。
    /// 日期已在 [`query`] 层归约成"当天/次日 0 点"两个毫秒边界，这里只按比较符
    /// 选开闭端（见 [`query::DateBound`] 对"整天"语义的说明）。
    fn build_mtime_query(&self, bound: &query::DateBound) -> Box<dyn Query> {
        let field = self.fields.mtime;
        let start = Term::from_field_i64(field, bound.start_ms);
        let next = Term::from_field_i64(field, bound.next_ms);
        let (lower, upper) = match bound.cmp {
            Cmp::Gt => (Bound::Included(next), Bound::Unbounded),
            Cmp::Ge => (Bound::Included(start), Bound::Unbounded),
            Cmp::Lt => (Bound::Unbounded, Bound::Excluded(start)),
            Cmp::Le => (Bound::Unbounded, Bound::Excluded(next)),
        };
        Box::new(RangeQuery::new(lower, upper))
    }

    /// 把一条 `size:` 过滤拼成 size 字段上的 [`RangeQuery`]。size 是 u64 FAST 字段，
    /// 同样走 fast field 范围扫描。阈值已在 [`query`] 层折算成字节。
    fn build_size_query(&self, bound: &query::SizeBound) -> Box<dyn Query> {
        let field = self.fields.size;
        let term = Term::from_field_u64(field, bound.bytes);
        let (lower, upper) = match bound.cmp {
            Cmp::Gt => (Bound::Excluded(term), Bound::Unbounded),
            Cmp::Ge => (Bound::Included(term), Bound::Unbounded),
            Cmp::Lt => (Bound::Unbounded, Bound::Excluded(term)),
            Cmp::Le => (Bound::Unbounded, Bound::Included(term)),
        };
        Box::new(RangeQuery::new(lower, upper))
    }

    /// 索引里的文档总数，给 CLI 的状态输出用。
    ///
    /// 读的是这个 searcher 当前 reader 快照里的数字——是打开（或上一次
    /// [`reload`](Self::reload)）那一刻的定格计数，不是实时值。并发写入端提交的
    /// 新文档要等下一次 `reload` 才会反映进来。
    pub fn num_docs(&self) -> u64 {
        self.reader.searcher().num_docs()
    }

    /// `root` 前缀下的文档数——多根索引（里程碑 7）托盘"索引文件夹"子菜单
    /// 每根一项要显示"路径 + N 篇"。查询构造跟 `updater.rs::delete_tree`
    /// 是同一套"精确项 ∪ 前缀区间"思路（同前缀不误伤兄弟目录），这里只读
    /// 计数、不删文档。
    pub fn count_under(&self, root: &Path) -> Result<u64> {
        let exact = root.to_string_lossy().into_owned();
        let mut prefix = exact.clone();
        if !prefix.ends_with(std::path::MAIN_SEPARATOR) {
            prefix.push(std::path::MAIN_SEPARATOR);
        }
        let upper = format!("{prefix}\u{10FFFF}");

        let exact_query = TermQuery::new(
            Term::from_field_text(self.fields.path, &exact),
            IndexRecordOption::Basic,
        );
        let subtree_query = RangeQuery::new(
            Bound::Included(Term::from_field_text(self.fields.path, &prefix)),
            Bound::Excluded(Term::from_field_text(self.fields.path, &upper)),
        );
        let query = BooleanQuery::new(vec![
            (Occur::Should, Box::new(exact_query) as Box<dyn Query>),
            (Occur::Should, Box::new(subtree_query) as Box<dyn Query>),
        ]);

        let searcher = self.reader.searcher();
        Ok(searcher.search(&query, &Count)? as u64)
    }

    /// 重新加载 reader，读到最新一次 commit 的段。
    ///
    /// MCP server 是独立进程，只读打开浮窗侧持有写权的同一份索引：tantivy 的段文件
    /// 不可变、读写可跨进程共存，但 reader 不会自己感知到别的进程提交了新段。
    /// 调用方（MCP 工具处理函数）应在每次请求前调用一次，保证读到浮窗侧最新的索引状态。
    pub fn reload(&self) -> Result<()> {
        self.reader.reload().context("索引 reader 重载失败")
    }

    /// 按路径取该文档更长的预览上下文（约 1500 字），命中词区间跟 search() 同一契约。
    /// 用于浮窗右侧预览区：用户在结果列表里选中一行后，用它的 path 和当前查询词
    /// 换一份比列表摘要（160 字）长得多的窗口。
    ///
    /// path 找不到、或者该文档已不在索引里（比如原文件被删除后索引还没重建），返回 None。
    ///
    /// # Examples
    ///
    /// ```no_run
    /// # fn main() -> anyhow::Result<()> {
    /// use std::path::Path;
    /// use dowse::Searcher;
    ///
    /// let searcher = Searcher::open(Path::new("./my-index"))?;
    /// let hits = searcher.search("限流器", 10)?;
    /// if let Some(hit) = hits.first() {
    ///     // 路径存在则拿到更长的预览窗口；文档已不在索引里则是 None。
    ///     match searcher.preview(&hit.path, "限流器")? {
    ///         Some(preview) => println!("{}", preview.snippet),
    ///         None => println!("该文档已不在索引里"),
    ///     }
    /// }
    /// # Ok(())
    /// # }
    /// ```
    pub fn preview(&self, path: &str, query_str: &str) -> Result<Option<PreviewHit>> {
        let searcher = self.reader.searcher();
        let path_term = Term::from_field_text(self.fields.path, path);
        let path_query = TermQuery::new(path_term, IndexRecordOption::Basic);

        // path 精确匹配 AND 用户查询词的**内容词部分**：既锁定这一篇文档，又让
        // SnippetGenerator 拿到内容词去定位高亮位置。用内容词那份而不是整串，是为了
        // 让预览查询词里若带了 path:/mtime:/size: 等操作符时不会把预览搞挂——操作符
        // 交给搜索层过滤即可，预览只关心"高亮哪些词"。纯词查询下这份就等于老的整串
        // parse_query，行为不变。
        let (_retrieval, content_query) = self.build_queries(query_str)?;
        let combined = BooleanQuery::new(vec![
            (Occur::Must, Box::new(path_query.clone()) as Box<dyn Query>),
            (Occur::Must, content_query),
        ]);

        let top_docs = searcher.search(&combined, &TopDocs::with_limit(1).order_by_score())?;
        if let Some((_, addr)) = top_docs.into_iter().next() {
            let mut snippet_gen =
                SnippetGenerator::create(&searcher, &combined, self.fields.content)?;
            snippet_gen.set_max_num_chars(PREVIEW_MAX_CHARS);

            let doc: TantivyDocument = searcher.doc(addr)?;
            let content = doc
                .get_first(self.fields.content)
                .and_then(|v| v.as_str())
                .unwrap_or_default();
            let (snippet, highlighted) =
                snippet_with_fallback(&snippet_gen, content, PREVIEW_MAX_CHARS);
            let (size, mtime, ext) = self.doc_meta(&doc);
            return Ok(Some(PreviewHit {
                snippet,
                highlighted,
                size,
                mtime,
                ext,
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
        let (size, mtime, ext) = self.doc_meta(&doc);
        Ok(Some(PreviewHit {
            snippet: window,
            highlighted: vec![],
            size,
            mtime,
            ext,
        }))
    }

    /// 从命中文档里取 (size, mtime, ext) 三元组，preview() 的两条分支共用。
    fn doc_meta(&self, doc: &TantivyDocument) -> (u64, i64, String) {
        let size = doc
            .get_first(self.fields.size)
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        let mtime = doc
            .get_first(self.fields.mtime)
            .and_then(|v| v.as_i64())
            .unwrap_or(0);
        let ext = doc
            .get_first(self.fields.ext)
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_owned();
        (size, mtime, ext)
    }
}

/// 把 tantivy SnippetGenerator 吐出的命中区间整理成有序且互不重叠的序列。
///
/// jieba 分词器会产出重叠 token（比如"分布式"同时切出"分布"和"分布式"），
/// SnippetGenerator 按 token 逐个给区间，顺序和是否重叠都不保证。
/// 这里按起点排序，再把重叠或相邻（下一个的 start <= 当前的 end）的区间合并，
/// 让调用方可以假设区间有序不重叠、按顺序切片渲染。
///
/// 对外暴露：dowse-app 的文件名高亮（highlight.rs）自己算出的匹配区间也要走
/// 同一套归并，两处共用这一份而不是各留一份。
pub fn normalize_ranges(mut ranges: Vec<Range<usize>>) -> Vec<Range<usize>> {
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

/// 把 content 截到不超过 `SNIPPET_SCAN_MAX_BYTES` 字节的安全前缀，边界落在字符边界上，
/// 保证截出来的 `&str` 本身合法（不会切在 UTF-8 多字节字符中间导致 panic）。
fn truncate_scan_window(content: &str) -> &str {
    if content.len() <= SNIPPET_SCAN_MAX_BYTES {
        return content;
    }
    let mut end = SNIPPET_SCAN_MAX_BYTES;
    while end > 0 && !content.is_char_boundary(end) {
        end -= 1;
    }
    &content[..end]
}

/// 在截断后的扫描窗口内生成摘要；如果窗口内一个命中区间都没有（比如命中词
/// 只出现在文档中后段、被截断切掉了），退回“文档开头一段、无高亮”的摘录，
/// 跟 `preview()` 里查询词和文档完全不匹配时的回退写法保持一致——
/// 都是“有文件但没有预览片段”的可接受降级，而不是给用户一个空字符串。
fn snippet_with_fallback(
    snippet_gen: &SnippetGenerator,
    content: &str,
    fallback_chars: usize,
) -> (String, Vec<Range<usize>>) {
    let scan_window = truncate_scan_window(content);
    let snippet = snippet_gen.snippet(scan_window);
    if !snippet.highlighted().is_empty() {
        return (
            snippet.fragment().to_owned(),
            normalize_ranges(snippet.highlighted().to_vec()),
        );
    }

    let head: String = content.chars().take(fallback_chars).collect();
    (head, Vec::new())
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
    fn search_is_case_insensitive_for_latin() -> Result<()> {
        let index_dir = tempfile::tempdir()?;
        let target_dir = tempfile::Builder::new().prefix("dowse-test-").tempdir()?;

        std::fs::write(target_dir.path().join("api.md"), "API design notes")?;
        std::fs::write(target_dir.path().join("fs.md"), "File system layout")?;

        crate::rebuild_index(index_dir.path(), target_dir.path())?;
        let searcher = Searcher::open(index_dir.path())?;

        for (query, expected) in [
            ("api", "api.md"),
            ("API", "api.md"),
            ("file", "fs.md"),
            ("FILE", "fs.md"),
        ] {
            let hits = searcher.search(query, 10)?;
            assert_eq!(hits.len(), 1, "查询 {query:?} 应命中一篇");
            assert!(
                hits[0].path.ends_with(expected),
                "查询 {query:?} 应命中 {expected}，实际 {:?}",
                hits[0].path
            );
        }
        Ok(())
    }

    #[test]
    fn search_hyphenated_string_matches_subwords() -> Result<()> {
        let index_dir = tempfile::tempdir()?;
        let target_dir = tempfile::Builder::new().prefix("dowse-test-").tempdir()?;

        std::fs::write(target_dir.path().join("covid.md"), "covid-19 vaccine")?;
        std::fs::write(
            target_dir.path().join("marker.md"),
            "glimmerfrost-9931-unique-marker",
        )?;

        crate::rebuild_index(index_dir.path(), target_dir.path())?;
        let searcher = Searcher::open(index_dir.path())?;

        for (query, expected) in [
            ("covid", "covid.md"),
            ("19", "covid.md"),
            ("glimmerfrost", "marker.md"),
            ("9931", "marker.md"),
            ("marker", "marker.md"),
        ] {
            let hits = searcher.search(query, 10)?;
            assert!(
                hits.iter().any(|h| h.path.ends_with(expected)),
                "子词查询 {query:?} 应命中 {expected}，实际 {:?}",
                hits.iter().map(|h| &h.path).collect::<Vec<_>>()
            );
        }

        // 整串查询：AND 语义下所有子词都在同一篇，应该命中。
        let full = searcher.search("glimmerfrost-9931-unique-marker", 10)?;
        assert!(
            full.iter().any(|h| h.path.ends_with("marker.md")),
            "整串查询应命中 marker.md，实际 {:?}",
            full.iter().map(|h| &h.path).collect::<Vec<_>>()
        );
        Ok(())
    }

    #[test]
    fn search_mixed_cjk_latin_document() -> Result<()> {
        let index_dir = tempfile::tempdir()?;
        let target_dir = tempfile::Builder::new().prefix("dowse-test-").tempdir()?;

        std::fs::write(
            target_dir.path().join("mixed.md"),
            "用 GPT-4 写的 covid-19 报告",
        )?;

        crate::rebuild_index(index_dir.path(), target_dir.path())?;
        let searcher = Searcher::open(index_dir.path())?;

        for query in ["gpt", "covid", "报告"] {
            let hits = searcher.search(query, 10)?;
            assert!(
                hits.iter().any(|h| h.path.ends_with("mixed.md")),
                "混合文档查询 {query:?} 应命中 mixed.md，实际 {:?}",
                hits.iter().map(|h| &h.path).collect::<Vec<_>>()
            );
        }
        Ok(())
    }

    #[test]
    fn search_mixed_document_highlight_ranges_are_valid() -> Result<()> {
        let index_dir = tempfile::tempdir()?;
        let target_dir = tempfile::Builder::new().prefix("dowse-test-").tempdir()?;

        std::fs::write(
            target_dir.path().join("mixed.md"),
            "这是一份关于 glimmerfrost-9931 的中文说明，包含 API 设计与 covid-19 数据。",
        )?;

        crate::rebuild_index(index_dir.path(), target_dir.path())?;
        let searcher = Searcher::open(index_dir.path())?;

        let hits = searcher.search("glimmerfrost", 10)?;
        assert_eq!(hits.len(), 1);
        let hit = &hits[0];
        assert!(!hit.highlighted.is_empty(), "命中词应该被标记出来");
        for w in hit.highlighted.windows(2) {
            assert!(w[0].end <= w[1].start, "区间应互不重叠且按起点排序");
        }
        for r in &hit.highlighted {
            assert!(r.start <= r.end);
            assert!(r.end <= hit.snippet.len());
            // 切片不 panic，且切出来的确实是原查询子串（小写归一后一致）。
            assert!(hit.snippet.is_char_boundary(r.start));
            assert!(hit.snippet.is_char_boundary(r.end));
            assert_eq!(hit.snippet[r.start..r.end].to_lowercase(), "glimmerfrost");
        }
        Ok(())
    }

    #[test]
    fn phrase_query_matches_adjacent_terms_under_sequential_positions() -> Result<()> {
        let index_dir = tempfile::tempdir()?;
        let target_dir = tempfile::Builder::new().prefix("dowse-test-").tempdir()?;

        std::fs::write(
            target_dir.path().join("marker.md"),
            "glimmerfrost-9931-unique-marker",
        )?;
        std::fs::write(
            target_dir.path().join("cn.md"),
            "系统采用分布式限流器保护后端服务。",
        )?;

        crate::rebuild_index(index_dir.path(), target_dir.path())?;
        let searcher = Searcher::open(index_dir.path())?;

        // 英文短语：unique 和 marker 在新的顺序 position 方案下相邻，短语应命中。
        let phrase = searcher.search("\"unique marker\"", 10)?;
        assert!(
            phrase.iter().any(|h| h.path.ends_with("marker.md")),
            "短语 \"unique marker\" 应命中 marker.md，实际 {:?}",
            phrase.iter().map(|h| &h.path).collect::<Vec<_>>()
        );

        // 顺序打乱的短语不应命中（验证短语确实按相邻位置匹配，不是退化成 OR）。
        let reversed = searcher.search("\"marker unique\"", 10)?;
        assert!(
            !reversed.iter().any(|h| h.path.ends_with("marker.md")),
            "词序颠倒的短语不该命中，实际 {:?}",
            reversed.iter().map(|h| &h.path).collect::<Vec<_>>()
        );

        // 中文短语：证明短语语法对 jieba 段照样解析并匹配。
        let cn = searcher.search("\"分布式\"", 10)?;
        assert!(
            cn.iter().any(|h| h.path.ends_with("cn.md")),
            "中文短语应命中 cn.md，实际 {:?}",
            cn.iter().map(|h| &h.path).collect::<Vec<_>>()
        );
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
        assert_eq!(
            hits.len(),
            1,
            "AND 语义下只有同时含两个词的文档命中: {hits:?}",
            hits = hits.iter().map(|h| &h.path).collect::<Vec<_>>()
        );
        assert!(hits[0].path.ends_with("both.md"));
        Ok(())
    }

    #[test]
    fn search_filtered_by_ext_only_matches_that_extension() -> Result<()> {
        let index_dir = tempfile::tempdir()?;
        let target_dir = tempfile::Builder::new().prefix("dowse-test-").tempdir()?;

        std::fs::write(target_dir.path().join("note.md"), "限流方案对比")?;
        std::fs::write(target_dir.path().join("note.txt"), "限流方案对比")?;

        crate::rebuild_index(index_dir.path(), target_dir.path())?;
        let searcher = Searcher::open(index_dir.path())?;

        let hits = searcher.search_filtered("限流", 10, Some("md"))?;
        assert_eq!(
            hits.len(),
            1,
            "ext 过滤后应该只剩 .md 那篇: {hits:?}",
            hits = hits.iter().map(|h| &h.path).collect::<Vec<_>>()
        );
        assert!(hits[0].path.ends_with("note.md"));
        assert_eq!(hits[0].ext, "md");

        // 不传 ext 过滤，两篇都应该命中
        let unfiltered = searcher.search("限流", 10)?;
        assert_eq!(unfiltered.len(), 2);
        Ok(())
    }

    #[test]
    fn search_advanced_ext_group_matches_only_group_members() -> Result<()> {
        let index_dir = tempfile::tempdir()?;
        let target_dir = tempfile::Builder::new().prefix("dowse-test-").tempdir()?;

        std::fs::write(target_dir.path().join("note.md"), "限流方案")?;
        std::fs::write(target_dir.path().join("main.rs"), "限流方案 fn main")?;
        std::fs::write(target_dir.path().join("note.txt"), "限流方案")?;

        crate::rebuild_index(index_dir.path(), target_dir.path())?;
        let searcher = Searcher::open(index_dir.path())?;

        // 代码分组（BooleanQuery Should 并集）应该只命中 .rs，md/txt 都不在这组里。
        let code_hits = searcher.search_advanced(
            "限流",
            10,
            Some(crate::ext_groups::CODE),
            SortMode::Relevance,
        )?;
        assert_eq!(
            code_hits.len(),
            1,
            "代码分组应该只命中 .rs: {:?}",
            code_hits.iter().map(|h| &h.path).collect::<Vec<_>>()
        );
        assert!(code_hits[0].path.ends_with("main.rs"));

        // 文档分组同时含 md 和 txt，验证并集确实取的是"任意一个成员命中"。
        let doc_hits = searcher.search_advanced(
            "限流",
            10,
            Some(crate::ext_groups::DOC),
            SortMode::Relevance,
        )?;
        assert_eq!(
            doc_hits.len(),
            2,
            "文档分组应该命中 .md 和 .txt 两篇: {:?}",
            doc_hits.iter().map(|h| &h.path).collect::<Vec<_>>()
        );
        Ok(())
    }

    #[test]
    fn search_sorted_by_mtime_desc_orders_newest_first() -> Result<()> {
        let index_dir = tempfile::tempdir()?;
        let target_dir = tempfile::Builder::new().prefix("dowse-test-").tempdir()?;

        std::fs::write(target_dir.path().join("old.md"), "笔记 alpha")?;
        // 相邻写入间隔一小段时间，保证两篇的 mtime 毫秒级可区分。
        std::thread::sleep(std::time::Duration::from_millis(20));
        std::fs::write(target_dir.path().join("new.md"), "笔记 beta")?;

        crate::rebuild_index(index_dir.path(), target_dir.path())?;
        let searcher = Searcher::open(index_dir.path())?;

        let hits = searcher.search_advanced("笔记", 10, None, SortMode::MtimeDesc)?;
        assert_eq!(hits.len(), 2);
        assert!(
            hits[0].path.ends_with("new.md"),
            "mtime 降序，最新的应排第一: {:?}",
            hits.iter().map(|h| &h.path).collect::<Vec<_>>()
        );
        assert!(hits[1].path.ends_with("old.md"));
        Ok(())
    }

    #[test]
    fn search_sorted_by_mtime_asc_orders_oldest_first() -> Result<()> {
        let index_dir = tempfile::tempdir()?;
        let target_dir = tempfile::Builder::new().prefix("dowse-test-").tempdir()?;

        std::fs::write(target_dir.path().join("old.md"), "笔记 alpha")?;
        std::thread::sleep(std::time::Duration::from_millis(20));
        std::fs::write(target_dir.path().join("new.md"), "笔记 beta")?;

        crate::rebuild_index(index_dir.path(), target_dir.path())?;
        let searcher = Searcher::open(index_dir.path())?;

        let hits = searcher.search_advanced("笔记", 10, None, SortMode::MtimeAsc)?;
        assert_eq!(hits.len(), 2);
        assert!(
            hits[0].path.ends_with("old.md"),
            "mtime 升序，最旧的应排第一: {:?}",
            hits.iter().map(|h| &h.path).collect::<Vec<_>>()
        );
        assert!(hits[1].path.ends_with("new.md"));
        Ok(())
    }

    #[test]
    fn search_sorted_by_size_desc_orders_largest_first() -> Result<()> {
        let index_dir = tempfile::tempdir()?;
        let target_dir = tempfile::Builder::new().prefix("dowse-test-").tempdir()?;

        std::fs::write(target_dir.path().join("small.md"), "笔记")?;
        std::fs::write(target_dir.path().join("big.md"), "笔记".repeat(500))?;

        crate::rebuild_index(index_dir.path(), target_dir.path())?;
        let searcher = Searcher::open(index_dir.path())?;

        let hits = searcher.search_advanced("笔记", 10, None, SortMode::SizeDesc)?;
        assert_eq!(hits.len(), 2);
        assert!(
            hits[0].path.ends_with("big.md"),
            "size 降序，最大的应排第一: {:?}",
            hits.iter().map(|h| &h.path).collect::<Vec<_>>()
        );
        assert!(hits[1].path.ends_with("small.md"));
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
    fn count_under_only_counts_docs_within_root_prefix() -> Result<()> {
        let index_dir = tempfile::tempdir()?;
        let target_dir = tempfile::Builder::new().prefix("dowse-test-").tempdir()?;

        std::fs::write(target_dir.path().join("a.md"), "内容")?;
        std::fs::write(target_dir.path().join("b.md"), "内容")?;
        let sibling_named_with_shared_prefix = target_dir.path().with_file_name(format!(
            "{}-sibling",
            target_dir.path().file_name().unwrap().to_string_lossy()
        ));
        std::fs::create_dir_all(&sibling_named_with_shared_prefix)?;
        std::fs::write(sibling_named_with_shared_prefix.join("c.md"), "内容")?;

        crate::rebuild_index(index_dir.path(), target_dir.path())?;
        let searcher = Searcher::open(index_dir.path())?;

        assert_eq!(searcher.count_under(target_dir.path())?, 2);

        std::fs::remove_dir_all(&sibling_named_with_shared_prefix).ok();
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

    #[test]
    fn search_path_operator_matches_by_directory_and_filename() -> Result<()> {
        let index_dir = tempfile::tempdir()?;
        let target_dir = tempfile::Builder::new().prefix("dowse-test-").tempdir()?;

        // 一篇埋在带辨识度名字的子目录里，一篇直接放根下。
        let sub = target_dir.path().join("zzreportfolder");
        std::fs::create_dir_all(&sub)?;
        std::fs::write(sub.join("note.md"), "内容")?;
        std::fs::write(target_dir.path().join("other.md"), "内容")?;

        crate::rebuild_index(index_dir.path(), target_dir.path())?;
        let searcher = Searcher::open(index_dir.path())?;

        // path: 命中目录名——只应中子目录里那篇。
        let by_dir = searcher.search("path:zzreportfolder", 10)?;
        assert_eq!(
            by_dir.len(),
            1,
            "path: 目录名应只中一篇: {by_dir:?}",
            by_dir = by_dir.iter().map(|h| &h.path).collect::<Vec<_>>()
        );
        assert!(by_dir[0].path.ends_with("note.md"));

        // path: 命中文件名词——only other.md 名里有 other。
        let by_name = searcher.search("path:other", 10)?;
        assert_eq!(by_name.len(), 1);
        assert!(by_name[0].path.ends_with("other.md"));

        // 内容词 AND path: 组合——两篇都含"内容"，但只有子目录那篇路径含 zzreportfolder。
        let combined = searcher.search("内容 path:zzreportfolder", 10)?;
        assert_eq!(combined.len(), 1);
        assert!(combined[0].path.ends_with("note.md"));
        Ok(())
    }

    #[test]
    fn search_path_operator_quoted_phrase_operand() -> Result<()> {
        let index_dir = tempfile::tempdir()?;
        let target_dir = tempfile::Builder::new().prefix("dowse-test-").tempdir()?;

        // 目录名切成相邻的两个 token（my / docs），验证带引号按整体（相邻短语）匹配。
        let sub = target_dir.path().join("my docs");
        std::fs::create_dir_all(&sub)?;
        std::fs::write(sub.join("a.md"), "内容")?;
        std::fs::write(target_dir.path().join("b.md"), "内容")?;

        crate::rebuild_index(index_dir.path(), target_dir.path())?;
        let searcher = Searcher::open(index_dir.path())?;

        let hits = searcher.search("path:\"my docs\"", 10)?;
        assert_eq!(hits.len(), 1, "带引号的 path 操作数应按整体匹配那一篇");
        assert!(hits[0].path.ends_with("a.md"));
        Ok(())
    }

    #[test]
    fn search_mtime_operator_filters_by_date() -> Result<()> {
        let index_dir = tempfile::tempdir()?;
        let target_dir = tempfile::Builder::new().prefix("dowse-test-").tempdir()?;
        std::fs::write(target_dir.path().join("note.md"), "笔记内容")?;

        crate::rebuild_index(index_dir.path(), target_dir.path())?;
        let searcher = Searcher::open(index_dir.path())?;

        // 文件 mtime 是"现在"，稳定地落在 2000 与 2099 之间，无需 sleep 也确定。
        assert_eq!(searcher.search("笔记 mtime:>2000-01-01", 10)?.len(), 1);
        assert_eq!(searcher.search("笔记 mtime:>=2000-01", 10)?.len(), 1);
        assert_eq!(
            searcher.search("笔记 mtime:<2000-01-01", 10)?.len(),
            0,
            "2000 年之前的过滤应把当前文件排除"
        );
        assert_eq!(
            searcher.search("笔记 mtime:>2099-01-01", 10)?.len(),
            0,
            "未来日期之后的过滤应无命中"
        );
        Ok(())
    }

    #[test]
    fn search_size_operator_filters_by_bytes() -> Result<()> {
        let index_dir = tempfile::tempdir()?;
        let target_dir = tempfile::Builder::new().prefix("dowse-test-").tempdir()?;
        std::fs::write(target_dir.path().join("small.md"), "小")?;
        // 2KB 以上的大文件。
        std::fs::write(target_dir.path().join("big.md"), "大".repeat(2000))?;

        crate::rebuild_index(index_dir.path(), target_dir.path())?;
        let searcher = Searcher::open(index_dir.path())?;

        let big = searcher.search("size:>1kb", 10)?;
        assert_eq!(big.len(), 1, "size:>1kb 只应中大文件");
        assert!(big[0].path.ends_with("big.md"));

        let small = searcher.search("size:<1kb", 10)?;
        assert_eq!(small.len(), 1, "size:<1kb 只应中小文件");
        assert!(small[0].path.ends_with("small.md"));
        Ok(())
    }

    #[test]
    fn search_or_groups_and_within_group_and() -> Result<()> {
        let index_dir = tempfile::tempdir()?;
        let target_dir = tempfile::Builder::new().prefix("dowse-test-").tempdir()?;
        std::fs::write(target_dir.path().join("a.md"), "alpha 内容")?;
        std::fs::write(target_dir.path().join("b.md"), "beta 内容")?;
        std::fs::write(target_dir.path().join("c.md"), "gamma 内容")?;

        crate::rebuild_index(index_dir.path(), target_dir.path())?;
        let searcher = Searcher::open(index_dir.path())?;

        // OR：两组各中一篇。
        let either = searcher.search("alpha OR beta", 10)?;
        assert_eq!(either.len(), 2, "alpha OR beta 应中两篇");

        // OR 里有一组无命中：只中 alpha 那篇。
        let one = searcher.search("alpha OR zzznotthere", 10)?;
        assert_eq!(one.len(), 1);
        assert!(one[0].path.ends_with("a.md"));

        // 组内 AND 优先于 OR：`alpha 缺失词 OR beta` → 第一组 (alpha AND 缺失词) 无命中，
        // 只剩第二组 beta。
        let grouped = searcher.search("alpha zzmissing OR beta", 10)?;
        assert_eq!(grouped.len(), 1, "AND 组优先，只应中 beta 那篇");
        assert!(grouped[0].path.ends_with("b.md"));
        Ok(())
    }

    #[test]
    fn search_exclusion_via_dash_and_not() -> Result<()> {
        let index_dir = tempfile::tempdir()?;
        let target_dir = tempfile::Builder::new().prefix("dowse-test-").tempdir()?;
        std::fs::write(target_dir.path().join("keep.md"), "限流 中间件")?;
        std::fs::write(target_dir.path().join("drop.md"), "限流 废弃")?;

        crate::rebuild_index(index_dir.path(), target_dir.path())?;
        let searcher = Searcher::open(index_dir.path())?;

        for query in ["限流 -废弃", "限流 NOT 废弃"] {
            let hits = searcher.search(query, 10)?;
            assert_eq!(hits.len(), 1, "查询 {query:?} 应排除含'废弃'那篇");
            assert!(hits[0].path.ends_with("keep.md"), "查询 {query:?} 结果不对");
        }
        Ok(())
    }

    #[test]
    fn search_invalid_operand_errors_not_silently_ignored() -> Result<()> {
        let index_dir = tempfile::tempdir()?;
        let target_dir = tempfile::Builder::new().prefix("dowse-test-").tempdir()?;
        std::fs::write(target_dir.path().join("note.md"), "内容")?;
        crate::rebuild_index(index_dir.path(), target_dir.path())?;
        let searcher = Searcher::open(index_dir.path())?;

        // 非法操作数必须报错，而不是静默当普通词——否则用户以为过滤生效了。
        assert!(searcher.search("mtime:>abc", 10).is_err());
        assert!(searcher.search("size:>10tb", 10).is_err());
        assert!(
            searcher.search("mtime:2026-01-01", 10).is_err(),
            "缺比较符应报错"
        );
        Ok(())
    }

    #[test]
    fn plain_query_without_operators_is_unchanged_regression() -> Result<()> {
        // 不含操作符的查询要跟从前逐字节一致：多词 AND + 短语都照旧。
        let index_dir = tempfile::tempdir()?;
        let target_dir = tempfile::Builder::new().prefix("dowse-test-").tempdir()?;
        std::fs::write(target_dir.path().join("both.md"), "限流中间件的实现")?;
        std::fs::write(target_dir.path().join("one.md"), "限流方案对比")?;

        crate::rebuild_index(index_dir.path(), target_dir.path())?;
        let searcher = Searcher::open(index_dir.path())?;

        // 多词默认 AND（无操作符走老路径）。
        let and_hits = searcher.search("限流 中间件", 10)?;
        assert_eq!(and_hits.len(), 1);
        assert!(and_hits[0].path.ends_with("both.md"));

        // 短语查询照旧。
        let phrase = searcher.search("\"限流中间件\"", 10)?;
        assert!(phrase.iter().any(|h| h.path.ends_with("both.md")));
        Ok(())
    }

    #[test]
    fn search_snippet_scan_truncation_keeps_retrieval_and_falls_back_beyond_scan_window()
    -> Result<()> {
        let index_dir = tempfile::tempdir()?;
        let target_dir = tempfile::Builder::new().prefix("dowse-test-").tempdir()?;

        // 哨兵词用纯英文字母数字串，避免中文分词器按上下文动态切词导致
        // 同一个词在不同句子里切法不一致（jieba 是上下文相关的统计分词，
        // 中文短语嵌进不同句子可能被切成不同的 token 边界）。
        const SENTINEL: &str = "zzzsentinelprobe888";

        // 大文档：哨兵词只出现在文档末尾，且前面的填充内容远超摘要扫描窗口
        // （128KB），所以扫描窗口内绝不会看到哨兵词。用于验证：
        // 1) 检索命中不受摘要截断影响（截断只影响摘要生成，不影响倒排检索）；
        // 2) 摘要生成因为扫描窗口内没有命中区间而退回无高亮兜底，不 panic。
        let filler = "填充内容占位符文本。".repeat(20_000);
        assert!(
            filler.len() > SNIPPET_SCAN_MAX_BYTES,
            "语料要真的超过截断阈值才能验证截断生效"
        );
        let big_content = format!("{filler}文档末尾出现了 {SENTINEL} 这个词。");
        std::fs::write(target_dir.path().join("big.md"), &big_content)?;

        // 小文档：哨兵词在开头，落在扫描窗口内，摘要应该正常带高亮。
        let small_content = format!("{SENTINEL} 出现在这篇小文档的开头。");
        std::fs::write(target_dir.path().join("small.md"), &small_content)?;

        crate::rebuild_index(index_dir.path(), target_dir.path())?;
        let searcher = Searcher::open(index_dir.path())?;

        let hits = searcher.search(SENTINEL, 10)?;
        assert_eq!(hits.len(), 2, "检索不该被摘要截断影响，两篇文档都要命中");

        let big_hit = hits
            .iter()
            .find(|h| h.path.ends_with("big.md"))
            .expect("大文档应该命中");
        assert!(
            big_hit.highlighted.is_empty(),
            "命中词被截断窗口挡在外面，摘要应退回无高亮兜底"
        );
        assert!(!big_hit.snippet.is_empty(), "兜底摘要不应该是空字符串");

        let small_hit = hits
            .iter()
            .find(|h| h.path.ends_with("small.md"))
            .expect("小文档应该命中");
        assert!(
            !small_hit.highlighted.is_empty(),
            "命中词在扫描窗口内，摘要应该正常高亮"
        );

        Ok(())
    }
}
