//! 集成测试共用的 tempdir 收尾辅助——四个曾经在 Windows 上 flaky 过的测试文件
//! （reconcile/e2e_watch/incremental/ocr_pipeline）都用这一份，别各自维护一套。
//!
//! flaky 根因：tantivy 的 mmap/合并线程、notify 的目录句柄、OCR worker 释放
//! 句柄不一定跟"测试函数认为自己用完了"这个时间点同步，`TempDir` 默认的
//! `Drop`/`close()` 都只尝试一次 `remove_dir_all`，撞上还没释放的句柄就直接
//! 报 `PermissionDenied`。测试收尾要：
//! 1. 显式 drop 掉持有索引/OCR 句柄的对象（`Searcher`/`IndexUpdater`/
//!    `OcrPipeline` 等），让句柄有机会先释放；
//! 2. 再用生产代码同款的重试退避逻辑（[`dowse::remove_dir_all_retrying`]）
//!    删掉临时目录，而不是指望 `TempDir::close()` 的单次尝试。
//!
//! 另外还带了 [`force_slow_lane_for_tests`]：让普通集成测试确定性地跳过
//! CI 管理员环境下的整卷 MFT 快速枚举，见该函数文档。

/// 关掉一个 `TempDir`，删除失败时按重试退避逻辑再试。
///
/// 不走 `TempDir::close()`——它内部也只是单次 `remove_dir_all`，撞上未释放的
/// 句柄一样会报错，而且 `close()` 一旦返回错误，目录多半没删干净、又没有
/// 办法在同一个 `TempDir` 上再重试一次（`close` 消费了 `self`）。这里改成
/// `keep()` 拿到路径本身（放弃 `TempDir` 自身的自动清理——我们接下来手动删，
/// 不需要它再删一次），直接调重试版本的 `remove_dir_all`。
pub fn close_tempdir_retrying(dir: tempfile::TempDir) {
    let path = dir.keep();
    if let Err(err) = dowse::remove_dir_all_retrying(&path) {
        eprintln!(
            "测试收尾清理临时目录失败，重试后仍未成功（不影响断言结果）{}: {err}",
            path.display()
        );
    }
}

/// 逃生舱开关：调一次就够，让本进程接下来所有 `rebuild_index`/`watch_roots_auto`
/// 调用确定性地走 walkdir + notify 慢车道，不管跑机是不是管理员——见
/// `dowse`（`volume.rs`）里 `DOWSE_FORCE_SLOW_LANE` 的文档。
///
/// 只给"只需要索引能用"的普通集成测试用（ocr_pipeline/e2e_watch/incremental/
/// reconcile/multi_root/office_extract）。`tests/ntfs_fast_path.rs` 专门验证
/// 真快车道本身，绝不能调这个函数。
///
/// 调用方要求：每个会触碰 `rebuild_index`/`watch_roots_auto` 的 `#[test]` 函数
/// 自己在最前面调一次——不是"某个测试调了全局就生效"那种隐式约定。原因：
/// Rust 集成测试同一个二进制内的多个 `#[test]` 默认并行跑在不同线程，
/// `env::set_var`（2024 edition 起标记 unsafe）不是线程安全的写入；用 `Once`
/// 收敛后，不管多少个测试线程同时调用这个函数，实际只会执行一次真正的
/// `set_var`，且 `call_once` 保证所有线程从这个函数返回时那次写入已经完成
/// ——只要调用方遵守"用之前先调"这一条，就不会有别的测试线程读到设置到一半
/// 的环境变量。
///
/// `common/mod.rs` 按源码整份编入每个 `mod common;` 的测试二进制——
/// `ntfs_fast_path.rs` 也 `mod common;`（用它的 `close_tempdir_retrying`），
/// 但按上面的规则绝不会调这个函数，因此在那个二进制里这两项天然"没被用到"；
/// `#[allow(dead_code)]` 就是为了那一个二进制不被 `-D warnings` 判死，不代表
/// 这份代码本身没人用（另外 5 个测试文件都在用）。
#[allow(dead_code)]
static FORCE_SLOW_LANE_INIT: std::sync::Once = std::sync::Once::new();

#[allow(dead_code)]
pub fn force_slow_lane_for_tests() {
    FORCE_SLOW_LANE_INIT.call_once(|| {
        // Safety: 全进程只有这一处写这个环境变量，`Once` 保证只执行一次；
        // 调用方约定"用 rebuild_index/watch_roots_auto 之前先调这个函数"，
        // 所以不存在与本函数并发的读者。
        unsafe {
            std::env::set_var("DOWSE_FORCE_SLOW_LANE", "1");
        }
    });
}
