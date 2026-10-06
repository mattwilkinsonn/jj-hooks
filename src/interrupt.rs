//! Process-global signal handling for hook worktrees.
//!
//! While a [`LiveWorktree`] exists, SIGINT, SIGTERM and SIGHUP become
//! [`JjHooksError::Interrupted`] so `Worktree`'s Drop removes the checkout.
//! Children spawned through [`status`], [`output`] or [`cleanup_output`] are
//! tracked so the watcher can forward the signal, escalating on repeats.

use std::process::{Command, ExitCode, ExitStatus, Output};

use crate::error::{JjHooksError, Result};

#[cfg(unix)]
use std::io::Read;
#[cfg(unix)]
use std::os::unix::process::{CommandExt, ExitStatusExt};
#[cfg(unix)]
use std::process::{Child, Stdio};
#[cfg(unix)]
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
#[cfg(unix)]
use std::sync::{Arc, LazyLock, Mutex, MutexGuard, Once, PoisonError, mpsc};
#[cfg(unix)]
use std::time::{Duration, Instant};

#[cfg(unix)]
use rustix::process::{Pid, Signal};
#[cfg(unix)]
use signal_hook::consts::signal::{SIGHUP, SIGINT, SIGTERM};

/// The first watched signal received; set inside the signal handler, never cleared.
#[cfg(unix)]
static PENDING: LazyLock<Arc<AtomicUsize>> = LazyLock::new(Default::default);
#[cfg(unix)]
static STATE: Mutex<State> = Mutex::new(State::new());
/// Spawn children in their own process group (no controlling terminal).
#[cfg(unix)]
static GROUP: AtomicBool = AtomicBool::new(false);
/// Bit `sig - 1` is set for each watched signal.
#[cfg(unix)]
static WATCHED: AtomicU64 = AtomicU64::new(0);
#[cfg(unix)]
static INSTALL: Once = Once::new();

#[cfg(unix)]
const READER_POLL: Duration = Duration::from_millis(100);
#[cfg(unix)]
const READER_GRACE: Duration = Duration::from_secs(1);

#[cfg(unix)]
struct State {
    live: usize,
    hits: u32,
    children: Vec<Tracked>,
}

#[cfg(unix)]
impl State {
    const fn new() -> Self {
        Self {
            live: 0,
            hits: 0,
            children: Vec::new(),
        }
    }
}

#[cfg(unix)]
#[derive(Debug, Clone, Copy)]
struct Tracked {
    pid: Pid,
    group: bool,
}

#[cfg(unix)]
#[derive(Debug, PartialEq, Eq)]
enum Action {
    Default,
    Send(Signal),
}

/// Pure watcher step: with no live worktree the signal acts as if unhandled.
#[cfg(unix)]
fn divert(state: &mut State, _sig: i32) -> Action {
    if state.live == 0 {
        return Action::Default;
    }
    state.hits = state.hits.saturating_add(1);
    match state.hits {
        1 => Action::Send(Signal::TERM),
        2 => Action::Send(Signal::KILL),
        _ => Action::Default,
    }
}

#[cfg(unix)]
fn lock() -> MutexGuard<'static, State> {
    STATE.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Install signal handling for this process. Idempotent.
///
/// The pending signal is sticky, so a long-lived host must not call this:
/// after one interrupt every later spawn would be refused.
pub fn install() {
    #[cfg(unix)]
    INSTALL.call_once(install_handlers);
}

#[cfg(unix)]
fn install_handlers() {
    let ignored = inherited_ignored();
    GROUP.store(std::fs::File::open("/dev/tty").is_err(), Ordering::SeqCst);

    let mut watched = Vec::new();
    for sig in candidate_signals() {
        if ignored & signal_bit(sig) != 0 {
            continue;
        }
        let Ok(value) = usize::try_from(sig) else {
            continue;
        };
        match signal_hook::flag::register_usize(sig, Arc::clone(&PENDING), value) {
            Ok(_) => watched.push(sig),
            Err(error) => tracing::warn!("failed to watch signal {sig}: {error}"),
        }
    }
    let mut signals = match signal_hook::iterator::Signals::new(&watched) {
        Ok(signals) => signals,
        Err(error) => {
            tracing::warn!("failed to start the signal watcher: {error}");
            return;
        }
    };
    let mask = watched.iter().fold(0, |mask, &sig| mask | signal_bit(sig));
    WATCHED.store(mask, Ordering::SeqCst);
    let spawned = std::thread::Builder::new()
        .name("jj-hp-signals".into())
        .spawn(move || {
            for sig in signals.forever() {
                watch(sig);
            }
        });
    if let Err(error) = spawned {
        tracing::warn!("failed to start the signal watcher: {error}");
    }
}

