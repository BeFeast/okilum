//! A `powershell.exe` planted in the selected vault folder. It must never run.
fn main() {
    let marker = std::env::var_os("OKILUM_TERMINAL_MARKER_START").expect("marker");
    let me = std::env::current_exe().expect("own path");
    std::fs::write(marker, format!("DECOY|{}", me.display())).expect("write marker");
}
