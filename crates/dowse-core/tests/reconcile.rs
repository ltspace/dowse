//! 启动对账集成测试：建索引后，不通过监听、直接在文件系统层面改动文件
//! （模拟程序停机期间发生的变更），触发对账，断言索引追平文件系统实际状态。

use std::path::Path;

use anyhow::Result;
use dowse_core::{
    IndexUpdater, ReconcileStats, Searcher, add_root, rebuild_index, reconcile, reconcile_orphans,
};

mod common;

fn target_dir() -> tempfile::TempDir {
    tempfile::Builder::new()
        .prefix("dowse-rec-")
        .tempdir()
        .unwrap()
}

fn count_hits(index_dir: &Path, query: &str) -> usize {
    let searcher = Searcher::open(index_dir).unwrap();
    searcher.search(query, 50).unwrap().len()
}

#[test]
fn reconcile_catches_offline_add_modify_delete() -> Result<()> {
    let index_dir = tempfile::tempdir()?;
    let target = target_dir();

    // 初始三篇
    let a = target.path().join("a.md");
    let b = target.path().join("b.md");
    let c = target.path().join("c.md");
    std::fs::write(&a, "甲文件原始内容 apricot")?;
    std::fs::write(&b, "乙文件将被删除 blueberry")?;
    std::fs::write(&c, "丙文件保持不变 cherry")?;

    rebuild_index(index_dir.path(), target.path())?;
    assert_eq!(count_hits(index_dir.path(), "apricot"), 1);
    assert_eq!(count_hits(index_dir.path(), "blueberry"), 1);

    // —— 模拟程序停机期间的文件系统变更（不走监听）——
    // 新增 d.md；改 a.md（内容和长度都变，size 一定不同，不依赖 mtime 精度）；删 b.md
    let d = target.path().join("d.md");
    std::fs::write(&d, "丁文件是新增的 durian")?;
    std::fs::write(&a, "甲文件被改成了完全不同且更长的一段内容 dragonfruit")?;
    std::fs::remove_file(&b)?;

    // —— 触发对账 ——
    let mut updater = IndexUpdater::open(index_dir.path())?;
    let stats = reconcile(target.path(), &mut updater)?;
    assert_eq!(
        stats,
        ReconcileStats {
            added: 1,
            modified: 1,
            removed: 1,
        },
        "对账应恰好识别出 1 增 1 改 1 删"
    );

    // —— 断言索引已追平文件系统实际状态 ——
    assert_eq!(count_hits(index_dir.path(), "durian"), 1, "新增文件应可搜");
    assert_eq!(
        count_hits(index_dir.path(), "dragonfruit"),
        1,
        "改后内容应可搜"
    );
    assert_eq!(
        count_hits(index_dir.path(), "apricot"),
        0,
        "改前内容应搜不到"
    );
    assert_eq!(
        count_hits(index_dir.path(), "blueberry"),
        0,
        "已删文件应搜不到"
    );
    assert_eq!(
        count_hits(index_dir.path(), "cherry"),
        1,
        "未变文件照常可搜"
    );

    // 显式 drop 掉持有索引写入端句柄的 updater，再走重试退避删临时目录——
    // Windows 下 tantivy 合并线程释放句柄有滞后，直接让 TempDir 隐式 drop
    // 偶尔会 flaky（PermissionDenied）。
    drop(updater);
    common::close_tempdir_retrying(index_dir);
    common::close_tempdir_retrying(target);
    Ok(())
}

#[test]
fn reconcile_on_unchanged_index_is_a_noop() -> Result<()> {
    let index_dir = tempfile::tempdir()?;
    let target = target_dir();
    std::fs::write(target.path().join("x.md"), "内容 elderberry")?;
    std::fs::write(target.path().join("y.md"), "内容 fig")?;
    rebuild_index(index_dir.path(), target.path())?;

    // 什么都不改，对账应识别出零差异
    let mut updater = IndexUpdater::open(index_dir.path())?;
    let stats = reconcile(target.path(), &mut updater)?;
    assert_eq!(stats, ReconcileStats::default(), "没有变更时对账应是空操作");

    assert_eq!(count_hits(index_dir.path(), "elderberry"), 1);
    assert_eq!(count_hits(index_dir.path(), "fig"), 1);

    drop(updater);
    common::close_tempdir_retrying(index_dir);
    common::close_tempdir_retrying(target);
    Ok(())
}

