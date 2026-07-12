//! 多根索引（里程碑 7）验收清单集成测试，逐条对应设计文档"验收清单"一节：
//!
//! 1. 加根 B 后：A、B 内容都可搜；B 的图片进 OCR 队列；改 B 下文件秒级可搜。
//! 2. 移除 B：B 内容全部消失，A 不受影响，OCR 队列无 B 残留。
//! 3. 尝试添加 A 的子目录 / 父目录：拒绝且提示清晰。
//! 4. 加根中途杀进程重启：对账后 B 完整可搜，无重复文档
//!    ——已在 `tests/reconcile.rs::crash_mid_add_root_then_restart_reconciles_to_clean_state`
//!    覆盖（跟孤儿清理规则是同一段逻辑，放在 reconcile 测试文件里更贴近实现）。
//! 5. 托盘与空态的根列表实时反映增删——两端都是每次呼出/每次状态变化时重新
//!    调用 `registered_roots()`（不缓存），下面第 1/2/3 条测试里"操作完成后
//!    立刻读 `registered_roots()` 断言"已经结构性验证了这个前提；托盘菜单/
//!    Svelte 组件本身的渲染不在 dowse-core 的测试范围内。

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::Result;
use dowse_core::{
    IndexUpdater, NotifyEventSource, OcrQueue, Searcher, add_root, add_root_with_progress,
    rebuild_index, registered_roots, remove_root, run_watch,
};

mod common;

fn tempdir(prefix: &str) -> tempfile::TempDir {
    tempfile::Builder::new().prefix(prefix).tempdir().unwrap()
}

fn count_hits(index_dir: &Path, query: &str) -> usize {
    Searcher::open(index_dir)
        .unwrap()
        .search(query, 50)
        .unwrap()
        .len()
}

/// 验收清单第 1 条（可搜部分 + OCR 入队部分）：加根 B 后 A、B 都可搜，B 的
/// 图片进 OCR 队列——图片入队不需要真的能识别出文字，`add_image_document`
/// 只看扩展名 + 文件体积就会把新图片塞进 pending，内容随便写。
#[test]
fn add_root_makes_both_roots_searchable_and_queues_new_images_for_ocr() -> Result<()> {
    common::force_slow_lane_for_tests();

    let index_dir = tempfile::tempdir()?;
    let a = tempdir("dowse-a-");
    std::fs::write(a.path().join("a.md"), "根 A 的内容 apricot")?;
    rebuild_index(index_dir.path(), a.path())?;

    let b = tempdir("dowse-b-");
    std::fs::write(b.path().join("b.md"), "根 B 的内容 blueberry")?;
    std::fs::write(b.path().join("shot.png"), b"fake png bytes for queueing")?;

    let mut updater = IndexUpdater::open(index_dir.path())?;
    let stats = add_root(index_dir.path(), b.path(), &mut updater)?;
    assert_eq!(stats.indexed, 2, "B 下的文本文件和图片文件都应该收录");
    drop(updater);

    assert_eq!(
        count_hits(index_dir.path(), "apricot"),
        1,
        "A 的内容不受影响"
    );
    assert_eq!(count_hits(index_dir.path(), "blueberry"), 1, "B 的内容可搜");

    let pending = OcrQueue::for_index_dir(index_dir.path()).pending_len();
    assert_eq!(pending, 1, "B 下新发现的图片应该进 OCR 队列");

    let roots = registered_roots(index_dir.path())?;
    assert_eq!(
        roots.len(),
        2,
        "验收清单第 5 条：add_root 完成后立刻反映在 registered_roots()"
    );

    common::close_tempdir_retrying(index_dir);
    common::close_tempdir_retrying(a);
    common::close_tempdir_retrying(b);
    Ok(())
}

