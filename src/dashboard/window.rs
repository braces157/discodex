use std::{
    io,
    os::windows::process::CommandExt,
    process::{Child, Command as ProcessCommand},
    sync::{Mutex, OnceLock},
};

use windows::{
    Win32::{
        Foundation::{CloseHandle, ERROR_ALREADY_EXISTS, GetLastError, HANDLE, HWND, POINT, RECT},
        Graphics::{
            Dwm::{
                DWM_WINDOW_CORNER_PREFERENCE, DWMWA_BORDER_COLOR, DWMWA_WINDOW_CORNER_PREFERENCE,
                DWMWCP_ROUND, DwmSetWindowAttribute,
            },
            Gdi::{
                GetMonitorInfoW, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromPoint,
                MonitorFromRect,
            },
        },
        System::Threading::CreateMutexW,
        UI::{
            HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI},
            WindowsAndMessaging::{
                FindWindowW, GetCursorPos, GetWindowRect, HWND_TOPMOST, SW_RESTORE, SWP_SHOWWINDOW,
                SetForegroundWindow, SetWindowPos, ShowWindow,
            },
        },
    },
    core::w,
};

pub fn show() -> io::Result<()> {
    crate::usage_cache::nudge_local_preloader();
    static CHILD: OnceLock<Mutex<Option<Child>>> = OnceLock::new();
    let mut child = CHILD
        .get_or_init(|| Mutex::new(None))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if focus_existing() {
        return Ok(());
    }
    if let Some(process) = child.as_mut()
        && process.try_wait()?.is_none()
    {
        return Ok(());
    }
    *child = Some(
        ProcessCommand::new(std::env::current_exe()?)
            .arg("--usage")
            .creation_flags(0x08000000)
            .spawn()?,
    );
    Ok(())
}

pub fn focus_existing() -> bool {
    // SAFETY: Win32 window lookup and foreground focus on a null-terminated literal title.
    unsafe {
        if let Ok(hwnd) = FindWindowW(None, w!("Discodex Usage")) {
            let _ = ShowWindow(hwnd, SW_RESTORE);
            let _ = SetForegroundWindow(hwnd);
            return true;
        }
    }
    false
}

unsafe fn apply_rounded_corners(hwnd: HWND) {
    let pref: DWM_WINDOW_CORNER_PREFERENCE = DWMWCP_ROUND;
    let border_color: u32 = 0x002F2D2B; // COLORREF 0x00BBGGRR matching BORDER_OUTER over BG_CANVAS
    // SAFETY: Caller passes a valid HWND; older Windows 10 builds cleanly return an ignored Err.
    unsafe {
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_WINDOW_CORNER_PREFERENCE,
            (&pref as *const DWM_WINDOW_CORNER_PREFERENCE).cast(),
            std::mem::size_of::<DWM_WINDOW_CORNER_PREFERENCE>() as u32,
        );
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_BORDER_COLOR,
            (&border_color as *const u32).cast(),
            std::mem::size_of::<u32>() as u32,
        );
    }
}

pub struct UsageMutex(HANDLE);

