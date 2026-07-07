//! 启动对账集成测试：建索引后，不通过监听、直接在文件系统层面改动文件
//! （模拟程序停机期间发生的变更），触发对账，断言索引追平文件系统实际状态。

use std::path::Path;

use anyhow::Result;
use dowse_core::{IndexUpdater, ReconcileStats, Searcher, rebuild_index, reconcile};

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
