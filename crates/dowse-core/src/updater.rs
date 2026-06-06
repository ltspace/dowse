use std::ops::Bound;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use tantivy::collector::DocSetCollector;
use tantivy::query::{BooleanQuery, Occur, Query, RangeQuery, TermQuery};
use tantivy::schema::{IndexRecordOption, Value};
use tantivy::{Index, IndexReader, IndexWriter, ReloadPolicy, TantivyDocument, Term};

use crate::events::{PendingChange, PendingOp};
use crate::indexer::{add_file_document, add_image_document_with_content};
use crate::ocr::is_image;
use crate::ocr_queue::OcrQueue;
use crate::{Fields, build_schema, register_tokenizers};

/// 一批增量更新的处理结果，给日志/调试看。
#[derive(Debug, Default, Clone, Copy)]
pub struct BatchOutcome {
    /// 新增或修改成功入索引的文件数。
    pub upserted: usize,
    /// 删除的文档数（含目录前缀圈选删掉的每一篇）。
    pub removed: usize,
    /// upsert 时抽不出可索引文本被跳过的文件数（先删的动作照常生效）。
    pub skipped: usize,
}

/// 增量更新器：持有索引的写入端 IndexWriter，把防抖后的一批变更落进索引，
/// 一批只 commit 一次（tantivy 的 commit 是重操作，绝不能一个文件一次）。
///
/// 一个索引同一时刻只能有一个 IndexWriter。启动对账和实时监听要共用同一个
/// IndexUpdater（宿主用 `Arc<Mutex<_>>` 串起来），别各开各的 writer——第二个
/// writer 会被 tantivy 的写锁挡住。搜索侧的 Searcher 用的是独立的只读 reader，
/// 提交后自动重载，所以更新/对账进行时索引照常可搜，不会被写入端锁死。
pub struct IndexUpdater {
    index: Index,
    writer: IndexWriter,
    fields: Fields,
    index_dir: PathBuf,
}

impl IndexUpdater {
    /// 打开已有索引的写入端。先校验 schema 版本，不匹配就报错提示重建。
    pub fn open(index_dir: &Path) -> Result<Self> {
        crate::meta::ensure_schema_version(index_dir)?;
        let index = Index::open_in_dir(index_dir).context("打不开索引目录，先建一次索引再监听")?;
        register_tokenizers(&index);
        let (_, fields) = build_schema();
        // 50MB 写缓冲：增量场景一批就几个到几千个文件，不用像全量重建那样开 200MB。
        let writer = index.writer(50 * 1024 * 1024)?;
        Ok(Self {
            index,
            writer,
            fields,
            index_dir: index_dir.to_path_buf(),
        })
    }

    /// 处理一批防抖合并后的变更，最后 commit 一次。
    /// commit 失败返回 Err，调用方（run_watch）负责把这批退回队列下轮重试。
    pub fn apply(&mut self, batch: &[PendingChange]) -> Result<BatchOutcome> {
        let mut outcome = BatchOutcome::default();
        let mut touched_image = false;
        for change in batch {
            match change.op {
                PendingOp::Upsert => {
                    // 先删后加，天然幂等：同一 path 无论之前有没有、内容变没变，
                    // 结果都是"索引里恰好有这一篇的最新版本"。
                    self.delete_exact(&change.path);
                    if is_image(&change.path) {
                        touched_image = true;
                    }
                    if add_file_document(&self.writer, &self.fields, &change.path, &self.index_dir)?
                    {
                        outcome.upserted += 1;
                    } else {
                        // 抽不出文本（不支持的格式/损坏/文件已消失）：先删的动作已生效，
                        // 相当于把这篇从索引里拿掉。计一次 skip，不中断整批。
                        outcome.skipped += 1;
                    }
                }
                PendingOp::Remove => {
                    self.delete_exact(&change.path);
                    outcome.removed += 1;
                }
                PendingOp::RemoveTree => {
                    outcome.removed += self.delete_tree(&change.path)?;
                }
            }
        }
        self.writer.commit().context("增量提交失败")?;
        if touched_image {
            // 这一批里确实有图片新增/变更被塞进了 OCR 队列的内存态，落一次盘——
            // 没有图片的批次（大多数文本编辑场景）不用为这个多写一次文件。
            OcrQueue::for_index_dir(&self.index_dir)
                .save()
                .context("保存 OCR 队列状态失败")?;
        }
        Ok(outcome)
    }

