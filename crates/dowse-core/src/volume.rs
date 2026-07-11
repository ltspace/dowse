//! 卷能力探测：一个监听根该走 NTFS MFT/USN 快车道还是 walkdir/notify 慢车道
//! （[`probe_root_capability`]，见 [`RootCapability`]）。判定结果按卷缓存，
//! 进程生命周期内只探测一次——权限令牌和卷文件系统类型在进程运行期间不会
//! 变化。诚实降级：拿不到卷句柄（非管理员）就静默走慢车道，不报错、不崩溃。

use std::collections::HashMap;
use std::path::Path;
use std::sync::{LazyLock, Mutex};

use crate::cursor::VolumeKey;

/// 一个监听根按卷判定后的结论（设计文档第一节的降级判定框图）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RootCapability {
    /// NTFS + 拿得到卷句柄（管理员权限）：走 MFT 快速枚举 + USN 事件源。
    Fast { volume: VolumeKey },
    /// 走现有 walkdir + notify 路径。reason 只给日志看，不影响行为——
    /// 两条路径产出的事件和文档完全一致，上层感知不到差别（设计文档原话）。
    Fallback { reason: String },
}

/// 从一个绝对路径推出它所在卷的标识（大写盘符+冒号，如 `"C:"`）。
/// 用 `Path::components()` 的 `Prefix` 分量——这是 std 里天然跨平台存在的
/// API（非 Windows 平台上 Windows 风格路径就是不会匹配到 Prefix，返回
/// None，不需要额外 cfg）。
pub(crate) fn volume_key(path: &Path) -> Option<VolumeKey> {
    use std::path::{Component, Prefix};
    match path.components().next()? {
        Component::Prefix(prefix) => match prefix.kind() {
            Prefix::Disk(letter) | Prefix::VerbatimDisk(letter) => {
                Some(format!("{}:", (letter as char).to_ascii_uppercase()))
            }
            _ => None,
        },
        _ => None,
    }
}

/// 每卷的判定结果只探测一次、进程生命周期内复用。原因有两条：
///
/// 1. 语义上站得住：Windows 进程的权限令牌（是否管理员）在整个进程生命周期
///    内不会变——"以管理员身份运行"是启动时的选择，不是运行期可以切换的
///    状态；卷的文件系统类型（NTFS/非 NTFS）就更不会在进程运行期间变化。
///    重复探测同一个卷得到的结论必然一样，缓存不会让判定过期。
/// 2. 实测发现的真实代价：非管理员权限下反复尝试开原始卷句柄
///    （`\\.\C:`，见 platform::open_volume_handle）虽然每次都会诚实失败、
///    正确降级，但在本机测试中观察到一个会重复触发的副作用——紧跟在失败的
///    探测调用之后，同一进程里对**无关文件**（tantivy 索引的临时分段文件）
///    的写入偶发被拒绝访问（约 1/3 概率的瞬时 PermissionDenied，不是资源
///    泄漏那种持续恶化的模式，也确认过 Windows Defender 实时防护在本机是
///    关闭的，不是常见的"AV 扫描锁文件"那套解释）。集成测试里同一进程要跑
///    几十个 `rebuild_index` 调用，探测次数被放大，才把这个概率问题变得
///    容易复现；真实产品里一个进程生命周期内探测次数少得多，但既然行为 1
///    已经保证缓存是安全的，干脆把探测次数降到每卷一次，从源头把这条不
///    完全确定成因的副作用面降到最小，不需要先查清楚它的根因才能规避它。
static CAPABILITY_CACHE: LazyLock<Mutex<HashMap<VolumeKey, RootCapability>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// 只给集成测试用的逃生舱：设了这个环境变量（任意非空值）就无条件判定
/// Fallback，连 NTFS/管理员探测都不做。跟 `DOWSE_INDEX_DIR`
/// （dowse-cli/src/main.rs）同一类东西——只给测试用，不是产品对外配置项。
///
/// 动机：CI 跑机（GitHub Actions windows-latest）是管理员身份，`rebuild_index`/
/// `watch_roots_auto` 在这种环境下会真的走 MFT 快速枚举，而它枚举的是**整卷**
/// （系统盘 C: 实测 ~130 万条 MFT 记录，几十秒量级），远超"只需要索引能用"的
/// 普通集成测试（ocr_pipeline/e2e_watch/incremental/reconcile 等）原本设计的
/// 轮询等待预算——这类测试只关心索引结果对不对，不关心走的是哪条车道。之前
/// 靠反复放宽轮询超时来兜（见 e2e_watch.rs 模块文档的排障记录）治标不治本，
/// 每次 CI 随机冲垮一个不同的测试。这个逃生舱让那些测试改成确定性地走
/// walkdir + notify 慢车道，快、稳定，且两条车道产出的文档/事件本来就承诺
/// 完全一致（设计文档原话），走慢车道不改变被测行为。
///
/// `tests/ntfs_fast_path.rs` 是唯一的例外：它就是专门验证真快车道（MFT 枚举 +
/// USN Journal）本身的测试，绝不能设这个变量，否则会在管理员环境下也静默
/// 退化成只测降级路径，白白丢失覆盖。
const FORCE_SLOW_LANE_ENV: &str = "DOWSE_FORCE_SLOW_LANE";

