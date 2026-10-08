// SPDX-License-Identifier: BSD-3-Clause
pub mod control;
pub mod timing;
use std::{
    ffi::{CString, OsString},
    fs, io,
    io::Write,
    os::unix::{ffi::OsStrExt, process::CommandExt},
    path::Path,
    process::{Child, Command, ExitStatus, Stdio},
    sync::atomic::{AtomicBool, Ordering},
    thread,
    time::Duration,
};

use tempfile::TempDir;

use crate::{session::control::Control, emulation::launcher, runtime::paths};
use imanlib::igui::{GuiState, Handoff};
use timing::{SystemTiming, Timing};

static INTERRUPTED: AtomicBool = AtomicBool::new(false);

extern "C" fn interrupt(_: libc::c_int) {
    INTERRUPTED.store(true, Ordering::Relaxed);
}

/// Install only in the executable, never during library import/construction.
pub fn install_signal_handlers() -> io::Result<()> {
    // SAFETY: the handler only stores an atomic flag, has C ABI and static lifetime.
    unsafe {
        let mut action: libc::sigaction = std::mem::zeroed();
        action.sa_sigaction = interrupt as *const () as usize;
        libc::sigemptyset(&mut action.sa_mask);
        for signal in [libc::SIGINT, libc::SIGTERM] {
            if libc::sigaction(signal, &action, std::ptr::null_mut()) != 0 {
                return Err(io::Error::last_os_error());
            }
        }
        // A full bounded diagnostic file must yield EFBIG, not kill the manager
        // (and consequently its GUI). Restore the default in each child below.
        if libc::signal(libc::SIGXFSZ, libc::SIG_IGN) == libc::SIG_ERR {
            return Err(io::Error::last_os_error());
        }
    }
    Ok(())
}

static RAM_OUTPUT_ACTIVE: AtomicBool = AtomicBool::new(false);
static DIAGNOSTIC_CONSOLE: std::sync::Mutex<Option<fs::File>> = std::sync::Mutex::new(None);

fn safe_stderr(allow_ram_file: bool) -> bool {
    let mut info = std::mem::MaybeUninit::<libc::stat>::uninit();
    if unsafe { libc::fstat(2, info.as_mut_ptr()) } != 0 { return false; }
    let info = unsafe { info.assume_init() };
    match info.st_mode & libc::S_IFMT {
        libc::S_IFSOCK | libc::S_IFIFO => true,
        libc::S_IFCHR => (unsafe { libc::isatty(2) == 1 })
            || (libc::major(info.st_rdev) == 1 && matches!(libc::minor(info.st_rdev), 3 | 7)),
        libc::S_IFREG if allow_ram_file => {
            let mut fs = std::mem::MaybeUninit::<libc::statfs>::uninit();
            unsafe { libc::fstatfs(2, fs.as_mut_ptr()) == 0
                && fs.assume_init().f_type as u64 == libc::TMPFS_MAGIC as u64 }
        }
        _ => false,
    }
}

pub(crate) fn diagnostic_console() -> Option<fs::File> {
    use std::os::fd::FromRawFd;
    if !safe_stderr(false) { return None; }
    let fd = unsafe { libc::fcntl(2, libc::F_DUPFD_CLOEXEC, 3) };
    if fd < 0 { None } else { Some(unsafe { fs::File::from_raw_fd(fd) }) }
}

pub(crate) fn set_diagnostic_console(console: Option<fs::File>) {
    RAM_OUTPUT_ACTIVE.store(true, Ordering::Relaxed);
    if let Ok(mut target) = DIAGNOSTIC_CONSOLE.lock() { *target = console; }
}

/// Never write session diagnostics to an inherited disk file, including before
/// RAM initialization. A terminal/pipe may also receive the bounded RAM log's messages.
pub fn diagnostic(message: std::fmt::Arguments<'_>) {
    if safe_stderr(RAM_OUTPUT_ACTIVE.load(Ordering::Relaxed)) { let _ = writeln!(io::stderr().lock(), "{message}"); }
    if let Ok(mut console) = DIAGNOSTIC_CONSOLE.lock() {
        if let Some(console) = console.as_mut() { let _ = writeln!(console, "{message}"); }
    }
}

