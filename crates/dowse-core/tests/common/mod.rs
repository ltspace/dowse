//! 集成测试共用的 tempdir 收尾辅助——四个曾经在 Windows 上 flaky 过的测试文件
//! （reconcile/e2e_watch/incremental/ocr_pipeline）都用这一份，别各自维护一套。
//!
//! flaky 根因：tantivy 的 mmap/合并线程、notify 的目录句柄、OCR worker 释放
//! 句柄不一定跟"测试函数认为自己用完了"这个时间点同步，`TempDir` 默认的
//! `Drop`/`close()` 都只尝试一次 `remove_dir_all`，撞上还没释放的句柄就直接
//! 报 `PermissionDenied`。测试收尾要：
//! 1. 显式 drop 掉持有索引/OCR 句柄的对象（`Searcher`/`IndexUpdater`/
//!    `OcrPipeline` 等），让句柄有机会先释放；
//! 2. 再用生产代码同款的重试退避逻辑（[`dowse_core::remove_dir_all_retrying`]）
//!    删掉临时目录，而不是指望 `TempDir::close()` 的单次尝试。

/// 关掉一个 `TempDir`，删除失败时按重试退避逻辑再试。
///
/// 不走 `TempDir::close()`——它内部也只是单次 `remove_dir_all`，撞上未释放的
/// 句柄一样会报错，而且 `close()` 一旦返回错误，目录多半没删干净、又没有
/// 办法在同一个 `TempDir` 上再重试一次（`close` 消费了 `self`）。这里改成
/// `keep()` 拿到路径本身（放弃 `TempDir` 自身的自动清理——我们接下来手动删，
/// 不需要它再删一次），直接调重试版本的 `remove_dir_all`。
pub fn close_tempdir_retrying(dir: tempfile::TempDir) {
    let path = dir.keep();
    if let Err(err) = dowse_core::remove_dir_all_retrying(&path) {
        eprintln!(
            "测试收尾清理临时目录失败，重试后仍未成功（不影响断言结果）{}: {err}",
            path.display()
        );
    }
}
