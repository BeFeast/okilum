//! Console-subsystem stand-in for Update.exe; invoked by the actual SDK delta path.
#[link(name = "kernel32")]
unsafe extern "system" {
    fn GetConsoleWindow() -> *mut std::ffi::c_void;
}

fn main() {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    assert_eq!(args.len(), 7);
    assert_eq!(args[0], "patch");
    assert_eq!(args[1], "--old");
    assert_eq!(args[3], "--output");
    assert_eq!(args[5], "--delta");
    let output = std::path::PathBuf::from(&args[4]);
    assert_eq!(std::fs::read(&args[2]).unwrap(), b"fixture base");
    std::fs::copy(&args[6], &output).unwrap();
    let console = unsafe { GetConsoleWindow() } as usize;
    std::fs::write(output.with_extension("console"), console.to_string()).unwrap();
}
