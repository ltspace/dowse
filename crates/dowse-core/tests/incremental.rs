//! 增量更新器的集成测试：用真实 tempdir 建索引，实际改/删/重命名文件，
//! 断言搜索结果随之变化。目录整体删除的前缀圈选单独一个用例。

use std::path::Path;

use anyhow::Result;
use dowse_core::{IndexUpdater, PendingChange, PendingOp, Searcher, rebuild_index};

mod common;

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

fn upsert_tree(path: &Path) -> PendingChange {
    PendingChange {
        path: path.to_path_buf(),
        op: PendingOp::UpsertTree,
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
    common::force_slow_lane_for_tests();

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

    drop(updater);
    common::close_tempdir_retrying(index_dir);
    common::close_tempdir_retrying(target);
    Ok(())
}

#[test]
fn delete_file_removes_it_from_index() -> Result<()> {
    common::force_slow_lane_for_tests();

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

    drop(updater);
    common::close_tempdir_retrying(index_dir);
    common::close_tempdir_retrying(target);
    Ok(())
}

#[test]
fn rename_file_old_name_gone_new_name_and_content_searchable() -> Result<()> {
    common::force_slow_lane_for_tests();

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

    drop(updater);
    common::close_tempdir_retrying(index_dir);
    common::close_tempdir_retrying(target);
    Ok(())
}

#[test]
fn remove_tree_prefix_deletes_whole_subdirectory() -> Result<()> {
    common::force_slow_lane_for_tests();

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

    drop(updater);
    common::close_tempdir_retrying(index_dir);
    common::close_tempdir_retrying(target);
    Ok(())
}

/// 目录整体新增/移入的展开：notify 回调线程只发一个 UpsertTree 标记（不带
/// 展开后的文件列表），真正"这个目录下有哪些文件"的 walk 挪到消费侧
/// （`IndexUpdater::apply`）做——这里直接构造 UpsertTree 变更验证展开结果，
/// 不依赖真实的 notify 事件触发时序。
#[test]
fn upsert_tree_expands_directory_into_every_file_inside() -> Result<()> {
    common::force_slow_lane_for_tests();

    let index_dir = tempfile::tempdir()?;
    let target = target_dir();

    // 先建一个空索引（没有任何文件），模拟"目录是之后才整体移入监听范围的"。
    rebuild_index(index_dir.path(), target.path())?;
    assert_eq!(count_hits(index_dir.path(), "watermelon"), 0);

    let moved_in = target.path().join("moved-in");
    std::fs::create_dir(&moved_in)?;
    std::fs::write(moved_in.join("a.md"), "子文件甲 watermelon")?;
    std::fs::write(moved_in.join("b.md"), "子文件乙 watermelon")?;
    std::fs::create_dir(moved_in.join("nested"))?;
    std::fs::write(
        moved_in.join("nested").join("c.md"),
        "嵌套子文件 watermelon",
    )?;

    let mut updater = IndexUpdater::open(index_dir.path())?;
    let outcome = updater.apply(&[upsert_tree(&moved_in)])?;
    assert_eq!(
        outcome.upserted, 3,
        "目录下三个文件（含嵌套）都应被展开收录"
    );

    assert_eq!(
        count_hits(index_dir.path(), "watermelon"),
        3,
        "整个移入目录下的文件都应变为可搜索"
    );

    drop(updater);
    common::close_tempdir_retrying(index_dir);
    common::close_tempdir_retrying(target);
    Ok(())
}
