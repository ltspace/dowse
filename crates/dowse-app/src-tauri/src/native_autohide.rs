//! WebView keyboard focus can leave during native move/resize while the top-level
//! window remains active. Only actual top-level deactivation should auto-hide.
//! This also avoids timer/JS mouseup races when Windows owns the modal drag loop.
use std::{
    rc::Rc,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};

use tauri::Manager;
use windows::Win32::{
    Foundation::{HWND, LPARAM, LRESULT, WPARAM},
    UI::{
        Shell::{DefSubclassProc, RemoveWindowSubclass, SetWindowSubclass},
        WindowsAndMessaging::{WA_INACTIVE, WM_ACTIVATE, WM_NCDESTROY},
    },
};

use crate::autohide::AutoHideSuppressor;

const SUBCLASS_ID: usize = 0x44575345;
struct Context {
    suppressed: Box<dyn Fn() -> bool>,
    hide: Rc<dyn Fn()>,
    activation_changed: Rc<dyn Fn()>,
}

/// Called from Tauri setup on the window's owning UI thread.
pub fn install(window: &tauri::WebviewWindow) -> anyhow::Result<()> {
    let hwnd = HWND(window.hwnd()?.0);
    let app = window.app_handle().clone();
    let hide_app = app.clone();
    let label = window.label().to_owned();
    let generation = Arc::new(AtomicU64::new(0));
    let activation_generation = generation.clone();
    install_native(
        hwnd,
        Box::new(move || app.state::<AutoHideSuppressor>().is_suppressed()),
        Rc::new(move || {
            // Native activation may occur while Tao holds its window state lock.
            // Queue a task from a worker: run_on_main_thread executes inline when
            // called directly here and would re-enter that lock.
            let app = hide_app.clone();
            let label = label.clone();
            let generation = generation.clone();
            let ticket = generation.load(Ordering::SeqCst);
            tauri::async_runtime::spawn(async move {
                let task_app = app.clone();
                if let Err(err) = app.run_on_main_thread(move || {
                    // A newer activation invalidates an old queued hide request.
                    if generation.load(Ordering::SeqCst) != ticket
                        || task_app.state::<AutoHideSuppressor>().is_suppressed()
                    {
                        return;
                    }
                    if let Some(window) = task_app.get_webview_window(&label)
                        && let Err(err) = window.hide()
                    {
                        crate::logging::log_line("window", &format!("auto-hide failed: {err}"));
                    }
                }) {
                    crate::logging::log_line("window", &format!("queue auto-hide failed: {err}"));
                }
            });
        }),
        Rc::new(move || {
            activation_generation.fetch_add(1, Ordering::SeqCst);
        }),
    )
}

fn install_native(
    hwnd: HWND,
    suppressed: Box<dyn Fn() -> bool>,
    hide: Rc<dyn Fn()>,
    activation_changed: Rc<dyn Fn()>,
) -> anyhow::Result<()> {
    let context = Box::into_raw(Box::new(Context {
        suppressed,
        hide,
        activation_changed,
    }));
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
    } else if message == WM_ACTIVATE {
        // Release the context borrow before invoking framework code, which may
        // re-enter this wndproc (including destruction of the subclass context).
        let (activation_changed, hide) = unsafe {
            let context = &*(data as *const Context);
            (
                context.activation_changed.clone(),
                ((wparam.0 & 0xffff) == WA_INACTIVE as usize && !(context.suppressed)())
                    .then(|| context.hide.clone()),
            )
        };
        activation_changed();
        if let Some(hide) = hide {
            hide();
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
            let hide_count = Rc::new(Cell::new(0));
            let calls = hide_count.clone();
            install_native(
                hwnd,
                Box::new(move || flag.get()),
                Rc::new(move || {
                    calls.set(calls.get() + 1);
                    let _ = ShowWindow(hwnd, SW_HIDE);
                }),
                Rc::new(|| {}),
            )
            .unwrap();
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
            assert_eq!(hide_count.get(), 1);

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
            assert_eq!(hide_count.get(), 1);
            suppressed.set(false);
            SendMessageW(
                hwnd,
                WM_ACTIVATE,
                Some(WPARAM(WA_INACTIVE as usize)),
                Some(LPARAM(0)),
            );
            assert!(!IsWindowVisible(hwnd).as_bool());
            assert_eq!(hide_count.get(), 2);
            DestroyWindow(hwnd).unwrap();
        }
    }
}
