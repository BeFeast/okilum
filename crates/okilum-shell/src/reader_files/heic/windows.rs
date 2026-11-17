//! Windows HEIC/HEIF decoding through WIC (#2177).
//!
//! Windows ships no HEIF codec of its own; Microsoft's HEIF Image Extensions
//! and HEVC Video Extensions (Store) register WIC components. Windows
//! PowerShell 5.1 and WPF, both part of every Windows 10/11 install, reach
//! them through `BitmapDecoder`. The codec runs in that child, never in
//! Okilum, inside a Job Object with memory and CPU limits; the 30 s wall
//! deadline and kill come from `run_decoder`.
use super::{run_decoder, DecoderFailed, Missing, MissingDecoder};
use std::io;
use std::os::windows::io::AsRawHandle as _;
use std::os::windows::process::CommandExt as _;
use std::path::Path;
use std::process::{Child, Command};
use std::time::Duration;
use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, INVALID_HANDLE_VALUE};
use windows_sys::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, Thread32First, Thread32Next, TH32CS_SNAPTHREAD, THREADENTRY32,
};
use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
    SetInformationJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
    JOB_OBJECT_LIMIT_DIE_ON_UNHANDLED_EXCEPTION, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    JOB_OBJECT_LIMIT_PROCESS_MEMORY, JOB_OBJECT_LIMIT_PROCESS_TIME,
};
use windows_sys::Win32::System::Threading::{
    OpenThread, ResumeThread, CREATE_NO_WINDOW, CREATE_SUSPENDED, THREAD_SUSPEND_RESUME,
};

/// Exit code the script uses when no WIC component can decode the image.
const CODEC_MISSING: i32 = 3;

/// Runs in the private directory: reads `source.heic`, writes `preview.png`.
/// The first frame is WIC's primary image. WIC returns pixels unrotated and
/// reports orientation as `System.Photo.Orientation`, which the script
/// applies (EXIF values 2–8). Microsoft does not document how its HEIF codec
/// maps `irot`/`imir`; hosted CI has no codec, so that mapping is unverified.
/// These mean the extensions are missing: 0x88982F50
/// (WINCODEC_ERR_COMPONENTNOTFOUND), 0x88982F8B
/// (WINCODEC_ERR_COMPONENTINITIALIZEFAILURE: the inbox HEIF container decoder
/// without the Store codec; observed on windows-2022), 0x88982F07
/// (WINCODEC_ERR_UNKNOWNIMAGEFORMAT; the bytes already passed the `ftyp`
/// check) and Media Foundation errors (0xC00D…, no HEVC decoder).
const SCRIPT: &str = r#"
$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'
$code = 1
try {
    Add-Type -AssemblyName PresentationCore
    $dir = [Environment]::CurrentDirectory
    $stream = [IO.File]::OpenRead([IO.Path]::Combine($dir, 'source.heic'))
    try {
        $decoder = [Windows.Media.Imaging.BitmapDecoder]::Create($stream, [Windows.Media.Imaging.BitmapCreateOptions]::None, [Windows.Media.Imaging.BitmapCacheOption]::None)
        $frame = $decoder.Frames[0]
        $width = $frame.PixelWidth
        $height = $frame.PixelHeight
        if ($width -le 0 -or $height -le 0 -or $width -gt 16384 -or $height -gt 16384 -or ([long]$width * $height) -gt 64000000) {
            [Console]::Error.WriteLine("dimensions ${width}x${height} exceed the preview limit")
            $code = 4
        } else {
            $orientation = 1
            try {
                $value = $frame.Metadata.GetQuery('System.Photo.Orientation')
                if ($null -ne $value) { $orientation = [int]$value }
            } catch { }
            $bitmap = [Windows.Media.Imaging.BitmapSource]$frame
            $rotation = switch ($orientation) { 3 { 180 } 5 { 90 } 6 { 90 } 7 { 270 } 8 { 270 } default { 0 } }
            $group = [Windows.Media.TransformGroup]::new()
            if ($rotation) { $group.Children.Add([Windows.Media.RotateTransform]::new($rotation)) }
            if ($orientation -in 2, 5, 7) { $group.Children.Add([Windows.Media.ScaleTransform]::new(-1.0, 1.0)) }
            if ($orientation -eq 4) { $group.Children.Add([Windows.Media.ScaleTransform]::new(1.0, -1.0)) }
            if ($group.Children.Count -gt 0) { $bitmap = [Windows.Media.Imaging.TransformedBitmap]::new($bitmap, $group) }
            $encoder = [Windows.Media.Imaging.PngBitmapEncoder]::new()
            $encoder.Frames.Add([Windows.Media.Imaging.BitmapFrame]::Create($bitmap))
            $out = [IO.File]::Create([IO.Path]::Combine($dir, 'preview.png'))
            try { $encoder.Save($out) } finally { $out.Dispose() }
            $code = 0
        }
    } finally {
        $stream.Dispose()
    }
} catch {
    $e = $_.Exception
    while ($null -ne $e) {
        $hr = '{0:X8}' -f $e.HResult
        [Console]::Error.WriteLine("$($e.GetType().FullName) 0x${hr}: $($e.Message)")
        if ($hr -in '88982F50', '88982F8B', '88982F07' -or $hr.StartsWith('C00D')) { $code = 3 }
        $e = $e.InnerException
    }
}
exit $code
"#;

