//! OCR 管线端到端集成测试：生成一张带哨兵词的 PNG，走完整的
//! "全量建索引 → OCR 队列 → 搜索" 链路，断言哨兵词可搜到。
//!
//! CI 护栏：GitHub Actions 的 windows-latest 镜像不一定装了中文语言包（甚至可能
//! 一个 OCR 语言包都没有），所以测试开头先探测 `dowse_core::is_available()`，
//! 不可用就跳过并打印原因，不让 CI 变红——这是设计文档要求的降级路径本身
//! （"系统无任何 OCR 语言包...管线整体停用...不报错不崩溃"）在测试环境里的镜像。
//!
//! 哨兵词用纯 ASCII 字母数字串而不是中文：中文 OCR 存在已知的系统性拆字误差
//! （见设计文档"验证结论回放"，池→氵也 一类），用中文哨兵词做精确匹配断言在
//! CI 上会偶发抖动。纯 ASCII 场景识别率接近 100%，用它做硬断言，同时图片里
//! 也放一行中文，走一遍双形态清洗的真实路径（不做精确断言，只要求整个链路不炸）。

use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};

mod common;

/// 轮询上限：见下面 `ocr_queue_survives_restart_and_resumes_pending_work` 的
/// 排障记录——两个 worker 并发处理两张图片时，索引提交到能被一个全新打开的
/// `Searcher` 读到之间偶发有 CI 才会撞上的短暂延迟，这条测试原来直接开
/// `Searcher::open` 后立即断言，是本仓库里唯一一处对"刚写完的索引"不走轮询
/// 就下断言的地方（e2e_watch.rs/ntfs_fast_path.rs 对同类断言全部走的是
/// wait_until 轮询）。跟那两个文件用同一个套路补上，不改断言本身要验的东西。
const SEARCH_VISIBLE_TIMEOUT: Duration = Duration::from_secs(15);

