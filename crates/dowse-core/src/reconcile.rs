use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use anyhow::Result;
use tantivy::collector::DocSetCollector;
use tantivy::query::AllQuery;
use tantivy::schema::Value;
use tantivy::{ReloadPolicy, TantivyDocument};

use crate::events::{PendingChange, PendingOp};
use crate::extract::is_extractable;
use crate::indexer::{file_stat, walk_index_files};
use crate::ocr::is_image;
use crate::updater::IndexUpdater;

/// 一次启动对账的差异统计。
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct ReconcileStats {
    /// 磁盘有、索引无：新增。
    pub added: usize,
    /// 两边都有但 mtime/size 变了：修改。
    pub modified: usize,
    /// 索引有、磁盘无：删除。
    pub removed: usize,
}

/// 启动对账：程序没跑的时候文件照样在变，启动时把索引追平文件系统的实际状态。
///
/// 对一个索引根做 (path, mtime, size) 三态比对：
/// - 磁盘有、索引无 → 新增
/// - 两边都有但 mtime/size 变了 → 修改
/// - 索引有、磁盘无 → 删除
///
/// 比对（读索引 + 扫盘）不碰写入端，落地复用传入 updater 的单一 writer；搜索侧是
/// 完全独立的只读 reader，对账进行时索引照常可搜、不会被锁死（旧数据可搜好过不可搜）。
/// 宿主应把本函数放后台线程、在挂上实时监听之前先跑一遍。
pub fn reconcile(root: &Path, updater: &mut IndexUpdater) -> Result<ReconcileStats> {
    // 1. 快照索引里 root 下每篇文档的 (mtime, size)
    let indexed = snapshot_indexed(updater, root)?;

    // 2. 扫盘，边走边比对，攒出新增/修改两类差异
    let mut batch: Vec<PendingChange> = Vec::new();
    let mut stats = ReconcileStats::default();
    let mut seen: HashSet<PathBuf> = HashSet::with_capacity(indexed.len());

    // 图片和文本文件共用同一套 (path,mtime,size) 三态比对——图片这条腿只是把
    // "有没有变化"判断出来，真正的 OCR 识别延后到 updater.apply() 内部按 upsert
    // 落到 OCR 队列，这里不碰 OCR 相关逻辑。
    for path in walk_index_files(root).filter(|p| is_extractable(p) || is_image(p)) {
        let Some((mtime, size)) = file_stat(&path) else {
            continue;
        };
        seen.insert(path.clone());
        match indexed.get(&path) {
            None => {
                batch.push(PendingChange {
                    path,
                    op: PendingOp::Upsert,
                });
                stats.added += 1;
            }
            Some(&(indexed_mtime, indexed_size)) => {
                if indexed_mtime != mtime || indexed_size != size {
                    batch.push(PendingChange {
                        path,
                        op: PendingOp::Upsert,
                    });
                    stats.modified += 1;
                }
            }
        }
    }

    // 3. 索引里有、扫盘没见到 → 删除
    for path in indexed.keys() {
        if !seen.contains(path) {
            batch.push(PendingChange {
                path: path.clone(),
                op: PendingOp::Remove,
            });
            stats.removed += 1;
        }
    }

    if !batch.is_empty() {
        updater.apply(&batch)?;
    }
    Ok(stats)
}

/// 孤儿文档清理（多根索引，里程碑 7）：索引里若存在 path 不属于 `roots`
/// 任一根的文档，直接删除。
///
/// 这是"添加根"/"移除根"两个操作中途崩溃的兜底（设计文档"边界与失败"一节）：
/// 添加根时 meta 要等目录树 upsert **完成后**才写入根，半路崩溃意味着索引里
/// 已经多出一批文档、但 meta 还不认它们属于任何根；移除根时 meta **先于**
/// 文档删除更新，半路崩溃意味着索引里还残留着一批文档、但 meta 已经不认
/// 它们了。两种情形殊途同归：都留下了"在索引里、但不属于任何已注册根"的
/// 孤儿文档，这条规则统一收尾，下次启动对账时随 `watch_roots_auto` 一并跑掉。
///
/// `roots` 为空时不做任何清理，直接返回 0——空根列表通常意味着索引还没建
/// 或者刚被清空，不该把这解读成"什么都不属于任何根、全部删掉"。
pub fn reconcile_orphans(roots: &[PathBuf], updater: &mut IndexUpdater) -> Result<usize> {
    if roots.is_empty() {
        return Ok(0);
    }

    let reader = updater
        .index()
        .reader_builder()
        .reload_policy(ReloadPolicy::Manual)
        .try_into()?;
    let searcher = reader.searcher();
    let fields = updater.fields();

    let hits = searcher.search(&AllQuery, &DocSetCollector)?;
    let mut batch: Vec<PendingChange> = Vec::new();
    for addr in hits {
        let doc: TantivyDocument = searcher.doc(addr)?;
        let Some(path) = doc.get_first(fields.path).and_then(|v| v.as_str()) else {
            continue;
        };
        let pbuf = PathBuf::from(path);
        if !roots.iter().any(|r| pbuf.starts_with(r)) {
            batch.push(PendingChange {
                path: pbuf,
                op: PendingOp::Remove,
            });
        }
    }

    let removed = batch.len();
    if !batch.is_empty() {
        updater.apply(&batch)?;
    }
    Ok(removed)
}

/// 读出索引里 root 前缀下所有文档的 (path -> (mtime, size))。
/// 用手动重载的只读 reader（不起后台线程），AllQuery 收集全部存活文档。
fn snapshot_indexed(updater: &IndexUpdater, root: &Path) -> Result<HashMap<PathBuf, (i64, u64)>> {
    let reader = updater
        .index()
        .reader_builder()
        .reload_policy(ReloadPolicy::Manual)
        .try_into()?;
    let searcher = reader.searcher();
    let fields = updater.fields();

    let hits = searcher.search(&AllQuery, &DocSetCollector)?;
    let mut map = HashMap::with_capacity(hits.len());
    for addr in hits {
        let doc: TantivyDocument = searcher.doc(addr)?;
        let Some(path) = doc.get_first(fields.path).and_then(|v| v.as_str()) else {
            continue;
        };
        let pbuf = PathBuf::from(path);
        // 只对账这个根下的文档，多个根各扫各的，互不干扰。
        if !pbuf.starts_with(root) {
            continue;
        }
        let mtime = doc
            .get_first(fields.mtime)
            .and_then(|v| v.as_i64())
            .unwrap_or(0);
        let size = doc
            .get_first(fields.size)
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        map.insert(pbuf, (mtime, size));
    }
    Ok(map)
}
