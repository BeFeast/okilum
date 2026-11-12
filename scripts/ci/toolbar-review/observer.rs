// Diagnostic-only clocks. The measured Instant wall gate is unchanged.
#[cfg(target_os = "windows")]
fn wall_stamp() -> u64 {
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetSystemTimePreciseAsFileTime(value: *mut windows_sys::Win32::Foundation::FILETIME);
    }
    let mut value = windows_sys::Win32::Foundation::FILETIME {
        dwLowDateTime: 0,
        dwHighDateTime: 0,
    };
    unsafe { GetSystemTimePreciseAsFileTime(&mut value) };
    (u64::from(value.dwHighDateTime) << 32) | u64::from(value.dwLowDateTime)
}
#[cfg(not(target_os = "windows"))]
fn wall_stamp() -> u64 {
    let mut value = libc::timespec { tv_sec: 0, tv_nsec: 0 };
    assert_eq!(unsafe { libc::clock_gettime(libc::CLOCK_REALTIME, &mut value) }, 0);
    value.tv_sec as u64 * 1_000_000_000 + value.tv_nsec as u64
}
#[cfg(target_os = "windows")]
fn native_thread_id() -> u64 {
    u64::from(unsafe { windows_sys::Win32::System::Threading::GetCurrentThreadId() })
}
#[cfg(target_os = "macos")]
fn native_thread_id() -> u64 {
    let mut value = 0;
    assert_eq!(unsafe { libc::pthread_threadid_np(0, &mut value) }, 0);
    value
}
#[cfg(not(any(target_os = "windows", target_os = "macos")))]
fn native_thread_id() -> u64 { 0 }
#[cfg(target_os = "macos")]
fn thread_cpu_ms() -> Option<f64> {
    let mut value = libc::timespec { tv_sec: 0, tv_nsec: 0 };
    assert_eq!(unsafe { libc::clock_gettime(libc::CLOCK_THREAD_CPUTIME_ID, &mut value) }, 0);
    Some(value.tv_sec as f64 * 1000. + value.tv_nsec as f64 / 1_000_000.)
}
#[cfg(not(any(target_os = "windows", target_os = "macos")))]
fn thread_cpu_ms() -> Option<f64> { None }
