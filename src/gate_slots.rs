//! Host-wide semaphore for concurrent hook gate runs.

use std::fs::{self, File, OpenOptions, TryLockError};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::thread;
use std::time::{Duration, Instant};

use tracing::warn;

use crate::error::{JjHooksError, Result};
use crate::jj::JjCli;

const POLL_INTERVAL: Duration = Duration::from_millis(500);
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(3600);

/// A host-wide gate slot. Dropping it releases the slot lock.
#[derive(Debug)]
pub struct GateSlot {
    _file: File,
}

#[derive(Debug)]
pub(crate) struct GateSlotConfig {
    pub(crate) dir: PathBuf,
    pub(crate) slots: usize,
    pub(crate) timeout: Duration,
}

static CONFIG: OnceLock<GateSlotConfig> = OnceLock::new();

/// Acquire one of `slots` exclusive locks, waiting up to `timeout` if needed.
/// `on_wait` runs once, when the first scan finds every slot occupied.
/// Returns `None` once `cancelled` reports true, without taking a slot.
pub fn acquire(
    dir: &Path,
    slots: usize,
    timeout: Duration,
    on_wait: impl FnOnce(usize),
    cancelled: impl Fn() -> bool,
) -> Result<Option<GateSlot>> {
    let slots = slots.max(1);
    let slot_paths: Vec<_> = (0..slots)
        .map(|index| dir.join(format!("slot-{index}")))
        .collect();
    fs::create_dir_all(dir)?;
    let started = Instant::now();
    let mut on_wait = Some(on_wait);

    loop {
        crate::interrupt::check()?;
        if cancelled() {
            return Ok(None);
        }
        for path in &slot_paths {
            let file = OpenOptions::new()
                .create(true)
                .truncate(false)
                .read(true)
                .write(true)
                .open(path)?;
            match file.try_lock() {
                Ok(()) => return Ok(Some(GateSlot { _file: file })),
                Err(TryLockError::WouldBlock) => {}
                Err(TryLockError::Error(error)) => return Err(error.into()),
            }
        }

        if let Some(callback) = on_wait.take() {
            callback(slots);
        }
        let elapsed = started.elapsed();
        if elapsed >= timeout {
            return Err(timeout_error(slots, elapsed));
        }
        thread::sleep(POLL_INTERVAL.min(timeout - elapsed));
        let elapsed = started.elapsed();
        if elapsed >= timeout {
            return Err(timeout_error(slots, elapsed));
        }
    }
}

fn timeout_error(slots: usize, waited: Duration) -> JjHooksError {
    JjHooksError::GateSlotTimeout {
        slots,
        waited_secs: waited
            .as_secs()
            .saturating_add(if waited.subsec_nanos() == 0 { 0 } else { 1 }),
    }
}

pub(crate) fn configured(jj: &JjCli) -> &'static GateSlotConfig {
    CONFIG.get_or_init(|| {
        let default_slots = (std::thread::available_parallelism()
            .map(usize::from)
            .unwrap_or(1)
            / 2)
        .max(1);
        let slots_env = std::env::var("JJ_HOOKS_GATE_SLOTS").ok();
        let slots_config = slots_env
            .is_none()
            .then(|| jj.run(&["config", "get", "jj-hooks.gate-slots"]).ok())
            .flatten();
        let slots = configured_slots(slots_env.as_deref(), slots_config.as_deref(), default_slots);

        let timeout_env = std::env::var("JJ_HOOKS_GATE_SLOT_TIMEOUT").ok();
        let timeout_config = timeout_env
            .is_none()
            .then(|| {
                jj.run(&["config", "get", "jj-hooks.gate-slot-timeout"])
                    .ok()
            })
            .flatten();
        let timeout = configured_timeout(timeout_env.as_deref(), timeout_config.as_deref());

        GateSlotConfig {
            dir: slot_dir(),
            slots,
            timeout,
        }
    })
}

fn configured_slots(env: Option<&str>, config: Option<&str>, default: usize) -> usize {
    let (source, value) = match env {
        Some(value) => ("JJ_HOOKS_GATE_SLOTS", Some(value)),
        None => ("jj-hooks.gate-slots", config),
    };
    let Some(value) = value else {
        return default;
    };
    match value.trim().parse::<usize>() {
        Ok(slots) if slots > 0 => slots,
        _ => {
            warn!("jj-hp: invalid {source} value; using default of {default} gate slots");
            default
        }
    }
}

