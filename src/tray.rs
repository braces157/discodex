use std::{
    cell::RefCell,
    sync::{Arc, Mutex, OnceLock, mpsc::Sender},
};

use windows::{
    Win32::{
        Foundation::{
            CloseHandle, HANDLE, HWND, LPARAM, LRESULT, POINT, WAIT_FAILED, WAIT_OBJECT_0, WPARAM,
        },
        System::{
            LibraryLoader::GetModuleHandleW,
            Threading::{INFINITE, OpenProcess, PROCESS_SYNCHRONIZE, WaitForSingleObject},
        },
        UI::{
            HiDpi::{DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext},
            Shell::{
                ExtractIconExW, NIF_ICON, NIF_MESSAGE, NIF_SHOWTIP, NIF_TIP, NIM_ADD, NIM_DELETE,
                NIM_SETVERSION, NIN_SELECT, NOTIFYICON_VERSION_4, NOTIFYICONDATAW,
                Shell_NotifyIconW,
            },
            WindowsAndMessaging::{
                AppendMenuW, CW_USEDEFAULT, ChangeWindowMessageFilterEx, CreatePopupMenu,
                CreateWindowExW, DefWindowProcW, DestroyIcon, DestroyMenu, DestroyWindow,
                DispatchMessageW, FindWindowW, GetCursorPos, GetWindowThreadProcessId, HICON,
                HMENU, IDI_APPLICATION, KillTimer, LoadIconW, MF_CHECKED, MF_GRAYED, MF_SEPARATOR,
                MF_STRING, MSG, MSGFLT_ALLOW, MsgWaitForMultipleObjects, PM_REMOVE, PeekMessageW,
                PostMessageW, PostQuitMessage, QS_ALLINPUT, RegisterClassW, RegisterWindowMessageW,
                SetForegroundWindow, SetTimer, TPM_RIGHTBUTTON, TrackPopupMenu, TranslateMessage,
                WINDOW_EX_STYLE, WM_APP, WM_CLOSE, WM_COMMAND, WM_CONTEXTMENU, WM_DESTROY,
                WM_LBUTTONDBLCLK, WM_LBUTTONUP, WM_QUIT, WM_RBUTTONUP, WM_TIMER, WNDCLASSW,
                WS_OVERLAPPED,
            },
        },
    },
    core::{PCWSTR, w},
};

use crate::{WorkerEvent, config::PrivacySettings};

const TRAY_WINDOW_CLASS: PCWSTR = w!("DiscodexTrayWindow");
const TRAY_WINDOW_TITLE: PCWSTR = w!("Discodex");
const WM_TRAYICON: u32 = WM_APP + 1;
const KEYBOARD_SELECT: u32 = NIN_SELECT | 1;
const RETRY_TIMER_ID: usize = 1;
const RETRY_TIMER_MS: u32 = 5_000;
const ID_TOGGLE_ENABLED: usize = 1001;
const ID_STATUS: usize = 1002;
const ID_TEST: usize = 1003;
const ID_STARTUP: usize = 1004;
const ID_CONFIG: usize = 1005;
const ID_EXIT: usize = 1006;
const ID_FOREGROUND: usize = 1007;
const ID_REASONING: usize = 1008;
const ID_PROJECT_NAMES: usize = 1009;
const ID_FILE_NAMES: usize = 1010;
const ID_DIAGNOSTICS: usize = 1011;
const ID_USAGE: usize = 1012;
const ID_RESTART: usize = 1013;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrayExitAction {
    Exit,
    Restart,
}

#[derive(Debug, Clone)]
pub struct TrayStatus {
    pub enabled: bool,
    pub foreground_detection: bool,
    pub start_with_windows: bool,
    pub text: String,
    pub privacy: PrivacySettings,
}

struct TrayGlobals {
    tx: Sender<WorkerEvent>,
    status: Arc<Mutex<TrayStatus>>,
}

static GLOBALS: OnceLock<TrayGlobals> = OnceLock::new();

#[derive(Clone, Copy)]
struct TrayWindowState {
    nid: NOTIFYICONDATAW,
    tray_added: bool,
    taskbar_created_message: u32,
    add_icon: fn(&mut NOTIFYICONDATAW) -> bool,
    delete_icon: Option<fn(&NOTIFYICONDATAW)>,
    exit_action: TrayExitAction,
}

fn delete_tray_icon(nid: &NOTIFYICONDATAW) {
    let _ = unsafe { Shell_NotifyIconW(NIM_DELETE, nid) };
}