/// 验收清单第 1 条（秒级可搜部分）：加根 B 之后挂上真实的常驻监听，B 下
/// 新写的文件应该像单根场景一样很快变可搜——多根不改变监听侧的行为。
#[test]
fn editing_a_file_under_newly_added_root_becomes_searchable_via_live_watch() -> Result<()> {
    common::force_slow_lane_for_tests();

    let index_dir = tempfile::tempdir()?;
    let a = tempdir("dowse-a-");
    std::fs::write(a.path().join("a.md"), "根 A 的内容 apricot")?;
    rebuild_index(index_dir.path(), a.path())?;

    let b = tempdir("dowse-b-");
    std::fs::write(b.path().join("seed.md"), "种子文件 seedword")?;

    {
        let mut updater = IndexUpdater::open(index_dir.path())?;
        add_root(index_dir.path(), b.path(), &mut updater)?;
    }

    // —— 加根完成后，用真实 NotifyEventSource 挂上覆盖两个根的常驻监听 ——
    let updater = Arc::new(Mutex::new(IndexUpdater::open(index_dir.path())?));
    let stop = Arc::new(AtomicBool::new(false));
    let roots = vec![a.path().to_path_buf(), b.path().to_path_buf()];
    let watch_handle = {
        let updater = updater.clone();
        let stop = stop.clone();
        std::thread::spawn(move || {
            let _ = run_watch(NotifyEventSource, &roots, updater, stop, |_| {});
        })
    };
    std::thread::sleep(Duration::from_millis(300));

    let added = b.path().join("live.md");
    std::fs::write(&added, "实时写入 freshwatermelon")?;

    let start = Instant::now();
    let timeout = Duration::from_secs(60);
    let mut found = false;
    while start.elapsed() < timeout {
        if let Ok(searcher) = Searcher::open(index_dir.path())
            && let Ok(hits) = searcher.search("freshwatermelon", 10)
            && hits.len() == 1
        {
            found = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    assert!(found, "新添加的根 B 下新写的文件应该在轮询超时内变为可搜索");

    stop.store(true, Ordering::Relaxed);
    let _ = watch_handle.join();
    drop(updater);

    common::close_tempdir_retrying(index_dir);
    common::close_tempdir_retrying(a);
    common::close_tempdir_retrying(b);
    Ok(())
}

/// 验收清单第 2 条：移除 B 后 B 的内容全部消失、A 不受影响、OCR 队列无 B
/// 残留（`compact` 用移除后的最新根集合裁剪）。
#[test]
fn remove_root_deletes_its_docs_and_compacts_ocr_queue() -> Result<()> {
    common::force_slow_lane_for_tests();

    let index_dir = tempfile::tempdir()?;
    let a = tempdir("dowse-a-");
    std::fs::write(a.path().join("a.md"), "根 A 的内容 apricot")?;
    rebuild_index(index_dir.path(), a.path())?;

    let b = tempdir("dowse-b-");
    std::fs::write(b.path().join("b.md"), "根 B 的内容 blueberry")?;
    std::fs::write(b.path().join("shot.png"), b"fake png bytes for queueing")?;

    let mut updater = IndexUpdater::open(index_dir.path())?;
    add_root(index_dir.path(), b.path(), &mut updater)?;
    assert_eq!(OcrQueue::for_index_dir(index_dir.path()).pending_len(), 1);

    let b_registered = registered_roots(index_dir.path())?
        .into_iter()
        .find(|r| r != &a.path().to_path_buf())
        .expect("B 应该已经注册");

    let stats = remove_root(index_dir.path(), &b_registered, &mut updater)?;
    assert_eq!(stats.removed, 2, "B 的两篇文档（文本+图片）都应该被删掉");
    drop(updater);

    assert_eq!(count_hits(index_dir.path(), "apricot"), 1, "A 不受影响");
    assert_eq!(
        count_hits(index_dir.path(), "blueberry"),
        0,
        "B 内容全部消失"
    );
    assert_eq!(
        OcrQueue::for_index_dir(index_dir.path()).pending_len(),
        0,
        "OCR 队列不应该残留 B 的条目"
    );

    let roots = registered_roots(index_dir.path())?;
    assert_eq!(
        roots,
        vec![a.path().to_path_buf()],
        "验收清单第 5 条：移除后立刻反映在 registered_roots()"
    );

    common::close_tempdir_retrying(index_dir);
    common::close_tempdir_retrying(a);
    common::close_tempdir_retrying(b);
    Ok(())
}

/// 验收清单第 3 条：尝试添加 A 的子目录 / 父目录都应该被拒绝且提示清晰，
/// 且拒绝不应该对索引/roots 产生任何副作用（既没有半途写入的文档，也没有
/// 污染 meta）。
#[test]
fn add_root_rejects_child_and_parent_of_existing_root() -> Result<()> {
    common::force_slow_lane_for_tests();

    let index_dir = tempfile::tempdir()?;
    let a = tempdir("dowse-a-");
    let sub = a.path().join("sub");
    std::fs::create_dir_all(&sub)?;
    std::fs::write(a.path().join("a.md"), "根 A 的内容 apricot")?;
    std::fs::write(sub.join("nested.md"), "嵌套文件内容 nestedword")?;
    rebuild_index(index_dir.path(), a.path())?;

    let mut updater = IndexUpdater::open(index_dir.path())?;

    // 子目录：拒绝
    let child_err = add_root_with_progress(index_dir.path(), &sub, &mut updater, |_| {})
        .expect_err("A 的子目录应该被拒绝");
    assert!(
        child_err.to_string().contains("嵌套"),
        "错误提示应该说明是嵌套问题: {child_err}"
    );

    // 父目录：拒绝
    let parent = a
        .path()
        .parent()
        .expect("临时目录应该有父目录")
        .to_path_buf();
    let parent_err = add_root_with_progress(index_dir.path(), &parent, &mut updater, |_| {})
        .expect_err("A 的父目录应该被拒绝");
    assert!(
        parent_err.to_string().contains("嵌套"),
        "错误提示应该说明是嵌套问题: {parent_err}"
    );

    drop(updater);

    // 两次拒绝都不应该有任何副作用：roots 还是只有 A 一个，索引内容没有变化。
    let roots = registered_roots(index_dir.path())?;
    assert_eq!(roots.len(), 1, "拒绝不应该污染 roots");
    assert_eq!(count_hits(index_dir.path(), "apricot"), 1);
    assert_eq!(count_hits(index_dir.path(), "nestedword"), 1);

    common::close_tempdir_retrying(index_dir);
    common::close_tempdir_retrying(a);
    Ok(())
}
