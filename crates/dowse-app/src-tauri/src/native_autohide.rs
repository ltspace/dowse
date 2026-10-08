//! WebView keyboard focus can leave during native move/resize while the top-level
//! window remains active. Only actual top-level deactivation should auto-hide.
//! This also avoids timer/JS mouseup races when Windows owns the modal drag loop.
use tauri::Manager;
use windows::Win32::{
    Foundation::{HWND, LPARAM, LRESULT, WPARAM},
    UI::{
        Shell::{DefSubclassProc, RemoveWindowSubclass, SetWindowSubclass},
        WindowsAndMessaging::{SW_HIDE, ShowWindow, WA_INACTIVE, WM_ACTIVATE, WM_NCDESTROY},
    },
};

use crate::autohide::AutoHideSuppressor;

const SUBCLASS_ID: usize = 0x44575345;
struct Context(Box<dyn Fn() -> bool>);

/// Called from Tauri setup on the window's owning UI thread.
pub fn install(window: &tauri::WebviewWindow) -> anyhow::Result<()> {
    let hwnd = HWND(window.hwnd()?.0);
    let app = window.app_handle().clone();
    install_native(
        hwnd,
        Box::new(move || app.state::<AutoHideSuppressor>().is_suppressed()),
    )
}

fn install_native(hwnd: HWND, suppressed: Box<dyn Fn() -> bool>) -> anyhow::Result<()> {
    let context = Box::into_raw(Box::new(Context(suppressed)));
    // The subclass owns this allocation until WM_NCDESTROY. No cross-thread access.
    if !unsafe { SetWindowSubclass(hwnd, Some(subclass), SUBCLASS_ID, context as usize) }.as_bool()
    {
        unsafe {
            drop(Box::from_raw(context));
        }
        anyhow::bail!("could not install native window auto-hide handler");
    }
    Ok(())
}

unsafe extern "system" fn subclass(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    id: usize,
    data: usize,
) -> LRESULT {
    if message == WM_NCDESTROY {
        unsafe {
            let _ = RemoveWindowSubclass(hwnd, Some(subclass), id);
            drop(Box::from_raw(data as *mut Context));
        }
    } else if message == WM_ACTIVATE && (wparam.0 & 0xffff) == WA_INACTIVE as usize {
        // Finish reading context before ShowWindow, which can re-enter the wndproc.
        let suppressed = unsafe { ((*(data as *const Context)).0)() };
        if !suppressed {
            unsafe {
                let _ = ShowWindow(hwnd, SW_HIDE);
            }
        }
    }
    // Keep Tauri, WebView, and other native subclasses receiving their messages.
    unsafe { DefSubclassProc(hwnd, message, wparam, lparam) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{cell::Cell, rc::Rc};
    use windows::{Win32::UI::WindowsAndMessaging::*, core::w};

    #[test]
    fn native_gestures_keep_window_visible_but_deactivation_hides() {
        // An off-screen, non-activating native window tests real message dispatch
        // without stealing focus from the user's desktop or starting the app.
        unsafe {
            let hwnd = CreateWindowExW(
                WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
                w!("STATIC"),
                w!("dowse auto-hide regression"),
                WS_POPUP,
                -32000,
                -32000,
                10,
                10,
                None,
                None,
                None,
                None,
            )
            .unwrap();
            let suppressed = Rc::new(Cell::new(false));
            let flag = suppressed.clone();
            install_native(hwnd, Box::new(move || flag.get())).unwrap();
            let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);

            // Focus may leave before the OS announces entry into its modal loop.
            for message in [
                WM_KILLFOCUS,
                WM_ENTERSIZEMOVE,
                WM_KILLFOCUS,
                WM_EXITSIZEMOVE,
            ] {
                SendMessageW(hwnd, message, Some(WPARAM(0)), Some(LPARAM(0)));
                assert!(IsWindowVisible(hwnd).as_bool(), "hidden by {message:#x}");
            }
            SendMessageW(
                hwnd,
                WM_ACTIVATE,
                Some(WPARAM(WA_INACTIVE as usize)),
                Some(LPARAM(0)),
            );
            assert!(!IsWindowVisible(hwnd).as_bool());

            // Pin/menu exemptions remain effective, and release restores auto-hide.
            suppressed.set(true);
            let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
            SendMessageW(
                hwnd,
                WM_ACTIVATE,
                Some(WPARAM(WA_INACTIVE as usize)),
                Some(LPARAM(0)),
            );
            assert!(IsWindowVisible(hwnd).as_bool());
            suppressed.set(false);
            SendMessageW(
                hwnd,
                WM_ACTIVATE,
                Some(WPARAM(WA_INACTIVE as usize)),
                Some(LPARAM(0)),
            );
            assert!(!IsWindowVisible(hwnd).as_bool());
            DestroyWindow(hwnd).unwrap();
        }
    }
}