thread_local! {
    static TRAY_WINDOW: RefCell<Option<TrayWindowState>> = const { RefCell::new(None) };
}

pub fn request_running_instance_exit(timeout: std::time::Duration) -> bool {
    if crate::instance::signal_quit_and_wait(timeout) {
        return true;
    }
    request_window_exit(TRAY_WINDOW_CLASS, TRAY_WINDOW_TITLE, timeout)
}

fn request_window_exit(class_name: PCWSTR, title: PCWSTR, timeout: std::time::Duration) -> bool {
    unsafe {
        let Ok(hwnd) = FindWindowW(class_name, title) else {
            return false;
        };
        if hwnd.is_invalid() {
            return false;
        }

        let mut pid = 0_u32;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        let process = if pid != 0 && pid != std::process::id() {
            OpenProcess(PROCESS_SYNCHRONIZE, false, pid).ok()
        } else {
            None
        };

        let posted = PostMessageW(Some(hwnd), WM_CLOSE, WPARAM(0), LPARAM(0)).is_ok();

        if let Some(handle) = process {
            let timeout_ms = timeout.as_millis().min(u32::MAX as u128) as u32;
            let wait_result = WaitForSingleObject(handle, timeout_ms);
            let _ = CloseHandle(handle);
            return posted && wait_result == WAIT_OBJECT_0;
        }

        posted
    }
}

unsafe fn run_message_loop(quit_event: HANDLE) {
    let handles = [quit_event];
    let mut message = MSG::default();
    loop {
        while unsafe { PeekMessageW(&mut message, None, 0, 0, PM_REMOVE) }.as_bool() {
            if message.message == WM_QUIT {
                return;
            }
            unsafe {
                let _ = TranslateMessage(&message);
                DispatchMessageW(&message);
            }
        }
        let wait =
            unsafe { MsgWaitForMultipleObjects(Some(&handles), false, INFINITE, QS_ALLINPUT) };
        if wait == WAIT_OBJECT_0 {
            if let Some(globals) = GLOBALS.get() {
                let _ = globals.tx.send(WorkerEvent::Exit);
            }
            return;
        }
        if wait == WAIT_FAILED {
            return;
        }
    }
}

