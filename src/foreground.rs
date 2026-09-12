use std::path::Path;

use windows::{
    Win32::{
        Foundation::CloseHandle,
        System::Threading::{
            OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
            QueryFullProcessImageNameW,
        },
        UI::WindowsAndMessaging::{GetForegroundWindow, GetWindowTextW, GetWindowThreadProcessId},
    },
    core::PWSTR,
};

use crate::provider::Provider;

pub fn active_provider() -> Option<Provider> {
    let window = unsafe { GetForegroundWindow() };
    if window.is_invalid() {
        return None;
    }
    let executable = foreground_executable(window)?;
    let title = foreground_title(window);
    Provider::from_window(&executable, &title)
}

fn foreground_executable(window: windows::Win32::Foundation::HWND) -> Option<String> {
    unsafe {
        let mut process_id = 0_u32;
        GetWindowThreadProcessId(window, Some(&mut process_id));
        if process_id == 0 {
            return None;
        }

        let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, process_id).ok()?;
        let mut buffer = vec![0_u16; 32_768];
        let mut length = buffer.len() as u32;
        let result = QueryFullProcessImageNameW(
            process,
            PROCESS_NAME_WIN32,
            PWSTR(buffer.as_mut_ptr()),
            &mut length,
        );
        let _ = CloseHandle(process);
        result.ok()?;

        let path = String::from_utf16_lossy(&buffer[..length as usize]);
        Path::new(&path)
            .file_name()
            .and_then(|name| name.to_str())
            .map(str::to_string)
    }
}

fn foreground_title(window: windows::Win32::Foundation::HWND) -> String {
    let mut buffer = vec![0_u16; 1024];
    let length = unsafe { GetWindowTextW(window, &mut buffer) };
    if length <= 0 {
        String::new()
    } else {
        String::from_utf16_lossy(&buffer[..length as usize])
    }
}
