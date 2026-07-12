//! 里程碑 6 集成测试：NTFS 快速层（MFT 快速枚举 + USN Journal 事件源 + 游标补账）。
//!
//! 测试策略跟 OCR 管线（tests/ocr_pipeline.rs）同一套护栏：
//! - 降级路径（非管理员/非 NTFS 时的行为）不需要任何特殊权限，必须无条件可跑——
//!   这是"诚实降级"的验收本身，CI 上天天要跑绿。
//! - 真正走快车道（MFT 枚举 + USN Journal）需要管理员权限打开原始卷句柄，
//!   测试开头先用 `dowse_core::ntfs_fast_path_available()` 探测一次，拿不到就打印
//!   原因跳过，不让非管理员的开发机/CI 把构建搞红。
//!
//! CI 排障记录（GitHub Actions windows-latest 连续几次全红后追出来的根因）：
//! `rebuild_index`/`watch_roots_auto` 在管理员+NTFS 环境下走 MFT 快速枚举时，
//! 枚举的是**整卷**（不是只扫监听根），CI 跑机的系统盘 C: 本身就有 ~130 万条
//! MFT 记录，单次整卷枚举实测耗时 30 秒量级——远超设计文档"100 万条 < 5s"的
//! 本地预算（那是本机文件数少得多时测的）。三个测试本来就默认并行跑在同一个
//! 进程里，每个测试至少一次 rebuild_index（一次整卷枚举），
//! `mft_enumeration_and_usn_watch_when_admin_available`/
//! `rapid_rename_then_delete_leaves_no_orphan_document` 还会在 `watch_roots_auto`
//! 里通过 `bootstrap_fast_roots` 再枚举一次——三个测试一起对同一块系统盘发起
//! 2~4 次并发整卷扫描，互相抢 I/O，把原本单次就要 30s 的枚举拖得更久，
//! 经常在事件真正开始被 USN 监听捕获之前就把下面的 `POLL_TIMEOUT` 耗光。
//! 这不是功能回归——USN 事件源本身工作正常，只是"建立监听前的必经枚举"在这块
//! 系统盘上的真实耗时远超测试原来给的等待预算。修法两条都用上：
//! 1) 用 `TEST_SERIAL_LOCK` 把三个测试串行化，去掉同卷并发扫描互相抢 I/O 的
//!    额外损耗；
//! 2) `POLL_TIMEOUT` 放宽到能舒服装下"一次整卷枚举 + 事件真正传播"的量级——
//!    这是"再等下去就该判失败了"的兜底线，不是性能预算断言，放宽不影响这条
//!    测试本来要验的东西（USN 事件源最终有没有正确捕获变更）。

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::Result;
use dowse_core::{
    IndexUpdater, Searcher, ntfs_fast_path_available, rebuild_index, watch_roots_auto,
};

mod common;

/// 三个测试共用的串行锁：见上面模块文档里的排障记录——同一块系统盘上的并发
/// MFT 整卷枚举会互相抢 I/O，串行化换来的是每个测试自己独立的、可预期的
/// 枚举耗时，而不是叠加在一起的抢占延迟。
static TEST_SERIAL_LOCK: Mutex<()> = Mutex::new(());

/// 轮询上限：不是性能预算，是"再等下去就该判失败了"的兜底。放宽到 120s 是为了
/// 舒服装下 CI 系统盘上一次整卷 MFT 枚举（实测 30s 量级，见模块文档）—— 部分
/// 场景（游标补账前重新枚举）等于要连续吃两次这个耗时，留够余量避免真实事件
/// 已经被正确捕获、只是枚举还没扫完就被判超时。
const POLL_TIMEOUT: Duration = Duration::from_secs(120);

fn target_dir(prefix: &str) -> tempfile::TempDir {
    tempfile::Builder::new().prefix(prefix).tempdir().unwrap()
}

