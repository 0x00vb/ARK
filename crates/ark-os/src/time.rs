//! Local time.

/// Seconds east of UTC in local time at Unix time `secs`, daylight saving included. `0` when
/// the system cannot say.
pub fn local_offset(secs: i64) -> i64 {
    sys::local_offset(secs)
}

#[cfg(unix)]
mod sys {
    pub fn local_offset(secs: i64) -> i64 {
        let t = secs as libc::time_t;
        // SAFETY: localtime_r only writes the `tm` it is given.
        unsafe {
            let mut tm: libc::tm = std::mem::zeroed();
            if libc::localtime_r(&t, &mut tm).is_null() {
                return 0;
            }
            tm.tm_gmtoff as i64
        }
    }
}

#[cfg(windows)]
mod sys {
    use windows_sys::Win32::Foundation::{FILETIME, SYSTEMTIME};
    use windows_sys::Win32::System::Time::{
        FileTimeToSystemTime, SystemTimeToFileTime, SystemTimeToTzSpecificLocalTime,
    };

    /// 100 ns ticks between 1601-01-01 (Windows) and 1970-01-01 (Unix).
    const EPOCH_DIFF: i64 = 116_444_736_000_000_000;

    pub fn local_offset(secs: i64) -> i64 {
        let ticks = secs.saturating_mul(10_000_000).saturating_add(EPOCH_DIFF);
        let utc_ft = FILETIME {
            dwLowDateTime: ticks as u32,
            dwHighDateTime: (ticks >> 32) as u32,
        };
        // SAFETY: plain conversions between structs on the stack.
        unsafe {
            let mut utc: SYSTEMTIME = std::mem::zeroed();
            let mut local: SYSTEMTIME = std::mem::zeroed();
            let mut local_ft: FILETIME = std::mem::zeroed();
            if FileTimeToSystemTime(&utc_ft, &mut utc) == 0
                || SystemTimeToTzSpecificLocalTime(std::ptr::null(), &utc, &mut local) == 0
                || SystemTimeToFileTime(&local, &mut local_ft) == 0
            {
                return 0;
            }
            let local_ticks =
                ((local_ft.dwHighDateTime as i64) << 32) | local_ft.dwLowDateTime as i64;
            (local_ticks - ticks) / 10_000_000
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn an_offset_is_within_a_day_and_whole_minutes() {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;
        let o = super::local_offset(now);
        assert!(o.abs() <= 14 * 3600, "{o}");
        assert_eq!(o % 60, 0, "{o}");
    }
}
