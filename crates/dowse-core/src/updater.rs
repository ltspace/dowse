use std::ops::Bound;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result};
use tantivy::collector::DocSetCollector;
use tantivy::query::{BooleanQuery, Occur, Query, RangeQuery, TermQuery};
use tantivy::schema::{IndexRecordOption, Value};
use tantivy::{Index, IndexReader, IndexWriter, ReloadPolicy, TantivyDocument, Term};

use crate::events::{PendingChange, PendingOp};
use crate::indexer::PROGRESS_INTERVAL;
use crate::indexer::{
    add_file_document, add_image_document_with_content, commit_index_tail,
    is_transient_writer_killed, walk_index_files,
};
use crate::ocr::is_image;
use crate::ocr_queue::OcrQueue;
use crate::{Fields, IndexProgress, build_schema, register_tokenizers};

/// 长驻写入端（`IndexUpdater`）撞上杀软扫描瞬时冲突时的重试参数。判据复用
/// `indexer::is_transient_writer_killed`——跟全量重建是同一个根因（见
/// `indexer.rs` 40d8437 的文档），但恢复手段不同：全量重建是"整次重来"（删
/// 目录重建一个全新的 IndexWriter），这里的 IndexWriter 是长期持有的对象，
/// 一旦被 tantivy 判定"已死"就永远死了，唯一的恢复手段是丢弃它未提交的部分、
/// 换一个新的写入端接着用（具体做法见 `reopen_writer` 的文档——不是"旧的先
/// drop 释放锁、新的再抢锁"这条路，那条路有构造性死锁）。重试次数比全量重建
/// 少（6 次而非 10 次）——增量批次/单张 OCR 结果本来就小，不需要像全量重建
/// 那样逐步收窄并发线程数，固定单线程写入本来就是默认；重试之间的退避节奏
/// 跟全量重建一致。
const WRITER_RETRIES: u32 = 6;
const WRITER_RETRY_BASE_DELAY: Duration = Duration::from_millis(300);
const WRITER_RETRY_MAX_DELAY: Duration = Duration::from_secs(3);

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
    ///
    /// 不需要进度直播的调用方走这个薄封装，回调是空操作——真正的实现和进度
    /// 上报都在 `apply_with_progress`，跟 `rebuild_index`/`rebuild_index_with_progress`
    /// 是同一套"薄封装 + 带进度实现"的分工。
    pub fn apply(&mut self, batch: &[PendingChange]) -> Result<BatchOutcome> {
        self.apply_with_progress(batch, |_| {})
    }

    /// 同 `apply`，多一个进度回调：`PendingOp::UpsertTree` 展开成具体文件后，
    /// 每处理 `PROGRESS_INTERVAL` 个文件就报一次累计处理数和当前文件路径——
    /// 多根索引（里程碑 7）"添加根"操作本质就是对新根整目录做一次 UpsertTree，
    /// 复用这条路径的同时需要把过程直播给前端（设计文档"进度直播复用现有
    /// 整套"），单文件的 `Upsert`/删除类操作数量级小，不单独计进度。
    ///
    /// 撞上杀软扫描瞬时冲突（见 `commit_with_retry` 文档）时整批重做——
    /// `upsert_one`/`delete_exact`/`delete_tree` 全部是"先删后加"或者按 term
    /// 精确删除，同一批重复应用结果不变，重做无害；进度计数在重做时会从头
    /// 重新累计，跟 `rebuild_index_with_progress` 重试时的行为一致。
    pub fn apply_with_progress(
        &mut self,
        batch: &[PendingChange],
        mut on_progress: impl FnMut(IndexProgress),
    ) -> Result<BatchOutcome> {
        self.commit_with_retry(|this| {
            let mut outcome = BatchOutcome::default();
            let mut touched_image = false;
            let mut processed = 0usize;
            for change in batch {
                match change.op {
                    PendingOp::Upsert => {
                        this.upsert_one(&change.path, &mut outcome, &mut touched_image)?;
                    }
                    PendingOp::UpsertTree => {
                        // 目录整体新建/移入：真正"这个目录下有哪些文件"的完整 walk
                        // 在这里做（消费侧线程），不在 notify 回调线程里做——大目录
                        // 阻塞秒级会让 OS 的目录变更缓冲溢出丢事件，见
                        // `watch.rs::emit_upsert` 的说明。展开后逐个文件走跟普通
                        // `Upsert` 完全一样的先删后加逻辑。
                        for file in walk_index_files(&change.path) {
                            this.upsert_one(&file, &mut outcome, &mut touched_image)?;
                            processed += 1;
                            if processed % PROGRESS_INTERVAL == 0 {
                                on_progress(IndexProgress {
                                    processed,
                                    path: file.clone(),
                                });
                            }
                        }
                    }
                    PendingOp::Remove => {
                        this.delete_exact(&change.path);
                        outcome.removed += 1;
                    }
                    PendingOp::RemoveTree => {
                        outcome.removed += this.delete_tree(&change.path)?;
                    }
                }
            }
            // 顺序不变量同 `commit_index_tail` 的文档：队列必须先于 commit 落盘，
            // 崩溃后重复识别一张 pending 图片是幂等无害的，反过来会丢数据。
            let index_dir = this.index_dir.clone();
            commit_index_tail(
                || {
                    if touched_image {
                        // 这一批里确实有图片新增/变更被塞进了 OCR 队列的内存态，落一次盘——
                        // 没有图片的批次（大多数文本编辑场景）不用为这个多写一次文件。
                        OcrQueue::for_index_dir(&index_dir)
                            .save()
                            .context("保存 OCR 队列状态失败")
                    } else {
                        Ok(())
                    }
                },
                || this.writer.commit().map(|_| ()).context("增量提交失败"),
            )?;
            Ok(outcome)
        })
    }

    /// 单个文件的先删后加：`PendingOp::Upsert` 和展开后的 `PendingOp::UpsertTree`
    /// 都走这里，保证两条路径落进索引、计数的逻辑完全一致。
    fn upsert_one(
        &self,
        path: &Path,
        outcome: &mut BatchOutcome,
        touched_image: &mut bool,
    ) -> Result<()> {
        // 先删后加，天然幂等：同一 path 无论之前有没有、内容变没变，
        // 结果都是"索引里恰好有这一篇的最新版本"。
        self.delete_exact(path);
        if is_image(path) {
            *touched_image = true;
        }
        if add_file_document(&self.writer, &self.fields, path, &self.index_dir)? {
            outcome.upserted += 1;
        } else {
            // 抽不出文本（不支持的格式/损坏/文件已消失）：先删的动作已生效，
            // 相当于把这篇从索引里拿掉。计一次 skip，不中断整批。
            outcome.skipped += 1;
        }
        Ok(())
    }

    /// OCR worker 写回一批图片的最终识别结果：每张先删旧文档、写入新内容，整批
    /// 攒完只 commit 一次。批量提交（而不是曾经的"每识别完一张就单独 commit
    /// 一次"）是 v0.6.1 的修复：15k 张图片对应 15k 次重量级 tantivy commit（每次
    /// 都重写段元文件、可能触发合并）会把磁盘 IO 打爆，现场表现为窗口唤起卡顿、
    /// 进程在高频建删文件时更容易撞上杀软实时扫描而崩溃。批次大小/时间窗口由
    /// 调用方（`ocr_worker.rs`）控制，这里只管"给一批、提交一批"。
    ///
    /// 空批直接返回 Ok，不做任何写入端操作。
    pub(crate) fn stage_and_commit_ocr_batch(
        &mut self,
        items: &[(PathBuf, i64, u64, String)],
    ) -> Result<()> {
        if items.is_empty() {
            return Ok(());
        }
        self.commit_with_retry(|this| {
            for (path, mtime, size, content) in items {
                this.delete_exact(path);
                add_image_document_with_content(
                    &this.writer,
                    &this.fields,
                    path,
                    *mtime,
                    *size,
                    content,
                )?;
            }
            this.writer.commit().context("OCR 批量结果提交失败")?;
            Ok(())
        })
    }

    /// 在长驻写入端上执行一次"写入 + commit"，撞上杀软扫描瞬时冲突（判据见
    /// `indexer::is_transient_writer_killed`，跟全量重建共用同一套探测逻辑）
    /// 时重开一个新 `IndexWriter` 再整次重做。`op` 每次重试都会被完整重新
    /// 调用一遍——调用方（`apply`/`stage_and_commit_ocr_batch`）内部全是
    /// "先删后加"或者按 term 精确删除，重复调用结果不变，天然满足这个前提。
    fn commit_with_retry<T>(&mut self, mut op: impl FnMut(&mut Self) -> Result<T>) -> Result<T> {
        let mut delay = WRITER_RETRY_BASE_DELAY;
        for attempt in 1..=WRITER_RETRIES {
            match op(self) {
                Ok(value) => return Ok(value),
                Err(err) if attempt < WRITER_RETRIES && is_transient_writer_killed(&err) => {
                    eprintln!(
                        "索引写入端撞上瞬时的杀软扫描冲突（第 {attempt}/{WRITER_RETRIES} 次，\
                         等待 {delay:?} 后重开写入端重试）: {err}"
                    );
                    std::thread::sleep(delay);
                    // 重开失败就直接把这次 `commit_with_retry` 判失败，不再把它
                    // 塞回循环下一轮凑数：`reopen_writer` 内部的 `IndexWriter::
                    // rollback()` 会先把写锁对象从旧 writer 身上取走，一旦
                    // `rollback()` 因为别的原因失败（理论上限于 tantivy 内部再
                    // 构造新 writer 时的异常，正常场景不会在这一步撞杀软特征），
                    // 旧 writer 就变成了"锁已经没了、但还挂在 self.writer 上"
                    // 的半吊子状态——再调用一次 `rollback()` 会撞上 tantivy 自己
                    // 那句 `.expect("The IndexWriter does not have any lock")`
                    // 直接 panic，而不是回一个能重试的 `Err`。这里提前 `?` 出去
                    // 换掉那条"继续循环、可能第二次调用 rollback 而 panic"的路。
                    // 提前放弃这一次调用是安全的：`apply`/`apply_with_progress`
                    // 失败时，调用方 `run_watch`（`watch.rs::flush_batch`）本来
                    // 就会把这一批变更退回防抖队列、留到下一个监听窗口重新提交，
                    // 是比这里内部循环更宽裕的外层重试。
                    self.reopen_writer()?;
                    delay = (delay * 2).min(WRITER_RETRY_MAX_DELAY);
                    continue;
                }
                Err(err) => return Err(err),
            }
        }
        unreachable!("循环要么在 Ok 分支返回，要么在最后一次尝试的 Err 分支返回")
    }

    /// 丢弃当前（已被 tantivy 判定"已死"的）`IndexWriter` 里未提交的部分，
    /// 换一个新的写入端接着用。
    ///
    /// 不能像最初设想的那样"新开一个 `IndexWriter`、赋值时让旧的自然 drop"：
    /// `self.writer = self.index.writer(..)?` 这一行，右边的 `self.index.writer(..)`
    /// 会先执行完（尝试拿写锁），赋值动作（连带旧 writer 的 drop、写锁文件释放）
    /// 要等右边成功返回之后才发生——旧 writer 这时候还活着、锁文件还在，新写入端
    /// 永远抢不到那把锁，`Index::writer` 用的是非阻塞锁（`INDEX_WRITER_LOCK.is_blocking
    /// == false`，见 tantivy `directory_lock.rs`），失败也不重试，当场就是
    /// `LockBusy`。这不是杀软/CI 环境的偶发问题，本机非管理员环境对着一个健康
    /// 存活的 writer 调用一次就能稳定复现，是重开路径本身的构造性死锁。
    ///
    /// 改用 tantivy 内置的 `IndexWriter::rollback()`：它把写锁文件对象从旧
    /// writer 身上"拿走"（而不是释放重抢），直接原地传给新构造的 writer 复用，
    /// 全程不涉及锁文件的释放/重新获取，天然绕开上面那个死锁。副作用是丢弃这个
    /// writer 上一次 commit 之后所有未提交的操作——`commit_with_retry` 的设计本来
    /// 就要求 `op` 整批可重放（先删后加/精确删除，见调用处文档），丢弃半成品批次
    /// 后下一轮重新跑一遍结果不变，这个副作用是安全的。
    fn reopen_writer(&mut self) -> Result<()> {
        self.writer.rollback().context("重开索引写入端失败")?;
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::{PendingChange, PendingOp};
    use crate::indexer::rebuild_index;

    /// `reopen_writer` 曾经用"新开一个 `IndexWriter`、让旧的在赋值时自然 drop"
    /// 的写法，实测无论 writer 是否真的"已死"，只要它还没被 drop 就还攥着写锁
    /// 文件——`Index::writer` 用的是非阻塞锁，抢不到当场返回 `LockBusy`，不重试。
    /// 这个用例不需要管理员权限、不依赖杀软/CI 环境的偶发时机，一个健康存活的
    /// writer 就能稳定复现那个构造性死锁，用来钉死回归。
    #[test]
    fn reopen_writer_succeeds_and_stays_usable() -> Result<()> {
        let index_dir = tempfile::tempdir()?;
        let target = tempfile::tempdir()?;
        std::fs::write(target.path().join("seed.md"), "种子内容 seedmarker")?;
        rebuild_index(index_dir.path(), target.path())?;

        let mut updater = IndexUpdater::open(index_dir.path())?;
        updater.reopen_writer()?;

        // 重开后的写入端不能只是"没报错"，还得真的能干活：跑一次完整的
        // 增量写入 + commit，确认不是停在半死状态。
        let added = target.path().join("added.md");
        std::fs::write(&added, "新增内容 freshmarker")?;
        let batch = vec![PendingChange {
            path: added.clone(),
            op: PendingOp::Upsert,
        }];
        let outcome = updater.apply(&batch)?;
        assert_eq!(
            outcome.upserted, 1,
            "重开后的写入端应能正常完成一次增量提交"
        );
        Ok(())
    }
}