    /// OCR worker 写回一张图片的最终识别结果：先删旧文档、写入新内容、立刻单独
    /// 提交。commit 粒度是"每张图片一次"，不跟主更新批次共用节奏——两者本来就是
    /// 独立的管线（设计文档"独立于文本管线"一节）。单文档的 commit 在 tantivy 里
    /// 很轻，OCR 识别本身（百毫秒级）远比它慢，不会成为瓶颈；换来的是实现简单、
    /// 每识别完一张就立刻可搜，不会因为 worker 中途崩溃丢掉一整批还没提交的结果。
    pub(crate) fn stage_and_commit_ocr_result(
        &mut self,
        path: &Path,
        mtime: i64,
        size: u64,
        content: &str,
    ) -> Result<()> {
        self.delete_exact(path);
        add_image_document_with_content(&self.writer, &self.fields, path, mtime, size, content)?;
        self.writer.commit().context("OCR 结果提交失败")?;
        Ok(())
    }

    /// 内部索引句柄，供启动对账枚举当前所有文档时开只读 reader 用。
    /// 对账和实时更新共用同一个 updater、同一个 index，避免开第二个 writer。
    pub(crate) fn index(&self) -> &Index {
        &self.index
    }

    /// 字段句柄，供对账读回文档的 path/mtime/size。
    pub(crate) fn fields(&self) -> &Fields {
        &self.fields
    }

    /// 按精确 path 删一篇文档。删不存在的 term 是空操作，幂等无害。
    fn delete_exact(&self, path: &Path) {
        let term = Term::from_field_text(self.fields.path, &path.to_string_lossy());
        self.writer.delete_term(term);
    }

    /// 删除一个路径及其整棵子树。既删这个 path 本身（它若是文件，删掉文件文档），
    /// 也前缀圈选删掉它名下的全部文档（它若是目录，删掉子树）——一个操作同时覆盖
    /// "删文件"和"删目录"两种情形，所以监听侧分不清是文件还是目录时发这一个就够，
    /// 不用再发一条精确删除（发两条会在防抖队列里按同一 path 合并、互相覆盖）。
    ///
    /// 实现：对 STRING 的 path 字段查 `path == dir` 或 `path ∈ [dir+分隔符, dir+分隔符+U+10FFFF)`，
    /// 收集命中文档逐个 delete_term——用 term 查询而不是逐文件比对，目录再大也只走
    /// 一遍倒排。子树范围末尾补分隔符很关键：圈的是"这个目录里的东西"，不会误伤
    /// 同前缀的兄弟（`log` 不会连 `log2/...` 一起删；精确项也只命中 `log` 自己，
    /// 不会碰到兄弟文件 `log2`）。
    fn delete_tree(&self, path: &Path) -> Result<usize> {
        let exact = path.to_string_lossy().into_owned();
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

        // 手动重载的只读 reader 反映当前已提交状态，删的正是这些已入索引的旧文档。
        // Manual 策略不起后台监听线程。
        let reader: IndexReader = self
            .index
            .reader_builder()
            .reload_policy(ReloadPolicy::Manual)
            .try_into()?;
        let searcher = reader.searcher();
        let hits = searcher.search(&query, &DocSetCollector)?;

        let mut count = 0usize;
        for addr in hits {
            let doc: TantivyDocument = searcher.doc(addr)?;
            if let Some(path) = doc.get_first(self.fields.path).and_then(|v| v.as_str()) {
                let term = Term::from_field_text(self.fields.path, path);
                self.writer.delete_term(term);
                count += 1;
            }
        }
        Ok(count)
    }
}
