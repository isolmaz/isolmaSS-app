use std::path::{Path, PathBuf};
use std::sync::{
    Mutex,
    atomic::{AtomicBool, Ordering},
};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

struct Cache {
    directory: PathBuf,
    paths: Vec<PathBuf>,
    refreshed: Option<Instant>,
    generation: u64,
    saved: Vec<(u64, PathBuf)>,
}
static CACHE: Mutex<Cache> = Mutex::new(Cache {
    directory: PathBuf::new(),
    paths: Vec::new(),
    refreshed: None,
    generation: 0,
    saved: Vec::new(),
});
static WORKER: Mutex<Option<JoinHandle<()>>> = Mutex::new(None);
static LAST_FOLDER: Mutex<Option<PathBuf>> = Mutex::new(None);
pub fn last_folder() -> Option<PathBuf> {
    LAST_FOLDER
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .clone()
}
static RUNNING: AtomicBool = AtomicBool::new(false);
static CANCELLED: AtomicBool = AtomicBool::new(false);

pub fn refresh(directory: &Path) {
    let mut cache = CACHE.lock().unwrap_or_else(|error| error.into_inner());
    if cache.directory != directory {
        cache.directory = directory.to_owned();
        cache.paths.clear();
        cache.saved.clear();
        cache.refreshed = None;
    }
    if cache
        .refreshed
        .is_some_and(|last| last.elapsed() < Duration::from_secs(30))
        || RUNNING.swap(true, Ordering::AcqRel)
    {
        return;
    }
    let generation = cache.generation;
    drop(cache);
    if let Some(previous) = WORKER
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .take()
        && previous.join().is_err()
    {
        crate::diagnostics::record(
            "recent captures",
            "The directory worker terminated unexpectedly.",
        );
    }
    CANCELLED.store(false, Ordering::Release);
    let directory = directory.to_owned();
    let spawned = std::thread::Builder::new()
        .name("recent-captures".to_string())
        .spawn(move || {
            // Release builds abort on panic (no recoverable freeze to guard there);
            // this only guarantees RUNNING clears on normal exit and on unwind in
            // panic=unwind builds, so refresh() can never wedge.
            struct ResetRunning;
            impl Drop for ResetRunning {
                fn drop(&mut self) {
                    RUNNING.store(false, Ordering::Release);
                }
            }
            let _reset = ResetRunning;
            let result = super::recent_screenshots(&directory, 5);
            finish_scan(&directory, generation, result);
            crate::tray::notify_tray_wakeup();
        });
    adopt_worker(spawned);
}

/// Stores the scan result unless the scan was cancelled or the folder changed since
/// it started. `generation` is the save counter when the scan started: captures saved
/// afterwards may be missing from `result`, so they are kept in front of it.
fn finish_scan(directory: &Path, generation: u64, result: std::io::Result<Vec<PathBuf>>) {
    let mut cache = CACHE.lock().unwrap_or_else(|error| error.into_inner());
    if CANCELLED.load(Ordering::Acquire) || cache.directory != directory {
        return;
    }
    match result {
        Ok(paths) => {
            let mut merged = saved_since(&cache, generation);
            for path in paths {
                if merged.len() < 5 && !merged.contains(&path) {
                    merged.push(path);
                }
            }
            cache.paths = merged;
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            cache.paths = saved_since(&cache, generation);
        }
        Err(error) => crate::diagnostics::record("recent captures", &error.to_string()),
    }
    cache.refreshed = Some(Instant::now());
}

/// The captures saved after `generation`, newest first.
fn saved_since(cache: &Cache, generation: u64) -> Vec<PathBuf> {
    cache
        .saved
        .iter()
        .filter(|(revision, _)| *revision > generation)
        .map(|(_, path)| path.clone())
        .collect()
}

/// Keeps the scan thread for `shutdown` and the next refresh. A thread that could not
/// be started releases the running flag so a later refresh can try again.
fn adopt_worker(spawned: std::io::Result<JoinHandle<()>>) {
    match spawned {
        Ok(handle) => {
            *WORKER.lock().unwrap_or_else(|error| error.into_inner()) = Some(handle);
        }
        Err(error) => {
            RUNNING.store(false, Ordering::Release);
            crate::diagnostics::record(
                "recent captures",
                &format!("Could not start the directory worker: {error}"),
            );
        }
    }
}

pub fn list() -> Vec<PathBuf> {
    CACHE
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .paths
        .clone()
}

