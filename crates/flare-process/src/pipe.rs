//! Readiness checks for captured child output without waiting for inherited writers.

#[cfg(unix)]
pub use std::os::fd::AsRawFd as PipeHandle;
#[cfg(windows)]
pub use std::os::windows::io::AsRawHandle as PipeHandle;

/// Bytes safe to read now, or `None` when the pipe would block.
/// Unix may report EOF as readable; Windows reports a broken pipe as an error.
#[cfg(windows)]
pub fn available_output_bytes<P: PipeHandle>(
    pipe: &P,
    maximum: usize,
) -> std::io::Result<Option<usize>> {
    let mut available = 0;
    // SAFETY: the caller owns this live pipe and is its only reader; no concurrent I/O
    // can consume the available bytes between this query and the following read.
    let ok = unsafe {
        windows_sys::Win32::System::Pipes::PeekNamedPipe(
            pipe.as_raw_handle(),
            std::ptr::null_mut(),
            0,
            std::ptr::null_mut(),
            &mut available,
            std::ptr::null_mut(),
        )
    };
    if ok == 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok((available > 0).then_some((available as usize).min(maximum)))
}

/// Bytes safe to read now, or `None` when the pipe would block.
/// EOF is readable and the subsequent read returns zero bytes.
#[cfg(unix)]
pub fn available_output_bytes<P: PipeHandle>(
    pipe: &P,
    maximum: usize,
) -> std::io::Result<Option<usize>> {
    let mut fd = libc::pollfd {
        fd: pipe.as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    };
    // SAFETY: poll borrows one valid descriptor and never outlives this stack value.
    let ready = unsafe { libc::poll(&mut fd, 1, 0) };
    if ready < 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok((ready > 0).then_some(maximum))
}

#[cfg(not(any(unix, windows)))]
pub trait PipeHandle {}
#[cfg(not(any(unix, windows)))]
impl<T> PipeHandle for T {}

#[cfg(not(any(unix, windows)))]
pub fn available_output_bytes<P>(_: &P, maximum: usize) -> std::io::Result<Option<usize>> {
    // shortcut: other platforms retain blocking reads; add readiness before supporting them.
    Ok(Some(maximum))
}