pub(crate) struct ChildGuard(pub(crate) Child);

impl Drop for ChildGuard {
    fn drop(&mut self) {
        if !matches!(self.0.try_wait(), Ok(Some(_))) {
            let _ = self.0.kill();
        }
        let _ = self.0.wait();
    }
}

pub(crate) fn prepare_child(command: &mut Command) {
    let parent = std::process::id();
    // SAFETY: pre_exec uses only async-signal-safe syscalls, without allocation.
    unsafe {
        command.pre_exec(move || {
            if libc::signal(libc::SIGXFSZ, libc::SIG_DFL) == libc::SIG_ERR {
                return Err(io::Error::last_os_error());
            }
            if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM) != 0 {
                return Err(io::Error::last_os_error());
            }
            if libc::getppid() as u32 != parent {
                return Err(io::Error::from_raw_os_error(libc::ESRCH));
            }
            let no_core = libc::rlimit {
                rlim_cur: 0,
                rlim_max: 0,
            };
            if libc::setrlimit(libc::RLIMIT_CORE, &no_core) != 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
    }
}

/// Captured children may write explicit data files without inheriting the log cap.
pub(crate) fn restore_file_limit(command: &mut Command) {
    // SAFETY: only async-signal-safe resource syscalls after fork.
    unsafe {
        command.pre_exec(|| {
            let mut size = std::mem::MaybeUninit::<libc::rlimit>::uninit();
            if libc::getrlimit(libc::RLIMIT_FSIZE, size.as_mut_ptr()) != 0 {
                return Err(io::Error::last_os_error());
            }
            let mut size = size.assume_init();
            size.rlim_cur = size.rlim_max;
            if libc::setrlimit(libc::RLIMIT_FSIZE, &size) != 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
    }
}

pub(crate) fn wait(
    child: &mut Child,
    observer: &mut dyn FnMut(&'static str),
    phase: &'static str,
) -> io::Result<ExitStatus> {
    loop {
        observer(phase);
        if INTERRUPTED.load(Ordering::Relaxed) {
            return Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "session cancelled",
            ));
        }
        if let Some(status) = child.try_wait()? {
            return Ok(status);
        }
        thread::sleep(Duration::from_millis(20));
    }
}

/// Writable game data must be on tmpfs, never silently fall back to disk.
pub fn runtime_directory(parent: &Path) -> io::Result<TempDir> {
    let path = CString::new(parent.as_os_str().as_bytes())?;
    let mut stat = std::mem::MaybeUninit::<libc::statfs>::uninit();
    // SAFETY: path is terminated; statfs initializes stat on success.
    if unsafe { libc::statfs(path.as_ptr(), stat.as_mut_ptr()) } != 0 {
        return Err(io::Error::last_os_error());
    }
    if unsafe { stat.assume_init() }.f_type as u64 != libc::TMPFS_MAGIC as u64 {
        return Err(io::Error::other("runtime directory must be on tmpfs"));
    }
    let dir = tempfile::Builder::new()
        .prefix(paths::RUNTIME_PREFIX)
        .tempdir_in(parent)?;
    for name in paths::RUNTIME_DIRS {
        fs::create_dir(dir.path().join(name))?;
    }
    Ok(dir)
}

pub(crate) fn wait_gui(
    child: &mut Child,
    control: &mut Control,
    observer: &mut dyn FnMut(&'static str),
) -> io::Result<Option<Handoff>> {
    loop {
        observer("gui");
        control.poll("gui");
        if INTERRUPTED.load(Ordering::Relaxed) {
            return Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "session cancelled",
            ));
        }
        if let Some(status) = child.try_wait()? {
            observer("gui_exit");
            diagnostic(format_args!("iman: IGUI exited with {status}"));
            let result = control.end_gui(status);
            control.poll("gui_exit");
            return result;
        }
        control.gui_health()?;
        thread::sleep(Duration::from_millis(20));
    }
}

fn run_gui(
    root: &Path,
    state: &GuiState,
    notice: Option<String>,
    args: &[OsString],
    control: &mut Control,
    observer: &mut dyn FnMut(&'static str),
) -> io::Result<Option<Handoff>> {
    let held_root = control.prepare_root()?;
    let app = held_root.open_executable(&format!("{}/{}", paths::SYSTEM_DIR, paths::GUI_BINARY))?;
    let mut arguments = vec![c"igui".to_owned(), c"--managed".to_owned(), c"--ipc-socket".to_owned(),
        CString::new(control.path().as_os_str().as_encoded_bytes())?];
    for arg in args { arguments.push(CString::new(arg.as_encoded_bytes())?); }
    crate::runtime::executable::pinned_gui(app.into(), arguments, Some(root), state, notice, control, observer)
}

/// Process policy chosen by the launcher adapter, independent of CPU/display.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GameEnvironment {
    Inherited,
    /// A diagnostic parent has a bounded log and a small soft file-size limit.
    /// Games use bounded output capture and restore the inherited hard file limit,
    /// so explicit saves are not capped by the parent's logging policy.
    DiagnosticParent,
}

fn run_game(
    root: &Path,
    handoff: &Handoff,
    environment: GameEnvironment,
    monitor_index: Option<u32>,
    control: &mut Control,
    observer: &mut dyn FnMut(&'static str),
) -> io::Result<ExitStatus> {
    observer("game_start");
    control.poll("game_start");
    // Repeat preflight after the GUI has exited; never overlap the two frontends.
    #[cfg(feature = "diag-cycle")]
    if let Some(segment) = control.cycle_segment() {
        segment.game = Some(serde_json::json!({"request":handoff.game,"core":null,"emulator":null,"pid":null}));
    }
    let _held_root = control.prepare_root()?;
    let plan = launcher::resolve(root, &handoff.game)?;
    #[cfg(feature = "diag-cycle")]
    if let Some(segment) = control.cycle_segment() {
        if let Some(game) = &mut segment.game {
            game["core"] = serde_json::json!(plan.core);
            game["emulator"] = serde_json::json!(plan.retroarch);
        }
    }
    let video_context = plan.settings.video_context_driver();
    if monitor_index.is_some() && video_context != Some("kms") {
        return Err(io::Error::other("DRM session requires the RetroArch kms context"));
    }
    let runtime = runtime_directory(control.runtime_parent())?;
    let mut config = launcher::config(&plan, runtime.path())?;
    if let Some(index) = monitor_index {
        config.push_str(&format!("video_monitor_index = \"{index}\"\n"));
    }
    fs::write(runtime.path().join(paths::RETROARCH_CONFIG_FILE), config)?;
    plan.core_options.prepare(runtime.path())?;
    launcher::prepare_content(&plan, runtime.path())?;
    if let Some(remap) = &plan.remap {
        remap.prepare(runtime.path())?;
    }
    let mut command = launcher::command(&plan, runtime.path());
    command
        .stdin(Stdio::null())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());
    prepare_child(&mut command);
    let mut game_log = None;
    if environment == GameEnvironment::DiagnosticParent || cfg!(feature = "diag-cycle") {
        game_log = Some(crate::runtime::child_log::ChildLog::attach(&mut command, control.cycle_enabled())?);
    }
    if environment == GameEnvironment::DiagnosticParent {
        restore_file_limit(&mut command);
    }
    diagnostic(format_args!(
        "iman: starting RetroArch for {}",
        handoff.game.image
    ));
    let mut child = ChildGuard(command.spawn()?);
    drop(command);
    if let Some(log) = &mut game_log { log.observe(child.0.id(), control.diagnostic_clock(), "iman_session"); }
    control.set_game(Some(child.0.id()));
    #[cfg(feature = "diag-cycle")]
    {
        let ms = control.elapsed_ms();
        if let Some(segment) = control.cycle_segment() {
            segment.game_result = Some(serde_json::json!({"pid":child.0.id(), "spawn_observed_ms":ms}));
            if let Some(game) = &mut segment.game { game["pid"] = serde_json::json!(child.0.id()); }
        }
    }
    #[cfg(feature = "diag-perf")]
    let mut performance =
        {
            // SAFETY: sysconf reads the process tick unit and has no side effects.
            let hz = unsafe { libc::sysconf(libc::_SC_CLK_TCK) };
            match crate::diagnostics::perf::Monitor::new(child.0.id(), u64::try_from(hz).unwrap_or(0)) {
                Ok(monitor) => Some((
                    std::time::Instant::now(),
                    control.elapsed_ms(),
                    crate::diagnostics::perf::Source::system(),
                    monitor,
                    child.0.id(),
                )),
                Err(e) => {
                    diagnostic(format_args!("IMAN perf unavailable: {e}"));
                    if let Some(segment) = control.cycle_segment() {
                        segment.performance = Some(serde_json::json!({"state":"unavailable","error":e.to_string()}));
                    }
                    None
                }
            }
        };

    let status = wait(
        &mut child.0,
        &mut |phase| {
            if let Some(log) = &mut game_log {
                log.poll();
            }
            #[cfg(feature = "diag-perf")]
            if let Some((started, _, source, monitor, pid)) = &mut performance {
                monitor.poll(
                    started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64,
                    || source.sample(*pid),
                );
            }
            control.poll(phase);
            observer(phase);
        },
        "game",
    );
    #[cfg(feature = "diag-cycle")]
    let exit_observed_ms = control.elapsed_ms();
    control.set_game(None);
    drop(child);
    #[cfg(feature = "diag-perf")]
    if let Some((_, begin_ms, _, monitor, _)) = performance {
        match monitor.finish_json(status.is_ok()) {
            Ok(json) => {
                diagnostic(format_args!("IMAN perf: {json}"));
                if let Some(segment) = control.cycle_segment() {
                    segment.performance = Some(serde_json::json!({"scope":"game", "clock":"game",
                        "session_begin_ms":begin_ms, "data":serde_json::from_str::<serde_json::Value>(&json)?}));
                }
            }
            Err(e) => {
                diagnostic(format_args!("IMAN perf report failed: {e}"));
                if let Some(segment) = control.cycle_segment() {
                    segment.performance = Some(serde_json::json!({"state":"failed","error":e.to_string()}));
                }
            }
        }
    }

    if let Some(log) = game_log {
        let (_bytes, _metadata) = log.report();
        diagnostic(format_args!("iman: RetroArch output captured={}, received={}, omitted={}",
            _metadata["head_bytes"], _metadata["received_bytes"], _metadata["omitted_bytes"]));
        if !_metadata["error"].is_null() {
            diagnostic(format_args!("iman: RetroArch output read failed: {}", _metadata["error"]));
        }
        // A cycle already carries retroarch.log. Minimal/failed-export sessions
        // retain bounded text in the manager log without duplicating cycle data.
        if !control.cycle_enabled() {
            diagnostic(format_args!("{}\niman: RetroArch output end", String::from_utf8_lossy(&_bytes)));
        }
        #[cfg(feature = "diag-cycle")]
        if let Some(segment) = control.cycle_segment() {
            segment.game_log = _bytes;
            segment.output = _metadata;
        }
    }
    #[cfg(feature = "diag-cycle")]
    if let Some(segment) = control.cycle_segment() {
        use std::os::unix::process::ExitStatusExt;
        let result = segment.game_result.get_or_insert_with(|| serde_json::json!({}));
        let status_fields = match &status {
            Ok(status) => serde_json::json!({"exit_code":status.code(),"signal":status.signal()}),
            Err(error) => serde_json::json!({"wait_error":error.to_string()}),
        };
        result.as_object_mut().unwrap().extend(status_fields.as_object().unwrap().clone());
        result["exit_observed_ms"] = serde_json::json!(exit_observed_ms);
    }
    observer("game_cleanup");
    control.poll("game_cleanup");
    // Explicit cleanup even after wait failure; retain both errors independently.
    let cleanup = runtime.close();
    #[cfg(feature = "diag-cycle")]
    if let Some(segment) = control.cycle_segment() {
        if let Some(result) = &mut segment.game_result {
            result["cleanup_error"] = serde_json::json!(cleanup.as_ref().err().map(ToString::to_string));
        }
    }
    let status = status?;
    cleanup?;
    observer("game_exit");
    control.poll("game_exit");
    Ok(status)
}

pub fn run(root: &Path, gui_args: &[OsString]) -> io::Result<()> {
    run_observed(root, gui_args, &mut |_| {})
}

/// Optional lifecycle observer; normal builds need no diagnostic implementation.
pub fn run_observed(
    root: &Path,
    gui_args: &[OsString],
    observer: &mut dyn FnMut(&'static str),
) -> io::Result<()> {
    let root = root.canonicalize()?;
    let mut control = Control::bind(&root, Path::new("/dev/shm"))?;
    control.start_diagnostics()?;
    run_controlled(&root, gui_args, &mut control, observer)
}

pub fn run_controlled(
    root: &Path,
    gui_args: &[OsString],
    control: &mut Control,
    observer: &mut dyn FnMut(&'static str),
) -> io::Result<()> {
    #[cfg(feature = "pc")]
    {
        let settings = if gui_args.iter().any(|arg| arg == "--headless") {
            crate::profile::Display::Headless
        } else {
            crate::profile::Display::Desktop
        };
        let mut display = crate::display::open_observed(
            &settings, None, control.display_diagnostics_requested(),
        )?;
        return run_controlled_with_display(root, gui_args, control, observer, display.as_mut());
    }
    #[cfg(not(feature = "pc"))]
    run_controlled_with_display(root, gui_args, control, observer, &mut crate::display::Unmanaged)
}

/// The ordinary guarded file/child path with a caller-owned presentation adapter.
pub fn run_controlled_with_display(
    root: &Path,
    gui_args: &[OsString],
    control: &mut Control,
    observer: &mut dyn FnMut(&'static str),
    display: &mut dyn crate::display::Lease,
) -> io::Result<()> {
    run_session_with_display(
        root,
        GameEnvironment::DiagnosticParent,
        control,
        observer,
        display,
        |state, notice, control, observer| {
            run_gui(root, state, notice, gui_args, control, observer)
        },
    )
}

pub fn run_configured(
    root: &Path,
    profile: &crate::profile::Profile,
    control: &mut Control,
    observer: &mut dyn FnMut(&'static str),
) -> io::Result<()> {
    let args = profile.gui_arguments();
    let configured = launcher::configured_video_context(root)?;
    let video_context = configured.as_ref().map(|context| context.as_deref());
    let mut display = crate::display::open_observed(
        profile.display(), video_context, control.display_diagnostics_requested(),
    )?;
    run_session_with_display(
        root,
        GameEnvironment::DiagnosticParent,
        control,
        observer,
        display.as_mut(),
        |state, notice, control, observer| run_gui(root, state, notice, &args, control, observer),
    )
}

/// Common GUI → game → GUI lifecycle, independent of the launching firmware.
/// The adapter supplies presentation/launch mechanics, not a game prohibition.
pub fn run_session(
    root: &Path,
    environment: GameEnvironment,
    control: &mut Control,
    observer: &mut dyn FnMut(&'static str),
    gui: impl FnMut(
        &GuiState,
        Option<String>,
        &mut Control,
        &mut dyn FnMut(&'static str),
    ) -> io::Result<Option<Handoff>>,
) -> io::Result<()> {
    run_session_with_display(
        root,
        environment,
        control,
        observer,
        &mut crate::display::Unmanaged,
        gui,
    )
}

/// Own the output for the whole session. The GUI adapter must reap its child
/// before returning, on errors too; run_game follows the same ChildGuard rule.
pub fn run_session_with_display(
    root: &Path,
    environment: GameEnvironment,
    control: &mut Control,
    observer: &mut dyn FnMut(&'static str),
    display: &mut dyn crate::display::Lease,
    gui: impl FnMut(
        &GuiState,
        Option<String>,
        &mut Control,
        &mut dyn FnMut(&'static str),
    ) -> io::Result<Option<Handoff>>,
) -> io::Result<()> {
    run_session_with_timing(
        root,
        environment,
        control,
        observer,
        display,
        &mut SystemTiming::default(),
        gui,
    )
}

/// Inject session timing without replacing file guards, IPC or child supervision.
pub fn run_session_with_timing(
    root: &Path,
    environment: GameEnvironment,
    control: &mut Control,
    observer: &mut dyn FnMut(&'static str),
    display: &mut dyn crate::display::Lease,
    timing: &mut dyn Timing,
    mut gui: impl FnMut(
        &GuiState,
        Option<String>,
        &mut Control,
        &mut dyn FnMut(&'static str),
    ) -> io::Result<Option<Handoff>>,
) -> io::Result<()> {
    control.record_manager_display("initializing", display.take_diagnostics());
    let mut display_available = true;
    #[allow(unused_mut)]
    let mut show_report_path = false;
    let result = (|| {
        let root = root.canonicalize()?;
        let mut state = GuiState::default();
        let mut notice = None;
        loop {
            display.observe_display(control.display_diagnostics_requested());
            display.starting_gui()?;
            #[cfg(feature = "pc")]
            display.poll_events()?;
            control.record_manager_display("starting_gui", display.take_diagnostics());
            display.observe_display(false);
            display_available = false;
            display.lend()?;
            let gui_result = gui(&state, notice.take(), control, observer);
            if let Err(error) = &gui_result {
                diagnostic(format_args!(
                    "iman: GUI failed before display reclaim: {error}"
                ));
                control.gui_failed();
                #[cfg(feature = "diag-cycle")]
                if let Some(segment) = control.cycle_segment() {
                    let result = segment.gui_result.get_or_insert_with(|| serde_json::json!({}));
                    result["launch_or_cleanup_error"] = serde_json::json!(error.to_string());
                }
            }
            control.poll("display_reclaim");
            display.reclaim()?;
            display_available = true;
            // Resume only the compiled manager collector after reclaiming the display.
            display.observe_display(control.display_diagnostics_requested());
            #[cfg(feature = "diag-input")]
            if gui_result.is_ok() && control.take_input_test_request() {
                show_report_path = true;
                let message = crate::input_test::run(&root, control, observer, display)?;
                control.record_manager_display("input_test", display.take_diagnostics());
                let visible = display.departure(&message)?;
                diagnostic_interval(control, observer, display, timing, visible, &message)?;
                departure_countdown(control, observer, display, timing, visible)?;
                return Ok(());
            }
            if let Err(error) = &gui_result {
                if error.kind() != io::ErrorKind::Interrupted {
                    let path = root.join(paths::SYSTEM_DIR).join(paths::GUI_BINARY);
                    let visible = display.gui_error(&path, error).map_err(|display_error| {
                        io::Error::new(
                            error.kind(),
                            format!("{error}; error screen: {display_error}"),
                        )
                    })?;
                    control.record_manager_display("gui_error", display.take_diagnostics());
                    diagnostic_interval(control, observer, display, timing, visible, "IGUI unavailable").map_err(|display_error| {
                        if display_error.kind() == io::ErrorKind::Interrupted { display_error }
                        else { io::Error::new(error.kind(), format!("{error}; diagnostic screen: {display_error}")) }
                    })?;
                    departure_countdown(control, observer, display, timing, visible).map_err(|display_error| {
                        if display_error.kind() == io::ErrorKind::Interrupted { display_error }
                        else { io::Error::new(error.kind(), format!("{error}; error countdown: {display_error}")) }
                    })?;
                }
            }
            let Some(handoff) = gui_result? else {
                return Ok(());
            };
            state = handoff.state.clone();
            #[cfg(feature = "diag-cycle")]
            if let Some(segment) = control.cycle_segment() {
                segment.game = Some(serde_json::json!({"request":handoff.game}));
            }
            display.observe_display(false);
            display_available = false;
            display.lend()?;
            let game_result = run_game(
                &root,
                &handoff,
                environment,
                display.monitor_index(),
                control,
                observer,
            );
            if let Err(error) = &game_result {
                diagnostic(format_args!(
                    "iman: game failed before display reclaim: {error}"
                ));
            }
            #[cfg(feature = "diag-cycle")]
            if let Some(segment) = control.cycle_segment() {
                if let Err(error) = &game_result {
                    let result = segment.game_result.get_or_insert_with(|| serde_json::json!({}));
                    result["launch_or_cleanup_error"] = serde_json::json!(error.to_string());
                }
            }
            control.poll("display_reclaim");
            display.reclaim()?;
            display_available = true;
            display.observe_display(control.display_diagnostics_requested());
            match game_result {
                Ok(status) if status.success() => {
                    diagnostic(format_args!("iman: game finished; returning to IGUI"))
                }
                Ok(status) => {
                    diagnostic(format_args!("iman: RetroArch exited with {status}"));
                    let details = launcher::failure_details(&root, &handoff.game, format_args!("Emulator status: {status}"));
                    notice = Some(format!(
                        "Game session failed.\n{details}\nCheck ROM, core and BIOS."
                    ));
                }
                Err(error) if error.kind() == io::ErrorKind::Interrupted => return Err(error),
                Err(error) => {
                    diagnostic(format_args!("iman: {error}"));
                    let details = launcher::failure_details(&root, &handoff.game, error);
                    notice = Some(format!("Cannot launch game.\n{details}"));
                }
            }
            let settle = control.settling_interval();
            if !settle.is_zero() {
                let began = timing.now();
                while timing.now().saturating_sub(began) < settle {
                    observer("post_game");
                    control.poll("post_game");
                    if INTERRUPTED.load(Ordering::Relaxed) {
                        return Err(io::Error::new(io::ErrorKind::Interrupted, "session cancelled"));
                    }
                    timing.wait(Duration::from_millis(20));
                }
            }
            if control.cycle_enabled() {
                control.record_manager_display("post_game", display.take_diagnostics());
            }
            save_segment(control, display, timing, display_available, None, false)?;
            control.next_segment();
            if let Some(error) = control.cycle_error() {
                let diagnostic_notice = format!("Automatic diagnostic export disabled; some data may be unsaved. {error}");
                notice = Some(match notice { Some(old) => format!("{old}\n{diagnostic_notice}"), None => diagnostic_notice });
            }
        }
    })();
    let final_export = if result.as_ref().is_err_and(|e| e.kind() == io::ErrorKind::Interrupted) {
        Ok(())
    } else if control.cycle_enabled() {
        control.poll("session_end");
        control.record_manager_display("session_end", display.take_diagnostics());
        save_segment(control, display, timing, display_available, result.as_ref().err().map(ToString::to_string).as_deref(), show_report_path)
    } else {
        Ok(())
    };
    let cleanup = display.finish().and(final_export);
    control.record_manager_display("session_end", display.take_diagnostics());
    match (result, cleanup) {
        (Ok(()), result) => result,
        (Err(error), Ok(())) => Err(error),
        (Err(error), Err(cleanup)) => Err(io::Error::new(
            error.kind(),
            format!("{error}; display cleanup: {cleanup}"),
        )),
    }
}

/// Shared cancellation boundary for the input owner and diagnostic intervals.
pub(crate) fn check_interrupted() -> io::Result<()> {
    if INTERRUPTED.load(Ordering::Relaxed) {
        Err(io::Error::new(io::ErrorKind::Interrupted, "session cancelled"))
    } else { Ok(()) }
}

fn diagnostic_interval(
    control: &mut Control,
    observer: &mut dyn FnMut(&'static str),
    display: &mut dyn crate::display::Lease,
    timing: &mut dyn Timing,
    visible: bool,
    message: &str,
) -> io::Result<()> {
    if !control.diagnostics_requested() {
        return Ok(());
    }
    let began = timing.now();
    let mut shown = None;
    loop {
        #[cfg(feature = "pc")]
        display.poll_events()?;
        observer("gui_diagnostics");
        control.poll("gui_diagnostics");
        check_interrupted()?;
        let seconds = timing.now().saturating_sub(began).as_secs();
        if seconds >= 10 {
            break;
        }
        if shown != Some(seconds) {
            display.observe_display(control.display_diagnostics_requested());
            let mut status = control.diagnostics_status();
            if control.display_diagnostics_requested() {
                status.push(display.diagnostics_status().unwrap_or_else(|| {
                    "Display (iman): unavailable (no managed DRM)".into()
                }));
            }
            if visible {
                display.diagnostic_status(&format!("{message}. Diagnostics {seconds}/10 s\n{}", status.join("\n")), seconds)?;
            }
            shown = Some(seconds);
        }
        timing.wait(Duration::from_millis(20));
    }
    diagnostic(format_args!("iman: diagnostics complete: {}", control.diagnostics_status().join("; ")));
    control.record_manager_display("gui_diagnostics", display.take_diagnostics());
    if !control.cycle_configured() {
        control.stop_diagnostics();
        display.observe_display(false);
    }
    Ok(())
}

fn departure_countdown(
    control: &mut Control,
    observer: &mut dyn FnMut(&'static str),
    display: &mut dyn crate::display::Lease,
    timing: &mut dyn Timing,
    visible: bool,
) -> io::Result<()> {
    if !visible {
        return Ok(());
    }
    display.error_tick(10)?;
    let began = timing.now();
    let mut shown = Some(10);
    loop {
        #[cfg(feature = "pc")]
        display.poll_events()?;
        observer("gui_error");
        control.poll("gui_error");
        check_interrupted()?;
        let remaining = crate::display::leaving_seconds(timing.now().saturating_sub(began));
        let Some(seconds) = remaining else {
            break;
        };
        if remaining != shown {
            display.error_tick(seconds)?;
            shown = remaining;
        }
        timing.wait(Duration::from_millis(20));
    }
    Ok(())
}

/// The display may be unavailable after a failed reclaim; never draw on an unknown route.
fn save_segment(
    control: &mut Control,
    display: &mut dyn crate::display::Lease,
    timing: &mut dyn Timing,
    available: bool,
    error: Option<&str>,
    show_report_path: bool,
) -> io::Result<()> {
    if !control.cycle_enabled() { return Ok(()); }
    if INTERRUPTED.load(Ordering::Relaxed) {
        return Err(io::Error::new(io::ErrorKind::Interrupted, "session cancelled"));
    }
    let mut screen_error = None;
    let attempt = control.export_segment(error, || {
        if available {
            if let Err(error) = display.export_status("Saving diagnostics...", false) {
                screen_error = Some(error);
            }
        }
    });
    if matches!(attempt, Ok(false)) { return Ok(()); }
    if let Err(error) = &attempt { control.disable_cycle_exports(&error.to_string()); }
    while control.cycle_pending() {
        // Cancellation never abandons an accepted persistent write.
        control.poll("diagnostic_export");
        #[cfg(feature = "pc")]
        if available && screen_error.is_none() {
            if let Err(error) = display.poll_events() { screen_error = Some(error); }
        }
        timing.wait(Duration::from_millis(20));
    }
    let failed = control.cycle_error().is_some();
    let message = if failed { "Diagnostics NOT saved. Automatic export disabled." }
        else { "Diagnostics synchronized." };
    diagnostic(format_args!("iman: {message}"));
    if available && screen_error.is_none() {
        let receipt = if show_report_path && !failed {
            control.synchronized_report_path().map(Path::to_path_buf)
        } else { None };
        let presented = if let Some(path) = &receipt {
            diagnostic(format_args!("iman: report saved: {}", path.display()));
            display.report_saved(path)
        } else {
            display.export_status(message, failed)
        };
        match presented {
            Ok(true) => {
                let began = timing.now();
                while timing.now().saturating_sub(began) < Duration::from_secs(if receipt.is_some() { 5 } else { 1 }) {
                    control.poll("diagnostic_export");
                    #[cfg(feature = "pc")]
                    if let Err(error) = display.poll_events() { screen_error = Some(error); break; }
                    if INTERRUPTED.load(Ordering::Relaxed) { break; }
                    timing.wait(Duration::from_millis(20));
                }
            }
            Ok(false) => (),
            Err(error) => screen_error = Some(error),
        }
    }
    control.record_manager_display("diagnostic_export", display.take_diagnostics());
    if let Some(error) = screen_error { return Err(error); }
    if INTERRUPTED.load(Ordering::Relaxed) {
        return Err(io::Error::new(io::ErrorKind::Interrupted, "session cancelled"));
    }
    Ok(())
}
