//! What the live views and the TUI share apart from their drawing: sizes in
//! the units they show, the host's name and the local time.

use std::time::{SystemTime, UNIX_EPOCH};

use rampire_core::{GIB, MIB};

pub fn gib(bytes: u64) -> String {
    format!("{:.3} GiB", bytes as f64 / GIB as f64)
}

pub fn mib(bytes: u64) -> u64 {
    bytes / MIB
}

pub fn host_name() -> String {
    std::fs::read_to_string("/proc/sys/kernel/hostname")
        .map(|name| name.trim().to_owned())
        .unwrap_or_else(|_| "host".to_owned())
}

/// Local wall-clock time as HH:MM:SS, using the system's UTC offset.
pub fn clock() -> String {
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs() as i64)
        + local_offset_seconds();
    let day = seconds.rem_euclid(86_400);
    format!("{:02}:{:02}:{:02}", day / 3600, day % 3600 / 60, day % 60)
}

#[cfg(unix)]
fn local_offset_seconds() -> i64 {
    #[repr(C)]
    struct Tm {
        tm_sec: i32,
        tm_min: i32,
        tm_hour: i32,
        tm_mday: i32,
        tm_mon: i32,
        tm_year: i32,
        tm_wday: i32,
        tm_yday: i32,
        tm_isdst: i32,
        tm_gmtoff: i64,
        tm_zone: *const u8,
    }
    unsafe extern "C" {
        fn time(timer: *mut i64) -> i64;
        fn localtime_r(timer: *const i64, result: *mut Tm) -> *mut Tm;
    }
    // SAFETY: `time` accepts a null pointer; `localtime_r` writes only the
    // caller-owned, correctly laid out `tm` for this glibc/x86-64 target.
    unsafe {
        let now = time(std::ptr::null_mut());
        let mut tm = std::mem::zeroed::<Tm>();
        if localtime_r(&now, &mut tm).is_null() {
            0
        } else {
            tm.tm_gmtoff
        }
    }
}

#[cfg(not(unix))]
fn local_offset_seconds() -> i64 {
    0
}