/// 反复开只读 Searcher 搜 query，直到命中数满足 predicate 或超时。
fn wait_until_searchable(index_dir: &Path, query: &str, predicate: impl Fn(usize) -> bool) -> bool {
    let start = Instant::now();
    loop {
        if let Ok(searcher) = dowse_core::Searcher::open(index_dir)
            && let Ok(hits) = searcher.search(query, 10)
            && predicate(hits.len())
        {
            return true;
        }
        if start.elapsed() > SEARCH_VISIBLE_TIMEOUT {
            return false;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}

// 纯小写字母数字、不含分隔符——跟 searcher.rs 测试里 "zzzsentinelprobe888" 同一个
// 套路：jieba 分词器会把这样一整串 ASCII 当成单个 token，搜索时不用担心被切碎。
//
// 字母/数字交界处特意避开小写 l 紧跟数字的写法（比如 "sentinel8842" 的
// l8 交界）——实测 Windows OCR 会把这种交界的 l 认成数字 1
// （"sentinel8842" 被识别成 "sentine18842"），这是 OCR 本身对西文的正常误差，
// 不是管线的 bug，只是不适合拿来做精确匹配断言，换一个交界处没有歧义字符的词。
const SENTINEL: &str = "dowsemagictoken7391";

// 断点续传测试（下面 ocr_queue_survives_restart_and_resumes_pending_work）需要
// 两个互不相同的哨兵词，分别标记两张图片各自的识别结果有没有落到对应文档。
// 早期实现是给同一个哨兵词加数字后缀区分（"...x0" / "...x1"），CI 上的 Windows
// OCR 会把结尾的 "0" 认成字母 "O"，识别结果变成 "...xO"，跟查询字面量 "...x0"
// 永远对不上——识别出来的字节数（21 字节）跟预期完全一样，从字节数上完全看不
// 出问题，只有比对识别原文才能看出差一个字符。这是本机跑用的 OCR 版本和 CI
// 镜像 OCR 版本对 0/O 的识别差异，不是索引或并发问题。换成两个纯字母、不含
// 数字、也避开 0/O、1/l/I 这类易混字形的词，从根上避开这一类识别误差。
const SENTINEL_SHOT0: &str = "dowsemagictokenquokka";
const SENTINEL_SHOT1: &str = "dowsemagictokenwombat";

fn generate_test_image(path: &Path, sentinel: &str) -> Result<()> {
    let script = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("make_ocr_test_image.ps1");

    let status = Command::new("powershell")
        .args(["-NoProfile", "-ExecutionPolicy", "Bypass", "-File"])
        .arg(&script)
        .arg("-Path")
        .arg(path)
        .arg("-Sentinel")
        .arg(sentinel)
        .status()
        .context("拉起 powershell 生成测试图片失败")?;

    anyhow::ensure!(status.success(), "make_ocr_test_image.ps1 执行失败");
    anyhow::ensure!(
        path.exists(),
        "测试图片生成后文件不存在: {}",
        path.display()
    );
    Ok(())
}

#[test]
fn images_with_sentinel_text_become_searchable_after_ocr() -> Result<()> {
    common::force_slow_lane_for_tests();

    if !dowse_core::is_available() {
        eprintln!(
            "跳过 images_with_sentinel_text_become_searchable_after_ocr：\
             本机没有检测到可用的 OCR 语言包（常见于 CI 的 windows-latest 镜像未装语言包）"
        );
        return Ok(());
    }

    let index_dir = tempfile::tempdir()?;
    let target_dir = tempfile::Builder::new()
        .prefix("dowse-ocr-test-")
        .tempdir()?;

    let img_path = target_dir.path().join("screenshot.png");
    generate_test_image(&img_path, SENTINEL)?;

    // 也顺手扔一篇普通文本文件进去，验证"文本先行可搜"没有被 OCR 分支破坏。
    std::fs::write(
        target_dir.path().join("note.md"),
        "普通文本文件，跟 OCR 无关",
    )?;

    let stats = dowse_core::rebuild_index(index_dir.path(), target_dir.path())?;
    // 全量重建阶段图片只落占位文档（内容为空），文本文件正常收录；两者都算 indexed。
    assert_eq!(
        stats.indexed, 2,
        "文本文件 + 图片占位文档，rebuild 阶段应收录 2 篇"
    );

    // 图片这时候应该已经可以按文件名搜到（占位符阶段），但正文还搜不到哨兵词。
    {
        let searcher = dowse_core::Searcher::open(index_dir.path())?;
        let by_name = searcher.search("screenshot", 10)?;
        assert!(
            !by_name.is_empty(),
            "OCR 完成前，图片应该已经能按文件名搜到"
        );
    }

    let drain_stats = dowse_core::drain_ocr_queue(index_dir.path(), 2)?;
    assert!(
        drain_stats.available,
        "上面已经探测过 is_available()，这里不该是 false"
    );
    assert_eq!(drain_stats.processed, 1, "应该正好处理了那一张截图");

    let searcher = dowse_core::Searcher::open(index_dir.path())?;
    let hits = searcher.search(SENTINEL, 10)?;
    assert!(!hits.is_empty(), "OCR 完成后应该能搜到哨兵词，实际没有命中");
    assert!(
        hits.iter().any(|h| h.path.ends_with("screenshot.png")),
        "命中的应该是截图文件本身: {:?}",
        hits.iter().map(|h| &h.path).collect::<Vec<_>>()
    );

    // 普通文本文件不受影响，照常可搜。
    let text_hits = searcher.search("普通文本文件", 10)?;
    assert!(!text_hits.is_empty(), "文本文件的索引不应该被 OCR 分支影响");

    // 显式 drop 掉持有索引句柄的 Searcher，再走重试退避删临时目录。
    drop(searcher);
    common::close_tempdir_retrying(index_dir);
    common::close_tempdir_retrying(target_dir);
    Ok(())
}

/// 断点续传：先建索引让图片入队但不处理，模拟"进程在 OCR 队列半程时退出"；
/// 重新打开一次 IndexUpdater（模拟"再启动"）之后，drain_ocr_queue 应该能把
/// 剩下的队列跑完，而不是从头重来——用处理耗时的粗略上界间接验证没有重复识别
/// （重复识别同一张图会让 processed 计数超过图片总数，这里直接断言计数）。
#[test]
fn ocr_queue_survives_restart_and_resumes_pending_work() -> Result<()> {
    common::force_slow_lane_for_tests();

    if !dowse_core::is_available() {
        eprintln!(
            "跳过 ocr_queue_survives_restart_and_resumes_pending_work：\
             本机没有检测到可用的 OCR 语言包"
        );
        return Ok(());
    }

    let index_dir = tempfile::tempdir()?;
    let target_dir = tempfile::Builder::new()
        .prefix("dowse-ocr-restart-")
        .tempdir()?;

    let shot_sentinels = [SENTINEL_SHOT0, SENTINEL_SHOT1];
    for (i, sentinel) in shot_sentinels.iter().enumerate() {
        let path = target_dir.path().join(format!("shot{i}.png"));
        generate_test_image(&path, sentinel)?;
    }

    // rebuild_index 只负责入队，不处理——模拟"程序在队列还没消化完就退出"。
    dowse_core::rebuild_index(index_dir.path(), target_dir.path())?;

    // "重启"：drain_ocr_queue 内部会重新 IndexUpdater::open 一次写入端。
    let stats = dowse_core::drain_ocr_queue(index_dir.path(), 2)?;
    assert_eq!(stats.processed, 2, "两张图片都应该被处理，一张都不该漏");

    // 再跑一次：队列应该已经空了，不会把同样两张图重新识别一遍。
    let second_pass = dowse_core::drain_ocr_queue(index_dir.path(), 2)?;
    assert_eq!(second_pass.processed, 0, "队列应已清空，不该重复处理");

    for (i, sentinel) in shot_sentinels.iter().enumerate() {
        assert!(
            wait_until_searchable(index_dir.path(), sentinel, |n| n > 0),
            "第 {i} 张图片的哨兵词应该能搜到"
        );
    }

    common::close_tempdir_retrying(index_dir);
    common::close_tempdir_retrying(target_dir);
    Ok(())
}
