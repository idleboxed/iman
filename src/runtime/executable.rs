// SPDX-License-Identifier: BSD-3-Clause
//! Common descriptor-held GUI execution, independent of CPU and firmware.
use std::{
    ffi::{CStr, CString},
    fs::File,
    io,
    os::{fd::{AsRawFd, OwnedFd}, unix::{fs::FileExt, process::CommandExt}},
    path::Path,
    process::{Command, ExitStatus, Stdio},
};
use crate::session::{self, ChildGuard};

/// Spawn a caller-owned, already validated ELF descriptor, without reopening its
/// path. Kept separate so synthetic executables can test the real lifecycle.
pub fn supervise(
    app: std::os::fd::OwnedFd,
    arguments: Vec<&'static CStr>,
    observer: &mut dyn FnMut(&'static str),
) -> io::Result<ExitStatus> {
    observer("gui_start");
    let (mut child, mut log) = spawn_pinned(app, arguments.iter().map(|s| (*s).to_owned()).collect(), None)?;
    log.observe(child.0.id(), Some(std::time::Instant::now()), "supervisor");
    crate::session::diagnostic(format_args!("iman: IGUI child pid={}", child.0.id()));
    let result = session::wait(&mut child.0, &mut |phase| {
        log.poll();
        observer(phase);
    }, "gui");
    drop(child);
    crate::runtime::child_log::finish_gui(log, None);
    observer("gui_exit");
    match &result {
        Ok(status) => crate::session::diagnostic(format_args!("iman: IGUI exited with {status}")),
        Err(error) => {
            crate::session::diagnostic(format_args!("iman: IGUI supervision failed: {error}"))
        }
    }
    result
}

fn spawn_pinned(
    app: OwnedFd,
    arguments: Vec<CString>,
    cwd: Option<&Path>,
) -> io::Result<(ChildGuard, crate::runtime::child_log::ChildLog)> {
    if arguments.is_empty() || arguments.len() >= 64 {
        return Err(io::Error::other("invalid pinned ELF arguments"));
    }
    let app = File::from(app);
    let mut prefix = [0; 2];
    let script = app.read_at(&mut prefix, 0)? == 2 && prefix == *b"#!";
    // Generic Linux sessions retain DISPLAY and other caller settings. Low-level
    // supervisors keep their former empty environment contract.
    let environment = if cwd.is_some() {
        std::env::vars_os().map(|(key, value)| {
            let mut item = key.into_encoded_bytes();
            item.push(b'=');
            item.extend_from_slice(value.as_encoded_bytes());
            CString::new(item)
        }).collect::<Result<Vec<_>, _>>()?
    } else { Vec::new() };
    let mut argv = arguments.iter().map(|s| s.as_ptr()).collect::<Vec<_>>();
    argv.push(std::ptr::null());
    let mut envp = environment.iter().map(|s| s.as_ptr()).collect::<Vec<_>>();
    envp.push(std::ptr::null());
    // Pointer arrays are prepared before fork. Capture their addresses as usize;
    // the owned backing strings remain in the Command closure until exec/drop.
    let argv = argv.into_iter().map(|p| p as usize).collect::<Vec<_>>();
    let envp = envp.into_iter().map(|p| p as usize).collect::<Vec<_>>();
    let mut command = Command::new("/no-path-fallback-for-pinned-igui");
    command.env_clear().stdin(Stdio::null()).stdout(Stdio::inherit()).stderr(Stdio::inherit());
    if let Some(cwd) = cwd { command.current_dir(cwd); }
    session::prepare_child(&mut command);
    let log = crate::runtime::child_log::ChildLog::attach(&mut command, true)?;
    session::restore_file_limit(&mut command);
    // SAFETY: only async-signal-safe syscalls after fork, no allocation or fallback.
    unsafe {
        command.pre_exec(move || {
            let _keep_alive = (&arguments, &environment);
            // Linux needs the script FD in its interpreter. Native ELF keeps
            // CLOEXEC; scripts inherit only this read-only FD, never other resources.
            if script && libc::fcntl(app.as_raw_fd(), libc::F_SETFD, 0) < 0 {
                return Err(io::Error::last_os_error());
            }
            // libc does not expose an execveat wrapper on every musl target.
            // Use its architecture-defined syscall number, never a hand-coded ABI.
            libc::syscall(libc::SYS_execveat, app.as_raw_fd(), c"".as_ptr(),
                argv.as_ptr(), envp.as_ptr(), libc::AT_EMPTY_PATH);
            Err(io::Error::last_os_error())
        });
    }
    let child = ChildGuard(command.spawn()?);
    drop(command);
    Ok((child, log))
}

/// Socket-managed lifecycle for an already pinned executable. The production
/// entry validates the root/executable first; fixtures supply only their own host ELF.
pub fn supervise_session(
    app: std::os::fd::OwnedFd,
    arguments: Vec<std::ffi::CString>,
    control: &mut crate::session::control::Control,
    observer: &mut dyn FnMut(&'static str),
) -> io::Result<()> {
    let handoff = pinned_gui(
        app,
        arguments,
        None,
        &imanlib::igui::GuiState::default(),
        None,
        control,
        observer,
    )?;
    if handoff.is_some() {
        return Err(io::Error::other(
            "single-GUI supervisor received a game handoff; use the session runner",
        ));
    }
    Ok(())
}

pub(crate) fn pinned_gui(
    app: std::os::fd::OwnedFd,
    arguments: Vec<std::ffi::CString>,
    cwd: Option<&Path>,
    state: &imanlib::igui::GuiState,
    notice: Option<String>,
    control: &mut crate::session::control::Control,
    observer: &mut dyn FnMut(&'static str),
) -> io::Result<Option<imanlib::igui::Handoff>> {
    observer("gui_start");
    control.poll("gui_start");
    let (mut child, mut log) = spawn_pinned(app, arguments, cwd)?;
    log.observe(child.0.id(), control.diagnostic_clock(), "iman_session");
    control.begin_gui(child.0.id(), state.clone(), notice);
    crate::session::diagnostic(format_args!("iman: IGUI child pid={}", child.0.id()));
    let result = session::wait_gui(&mut child.0, control, &mut |phase| {
        log.poll();
        observer(phase);
    });
    drop(child);
    crate::runtime::child_log::finish_gui(log, Some(control));
    result
}
