use tauri::{Emitter, PhysicalPosition, WebviewWindow};

/// Apply native rounded corners to the opaque window.
#[cfg(target_os = "windows")]
pub fn apply_rounded_corners(window: &WebviewWindow) {
    const DWMWA_WINDOW_CORNER_PREFERENCE: u32 = 33;
    const DWMWCP_ROUND: i32 = 2;

    #[link(name = "dwmapi")]
    unsafe extern "system" {
        fn DwmSetWindowAttribute(
            hwnd: *mut core::ffi::c_void,
            dw_attribute: u32,
            pv_attribute: *const core::ffi::c_void,
            cb_attribute: u32,
        ) -> i32;
    }

    let Ok(hwnd) = window.hwnd() else {
        eprintln!("圆角裁切：拿不到 HWND，跳过");
        return;
    };

    let preference: i32 = DWMWCP_ROUND;
    let hr = unsafe {
        DwmSetWindowAttribute(
            hwnd.0,
            DWMWA_WINDOW_CORNER_PREFERENCE,
            &preference as *const i32 as *const core::ffi::c_void,
            std::mem::size_of::<i32>() as u32,
        )
    };
    if hr != 0 {
        eprintln!(
            "圆角裁切：DwmSetWindowAttribute 返回 HRESULT 0x{hr:x}，可能是系统版本太老（Win10 v20H1 以前没有这个属性）"
        );
    }
}

/// 窗口居中偏上——参照 Spotlight/Raycast 的位置习惯，不是正中央。
/// 屏幕高度的约 22% 处起摆，比 50% 正中更符合"呼出即用"的视觉预期。
///
/// v0.4.1 把窗口从 750×500 放大到 860×560 时复核过这条逻辑：这里用的是
/// `window.outer_size()`（整个窗口——v0.4.2 起 .panel 满铺窗口，两者已经
/// 是同一个尺寸，见 app.css 里双框 bug 的修复说明），横向居中和纵向 22%
/// 起摆都是相对屏幕尺寸的比例计算，不依赖任何写死的像素基准，换了窗口
/// 尺寸不需要跟着改数。
pub fn position_upper_center(window: &WebviewWindow) -> tauri::Result<()> {
    let Some(monitor) = window.current_monitor()? else {
        return Ok(());
    };
    let screen_size = *monitor.size();
    let screen_pos = *monitor.position();
    let win_size = window.outer_size()?;

    let x = screen_pos.x + (screen_size.width as i32 - win_size.width as i32) / 2;
    let y = screen_pos.y + (screen_size.height as f64 * 0.22) as i32;

    window.set_position(PhysicalPosition::new(x, y))
}

/// 呼出：定位到居中偏上、显示、抢焦点，再广播一个事件给前端——
/// 前端监听它来做"输入框自动聚焦、上次查询词全选"（设计文档的交互规则）。
pub fn show_window(window: &WebviewWindow) {
    let _ = position_upper_center(window);
    let _ = window.show();
    let _ = window.set_focus();
    let _ = window.emit("dowse://shown", ());
}

pub fn hide_window(window: &WebviewWindow) {
    let _ = window.hide();
}

pub fn toggle_window(window: &WebviewWindow) {
    if window.is_visible().unwrap_or(false) {
        hide_window(window);
    } else {
        show_window(window);
    }
}