#[cfg(unix)]
fn watch(sig: i32) {
    let mut state = lock();
    match divert(&mut state, sig) {
        Action::Default => {
            // Returns only when the default action does not terminate.
            if let Err(error) = signal_hook::low_level::emulate_default_handler(sig) {
                tracing::warn!("failed to apply the default action for signal {sig}: {error}");
            }
        }
        Action::Send(signal) => {
            for child in &state.children {
                // A registered PID is our live child or zombie, so it is never reused.
                if let Err(error) = send(*child, signal) {
                    tracing::debug!("failed to signal child {:?}: {error}", child.pid);
                }
            }
        }
    }
}

#[cfg(unix)]
fn send(child: Tracked, signal: Signal) -> rustix::io::Result<()> {
    if child.group {
        rustix::process::kill_process_group(child.pid, signal)
    } else {
        rustix::process::kill_process(child.pid, signal)
    }
}

#[cfg(target_os = "linux")]
fn candidate_signals() -> [i32; 3] {
    [SIGINT, SIGTERM, SIGHUP]
}

#[cfg(all(unix, not(target_os = "linux")))]
fn candidate_signals() -> [i32; 2] {
    [SIGINT, SIGTERM]
}

/// The `SigIgn` mask inherited from our parent, so `nohup` keeps working.
#[cfg(target_os = "linux")]
fn inherited_ignored() -> u64 {
    let status = match std::fs::read_to_string("/proc/self/status") {
        Ok(status) => status,
        Err(error) => {
            tracing::warn!("failed to read inherited signal dispositions: {error}");
            return 0;
        }
    };
    status
        .lines()
        .find_map(|line| line.strip_prefix("SigIgn:"))
        .and_then(|mask| u64::from_str_radix(mask.trim(), 16).ok())
        .unwrap_or(0)
}

#[cfg(all(unix, not(target_os = "linux")))]
fn inherited_ignored() -> u64 {
    0
}

#[cfg(unix)]
fn signal_bit(sig: i32) -> u64 {
    u32::try_from(sig - 1)
        .ok()
        .and_then(|shift| 1u64.checked_shl(shift))
        .unwrap_or(0)
}

/// The pending signal, if one interrupted this process.
pub fn signal() -> Option<i32> {
    #[cfg(unix)]
    {
        match PENDING.load(Ordering::SeqCst) {
            0 => None,
            sig => i32::try_from(sig).ok(),
        }
    }
    #[cfg(not(unix))]
    {
        None
    }
}

/// Fail with [`JjHooksError::Interrupted`] once a signal is pending.
pub fn check() -> Result<()> {
    match signal() {
        Some(signal) => Err(JjHooksError::Interrupted { signal }),
        None => Ok(()),
    }
}

/// Die by `signal` as if it had never been handled; else exit `128 + signal`.
pub fn reraise(signal: i32) -> ExitCode {
    #[cfg(unix)]
    if let Err(error) = signal_hook::low_level::emulate_default_handler(signal) {
        tracing::warn!("failed to re-raise signal {signal}: {error}");
    }
    ExitCode::from(u8::try_from(128 + signal).unwrap_or(u8::MAX))
}

/// Counts a live hook worktree, so signals are diverted instead of fatal.
pub(crate) struct LiveWorktree(());

impl LiveWorktree {
    pub(crate) fn new() -> Self {
        #[cfg(unix)]
        {
            let mut state = lock();
            state.live = state.live.saturating_add(1);
        }
        Self(())
    }
}

