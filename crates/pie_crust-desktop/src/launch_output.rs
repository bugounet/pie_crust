//! A GUI launch never allocates a console. CLI help/errors can still use the
//! caller's terminal or redirected output; Explorer launches use a dialog.

pub(super) fn show(message: &str, error: bool) {
    #[cfg(windows)]
    if !windows::has_output(error) {
        windows::dialog(message, error);
        return;
    }

    use std::io::Write;
    if error {
        let _ = writeln!(std::io::stderr().lock(), "{message}");
    } else {
        let _ = writeln!(std::io::stdout().lock(), "{message}");
    }
}

#[cfg(windows)]
mod windows {
    use std::ffi::c_void;

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn AttachConsole(process_id: u32) -> i32;
        fn GetStdHandle(handle: u32) -> *mut c_void;
        fn GetFileType(handle: *mut c_void) -> u32;
    }

    #[link(name = "user32")]
    unsafe extern "system" {
        fn MessageBoxW(window: *mut c_void, text: *const u16, title: *const u16, flags: u32)
        -> i32;
    }

    pub(super) fn has_output(error: bool) -> bool {
        let stream = if error { -12_i32 } else { -11_i32 } as u32;
        // SAFETY: these calls accept the documented standard handle selectors.
        // Preserve inherited pipes/files instead of replacing them by attaching.
        unsafe {
            if GetFileType(GetStdHandle(stream)) != 0 {
                return true;
            }
            AttachConsole(u32::MAX); // ATTACH_PARENT_PROCESS; never AllocConsole.
            GetFileType(GetStdHandle(stream)) != 0
        }
    }

    pub(super) fn dialog(message: &str, error: bool) {
        let text: Vec<u16> = message
            .replace('\0', " ")
            .encode_utf16()
            .chain([0])
            .collect();
        let title: Vec<u16> = "pie_crust".encode_utf16().chain([0]).collect();
        // SAFETY: both buffers are NUL-terminated and live until the dialog closes.
        unsafe {
            MessageBoxW(
                std::ptr::null_mut(),
                text.as_ptr(),
                title.as_ptr(),
                if error { 0x10 } else { 0x40 },
            );
        }
    }
}
