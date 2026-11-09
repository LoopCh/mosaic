//! Single instance: a second launch asks the running Mosaic to show its
//! window and exits. Windows: a named mutex detects the running instance, a
//! named auto-reset event carries the "show yourself" request. Unix: a socket.

#[cfg(windows)]
mod imp {
    use std::ffi::c_void;

    const ERROR_ALREADY_EXISTS: u32 = 183;
    const EVENT_MODIFY_STATE: u32 = 0x0002;
    const INFINITE: u32 = 0xFFFF_FFFF;
    const WAIT_OBJECT_0: u32 = 0;

    #[link(name = "kernel32")]
    extern "system" {
        fn CreateMutexW(attrs: *const c_void, initial_owner: i32, name: *const u16) -> isize;
        fn CreateEventW(attrs: *const c_void, manual_reset: i32, initial_state: i32, name: *const u16) -> isize;
        fn OpenEventW(access: u32, inherit: i32, name: *const u16) -> isize;
        fn SetEvent(h: isize) -> i32;
        fn WaitForSingleObject(h: isize, ms: u32) -> u32;
        fn CloseHandle(h: isize) -> i32;
        fn GetLastError() -> u32;
    }

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(Some(0)).collect()
    }

    // Per-user names: two users on one PC can each run Mosaic.
    const MUTEX: &str = "Local\\Mosaic.SingleInstance";
    const EVENT: &str = "Local\\Mosaic.ShowWindow";

    pub struct Guard {
        _mutex: isize,
        event: isize,
    }

    // Plain kernel handles, used only from the waiting thread.
    unsafe impl Send for Guard {}

    /// Some(guard) for the first instance; None if another one is running
    /// (it has been asked to show its window).
    pub fn acquire() -> Option<Guard> {
        unsafe {
            let m = CreateMutexW(std::ptr::null(), 0, wide(MUTEX).as_ptr());
            let already = GetLastError() == ERROR_ALREADY_EXISTS;
            if already {
                let e = OpenEventW(EVENT_MODIFY_STATE, 0, wide(EVENT).as_ptr());
                if e != 0 {
                    SetEvent(e);
                    CloseHandle(e);
                }
                if m != 0 {
                    CloseHandle(m);
                }
                return None;
            }
            let event = CreateEventW(std::ptr::null(), 0, 0, wide(EVENT).as_ptr());
            Some(Guard { _mutex: m, event })
        }
    }

    /// Call `on_show` every time another launch asks us to show the window.
    pub fn listen(guard: Guard, on_show: impl Fn() + Send + 'static) {
        std::thread::spawn(move || loop {
            let rc = unsafe { WaitForSingleObject(guard.event, INFINITE) };
            if rc != WAIT_OBJECT_0 {
                break;
            }
            on_show();
        });
    }
}

/// Linux / macOS: a Unix socket in the user's runtime folder; connecting to it
/// means "show yourself".
#[cfg(unix)]
mod imp {
    use std::io::Write;
    use std::os::unix::net::{UnixListener, UnixStream};
    use std::path::PathBuf;

    pub struct Guard(Option<UnixListener>);

    fn socket_path() -> PathBuf {
        let dir = std::env::var_os("XDG_RUNTIME_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| std::env::temp_dir().join(format!("mosaic-{}", std::env::var("USER").unwrap_or_default())));
        dir.join("mosaic.sock")
    }

    pub fn acquire() -> Option<Guard> {
        let path = socket_path();
        if let Ok(mut s) = UnixStream::connect(&path) {
            let _ = s.write_all(b"show");
            return None;
        }
        // Stale socket from a crashed run.
        let _ = std::fs::remove_file(&path);
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        Some(Guard(UnixListener::bind(&path).ok()))
    }

    pub fn listen(guard: Guard, on_show: impl Fn() + Send + 'static) {
        let Some(listener) = guard.0 else { return };
        std::thread::spawn(move || {
            for conn in listener.incoming() {
                if conn.is_ok() {
                    on_show();
                }
            }
        });
    }
}

#[cfg(not(any(windows, unix)))]
mod imp {
    pub struct Guard;
    pub fn acquire() -> Option<Guard> {
        Some(Guard)
    }
    pub fn listen(_guard: Guard, _on_show: impl Fn() + Send + 'static) {}
}

pub use imp::{acquire, listen};
