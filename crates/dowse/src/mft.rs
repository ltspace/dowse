//! MFT 快速枚举：一次性读出整卷的 (FRN, 父 FRN, 文件名) 三元组，不打开任何
//! 文件、不走目录树遍历——设计文档第二节，Everything 同款技术
//! （FSCTL_ENUM_USN_DATA）。
//!
//! 只在 Windows 上编译（lib.rs 里 `mod mft;` 本身已经 `#[cfg(windows)]`
//! 限定，这里不用重复标注）：调用方（indexer.rs 的 fast-path 分支）也是
//! `#[cfg(windows)]` 限定的，不需要跨平台桩实现（对照 volume.rs 的说明）。

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use windows::Win32::Foundation::{CloseHandle, ERROR_HANDLE_EOF, HANDLE};
use windows::Win32::Storage::FileSystem::{
    BY_HANDLE_FILE_INFORMATION, CreateFileW, FILE_FLAG_BACKUP_SEMANTICS, FILE_SHARE_READ,
    FILE_SHARE_WRITE, GetFileInformationByHandle, OPEN_EXISTING,
};
use windows::Win32::System::IO::DeviceIoControl;
use windows::Win32::System::Ioctl::{FSCTL_ENUM_USN_DATA, MFT_ENUM_DATA_V0};
use windows::core::PCWSTR;

use crate::cursor::VolumeKey;
use crate::frn_table::{FrnEntry, FrnTable};
use crate::usn_translate::parse_usn_record_v2_bytes;

/// 一次 MFT 枚举的统计，给日志/性能预算验收用。
pub(crate) struct MftEnumerationStats {
    /// 整卷扫到的原始记录数（含不落在任何监听根内、后来被瘦身掉的那些）。
    pub scanned: usize,
    /// 落在监听根内、最终保留的条目数。
    pub matched: usize,
}

/// FSCTL_ENUM_USN_DATA 一次调用返回的缓冲区大小。64KB 是微软文档给的常见
/// 取值——足够装下几百条记录，调用次数和内存占用之间的平衡点。
const ENUM_BUFFER_SIZE: usize = 64 * 1024;

struct VolumeHandleGuard(HANDLE);
impl Drop for VolumeHandleGuard {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

/// 对一批同卷的注册根做一次 MFT 快速枚举。
///
/// 返回：
/// - 一张只含这些根子树的 [`FrnTable`]——后续交给 USN 事件源复用，
///   保证"启动枚举"到"live 监听"路径解析连续、不留缝；
/// - 这些根下所有该收录文件的路径清单，喂给现有索引管线（先文件名后内容
///   的既有节奏，见设计文档第二节）；
/// - 统计数字。
///
/// 实现上先把整卷记录都收进一张临时表——MFT 按 FRN 顺序返回，父子记录谁先
/// 谁后没有保证，必须全量收完才能可靠地做父链回溯，之后再瘦身到只留监听
/// 根子树（[`FrnTable::retain_in_scope`]），常驻内存的大小只跟监听根下的
/// 文件数成正比,不跟整卷文件数成正比（对应设计文档的内存预算）。
pub(crate) fn enumerate(
    volume: &VolumeKey,
    roots: &[PathBuf],
) -> Result<(FrnTable, Vec<PathBuf>, MftEnumerationStats)> {
    let handle = crate::volume::open_volume_handle(volume)
        .map_err(|e| anyhow::anyhow!("打开卷句柄失败: {e}"))?;
    let _guard = VolumeHandleGuard(handle);

    let mut table = FrnTable::new();
    for root in roots {
        let frn = resolve_frn(root)
            .with_context(|| format!("解析监听根的 FRN 失败: {}", root.display()))?;
        table.register_root(frn, root.clone());
    }

    let mut scanned = 0usize;
    let mut start_frn: u64 = 0;
    let mut buf = vec![0u8; ENUM_BUFFER_SIZE];

    loop {
        let input = MFT_ENUM_DATA_V0 {
            StartFileReferenceNumber: start_frn,
            LowUsn: 0,
            HighUsn: i64::MAX,
        };
        let mut bytes_returned: u32 = 0;
        let result = unsafe {
            DeviceIoControl(
                handle,
                FSCTL_ENUM_USN_DATA,
                Some(std::ptr::from_ref(&input).cast::<core::ffi::c_void>()),
                std::mem::size_of::<MFT_ENUM_DATA_V0>() as u32,
                Some(buf.as_mut_ptr().cast::<core::ffi::c_void>()),
                buf.len() as u32,
                Some(&mut bytes_returned),
                None,
            )
        };
        match result {
            Ok(()) => {}
            // 卷扫到底了：这是正常收尾信号，不是错误。
            Err(err) if err.code() == ERROR_HANDLE_EOF.to_hresult() => break,
            Err(err) => return Err(anyhow::anyhow!("FSCTL_ENUM_USN_DATA 失败: {err}")),
        }
        if (bytes_returned as usize) < 8 {
            break;
        }

        // 缓冲区前 8 字节：下一次调用该从哪个 FRN 继续。
        start_frn = u64::from_ne_bytes(buf[0..8].try_into().expect("8 字节切片"));

        let mut offset = 8usize;
        let end = bytes_returned as usize;
        while offset + 4 <= end {
            let record_length =
                u32::from_ne_bytes(buf[offset..offset + 4].try_into().expect("4 字节切片"))
                    as usize;
            if record_length == 0 || offset + record_length > end {
                break;
            }
            if let Some(record) = parse_usn_record_v2_bytes(&buf[offset..offset + record_length]) {
                table.upsert(
                    record.frn,
                    FrnEntry {
                        parent_frn: record.parent_frn,
                        name: record.name,
                        is_dir: record.is_dir,
                    },
                );
                scanned += 1;
            }
            offset += record_length;
        }
    }

    table.retain_in_scope();
    let matched = table.entry_count();

    let mut files = Vec::with_capacity(matched);
    for (frn, entry) in table.iter() {
        if entry.is_dir {
            continue;
        }
        if let Some(path) = table.reconstruct_path(*frn) {
            files.push(path);
        }
    }

    Ok((table, files, MftEnumerationStats { scanned, matched }))
}

/// 打开一个目录/文件句柄，查出它的 64 位 FRN——用来给注册根本身在 FrnTable
/// 里登记锚点。`FILE_FLAG_BACKUP_SEMANTICS` 是打开目录句柄的必要条件。
fn resolve_frn(path: &Path) -> Result<u64> {
    use std::os::windows::ffi::OsStrExt;
    let wide: Vec<u16> = path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();

    let handle = unsafe {
        CreateFileW(
            PCWSTR(wide.as_ptr()),
            windows::Win32::Storage::FileSystem::FILE_GENERIC_READ.0,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            None,
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS,
            None,
        )
    }
    .map_err(|e| anyhow::anyhow!("打开句柄失败: {e}"))?;
    let _guard = VolumeHandleGuard(handle);

    let mut info = BY_HANDLE_FILE_INFORMATION::default();
    unsafe { GetFileInformationByHandle(handle, &mut info) }
        .map_err(|e| anyhow::anyhow!("查询文件信息失败: {e}"))?;

    Ok(((info.nFileIndexHigh as u64) << 32) | info.nFileIndexLow as u64)
}
