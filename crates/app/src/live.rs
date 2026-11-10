//! The current match and replay reading, from files on disk only (a port of lootledger/live.py and
//! replay_reader.scan). Nothing here touches the Fortnite process.
//!
//! States: idle (Fortnite not running), waiting (running, no match being recorded), live (a replay is being
//! recorded), saving (the replay just finished; it is being read) and ended (that match is counted).

use lootledger_core::{
    container,
    model::Replay,
    store::{Error, Store},
};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{Mutex, OnceLock},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

/// Fortnite flushes the replay about every 20 s; no write for 90 s means recording stopped.
const STALE: f64 = 90.0;
/// The game writes its log continuously while it runs.
const RUNNING: f64 = 120.0;
/// A replay still marked live but untouched for longer was abandoned (crash or quit).
const LIVE_WINDOW: f64 = 15.0 * 60.0;
/// A finalized replay needs a short quiet period and two observations with the same size.
const FINALIZED_QUIET: f64 = 4.0;
/// If Fortnite has not written the decryption key yet, wait longer before trying again.
const KEY_MISSING_QUIET: f64 = 10.0;
/// If the header cannot yet be parsed, wait conservatively before attempting a decode.
const UNKNOWN_HEADER_QUIET: f64 = 120.0;

#[derive(Clone, Copy)]
struct Readiness {
    size: u64,
    modified: f64,
    first_seen: f64,
}

static REPLAY_READINESS: OnceLock<Mutex<HashMap<PathBuf, Readiness>>> = OnceLock::new();

/// Mirror ChestCounter's readiness gate: don't decode while Fortnite is still flushing the replay.
fn replay_is_ready(path: &Path, size: u64, modified: f64, facts: Option<Facts>, now: f64) -> bool {
    let cache = REPLAY_READINESS.get_or_init(|| Mutex::new(HashMap::new()));
    let Ok(mut cache) = cache.lock() else {
        return false;
    };
    let entry = cache.entry(path.to_path_buf()).or_insert(Readiness {
        size,
        modified,
        first_seen: now,
    });
    if entry.size != size || (entry.modified - modified).abs() > 1e-6 {
        *entry = Readiness {
            size,
            modified,
            first_seen: now,
        };
        return false;
    }

    let stable_for = now - entry.first_seen;
    let quiet_for = now - modified;
    match facts {
        // An active replay is not ready, even if the key happens to be present.
        Some(Facts { live: true, .. }) => {
            // A replay still marked live but untouched for a long time was abandoned (crash/quit).
            stable_for >= KEY_MISSING_QUIET && quiet_for >= KEY_MISSING_QUIET
        }
        // Fortnite has cleared the live flag and written the key: require a stable file
        // and a short quiet period before decoding.
        Some(Facts { locked: false, .. }) => {
            stable_for >= FINALIZED_QUIET && quiet_for >= FINALIZED_QUIET
        }
        // The header is finalized but the encryption key is not available yet.
        // A quiet file is not enough: Fortnite may write the decryption key later
        // without changing the replay file itself. Keep it pending until peek() sees unlocked.
        Some(Facts { locked: true, .. }) => {
            stable_for >= KEY_MISSING_QUIET && quiet_for >= KEY_MISSING_QUIET
        }
        // An unreadable/partial header is handled conservatively by the caller.
        None => stable_for >= UNKNOWN_HEADER_QUIET && quiet_for >= UNKNOWN_HEADER_QUIET,
    }
}

/// Distinguish an incomplete real replay header from arbitrary invalid files (e.g. corrupt files).
fn has_replay_magic(path: &Path) -> bool {
    use std::io::Read;
    let mut magic = [0u8; 4];
    std::fs::File::open(path)
        .and_then(|mut file| file.read_exact(&mut magic))
        .is_ok()
        && magic == 0x1CA2E27Fu32.to_le_bytes()
}

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
    /// At least one replay is still being finalized; the caller should schedule a short retry.
    pub pending: bool,
}

