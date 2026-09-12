use std::sync::{Arc, Mutex, OnceLock, mpsc::Sender};

use windows::{
    Win32::{
        Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM},
        System::LibraryLoader::GetModuleHandleW,
        UI::{
            HiDpi::{DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext},
            Shell::{
                ExtractIconExW, NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE,
                NIM_SETVERSION, NOTIFYICON_VERSION_4, NOTIFYICONDATAW, Shell_NotifyIconW,
            },
            WindowsAndMessaging::{
                AppendMenuW, CW_USEDEFAULT, CreatePopupMenu, CreateWindowExW, DefWindowProcW,
                DestroyIcon, DestroyMenu, DispatchMessageW, GetCursorPos, GetMessageW, HICON,
                HMENU, IDI_APPLICATION, LoadIconW, MF_CHECKED, MF_GRAYED, MF_SEPARATOR, MF_STRING,
                MSG, PostQuitMessage, RegisterClassW, RegisterWindowMessageW, SetForegroundWindow,
                TPM_RIGHTBUTTON, TrackPopupMenu, TranslateMessage, WINDOW_EX_STYLE, WM_APP,
                WM_COMMAND, WM_CONTEXTMENU, WM_DESTROY, WM_LBUTTONDBLCLK, WM_RBUTTONUP, WNDCLASSW,
                WS_OVERLAPPED,
            },
        },
    },
    core::{PCWSTR, w},
};

use crate::WorkerEvent;

const WM_TRAYICON: u32 = WM_APP + 1;
const ID_TOGGLE_ENABLED: usize = 1001;
const ID_STATUS: usize = 1002;
const ID_TEST: usize = 1003;
const ID_STARTUP: usize = 1004;
const ID_CONFIG: usize = 1005;
const ID_EXIT: usize = 1006;
const ID_FOREGROUND: usize = 1007;

#[derive(Debug, Clone)]
pub struct TrayStatus {
    pub enabled: bool,
    pub foreground_detection: bool,
    pub start_with_windows: bool,
    pub text: String,
}

struct TrayGlobals {
    tx: Sender<WorkerEvent>,
    status: Arc<Mutex<TrayStatus>>,
}

static GLOBALS: OnceLock<TrayGlobals> = OnceLock::new();

pub fn run(
    tx: Sender<WorkerEvent>,
    status: Arc<Mutex<TrayStatus>>,
) -> Result<(), Box<dyn std::error::Error>> {
    let _ = GLOBALS.set(TrayGlobals { tx, status });

    unsafe {
        // Opt into per-monitor DPI awareness before any HWND is created. Without this,
        // Windows bitmap-scales the native popup menu on high-DPI displays, which makes
        // the text look soft on 1440p/4K monitors.
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);

        let instance = GetModuleHandleW(None)?;
        let class_name = w!("DiscodexTrayWindow");
        let class = WNDCLASSW {
            hInstance: instance.into(),
            lpszClassName: class_name,
            lpfnWndProc: Some(window_proc),
            ..Default::default()
        };
        if RegisterClassW(&class) == 0 {
            return Err("failed to register tray window class".into());
        }

        let hwnd = CreateWindowExW(
            WINDOW_EX_STYLE::default(),
            class_name,
            w!("Discodex"),
            WS_OVERLAPPED,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            0,
            0,
            None,
            None,
            Some(instance.into()),
            None,
        )?;

        let (icon, destroy_icon) = load_app_icon()?;
        let mut nid = NOTIFYICONDATAW {
            cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
            hWnd: hwnd,
            uID: 1,
            uFlags: NIF_MESSAGE | NIF_ICON | NIF_TIP,
            uCallbackMessage: WM_TRAYICON,
            hIcon: icon,
            ..Default::default()
        };
        copy_wide(&mut nid.szTip, "Discodex — AI Discord Presence");
        add_tray_icon(&mut nid)?;

        // Explorer broadcasts this after it restarts. Notification icons are wiped when
        // Explorer exits, so re-add ours automatically instead of silently disappearing.
        let taskbar_created_message = RegisterWindowMessageW(w!("TaskbarCreated"));

        let mut message = MSG::default();
        while GetMessageW(&mut message, None, 0, 0).as_bool() {
            if taskbar_created_message != 0 && message.message == taskbar_created_message {
                let _ = add_tray_icon(&mut nid);
                continue;
            }
            let _ = TranslateMessage(&message);
            DispatchMessageW(&message);
        }
        let _ = Shell_NotifyIconW(NIM_DELETE, &nid);
        if destroy_icon {
            let _ = DestroyIcon(icon);
        }
    }
    Ok(())
}

fn add_tray_icon(nid: &mut NOTIFYICONDATAW) -> Result<(), Box<dyn std::error::Error>> {
    unsafe {
        if !Shell_NotifyIconW(NIM_ADD, nid).as_bool() {
            return Err("Explorer rejected the Discodex tray icon".into());
        }

        // Use current notification-area callback semantics on modern Windows.
        nid.Anonymous.uVersion = NOTIFYICON_VERSION_4;
        let _ = Shell_NotifyIconW(NIM_SETVERSION, nid);
    }
    Ok(())
}

