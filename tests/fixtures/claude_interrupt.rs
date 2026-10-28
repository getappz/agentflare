use std::sync::atomic::{AtomicBool, Ordering};

static INTERRUPTED: AtomicBool = AtomicBool::new(false);

#[cfg(unix)]
extern "C" fn interrupt(_: i32) {
    INTERRUPTED.store(true, Ordering::SeqCst);
}

#[cfg(unix)]
unsafe extern "C" {
    fn signal(number: i32, handler: usize) -> usize;
}

#[cfg(windows)]
unsafe extern "system" fn interrupt(event: u32) -> i32 {
    if event == 0 {
        INTERRUPTED.store(true, Ordering::SeqCst);
        1
    } else {
        0
    }
}

#[cfg(windows)]
#[link(name = "kernel32")]
unsafe extern "system" {
    fn SetConsoleCtrlHandler(handler: Option<unsafe extern "system" fn(u32) -> i32>, add: i32) -> i32;
}

fn main() {
    #[cfg(unix)]
    unsafe { signal(2, interrupt as *const () as usize); }
    #[cfg(windows)]
    unsafe { assert_ne!(SetConsoleCtrlHandler(Some(interrupt), 1), 0); }
    let ready = std::env::var_os("AF_INTERRUPT_READY").unwrap();
    let received = std::env::var_os("AF_INTERRUPT_RECEIVED").unwrap();
    let stop = std::env::var_os("AF_INTERRUPT_STOP").unwrap();
    let mut payload = format!("{}\n", std::process::id());
    for arg in std::env::args_os().skip(1) {
        for byte in arg.as_encoded_bytes() {
            payload.push_str(&format!("{byte:02x}"));
        }
        payload.push('\n');
    }
    std::fs::write(ready, payload).unwrap();
    while !std::path::Path::new(&stop).exists() {
        if INTERRUPTED.swap(false, Ordering::SeqCst) {
            std::fs::write(&received, "received").unwrap();
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    std::process::exit(7);
}