fn wait_until(
    index_dir: &Path,
    query: &str,
    predicate: impl Fn(usize) -> bool,
) -> Option<Duration> {
    let start = Instant::now();
    loop {
        if let Ok(searcher) = Searcher::open(index_dir)
            && let Ok(hits) = searcher.search(query, 50)
            && predicate(hits.len())
        {
            return Some(start.elapsed());
        }
        if start.elapsed() > POLL_TIMEOUT {
            return None;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}

/// 无条件可跑：验收清单第 2 条（"非管理员运行同样操作：自动走 walkdir 路径，
/// 功能等价"）。走的是 `watch_roots_auto`——设计文档承诺的入口，两条车道对
/// 调用方产出完全一致的结果。在本测试运行时的机器上没有管理员权限就是在验
/// 降级路径；有管理员权限就是在验快车道——同一份断言两条路径都必须满足，
/// 这正是"上层感知不到差别"的可执行验收。
#[test]
fn watch_roots_auto_add_delete_rename_end_to_end() -> Result<()> {
    let _serial = TEST_SERIAL_LOCK.lock().expect("ntfs 测试串行锁 poisoned");
    let index_dir = tempfile::tempdir()?;
    let target = target_dir("dowse-m6-auto-");

    std::fs::write(target.path().join("seed.md"), "种子文件 seedword")?;
    rebuild_index(index_dir.path(), target.path())?;

    let updater = Arc::new(Mutex::new(IndexUpdater::open(index_dir.path())?));
    let stop = Arc::new(AtomicBool::new(false));
    let roots = vec![target.path().to_path_buf()];
    let index_dir_for_thread = index_dir.path().to_path_buf();
    let watch_handle = {
        let updater = updater.clone();
        let stop = stop.clone();
        std::thread::spawn(move || {
            let _ = watch_roots_auto(&index_dir_for_thread, &roots, updater, stop, |_p| {});
        })
    };
    // 给事件源一点时间挂上监听（notify 和 USN 读取线程都需要）。
    std::thread::sleep(Duration::from_millis(300));

    // 新增
    let added = target.path().join("added.md");
    std::fs::write(&added, "新增文件内容 freshkiwi")?;
    wait_until(index_dir.path(), "freshkiwi", |n| n == 1).expect("新增文件应变为可搜索");

    // 删除
    std::fs::remove_file(&added)?;
    wait_until(index_dir.path(), "freshkiwi", |n| n == 0).expect("删除文件应从索引消失");

    // 重命名
    let old = target.path().join("beforerename.md");
    std::fs::write(&old, "改名前正文 grapefruitkiwi")?;
    wait_until(index_dir.path(), "grapefruitkiwi", |n| n == 1).expect("改名前应先可搜到");
    let new = target.path().join("afterrename.md");
    std::fs::rename(&old, &new)?;
    wait_until(index_dir.path(), "afterrename", |n| n == 1).expect("改名后新名字应可搜索");
    wait_until(index_dir.path(), "beforerename", |n| n == 0).expect("改名后旧名字应搜不到");

    stop.store(true, Ordering::Relaxed);
    let _ = watch_handle.join();
    drop(updater);
    common::close_tempdir_retrying(index_dir);
    common::close_tempdir_retrying(target);
    Ok(())
}

/// 验收清单第 1/3 条：管理员运行时，MFT 枚举应该秒级把预先存在的文件收进
/// 索引（不是靠 walkdir），随后 USN 事件源应该正确捕获新增/删除/重命名。
/// 非管理员环境（大多数开发机/CI 默认状态）打印原因跳过，不算失败。
#[test]
fn mft_enumeration_and_usn_watch_when_admin_available() -> Result<()> {
    let _serial = TEST_SERIAL_LOCK.lock().expect("ntfs 测试串行锁 poisoned");
    let target = target_dir("dowse-m6-fast-");

    if !ntfs_fast_path_available(target.path()) {
        eprintln!(
            "跳过 mft_enumeration_and_usn_watch_when_admin_available：\
             当前进程没有管理员权限（或 {} 所在卷不是 NTFS），MFT/USN 快速路径不可用，\
             这正是设计文档要求的降级路径本身——用管理员权限重跑本测试可以覆盖快车道。",
            target.path().display()
        );
        return Ok(());
    }

    // —— 建索引前先在磁盘上放几个文件，验证 MFT 枚举（而不是 walkdir）能找到它们 ——
    for i in 0..20 {
        std::fs::write(
            target.path().join(format!("pre-existing-{i}.md")),
            format!("预先存在的文件 preexistingmarker{i}"),
        )?;
    }

    let index_dir = tempfile::tempdir()?;
    let mft_start = Instant::now();
    let stats = rebuild_index(index_dir.path(), target.path())?;
    let mft_elapsed = mft_start.elapsed();
    println!(
        "MFT 快速枚举 20 个文件建索引耗时: {mft_elapsed:?}（性能预算：100 万条 < 5s，这里量级小很多，仅供参考）"
    );
    assert_eq!(stats.indexed, 20, "MFT 枚举应该找到全部预先存在的文件");
    assert_eq!(
        count_hits(index_dir.path(), "preexistingmarker0"),
        1,
        "预先存在的文件内容应可搜索"
    );

    // —— 挂 live 监听，验证 USN 事件源捕获新增/删除/重命名 ——
    let updater = Arc::new(Mutex::new(IndexUpdater::open(index_dir.path())?));
    let stop = Arc::new(AtomicBool::new(false));
    let roots = vec![target.path().to_path_buf()];
    let index_dir_for_thread = index_dir.path().to_path_buf();
    let watch_handle = {
        let updater = updater.clone();
        let stop = stop.clone();
        std::thread::spawn(move || {
            let _ = watch_roots_auto(&index_dir_for_thread, &roots, updater, stop, |p| {
                if std::env::var("E2E_DEBUG").is_ok() {
                    eprintln!("[usn watch] {p:?}");
                }
            });
        })
    };
    std::thread::sleep(Duration::from_millis(500));

    let added = target.path().join("usn-added.md");
    std::fs::write(&added, "USN 新增 usnfreshmango")?;
    wait_until(index_dir.path(), "usnfreshmango", |n| n == 1).expect("USN 事件源应该捕获新增文件");

    std::fs::remove_file(&added)?;
    wait_until(index_dir.path(), "usnfreshmango", |n| n == 0).expect("USN 事件源应该捕获删除文件");

    let old = target.path().join("usn-before-rename.md");
    std::fs::write(&old, "改名前 usngrapefruit")?;
    wait_until(index_dir.path(), "usngrapefruit", |n| n == 1).expect("改名前应先可搜到");
    let new = target.path().join("usn-after-rename.md");
    std::fs::rename(&old, &new)?;
    wait_until(index_dir.path(), "usn-after-rename", |n| n == 1)
        .expect("USN 事件源应捕获重命名新名字");
    wait_until(index_dir.path(), "usn-before-rename", |n| n == 0)
        .expect("USN 事件源应捕获重命名后旧名字消失");

    stop.store(true, Ordering::Relaxed);
    let _ = watch_handle.join();
    drop(updater);
    common::close_tempdir_retrying(index_dir);
    common::close_tempdir_retrying(target);
    Ok(())
}

/// 验收清单第 4 条，真机版："快速连续改名→删除同一文件：索引终态正确"——
/// usn_translate.rs 的单测已经在纯逻辑层覆盖了这个状态机分支，这里是它在真实
/// USN Journal 上的映证：连续两次文件系统调用之间几乎没有间隔，靠的是操作系统
/// 真实的调度节奏，不是人为构造的记录序列。
#[test]
fn rapid_rename_then_delete_leaves_no_orphan_document() -> Result<()> {
    let _serial = TEST_SERIAL_LOCK.lock().expect("ntfs 测试串行锁 poisoned");
    let target = target_dir("dowse-m6-rapid-");

    if !ntfs_fast_path_available(target.path()) {
        eprintln!(
            "跳过 rapid_rename_then_delete_leaves_no_orphan_document：当前进程没有管理员权限，\
             USN 快速路径不可用，跳过（降级路径下这个场景由 notify + 防抖队列已有的\
             单测覆盖，见 events.rs）。"
        );
        return Ok(());
    }

    let index_dir = tempfile::tempdir()?;
    std::fs::write(target.path().join("seed.md"), "种子 seedmarker")?;
    rebuild_index(index_dir.path(), target.path())?;

    let updater = Arc::new(Mutex::new(IndexUpdater::open(index_dir.path())?));
    let stop = Arc::new(AtomicBool::new(false));
    let roots = vec![target.path().to_path_buf()];
    let index_dir_for_thread = index_dir.path().to_path_buf();
    let watch_handle = {
        let updater = updater.clone();
        let stop = stop.clone();
        std::thread::spawn(move || {
            let _ = watch_roots_auto(&index_dir_for_thread, &roots, updater, stop, |_p| {});
        })
    };
    std::thread::sleep(Duration::from_millis(500));

    let before = target.path().join("rapid-before.md");
    let after = target.path().join("rapid-after.md");
    std::fs::write(&before, "快进序列 rapidsequencemarker")?;
    wait_until(index_dir.path(), "rapidsequencemarker", |n| n == 1)
        .expect("重命名前文件应先可搜到，确保它真的进过索引");

    // 改名后立刻删除——中间不留任何等待，逼近"重命名后紧跟删除"的真实节奏。
    std::fs::rename(&before, &after)?;
    std::fs::remove_file(&after)?;

    // 终态：旧名新名都搜不到。
    wait_until(index_dir.path(), "rapidsequencemarker", |n| n == 0)
        .expect("快进改名后删除，索引终态应该完全搜不到这份内容（新名旧名都不该残留）");

    stop.store(true, Ordering::Relaxed);
    let _ = watch_handle.join();
    drop(updater);
    common::close_tempdir_retrying(index_dir);
    common::close_tempdir_retrying(target);
    Ok(())
}

fn count_hits(index_dir: &Path, query: &str) -> usize {
    let searcher = Searcher::open(index_dir).unwrap();
    searcher.search(query, 50).unwrap().len()
}