pub(super) fn decode(directory: &Path) -> anyhow::Result<()> {
    use base64::Engine as _;
    let script: Vec<u8> = SCRIPT.encode_utf16().flat_map(u16::to_le_bytes).collect();
    // The system copy, never a `powershell.exe` found first on PATH.
    let root = std::env::var_os("SystemRoot").unwrap_or_else(|| r"C:\Windows".into());
    let powershell = Path::new(&root).join(r"System32\WindowsPowerShell\v1.0\powershell.exe");
    let log = directory.join("decoder.log");
    let mut command = Command::new(powershell);
    // -EncodedCommand is not subject to the script execution policy.
    command
        .args([
            "-NoLogo",
            "-NoProfile",
            "-NonInteractive",
            "-EncodedCommand",
        ])
        .arg(base64::engine::general_purpose::STANDARD.encode(script))
        .current_dir(directory)
        .stderr(std::fs::File::create(&log)?);
    let Err(error) = run_decoder(&mut command, Duration::from_secs(30)) else {
        return Ok(());
    };
    if let Some(DecoderFailed {
        code: Some(CODEC_MISSING),
        ..
    }) = error.downcast_ref::<DecoderFailed>()
    {
        return Err(MissingDecoder(Missing::WindowsCodec).into());
    }
    use std::io::Read as _;
    let mut text = String::new();
    if let Ok(file) = std::fs::File::open(&log) {
        let _ = file.take(16 * 1024).read_to_string(&mut text);
    }
    Err(error.context(text.trim().to_owned()))
}

/// A Job Object owning the decoder and anything it starts. Dropping it
/// (`JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`) ends whatever is left.
pub(super) struct Job(HANDLE);

impl Drop for Job {
    fn drop(&mut self) {
        // SAFETY: the handle is owned by this value and closed once.
        unsafe { CloseHandle(self.0) };
    }
}

impl Job {
    fn limited() -> io::Result<Self> {
        // SAFETY: plain Win32 calls on a handle owned by this value.
        let handle = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
        if handle.is_null() {
            return Err(io::Error::last_os_error());
        }
        let job = Job(handle);
        let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE
            | JOB_OBJECT_LIMIT_DIE_ON_UNHANDLED_EXCEPTION
            | JOB_OBJECT_LIMIT_PROCESS_MEMORY
            | JOB_OBJECT_LIMIT_PROCESS_TIME;
        // Committed memory per process: the .NET runtime plus a 64 MP frame
        // and one transformed copy. User CPU time in 100 ns units.
        limits.ProcessMemoryLimit = 1536 * 1024 * 1024;
        limits.BasicLimitInformation.PerProcessUserTimeLimit = 20 * 10_000_000;
        // SAFETY: the pointer and size describe `limits`, which outlives the call.
        let ok = unsafe {
            SetInformationJobObject(
                job.0,
                JobObjectExtendedLimitInformation,
                &limits as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION as *const core::ffi::c_void,
                std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            )
        };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(job)
    }
}

/// Starts the child suspended, puts it in a limited job, then resumes it, so
/// no decoder instruction runs outside the limits.
pub(super) fn spawn_limited(command: &mut Command) -> io::Result<(Child, Job)> {
    command.creation_flags(CREATE_SUSPENDED | CREATE_NO_WINDOW);
    let mut child = command.spawn()?;
    let started = Job::limited().and_then(|job| {
        // SAFETY: both handles are valid for the duration of the call.
        if unsafe { AssignProcessToJobObject(job.0, child.as_raw_handle() as HANDLE) } == 0 {
            return Err(io::Error::last_os_error());
        }
        resume(child.id())?;
        Ok(job)
    });
    match started {
        Ok(job) => Ok((child, job)),
        Err(error) => {
            let _ = child.kill();
            let _ = child.wait();
            Err(error)
        }
    }
}

/// Resume the threads of a process created with `CREATE_SUSPENDED`.
fn resume(pid: u32) -> io::Result<()> {
    // SAFETY: Toolhelp snapshot handles are closed below; thread handles are
    // opened, used and closed within one iteration.
    let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0) };
    if snapshot == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }
    let mut entry = THREADENTRY32 {
        dwSize: std::mem::size_of::<THREADENTRY32>() as u32,
        ..Default::default()
    };
    let mut resumed = 0;
    let mut more = unsafe { Thread32First(snapshot, &mut entry) };
    while more != 0 {
        if entry.th32OwnerProcessID == pid {
            let thread = unsafe { OpenThread(THREAD_SUSPEND_RESUME, 0, entry.th32ThreadID) };
            if !thread.is_null() {
                if unsafe { ResumeThread(thread) } != u32::MAX {
                    resumed += 1;
                }
                unsafe { CloseHandle(thread) };
            }
        }
        entry.dwSize = std::mem::size_of::<THREADENTRY32>() as u32;
        more = unsafe { Thread32Next(snapshot, &mut entry) };
    }
    unsafe { CloseHandle(snapshot) };
    if resumed == 0 {
        return Err(io::Error::other(
            "could not resume the suspended HEIC decoder",
        ));
    }
    Ok(())
}