pub fn run(
    tx: Sender<WorkerEvent>,
    status: Arc<Mutex<TrayStatus>>,
    quit_event: HANDLE,
) -> Result<TrayExitAction, Box<dyn std::error::Error>> {
    let _ = GLOBALS.set(TrayGlobals { tx, status });

    let exit_action = unsafe {
        // Opt into per-monitor DPI awareness before any HWND is created. Without this,
        // Windows bitmap-scales the native popup menu on high-DPI displays, which makes
        // the text look soft on 1440p/4K monitors.
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        let _ = windows::Win32::System::Com::CoInitializeEx(
            None,
            windows::Win32::System::Com::COINIT_APARTMENTTHREADED,
        );

        let instance = GetModuleHandleW(None)?;
        let class_name = TRAY_WINDOW_CLASS;
        let class = WNDCLASSW {
            hInstance: instance.into(),
            lpszClassName: class_name,
            lpfnWndProc: Some(window_proc),
            ..Default::default()
        };
        if RegisterClassW(&class) == 0 {
            return Err("failed to register tray window class".into());
        }

        // Explorer (and third-party tray hosts) broadcast this when the taskbar is
        // created or restarted. Register before CreateWindowExW and handle inside
        // window_proc because SendNotifyMessage/SendMessage broadcasts bypass GetMessageW.
        let taskbar_created_message = RegisterWindowMessageW(w!("TaskbarCreated"));

        let hwnd = CreateWindowExW(
            WINDOW_EX_STYLE::default(),
            class_name,
            TRAY_WINDOW_TITLE,
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
        if taskbar_created_message != 0 {
            let _ = ChangeWindowMessageFilterEx(hwnd, taskbar_created_message, MSGFLT_ALLOW, None);
        }

        let (icon, destroy_icon) = load_app_icon()?;
        let mut nid = NOTIFYICONDATAW {
            cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
            hWnd: hwnd,
            uID: 1,
            uFlags: NIF_MESSAGE | NIF_ICON | NIF_TIP | NIF_SHOWTIP,
            uCallbackMessage: WM_TRAYICON,
            hIcon: icon,
            ..Default::default()
        };
        copy_wide(&mut nid.szTip, "Discodex — AI Discord Presence");

        TRAY_WINDOW.with(|cell| {
            *cell.borrow_mut() = Some(TrayWindowState {
                nid,
                tray_added: false,
                taskbar_created_message,
                add_icon: add_tray_icon,
                delete_icon: Some(delete_tray_icon),
                exit_action: TrayExitAction::Exit,
            });
        });
        re_add_tray_icon(hwnd, false);

        run_message_loop(quit_event);

        let state = TRAY_WINDOW.with(|cell| cell.borrow_mut().take());
        let _ = KillTimer(Some(hwnd), RETRY_TIMER_ID);
        let exit_action = state.map_or(TrayExitAction::Exit, |state| {
            if state.tray_added
                && let Some(delete) = state.delete_icon
            {
                delete(&state.nid);
            }
            state.exit_action
        });
        let _ = DestroyWindow(hwnd);
        if destroy_icon {
            let _ = DestroyIcon(icon);
        }
        windows::Win32::System::Com::CoUninitialize();
        exit_action
    };
    Ok(exit_action)
}

fn re_add_tray_icon(hwnd: HWND, only_if_missing: bool) {
    let target = TRAY_WINDOW.with(|cell| {
        let borrow = cell.borrow();
        let state = (*borrow)?;
        if state.nid.hWnd != hwnd || (only_if_missing && state.tray_added) {
            return None;
        }
        Some((state.nid, state.add_icon))
    });
    let Some((mut nid, add_icon)) = target else {
        return;
    };

    // Drop the RefCell borrow before invoking Shell/Timer APIs because they can
    // pump sent messages and re-enter window_proc on the same thread.
    let added = add_icon(&mut nid);
    TRAY_WINDOW.with(|cell| {
        if let Some(state) = cell.borrow_mut().as_mut()
            && state.nid.hWnd == hwnd
        {
            state.nid = nid;
            state.tray_added = added;
        }
    });

    unsafe {
        if added {
            let _ = KillTimer(Some(hwnd), RETRY_TIMER_ID);
        } else {
            let _ = SetTimer(Some(hwnd), RETRY_TIMER_ID, RETRY_TIMER_MS, None);
        }
    }
}

fn add_tray_icon(nid: &mut NOTIFYICONDATAW) -> bool {
    unsafe {
        let _ = Shell_NotifyIconW(NIM_DELETE, nid);
        if !Shell_NotifyIconW(NIM_ADD, nid).as_bool() {
            return false;
        }

        nid.Anonymous.uVersion = NOTIFYICON_VERSION_4;
        let _ = Shell_NotifyIconW(NIM_SETVERSION, nid);
        true
    }
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
    let is_taskbar_created = TRAY_WINDOW.with(|cell| {
        cell.borrow().is_some_and(|state| {
            state.nid.hWnd == hwnd
                && state.taskbar_created_message != 0
                && message == state.taskbar_created_message
        })
    });
    if is_taskbar_created {
        re_add_tray_icon(hwnd, false);
        return LRESULT(0);
    }

    match message {
        WM_TIMER if wparam == WPARAM(RETRY_TIMER_ID) => {
            re_add_tray_icon(hwnd, true);
            LRESULT(0)
        }
        WM_TRAYICON => {
            // With NOTIFYICON_VERSION_4, the notification code is stored in LOWORD(lParam).
            let event = (lparam.0 as u32) & 0xffff;
            if matches!(
                event,
                WM_LBUTTONUP | WM_LBUTTONDBLCLK | NIN_SELECT | KEYBOARD_SELECT
            ) {
                if let Err(error) = crate::dashboard::show() {
                    crate::log_line(&format!("Could not open usage popup: {error}"));
                }
            } else if event == WM_RBUTTONUP || event == WM_CONTEXTMENU {
                unsafe { show_menu(hwnd) };
            }
            LRESULT(0)
        }
        WM_COMMAND => {
            let id = wparam.0 & 0xffff;
            if let Some(globals) = GLOBALS.get() {
                match id {
                    ID_USAGE => {
                        if let Err(error) = crate::dashboard::show() {
                            crate::log_line(&format!("Could not open usage dashboard: {error}"));
                        }
                    }
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
                    ID_REASONING => {
                        let _ = globals.tx.send(WorkerEvent::ToggleReasoning);
                    }
                    ID_PROJECT_NAMES => {
                        let _ = globals.tx.send(WorkerEvent::ToggleProjectNames);
                    }
                    ID_FILE_NAMES => {
                        let _ = globals.tx.send(WorkerEvent::ToggleFileNames);
                    }
                    ID_DIAGNOSTICS => {
                        let _ = globals.tx.send(WorkerEvent::OpenDiagnostics);
                    }
                    ID_RESTART => {
                        TRAY_WINDOW.with(|cell| {
                            if let Some(state) = cell.borrow_mut().as_mut() {
                                state.exit_action = TrayExitAction::Restart;
                            }
                        });
                        let _ = globals.tx.send(WorkerEvent::Exit);
                        unsafe { PostQuitMessage(0) };
                    }
                    ID_EXIT => {
                        TRAY_WINDOW.with(|cell| {
                            if let Some(state) = cell.borrow_mut().as_mut() {
                                state.exit_action = TrayExitAction::Exit;
                            }
                        });
                        let _ = globals.tx.send(WorkerEvent::Exit);
                        unsafe { PostQuitMessage(0) };
                    }
                    _ => {}
                }
            } else if id == ID_RESTART || id == ID_EXIT {
                TRAY_WINDOW.with(|cell| {
                    if let Some(state) = cell.borrow_mut().as_mut() {
                        state.exit_action = if id == ID_RESTART {
                            TrayExitAction::Restart
                        } else {
                            TrayExitAction::Exit
                        };
                    }
                });
                unsafe { PostQuitMessage(0) };
            }
            LRESULT(0)
        }
        WM_DESTROY => {
            let delete_target = TRAY_WINDOW.with(|cell| {
                let mut borrow = cell.borrow_mut();
                let state = borrow.as_mut()?;
                if state.nid.hWnd != hwnd {
                    return None;
                }
                let delete = state.delete_icon.take()?;
                state.tray_added.then_some((delete, state.nid))
            });
            if let Some((delete, nid)) = delete_target {
                delete(&nid);
            }
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
            privacy: PrivacySettings::default(),
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
        append_item(menu, ID_USAGE, "Usage Dashboard", false, false);
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
        append_item(
            menu,
            ID_REASONING,
            "Share reasoning (may contain private text)",
            status.privacy.share_reasoning,
            false,
        );
        append_item(
            menu,
            ID_PROJECT_NAMES,
            "Show project names",
            status.privacy.show_project_names,
            false,
        );
        append_item(
            menu,
            ID_FILE_NAMES,
            "Show file names",
            status.privacy.show_file_names,
            false,
        );
        append_item(menu, ID_DIAGNOSTICS, "Diagnostics", false, false);
        append_item(menu, ID_CONFIG, "Open Config", false, false);
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
        append_item(menu, ID_RESTART, "Restart", false, false);
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

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use windows::Win32::{
        System::Threading::{CreateEventW, SetEvent},
        UI::WindowsAndMessaging::{GetMessageW, SendNotifyMessageW, UnregisterClassW},
    };

    use super::*;

    static NEXT_CLASS_ID: AtomicUsize = AtomicUsize::new(1);

    fn run_tray_window_test(
        initial_tray_added: bool,
        mock_add_icon: fn(&mut NOTIFYICONDATAW) -> bool,
        send_messages: impl FnOnce(HWND, u32) + Send + 'static,
    ) -> bool {
        std::thread::spawn(move || {
            let instance = unsafe { GetModuleHandleW(None) }.unwrap();
            let class_id = NEXT_CLASS_ID.fetch_add(1, Ordering::Relaxed);
            let class_wide = to_wide(&format!("DiscodexTrayTestWindow_{class_id}"));
            let class_name = PCWSTR(class_wide.as_ptr());
            let class = WNDCLASSW {
                hInstance: instance.into(),
                lpszClassName: class_name,
                lpfnWndProc: Some(window_proc),
                ..Default::default()
            };
            assert_ne!(unsafe { RegisterClassW(&class) }, 0);

            let taskbar_created_message = unsafe { RegisterWindowMessageW(w!("TaskbarCreated")) };
            assert_ne!(taskbar_created_message, 0);

            let hwnd = unsafe {
                CreateWindowExW(
                    WINDOW_EX_STYLE::default(),
                    class_name,
                    w!("Discodex Test"),
                    WS_OVERLAPPED,
                    CW_USEDEFAULT,
                    CW_USEDEFAULT,
                    0,
                    0,
                    None,
                    None,
                    Some(instance.into()),
                    None,
                )
            }
            .unwrap();

            let nid = NOTIFYICONDATAW {
                cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
                hWnd: hwnd,
                uID: 1,
                ..Default::default()
            };
            TRAY_WINDOW.with(|cell| {
                *cell.borrow_mut() = Some(TrayWindowState {
                    nid,
                    tray_added: initial_tray_added,
                    taskbar_created_message,
                    add_icon: mock_add_icon,
                    delete_icon: None,
                    exit_action: TrayExitAction::Exit,
                });
            });

            let hwnd_raw = hwnd.0 as usize;
            let sender = std::thread::spawn(move || {
                let target = HWND(hwnd_raw as *mut std::ffi::c_void);
                send_messages(target, taskbar_created_message);
                unsafe { PostMessageW(Some(target), WM_CLOSE, WPARAM(0), LPARAM(0)) }.unwrap();
            });

            let mut message = MSG::default();
            while unsafe { GetMessageW(&mut message, None, 0, 0) }.as_bool() {
                unsafe {
                    let _ = TranslateMessage(&message);
                    DispatchMessageW(&message);
                }
            }
            sender.join().unwrap();

            let final_added = TRAY_WINDOW
                .with(|cell| cell.borrow_mut().take())
                .is_some_and(|state| state.tray_added);
            unsafe {
                let _ = KillTimer(Some(hwnd), RETRY_TIMER_ID);
                let _ = UnregisterClassW(class_name, Some(instance.into()));
            }
            final_added
        })
        .join()
        .unwrap()
    }

    #[test]
    fn taskbar_created_via_send_notify_message_readds_icon() {
        static CALLS: AtomicUsize = AtomicUsize::new(0);
        fn mock_add_icon(_: &mut NOTIFYICONDATAW) -> bool {
            CALLS.fetch_add(1, Ordering::SeqCst);
            true
        }

        CALLS.store(0, Ordering::SeqCst);
        let final_added = run_tray_window_test(true, mock_add_icon, |hwnd, taskbar_msg| {
            unsafe { SendNotifyMessageW(hwnd, taskbar_msg, WPARAM(0), LPARAM(0)) }.unwrap();
        });

        assert_eq!(CALLS.load(Ordering::SeqCst), 1);
        assert!(final_added);
    }

    #[test]
    fn taskbar_created_failure_retries_on_timer_and_ignores_unrelated_timers() {
        static CALLS: AtomicUsize = AtomicUsize::new(0);
        fn mock_add_icon(_: &mut NOTIFYICONDATAW) -> bool {
            CALLS.fetch_add(1, Ordering::SeqCst) > 0
        }

        CALLS.store(0, Ordering::SeqCst);
        let final_added = run_tray_window_test(true, mock_add_icon, |hwnd, taskbar_msg| unsafe {
            SendNotifyMessageW(hwnd, taskbar_msg, WPARAM(0), LPARAM(0)).unwrap();
            PostMessageW(Some(hwnd), WM_TIMER, WPARAM(99), LPARAM(0)).unwrap();
            PostMessageW(Some(hwnd), WM_TIMER, WPARAM(RETRY_TIMER_ID), LPARAM(0)).unwrap();
            PostMessageW(Some(hwnd), WM_TIMER, WPARAM(RETRY_TIMER_ID), LPARAM(0)).unwrap();
        });

        assert_eq!(CALLS.load(Ordering::SeqCst), 2);
        assert!(final_added);
    }

    #[test]
    fn request_window_exit_closes_matching_window() {
        let class_id = NEXT_CLASS_ID.fetch_add(1, Ordering::Relaxed);
        let class_text = format!("DiscodexTrayExitTest_{class_id}_{}", std::process::id());
        let title_text = format!("Discodex Exit Test {class_id}");
        let (ready_tx, ready_rx) = std::sync::mpsc::channel();

        let class_text_thread = class_text.clone();
        let title_text_thread = title_text.clone();
        let handle = std::thread::spawn(move || {
            let instance = unsafe { GetModuleHandleW(None) }.unwrap();
            let class_wide = to_wide(&class_text_thread);
            let title_wide = to_wide(&title_text_thread);
            let class_name = PCWSTR(class_wide.as_ptr());
            let title = PCWSTR(title_wide.as_ptr());
            let class = WNDCLASSW {
                hInstance: instance.into(),
                lpszClassName: class_name,
                lpfnWndProc: Some(window_proc),
                ..Default::default()
            };
            assert_ne!(unsafe { RegisterClassW(&class) }, 0);
            let hwnd = unsafe {
                CreateWindowExW(
                    WINDOW_EX_STYLE::default(),
                    class_name,
                    title,
                    WS_OVERLAPPED,
                    CW_USEDEFAULT,
                    CW_USEDEFAULT,
                    0,
                    0,
                    None,
                    None,
                    Some(instance.into()),
                    None,
                )
            }
            .unwrap();
            ready_tx.send(()).unwrap();

            let mut message = MSG::default();
            while unsafe { GetMessageW(&mut message, None, 0, 0) }.as_bool() {
                unsafe {
                    let _ = TranslateMessage(&message);
                    DispatchMessageW(&message);
                }
            }
            unsafe {
                let _ = KillTimer(Some(hwnd), RETRY_TIMER_ID);
                let _ = UnregisterClassW(class_name, Some(instance.into()));
            }
        });

        ready_rx.recv().unwrap();
        let class_wide = to_wide(&class_text);
        let title_wide = to_wide(&title_text);
        let closed = request_window_exit(
            PCWSTR(class_wide.as_ptr()),
            PCWSTR(title_wide.as_ptr()),
            std::time::Duration::from_secs(2),
        );
        assert!(closed);
        handle.join().unwrap();
    }

    #[test]
    fn restart_command_sets_restart_exit_action() {
        let exit_action = std::thread::spawn(move || {
            let instance = unsafe { GetModuleHandleW(None) }.unwrap();
            let class_id = NEXT_CLASS_ID.fetch_add(1, Ordering::Relaxed);
            let class_wide = to_wide(&format!("DiscodexTrayRestartTest_{class_id}"));
            let class_name = PCWSTR(class_wide.as_ptr());
            let class = WNDCLASSW {
                hInstance: instance.into(),
                lpszClassName: class_name,
                lpfnWndProc: Some(window_proc),
                ..Default::default()
            };
            assert_ne!(unsafe { RegisterClassW(&class) }, 0);

            let hwnd = unsafe {
                CreateWindowExW(
                    WINDOW_EX_STYLE::default(),
                    class_name,
                    w!("Discodex Restart Test"),
                    WS_OVERLAPPED,
                    CW_USEDEFAULT,
                    CW_USEDEFAULT,
                    0,
                    0,
                    None,
                    None,
                    Some(instance.into()),
                    None,
                )
            }
            .unwrap();

            let nid = NOTIFYICONDATAW {
                cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
                hWnd: hwnd,
                uID: 1,
                ..Default::default()
            };
            TRAY_WINDOW.with(|cell| {
                *cell.borrow_mut() = Some(TrayWindowState {
                    nid,
                    tray_added: false,
                    taskbar_created_message: 0,
                    add_icon: |_| true,
                    delete_icon: None,
                    exit_action: TrayExitAction::Exit,
                });
            });

            unsafe { PostMessageW(Some(hwnd), WM_COMMAND, WPARAM(ID_RESTART), LPARAM(0)) }.unwrap();

            let mut message = MSG::default();
            while unsafe { GetMessageW(&mut message, None, 0, 0) }.as_bool() {
                unsafe {
                    let _ = TranslateMessage(&message);
                    DispatchMessageW(&message);
                }
            }

            let action = TRAY_WINDOW
                .with(|cell| cell.borrow_mut().take())
                .map_or(TrayExitAction::Exit, |state| state.exit_action);
            unsafe {
                let _ = DestroyWindow(hwnd);
                let _ = UnregisterClassW(class_name, Some(instance.into()));
            }
            action
        })
        .join()
        .unwrap();

        assert_eq!(exit_action, TrayExitAction::Restart);
    }

    #[test]
    fn quit_event_exits_message_loop_cleanly() {
        let quit_event = unsafe { CreateEventW(None, true, false, PCWSTR::null()) }.unwrap();
        let quit_raw = quit_event.0 as usize;
        let (ready_tx, ready_rx) = std::sync::mpsc::channel();

        let loop_thread = std::thread::spawn(move || {
            let ev = HANDLE(quit_raw as *mut std::ffi::c_void);
            ready_tx.send(()).unwrap();
            unsafe { run_message_loop(ev) };
        });

        ready_rx.recv().unwrap();
        unsafe { SetEvent(quit_event) }.unwrap();
        loop_thread.join().unwrap();
        unsafe {
            let _ = CloseHandle(quit_event);
        }
    }
}