fn configured_timeout(env: Option<&str>, config: Option<&str>) -> Duration {
    let (source, value) = match env {
        Some(value) => ("JJ_HOOKS_GATE_SLOT_TIMEOUT", Some(value)),
        None => ("jj-hooks.gate-slot-timeout", config),
    };
    match value
        .map(str::trim)
        .and_then(|value| value.parse::<u64>().ok())
    {
        Some(seconds) => Duration::from_secs(seconds),
        None if value.is_some() => {
            warn!("jj-hp: invalid {source} value; using the 3600-second gate slot timeout");
            DEFAULT_TIMEOUT
        }
        None => DEFAULT_TIMEOUT,
    }
}

fn slot_dir() -> PathBuf {
    if let Some(path) = std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
    {
        return path.join("jj-hooks").join("slots");
    }
    if let Some(home) = std::env::home_dir() {
        return home
            .join(".local")
            .join("state")
            .join("jj-hooks")
            .join("slots");
    }
    std::env::temp_dir().join("jj-hooks-slots")
}

#[cfg(test)]
mod tests {
    use super::{acquire, configured_slots};
    use crate::error::JjHooksError;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, mpsc};
    use std::thread;
    use std::time::Duration;

    const NEVER: fn() -> bool = || false;

    #[test]
    fn waits_then_times_out_when_all_slots_are_busy() {
        let dir = tempfile::tempdir().unwrap();
        let first = acquire(dir.path(), 2, Duration::from_secs(1), |_| {}, NEVER).unwrap();
        let second = acquire(dir.path(), 2, Duration::from_secs(1), |_| {}, NEVER).unwrap();
        assert!(first.is_some() && second.is_some());
        let mut waits = Vec::new();
        let error = acquire(
            dir.path(),
            2,
            Duration::from_millis(300),
            |in_use| waits.push(in_use),
            NEVER,
        )
        .unwrap_err();

        assert!(matches!(
            error,
            JjHooksError::GateSlotTimeout { slots: 2, .. }
        ));
        assert_eq!(waits, [2]);
        drop((first, second));
    }

    #[test]
    fn pending_acquire_succeeds_after_a_slot_is_released() {
        let dir = tempfile::tempdir().unwrap();
        let held = acquire(dir.path(), 1, Duration::from_secs(5), |_| {}, NEVER).unwrap();
        let path = dir.path().to_path_buf();
        let (waiting_tx, waiting_rx) = mpsc::channel();
        let (acquired_tx, acquired_rx) = mpsc::channel();
        let worker = thread::spawn(move || {
            let result = acquire(
                &path,
                1,
                Duration::from_secs(5),
                |_| waiting_tx.send(()).unwrap(),
                NEVER,
            );
            acquired_tx.send(matches!(result, Ok(Some(_)))).unwrap();
            result
        });

        waiting_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        drop(held);
        assert!(acquired_rx.recv_timeout(Duration::from_secs(2)).unwrap());
        worker.join().unwrap().unwrap();
    }

    #[test]
    fn cancelled_waiter_returns_without_a_slot() {
        let dir = tempfile::tempdir().unwrap();
        let _held = acquire(dir.path(), 1, Duration::from_secs(5), |_| {}, NEVER).unwrap();
        let path = dir.path().to_path_buf();
        let cancel = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&cancel);
        let (waiting_tx, waiting_rx) = mpsc::channel();
        let (done_tx, done_rx) = mpsc::channel();
        thread::spawn(move || {
            let result = acquire(
                &path,
                1,
                Duration::from_secs(30),
                |_| waiting_tx.send(()).unwrap(),
                || flag.load(Ordering::SeqCst),
            );
            done_tx.send(matches!(result, Ok(None))).unwrap();
        });

        waiting_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        cancel.store(true, Ordering::SeqCst);
        assert!(done_rx.recv_timeout(Duration::from_secs(2)).unwrap());
    }
    #[test]
    fn slot_count_requires_a_positive_integer() {
        assert_eq!(configured_slots(Some("0"), None, 4), 4);
        assert_eq!(configured_slots(Some("abc"), None, 4), 4);
        assert_eq!(configured_slots(Some(""), None, 4), 4);
        assert_eq!(configured_slots(Some("3"), None, 4), 3);
    }
}
