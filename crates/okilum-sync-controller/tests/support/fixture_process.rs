//! Linux lifetime ownership for disposable fixture processes only.
use std::{
    io,
    os::unix::process::CommandExt,
    process::{Child, Command},
};

pub fn spawn(command: &mut Command) -> io::Result<Child> {
    let parent = std::process::id() as libc::pid_t;
    command.process_group(0);
    // SAFETY: only async-signal-safe syscalls run between fork and exec. Register
    // before exec, then close the race where the test died before registration.
    unsafe {
        command.pre_exec(move || {
            if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL) != 0 {
                return Err(io::Error::last_os_error());
            }
            if libc::getppid() != parent {
                libc::_exit(1);
            }
            Ok(())
        });
    }
    command.spawn()
}

pub fn stop(child: &mut Child) -> io::Result<()> {
    // Each fixture owns its process group. Kill descendants as well, and always
    // wait for the direct child even if signaling fails or it already exited.
    let killed = unsafe { libc::kill(-(child.id() as libc::pid_t), libc::SIGKILL) };
    let error = io::Error::last_os_error();
    let waited = child.wait();
    waited?;
    if killed != 0 && error.raw_os_error() != Some(libc::ESRCH) {
        return Err(error);
    }
    Ok(())
}
