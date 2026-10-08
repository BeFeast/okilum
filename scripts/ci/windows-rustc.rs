//! Native launcher: preserve Cargo's arguments without cmd.exe re-parsing.
use std::{
    env,
    ffi::OsString,
    process::{Command, ExitCode},
};

fn portable_shaders(args: &[OsString]) -> bool {
    args.windows(2)
        .any(|pair| pair[0] == "--crate-name" && pair[1] == "gpui_windows")
        && args
            .windows(2)
            .any(|pair| pair[0] == "--target" && pair[1] == "x86_64-pc-windows-msvc")
}

fn main() -> ExitCode {
    let mut arguments = env::args_os().skip(1);
    let Some(compiler) = arguments.next() else {
        eprintln!("windows-rustc: missing compiler");
        return ExitCode::FAILURE;
    };
    let args: Vec<_> = arguments.collect();
    let mut command = Command::new(compiler);
    command.args(&args);
    if portable_shaders(&args) {
        command.env("CARGO_MANIFEST_DIR", "gpui-shaders");
    }
    match command.status() {
        Ok(status) if status.success() => ExitCode::SUCCESS,
        Ok(status) => {
            eprintln!("windows-rustc: compiler exited with {status}");
            ExitCode::FAILURE
        }
        Err(error) => {
            eprintln!("windows-rustc: {error}");
            ExitCode::FAILURE
        }
    }
}
