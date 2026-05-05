//! 端到端验证（真实执行，不是纸面推导）：
//! tempdir 建索引 → 用真实的 NotifyEventSource 挂上常驻监听 → 实际写/删/重命名文件
//! → 轮询搜索直到状态变化，记录"写入到可搜索"等实测耗时并与 3s 预算对比。
//!
//! 跑法：`cargo test -p dowse-core --test e2e_watch -- --nocapture`，耗时数字会打到
//! 标准输出。（文件名用 e2e_watch 而非含 update 的名字，避开 Windows 把
//! update/install 类可执行文件当安装程序要求 UAC 提权的坑。）

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::Result;
use dowse_core::{rebuild_index, run_watch, IndexUpdater, NotifyEventSource, Searcher};

/// 单文件修改到可搜索的性能预算：< 3s（含 500ms 防抖）。
const BUDGET: Duration = Duration::from_secs(3);
/// 轮询上限：远松于预算，用来在超时的情况下快速失败而不是挂死。
const POLL_TIMEOUT: Duration = Duration::from_secs(15);

fn target_dir() -> tempfile::TempDir {
    tempfile::Builder::new()
        .prefix("dowse-e2e-")
        .tempdir()
        .unwrap()
}

/// 反复开只读 Searcher 搜 query，直到命中数满足 predicate 或超时。返回耗时。
fn wait_until(index_dir: &Path, query: &str, predicate: impl Fn(usize) -> bool) -> Option<Duration> {
    let start = Instant::now();
    loop {
        // 每轮开新的 Searcher：reader 提交后自动重载有微小延迟，开新的最稳。
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

#[test]
fn end_to_end_watch_add_delete_rename_latency() -> Result<()> {
    let index_dir = tempfile::tempdir()?;
    let target = target_dir();

    // 先放一个初始文件并全量建索引，让监听有个已存在的索引可写。
    std::fs::write(target.path().join("seed.md"), "种子文件 seedword")?;
    rebuild_index(index_dir.path(), target.path())?;

    // —— 启动常驻监听（真实 NotifyEventSource + run_watch，在后台线程跑）——
    let updater = Arc::new(Mutex::new(IndexUpdater::open(index_dir.path())?));
    let stop = Arc::new(AtomicBool::new(false));
    let roots = vec![target.path().to_path_buf()];
    let watch_handle = {
        let updater = updater.clone();
        let stop = stop.clone();
        std::thread::spawn(move || {
            let _ = run_watch(NotifyEventSource, &roots, updater, stop, |p| {
                if std::env::var("E2E_DEBUG").is_ok() {
                    eprintln!("[watch] {p:?}");
                }
            });
        })
    };
    // 给 notify 一点时间把 watch 挂上（否则最早的写入可能漏掉）。
    std::thread::sleep(Duration::from_millis(300));

    // —— 1) 新增文件：写入 → 可搜索 ——
    let added = target.path().join("added.md");
    std::fs::write(&added, "新增文件内容 freshpineapple")?;
    let t_add = wait_until(index_dir.path(), "freshpineapple", |n| n == 1)
        .expect("新增文件应在轮询超时内变为可搜索");

    // —— 2) 删除文件：删除 → 搜不到 ——
    std::fs::remove_file(&added)?;
    let t_del = wait_until(index_dir.path(), "freshpineapple", |n| n == 0)
        .expect("删除文件应在轮询超时内从索引消失");

    // —— 3) 重命名：旧名搜不到、新名能搜到（内容照常命中）——
    let old = target.path().join("beforerename.md");
    std::fs::write(&old, "改名前的正文 grapefruitword")?;
    wait_until(index_dir.path(), "beforerename", |n| n == 1)
        .expect("改名前文件应先可搜到");
    let new = target.path().join("afterrename.md");
    std::fs::rename(&old, &new)?;
    let t_rename_new = wait_until(index_dir.path(), "afterrename", |n| n == 1)
        .expect("改名后新名字应变为可搜索");
    let t_rename_old = wait_until(index_dir.path(), "beforerename", |n| n == 0)
        .expect("改名后旧名字应搜不到");

    // —— 停止监听 ——
    stop.store(true, Ordering::Relaxed);
    let _ = watch_handle.join();

    // —— 打印实测耗时，供最终汇报 ——
    println!("\n===== M3 端到端实测耗时（预算：单文件修改到可搜索 < 3s）=====");
    println!("写入到可搜索:      {:>6} ms", t_add.as_millis());
    println!("删除到搜不到:      {:>6} ms", t_del.as_millis());
    println!("重命名-新名可搜:   {:>6} ms", t_rename_new.as_millis());
    println!("重命名-旧名消失:   {:>6} ms", t_rename_old.as_millis());
    println!("===============================================================\n");

    // 断言在预算内。留一点余量（BUDGET 是设计预算），主要防回归。
    assert!(t_add <= BUDGET, "写入到可搜索 {}ms 超出 3s 预算", t_add.as_millis());
    assert!(t_del <= BUDGET, "删除到搜不到 {}ms 超出 3s 预算", t_del.as_millis());
    assert!(
        t_rename_new <= BUDGET,
        "重命名新名可搜 {}ms 超出 3s 预算",
        t_rename_new.as_millis()
    );
    Ok(())
}
