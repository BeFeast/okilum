//! Minimal `SSH_ASKPASS` program for the real-OpenSSH test in
//! `tests/ssh_askpass_e2e.rs`; the shipped app runs the same entry at the top
//! of its own `main`.
fn main() {
    std::process::exit(okilum_core::ssh_askpass::helper_entry().unwrap_or(2));
}
