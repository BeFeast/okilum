//! Console output must never turn a closed desktop-launch pipe into a crash.
use std::{fmt, io::Write as _};

pub(crate) fn stderr(args: fmt::Arguments<'_>) {
    let _ = writeln!(std::io::stderr().lock(), "{args}");
}

// Some feature sets only emit errors; both sinks are covered by pipe tests.
#[allow(dead_code)]
pub(crate) fn stdout(args: fmt::Arguments<'_>) {
    let _ = writeln!(std::io::stdout().lock(), "{args}");
}

macro_rules! diagnostic_error {
    () => { $crate::diagnostic_io::stderr(format_args!("")) };
    ($($arg:tt)*) => { $crate::diagnostic_io::stderr(format_args!($($arg)*)) };
}

#[allow(unused_macros)]
macro_rules! diagnostic_println {
    () => { $crate::diagnostic_io::stdout(format_args!("")) };
    ($($arg:tt)*) => { $crate::diagnostic_io::stdout(format_args!($($arg)*)) };
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead as _, Read as _};

    #[test]
    fn closed_pipe_output_is_best_effort() {
        const CHILD: &str = "OKILUM_CLOSED_PIPE_TEST";
        if std::env::var_os(CHILD).is_some() {
            // The parent waits for this handshake before closing both pipes.
            std::io::stdout().write_all(b"\nREADY\n").unwrap();
            std::io::stdout().flush().unwrap();
            std::io::stdin().read_exact(&mut [0]).unwrap();
            for _ in 0..32 {
                diagnostic_error!("desktop diagnostic {}", 2787);
                diagnostic_println!("CLI diagnostic {}", 2787);
            }
            // Avoid the test harness writing its own summary to the closed pipe.
            std::process::exit(0);
        }
        let mut child = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "diagnostic_io::tests::closed_pipe_output_is_best_effort",
                "--nocapture",
            ])
            .env(CHILD, "1")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        let mut output = std::io::BufReader::new(child.stdout.take().unwrap());
        let mut line = String::new();
        loop {
            line.clear();
            assert!(
                output.read_line(&mut line).unwrap() > 0,
                "child did not reach its handshake"
            );
            if line.trim() == "READY" {
                break;
            }
        }
        drop(output);
        drop(child.stderr.take());
        child.stdin.take().unwrap().write_all(b"x").unwrap();
        assert!(
            child.wait().unwrap().success(),
            "closed output pipes killed the child"
        );
    }
}
