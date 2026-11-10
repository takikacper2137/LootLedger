//! The current match and replay reading, from files on disk only (a port of lootledger/live.py and
//! replay_reader.scan). Nothing here touches the Fortnite process.
//!
//! States: idle (Fortnite not running), waiting (running, no match being recorded), live (a replay is being
//! recorded), saving (the replay just finished; it is being read) and ended (that match is counted).

use lootledger_core::{
    container,
    model::Replay,
    replay,
    store::{Error, Store},
};
use std::{
    path::{Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

/// Fortnite flushes the replay about every 20 s; no write for 90 s means recording stopped.
const STALE: f64 = 90.0;
/// The game writes its log continuously while it runs.
const RUNNING: f64 = 120.0;
/// A replay still marked live but untouched for longer was abandoned (crash or quit).
const LIVE_WINDOW: f64 = 15.0 * 60.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    Idle,
    Waiting,
    Live,
    Saving,
    Ended,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct Facts {
    pub live: bool,
    pub locked: bool,
    pub recorded: f64,
}

/// The replay header only: live flag, whether it can be decoded yet, and how much is recorded.
pub fn peek(path: &Path) -> Option<Facts> {
    use std::io::Read;
    let mut head = Vec::with_capacity(4096);
    std::fs::File::open(path)
        .ok()?
        .take(4096)
        .read_to_end(&mut head)
        .ok()?;
    let info = container::read_info(&head).ok()?;
    Some(Facts {
        live: info.live,
        locked: info.locked(),
        recorded: info.length_ms as f64 / 1000.0,
    })
}

pub fn seconds(time: SystemTime) -> f64 {
    time.duration_since(UNIX_EPOCH)
        .unwrap_or(Duration::ZERO)
        .as_secs_f64()
}

fn now() -> f64 {
    seconds(SystemTime::now())
}

pub fn replays(folder: &Path) -> Vec<(PathBuf, std::fs::Metadata)> {
    let mut files: Vec<_> = std::fs::read_dir(folder)
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| {
            p.extension()
                .is_some_and(|e| e.eq_ignore_ascii_case("replay"))
        })
        .filter_map(|p| std::fs::metadata(&p).ok().map(|m| (p, m)))
        .collect();
    files.sort_by(|a, b| {
        seconds(a.1.modified().unwrap_or(UNIX_EPOCH))
            .total_cmp(&seconds(b.1.modified().unwrap_or(UNIX_EPOCH)))
    });
    files
}

pub struct Tracker {
    log: PathBuf,
    pub state: State,
    file: Option<PathBuf>,
    size: Option<u64>,
    modified: Option<f64>,
    facts: Option<Facts>,
    started: Option<f64>,
}

impl Tracker {
    pub fn new(log: PathBuf) -> Tracker {
        Tracker {
            log,
            state: State::Idle,
            file: None,
            size: None,
            modified: None,
            facts: None,
            started: None,
        }
    }

    /// Update and return the state: one directory listing, a stat, and a header read on change.
    pub fn poll(&mut self, folder: &Path) -> State {
        let now = now();
        if let Some((newest, meta)) = replays(folder).pop() {
            let mtime = seconds(meta.modified().unwrap_or(UNIX_EPOCH));
            if now - mtime < STALE {
                // Fortnite can finalize a replay by rewriting its header without changing its size.
                // Watch the modification time as well as the path and length, otherwise `live` can
                // remain cached and the tracker will not notice that the match has ended.
                if Some(&newest) != self.file.as_ref()
                    || Some(meta.len()) != self.size
                    || Some(mtime) != self.modified
                {
                    self.facts = peek(&newest);
                    self.size = Some(meta.len());
                    self.modified = Some(mtime);
                }
                if self.facts.is_some_and(|f| f.live) {
                    if Some(&newest) != self.file.as_ref() {
                        // Windows: creation time = recording start.
                        self.started = meta.created().ok().map(seconds);
                        self.file = Some(newest);
                    }
                    self.state = State::Live;
                    return self.state;
                }
            }
        }
        if matches!(self.state, State::Live | State::Saving) && self.file.is_some() {
            self.state = State::Saving; // finished (or stopped); stays here until the import is done
            return self.state;
        }
        if self.state != State::Ended {
            let running = std::fs::metadata(&self.log)
                .and_then(|m| m.modified())
                .is_ok_and(|t| now - seconds(t) < RUNNING);
            self.state = if running { State::Waiting } else { State::Idle };
        }
        self.state
    }

    /// The finished replay was imported: show it as ended until the next match starts.
    pub fn counted(&mut self) {
        if self.state == State::Saving {
            self.state = State::Ended;
        }
    }

    pub fn file_name(&self) -> Option<String> {
        self.file
            .as_ref()
            .and_then(|f| f.file_name())
            .map(|n| n.to_string_lossy().into_owned())
    }

    pub fn elapsed(&self) -> Option<f64> {
        (self.state == State::Live)
            .then_some(self.started?)
            .map(|s| now() - s)
    }

    pub fn recorded(&self) -> f64 {
        self.facts.map_or(0.0, |f| f.recorded)
    }

    pub fn locked(&self) -> bool {
        self.facts.is_some_and(|f| f.locked)
    }
}

#[derive(Default)]
pub struct ScanResult {
    pub live: Option<Replay>,
    /// Trouble with the ledger itself, which would repeat for every file: the import stopped.
    pub error: Option<String>,
    /// Replay files that could not be read or imported this time (locked by another program, say).
    /// They never stop the others and are tried again at the next check.
    pub skipped: Vec<String>,
}

/// Import new or changed replays with this store (its own connection, on a background thread).
/// Imports are idempotent, so a renamed copy or a re-scan never double counts.
pub fn scan(
    store: &mut Store,
    folder: &Path,
    mut progress: impl FnMut(usize, usize),
) -> ScanResult {
    if !folder.is_dir() {
        return ScanResult {
            error: Some(format!("Replay folder not found: {}", folder.display())),
            ..ScanResult::default()
        };
    }
    let now = now();
    let (known, older) = match (store.replay_files(), store.files_without_players()) {
        (Ok(k), Ok(o)) => (k, o),
        (Err(e), _) | (_, Err(e)) => {
            return ScanResult {
                error: Some(e.to_string()),
                ..ScanResult::default()
            };
        }
    };
    let mut todo = Vec::new();
    let mut live = None;
    for (path, meta) in replays(folder) {
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let mtime = seconds(meta.modified().unwrap_or(UNIX_EPOCH));
        // Python and Rust may round a file time differently in the last bit: compare within a microsecond.
        let unchanged = known
            .get(&name)
            .is_some_and(|(size, time)| *size == meta.len() as i64 && (time - mtime).abs() < 1e-6);
        if !older.contains(&name) && unchanged {
            continue;
        }
        if let Some(facts) = peek(&path)
            && facts.live
            && facts.locked
            && now - mtime < LIVE_WINDOW
        {
            // Still recording and encrypted without its key: nothing can be decoded until Fortnite finishes it.
            live = Some(Replay {
                file: path.to_string_lossy().into_owned(),
                live: true,
                length_seconds: facts.recorded,
                ..Replay::default()
            });
            continue;
        }
        todo.push((path, name, meta.len(), mtime));
    }
    let total = todo.len();
    // Decoding is the slow part (about 1 s per replay), so a few threads read and decode while this one
    // imports through its single connection. Half the cores at most: Fortnite may be running.
    let workers = std::thread::available_parallelism()
        .map_or(1, |n| (n.get() / 2).clamp(1, 6))
        .min(total.max(1));
    let queue = std::sync::Mutex::new(todo.into_iter());
    let (sender, receiver) = std::sync::mpsc::sync_channel(workers);
    let mut error = None;
    let mut skipped = Vec::new();
    std::thread::scope(|scope| {
        for _ in 0..workers {
            let sender = sender.clone();
            let queue = &queue;
            scope.spawn(move || {
                while let Some((path, name, size, mtime)) =
                    queue.lock().ok().and_then(|mut q| q.next())
                {
                    let result = std::fs::read(&path)
                        .map(|data| replay::decode(&path.to_string_lossy(), &data))
                        .map_err(|e| format!("{}: {e}", path.display()));
                    if sender.send((result, name, size, mtime)).is_err() {
                        break; // the import stopped on an error
                    }
                }
            });
        }
        drop(sender);
        let receiver = receiver; // dropped on an early stop, which releases any worker waiting to send
        for (done, (result, name, size, mtime)) in receiver.iter().enumerate() {
            let done = done + 1; // every file counts toward the progress, whatever became of it
            let result = match result {
                Ok(r) => r,
                Err(e) => {
                    skipped.push(e); // one unreadable file must not hold back the rest
                    progress(done, total);
                    continue;
                }
            };
            if result.live && now - mtime < LIVE_WINDOW {
                live = Some(result);
                progress(done, total);
                continue;
            }
            match store.import_replay(&result, &name, size, mtime) {
                Ok(_) => {}
                // This replay holds values the ledger refuses; the others are fine.
                Err(Error::Invalid(e)) => skipped.push(format!("{name}: {e}")),
                // The database itself is in trouble: every further file would fail the same way.
                Err(e) => {
                    error = Some(format!("{name}: {e}"));
                    break;
                }
            }
            progress(done, total);
        }
    });
    ScanResult {
        live,
        error,
        skipped,
    }
}

/// A short two-note chime at this volume (0–100), written once per volume and played without blocking.
pub fn chime(folder: &Path, volume: i64) {
    #[cfg(windows)]
    {
        if volume <= 0 {
            return;
        }
        let path = folder.join(format!("chime-{volume}.wav"));
        if !path.exists() {
            let rate = 22050u32;
            let mut samples = Vec::new();
            for note in [880.0f64, 1320.0] {
                for i in 0..(rate as f64 * 0.22) as u32 {
                    let t = i as f64 / rate as f64;
                    let v = 32767.0 * volume as f64 / 100.0
                        * 0.5
                        * (2.0 * std::f64::consts::PI * note * t).sin()
                        * (-t * 14.0).exp();
                    samples.extend_from_slice(&(v as i16).to_le_bytes());
                }
            }
            let mut wav = Vec::with_capacity(44 + samples.len());
            wav.extend_from_slice(b"RIFF");
            wav.extend_from_slice(&(36 + samples.len() as u32).to_le_bytes());
            wav.extend_from_slice(b"WAVEfmt ");
            wav.extend_from_slice(&16u32.to_le_bytes());
            wav.extend_from_slice(&1u16.to_le_bytes());
            wav.extend_from_slice(&1u16.to_le_bytes());
            wav.extend_from_slice(&rate.to_le_bytes());
            wav.extend_from_slice(&(rate * 2).to_le_bytes());
            wav.extend_from_slice(&2u16.to_le_bytes());
            wav.extend_from_slice(&16u16.to_le_bytes());
            wav.extend_from_slice(b"data");
            wav.extend_from_slice(&(samples.len() as u32).to_le_bytes());
            wav.extend_from_slice(&samples);
            if std::fs::write(&path, wav).is_err() {
                return;
            }
        }
        use windows::Win32::Media::Audio::{PlaySoundW, SND_ASYNC, SND_FILENAME, SND_NODEFAULT};
        let wide: Vec<u16> = path.as_os_str().encode_wide_null();
        // SAFETY: the path buffer outlives the call; SND_ASYNC copies what it needs.
        unsafe {
            let _ = PlaySoundW(
                windows::core::PCWSTR(wide.as_ptr()),
                None,
                SND_FILENAME | SND_ASYNC | SND_NODEFAULT,
            );
        }
    }
    #[cfg(not(windows))]
    let _ = (folder, volume);
}

#[cfg(windows)]
trait WideNull {
    fn encode_wide_null(&self) -> Vec<u16>;
}

#[cfg(windows)]
impl WideNull for std::ffi::OsStr {
    fn encode_wide_null(&self) -> Vec<u16> {
        use std::os::windows::ffi::OsStrExt;
        self.encode_wide().chain(std::iter::once(0)).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scan_reads_every_file_once_on_several_threads() {
        let dir = std::env::temp_dir().join(format!("lootledger-scan-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        for i in 0..23 {
            std::fs::write(
                dir.join(format!("UnsavedReplay-{i:02}.replay")),
                [i as u8; 64],
            )
            .unwrap();
        }
        let mut store = Store::open(dir.join("ledger.sqlite")).unwrap();
        let mut seen = Vec::new();
        let result = scan(&mut store, &dir, |done, total| seen.push((done, total)));
        assert!(result.error.is_none(), "{:?}", result.error);
        assert_eq!(seen.last(), Some(&(23, 23)));
        assert_eq!(store.replay_files().unwrap().len(), 23);
        // A second scan finds nothing new.
        let mut again = 0;
        scan(&mut store, &dir, |_, _| again += 1);
        assert_eq!(again, 0);
        drop(store);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn one_unreadable_file_does_not_stop_the_others() {
        let dir = std::env::temp_dir().join(format!("lootledger-bad-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        for i in 0..12 {
            std::fs::write(
                dir.join(format!("UnsavedReplay-{i:02}.replay")),
                [i as u8; 64],
            )
            .unwrap();
        }
        // A directory with a replay's name stands in for a file that cannot be read (locked, say).
        std::fs::create_dir(dir.join("UnsavedReplay-bad.replay")).unwrap();
        let mut store = Store::open(dir.join("ledger.sqlite")).unwrap();
        let mut seen = Vec::new();
        let result = scan(&mut store, &dir, |done, total| seen.push((done, total)));
        assert!(result.error.is_none(), "{:?}", result.error);
        assert_eq!(store.replay_files().unwrap().len(), 12);
        assert_eq!(result.skipped.len(), 1, "{:?}", result.skipped);
        assert!(result.skipped[0].contains("UnsavedReplay-bad"));
        assert_eq!(seen.last(), Some(&(13, 13)), "progress counts every file");
        // The next check tries the unreadable one again and still imports nothing twice.
        let mut again = 0;
        let second = scan(&mut store, &dir, |_, _| again += 1);
        assert_eq!((again, second.skipped.len()), (1, 1));
        assert_eq!(store.replay_files().unwrap().len(), 12);
        drop(store);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
