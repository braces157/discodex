use windows::{
    Win32::{
        Foundation::{CloseHandle, ERROR_ALREADY_EXISTS, GetLastError, HANDLE},
        System::Threading::CreateMutexW,
    },
    core::w,
};

pub struct SingleInstance {
    handle: HANDLE,
}

impl SingleInstance {
    pub fn acquire() -> Result<Option<Self>, Box<dyn std::error::Error>> {
        unsafe {
            let handle = CreateMutexW(None, false, w!("Local\\Discodex.SingleInstance"))?;
            if GetLastError() == ERROR_ALREADY_EXISTS {
                let _ = CloseHandle(handle);
                return Ok(None);
            }
            Ok(Some(Self { handle }))
        }
    }
}

impl Drop for SingleInstance {
    fn drop(&mut self) {
        let _ = unsafe { CloseHandle(self.handle) };
    }
}