/// 探测一个监听根应该走快车道还是慢车道。诚实降级：拿不到卷句柄（非管理员）
/// 就静默走现有 notify 路径，不报错、不崩溃，只打一行日志说明原因
/// （设计文档"与现有架构的关系"一节："诚实降级：落到哪条路径、为什么，
/// 写日志"）。按卷缓存结果，见 `CAPABILITY_CACHE` 的文档。
pub(crate) fn probe_root_capability(root: &Path) -> RootCapability {
    // `dowse-core` 自己 `#[cfg(test)] mod tests`（indexer.rs/meta.rs/roots.rs/
    // searcher.rs/status.rs/updater.rs 等）里散落的几十处 `rebuild_index` 调用
    // 同样只关心索引结果对不对，不关心走哪条车道——同上面 `FORCE_SLOW_LANE_ENV`
    // 一个动机，但这些是库自身的单元测试，不值得每个调用点都手动加一次逃生舱。
    // `cfg!(test)` 在这里是精确的开关：它只在 `dowse-core` 编译自己的单元测试
    // 二进制（`cargo test -p dowse-core --lib`）时为真；`tests/*.rs` 下的集成
    // 测试（含 `ntfs_fast_path.rs`）是把 `dowse-core` 当普通依赖库链接，编译时
    // 不带 `--cfg test`，不受这条短路影响，快车道覆盖不受损。
    if cfg!(test) || std::env::var_os(FORCE_SLOW_LANE_ENV).is_some() {
        return RootCapability::Fallback {
            reason: "测试环境（cfg(test) 或 DOWSE_FORCE_SLOW_LANE 逃生舱），强制走慢车道"
                .to_string(),
        };
    }

    let Some(vol) = volume_key(root) else {
        // 连卷标识都解析不出来，没法缓存，也没必要——这种路径本来就走不到
        // 探测这一步之后的任何缓存收益。
        let capability = platform::probe(root);
        log_capability(root, &capability);
        return capability;
    };

    if let Some(cached) = CAPABILITY_CACHE
        .lock()
        .expect("capability cache mutex poisoned")
        .get(&vol)
    {
        return cached.clone();
    }

    let capability = platform::probe(root);
    log_capability(root, &capability);
    CAPABILITY_CACHE
        .lock()
        .expect("capability cache mutex poisoned")
        .insert(vol, capability.clone());
    capability
}

fn log_capability(root: &Path, capability: &RootCapability) {
    match capability {
        RootCapability::Fast { volume } => {
            eprintln!("{volume}: NTFS + 管理员权限，启用 MFT/USN 快速路径");
        }
        RootCapability::Fallback { reason } => {
            eprintln!(
                "{}: 走现有 walkdir + notify 路径（{reason}）",
                root.display()
            );
        }
    }
}

/// 供集成测试用的权限护栏：这个路径能不能走 NTFS 快速路径（MFT 枚举 +
/// USN Journal）。跟 `dowse_core::is_available()`（OCR 语言包探测）是同一个
/// 用途——测试开头先探测一次，探测不到就打印原因跳过，不需要管理员权限的
/// 测试永远能跑，需要的测试在非管理员机器/CI 上不会把构建搞红。
pub fn ntfs_fast_path_available(root: &Path) -> bool {
    matches!(probe_root_capability(root), RootCapability::Fast { .. })
}

/// 打开一个原始卷句柄（`\\.\C:` 形式），mft.rs/usn.rs 共用。只在 Windows 上
/// 存在——调用方（indexer.rs/watcher.rs 的 fast-path 分支）本身也是
/// `#[cfg(windows)]` 限定的，不需要跨平台桩实现。
#[cfg(windows)]
pub(crate) fn open_volume_handle(
    volume: &VolumeKey,
) -> windows::core::Result<windows::Win32::Foundation::HANDLE> {
    platform::open_volume_handle(volume)
}