fn load_app_icon() -> Result<(HICON, bool), Box<dyn std::error::Error>> {
    if let Ok(executable) = std::env::current_exe() {
        let path = to_wide(&executable.to_string_lossy());
        let mut small = HICON::default();
        let count = unsafe { ExtractIconExW(PCWSTR(path.as_ptr()), 0, None, Some(&mut small), 1) };
        if count > 0 && !small.is_invalid() {
            return Ok((small, true));
        }
    }

    Ok((unsafe { LoadIconW(None, IDI_APPLICATION)? }, false))
}

unsafe extern "system" fn window_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match message {
        WM_TRAYICON => {
            // With NOTIFYICON_VERSION_4, the notification code is stored in LOWORD(lParam).
            let event = (lparam.0 as u32) & 0xffff;
            if event == WM_RBUTTONUP || event == WM_CONTEXTMENU || event == WM_LBUTTONDBLCLK {
                unsafe { show_menu(hwnd) };
            }
            LRESULT(0)
        }
        WM_COMMAND => {
            let id = wparam.0 & 0xffff;
            if let Some(globals) = GLOBALS.get() {
                match id {
                    ID_TOGGLE_ENABLED => {
                        let _ = globals.tx.send(WorkerEvent::ToggleEnabled);
                    }
                    ID_TEST => {
                        let _ = globals.tx.send(WorkerEvent::TestPresence);
                    }
                    ID_FOREGROUND => {
                        let _ = globals.tx.send(WorkerEvent::ToggleForegroundDetection);
                    }
                    ID_STARTUP => {
                        let _ = globals.tx.send(WorkerEvent::ToggleStartup);
                    }
                    ID_CONFIG => {
                        let _ = globals.tx.send(WorkerEvent::OpenConfig);
                    }
                    ID_EXIT => {
                        let _ = globals.tx.send(WorkerEvent::Exit);
                        unsafe { PostQuitMessage(0) };
                    }
                    _ => {}
                }
            }
            LRESULT(0)
        }
        WM_DESTROY => {
            if let Some(globals) = GLOBALS.get() {
                let _ = globals.tx.send(WorkerEvent::Exit);
            }
            unsafe { PostQuitMessage(0) };
            LRESULT(0)
        }
        _ => unsafe { DefWindowProcW(hwnd, message, wparam, lparam) },
    }
}

unsafe fn show_menu(hwnd: HWND) {
    let Some(globals) = GLOBALS.get() else { return };
    let status = globals
        .status
        .lock()
        .ok()
        .map(|s| s.clone())
        .unwrap_or(TrayStatus {
            enabled: false,
            foreground_detection: false,
            start_with_windows: false,
            text: "Unavailable".to_string(),
        });
    let Ok(menu) = (unsafe { CreatePopupMenu() }) else {
        return;
    };
    unsafe {
        append_item(
            menu,
            ID_TOGGLE_ENABLED,
            if status.enabled {
                "Presence Enabled"
            } else {
                "Presence Disabled"
            },
            status.enabled,
            false,
        );
        append_item(
            menu,
            ID_STATUS,
            &format!("Status: {}", status.text),
            false,
            true,
        );
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
        append_item(menu, ID_TEST, "Test Presence", false, false);
        append_item(
            menu,
            ID_FOREGROUND,
            "Detect focused AI apps",
            status.foreground_detection,
            false,
        );
        append_item(
            menu,
            ID_STARTUP,
            "Start with Windows",
            status.start_with_windows,
            false,
        );
        append_item(menu, ID_CONFIG, "Open Config", false, false);
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
        append_item(menu, ID_EXIT, "Exit", false, false);
    }

    let mut point = POINT::default();
    if unsafe { GetCursorPos(&mut point) }.is_ok() {
        let _ = unsafe { SetForegroundWindow(hwnd) };
        let _ =
            unsafe { TrackPopupMenu(menu, TPM_RIGHTBUTTON, point.x, point.y, None, hwnd, None) };
    }
    let _ = unsafe { DestroyMenu(menu) };
}

unsafe fn append_item(menu: HMENU, id: usize, text: &str, checked: bool, disabled: bool) {
    let mut flags = MF_STRING;
    if checked {
        flags |= MF_CHECKED;
    }
    if disabled {
        flags |= MF_GRAYED;
    }
    let wide = to_wide(text);
    let _ = unsafe { AppendMenuW(menu, flags, id, PCWSTR(wide.as_ptr())) };
}

fn to_wide(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(std::iter::once(0)).collect()
}

fn copy_wide<const N: usize>(target: &mut [u16; N], value: &str) {
    let wide = to_wide(value);
    let count = wide.len().min(N);
    target[..count].copy_from_slice(&wide[..count]);
    if count == N {
        target[N - 1] = 0;
    }
}