/// Decode in a child process so a malformed replay or a stuck native decompressor cannot
/// hold the live scan thread forever. The child can be killed safely on timeout.
fn decode_isolated(path: &Path) -> Result<Replay, String> {
    // Unit tests run inside Rust's test harness, not the LootLedger executable.
    // The harness does not implement the private --decode-replay worker arguments.
    // Keep tests on the same decoder while production builds use process isolation.
    #[cfg(test)]
    {
        let data = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
        return Ok(lootledger_core::replay::decode(
            &path.to_string_lossy(),
            &data,
        ));
    }

    #[cfg(not(test))]
    {
    const TIMEOUT: Duration = Duration::from_secs(180);
    let executable = std::env::current_exe().map_err(|e| format!("decoder executable: {e}"))?;
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or(Duration::ZERO)
        .as_nanos();
    let output = std::env::temp_dir().join(format!(
        "lootledger-decode-{}-{stamp}.json",
        std::process::id()
    ));
    let mut child = Command::new(executable)
        .arg(format!("--decode-replay={}", path.to_string_lossy()))
        .arg(format!("--decode-output={}", output.to_string_lossy()))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("{}: could not start isolated decoder: {e}", path.display()))?;
    
    let started = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if started.elapsed() < TIMEOUT => {
                std::thread::sleep(Duration::from_millis(50));
            }
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                let _ = std::fs::remove_file(&output);
                return Err(format!(
                    "{}: replay decoder exceeded {} seconds and was stopped",
                    path.display(),
                    TIMEOUT.as_secs()
                ));
            }
            Err(e) => {
                let _ = child.kill();
                let _ = child.wait();
                let _ = std::fs::remove_file(&output);
                return Err(format!(
                    "{}: decoder process status failed: {e}",
                    path.display()
                ));
            }
        }
    };
    if !status.success() {
        let _ = std::fs::remove_file(&output);
        return Err(format!(
            "{}: isolated replay decoder exited with {status}",
            path.display()
        ));
    }
    let bytes = std::fs::read(&output)
        .map_err(|e| format!("{}: decoder output unavailable: {e}", path.display()));
    let _ = std::fs::remove_file(&output);
    let bytes = bytes?;
    serde_json::from_slice(&bytes)
        .map_err(|e| format!("{}: invalid decoder output: {e}", path.display()))
    }
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
    let mut pending = false;
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
        let facts = peek(&path);
        if let Some(facts) = facts
            && facts.live
            && now - mtime < LIVE_WINDOW
        {
            // Still recording: keep the live estimate and mark the scan pending.
            // The next scan must happen even when live tracking is disabled or the app
            // started after the match began; otherwise this replay is silently forgotten
            // until the user restarts LootLedger.
            live = Some(Replay {
                file: path.to_string_lossy().into_owned(),
                live: true,
                length_seconds: facts.recorded,
                ..Replay::default()
            });
            pending = true;
            continue;
        }
        // A valid but incomplete header can happen while Fortnite is finalizing the replay.
        // Do not mistake arbitrary corrupt files for an in-progress write.
        if (facts.is_some() || has_replay_magic(&path))
            && !replay_is_ready(&path, meta.len(), mtime, facts, now)
        {
            pending = true;
            continue;
        }
        todo.push((path, name, meta.len(), mtime));
    }
    // Start with the newest replay. The live tracker follows that file, so an older replay
    // must not occupy the first worker slot while the just-finished match waits in the queue.
    todo.sort_by(|a, b| b.3.total_cmp(&a.3));
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
                    let result = decode_isolated(&path);
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
        pending,
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
    fn replay_readiness_waits_for_a_stable_finalized_file() {
        let path = std::env::temp_dir().join(format!(
            "lootledger-readiness-{}-{}.replay",
            std::process::id(),
            now()
        ));
        let facts = Some(Facts {
            live: false,
            locked: false,
            recorded: 120.0,
        });
        let modified = 1_000.0;
        assert!(!replay_is_ready(&path, 500, modified, facts, 1_000.0));
        assert!(!replay_is_ready(&path, 500, modified, facts, 1_003.9));
        assert!(replay_is_ready(&path, 500, modified, facts, 1_004.1));
        // Any further write restarts the quiet/stability window.
        assert!(!replay_is_ready(&path, 600, 1_005.0, facts, 1_005.0));
        assert!(!replay_is_ready(&path, 600, 1_005.0, facts, 1_008.9));
        assert!(replay_is_ready(&path, 600, 1_005.0, facts, 1_009.1));
    }

    #[test]
    fn replay_readiness_waits_longer_for_the_encryption_key() {
        let path = std::env::temp_dir().join(format!(
            "lootledger-key-readiness-{}-{}.replay",
            std::process::id(),
            now()
        ));
        let facts = Some(Facts {
            live: false,
            locked: true,
            recorded: 120.0,
        });
        assert!(!replay_is_ready(&path, 500, 1_000.0, facts, 1_000.0));
        assert!(!replay_is_ready(&path, 500, 1_000.0, facts, 1_009.9));
        assert!(replay_is_ready(&path, 500, 1_000.0, facts, 1_010.1));
    }

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
