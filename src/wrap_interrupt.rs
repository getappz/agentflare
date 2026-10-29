use std::io;

#[cfg(unix)]
pub(super) struct Guard(libc::sigaction);

#[cfg(unix)]
extern "C" fn handle(_: libc::c_int) {}

#[cfg(unix)]
impl Guard {
    pub(super) fn install() -> io::Result<Self> {
        // A caught disposition resets in the child on exec; SIG_IGN would be inherited.
        unsafe {
            let mut action: libc::sigaction = std::mem::zeroed();
            let mut previous = std::mem::zeroed();
            action.sa_sigaction = handle as *const () as usize;
            libc::sigemptyset(&mut action.sa_mask);
            if libc::sigaction(libc::SIGINT, &action, &mut previous) != 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(Self(previous))
        }
    }
}

#[cfg(unix)]
impl Drop for Guard {
    fn drop(&mut self) {
        unsafe { libc::sigaction(libc::SIGINT, &self.0, std::ptr::null_mut()) };
    }
}

#[cfg(windows)]
pub(super) struct Guard(bool);

#[cfg(windows)]
unsafe extern "system" fn handle(event: u32) -> i32 {
    i32::from(event == windows_sys::Win32::System::Console::CTRL_C_EVENT)
}

#[cfg(windows)]
impl Guard {
    pub(super) fn install() -> io::Result<Self> {
        use windows_sys::Win32::System::Console::{GetConsoleCP, SetConsoleCtrlHandler};
        // No console means there are no console Ctrl-C events to handle.
        unsafe {
            if GetConsoleCP() == 0 {
                return Ok(Self(false));
            }
            if SetConsoleCtrlHandler(Some(handle), 1) == 0 {
                return Err(io::Error::last_os_error());
            }
        }
        Ok(Self(true))
    }
}

#[cfg(windows)]
impl Drop for Guard {
    fn drop(&mut self) {
        if self.0 {
            unsafe {
                windows_sys::Win32::System::Console::SetConsoleCtrlHandler(Some(handle), 0);
            }
        }
    }
}
