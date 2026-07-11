//! 增量更新器的集成测试：用真实 tempdir 建索引，实际改/删/重命名文件，
//! 断言搜索结果随之变化。目录整体删除的前缀圈选单独一个用例。

use std::path::Path;

use anyhow::Result;
use dowse_core::{IndexUpdater, PendingChange, PendingOp, Searcher, rebuild_index};

/// 建索引用的目标目录名不能带 "." 前缀——walk_index_files 会整棵跳过隐藏目录，
/// 而 tempfile 默认给临时目录起 ".tmpXXXX" 这种名字。
fn target_dir() -> tempfile::TempDir {
    tempfile::Builder::new()
        .prefix("dowse-upd-")
        .tempdir()
        .unwrap()
}

fn upsert(path: &Path) -> PendingChange {
    PendingChange {
        path: path.to_path_buf(),
        op: PendingOp::Upsert,
    }
}

fn remove(path: &Path) -> PendingChange {
    PendingChange {
        path: path.to_path_buf(),
        op: PendingOp::Remove,
    }
}

/// 每次断言都开一个新的 Searcher：Searcher 的 reader 虽然会在 commit 后自动重载，
/// 但重载有微小延迟，测试里直接开新的最稳妥、无时序假设。
fn count_hits(index_dir: &Path, query: &str) -> usize {
    let searcher = Searcher::open(index_dir).unwrap();
    searcher.search(query, 50).unwrap().len()
}

#[test]
fn modify_file_updates_searchable_content() -> Result<()> {
    let index_dir = tempfile::tempdir()?;
    let target = target_dir();
    let file = target.path().join("note.md");
    std::fs::write(&file, "最初的内容讲的是苹果")?;

    rebuild_index(index_dir.path(), target.path())?;
    assert_eq!(count_hits(index_dir.path(), "苹果"), 1, "初始内容应能搜到");
    assert_eq!(count_hits(index_dir.path(), "香蕉"), 0);

    // 改内容，走增量更新
    std::fs::write(&file, "改过之后讲的是香蕉")?;
    let mut updater = IndexUpdater::open(index_dir.path())?;
    let outcome = updater.apply(&[upsert(&file)])?;
    assert_eq!(outcome.upserted, 1);

    assert_eq!(count_hits(index_dir.path(), "香蕉"), 1, "新内容应能搜到");
    assert_eq!(count_hits(index_dir.path(), "苹果"), 0, "旧内容应搜不到");
    Ok(())
}

#[test]
fn delete_file_removes_it_from_index() -> Result<()> {
    let index_dir = tempfile::tempdir()?;
    let target = target_dir();
    let file = target.path().join("doomed.md");
    std::fs::write(&file, "待删除的文档独有词汇 zzqq")?;

    rebuild_index(index_dir.path(), target.path())?;
    assert_eq!(count_hits(index_dir.path(), "zzqq"), 1);

    std::fs::remove_file(&file)?;
    let mut updater = IndexUpdater::open(index_dir.path())?;
    let outcome = updater.apply(&[remove(&file)])?;
    assert_eq!(outcome.removed, 1);

    assert_eq!(count_hits(index_dir.path(), "zzqq"), 0, "删除后应搜不到");
    Ok(())
}

#[test]
fn rename_file_old_name_gone_new_name_and_content_searchable() -> Result<()> {
    let index_dir = tempfile::tempdir()?;
    let target = target_dir();
    let old = target.path().join("oldname.md");
    let new = target.path().join("newname.md");
    std::fs::write(&old, "改名测试的正文内容 mango")?;

    rebuild_index(index_dir.path(), target.path())?;
    assert_eq!(
        count_hits(index_dir.path(), "oldname"),
        1,
        "旧文件名应能搜到"
    );

    // 物理改名，再按"删旧名 + 加新名"落进索引
    std::fs::rename(&old, &new)?;
    let mut updater = IndexUpdater::open(index_dir.path())?;
    updater.apply(&[remove(&old), upsert(&new)])?;

    assert_eq!(
        count_hits(index_dir.path(), "oldname"),
        0,
        "旧文件名应搜不到"
    );
    assert_eq!(
        count_hits(index_dir.path(), "newname"),
        1,
        "新文件名应能搜到"
    );
    assert_eq!(count_hits(index_dir.path(), "mango"), 1, "正文内容照常命中");
    Ok(())
}

#[test]
fn remove_tree_prefix_deletes_whole_subdirectory() -> Result<()> {
    // 目录整体删除的前缀圈选：sub 下的都删掉，兄弟目录 sub2 和顶层文件不受影响。
    let index_dir = tempfile::tempdir()?;
    let target = target_dir();

    let sub = target.path().join("sub");
    let sub2 = target.path().join("sub2");
    std::fs::create_dir(&sub)?;
    std::fs::create_dir(&sub2)?;
    std::fs::write(sub.join("a.md"), "子目录文件甲 alpha")?;
    std::fs::write(sub.join("b.md"), "子目录文件乙 alpha")?;
    std::fs::write(sub2.join("c.md"), "兄弟目录文件丙 alpha")?;
    std::fs::write(target.path().join("top.md"), "顶层文件 alpha")?;

    rebuild_index(index_dir.path(), target.path())?;
    assert_eq!(
        count_hits(index_dir.path(), "alpha"),
        4,
        "初始四篇都含 alpha"
    );

    // 前缀圈选删除整个 sub 目录（updater 内部用 term 范围查询，不逐文件比对）
    let mut updater = IndexUpdater::open(index_dir.path())?;
    let outcome = updater.apply(&[PendingChange {
        path: sub.clone(),
        op: PendingOp::RemoveTree,
    }])?;
    assert_eq!(outcome.removed, 2, "应恰好圈选删掉 sub 下的两篇");

    assert_eq!(
        count_hits(index_dir.path(), "alpha"),
        2,
        "删完 sub 只剩兄弟目录和顶层两篇"
    );
    // 兄弟目录 sub2 不能被误删（前缀末尾补分隔符的意义）
    assert_eq!(
        count_hits(index_dir.path(), "丙"),
        1,
        "兄弟目录 sub2 不受影响"
    );
    assert_eq!(count_hits(index_dir.path(), "顶层"), 1, "顶层文件不受影响");
    assert_eq!(count_hits(index_dir.path(), "甲"), 0, "sub 下文件已删");
    Ok(())
}
