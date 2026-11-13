//! Diagnostic branch only: directly bracket the unchanged Instant interval.
//! ETW markers outside the interval align raw QPC with exported kernel events.
use windows_sys::Win32::System::Threading::GetCurrentThreadId;

#[repr(C)]
struct ProviderId(u32, u16, u16, [u8; 8]);

// ETW provider: {97057e99-44cf-4e25-aeb7-139ea0cdb951}.
const PROVIDER: ProviderId = ProviderId(
    0x97057e99,
    0x44cf,
    0x4e25,
    [0xae, 0xb7, 0x13, 0x9e, 0xa0, 0xcd, 0xb9, 0x51],
);

#[link(name = "kernel32")]
unsafe extern "system" {
    fn QueryPerformanceCounter(value: *mut i64) -> i32;
    fn QueryPerformanceFrequency(value: *mut i64) -> i32;
}

#[link(name = "advapi32")]
unsafe extern "system" {
    fn EventRegister(
        provider: *const ProviderId,
        callback: *const std::ffi::c_void,
        context: *const std::ffi::c_void,
        handle: *mut u64,
    ) -> u32;
    fn EventWriteString(handle: u64, level: u8, keyword: u64, message: *const u16) -> u32;
    fn EventUnregister(handle: u64) -> u32;
}

pub(super) fn qpc() -> i64 {
    let mut value = 0;
    // SAFETY: a writable aligned counter output is live for this call.
    assert_ne!(unsafe { QueryPerformanceCounter(&mut value) }, 0);
    value
}

pub(super) struct Clock {
    handle: u64,
    frequency: i64,
}

impl Clock {
    pub(super) fn new() -> Self {
        let mut frequency = 0;
        let mut handle = 0;
        // SAFETY: outputs are live; the provider GUID is static and the optional
        // callback/context are null. Registration is owned until Drop.
        unsafe {
            assert_ne!(QueryPerformanceFrequency(&mut frequency), 0);
            assert_eq!(
                EventRegister(&PROVIDER, std::ptr::null(), std::ptr::null(), &mut handle),
                0
            );
        }
        assert!(frequency > 0);
        Self { handle, frequency }
    }

    pub(super) fn anchor(&self, id: u64) -> serde_json::Value {
        let pid = std::process::id();
        // SAFETY: GetCurrentThreadId has no pointer arguments.
        let tid = unsafe { GetCurrentThreadId() };
        let message: Vec<u16> = format!("OKILUM_QPC_V1 id={id} pid={pid} tid={tid}")
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect();
        let before = qpc();
        // SAFETY: the registration is live and message is NUL-terminated.
        let status = unsafe { EventWriteString(self.handle, 4, 1, message.as_ptr()) };
        let after = qpc();
        assert_eq!(status, 0, "diagnostic ETW marker write failed");
        serde_json::json!({
            "marker_id": id, "pid": pid, "tid": tid,
            "frequency": self.frequency, "event_qpc_bounds": [before, after]
        })
    }
}

impl Drop for Clock {
    fn drop(&mut self) {
        // SAFETY: this object owns the registration and no callback is installed.
        unsafe { EventUnregister(self.handle) };
    }
}
