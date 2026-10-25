//! Dragging the uploaded image out of the upload card into another app: the temporary
//! copy the drop target receives, its cleanup, and the bytes of the two clipboard formats
//! the drag offers (the image file and the share link). The OLE data object that serves
//! them to the drop target lives in `ole.rs`.
use std::io;
use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, SystemTime};
use windows::Win32::Foundation::{DRAGDROP_S_DROP, SYSTEMTIME};
use windows::Win32::System::SystemInformation::GetLocalTime;
use windows::core::HRESULT;

mod ole;
pub use ole::run;

/// `CF_HDROP`: a list of files.
const CF_HDROP: u16 = 15;
/// `CF_UNICODETEXT`: UTF-16 text.
const CF_UNICODETEXT: u16 = 13;
/// Size of the `DROPFILES` header that starts an `HDROP` buffer.
const DROPFILES_LEN: usize = 20;
/// How many `-2`, `-3`, … names are tried when the plain name is taken.
const NAME_TRIES: u32 = 99;
/// How long a dragged copy stays in the temporary folder if nothing else removed it.
const KEEP: Duration = Duration::from_secs(24 * 60 * 60);
/// How long after a drop its card's copy is kept: chat apps and browsers may read the file
/// only after `DoDragDrop` has returned.
const GRACE: Duration = Duration::from_secs(10);

/// The deletions [`discard`] has started and that have not been waited for.
static REMOVALS: Mutex<Vec<JoinHandle<()>>> = Mutex::new(Vec::new());

/// An `HDROP` buffer for one file: the `DROPFILES` header (offset of the list, drop point,
/// non-client flag, wide-characters flag), the UTF-16 path and the double NUL that ends
/// the list.
fn hdrop_bytes(path: &Path) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(DROPFILES_LEN + 2 * (path.as_os_str().len() + 2));
    bytes.extend_from_slice(&(DROPFILES_LEN as u32).to_le_bytes());
    bytes.extend_from_slice(&[0; 8]);
    bytes.extend_from_slice(&0u32.to_le_bytes());
    bytes.extend_from_slice(&1u32.to_le_bytes());
    for unit in path.as_os_str().encode_wide().chain([0, 0]) {
        bytes.extend_from_slice(&unit.to_le_bytes());
    }
    bytes
}

/// A NUL-terminated UTF-16 string, the memory of a `CF_UNICODETEXT` item.
fn unicode_text_bytes(text: &str) -> Vec<u8> {
    text.encode_utf16()
        .chain(Some(0))
        .flat_map(u16::to_le_bytes)
        .collect()
}

/// What a drag from the upload card offers the drop target: the image as a file when a
/// copy of it could be made, and the share link as text.
pub struct Payload {
    files: Option<Vec<u8>>,
    text: Vec<u8>,
}

impl Payload {
    pub fn new(file: Option<&Path>, link: &str) -> Self {
        Self {
            files: file.map(hdrop_bytes),
            text: unicode_text_bytes(link),
        }
    }

    /// The clipboard formats on offer, the file first so that targets taking either use it.
    pub fn formats(&self) -> Vec<u16> {
        let mut formats = Vec::with_capacity(2);
        if self.files.is_some() {
            formats.push(CF_HDROP);
        }
        formats.push(CF_UNICODETEXT);
        formats
    }

    /// The memory content of `format`, `None` when it is not on offer.
    pub fn data(&self, format: u16) -> Option<&[u8]> {
        match format {
            CF_HDROP => self.files.as_deref(),
            CF_UNICODETEXT => Some(&self.text),
            _ => None,
        }
    }
}

/// `isolmaSS-YYYYMMDD-HHMMSS` for the given local time: the name of a dragged file
/// without its extension.
fn file_stem(time: &SYSTEMTIME) -> String {
    format!(
        "isolmaSS-{:04}{:02}{:02}-{:02}{:02}{:02}",
        time.wYear, time.wMonth, time.wDay, time.wHour, time.wMinute, time.wSecond
    )
}

/// [`file_stem`] for now.
pub fn file_stem_now() -> String {
    file_stem(&unsafe { GetLocalTime() })
}

/// `%TEMP%\isolmaSS\drag`, where the dragged copies live.
pub fn directory() -> PathBuf {
    std::env::temp_dir().join("isolmaSS").join("drag")
}

