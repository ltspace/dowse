//! 结果行/预览区的"真实文件图标"——问系统"这个扩展名的文件长什么图标"，
//! 而不是自己手绘一套通用图形。Windows 侧用 `SHGetFileInfoW` 按扩展名取关联
//! 图标（`SHGFI_USEFILEATTRIBUTES`：只问扩展名，不需要文件真的存在/被打开），
//! 转成 PNG 编码成 base64 data URI 传给前端 `<img>` 直接用。
//!
//! 按扩展名缓存在进程常驻的 `HashMap` 里：同一个扩展名（比如一屏结果里
//! 一堆 `.rs`）只问系统一次，后续直接命中缓存——图标提取要过 GDI，
//! 比字符串比较贵得多，缓存是这个功能能用的前提，不是可选优化。

use std::collections::HashMap;
use std::sync::Mutex;

/// 扩展名 -> 图标 data URI（`None` 表示问过系统但没拿到图标，也要缓存这个
/// "没有"结果，不然每次都重新问一遍系统白跑一趟 GDI 调用）。
#[derive(Default)]
pub struct FileIconCache(Mutex<HashMap<String, Option<String>>>);

impl FileIconCache {
    pub fn new() -> Self {
        Self::default()
    }

    /// 取某个扩展名的图标 data URI，命中缓存就直接还，没有就问系统并记下来。
    /// `ext` 传空字符串代表"没有扩展名的文件"，系统会给一个通用文件图标。
    pub fn get(&self, ext: &str) -> Option<String> {
        let key = ext.to_lowercase();
        {
            let cache = self.0.lock().expect("file icon cache mutex poisoned");
            if let Some(hit) = cache.get(&key) {
                return hit.clone();
            }
        }
        let fetched = fetch_icon_data_uri(&key);
        self.0
            .lock()
            .expect("file icon cache mutex poisoned")
            .insert(key, fetched.clone());
        fetched
    }
}

#[cfg(target_os = "windows")]
fn fetch_icon_data_uri(ext: &str) -> Option<String> {
    use windows::Win32::Storage::FileSystem::FILE_ATTRIBUTE_NORMAL;
    use windows::Win32::UI::Shell::{
        SHFILEINFOW, SHGFI_ICON, SHGFI_SMALLICON, SHGFI_USEFILEATTRIBUTES, SHGetFileInfoW,
    };
    use windows::Win32::UI::WindowsAndMessaging::DestroyIcon;
    use windows::core::PCWSTR;

    // SHGetFileInfoW 靠路径里的扩展名做关联查找，配 SHGFI_USEFILEATTRIBUTES
    // 之后完全不碰磁盘——传一个不存在的 "file.ext" 路径就够，不需要真文件。
    let fake_name = if ext.is_empty() {
        "file".to_string()
    } else {
        format!("file.{ext}")
    };
    let wide: Vec<u16> = fake_name.encode_utf16().chain(std::iter::once(0)).collect();

    let mut info = SHFILEINFOW::default();
    let flags = SHGFI_ICON | SHGFI_SMALLICON | SHGFI_USEFILEATTRIBUTES;
    let ok = unsafe {
        SHGetFileInfoW(
            PCWSTR(wide.as_ptr()),
            FILE_ATTRIBUTE_NORMAL,
            Some(&mut info),
            std::mem::size_of::<SHFILEINFOW>() as u32,
            flags,
        )
    };
    if ok == 0 || info.hIcon.is_invalid() {
        return None;
    }

    let png = hicon_to_png(info.hIcon);
    unsafe {
        let _ = DestroyIcon(info.hIcon);
    }
    png.map(|bytes| format!("data:image/png;base64,{}", base64_encode(&bytes)))
}