impl Drop for UsageMutex {
    fn drop(&mut self) {
        // SAFETY: `self.0` is a valid mutex handle owned by this guard.
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

pub fn acquire_usage_mutex() -> windows::core::Result<Option<UsageMutex>> {
    // SAFETY: `CreateMutexW` takes a static null-terminated UTF-16 name and returns an owned handle.
    unsafe {
        let handle = CreateMutexW(None, false, w!("Local\\Discodex.Usage.Popup"))?;
        let guard = UsageMutex(handle);
        if GetLastError() == ERROR_ALREADY_EXISTS {
            focus_existing();
            return Ok(None);
        }
        Ok(Some(guard))
    }
}

pub struct Placement {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
    pub scale: f32,
}

impl Placement {
    /// Computes initial window placement clamped to monitor work area (centered in Wide mode,
    /// near the mouse cursor in Compact mode).
    pub fn initial_for(logical_w: f32, logical_h: f32, center: bool) -> Self {
        // SAFETY: Win32 monitor/DPI queries using initialized structs and stack output variables.
        unsafe {
            let mut cursor = POINT::default();
            let _ = GetCursorPos(&mut cursor);
            let monitor = MonitorFromPoint(cursor, MONITOR_DEFAULTTONEAREST);
            let mut info = MONITORINFO {
                cbSize: std::mem::size_of::<MONITORINFO>() as u32,
                ..Default::default()
            };
            if !GetMonitorInfoW(monitor, &mut info).as_bool() {
                return Self {
                    x: 40,
                    y: 40,
                    width: logical_w as i32,
                    height: logical_h as i32,
                    scale: 1.0,
                };
            }
            let (mut dx, mut dy) = (96, 96);
            let _ = GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut dx, &mut dy);
            let scale = dx.max(96) as f32 / 96.0;
            let work = info.rcWork;
            let margin = (10.0 * scale) as i32;
            let width =
                ((logical_w * scale) as i32).min((work.right - work.left - 2 * margin).max(1));
            let height =
                ((logical_h * scale) as i32).min((work.bottom - work.top - 2 * margin).max(1));
            let (raw_x, raw_y) = if center {
                (
                    work.left + ((work.right - work.left) - width) / 2,
                    work.top + ((work.bottom - work.top) - height) / 2,
                )
            } else {
                let mid_y = (work.top + work.bottom) / 2;
                (
                    cursor.x - width + (24.0 * scale) as i32,
                    if cursor.y < mid_y {
                        cursor.y + margin
                    } else {
                        cursor.y - height - margin
                    },
                )
            };
            let x = raw_x.clamp(
                work.left + margin,
                (work.right - width - margin).max(work.left + margin),
            );
            let y = raw_y.clamp(
                work.top + margin,
                (work.bottom - height - margin).max(work.top + margin),
            );
            Self {
                x,
                y,
                width,
                height,
                scale,
            }
        }
    }

    pub fn apply(&self) {
        // SAFETY: Repositions the existing Discodex Usage window using validated monitor coordinates.
        unsafe {
            if let Ok(hwnd) = FindWindowW(None, w!("Discodex Usage")) {
                apply_rounded_corners(hwnd);
                let _ = SetWindowPos(
                    hwnd,
                    Some(HWND_TOPMOST),
                    self.x,
                    self.y,
                    self.width,
                    self.height,
                    SWP_SHOWWINDOW,
                );
                let _ = SetForegroundWindow(hwnd);
            }
        }
    }

    /// Resizes the active popup via Win32 `SetWindowPos`, centering on the monitor work area when
    /// expanding to Wide mode and edge-clamping when returning to Compact mode.
    pub fn resize_active_window(logical_w: f32, logical_h: f32, center: bool) {
        // SAFETY: Queries current HWND rect/monitor info and resizes `Discodex Usage`.
        unsafe {
            let Ok(hwnd) = FindWindowW(None, w!("Discodex Usage")) else {
                return;
            };
            let mut rect = RECT::default();
            if GetWindowRect(hwnd, &mut rect).is_err() {
                return;
            }
            let monitor = MonitorFromRect(&rect, MONITOR_DEFAULTTONEAREST);
            let mut info = MONITORINFO {
                cbSize: std::mem::size_of::<MONITORINFO>() as u32,
                ..Default::default()
            };
            if !GetMonitorInfoW(monitor, &mut info).as_bool() {
                return;
            }
            let (mut dx, mut dy) = (96, 96);
            let _ = GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut dx, &mut dy);
            let scale = dx.max(96) as f32 / 96.0;
            let work = info.rcWork;
            let margin = (10.0 * scale) as i32;
            let width =
                ((logical_w * scale) as i32).min((work.right - work.left - 2 * margin).max(1));
            let height =
                ((logical_h * scale) as i32).min((work.bottom - work.top - 2 * margin).max(1));

            let max_x = (work.right - width - margin).max(work.left + margin);
            let max_y = (work.bottom - height - margin).max(work.top + margin);
            let (preferred_x, preferred_y) = if center {
                (
                    work.left + ((work.right - work.left) - width) / 2,
                    work.top + ((work.bottom - work.top) - height) / 2,
                )
            } else {
                (
                    if rect.left + width > work.right - margin {
                        rect.right - width
                    } else {
                        rect.left
                    },
                    if rect.top + height > work.bottom - margin {
                        rect.bottom - height
                    } else {
                        rect.top
                    },
                )
            };
            let x = preferred_x.clamp(work.left + margin, max_x);
            let y = preferred_y.clamp(work.top + margin, max_y);

            let _ = SetWindowPos(
                hwnd,
                Some(HWND_TOPMOST),
                x,
                y,
                width,
                height,
                SWP_SHOWWINDOW,
            );
        }
    }
}
