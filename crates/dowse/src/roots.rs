//! 多根索引（里程碑 7）的三个核心操作：添加根 / 移除根 / 重建单根。
//!
//! 语义表见设计文档"核心操作语义"一节。三个函数都要求调用方传入一个已经
//! 打开的 `&mut IndexUpdater`，而不是自己 `IndexUpdater::open` 一份——一个
//! 索引同一时刻只能有一个 `IndexWriter`（`updater.rs` 的文档），常驻托盘
//! 程序的实时监听线程本来就持有一份长期存活的 `Arc<Mutex<IndexUpdater>>`，
//! 这几个操作必须复用它，不能另开一个写入端跟它抢锁。CLI/测试等没有常驻
//! 监听的场景可以现开一个 `IndexUpdater::open` 传进来，用完随手 drop。

use std::path::Path;

use anyhow::{Context, Result};

use crate::IndexProgress;
use crate::events::{PendingChange, PendingOp};
use crate::meta;
use crate::ocr_queue::OcrQueue;
use crate::updater::IndexUpdater;

/// 添加根的统计结果，跟 `IndexStats`（全量重建）同一口径：收录/跳过文件数。
#[derive(Debug, Clone, Copy)]
pub struct AddRootStats {
    /// 这个根名下成功收录进索引的文件数。
    pub indexed: usize,
    /// 这个根名下被跳过的文件数（无法抽取或不在收录范围内）。
    pub skipped: usize,
}

/// 移除根的统计结果：从索引里删掉的文档数。
#[derive(Debug, Clone, Copy)]
pub struct RemoveRootStats {
    /// 从索引里删掉的文档数。
    pub removed: usize,
}

/// 添加一个根：不动现有索引，对新根做一次"目录树 upsert"（遍历 + 先删后加，
/// 幂等），完成后把根追加进 meta。不需要进度直播的调用方走这个薄封装。
pub fn add_root(index_dir: &Path, root: &Path, updater: &mut IndexUpdater) -> Result<AddRootStats> {
    add_root_with_progress(index_dir, root, updater, |_| {})
}

/// 同 `add_root`，多一个进度回调，直播这个根的目录树 upsert 进度——设计文档
/// "进度直播复用现有整套"要求这条路径也能像全量重建一样把过程推给前端。
///
/// 顺序不变量（设计文档"边界与失败"）：**先**完成目录树 upsert，**后**把根
/// 写进 meta。半路崩溃时 meta 还不认这批刚写入索引的文档属于任何根，下次
/// 启动对账时会被孤儿清理规则（`reconcile::reconcile_orphans`）当垃圾删掉，
/// 不会留下一个"文档在、根不在"的幽灵状态。
pub fn add_root_with_progress(
    index_dir: &Path,
    root: &Path,
    updater: &mut IndexUpdater,
    on_progress: impl FnMut(IndexProgress),
) -> Result<AddRootStats> {
    let root = root
        .canonicalize()
        .with_context(|| format!("目录不存在: {}", root.display()))?;

    // 先做嵌套校验，失败就直接返回，不碰索引——嵌套是一个纯粹基于路径列表
    // 的判断，没必要先把整棵目录树扫一遍才发现选错了目录。
    let existing = meta::registered_roots(index_dir)?;
    meta::assert_no_root_nesting(&existing, &root)?;

    // 复用增量更新器的 UpsertTree 路径：跟监听侧"目录整体移入监听范围"走的
    // 是完全同一段代码（先删后加、一次 commit），保证这里落进索引的字段
    // 跟实时监听/全量重建完全一致。
    let batch = [PendingChange {
        path: root.clone(),
        op: PendingOp::UpsertTree,
    }];
    let outcome = updater.apply_with_progress(&batch, on_progress)?;

    meta::append_root(index_dir, &root)?;

    Ok(AddRootStats {
        indexed: outcome.upserted,
        skipped: outcome.skipped,
    })
}

/// 移除一个根：前缀圈选删除该根名下的全部文档，OCR 队列 compact 掉该根的
/// 条目，roots 里移除这一项。`root` 必须是 `registered_roots` 返回值里的
/// 原样一项（不做 canonicalize/存在性校验）——移除本来就要覆盖"根所在目录
/// 已经从磁盘上消失"这种场景，不能要求它此刻还能被 stat 到。
pub fn remove_root(
    index_dir: &Path,
    root: &Path,
    updater: &mut IndexUpdater,
) -> Result<RemoveRootStats> {
    // 顺序不变量：先把根从 meta 移除，再删文档——半路崩溃时残留文档已经
    // 不属于任何注册根，由孤儿清理规则兜底，不会重复计数也不会遗留幽灵根。
    meta::remove_root_from_meta(index_dir, root)?;

    let batch = [PendingChange {
        path: root.to_path_buf(),
        op: PendingOp::RemoveTree,
    }];
    let outcome = updater.apply(&batch)?;

    // OCR 队列跟着瘦身：用移除后的最新根集合裁剪，不让这个根的历史 pending/
    // processed 条目永久堆积（复用 rebuild_index 已有的 compact 逻辑）。
    let remaining = meta::registered_roots(index_dir)?;
    let queue = OcrQueue::for_index_dir(index_dir);
    queue.compact(&remaining);
    queue.save().context("保存 OCR 队列状态失败")?;

    Ok(RemoveRootStats {
        removed: outcome.removed,
    })
}