/// Copies `source`, the uploaded file, into `directory` as `<stem>.<extension>`, or
/// `<stem>-2.<extension>`, `<stem>-3.<extension>`, … when an earlier card took that name.
/// The bytes are copied from the file on disk, never held in memory.
pub fn stage(source: &Path, directory: &Path, stem: &str, extension: &str) -> io::Result<PathBuf> {
    std::fs::create_dir_all(directory)?;
    let path = (1..=NAME_TRIES)
        .map(|attempt| match attempt {
            1 => directory.join(format!("{stem}.{extension}")),
            _ => directory.join(format!("{stem}-{attempt}.{extension}")),
        })
        .find(|path| !path.exists())
        .ok_or_else(|| io::Error::from(io::ErrorKind::AlreadyExists))?;
    std::fs::copy(source, &path)?;
    Ok(path)
}

/// The copy for one upload card: the one an earlier drag of the same card made while it is
/// still there, else a new one that is remembered in `cached`.
pub fn staged_copy(
    cached: &mut Option<PathBuf>,
    source: &Path,
    directory: &Path,
    stem: &str,
    extension: &str,
) -> io::Result<PathBuf> {
    if let Some(path) = cached.as_ref().filter(|path| path.is_file()) {
        return Ok(path.clone());
    }
    let path = stage(source, directory, stem, extension)?;
    *cached = Some(path.clone());
    Ok(path)
}

/// Whether `entry` is a file last modified more than `max_age` before `now`.
fn is_stale(entry: &std::fs::DirEntry, now: SystemTime, max_age: Duration) -> bool {
    entry
        .metadata()
        .ok()
        .filter(std::fs::Metadata::is_file)
        .and_then(|metadata| metadata.modified().ok())
        .and_then(|modified| now.duration_since(modified).ok())
        .is_some_and(|age| age > max_age)
}

/// Deletes the files in `directory` last modified more than `max_age` before `now`;
/// folders and anything that cannot be deleted stay. Returns how many files went.
fn remove_older_than(directory: &Path, now: SystemTime, max_age: Duration) -> usize {
    let Ok(entries) = std::fs::read_dir(directory) else {
        return 0;
    };
    entries
        .flatten()
        .filter(|entry| is_stale(entry, now, max_age))
        .filter(|entry| std::fs::remove_file(entry.path()).is_ok())
        .count()
}

/// Removes the dragged copies that are a day old, whichever card or session made them, and
/// the encoded uploads of the same age that a card could not delete because the app ended.
pub fn sweep() -> usize {
    let now = SystemTime::now();
    remove_older_than(&directory(), now, KEEP)
        + remove_older_than(&crate::upload::staging_directory(), now, KEEP)
}

/// [`sweep`] on a background thread, for the UI thread; the thread returns the count.
pub fn sweep_in_background() -> JoinHandle<usize> {
    std::thread::spawn(sweep)
}

/// Whether `DoDragDrop` ended with a drop that the target accepted, as opposed to Esc, a
/// release over nothing or a target that refused the copy.
fn dropped(outcome: HRESULT, effect: u32) -> bool {
    outcome == DRAGDROP_S_DROP && effect != 0
}

/// What is left of the grace period when the card closes `elapsed` after its last accepted
/// drop; none when nothing was dropped.
pub fn grace_left(elapsed: Option<Duration>) -> Duration {
    elapsed.map_or(Duration::ZERO, |elapsed| GRACE.saturating_sub(elapsed))
}

/// Deletes a file; one that is already gone is fine, any other failure is recorded.
fn remove_quietly(path: &Path) {
    if let Err(error) = std::fs::remove_file(path)
        && error.kind() != io::ErrorKind::NotFound
    {
        crate::diagnostics::record("drag cleanup", &error.to_string());
    }
}

/// Deletes the dragged copy `path` after `wait`, on a background thread: right away
/// (`Duration::ZERO`) for a card whose image was never dropped, after what is left of the
/// grace period for one that was.
pub fn discard(path: PathBuf, wait: Duration) {
    let removal = std::thread::spawn(move || {
        std::thread::sleep(wait);
        remove_quietly(&path);
    });
    let mut pending = REMOVALS.lock().unwrap_or_else(PoisonError::into_inner);
    pending.retain(|started| !started.is_finished());
    pending.push(removal);
}

