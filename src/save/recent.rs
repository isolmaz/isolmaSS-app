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
            let mut cache = CACHE.lock().unwrap_or_else(|error| error.into_inner());
            if !CANCELLED.load(Ordering::Acquire) && cache.directory == directory {
                match result {
                    Ok(paths) => {
                        let mut merged: Vec<_> = cache
                            .saved
                            .iter()
                            .filter(|(revision, _)| *revision > generation)
                            .map(|(_, path)| path.clone())
                            .collect();
                        for path in paths {
                            if merged.len() < 5 && !merged.contains(&path) {
                                merged.push(path);
                            }
                        }
                        cache.paths = merged;
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                        cache.paths = cache
                            .saved
                            .iter()
                            .filter(|(revision, _)| *revision > generation)
                            .map(|(_, path)| path.clone())
                            .collect();
                    }
                    Err(error) => crate::diagnostics::record("recent captures", &error.to_string()),
                }
                cache.refreshed = Some(Instant::now());
            }
            crate::tray::notify_tray_wakeup();
        });
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