/// HICON -> PNG 字节。走 GDI：先 `GetIconInfo` 拆出颜色位图和掩码位图，
/// `GetObjectW` 量出尺寸，再用 `GetDIBits` 把颜色位图整段拷成 32bpp 顶到底的
/// BGRA 缓冲区。
///
/// 现代（Vista+）小图标的颜色位图本身就是带真 alpha 通道的 32bpp 位图，
/// 直接读出来的 alpha 就是对的；极少数老式图标颜色位图不带 alpha（读出来
/// 整个 alpha 通道全 0），这种情况下退回掩码位图重建 alpha——掩码是 1bpp，
/// 1 代表透明、0 代表不透明。
#[cfg(target_os = "windows")]
fn hicon_to_png(hicon: windows::Win32::UI::WindowsAndMessaging::HICON) -> Option<Vec<u8>> {
    use windows::Win32::Graphics::Gdi::{
        BI_RGB, BITMAP, BITMAPINFO, BITMAPINFOHEADER, CreateCompatibleDC, DIB_RGB_COLORS, DeleteDC,
        DeleteObject, GetDIBits, GetObjectW,
    };
    use windows::Win32::UI::WindowsAndMessaging::{GetIconInfo, ICONINFO};

    unsafe {
        let mut icon_info = ICONINFO::default();
        if GetIconInfo(hicon, &mut icon_info).is_err() {
            return None;
        }
        // hbmColor 在极少数场景下可能是空句柄（单色图标只有掩码没有颜色位图），
        // 这种图标本来就罕见到可以直接放弃、走前端的手绘兜底。
        if icon_info.hbmColor.is_invalid() {
            let _ = DeleteObject(icon_info.hbmMask.into());
            return None;
        }

        let mut bmp = BITMAP::default();
        let bmp_size = std::mem::size_of::<BITMAP>() as i32;
        if GetObjectW(
            icon_info.hbmColor.into(),
            bmp_size,
            Some(&mut bmp as *mut BITMAP as *mut core::ffi::c_void),
        ) == 0
        {
            let _ = DeleteObject(icon_info.hbmColor.into());
            let _ = DeleteObject(icon_info.hbmMask.into());
            return None;
        }
        let width = bmp.bmWidth;
        let height = bmp.bmHeight;
        if width <= 0 || height <= 0 {
            let _ = DeleteObject(icon_info.hbmColor.into());
            let _ = DeleteObject(icon_info.hbmMask.into());
            return None;
        }

        let hdc = CreateCompatibleDC(None);

        let header = BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: width,
            biHeight: -height, // 负数＝顶到底存储，省得再手动翻行
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            ..Default::default()
        };
        let mut bmi = BITMAPINFO {
            bmiHeader: header,
            ..Default::default()
        };

        let pixel_count = (width as usize) * (height as usize);
        let mut buf = vec![0u8; pixel_count * 4];
        let scan_lines = GetDIBits(
            hdc,
            icon_info.hbmColor,
            0,
            height as u32,
            Some(buf.as_mut_ptr() as *mut core::ffi::c_void),
            &mut bmi,
            DIB_RGB_COLORS,
        );

        if scan_lines == 0 {
            let _ = DeleteDC(hdc);
            let _ = DeleteObject(icon_info.hbmColor.into());
            let _ = DeleteObject(icon_info.hbmMask.into());
            return None;
        }

        // BGRA -> RGBA，同时判断这批像素是不是带了真 alpha。
        let mut has_alpha = false;
        for px in buf.chunks_exact_mut(4) {
            px.swap(0, 2); // B<->R
            if px[3] != 0 {
                has_alpha = true;
            }
        }

        if !has_alpha {
            // 复用同一个 hdc 读掩码位图，不额外开一个 DC。
            apply_mask_alpha(hdc, icon_info.hbmMask, width, height, &mut buf);
        }

        let _ = DeleteDC(hdc);
        let _ = DeleteObject(icon_info.hbmColor.into());
        let _ = DeleteObject(icon_info.hbmMask.into());

        Some(encode_rgba_png(&buf, width as u32, height as u32))
    }
}

