//! 崩溃取证设施：把进程的 stdout/stderr 重定向到
//! `%LOCALAPPDATA%\dowse\logs\dowse.log`（按体积轮转，2 个文件封顶），
//! 再挂一个 panic hook 记下崩溃线程/位置/信息。
//!
//! release 构建用 `windows_subsystem = "windows"`（见 main.rs）没有控制台，
//! 之前散布在 dowse/dowse-app 各处排障用的 `eprintln!`（写入端重试、
//! 快慢车道降级、OCR 批次失败等）在生产环境里全部无声丢失——这里在进程
//! 最开始把标准输出/错误句柄整体重定向到日志文件，不用逐个调用点改造成
//! 显式记日志，覆盖率最高、改动量最小。必须在 `run()` 最开始调用。
//!
//! 只记生命周期/错误/降级事件，不记每次搜索——见 `log_line` 各调用点。

use std::fs::OpenOptions;
use std::path::{Path, PathBuf};

/// 单个日志文件的体积上限，超过就轮转。崩溃排查用的日志量不大，5MB 在正常
/// 使用节奏下能覆盖相当长的运行时间。
const MAX_LOG_BYTES: u64 = 5 * 1024 * 1024;

fn log_dir() -> Option<PathBuf> {
    directories::ProjectDirs::from("", "", "dowse").map(|dirs| dirs.data_local_dir().join("logs"))
}

fn log_file_path(dir: &Path) -> PathBuf {
    dir.join("dowse.log")
}

/// 轮转：当前文件超过体积上限时，把它挪成 `.1`（覆盖旧的 `.1`），空出一个
/// 全新的 `dowse.log`。总共只保留 2 个文件——够崩溃复盘用，不无限堆积。
fn rotate_if_needed(dir: &Path) {
    let current = log_file_path(dir);
    let Ok(meta) = std::fs::metadata(&current) else {
        return;
    };
    if meta.len() < MAX_LOG_BYTES {
        return;
    }
    let rotated = dir.join("dowse.log.1");
    let _ = std::fs::remove_file(&rotated);
    let _ = std::fs::rename(&current, &rotated);
}

/// 初始化：建目录、按需轮转、打开日志文件、重定向 stdout/stderr、挂 panic hook。
/// 任何一步失败都只是放弃日志能力，不影响应用正常启动——诊断设施本身不该
/// 成为新的故障点。
pub fn init() {
    let Some(dir) = log_dir() else {
        return;
    };
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    rotate_if_needed(&dir);

    let path = log_file_path(&dir);
    let Ok(file) = OpenOptions::new().create(true).append(true).open(&path) else {
        return;
    };

    #[cfg(target_os = "windows")]
    redirect_std_handles(&file);
    // `SetStdHandle` only points the process-global STD_ERROR_HANDLE/
    // STD_OUTPUT_HANDLE table entries at this handle *value*——it does not
    // take ownership or bump any refcount. If `file` were allowed to drop
    // normally here, `File::drop` would `CloseHandle` the very handle the OS
    // table now points at, silently invalidating stdout/stderr for the rest
    // of the process (every write after this function returns would fail
    // quietly). Deliberately leak it: this log file's OS handle should live
    // exactly as long as the process anyway, so skipping `Drop` here is the
    // correct lifetime, not actually a leak in the harmful sense.
    std::mem::forget(file);

    log_line(
        "startup",
        &format!("dowse {} 启动", env!("CARGO_PKG_VERSION")),
    );
    install_panic_hook();
}

/// 把当前进程的 STD_OUTPUT_HANDLE / STD_ERROR_HANDLE 都指向日志文件的
/// 底层 Win32 句柄——重定向之后，进程内任何地方（包括 dowse 的
/// `eprintln!`）原样落进日志文件，不需要它们感知到日志系统的存在。
#[cfg(target_os = "windows")]
fn redirect_std_handles(file: &std::fs::File) {
    use std::os::windows::io::AsRawHandle;
    use windows::Win32::Foundation::HANDLE;
    use windows::Win32::System::Console::{STD_ERROR_HANDLE, STD_OUTPUT_HANDLE, SetStdHandle};

    let handle = HANDLE(file.as_raw_handle());
    unsafe {
        let _ = SetStdHandle(STD_OUTPUT_HANDLE, handle);
        let _ = SetStdHandle(STD_ERROR_HANDLE, handle);
    }
}

/// 挂 panic hook：崩溃时把线程名、位置、payload 落一行日志（走的是上面
/// 重定向过的 stderr），再链到系统默认 hook——开发时 `cargo tauri dev`
/// 带控制台，原有的默认崩溃输出照样能看到，不丢失。
fn install_panic_hook() {
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let thread = std::thread::current();
        let thread_name = thread.name().unwrap_or("<unnamed>");
        let location = info
            .location()
            .map(|l| format!("{}:{}:{}", l.file(), l.line(), l.column()))
            .unwrap_or_else(|| "<unknown>".to_string());
        let payload = info
            .payload()
            .downcast_ref::<&str>()
            .map(|s| s.to_string())
            .or_else(|| info.payload().downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "<non-string panic payload>".to_string());
        log_line(
            "panic",
            &format!("线程 [{thread_name}] 在 {location} 崩溃: {payload}"),
        );
        default_hook(info);
    }));
}

/// 记一行带时间戳的日志。`component` 是简短的来源标签（如 "watch"/"rebuild"/
/// "ocr"/"panic"），`msg` 是人类可读的一句话。走 `eprintln!`——`init()` 已经把
/// 进程 stderr 重定向到日志文件，这里不用关心底层是文件还是控制台。
///
/// 克制使用：只记生命周期/错误/降级事件（管线启停、重建开始/结束/失败、
/// 写入端重试与降级、mutex poisoned 等），不记每次搜索/每次文件事件——
/// 那些量级太大，5MB 的轮转窗口几分钟就会被挤爆，反而挤掉真正有用的记录。
pub fn log_line(component: &str, msg: &str) {
    eprintln!("[{}] [{component}] {msg}", format_now());
}

/// 不引入日期时间 crate，手写一个 UTC 时间戳格式化（Howard Hinnant 的
/// civil_from_days 算法）——日志只是给人看的排障材料，精确到秒的 UTC
/// 时间戳完全够用，没必要为了本地时区/多种格式拉一个新依赖。
fn format_now() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    let secs = now.as_secs();
    let (days, rem) = (secs / 86_400, secs % 86_400);
    let (hour, rem) = (rem / 3600, rem % 3600);
    let (min, sec) = (rem / 60, rem % 60);
    let (year, month, day) = civil_from_days(days as i64);
    format!("{year:04}-{month:02}-{day:02} {hour:02}:{min:02}:{sec:02} UTC")
}

/// `days` = 自 1970-01-01 起的天数，返回 (year, month, day)。算法来自
/// Howard Hinnant 的 `civil_from_days`（公开的、被广泛引用的无分支实现）。
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = if m <= 2 { y + 1 } else { y };
    (y, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn civil_from_days_epoch_is_1970_01_01() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
    }

    #[test]
    fn civil_from_days_known_date() {
        // 2024-01-01 是 epoch 之后第 19723 天。
        assert_eq!(civil_from_days(19_723), (2024, 1, 1));
    }
}