impl Drop for LiveWorktree {
    fn drop(&mut self) {
        #[cfg(unix)]
        {
            let mut state = lock();
            state.live = state.live.saturating_sub(1);
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    Status,
    Output,
    Cleanup,
}

impl Mode {
    #[cfg(unix)]
    fn captures(self) -> bool {
        self != Mode::Status
    }

    #[cfg(unix)]
    fn checks(self) -> bool {
        self != Mode::Cleanup
    }
}

/// Run `cmd` with inherited stdio; refuses to spawn once interrupted.
pub(crate) fn status(cmd: &mut Command) -> Result<ExitStatus> {
    run(cmd, Mode::Status).map(|output| output.status)
}

/// Run `cmd` with stdin null and piped output; refuses to spawn once interrupted.
pub(crate) fn output(cmd: &mut Command) -> Result<Output> {
    run(cmd, Mode::Output)
}

/// [`output`] that still runs after an interrupt, for worktree cleanup.
pub(crate) fn cleanup_output(cmd: &mut Command) -> Result<Output> {
    run(cmd, Mode::Cleanup)
}

#[cfg(not(unix))]
fn run(cmd: &mut Command, mode: Mode) -> Result<Output> {
    if mode == Mode::Status {
        let status = cmd.status()?;
        return Ok(Output {
            status,
            stdout: Vec::new(),
            stderr: Vec::new(),
        });
    }
    cmd.stdin(std::process::Stdio::null());
    Ok(cmd.output()?)
}

#[cfg(unix)]
fn run(cmd: &mut Command, mode: Mode) -> Result<Output> {
    let spawned = spawn(cmd, mode)?;
    complete(spawned, mode)
}

#[cfg(unix)]
type ReaderMessage = (bool, std::io::Result<Vec<u8>>);

#[cfg(unix)]
struct Spawned {
    child: Child,
    pid: Pid,
    group: bool,
    readers: Option<mpsc::Receiver<ReaderMessage>>,
}

/// Spawn and register under `STATE`, so the child sees every later escalation.
#[cfg(unix)]
fn spawn(cmd: &mut Command, mode: Mode) -> Result<Spawned> {
    let group = GROUP.load(Ordering::SeqCst);
    if group {
        cmd.process_group(0);
    }
    if mode.captures() {
        cmd.stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
    }
    let mut state = lock();
    if mode.checks() {
        check()?;
    }
    let mut child = cmd.spawn()?;
    let pid = Pid::from_child(&child);
    state.children.push(Tracked { pid, group });
    drop(state);

    let readers = if mode.captures() {
        match start_readers(&mut child) {
            Ok(readers) => Some(readers),
            Err(error) => {
                abandon(child, pid);
                return Err(error.into());
            }
        }
    } else {
        None
    };
    Ok(Spawned {
        child,
        pid,
        group,
        readers,
    })
}

#[cfg(unix)]
fn start_readers(child: &mut Child) -> std::io::Result<mpsc::Receiver<ReaderMessage>> {
    let (tx, rx) = mpsc::channel();
    if let Some(stdout) = child.stdout.take() {
        read_in_background(stdout, true, tx.clone())?;
    }
    if let Some(stderr) = child.stderr.take() {
        read_in_background(stderr, false, tx)?;
    }
    Ok(rx)
}

#[cfg(unix)]
fn read_in_background(
    mut pipe: impl Read + Send + 'static,
    is_stdout: bool,
    tx: mpsc::Sender<ReaderMessage>,
) -> std::io::Result<()> {
    std::thread::Builder::new()
        .name("jj-hp-pipe".into())
        .spawn(move || {
            let mut buf = Vec::new();
            let read = pipe.read_to_end(&mut buf).map(|_| buf);
            // The receiver is gone only when the run detached this reader.
            if tx.send((is_stdout, read)).is_err() {
                tracing::debug!("detached pipe reader finished");
            }
        })?;
    Ok(())
}

/// Kill, deregister and reap a child whose run cannot continue.
#[cfg(unix)]
fn abandon(mut child: Child, pid: Pid) {
    if let Err(error) = rustix::process::kill_process(pid, Signal::KILL) {
        tracing::debug!("failed to kill abandoned child {pid:?}: {error}");
    }
    deregister(pid);
    if let Err(error) = child.wait() {
        tracing::warn!("failed to reap abandoned child {pid:?}: {error}");
    }
}

#[cfg(unix)]
fn deregister(pid: Pid) {
    lock().children.retain(|tracked| tracked.pid != pid);
}

/// Wait for exit without reaping, so the registered PID stays ours.
#[cfg(unix)]
fn wait_exited(pid: Pid) -> std::io::Result<()> {
    use rustix::process::{WaitId, WaitIdOptions, waitid};
    loop {
        match waitid(
            WaitId::Pid(pid),
            WaitIdOptions::EXITED | WaitIdOptions::NOWAIT,
        ) {
            Ok(_) => return Ok(()),
            Err(rustix::io::Errno::INTR) => continue,
            Err(error) => return Err(error.into()),
        }
    }
}

#[cfg(unix)]
fn complete(spawned: Spawned, mode: Mode) -> Result<Output> {
    let Spawned {
        mut child,
        pid,
        group,
        readers,
    } = spawned;
    let waited = wait_exited(pid);

    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let mut read_error = None;
    let mut outstanding = if readers.is_some() { 2 } else { 0 };
    let mut take = |(is_stdout, read): ReaderMessage| match read {
        Ok(buf) if is_stdout => stdout = buf,
        Ok(buf) => stderr = buf,
        Err(error) => read_error = Some(error),
    };

    let mut interrupted = false;
    if let Some(rx) = &readers {
        // A grandchild holding the pipe must not block an interrupt.
        while outstanding > 0 {
            match rx.recv_timeout(READER_POLL) {
                Ok(message) => {
                    outstanding -= 1;
                    take(message);
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    if mode.checks() && check().is_err() {
                        interrupted = true;
                        break;
                    }
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            }
        }
    }
    interrupted |= mode.checks() && check().is_err();

    if interrupted {
        // The unreaped leader pins the group ID, so this cannot hit a stranger.
        if group && let Err(error) = rustix::process::kill_process_group(pid, Signal::KILL) {
            tracing::debug!("failed to kill process group {pid:?}: {error}");
        }
        if let Some(rx) = &readers {
            let deadline = Instant::now() + READER_GRACE;
            while outstanding > 0 {
                let left = deadline.saturating_duration_since(Instant::now());
                match rx.recv_timeout(left) {
                    Ok(message) => {
                        outstanding -= 1;
                        take(message);
                    }
                    // Still blocked: dropping the receiver detaches the reader.
                    Err(_) => break,
                }
            }
        }
    }

    deregister(pid);
    let status = child.wait()?;
    waited?;

    if !group
        && let Some(sig) = status.signal()
        && (sig == SIGINT || sig == SIGHUP)
        && WATCHED.load(Ordering::SeqCst) & signal_bit(sig) != 0
        && let Ok(value) = usize::try_from(sig)
    {
        // The terminal sent it to jj-hp too; an earlier signal keeps priority.
        let _ = PENDING.compare_exchange(0, value, Ordering::SeqCst, Ordering::SeqCst);
    }
    if mode.checks() {
        check()?;
    }
    if let Some(error) = read_error {
        return Err(error.into());
    }
    Ok(Output {
        status,
        stdout,
        stderr,
    })
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    fn with_live(live: usize, hits: u32) -> State {
        State {
            live,
            hits,
            children: Vec::new(),
        }
    }

    #[test]
    fn divert_without_live_worktree_acts_as_unhandled() {
        let mut state = with_live(0, 0);
        assert_eq!(divert(&mut state, SIGTERM), Action::Default);
        assert_eq!(divert(&mut state, SIGINT), Action::Default);
        assert_eq!(state.hits, 0);
    }

    #[test]
    fn divert_escalates_term_then_kill_then_default() {
        let mut state = with_live(1, 0);
        assert_eq!(divert(&mut state, SIGINT), Action::Send(Signal::TERM));
        assert_eq!(divert(&mut state, SIGINT), Action::Send(Signal::KILL));
        assert_eq!(divert(&mut state, SIGINT), Action::Default);
        assert_eq!(state.hits, 3);
    }

    // Sets the sticky PENDING flag; relies on nextest's process-per-test.
    #[test]
    fn status_refuses_to_spawn_once_interrupted() {
        let tmp = tempfile::TempDir::new().unwrap();
        let marker = tmp.path().join("spawned");
        PENDING.store(15, Ordering::SeqCst);
        let err = status(Command::new("touch").arg(&marker)).unwrap_err();
        assert!(
            matches!(err, JjHooksError::Interrupted { signal: 15 }),
            "{err:?}"
        );
        assert!(!marker.exists(), "the child was spawned");
        assert!(lock().children.is_empty());
    }

    #[test]
    fn exited_child_stays_signalable_until_reaped() {
        let spawned = spawn(Command::new("true").arg("x"), Mode::Status).unwrap();
        let pid = spawned.pid;
        wait_exited(pid).unwrap();
        assert!(rustix::process::test_kill_process(pid).is_ok());
        assert!(lock().children.iter().any(|tracked| tracked.pid == pid));

        let output = complete(spawned, Mode::Status).unwrap();
        assert!(output.status.success());
        assert!(lock().children.is_empty());
    }
}