pub fn saved(path: &Path) {
    *LAST_FOLDER
        .lock()
        .unwrap_or_else(|error| error.into_inner()) = path.parent().map(Path::to_owned);
    let mut cache = CACHE.lock().unwrap_or_else(|error| error.into_inner());
    if path.parent() == Some(cache.directory.as_path()) {
        cache.generation += 1;
        let generation = cache.generation;
        cache.saved.retain(|(_, previous)| previous != path);
        cache.saved.insert(0, (generation, path.to_owned()));
        cache.saved.truncate(5);
        cache.paths.retain(|existing| existing != path);
        cache.paths.insert(0, path.to_owned());
        cache.paths.truncate(5);
    }
}

pub fn cancelled() -> bool {
    CANCELLED.load(Ordering::Acquire)
}

pub fn shutdown() {
    CANCELLED.store(true, Ordering::Release);
    if let Some(worker) = WORKER
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .take()
    {
        // Stop a pending filesystem operation on a slow network folder.
        use std::os::windows::io::AsRawHandle;
        unsafe {
            let _ = windows::Win32::System::IO::CancelSynchronousIo(
                windows::Win32::Foundation::HANDLE(worker.as_raw_handle()),
            );
        }
        if worker.join().is_err() {
            crate::diagnostics::record(
                "recent captures",
                "The directory worker terminated unexpectedly.",
            );
        }
    }
}

/// Holds the module's process-wide state for one test and starts and ends it clean:
/// no worker, empty cache, no remembered folder, not cancelled.
#[cfg(test)]
pub(super) struct Isolated {
    _serial: std::sync::MutexGuard<'static, ()>,
}

#[cfg(test)]
pub(super) fn isolated() -> Isolated {
    static STATE: Mutex<()> = Mutex::new(());
    let guard = STATE.lock().unwrap_or_else(|error| error.into_inner());
    let isolated = Isolated { _serial: guard };
    isolated.reset();
    isolated
}

#[cfg(test)]
impl Isolated {
    fn reset(&self) {
        shutdown();
        *CACHE.lock().unwrap_or_else(|error| error.into_inner()) = Cache {
            directory: PathBuf::new(),
            paths: Vec::new(),
            refreshed: None,
            generation: 0,
            saved: Vec::new(),
        };
        *LAST_FOLDER
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = None;
        RUNNING.store(false, Ordering::Release);
        CANCELLED.store(false, Ordering::Release);
    }
}