/// 重建单根 = 移除根 + 添加根的组合，全部复用上面两个操作（设计文档"核心
/// 操作语义"一节）。给已经注册过的根一次干净的重新收录，而不用先手动移除
/// 再手动添加。
pub fn rebuild_root(
    index_dir: &Path,
    root: &Path,
    updater: &mut IndexUpdater,
) -> Result<AddRootStats> {
    rebuild_root_with_progress(index_dir, root, updater, |_| {})
}

/// 同 `rebuild_root`，多一个进度回调，直播添加阶段的目录树 upsert 进度。
pub fn rebuild_root_with_progress(
    index_dir: &Path,
    root: &Path,
    updater: &mut IndexUpdater,
    on_progress: impl FnMut(IndexProgress),
) -> Result<AddRootStats> {
    remove_root(index_dir, root, updater)?;
    add_root_with_progress(index_dir, root, updater, on_progress)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(dir: &Path, name: &str, content: &str) {
        std::fs::write(dir.join(name), content).unwrap();
    }

    #[test]
    fn add_root_indexes_new_root_without_touching_existing() -> Result<()> {
        let index_dir = tempfile::tempdir()?;
        let a = tempfile::Builder::new().prefix("dowse-a-").tempdir()?;
        write(a.path(), "a.md", "根 A 的内容 apricot");
        crate::rebuild_index(index_dir.path(), a.path())?;

        let b = tempfile::Builder::new().prefix("dowse-b-").tempdir()?;
        write(b.path(), "b.md", "根 B 的内容 blueberry");

        let mut updater = IndexUpdater::open(index_dir.path())?;
        let stats = add_root(index_dir.path(), b.path(), &mut updater)?;
        assert_eq!(stats.indexed, 1);
        drop(updater);

        let searcher = crate::Searcher::open(index_dir.path())?;
        assert_eq!(
            searcher.search("apricot", 10)?.len(),
            1,
            "A 的内容不应受影响"
        );
        assert_eq!(searcher.search("blueberry", 10)?.len(), 1, "B 的内容应可搜");

        let roots = meta::registered_roots(index_dir.path())?;
        assert_eq!(roots.len(), 2, "roots 应该追加而不是覆盖");
        Ok(())
    }

    #[test]
    fn add_root_rejects_nested_directory() -> Result<()> {
        let index_dir = tempfile::tempdir()?;
        let a = tempfile::Builder::new().prefix("dowse-a-").tempdir()?;
        write(a.path(), "a.md", "内容");
        crate::rebuild_index(index_dir.path(), a.path())?;

        let nested = a.path().join("sub");
        std::fs::create_dir_all(&nested)?;

        let mut updater = IndexUpdater::open(index_dir.path())?;
        let err = add_root(index_dir.path(), &nested, &mut updater).expect_err("子目录应该被拒绝");
        assert!(err.to_string().contains("嵌套"));

        let roots = meta::registered_roots(index_dir.path())?;
        assert_eq!(roots.len(), 1, "校验失败不应该改动 roots");
        Ok(())
    }

    #[test]
    fn remove_root_deletes_only_that_roots_documents() -> Result<()> {
        let index_dir = tempfile::tempdir()?;
        let a = tempfile::Builder::new().prefix("dowse-a-").tempdir()?;
        write(a.path(), "a.md", "根 A 的内容 apricot");
        crate::rebuild_index(index_dir.path(), a.path())?;

        let b = tempfile::Builder::new().prefix("dowse-b-").tempdir()?;
        write(b.path(), "b.md", "根 B 的内容 blueberry");

        let mut updater = IndexUpdater::open(index_dir.path())?;
        add_root(index_dir.path(), b.path(), &mut updater)?;
        let b_canonical = b.path().canonicalize()?;

        let stats = remove_root(index_dir.path(), &b_canonical, &mut updater)?;
        assert_eq!(stats.removed, 1);
        drop(updater);

        let searcher = crate::Searcher::open(index_dir.path())?;
        assert_eq!(searcher.search("apricot", 10)?.len(), 1, "A 不受影响");
        assert_eq!(searcher.search("blueberry", 10)?.len(), 0, "B 应该被删干净");

        let roots = meta::registered_roots(index_dir.path())?;
        assert_eq!(roots.len(), 1);
        Ok(())
    }

    #[test]
    fn rebuild_root_reindexes_a_single_root() -> Result<()> {
        let index_dir = tempfile::tempdir()?;
        let a = tempfile::Builder::new().prefix("dowse-a-").tempdir()?;
        write(a.path(), "a.md", "根 A 的初始内容 apricot");
        crate::rebuild_index(index_dir.path(), a.path())?;

        // 停机期间根 A 下的内容发生了变化（新增一篇），重建单根应该追平。
        write(a.path(), "extra.md", "新增文件 blueberry");
        // remove_root 按 registered_roots() 里的原样值精确匹配（见函数文档），
        // rebuild_index 存的是未经 canonicalize 的 target_dir，这里同样取
        // registered_roots() 的原样值，而不是自己重新 canonicalize 一遍。
        let a_registered = meta::registered_roots(index_dir.path())?
            .into_iter()
            .next()
            .expect("应该已经注册了一个根");

        let mut updater = IndexUpdater::open(index_dir.path())?;
        let stats = rebuild_root(index_dir.path(), &a_registered, &mut updater)?;
        assert_eq!(stats.indexed, 2);
        drop(updater);

        let searcher = crate::Searcher::open(index_dir.path())?;
        assert_eq!(searcher.search("apricot", 10)?.len(), 1);
        assert_eq!(searcher.search("blueberry", 10)?.len(), 1);

        let roots = meta::registered_roots(index_dir.path())?;
        assert_eq!(roots.len(), 1, "重建单根不应该改变根的数量");
        Ok(())
    }
}
