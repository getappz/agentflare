pub mod process;

#[allow(unsafe_code)]
pub enum DaemonAddr {
    #[cfg(unix)]
    UnixSocket(std::path::PathBuf),
    #[cfg(windows)]
    NamedPipe(String),
}

impl DaemonAddr {
    pub fn default_for_pid(pid: u32) -> Self {
        #[cfg(unix)]
        {
            let dir = dirs::runtime_dir()
                .map(|d| d.join("agentflare"))
                .unwrap_or_else(|| {
                    let mut p = std::env::temp_dir();
                    p.push("agentflare-daemon");
                    p
                });
            let _ = std::fs::create_dir_all(&dir);
            // Harden perms: dirs::runtime_dir() (XDG_RUNTIME_DIR) is already
            // 0700, but the temp_dir() fallback is world-writable — without
            // this, any local user who guesses the predictable socket path
            // could connect to the daemon and issue tool calls as this user.
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700));
            }
            DaemonAddr::UnixSocket(dir.join("daemon.sock"))
        }
        #[cfg(windows)]
        {
            DaemonAddr::NamedPipe(format!(r"\\.\pipe\agentflare-daemon-{pid}"))
        }
    }

    #[allow(dead_code)]
    pub fn default_path() -> Self {
        Self::default_for_pid(std::process::id())
    }

    #[allow(dead_code)]
    pub fn display(&self) -> String {
        match self {
            #[cfg(unix)]
            DaemonAddr::UnixSocket(p) => p.to_string_lossy().to_string(),
            #[cfg(windows)]
            DaemonAddr::NamedPipe(p) => p.clone(),
        }
    }
}

#[allow(unsafe_code)]
pub fn connect(addr: &DaemonAddr) -> Result<ConnStream, String> {
    match addr {
        #[cfg(unix)]
        DaemonAddr::UnixSocket(path) => {
            use std::os::unix::net::UnixStream;
            UnixStream::connect(path)
                .map(ConnStream::Unix)
                .map_err(|e| format!("connect to {}: {e}", path.display()))
        }
        #[cfg(windows)]
        DaemonAddr::NamedPipe(path) => {
            use std::os::windows::io::FromRawHandle;
            use windows_sys::Win32::Foundation::*;
            use windows_sys::Win32::Storage::FileSystem::CreateFileW;
            use windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_NORMAL;
            use windows_sys::Win32::Storage::FileSystem::FILE_SHARE_READ;
            use windows_sys::Win32::Storage::FileSystem::FILE_SHARE_WRITE;
            use windows_sys::Win32::Storage::FileSystem::OPEN_EXISTING;
            let wide: Vec<u16> = path.encode_utf16().chain(std::iter::once(0)).collect();
            let handle = unsafe {
                CreateFileW(
                    wide.as_ptr(),
                    GENERIC_READ | GENERIC_WRITE,
                    FILE_SHARE_READ | FILE_SHARE_WRITE,
                    std::ptr::null_mut(),
                    OPEN_EXISTING,
                    FILE_ATTRIBUTE_NORMAL,
                    std::ptr::null_mut(),
                )
            };
            if handle == INVALID_HANDLE_VALUE {
                return Err(format!("connect to named pipe {path}: {:#x}", unsafe {
                    GetLastError()
                }));
            }
            let file = unsafe { std::fs::File::from_raw_handle(handle as _) };
            Ok(ConnStream::WinPipe(file))
        }
    }
}

pub fn cleanup(addr: &DaemonAddr) {
    match addr {
        #[cfg(unix)]
        DaemonAddr::UnixSocket(path) => {
            let _ = std::fs::remove_file(path);
        }
        #[cfg(windows)]
        DaemonAddr::NamedPipe(_) => {}
    }
}

#[allow(unsafe_code)]
pub enum ConnStream {
    #[cfg(unix)]
    Unix(std::os::unix::net::UnixStream),
    #[cfg(windows)]
    WinPipe(std::fs::File),
}

impl ConnStream {
    pub fn write_all(&self, buf: &[u8]) -> Result<(), String> {
        use std::io::Write;
        match self {
            #[cfg(unix)]
            ConnStream::Unix(s) => {
                let mut s = s;
                s.write_all(buf).map_err(|e| format!("write: {e}"))
            }
            #[cfg(windows)]
            ConnStream::WinPipe(f) => {
                let mut f = f;
                f.write_all(buf).map_err(|e| format!("write: {e}"))
            }
        }
    }

    #[allow(dead_code)]
    pub fn read_all(&self) -> Result<Vec<u8>, String> {
        use std::io::Read;
        match self {
            #[cfg(unix)]
            ConnStream::Unix(s) => {
                let mut s = s;
                let mut buf = Vec::new();
                s.read_to_end(&mut buf).map_err(|e| format!("read: {e}"))?;
                Ok(buf)
            }
            #[cfg(windows)]
            ConnStream::WinPipe(f) => {
                let mut f = f;
                let mut buf = Vec::new();
                f.read_to_end(&mut buf).map_err(|e| format!("read: {e}"))?;
                Ok(buf)
            }
        }
    }
}