#[cfg(test)]
impl Drop for Isolated {
    fn drop(&mut self) {
        self.reset();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::test_env::TempDir;
    use std::io::ErrorKind;
    use std::time::UNIX_EPOCH;

    /// Creates `name` in `directory`, `seconds` after a fixed instant, so the order of
    /// the fixtures does not depend on the clock or on how fast the test runs.
    fn touch(directory: &Path, name: &str, seconds: u64) -> PathBuf {
        let path = directory.join(name);
        std::fs::write(&path, b"image").unwrap();
        std::fs::OpenOptions::new()
            .write(true)
            .open(&path)
            .unwrap()
            .set_modified(UNIX_EPOCH + Duration::from_secs(1_700_000_000 + seconds))
            .unwrap();
        path
    }

    /// `count` images `p1.png` ... `p<count>.png`, each one second newer than the last.
    fn captures(directory: &Path, count: u64) -> Vec<PathBuf> {
        (1..=count)
            .map(|index| touch(directory, &format!("p{index}.png"), index))
            .collect()
    }

    /// Waits for the scan that `refresh` started.
    fn wait_for_scan() {
        let worker = WORKER
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .take()
            .expect("refresh started a scan");
        worker.join().expect("the scan finished normally");
        assert!(
            !RUNNING.load(Ordering::Acquire),
            "a finished scan is not running"
        );
    }

    fn has_worker() -> bool {
        WORKER
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .is_some()
    }

    fn newest_first(paths: &[PathBuf], indexes: &[usize]) -> Vec<PathBuf> {
        indexes
            .iter()
            .map(|index| paths[index - 1].clone())
            .collect()
    }

    fn generation() -> u64 {
        CACHE
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .generation
    }

    #[test]
    fn refresh_lists_the_five_newest_and_then_serves_the_cache() {
        let _state = isolated();
        let dir = TempDir::new("recent_refresh");
        let files = captures(dir.path(), 7);
        assert!(list().is_empty(), "nothing is listed before the first scan");

        refresh(dir.path());
        wait_for_scan();
        assert_eq!(list(), newest_first(&files, &[7, 6, 5, 4, 3]));

        // Within 30 seconds the cache is served; the folder is not scanned again.
        let newer = touch(dir.path(), "p8.png", 8);
        refresh(dir.path());
        assert!(!has_worker(), "no new scan starts while the cache is fresh");
        assert_eq!(list(), newest_first(&files, &[7, 6, 5, 4, 3]));

        // After 30 seconds it is.
        CACHE
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .refreshed = Instant::now().checked_sub(Duration::from_secs(31));
        refresh(dir.path());
        wait_for_scan();
        let mut expected = newest_first(&files, &[7, 6, 5, 4]);
        expected.insert(0, newer);
        assert_eq!(list(), expected);
    }

    #[test]
    fn another_folder_starts_a_fresh_listing() {
        let _state = isolated();
        let first = TempDir::new("recent_first");
        let second = TempDir::new("recent_second");
        captures(first.path(), 3);
        let others = captures(second.path(), 2);

        refresh(first.path());
        wait_for_scan();
        assert_eq!(list().len(), 3);
        saved(&first.path().join("kept.png"));

        refresh(second.path());
        wait_for_scan();
        assert_eq!(
            list(),
            newest_first(&others, &[2, 1]),
            "only the new folder"
        );
        let cache = CACHE.lock().unwrap_or_else(|error| error.into_inner());
        assert!(
            cache.saved.is_empty(),
            "saves of the old folder are forgotten"
        );
    }

    #[test]
    fn a_running_scan_is_not_started_twice() {
        let _state = isolated();
        let dir = TempDir::new("recent_running");
        captures(dir.path(), 2);
        RUNNING.store(true, Ordering::Release);
        refresh(dir.path());
        assert!(!has_worker());
        assert!(list().is_empty());
        assert!(
            CACHE
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .refreshed
                .is_none()
        );
    }

    #[test]
    fn refresh_reaps_a_worker_that_panicked() {
        let _state = isolated();
        let dir = TempDir::new("recent_panicked");
        let files = captures(dir.path(), 2);
        // `resume_unwind` ends the thread as a panic would, without the hook's noise.
        adopt_worker(Ok(std::thread::spawn(|| {
            std::panic::resume_unwind(Box::new("test worker"))
        })));
        refresh(dir.path());
        wait_for_scan();
        assert_eq!(list(), newest_first(&files, &[2, 1]));
    }

    #[test]
    fn captures_saved_during_a_scan_stay_in_front_of_its_result() {
        let _state = isolated();
        let dir = TempDir::new("recent_merge");
        let files = captures(dir.path(), 3);
        refresh(dir.path());
        wait_for_scan();

        // A save lands after a scan started at `started` but before it finished.
        let started = generation();
        let fresh = dir.path().join("fresh.png");
        saved(&fresh);
        assert_eq!(generation(), started + 1);
        let scanned = || {
            vec![
                files[2].clone(),
                fresh.clone(),
                files[1].clone(),
                files[0].clone(),
                dir.path().join("older1.png"),
                dir.path().join("older2.png"),
            ]
        };
        finish_scan(dir.path(), started, Ok(scanned()));
        assert_eq!(
            list(),
            [
                fresh.clone(),
                files[2].clone(),
                files[1].clone(),
                files[0].clone(),
                dir.path().join("older1.png"),
            ],
            "the fresh capture leads, appears once, and the list stops at five"
        );

        // A scan that started after the save already contains it (or deleted it).
        finish_scan(dir.path(), generation(), Ok(vec![files[2].clone()]));
        assert_eq!(list(), [files[2].clone()]);
    }

    #[test]
    fn a_missing_folder_keeps_only_captures_saved_since_the_scan_started() {
        let _state = isolated();
        let dir = TempDir::new("recent_gone");
        refresh(dir.path());
        wait_for_scan();
        let started = generation();
        let fresh = dir.path().join("fresh.png");
        saved(&fresh);

        let gone = || Err(std::io::Error::from(ErrorKind::NotFound));
        finish_scan(dir.path(), started, gone());
        assert_eq!(list(), [fresh]);
        finish_scan(dir.path(), generation(), gone());
        assert!(list().is_empty());
    }

    #[test]
    fn other_scan_errors_keep_the_list_but_end_the_wait() {
        let _state = isolated();
        let dir = TempDir::new("recent_error");
        let files = captures(dir.path(), 2);
        refresh(dir.path());
        wait_for_scan();
        let before = list();
        CACHE
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .refreshed = None;

        finish_scan(
            dir.path(),
            generation(),
            Err(std::io::Error::from(ErrorKind::PermissionDenied)),
        );
        assert_eq!(list(), before);
        assert_eq!(before, newest_first(&files, &[2, 1]));
        assert!(
            CACHE
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .refreshed
                .is_some(),
            "the next refresh waits instead of retrying at once"
        );
    }

    #[test]
    fn a_cancelled_or_outdated_scan_is_ignored() {
        let _state = isolated();
        let dir = TempDir::new("recent_stale");
        let other = TempDir::new("recent_other");
        let files = captures(dir.path(), 2);
        refresh(dir.path());
        wait_for_scan();
        let kept = list();
        assert_eq!(kept, newest_first(&files, &[2, 1]));

        finish_scan(
            other.path(),
            generation(),
            Ok(vec![other.path().join("x.png")]),
        );
        assert_eq!(list(), kept, "the folder changed since the scan started");

        CANCELLED.store(true, Ordering::Release);
        finish_scan(dir.path(), generation(), Ok(vec![dir.path().join("y.png")]));
        assert_eq!(list(), kept, "the application is shutting down");
    }

    #[test]
    fn saved_captures_lead_the_list_without_duplicates_and_stop_at_five() {
        let _state = isolated();
        let dir = TempDir::new("recent_saved");
        refresh(dir.path());
        wait_for_scan();

        let paths: Vec<PathBuf> = (1..=7)
            .map(|index| dir.path().join(format!("s{index}.png")))
            .collect();
        for path in &paths {
            saved(path);
        }
        assert_eq!(
            list(),
            newest_first(&paths, &[7, 6, 5, 4, 3]),
            "newest first, five at most"
        );
        saved(&paths[3]);
        assert_eq!(
            list(),
            newest_first(&paths, &[4, 7, 6, 5, 3]),
            "saving a path again moves it to the front"
        );
        {
            let cache = CACHE.lock().unwrap_or_else(|error| error.into_inner());
            assert_eq!(cache.saved.len(), 5);
            assert_eq!(cache.generation, 8);
        }
        assert_eq!(last_folder(), Some(dir.path().to_owned()));
    }

    #[test]
    fn captures_saved_elsewhere_only_update_the_remembered_folder() {
        let _state = isolated();
        let dir = TempDir::new("recent_watched");
        let elsewhere = TempDir::new("recent_elsewhere");
        let files = captures(dir.path(), 2);
        refresh(dir.path());
        wait_for_scan();

        saved(&elsewhere.path().join("away.png"));
        assert_eq!(
            list(),
            newest_first(&files, &[2, 1]),
            "the watched list is unchanged"
        );
        assert_eq!(generation(), 0);
        assert_eq!(last_folder(), Some(elsewhere.path().to_owned()));
    }

    #[test]
    fn shutdown_stops_the_scan_thread_and_marks_the_application_as_closing() {
        let _state = isolated();
        let dir = TempDir::new("recent_shutdown");
        captures(dir.path(), 3);
        refresh(dir.path());
        shutdown();
        assert!(cancelled());
        assert!(!has_worker(), "the thread was joined");
        assert!(!RUNNING.load(Ordering::Acquire), "and has finished");
    }

    #[test]
    fn shutdown_survives_a_worker_that_panicked_and_one_that_never_existed() {
        let _state = isolated();
        shutdown();
        assert!(cancelled(), "nothing to stop is fine");

        adopt_worker(Ok(std::thread::spawn(|| {
            std::panic::resume_unwind(Box::new("test worker"))
        })));
        shutdown();
        assert!(!has_worker());
    }

    #[test]
    fn a_worker_that_cannot_start_releases_the_running_flag() {
        let _state = isolated();
        RUNNING.store(true, Ordering::Release);
        adopt_worker(Err(std::io::Error::from(ErrorKind::OutOfMemory)));
        assert!(!RUNNING.load(Ordering::Acquire));
        assert!(!has_worker());

        adopt_worker(Ok(std::thread::spawn(|| {})));
        assert!(has_worker(), "a started thread is kept for shutdown");
        wait_for_scan();
    }
}
