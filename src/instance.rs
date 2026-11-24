use std::{path::PathBuf, time::Duration};

use windows::{
    Win32::{
        Foundation::{CloseHandle, ERROR_ALREADY_EXISTS, GetLastError, HANDLE, WAIT_OBJECT_0},
        System::{
            RemoteDesktop::ProcessIdToSessionId,
            Threading::{
                CreateEventW, CreateMutexW, EVENT_MODIFY_STATE, OpenEventW, OpenMutexW,
                OpenProcess, PROCESS_SYNCHRONIZE, SYNCHRONIZATION_SYNCHRONIZE, SetEvent,
                WaitForSingleObject,
            },
        },
    },
    core::{PCWSTR, w},
};

const SINGLE_INSTANCE_MUTEX: PCWSTR = w!("Local\\Discodex.SingleInstance");

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuitOutcome {
    NotRunning,
    Stopped,
    TimedOut,
}

pub fn current_session_id() -> u32 {
    let mut session_id = 0_u32;
    let ok = unsafe { ProcessIdToSessionId(std::process::id(), &mut session_id) }.is_ok();
    if ok { session_id } else { 0 }
}

pub fn tray_pid_file_path() -> PathBuf {
    let base = std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    base.join("Discodex")
        .join(format!("tray-session-{}.pid", current_session_id()))
}

fn to_wide(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(std::iter::once(0)).collect()
}

fn quit_event_name_for_pid(pid: u32) -> Vec<u16> {
    to_wide(&format!("Local\\Discodex.QuitEvent.{pid}"))
}

pub fn tray_ready_event_name_for_pid(pid: u32) -> Vec<u16> {
    to_wide(&format!("Local\\Discodex.TrayReady.{pid}"))
}

pub struct SingleInstance {
    handle: HANDLE,
    quit_event: HANDLE,
    pid: u32,
    pid_file: PathBuf,
}

impl SingleInstance {
    pub fn acquire() -> Result<Option<Self>, Box<dyn std::error::Error>> {
        let pid = std::process::id();
        let event_wide = quit_event_name_for_pid(pid);
        Self::acquire_named(
            SINGLE_INSTANCE_MUTEX,
            PCWSTR(event_wide.as_ptr()),
            pid,
            tray_pid_file_path(),
        )
    }

    fn acquire_named(
        mutex_name: PCWSTR,
        event_name: PCWSTR,
        pid: u32,
        pid_file: PathBuf,
    ) -> Result<Option<Self>, Box<dyn std::error::Error>> {
        unsafe {
            // Create the fresh per-PID quit event before claiming the mutex so any
            // --quit issued during startup has an endpoint to signal without racing.
            let quit_event = CreateEventW(None, true, false, event_name)?;
            let handle = match CreateMutexW(None, false, mutex_name) {
                Ok(handle) => handle,
                Err(error) => {
                    let _ = CloseHandle(quit_event);
                    return Err(error.into());
                }
            };
            if GetLastError() == ERROR_ALREADY_EXISTS {
                let _ = CloseHandle(handle);
                let _ = CloseHandle(quit_event);
                return Ok(None);
            }
            if let Some(parent) = pid_file.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            let _ = std::fs::write(&pid_file, pid.to_string());
            Ok(Some(Self {
                handle,
                quit_event,
                pid,
                pid_file,
            }))
        }
    }

    pub fn quit_event_handle(&self) -> HANDLE {
        self.quit_event
    }
}

impl Drop for SingleInstance {
    fn drop(&mut self) {
        if let Ok(contents) = std::fs::read_to_string(&self.pid_file)
            && contents.trim().parse::<u32>() == Ok(self.pid)
        {
            let _ = std::fs::remove_file(&self.pid_file);
        }
        unsafe {
            let _ = CloseHandle(self.quit_event);
            let _ = CloseHandle(self.handle);
        }
    }
}

pub fn is_running() -> bool {
    is_running_named(SINGLE_INSTANCE_MUTEX)
}

fn is_running_named(mutex_name: PCWSTR) -> bool {
    unsafe {
        let Ok(handle) = OpenMutexW(SYNCHRONIZATION_SYNCHRONIZE, false, mutex_name) else {
            return false;
        };
        let _ = CloseHandle(handle);
        true
    }
}