#[cfg(windows)]
mod platform {
    use std::path::Path;

    use windows::Win32::Foundation::{CloseHandle, HANDLE};
    use windows::Win32::Storage::FileSystem::{
        CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_SHARE_READ, FILE_SHARE_WRITE,
        GetVolumeInformationW, GetVolumePathNameW, OPEN_EXISTING,
    };
    use windows::core::PCWSTR;

    use super::{RootCapability, volume_key};

    /// 把 root 归一到卷根路径（如 `C:\Users\foo\docs` -> `C:\`），
    /// GetVolumeInformationW/CreateFileW 打开卷句柄都要这个形式。
    fn volume_root_path(root: &Path) -> Option<Vec<u16>> {
        use std::os::windows::ffi::OsStrExt;

        let wide: Vec<u16> = root
            .as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();
        let mut buf = vec![0u16; 261]; // MAX_PATH + 1，卷根路径不会超
        unsafe { GetVolumePathNameW(PCWSTR(wide.as_ptr()), &mut buf) }.ok()?;
        let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
        buf.truncate(len);
        Some(buf)
    }

    fn is_ntfs(volume_root: &[u16]) -> bool {
        let mut fs_name = vec![0u16; 32];
        let ok = unsafe {
            GetVolumeInformationW(
                PCWSTR(volume_root.as_ptr()),
                None,
                None,
                None,
                None,
                Some(&mut fs_name),
            )
        }
        .is_ok();
        if !ok {
            return false;
        }
        let len = fs_name
            .iter()
            .position(|&c| c == 0)
            .unwrap_or(fs_name.len());
        String::from_utf16_lossy(&fs_name[..len]).eq_ignore_ascii_case("NTFS")
    }

    /// 试着开一个原始卷句柄（`\\.\C:` 形式）。开不开得下来就是"有没有管理员
    /// 权限"的诚实检验——设计文档明确用这个信号做降级判定，不额外查进程
    /// token/权限位。mft.rs/usn.rs 也用这个函数开长期持有的卷句柄，不是只有
    /// 探测阶段才用——两边共用同一份 FFI，别各写一遍。
    pub(crate) fn open_volume_handle(volume: &str) -> windows::core::Result<HANDLE> {
        let mut device_path: Vec<u16> = r"\\.\".encode_utf16().collect();
        device_path.extend(volume.encode_utf16());
        device_path.push(0);

        unsafe {
            CreateFileW(
                PCWSTR(device_path.as_ptr()),
                windows::Win32::Storage::FileSystem::FILE_GENERIC_READ.0,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                None,
                OPEN_EXISTING,
                FILE_ATTRIBUTE_NORMAL,
                None,
            )
        }
    }

    pub(super) fn probe(root: &Path) -> RootCapability {
        let Some(volume_root) = volume_root_path(root) else {
            return RootCapability::Fallback {
                reason: "解析不出卷根路径".to_string(),
            };
        };
        if !is_ntfs(&volume_root) {
            return RootCapability::Fallback {
                reason: "非 NTFS 卷".to_string(),
            };
        }
        let Some(volume) = volume_key(root) else {
            return RootCapability::Fallback {
                reason: "解析不出盘符".to_string(),
            };
        };
        match open_volume_handle(&volume) {
            Ok(handle) => {
                unsafe {
                    let _ = CloseHandle(handle);
                }
                RootCapability::Fast { volume }
            }
            Err(err) => RootCapability::Fallback {
                reason: format!("未获管理员权限，该卷改用通用索引方式，功能不受影响: {err}"),
            },
        }
    }
}

#[cfg(not(windows))]
mod platform {
    use std::path::Path;

    use super::RootCapability;

    pub(super) fn probe(_root: &Path) -> RootCapability {
        RootCapability::Fallback {
            reason: "非 Windows 平台".to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn volume_key_extracts_uppercase_drive_letter() {
        if cfg!(windows) {
            assert_eq!(
                volume_key(&PathBuf::from(r"C:\Users\foo")),
                Some("C:".to_string())
            );
            assert_eq!(
                volume_key(&PathBuf::from(r"d:\data")),
                Some("D:".to_string())
            );
        }
    }

    #[test]
    fn volume_key_of_relative_path_is_none() {
        assert_eq!(volume_key(&PathBuf::from("relative/path")), None);
    }

    #[cfg(not(windows))]
    #[test]
    fn non_windows_always_falls_back() {
        let cap = probe_root_capability(&PathBuf::from("/tmp/watch"));
        assert!(matches!(cap, RootCapability::Fallback { .. }));
    }
}
