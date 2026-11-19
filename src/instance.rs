use std::time::{Duration, Instant};

use windows::{
    Win32::{
        Foundation::{CloseHandle, ERROR_ALREADY_EXISTS, GetLastError, HANDLE},
        System::Threading::{
            CreateEventW, CreateMutexW, EVENT_MODIFY_STATE, OpenEventW, OpenMutexW, ResetEvent,
            SYNCHRONIZATION_SYNCHRONIZE, SetEvent,
        },
    },
    core::{PCWSTR, w},
};

const SINGLE_INSTANCE_MUTEX: PCWSTR = w!("Local\\Discodex.SingleInstance");
const QUIT_EVENT_NAME: PCWSTR = w!("Local\\Discodex.QuitEvent");

pub struct SingleInstance {
    handle: HANDLE,
    quit_event: HANDLE,
}

impl SingleInstance {
    pub fn acquire() -> Result<Option<Self>, Box<dyn std::error::Error>> {
        Self::acquire_named(SINGLE_INSTANCE_MUTEX, QUIT_EVENT_NAME)
    }

    fn acquire_named(
        mutex_name: PCWSTR,
        event_name: PCWSTR,
    ) -> Result<Option<Self>, Box<dyn std::error::Error>> {
        unsafe {
            let handle = CreateMutexW(None, false, mutex_name)?;
            if GetLastError() == ERROR_ALREADY_EXISTS {
                let _ = CloseHandle(handle);
                return Ok(None);
            }
            let quit_event = match CreateEventW(None, true, false, event_name) {
                Ok(ev) => ev,
                Err(err) => {
                    let _ = CloseHandle(handle);
                    return Err(err.into());
                }
            };
            let _ = ResetEvent(quit_event);
            Ok(Some(Self { handle, quit_event }))
        }
    }

    pub fn quit_event_handle(&self) -> HANDLE {
        self.quit_event
    }
}

impl Drop for SingleInstance {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.quit_event);
            let _ = CloseHandle(self.handle);
        }
    }
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

pub fn signal_quit_and_wait(timeout: Duration) -> bool {
    signal_quit_and_wait_named(SINGLE_INSTANCE_MUTEX, QUIT_EVENT_NAME, timeout)
}

fn signal_quit_and_wait_named(mutex_name: PCWSTR, event_name: PCWSTR, timeout: Duration) -> bool {
    unsafe {
        let Ok(ev) = OpenEventW(EVENT_MODIFY_STATE, false, event_name) else {
            return false;
        };
        let signaled = SetEvent(ev).is_ok();
        let _ = CloseHandle(ev);
        if !signaled {
            return false;
        }
    }

    let deadline = Instant::now() + timeout;
    while is_running_named(mutex_name) {
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    true
}

#[cfg(test)]
mod tests {
    use windows::Win32::{Foundation::WAIT_OBJECT_0, System::Threading::WaitForSingleObject};

    use super::*;

    fn to_wide(value: &str) -> Vec<u16> {
        value.encode_utf16().chain(std::iter::once(0)).collect()
    }

    #[test]
    fn named_quit_event_signals_and_waits_for_instance_exit() {
        let pid = std::process::id();
        let mutex_wide = to_wide(&format!("Local\\Discodex.TestMutex.{pid}"));
        let event_wide = to_wide(&format!("Local\\Discodex.TestEvent.{pid}"));
        let mutex_name = PCWSTR(mutex_wide.as_ptr());
        let event_name = PCWSTR(event_wide.as_ptr());

        assert!(!is_running_named(mutex_name));
        let (ready_tx, ready_rx) = std::sync::mpsc::channel();
        let mutex_thread = mutex_wide.clone();
        let event_thread = event_wide.clone();
        let waiter = std::thread::spawn(move || {
            let m_ptr = PCWSTR(mutex_thread.as_ptr());
            let e_ptr = PCWSTR(event_thread.as_ptr());
            let guard = SingleInstance::acquire_named(m_ptr, e_ptr)
                .unwrap()
                .unwrap();
            assert!(is_running_named(m_ptr));
            assert!(
                SingleInstance::acquire_named(m_ptr, e_ptr)
                    .unwrap()
                    .is_none()
            );
            ready_tx.send(()).unwrap();
            assert_eq!(
                unsafe { WaitForSingleObject(guard.quit_event_handle(), 2_000) },
                WAIT_OBJECT_0
            );
        });

        ready_rx.recv().unwrap();
        assert!(signal_quit_and_wait_named(
            mutex_name,
            event_name,
            Duration::from_secs(2)
        ));
        waiter.join().unwrap();
        assert!(!is_running_named(mutex_name));
    }
}