pub fn read_tray_pid() -> Option<u32> {
    std::fs::read_to_string(tray_pid_file_path())
        .ok()
        .and_then(|s| s.trim().parse::<u32>().ok())
}

pub fn signal_pid_quit_and_wait(pid: u32, timeout: Duration) -> Option<bool> {
    if pid == 0 || pid == std::process::id() {
        return None;
    }
    let event_wide = quit_event_name_for_pid(pid);
    unsafe {
        let Ok(ev) = OpenEventW(EVENT_MODIFY_STATE, false, PCWSTR(event_wide.as_ptr())) else {
            return None;
        };
        let proc_handle = OpenProcess(PROCESS_SYNCHRONIZE, false, pid).ok();
        let signaled = SetEvent(ev).is_ok();
        let _ = CloseHandle(ev);
        if !signaled {
            if let Some(handle) = proc_handle {
                let _ = CloseHandle(handle);
            }
            return Some(false);
        }
        if let Some(handle) = proc_handle {
            let timeout_ms = timeout.as_millis().min(u32::MAX as u128) as u32;
            let wait = WaitForSingleObject(handle, timeout_ms);
            let _ = CloseHandle(handle);
            Some(wait == WAIT_OBJECT_0)
        } else {
            // Process exited between OpenEventW and OpenProcess.
            Some(true)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn per_pid_instance_tracks_pid_file_without_clobbering_newer_owner() {
        let pid = std::process::id();
        let root = std::env::temp_dir().join(format!("discodex-inst-test-{pid}"));
        let _ = std::fs::remove_dir_all(&root);
        let pid_file = root.join("tray.pid");
        let mutex_wide = to_wide(&format!("Local\\Discodex.TestMutex.{pid}.1"));
        let event_wide = to_wide(&format!("Local\\Discodex.TestQuit.{pid}.1"));
        let mutex_name = PCWSTR(mutex_wide.as_ptr());
        let event_name = PCWSTR(event_wide.as_ptr());

        assert!(!is_running_named(mutex_name));
        let guard = SingleInstance::acquire_named(mutex_name, event_name, pid, pid_file.clone())
            .unwrap()
            .unwrap();
        assert!(is_running_named(mutex_name));
        assert_eq!(std::fs::read_to_string(&pid_file).unwrap(), pid.to_string());
        assert!(
            SingleInstance::acquire_named(mutex_name, event_name, pid, pid_file.clone())
                .unwrap()
                .is_none()
        );

        drop(guard);
        assert!(!is_running_named(mutex_name));
        assert!(!pid_file.exists());

        let guard2 = SingleInstance::acquire_named(mutex_name, event_name, pid, pid_file.clone())
            .unwrap()
            .unwrap();
        std::fs::write(&pid_file, "999999").unwrap();
        drop(guard2);
        assert_eq!(std::fs::read_to_string(&pid_file).unwrap(), "999999");

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn startup_quit_signal_before_acquire_is_preserved() {
        let pid = std::process::id();
        let root = std::env::temp_dir().join(format!("discodex-inst-prequit-{pid}"));
        let _ = std::fs::remove_dir_all(&root);
        let pid_file = root.join("tray.pid");
        let mutex_wide = to_wide(&format!("Local\\Discodex.TestMutex.{pid}.2"));
        let event_wide = to_wide(&format!("Local\\Discodex.TestQuit.{pid}.2"));
        let mutex_name = PCWSTR(mutex_wide.as_ptr());
        let event_name = PCWSTR(event_wide.as_ptr());

        let pre_ev = unsafe { CreateEventW(None, true, false, event_name) }.unwrap();
        unsafe { SetEvent(pre_ev) }.unwrap();

        let guard = SingleInstance::acquire_named(mutex_name, event_name, pid, pid_file)
            .unwrap()
            .unwrap();
        assert_eq!(
            unsafe { WaitForSingleObject(guard.quit_event_handle(), 0) },
            WAIT_OBJECT_0
        );

        drop(guard);
        unsafe {
            let _ = CloseHandle(pre_ev);
        }
        let _ = std::fs::remove_dir_all(&root);
    }
}
