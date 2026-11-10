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
    collections::{HashMap, HashSet, VecDeque},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicUsize, Ordering},
        mpsc::{RecvTimeoutError, SyncSender},
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
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
                if Some(&newest) != self.file.as_ref() || Some(meta.len()) != self.size {
                    self.facts = peek(&newest);
                    self.size = Some(meta.len());
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

/// A plain-text trace of what the scan decided and why (`scan.log` next to the ledger). It is written
/// line by line while the scan runs, so it also shows where a scan that never finishes got stuck. It holds
/// file names, sizes and error messages only, and starts over when it passes 1 MB.
#[derive(Clone, Default)]
pub struct ScanLog(Option<PathBuf>);

impl ScanLog {
    pub fn new(path: PathBuf) -> Self {
        ScanLog(Some(path))
    }

    pub fn line(&self, text: &str) {
        use std::io::Write;
        let Some(path) = &self.0 else { return };
        if std::fs::metadata(path).is_ok_and(|m| m.len() > 1_000_000) {
            let _ = std::fs::rename(path, path.with_extension("log.old"));
        }
        let stamp = chrono::Local::now().format("%Y-%m-%d %H:%M:%S%.3f");
        if let Ok(mut file) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
        {
            let _ = file.write_all(format!("{stamp} {text}\n").as_bytes());
        }
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
    /// Finished replays that Fortnite may still be writing to: look again in a few seconds.
    pub pending: usize,
}

/// The longest one replay may take to decode before it is given up on, so a single bad file cannot hold the
/// whole scan (and every match after it) back. A normal replay takes about a second.
const DECODE_LIMIT: Duration = Duration::from_secs(120);
/// A finished replay that nobody has written to for this long is complete for sure.
const SETTLED: f64 = 60.0;
/// Fortnite can keep appending to a replay for a few seconds after it clears the "live" flag. A finished replay
/// is read once its size was the same at two checks in a row and nothing was written for this long...
const QUIET_FINISHED: f64 = 4.0;
/// ...or this long while its decryption key is still missing.
const QUIET_NO_KEY: f64 = 10.0;
/// Decoders left running after a timeout. Past this many, no replacement is started.
const MAX_STUCK: usize = 3;
static STUCK: AtomicUsize = AtomicUsize::new(0);

type Decoder = fn(&str, &[u8]) -> Replay;

/// The size each recent replay had at the previous check, to tell when it stopped growing.
fn last_seen() -> &'static Mutex<HashMap<PathBuf, u64>> {
    static SEEN: OnceLock<Mutex<HashMap<PathBuf, u64>>> = OnceLock::new();
    SEEN.get_or_init(Mutex::default)
}

#[derive(Clone)]
struct Job {
    path: PathBuf,
    name: String,
    size: u64,
    mtime: f64,
}

struct Done {
    result: Result<Replay, String>,
    job: Job,
}

/// What the decoder threads share: the files still to read, what each thread is working on (and since when),
/// and the files whose thread was given up on.
struct Shared {
    queue: Mutex<VecDeque<Job>>,
    busy: Mutex<Vec<Option<(Job, Instant)>>>,
    abandoned: Mutex<HashSet<String>>,
}

fn start_worker(
    shared: &Arc<Shared>,
    sender: &SyncSender<Done>,
    log: &ScanLog,
    decoder: Decoder,
) -> bool {
    let slot = {
        let Ok(mut busy) = shared.busy.lock() else {
            return false;
        };
        busy.push(None);
        busy.len() - 1
    };
    let (shared, sender, log) = (shared.clone(), sender.clone(), log.clone());
    std::thread::spawn(move || {
        loop {
            // Take the next file and mark it as this thread's work in one step, so a watcher that sees an
            // empty queue and no busy thread can trust that nothing is in flight.
            let job = {
                let Ok(mut queue) = shared.queue.lock() else {
                    return;
                };
                let job = queue.pop_front();
                if let (Some(job), Ok(mut busy)) = (&job, shared.busy.lock()) {
                    busy[slot] = Some((job.clone(), Instant::now()));
                }
                job
            };
            let Some(job) = job else { return };
            let started = Instant::now();
            log.line(&format!("  {}: reading", job.name));
            let result = std::fs::read(&job.path)
                .map(|data| {
                    log.line(&format!(
                        "  {}: read {} bytes, decoding",
                        job.name,
                        data.len()
                    ));
                    decoder(&job.path.to_string_lossy(), &data)
                })
                .map_err(|e| format!("{}: {e}", job.path.display()));
            match &result {
                Ok(r) => log.line(&format!(
                    "  {}: decoded in {:.1} s: ok={} live={} events={} error={}",
                    job.name,
                    started.elapsed().as_secs_f64(),
                    r.ok,
                    r.live,
                    r.events.len(),
                    r.error.as_deref().unwrap_or("none")
                )),
                Err(e) => log.line(&format!("  {}: could not read: {e}", job.name)),
            }
            let given_up = shared.abandoned.lock().is_ok_and(|a| a.contains(&job.name));
            if given_up {
                // The scan already moved on and started a replacement for this thread.
                STUCK.fetch_sub(1, Ordering::SeqCst);
                log.line(&format!("  {}: finished after being given up on", job.name));
                return;
            }
            if sender.send(Done { result, job }).is_err() {
                return; // the scan stopped on an error
            }
            if let Ok(mut busy) = shared.busy.lock() {
                busy[slot] = None;
            }
        }
    });
    true
}

/// Import new or changed replays with this store (its own connection, on a background thread).
/// Imports are idempotent, so a renamed copy or a re-scan never double counts.
pub fn scan(
    store: &mut Store,
    folder: &Path,
    log: &ScanLog,
    progress: impl FnMut(usize, usize),
) -> ScanResult {
    scan_with(store, folder, log, replay::decode, DECODE_LIMIT, progress)
}

fn scan_with(
    store: &mut Store,
    folder: &Path,
    log: &ScanLog,
    decoder: Decoder,
    limit: Duration,
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
    let mut todo = VecDeque::new();
    let mut live = None;
    let mut pending = 0;
    let files = replays(folder);
    let mut unchanged_files = 0;
    for (path, meta) in &files {
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
            unchanged_files += 1;
            continue;
        }
        let facts = peek(path);
        let why = match known.get(&name) {
            None => "new file".to_owned(),
            Some((size, _)) if older.contains(&name) => {
                format!("read by an older version (was {size} bytes)")
            }
            Some((size, _)) => format!("changed since it was read (was {size} bytes)"),
        };
        log.line(&format!(
            "candidate {name}: {} bytes, last written {:.0} s ago, {why}; header: {}",
            meta.len(),
            now - mtime,
            facts.map_or("unreadable".to_owned(), |f| format!(
                "live={} locked={} recorded={:.0}s",
                f.live, f.locked, f.recorded
            ))
        ));
        if let Some(facts) = facts
            && facts.live
            && facts.locked
            && now - mtime < LIVE_WINDOW
        {
            // Still recording and encrypted without its key: nothing can be decoded until Fortnite finishes it.
            log.line(&format!("  {name}: still recording, left for later"));
            live = Some(Replay {
                file: path.to_string_lossy().into_owned(),
                live: true,
                length_seconds: facts.recorded,
                ..Replay::default()
            });
            continue;
        }
        if let Some(facts) = facts
            && !facts.live
        {
            // Finished, but Fortnite may still be adding the last data: wait until it has gone quiet.
            let quiet = now - mtime;
            let stable = last_seen()
                .lock()
                .ok()
                .and_then(|mut seen| seen.insert(path.clone(), meta.len()))
                == Some(meta.len());
            let needed = if facts.locked {
                QUIET_NO_KEY
            } else {
                QUIET_FINISHED
            };
            if quiet <= SETTLED && !(stable && quiet >= needed) {
                log.line(&format!(
                    "  {name}: finished but still settling (quiet {quiet:.0} s of {needed:.0} s, size {}), looking again soon",
                    if stable { "unchanged" } else { "changed or not seen yet" }
                ));
                pending += 1;
                continue;
            }
            if let Ok(mut seen) = last_seen().lock() {
                seen.remove(path);
            }
        }
        todo.push_back(Job {
            path: path.clone(),
            name,
            size: meta.len(),
            mtime,
        });
    }
    let total = todo.len();
    log.line(&format!(
        "scan: {} replay file(s), {unchanged_files} already read, {total} to read now, {pending} settling, {} still recording",
        files.len(),
        usize::from(live.is_some())
    ));
    // Decoding is the slow part (about 1 s per replay), so a few threads read and decode while this one
    // imports through its single connection. Half the cores at most: Fortnite may be running.
    let workers = std::thread::available_parallelism()
        .map_or(1, |n| (n.get() / 2).clamp(1, 6))
        .min(total.max(1));
    let shared = Arc::new(Shared {
        queue: Mutex::new(todo),
        busy: Mutex::new(Vec::new()),
        abandoned: Mutex::new(HashSet::new()),
    });
    let (sender, receiver) = std::sync::mpsc::sync_channel::<Done>(workers);
    let mut alive = 0;
    if total > 0 {
        for _ in 0..workers {
            alive += usize::from(start_worker(&shared, &sender, log, decoder));
        }
    }
    let mut error = None;
    let mut skipped = Vec::new();
    let mut handled = 0; // files whose fate is settled: imported, set aside or given up on
    while handled < total {
        let done = match receiver.recv_timeout(Duration::from_millis(500)) {
            Ok(done) => done,
            Err(RecvTimeoutError::Timeout) => {
                // Watch for a decoder that has been busy with one file for too long.
                let stuck: Vec<Job> = shared
                    .busy
                    .lock()
                    .map(|mut busy| {
                        busy.iter_mut()
                            .filter_map(|slot| {
                                if slot
                                    .as_ref()
                                    .is_some_and(|(_, since)| since.elapsed() > limit)
                                {
                                    slot.take().map(|(job, _)| job)
                                } else {
                                    None
                                }
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                for job in stuck {
                    log.line(&format!(
                        "  {}: still decoding after {} s, giving up on it (it is tried again if the file changes)",
                        job.name,
                        limit.as_secs()
                    ));
                    if let Ok(mut gone) = shared.abandoned.lock() {
                        gone.insert(job.name.clone());
                    }
                    STUCK.fetch_add(1, Ordering::SeqCst);
                    alive -= 1;
                    if STUCK.load(Ordering::SeqCst) <= MAX_STUCK
                        && start_worker(&shared, &sender, log, decoder)
                    {
                        alive += 1;
                    }
                    let record = Replay {
                        file: job.path.to_string_lossy().into_owned(),
                        error: Some(format!("analysis took longer than {} s", limit.as_secs())),
                        ..Replay::default()
                    };
                    match store.import_replay(&record, &job.name, job.size, job.mtime) {
                        Ok(_) | Err(Error::Invalid(_)) => {
                            skipped.push(format!("{}: analysis took too long", job.name));
                        }
                        Err(e) => {
                            error = Some(format!("{}: {e}", job.name));
                        }
                    }
                    handled += 1;
                    progress(handled, total);
                }
                if error.is_some() {
                    break;
                }
                // Nothing queued, nothing in progress, nothing waiting to be received: all is delivered.
                let idle = shared.queue.lock().is_ok_and(|queue| {
                    queue.is_empty()
                        && shared
                            .busy
                            .lock()
                            .is_ok_and(|busy| busy.iter().all(Option::is_none))
                });
                if idle || alive == 0 {
                    break;
                }
                continue;
            }
            Err(RecvTimeoutError::Disconnected) => break,
        };
        handled += 1; // every file counts toward the progress, whatever became of it
        let Done { result, job } = done;
        let result = match result {
            Ok(r) => r,
            Err(e) => {
                skipped.push(e); // one unreadable file must not hold back the rest
                progress(handled, total);
                continue;
            }
        };
        if result.live && now - job.mtime < LIVE_WINDOW {
            live = Some(result);
            progress(handled, total);
            continue;
        }
        match store.import_replay(&result, &job.name, job.size, job.mtime) {
            Ok(id) => log.line(&format!(
                "  {}: saved as {}",
                job.name,
                id.map_or(
                    "not counted (no chests or no owner found)".to_owned(),
                    |_| { "a match".to_owned() }
                )
            )),
            // This replay holds values the ledger refuses; the others are fine.
            Err(Error::Invalid(e)) => {
                log.line(&format!("  {}: refused by the ledger: {e}", job.name));
                skipped.push(format!("{}: {e}", job.name));
            }
            // The database itself is in trouble: every further file would fail the same way.
            Err(e) => {
                error = Some(format!("{}: {e}", job.name));
                break;
            }
        }
        progress(handled, total);
    }
    // Whatever stopped the loop early, no thread should start on the rest of the queue.
    if let Ok(mut queue) = shared.queue.lock() {
        queue.clear();
    }
    log.line(&format!(
        "scan done: {} file(s) set aside, {pending} settling, error={}",
        skipped.len(),
        error.as_deref().unwrap_or("none")
    ));
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
    use std::io::Write;

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
        let result = scan(&mut store, &dir, &ScanLog::default(), |done, total| {
            seen.push((done, total))
        });
        assert!(result.error.is_none(), "{:?}", result.error);
        assert_eq!(seen.last(), Some(&(23, 23)));
        assert_eq!(store.replay_files().unwrap().len(), 23);
        // A second scan finds nothing new.
        let mut again = 0;
        scan(&mut store, &dir, &ScanLog::default(), |_, _| again += 1);
        assert_eq!(again, 0);
        drop(store);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_scan_log_records_what_was_decided_and_why() {
        let dir = std::env::temp_dir().join(format!("lootledger-log-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("UnsavedReplay-01.replay"), [1u8; 64]).unwrap();
        let mut store = Store::open(dir.join("ledger.sqlite")).unwrap();
        let log = ScanLog::new(dir.join("scan.log"));
        scan(&mut store, &dir, &log, |_, _| {});
        scan(&mut store, &dir, &log, |_, _| {});
        let text = std::fs::read_to_string(dir.join("scan.log")).unwrap();
        for wanted in [
            "candidate UnsavedReplay-01.replay: 64 bytes",
            "new file",
            "1 to read now",
            "UnsavedReplay-01.replay: decoded in",
            "0 to read now", // the second scan has nothing new
            "scan done",
        ] {
            assert!(text.contains(wanted), "missing {wanted:?} in:\n{text}");
        }
        drop(store);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A replay header like Fortnite writes it: `key` empty means still live, all zeros means no key yet.
    fn header(key: &[u8]) -> Vec<u8> {
        let mut v = Vec::new();
        for n in [
            container::MAGIC,
            7,
            0,
            40000,
            1,
            1,
            0,
            u32::from(key.is_empty()),
        ] {
            v.extend(n.to_le_bytes());
        }
        v.extend(123i64.to_le_bytes());
        for n in [1, 1, key.len() as u32] {
            v.extend(n.to_le_bytes());
        }
        v.extend(key);
        v
    }

    fn written_ago(path: &Path, seconds: u64) {
        let file = std::fs::File::options().write(true).open(path).unwrap();
        file.set_modified(SystemTime::now() - Duration::from_secs(seconds))
            .unwrap();
    }

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("lootledger-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn a_just_finished_replay_is_read_only_once_it_has_settled() {
        let dir = temp_dir("settle");
        let file = dir.join("UnsavedReplay-fresh.replay");
        std::fs::write(&file, header(&[7; 32])).unwrap();
        let mut store = Store::open(dir.join("ledger.sqlite")).unwrap();
        let log = ScanLog::default();

        // Written a moment ago: Fortnite may still be adding data, so it waits.
        let first = scan(&mut store, &dir, &log, |_, _| {});
        assert_eq!(first.pending, 1);
        assert!(store.replay_files().unwrap().is_empty());

        // Quiet for 6 s and the same size as at the previous check: read it.
        written_ago(&file, 6);
        let second = scan(&mut store, &dir, &log, |_, _| {});
        assert_eq!(second.pending, 0);
        assert!(
            store
                .replay_files()
                .unwrap()
                .contains_key("UnsavedReplay-fresh.replay")
        );
        drop(store);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_growing_or_keyless_replay_waits_longer_and_an_old_one_not_at_all() {
        let dir = temp_dir("settle2");
        let keyless = dir.join("UnsavedReplay-keyless.replay");
        let old = dir.join("UnsavedReplay-old.replay");
        std::fs::write(&keyless, header(&[0; 32])).unwrap();
        std::fs::write(&old, header(&[7; 32])).unwrap();
        written_ago(&old, 120);
        let mut store = Store::open(dir.join("ledger.sqlite")).unwrap();
        let log = ScanLog::default();

        // The old one needs no wait; the keyless one is quiet for 6 s, but without its key it needs 10.
        written_ago(&keyless, 6);
        scan(&mut store, &dir, &log, |_, _| {});
        let second = scan(&mut store, &dir, &log, |_, _| {});
        let known = store.replay_files().unwrap();
        assert!(known.contains_key("UnsavedReplay-old.replay"));
        assert!(!known.contains_key("UnsavedReplay-keyless.replay"));
        assert_eq!(second.pending, 1);

        // A file that grew since the last check is not stable yet, however quiet it is.
        std::fs::OpenOptions::new()
            .append(true)
            .open(&keyless)
            .unwrap()
            .write_all(&[0; 16])
            .unwrap();
        written_ago(&keyless, 12);
        assert_eq!(scan(&mut store, &dir, &log, |_, _| {}).pending, 1);

        // Unchanged since the last check and quiet for 12 s: now it is read.
        let last = scan(&mut store, &dir, &log, |_, _| {});
        assert_eq!(last.pending, 0);
        assert!(
            store
                .replay_files()
                .unwrap()
                .contains_key("UnsavedReplay-keyless.replay")
        );
        drop(store);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_replay_that_never_finishes_decoding_cannot_hold_back_the_others() {
        fn slow_for_one(file: &str, data: &[u8]) -> Replay {
            if file.contains("slow") {
                std::thread::sleep(Duration::from_secs(6));
            }
            replay::decode(file, data)
        }
        let dir = temp_dir("timeout");
        for i in 0..5 {
            std::fs::write(dir.join(format!("UnsavedReplay-{i}.replay")), [i as u8; 64]).unwrap();
        }
        std::fs::write(dir.join("UnsavedReplay-slow.replay"), [9u8; 64]).unwrap();
        let mut store = Store::open(dir.join("ledger.sqlite")).unwrap();
        let mut seen = Vec::new();
        let started = Instant::now();
        let result = scan_with(
            &mut store,
            &dir,
            &ScanLog::default(),
            slow_for_one,
            Duration::from_secs(1),
            |done, total| seen.push((done, total)),
        );
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "waited for the stuck file: {:?}",
            started.elapsed()
        );
        assert!(result.error.is_none(), "{:?}", result.error);
        assert_eq!(result.skipped.len(), 1, "{:?}", result.skipped);
        assert!(result.skipped[0].contains("slow") && result.skipped[0].contains("too long"));
        assert_eq!(seen.last(), Some(&(6, 6)));
        // Every file has a row, so the stuck one is not decoded again at each check.
        assert_eq!(store.replay_files().unwrap().len(), 6);
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
        let result = scan(&mut store, &dir, &ScanLog::default(), |done, total| {
            seen.push((done, total))
        });
        assert!(result.error.is_none(), "{:?}", result.error);
        assert_eq!(store.replay_files().unwrap().len(), 12);
        assert_eq!(result.skipped.len(), 1, "{:?}", result.skipped);
        assert!(result.skipped[0].contains("UnsavedReplay-bad"));
        assert_eq!(seen.last(), Some(&(13, 13)), "progress counts every file");
        // The next check tries the unreadable one again and still imports nothing twice.
        let mut again = 0;
        let second = scan(&mut store, &dir, &ScanLog::default(), |_, _| again += 1);
        assert_eq!((again, second.skipped.len()), (1, 1));
        assert_eq!(store.replay_files().unwrap().len(), 12);
        drop(store);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