/// 孤儿文档清理（多根索引，里程碑 7）：索引里若有文档的 path 不属于给定
/// roots 任一个前缀，`reconcile_orphans` 应该把它删掉；属于某个根的文档
/// 不受影响。
#[test]
fn reconcile_orphans_removes_docs_outside_all_roots() -> Result<()> {
    let index_dir = tempfile::tempdir()?;
    let root = target_dir();
    std::fs::write(root.path().join("kept.md"), "内容 huckleberry")?;
    rebuild_index(index_dir.path(), root.path())?;

    // 手工构造一个"孤儿"文档：直接用 IndexUpdater 塞一篇不属于任何注册根的
    // 文档——等价于"添加根 B 半路崩溃、meta 还没来得及认领这批文档"的现场。
    let orphan_dir = target_dir();
    let orphan_path = orphan_dir.path().join("orphan.md");
    std::fs::write(&orphan_path, "孤儿内容 juniper")?;

    let mut updater = IndexUpdater::open(index_dir.path())?;
    updater.apply(&[dowse_core::PendingChange {
        path: orphan_path.clone(),
        op: dowse_core::PendingOp::Upsert,
    }])?;
    assert_eq!(
        count_hits(index_dir.path(), "juniper"),
        1,
        "孤儿文档应该先入了索引"
    );

    let removed = reconcile_orphans(&[root.path().to_path_buf()], &mut updater)?;
    assert_eq!(removed, 1, "应该恰好清掉这一篇孤儿文档");

    assert_eq!(
        count_hits(index_dir.path(), "juniper"),
        0,
        "孤儿文档应该被清掉"
    );
    assert_eq!(
        count_hits(index_dir.path(), "huckleberry"),
        1,
        "注册根内的文档不受影响"
    );

    drop(updater);
    common::close_tempdir_retrying(index_dir);
    common::close_tempdir_retrying(root);
    common::close_tempdir_retrying(orphan_dir);
    Ok(())
}

/// 验收清单第 4 条："加根中途杀进程重启：对账后 B 完整可搜，无重复文档"。
/// 模拟"目录树 upsert 已经把 B 的文档写进索引，但进程在 append_root 之前
/// 崩溃"——重启后走一次孤儿清理会先把这批未被 meta 认领的文档删掉，
/// 随后正常的 `add_root` 重新收录 B，结果应该是"B 完整可搜、且不重复"。
#[test]
fn crash_mid_add_root_then_restart_reconciles_to_clean_state() -> Result<()> {
    let index_dir = tempfile::tempdir()?;
    let a = target_dir();
    std::fs::write(a.path().join("a.md"), "根 A 的内容 apricot")?;
    rebuild_index(index_dir.path(), a.path())?;

    let b = target_dir();
    std::fs::write(b.path().join("b1.md"), "根 B 文件一 blueberry")?;
    std::fs::write(b.path().join("b2.md"), "根 B 文件二 boysenberry")?;

    // —— 模拟崩溃：只做了目录树 upsert，没有走到 append_root ——
    let mut updater = IndexUpdater::open(index_dir.path())?;
    updater.apply(&[dowse_core::PendingChange {
        path: b.path().to_path_buf(),
        op: dowse_core::PendingOp::UpsertTree,
    }])?;
    assert_eq!(
        count_hits(index_dir.path(), "blueberry"),
        1,
        "崩溃前 B 的文档已经落了索引"
    );

    // —— 重启：先跑孤儿清理（B 不在任何注册根里，应该被当孤儿删掉）——
    let removed = reconcile_orphans(&[a.path().to_path_buf()], &mut updater)?;
    assert_eq!(removed, 2, "两篇未被 meta 认领的 B 文档应该被清掉");
    assert_eq!(count_hits(index_dir.path(), "blueberry"), 0);

    // —— 用户重新触发添加根 B：正常走 add_root，应该完整可搜、不重复 ——
    add_root(index_dir.path(), b.path(), &mut updater)?;
    assert_eq!(
        count_hits(index_dir.path(), "blueberry"),
        1,
        "重新添加后 B 应完整可搜"
    );
    assert_eq!(count_hits(index_dir.path(), "boysenberry"), 1);
    assert_eq!(count_hits(index_dir.path(), "apricot"), 1, "A 全程不受影响");

    let roots = dowse_core::registered_roots(index_dir.path())?;
    assert_eq!(roots.len(), 2, "最终应该恰好两个根，没有重复注册");

    drop(updater);
    common::close_tempdir_retrying(index_dir);
    common::close_tempdir_retrying(a);
    common::close_tempdir_retrying(b);
    Ok(())
}
