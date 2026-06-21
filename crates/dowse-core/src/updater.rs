use std::ops::Bound;
use std::path::Path;

use anyhow::{Context, Result};
use tantivy::collector::DocSetCollector;
use tantivy::query::RangeQuery;
use tantivy::schema::Value;
use tantivy::{Index, IndexReader, IndexWriter, ReloadPolicy, TantivyDocument, Term};

use crate::events::{PendingChange, PendingOp};
use crate::indexer::add_file_document;
use crate::{build_schema, register_tokenizers, Fields};

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
}

impl IndexUpdater {
    /// 打开已有索引的写入端。先校验 schema 版本，不匹配就报错提示重建。
    pub fn open(index_dir: &Path) -> Result<Self> {
        crate::meta::ensure_schema_version(index_dir)?;
        let index = Index::open_in_dir(index_dir)
            .context("打不开索引目录，先建一次索引再监听")?;
        register_tokenizers(&index);
        let (_, fields) = build_schema();
        // 50MB 写缓冲：增量场景一批就几个到几千个文件，不用像全量重建那样开 200MB。
        let writer = index.writer(50 * 1024 * 1024)?;
        Ok(Self {
            index,
            writer,
            fields,
        })
    }

    /// 处理一批防抖合并后的变更，最后 commit 一次。
    /// commit 失败返回 Err，调用方（run_watch）负责把这批退回队列下轮重试。
    pub fn apply(&mut self, batch: &[PendingChange]) -> Result<BatchOutcome> {
        let mut outcome = BatchOutcome::default();
        for change in batch {
            match change.op {
                PendingOp::Upsert => {
                    // 先删后加，天然幂等：同一 path 无论之前有没有、内容变没变，
                    // 结果都是"索引里恰好有这一篇的最新版本"。
                    self.delete_exact(&change.path);
                    if add_file_document(&self.writer, &self.fields, &change.path)? {
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
                    outcome.removed += self.delete_prefix(&change.path)?;
                }
            }
        }
        self.writer.commit().context("增量提交失败")?;
        Ok(outcome)
    }

    /// 按精确 path 删一篇文档。删不存在的 term 是空操作，幂等无害。
    fn delete_exact(&self, path: &Path) {
        let term = Term::from_field_text(self.fields.path, &path.to_string_lossy());
        self.writer.delete_term(term);
    }

    /// 目录删除：前缀圈选删掉整棵子树。
    ///
    /// path 字段是 STRING（整条路径存成一个 term），对它做 `[dir+分隔符, dir+分隔符+U+10FFFF)`
    /// 的范围查询，一次圈出该目录下的全部文档，逐个 delete_term——不是逐文件比对，
    /// 目录再大也只走一遍倒排。末尾补分隔符很关键：圈的是"这个目录里的东西"，
    /// 不会误伤同前缀的兄弟目录（`log` 不会连 `log2/...` 一起删）。
    fn delete_prefix(&self, dir: &Path) -> Result<usize> {
        let mut prefix = dir.to_string_lossy().into_owned();
        if !prefix.ends_with(std::path::MAIN_SEPARATOR) {
            prefix.push(std::path::MAIN_SEPARATOR);
        }
        let upper = format!("{prefix}\u{10FFFF}");

        let lower_term = Term::from_field_text(self.fields.path, &prefix);
        let upper_term = Term::from_field_text(self.fields.path, &upper);
        let query = RangeQuery::new(Bound::Included(lower_term), Bound::Excluded(upper_term));

        // 用一个手动重载的只读 reader 收集命中：反映的是当前已提交的索引状态，
        // 目录删除针对的正是这些已入索引的旧文档。Manual 策略不起后台监听线程。
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
