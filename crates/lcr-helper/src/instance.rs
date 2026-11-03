use std::{io, ptr::null};

type Handle = *mut core::ffi::c_void;

const ERROR_ALREADY_EXISTS: u32 = 183;

#[link(name = "kernel32")]
extern "system" {
    fn CreateMutexW(
        mutex_attributes: *const core::ffi::c_void,
        initial_owner: i32,
        name: *const u16,
    ) -> Handle;
    fn CloseHandle(handle: Handle) -> i32;
    fn GetLastError() -> u32;
    fn SetLastError(error: u32);
}

/// Keeps a named Win32 mutex object alive for exactly this process lifetime.
///
/// LCR uses existence of the named kernel object as a lightweight
/// single-instance guard. It does not acquire/release mutex ownership and does
/// not wait or poll.
#[must_use]
pub struct NamedMutexGuard {
    handle: Handle,
}

impl NamedMutexGuard {
    /// Borrow the exact owned kernel HANDLE for a bounded native inheritance
    /// operation. Ownership remains with this guard and is never transferred.
    pub(crate) fn raw_handle(&self) -> *mut core::ffi::c_void {
        self.handle
    }
}

pub enum NamedMutexAcquire {
    Acquired(NamedMutexGuard),
    AlreadyExists,
}

/// Attempts to create one session-local named mutex.
///
/// `AlreadyExists` is race-safe: `CreateMutexW` atomically creates or opens the
/// named object. A duplicate handle is closed immediately. The acquired guard
/// closes its handle on drop, allowing a future process to become the owner
/// once all handles to the named object are gone.
pub fn try_acquire_named_mutex(name: &str) -> io::Result<NamedMutexAcquire> {
    if name.encode_utf16().any(|unit| unit == 0) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "named mutex contains NUL",
        ));
    }

    let mut name = name.encode_utf16().collect::<Vec<_>>();
    name.push(0);

    unsafe {
        // Prevent stale thread-local last-error state from masquerading as
        // ERROR_ALREADY_EXISTS after a newly created mutex.
        SetLastError(0);

        let handle = CreateMutexW(null(), 0, name.as_ptr());

        if handle.is_null() {
            return Err(io::Error::last_os_error());
        }

        if GetLastError() == ERROR_ALREADY_EXISTS {
            let _ = CloseHandle(handle);
            Ok(NamedMutexAcquire::AlreadyExists)
        } else {
            Ok(NamedMutexAcquire::Acquired(NamedMutexGuard { handle }))
        }
    }
}

impl Drop for NamedMutexGuard {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.handle);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_mutex_name(case: &str) -> String {
        format!(
            r"Local\LinhLanBang.LCR.Helper.Test.{}.{}",
            std::process::id(),
            case
        )
    }

    #[test]
    fn second_named_mutex_acquisition_is_rejected_until_guard_drops() {
        let name = test_mutex_name("exclusive");

        let first = try_acquire_named_mutex(&name).unwrap();
        let first_guard = match first {
            NamedMutexAcquire::Acquired(guard) => guard,
            NamedMutexAcquire::AlreadyExists => panic!("fresh test mutex already exists"),
        };

        assert!(matches!(
            try_acquire_named_mutex(&name).unwrap(),
            NamedMutexAcquire::AlreadyExists
        ));

        drop(first_guard);

        assert!(matches!(
            try_acquire_named_mutex(&name).unwrap(),
            NamedMutexAcquire::Acquired(_)
        ));
    }

    #[test]
    fn distinct_named_mutexes_do_not_contend() {
        let first_name = test_mutex_name("distinct-a");
        let second_name = test_mutex_name("distinct-b");

        let first = try_acquire_named_mutex(&first_name).unwrap();
        let second = try_acquire_named_mutex(&second_name).unwrap();

        assert!(matches!(first, NamedMutexAcquire::Acquired(_)));
        assert!(matches!(second, NamedMutexAcquire::Acquired(_)));
    }

    #[test]
    fn interior_nul_mutex_name_is_rejected_before_win32_call() {
        let result = try_acquire_named_mutex("Local\\LCR\0invalid");

        match result {
            Err(error) => assert_eq!(error.kind(), io::ErrorKind::InvalidInput),
            Ok(_) => panic!("interior NUL unexpectedly accepted"),
        }
    }
}
