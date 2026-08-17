use std::ffi::c_void;
use std::io;
use std::sync::atomic::{AtomicBool, AtomicPtr, AtomicU32, Ordering};

use windows_sys::Win32::Foundation::{HANDLE, INVALID_HANDLE_VALUE};
use windows_sys::Win32::System::Console::{
    CTRL_BREAK_EVENT, CTRL_C_EVENT, ENABLE_ECHO_INPUT, ENABLE_LINE_INPUT, ENABLE_PROCESSED_INPUT,
    GetConsoleMode, GetStdHandle, STD_INPUT_HANDLE, SetConsoleCtrlHandler, SetConsoleMode,
};

static GUARD_ACTIVE: AtomicBool = AtomicBool::new(false);
static GUARDED_HANDLE: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());
static GUARDED_MODE: AtomicU32 = AtomicU32::new(0);

/// Repairs a console left in raw mode by a previously interrupted password
/// prompt while preserving PSReadLine and terminal-specific input flags.
pub fn restore_line_input() -> io::Result<()> {
    let (handle, mode) = input_mode()?;
    let restored = line_input_mode(mode);
    if restored != mode && unsafe { SetConsoleMode(handle, restored) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// Restores the console mode if Windows terminates the process on Ctrl+C before
/// rpassword's normal RAII cleanup gets a chance to run.
pub struct ConsoleModeGuard {
    handle: HANDLE,
    mode: u32,
}

impl ConsoleModeGuard {
    pub fn install() -> io::Result<Self> {
        let (handle, mode) = input_mode()?;
        if GUARD_ACTIVE.swap(true, Ordering::AcqRel) {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "a Windows console mode guard is already active",
            ));
        }

        GUARDED_MODE.store(mode, Ordering::Relaxed);
        GUARDED_HANDLE.store(handle, Ordering::Release);
        if unsafe { SetConsoleCtrlHandler(Some(restore_console_mode), 1) } == 0 {
            GUARDED_HANDLE.store(std::ptr::null_mut(), Ordering::Release);
            GUARD_ACTIVE.store(false, Ordering::Release);
            return Err(io::Error::last_os_error());
        }

        Ok(Self { handle, mode })
    }
}

impl Drop for ConsoleModeGuard {
    fn drop(&mut self) {
        // Best effort: there is no useful recovery path while unwinding from
        // another setup error, and rpassword also attempts the normal restore.
        unsafe {
            SetConsoleMode(self.handle, self.mode);
        }
        GUARD_ACTIVE.store(false, Ordering::Release);
        GUARDED_HANDLE.store(std::ptr::null_mut(), Ordering::Release);
        unsafe {
            SetConsoleCtrlHandler(Some(restore_console_mode), 0);
        }
    }
}

fn input_mode() -> io::Result<(HANDLE, u32)> {
    let handle = unsafe { GetStdHandle(STD_INPUT_HANDLE) };
    if handle.is_null() || handle == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }

    let mut mode = 0;
    if unsafe { GetConsoleMode(handle, &mut mode) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok((handle, mode))
}

const fn line_input_mode(mode: u32) -> u32 {
    mode | ENABLE_PROCESSED_INPUT | ENABLE_LINE_INPUT | ENABLE_ECHO_INPUT
}

unsafe extern "system" fn restore_console_mode(control_type: u32) -> i32 {
    if matches!(control_type, CTRL_C_EVENT | CTRL_BREAK_EVENT)
        && GUARD_ACTIVE.load(Ordering::Acquire)
    {
        let handle = GUARDED_HANDLE.load(Ordering::Acquire);
        if !handle.is_null() {
            unsafe {
                SetConsoleMode(handle, GUARDED_MODE.load(Ordering::Relaxed));
            }
        }
    }

    // Let the next registered handler (ultimately Windows' default handler)
    // preserve normal Ctrl+C termination semantics after the mode is restored.
    0
}

#[cfg(test)]
mod tests {
    use super::{ENABLE_ECHO_INPUT, ENABLE_LINE_INPUT, ENABLE_PROCESSED_INPUT, line_input_mode};

    #[test]
    fn restoring_line_input_preserves_unrelated_console_flags() {
        let terminal_flags = 0x0010 | 0x0080 | 0x0200;
        let restored = line_input_mode(terminal_flags);

        assert_eq!(restored & terminal_flags, terminal_flags);
        assert_ne!(restored & ENABLE_PROCESSED_INPUT, 0);
        assert_ne!(restored & ENABLE_LINE_INPUT, 0);
        assert_ne!(restored & ENABLE_ECHO_INPUT, 0);
    }
}
