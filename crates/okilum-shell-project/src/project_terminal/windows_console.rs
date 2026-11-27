//! Windows PowerShell in its own console for «Open in terminal» without
//! Windows Terminal (#2837 review). `std::process::Command` always passes
//! standard handles (STARTF_USESTDHANDLES); a new console does not replace
//! them, so a NUL or null stdin would leave `-NoExit` PowerShell at EOF.
//! CreateProcessW here sets no standard handles and inherits none, so the
//! child attaches its std handles to its new console.
//!
//! Self-contained raw FFI: `scripts/ci/windows-terminal-console.py` compiles
//! this exact file with rustc and drives it on a native Windows runner.
use std::ffi::{c_void, OsStr, OsString};
use std::os::windows::ffi::{OsStrExt as _, OsStringExt as _};
use std::path::{Path, PathBuf};

#[repr(C)]
struct StartupInfoW {
    cb: u32,
    reserved: *mut u16,
    desktop: *mut u16,
    title: *mut u16,
    x: u32,
    y: u32,
    x_size: u32,
    y_size: u32,
    x_count_chars: u32,
    y_count_chars: u32,
    fill_attribute: u32,
    flags: u32,
    show_window: u16,
    reserved2_size: u16,
    reserved2: *mut u8,
    std_input: *mut c_void,
    std_output: *mut c_void,
    std_error: *mut c_void,
}

#[repr(C)]
struct ProcessInformation {
    process: *mut c_void,
    thread: *mut c_void,
    process_id: u32,
    thread_id: u32,
}

#[link(name = "kernel32")]
unsafe extern "system" {
    fn CreateProcessW(
        application: *const u16,
        command_line: *mut u16,
        process_attributes: *const c_void,
        thread_attributes: *const c_void,
        inherit_handles: i32,
        creation_flags: u32,
        environment: *const c_void,
        current_directory: *const u16,
        startup_info: *const StartupInfoW,
        process_information: *mut ProcessInformation,
    ) -> i32;
    fn CloseHandle(handle: *mut c_void) -> i32;
    fn GetSystemDirectoryW(buffer: *mut u16, size: u32) -> u32;
}

const CREATE_NEW_CONSOLE: u32 = 0x0000_0010;
const CREATE_UNICODE_ENVIRONMENT: u32 = 0x0000_0400;

/// The Windows system directory from the OS, not from the environment.
pub fn system_directory() -> Option<PathBuf> {
    let mut buffer = vec![0u16; 260];
    // SAFETY: the buffer length is passed; the call writes at most that many units.
    let length = unsafe { GetSystemDirectoryW(buffer.as_mut_ptr(), buffer.len() as u32) } as usize;
    (length > 0 && length < buffer.len())
        .then(|| PathBuf::from(OsString::from_wide(&buffer[..length])))
}

/// One argument in the MSVC CRT convention, over UTF-16 units so unpaired
/// surrogates survive.
fn quote(argument: &OsStr, line: &mut Vec<u16>) {
    const QUOTE: u16 = b'"' as u16;
    const BACKSLASH: u16 = b'\\' as u16;
    line.push(QUOTE);
    let mut slashes = 0;
    for unit in argument.encode_wide() {
        if unit == BACKSLASH {
            slashes += 1;
            continue;
        }
        let escaped = if unit == QUOTE {
            slashes * 2 + 1
        } else {
            slashes
        };
        line.extend(std::iter::repeat_n(BACKSLASH, escaped));
        line.push(unit);
        slashes = 0;
    }
    line.extend(std::iter::repeat_n(BACKSLASH, slashes * 2));
    line.push(QUOTE);
}

/// The command line CreateProcessW receives: every word quoted.
pub fn command_line(program: &Path, arguments: &[OsString]) -> Vec<u16> {
    let mut line = Vec::new();
    quote(program.as_os_str(), &mut line);
    for argument in arguments {
        line.push(b' ' as u16);
        quote(argument, &mut line);
    }
    line.push(0);
    line
}

/// Starts `program` (absolute; no search) in a new console with no standard
/// handles set and no handles inherited, in Okilum's own working directory.
/// Returns the process id; the console lives on independently.
pub fn spawn(program: &Path, arguments: &[OsString]) -> std::io::Result<u32> {
    if !program.is_absolute() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "terminal program must be an absolute path",
        ));
    }
    let application: Vec<u16> = program.as_os_str().encode_wide().chain([0]).collect();
    let mut line = command_line(program, arguments);
    // SAFETY: all-zero is a valid STARTUPINFOW; flags stay 0, so no standard
    // handles are passed and the new console provides them.
    let mut startup: StartupInfoW = unsafe { std::mem::zeroed() };
    startup.cb = std::mem::size_of::<StartupInfoW>() as u32;
    // SAFETY: zeroed PROCESS_INFORMATION is filled by CreateProcessW.
    let mut process: ProcessInformation = unsafe { std::mem::zeroed() };
    // SAFETY: NUL-terminated buffers outlive the call; the command line is
    // mutable as the API requires; null environment/directory inherit ours.
    let created = unsafe {
        CreateProcessW(
            application.as_ptr(),
            line.as_mut_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            0,
            CREATE_NEW_CONSOLE | CREATE_UNICODE_ENVIRONMENT,
            std::ptr::null(),
            std::ptr::null(),
            &startup,
            &mut process,
        )
    };
    if created == 0 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: both handles were just returned by CreateProcessW.
    unsafe {
        CloseHandle(process.thread);
        CloseHandle(process.process);
    }
    Ok(process.process_id)
}