/// Waits until every deletion started by [`discard`] has been made, at most one grace
/// period. A process that is about to end calls this, since ending would take the
/// background thread, and with it the deletion, along.
pub fn finish_pending() {
    let mut guard = REMOVALS.lock().unwrap_or_else(PoisonError::into_inner);
    let pending = std::mem::take(&mut *guard);
    drop(guard);
    for removal in pending {
        let _ = removal.join();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::test_env::TempDir;

    fn at(year: u16, month: u16, day: u16, hour: u16, minute: u16, second: u16) -> SYSTEMTIME {
        SYSTEMTIME {
            wYear: year,
            wMonth: month,
            wDay: day,
            wHour: hour,
            wMinute: minute,
            wSecond: second,
            ..Default::default()
        }
    }

    fn units(bytes: &[u8]) -> Vec<u16> {
        bytes
            .as_chunks::<2>()
            .0
            .iter()
            .map(|pair| u16::from_le_bytes(*pair))
            .collect()
    }

    /// Sets the modification time of a file or folder to `by` ago.
    fn age(path: &Path, by: Duration) {
        use std::os::windows::fs::OpenOptionsExt;
        const FILE_WRITE_ATTRIBUTES: u32 = 0x100;
        // Backup semantics let a folder be opened like a file.
        const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
        std::fs::File::options()
            .access_mode(FILE_WRITE_ATTRIBUTES)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
            .open(path)
            .expect("the path opens")
            .set_modified(SystemTime::now() - by)
            .expect("the time is set");
    }

    #[test]
    fn the_dragged_file_is_named_after_the_local_time() {
        assert_eq!(
            file_stem(&at(2026, 3, 7, 9, 5, 2)),
            "isolmaSS-20260307-090502"
        );
        assert_eq!(
            file_stem(&at(2026, 12, 31, 23, 59, 59)),
            "isolmaSS-20261231-235959"
        );
        let now = file_stem_now();
        let (prefix, stamp) = now.split_at("isolmaSS-".len());
        assert_eq!(prefix, "isolmaSS-");
        assert_eq!(stamp.len(), 15);
        assert_eq!(stamp.as_bytes()[8], b'-');
        assert!(
            stamp
                .bytes()
                .enumerate()
                .all(|(index, byte)| index == 8 || byte.is_ascii_digit()),
            "{stamp}"
        );
    }

    #[test]
    fn the_copies_live_in_an_isolmass_folder_of_the_temp_directory() {
        assert_eq!(
            directory(),
            std::env::temp_dir().join("isolmaSS").join("drag")
        );
    }

    #[test]
    fn an_hdrop_is_a_dropfiles_header_followed_by_a_double_nul_terminated_wide_path() {
        let path = Path::new(r"C:\Temp\isolmaSS\drag\ü shot.png");
        let bytes = hdrop_bytes(path);
        assert_eq!(
            DROPFILES_LEN,
            std::mem::size_of::<windows::Win32::UI::Shell::DROPFILES>()
        );
        let word = |at: usize| u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap());
        assert_eq!(
            word(0) as usize,
            DROPFILES_LEN,
            "pFiles points past the header"
        );
        assert_eq!((word(4), word(8)), (0, 0), "no drop point");
        assert_eq!(word(12), 0, "client coordinates");
        assert_eq!(word(16), 1, "the paths are wide characters");
        let list = units(&bytes[DROPFILES_LEN..]);
        let expected: Vec<u16> = path.as_os_str().encode_wide().collect();
        assert_eq!(&list[..expected.len()], expected.as_slice());
        assert_eq!(
            &list[expected.len()..],
            [0, 0],
            "the path and the list end in a NUL each"
        );
        assert_eq!(bytes.len(), DROPFILES_LEN + 2 * (expected.len() + 2));
    }

    #[test]
    fn text_is_utf16_with_a_terminating_nul() {
        let bytes = unicode_text_bytes("https://x.example/i/ü");
        let text = units(&bytes);
        assert_eq!(text.last(), Some(&0));
        assert_eq!(
            String::from_utf16(&text[..text.len() - 1]).unwrap(),
            "https://x.example/i/ü"
        );
        assert_eq!(unicode_text_bytes(""), [0, 0]);
    }

    #[test]
    fn a_payload_with_an_image_offers_the_file_first_then_the_link() {
        let path = Path::new(r"C:\Temp\shot.png");
        let payload = Payload::new(Some(path), "https://x.example/i/1");
        assert_eq!(payload.formats(), [CF_HDROP, CF_UNICODETEXT]);
        assert_eq!(payload.data(CF_HDROP), Some(hdrop_bytes(path).as_slice()));
        assert_eq!(
            payload.data(CF_UNICODETEXT),
            Some(unicode_text_bytes("https://x.example/i/1").as_slice())
        );
        assert_eq!(payload.data(8), None, "no other format is on offer");
    }

    #[test]
    fn a_payload_without_an_image_offers_the_link_only() {
        let payload = Payload::new(None, "https://x.example/i/1");
        assert_eq!(payload.formats(), [CF_UNICODETEXT]);
        assert_eq!(payload.data(CF_HDROP), None);
        assert!(payload.data(CF_UNICODETEXT).is_some());
    }

    #[test]
    fn staging_copies_the_uploaded_bytes_into_the_drag_folder() {
        let dir = TempDir::new("drag_stage");
        let source = dir.path().join("upload.bin");
        let bytes: Vec<u8> = (0..=255).cycle().take(70_000).collect();
        std::fs::write(&source, &bytes).unwrap();
        let folder = dir.path().join("nested").join("drag");
        let staged = stage(&source, &folder, "isolmaSS-20260307-090502", "png").unwrap();
        assert_eq!(staged, folder.join("isolmaSS-20260307-090502.png"));
        assert_eq!(std::fs::read(&staged).unwrap(), bytes);
        assert!(source.exists(), "the upload file is left to its owner");
    }

    #[test]
    fn a_taken_name_gets_a_number_and_never_overwrites_an_earlier_card() {
        let dir = TempDir::new("drag_names");
        let first = dir.path().join("first.bin");
        let second = dir.path().join("second.bin");
        std::fs::write(&first, b"first").unwrap();
        std::fs::write(&second, b"second").unwrap();
        let folder = dir.path().join("drag");
        let one = stage(&first, &folder, "shot", "jpg").unwrap();
        let two = stage(&second, &folder, "shot", "jpg").unwrap();
        let three = stage(&second, &folder, "shot", "jpg").unwrap();
        assert_eq!(one.file_name().unwrap(), "shot.jpg");
        assert_eq!(two.file_name().unwrap(), "shot-2.jpg");
        assert_eq!(three.file_name().unwrap(), "shot-3.jpg");
        assert_eq!(std::fs::read(&one).unwrap(), b"first");
        assert_eq!(std::fs::read(&two).unwrap(), b"second");
    }

    #[test]
    fn staging_gives_up_when_every_name_is_taken() {
        let dir = TempDir::new("drag_full");
        let source = dir.path().join("upload.bin");
        std::fs::write(&source, b"image").unwrap();
        let folder = dir.path().join("drag");
        std::fs::create_dir(&folder).unwrap();
        std::fs::write(folder.join("shot.png"), b"x").unwrap();
        for attempt in 2..=NAME_TRIES {
            std::fs::write(folder.join(format!("shot-{attempt}.png")), b"x").unwrap();
        }
        let error = stage(&source, &folder, "shot", "png").unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
    }

    #[test]
    fn staging_reports_a_missing_upload_file_and_leaves_nothing_behind() {
        let dir = TempDir::new("drag_missing");
        let folder = dir.path().join("drag");
        let error = stage(&dir.path().join("gone.bin"), &folder, "shot", "png").unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::NotFound);
        assert!(!folder.join("shot.png").exists());
    }

    #[test]
    fn a_card_reuses_its_copy_until_the_file_disappears() {
        let dir = TempDir::new("drag_reuse");
        let source = dir.path().join("upload.bin");
        std::fs::write(&source, b"image").unwrap();
        let folder = dir.path().join("drag");
        let mut cached = None;
        let first = staged_copy(&mut cached, &source, &folder, "shot", "png").unwrap();
        assert_eq!(cached.as_deref(), Some(first.as_path()));
        std::fs::write(&first, b"edited by the target app").unwrap();
        let again = staged_copy(&mut cached, &source, &folder, "other", "png").unwrap();
        assert_eq!(again, first, "the same file, no second copy");
        assert_eq!(std::fs::read(&again).unwrap(), b"edited by the target app");

        std::fs::remove_file(&first).unwrap();
        let fresh = staged_copy(&mut cached, &source, &folder, "shot", "png").unwrap();
        assert_eq!(std::fs::read(&fresh).unwrap(), b"image");
        assert_eq!(cached.as_deref(), Some(fresh.as_path()));
    }

    #[test]
    fn a_failed_copy_is_not_remembered() {
        let dir = TempDir::new("drag_failed");
        let mut cached = None;
        let result = staged_copy(
            &mut cached,
            &dir.path().join("gone.bin"),
            &dir.path().join("drag"),
            "shot",
            "png",
        );
        assert!(result.is_err());
        assert_eq!(cached, None);
    }

    #[test]
    fn cleanup_removes_only_files_older_than_the_limit() {
        let dir = TempDir::new("drag_cleanup");
        let day = Duration::from_secs(24 * 60 * 60);
        let old = dir.path().join("old.png");
        let recent = dir.path().join("recent.png");
        let fresh = dir.path().join("fresh.png");
        for file in [&old, &recent, &fresh] {
            std::fs::write(file, b"image").unwrap();
        }
        age(&old, day * 3);
        age(&recent, day - Duration::from_secs(600));
        let folder = dir.path().join("older-folder");
        std::fs::create_dir(&folder).unwrap();
        age(&folder, day * 5);

        assert_eq!(remove_older_than(dir.path(), SystemTime::now(), day), 1);
        assert!(!old.exists());
        assert!(recent.exists(), "not yet a day old");
        assert!(fresh.exists());
        assert!(folder.is_dir(), "folders are never removed");
    }

    #[test]
    fn cleanup_is_measured_against_the_given_time() {
        let dir = TempDir::new("drag_clock");
        let file = dir.path().join("shot.png");
        std::fs::write(&file, b"image").unwrap();
        let day = Duration::from_secs(24 * 60 * 60);
        assert_eq!(remove_older_than(dir.path(), SystemTime::now(), day), 0);
        assert!(file.exists());
        assert_eq!(
            remove_older_than(dir.path(), SystemTime::now() + day * 2, day),
            1
        );
        assert!(!file.exists());
        // A clock set back after the file was written never makes it stale.
        std::fs::write(&file, b"image").unwrap();
        assert_eq!(
            remove_older_than(dir.path(), SystemTime::UNIX_EPOCH, day),
            0
        );
    }

    #[test]
    fn cleanup_of_a_missing_folder_does_nothing() {
        let dir = TempDir::new("drag_nothing");
        assert_eq!(
            remove_older_than(&dir.path().join("absent"), SystemTime::now(), KEEP),
            0
        );
    }

    #[test]
    fn the_sweep_clears_a_stale_copy_from_the_drag_folder_but_not_from_its_parent() {
        let folder = directory();
        std::fs::create_dir_all(&folder).unwrap();
        let name = format!("sweep-test-{}", std::process::id());
        let stale = folder.join(format!("{name}-stale.png"));
        let fresh = folder.join(format!("{name}-fresh.png"));
        std::fs::write(&stale, b"image").unwrap();
        std::fs::write(&fresh, b"image").unwrap();
        age(&stale, KEEP * 2);
        // A stale file next to the folder, in the parent, is not this sweep's business.
        let beside = folder.parent().unwrap().join(format!("{name}-beside.png"));
        std::fs::write(&beside, b"image").unwrap();
        age(&beside, KEEP * 2);

        let removed = sweep_in_background().join().expect("the sweep finished");
        assert!(removed >= 1);
        assert!(!stale.exists());
        assert!(fresh.exists());
        assert!(beside.exists());
        std::fs::remove_file(&fresh).unwrap();
        std::fs::remove_file(&beside).unwrap();
    }

    #[test]
    fn the_sweep_clears_a_stale_upload_left_behind_by_an_ended_app() {
        let folder = crate::upload::staging_directory();
        std::fs::create_dir_all(&folder).unwrap();
        let name = format!("isolmass-upload-sweep-test-{}", std::process::id());
        let stale = folder.join(format!("{name}-stale.png"));
        let fresh = folder.join(format!("{name}-fresh.png"));
        std::fs::write(&stale, b"image").unwrap();
        std::fs::write(&fresh, b"image").unwrap();
        age(&stale, KEEP * 2);

        assert!(sweep() >= 1);
        assert!(!stale.exists());
        assert!(fresh.exists(), "an upload card may still be using it");
        std::fs::remove_file(&fresh).unwrap();
    }

    #[test]
    fn only_an_accepted_drop_counts_as_a_drop() {
        use windows::Win32::Foundation::{DRAGDROP_S_CANCEL, S_OK};
        assert!(dropped(DRAGDROP_S_DROP, 1), "copied");
        assert!(
            !dropped(DRAGDROP_S_DROP, 0),
            "released over a target that refused it"
        );
        assert!(!dropped(DRAGDROP_S_CANCEL, 1), "Esc");
        assert!(!dropped(S_OK, 1));
    }

    #[test]
    fn the_copy_of_a_dropped_card_is_kept_for_ten_seconds_after_the_drop() {
        let secs = Duration::from_secs;
        assert_eq!(grace_left(None), Duration::ZERO, "nothing was dropped");
        assert_eq!(grace_left(Some(Duration::ZERO)), secs(10));
        assert_eq!(grace_left(Some(secs(4))), secs(6));
        assert_eq!(
            grace_left(Some(Duration::from_millis(9_999))),
            Duration::from_millis(1)
        );
        assert_eq!(grace_left(Some(secs(10))), Duration::ZERO);
        assert_eq!(grace_left(Some(secs(3600))), Duration::ZERO);
    }

    #[test]
    fn a_discarded_copy_is_deleted_in_the_background_without_delay() {
        let dir = TempDir::new("drag_discard");
        let copy = dir.path().join("shot.png");
        let neighbour = dir.path().join("neighbour.png");
        std::fs::write(&copy, b"image").unwrap();
        std::fs::write(&neighbour, b"image").unwrap();
        discard(copy.clone(), Duration::ZERO);
        finish_pending();
        assert!(!copy.exists());
        assert!(neighbour.exists(), "only the named copy goes");
        finish_pending();
    }

    #[test]
    fn a_copy_that_was_just_dropped_survives_until_its_grace_period_is_over() {
        let dir = TempDir::new("drag_grace");
        let copy = dir.path().join("shot.png");
        std::fs::write(&copy, b"image").unwrap();
        let wait = Duration::from_millis(400);
        let started = std::time::Instant::now();
        discard(copy.clone(), wait);
        assert!(copy.exists(), "a chat app may still be reading it");
        finish_pending();
        assert!(!copy.exists());
        assert!(
            started.elapsed() >= wait,
            "ending the process waits for the deletion"
        );
    }

    #[test]
    fn discarding_a_copy_that_is_gone_or_cannot_be_deleted_changes_nothing_else() {
        let dir = TempDir::new("drag_discard_fails");
        discard(dir.path().join("gone.png"), Duration::ZERO);
        let folder = dir.path().join("folder.png");
        std::fs::create_dir(&folder).unwrap();
        discard(folder.clone(), Duration::ZERO);
        finish_pending();
        assert!(folder.is_dir(), "a folder is not a copy and stays");
    }

    #[test]
    fn finished_deletions_are_not_kept_in_the_list() {
        let dir = TempDir::new("drag_reap");
        let first = dir.path().join("first.png");
        let second = dir.path().join("second.png");
        std::fs::write(&first, b"image").unwrap();
        std::fs::write(&second, b"image").unwrap();
        let pending = || REMOVALS.lock().unwrap_or_else(PoisonError::into_inner);
        discard(first, Duration::ZERO);
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while !pending().iter().all(JoinHandle::is_finished) && std::time::Instant::now() < deadline
        {
            std::thread::sleep(Duration::from_millis(5));
        }
        discard(second.clone(), Duration::ZERO);
        assert_eq!(pending().len(), 1, "only the new deletion is waited for");
        finish_pending();
        assert_eq!(pending().len(), 0);
        assert!(!second.exists());
    }
}