/// 用掩码位图（1bpp，1＝透明）重建 alpha 通道，覆盖 `buf` 里已经翻好的 RGBA。
#[cfg(target_os = "windows")]
fn apply_mask_alpha(
    hdc: windows::Win32::Graphics::Gdi::HDC,
    hbm_mask: windows::Win32::Graphics::Gdi::HBITMAP,
    width: i32,
    height: i32,
    buf: &mut [u8],
) {
    use windows::Win32::Graphics::Gdi::{
        BI_RGB, BITMAPINFO, BITMAPINFOHEADER, DIB_RGB_COLORS, GetDIBits,
    };

    // 1bpp DIB 每行按 4 字节对齐。
    let stride = (width as usize).div_ceil(32) * 4;
    let mut mask_buf = vec![0u8; stride * height as usize];
    let header = BITMAPINFOHEADER {
        biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
        biWidth: width,
        biHeight: -height,
        biPlanes: 1,
        biBitCount: 1,
        biCompression: BI_RGB.0,
        ..Default::default()
    };
    let mut bmi = BITMAPINFO {
        bmiHeader: header,
        ..Default::default()
    };

    let ok = unsafe {
        GetDIBits(
            hdc,
            hbm_mask,
            0,
            height as u32,
            Some(mask_buf.as_mut_ptr() as *mut core::ffi::c_void),
            &mut bmi,
            DIB_RGB_COLORS,
        )
    };
    if ok == 0 {
        // 掩码也读不出来：宁可整张图不透明地显示，也不要留一张全透明的空白图。
        for px in buf.chunks_exact_mut(4) {
            px[3] = 255;
        }
        return;
    }

    for y in 0..height as usize {
        for x in 0..width as usize {
            let byte = mask_buf[y * stride + x / 8];
            let bit_transparent = (byte >> (7 - (x % 8))) & 1 == 1;
            let idx = (y * width as usize + x) * 4 + 3;
            buf[idx] = if bit_transparent { 0 } else { 255 };
        }
    }
}

#[cfg(target_os = "windows")]
fn encode_rgba_png(rgba: &[u8], width: u32, height: u32) -> Vec<u8> {
    let mut out = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut out, width, height);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder
            .write_header()
            .expect("PNG 编码器写 header 不应该失败");
        writer
            .write_image_data(rgba)
            .expect("PNG 编码写像素数据不应该失败");
    }
    out
}

/// 极小的 base64 编码器——项目里目前没有别的地方需要 base64，为这一处引入
/// 整个 `base64` crate 划不来，标准字母表 + 补齐这几行代码够用。
#[cfg(target_os = "windows")]
fn base64_encode(data: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b0 = chunk[0];
        let b1 = *chunk.get(1).unwrap_or(&0);
        let b2 = *chunk.get(2).unwrap_or(&0);
        out.push(ALPHABET[(b0 >> 2) as usize] as char);
        out.push(ALPHABET[(((b0 & 0x03) << 4) | (b1 >> 4)) as usize] as char);
        out.push(if chunk.len() > 1 {
            ALPHABET[(((b1 & 0x0f) << 2) | (b2 >> 6)) as usize] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            ALPHABET[(b2 & 0x3f) as usize] as char
        } else {
            '='
        });
    }
    out
}

#[cfg(not(target_os = "windows"))]
fn fetch_icon_data_uri(_ext: &str) -> Option<String> {
    None
}

#[cfg(all(test, target_os = "windows"))]
mod tests {
    use super::*;

    #[test]
    fn base64_encode_matches_known_vectors() {
        assert_eq!(base64_encode(b""), "");
        assert_eq!(base64_encode(b"f"), "Zg==");
        assert_eq!(base64_encode(b"fo"), "Zm8=");
        assert_eq!(base64_encode(b"foo"), "Zm9v");
        assert_eq!(base64_encode(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn cache_returns_consistent_result_for_same_extension() {
        let cache = FileIconCache::new();
        let first = cache.get("rs");
        let second = cache.get("RS"); // 大小写不应该影响缓存命中
        assert_eq!(first, second);
    }
}
