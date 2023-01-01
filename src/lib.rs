#![no_std]
extern crate alloc;

macro_rules! eprint {
    ($msg:literal, $($arg:expr),*) => {
        match ::alloc::format!($msg, $($arg),*) {
            msg => {
                unsafe { libc::write(2, msg.as_bytes().as_ptr() as *const _, msg.len()) };
            }
        }
    }
}

pub mod xsk;
/// Bindings for XDP (kernel-interface).
pub mod xdp;

pub(crate) struct LastErrno;
pub struct Errno(libc::c_int);

impl From<LastErrno> for Errno {
    fn from(LastErrno: LastErrno) -> Self {
        Errno::new()
    }
}

impl Errno {
    pub(crate) fn new() -> Self {
        Errno(unsafe { *libc::__errno_location() })
    }
}

impl core::fmt::Display for Errno {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let st = unsafe { libc::strerror(self.0) };
        let cstr = unsafe { core::ffi::CStr::from_ptr(st) };
        write!(f, "{}", cstr.to_string_lossy())
    }
}

impl core::fmt::Debug for Errno {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "Errno({}: {})", self.0, self)
    }
}
