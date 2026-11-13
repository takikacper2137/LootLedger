//! Talks to the Discord desktop app on this PC so a line like "8,838 chests opened" shows under your name.
//! Everything runs on one background thread: it connects when Discord is open, resends when the text changes
//! (never faster than Discord allows) and gives up quietly when Discord is closed. Nothing goes over the
//! internet from here.

use lootledger_core::discord::{self, Activity};
use std::io::Write;
use std::sync::Mutex;
use std::sync::mpsc::{RecvTimeoutError, Sender, channel};
use std::time::{Duration, Instant};

/// What the settings page says about the connection.
static STATUS: Mutex<String> = Mutex::new(String::new());

pub fn status() -> String {
    STATUS.lock().map(|s| s.clone()).unwrap_or_default()
}

fn set_status(text: &str) {
    if let Ok(mut s) = STATUS.lock()
        && *s != text
    {
        *s = text.to_owned();
        let _ = slint::invoke_from_event_loop(crate::fill_settings);
    }
}

type Wanted = (String, Option<Activity>);

pub struct Presence {
    tx: Sender<Wanted>,
}

impl Presence {
    pub fn start() -> Self {
        let (tx, rx) = channel::<Wanted>();
        std::thread::spawn(move || {
            let mut conn: Option<(String, Connection)> = None;
            let mut wanted: Option<Wanted> = None;
            let mut sent: Option<Wanted> = None;
            let (mut last_try, mut last_send): (Option<Instant>, Option<Instant>) = (None, None);
            let mut nonce = 0u64;
            loop {
                match rx.recv_timeout(Duration::from_secs(2)) {
                    Ok(w) => {
                        wanted = Some(w);
                        while let Ok(w) = rx.try_recv() {
                            wanted = Some(w); // only the latest matters
                        }
                    }
                    Err(RecvTimeoutError::Timeout) => {}
                    Err(RecvTimeoutError::Disconnected) => break,
                }
                let Some((client, activity)) = wanted.clone() else {
                    continue;
                };
                if client.is_empty() {
                    // Switched off: clear what was shown, then let go of Discord.
                    if let Some((_, c)) = conn.as_mut() {
                        nonce += 1;
                        let _ = c.send(&discord::set_activity(
                            std::process::id(),
                            &nonce.to_string(),
                            None,
                        ));
                    }
                    conn = None;
                    sent = None;
                    wanted = None;
                    set_status("");
                    continue;
                }
                if !discord::valid_client_id(&client) {
                    set_status("That does not look like an application ID (17–20 digits).");
                    wanted = None;
                    continue;
                }
                if Some(&(client.clone(), activity.clone())) == sent.as_ref() {
                    continue;
                }
                if conn.as_ref().is_some_and(|(c, _)| *c != client) {
                    conn = None;
                }
                if last_send.is_some_and(|t| t.elapsed() < Duration::from_secs(5)) {
                    continue; // Discord allows about five updates per twenty seconds
                }
                if conn.is_none() {
                    if last_try.is_some_and(|t| t.elapsed() < Duration::from_secs(20)) {
                        continue;
                    }
                    last_try = Some(Instant::now());
                    match Connection::open(&client) {
                        Ok(c) => conn = Some((client.clone(), c)),
                        Err(e) => {
                            set_status(&e);
                            continue;
                        }
                    }
                }
                nonce += 1;
                let message = discord::set_activity(
                    std::process::id(),
                    &nonce.to_string(),
                    activity.as_ref(),
                );
                let outcome = conn.as_mut().map(|(_, c)| c.send(&message));
                last_send = Some(Instant::now());
                match outcome {
                    Some(Ok(())) => {
                        sent = Some((client, activity));
                        set_status("Connected: your chests show under your name in Discord.");
                    }
                    _ => {
                        conn = None;
                        sent = None;
                        set_status("Lost the connection to Discord; trying again.");
                    }
                }
            }
        });
        Presence { tx }
    }

    /// Show `activity` for this application id; an empty id switches the status off.
    pub fn show(&self, client_id: &str, activity: Option<Activity>) {
        let _ = self.tx.send((client_id.to_owned(), activity));
    }
}

struct Connection {
    #[cfg(windows)]
    pipe: std::fs::File,
}

impl Connection {
    #[cfg(windows)]
    fn open(client_id: &str) -> Result<Connection, String> {
        let mut pipe = (0..10)
            .find_map(|n| {
                std::fs::OpenOptions::new()
                    .read(true)
                    .write(true)
                    .open(format!(r"\\?\pipe\discord-ipc-{n}"))
                    .ok()
            })
            .ok_or(
                "Discord is not running on this PC. Open the Discord app and this reconnects.",
            )?;
        pipe.write_all(&discord::handshake(client_id))
            .map_err(|_| "Could not talk to Discord.".to_owned())?;
        match discord::read_frame(&mut pipe) {
            Ok((1, _)) => Ok(Connection { pipe }),
            Ok(_) => Err(
                "Discord refused this application ID. Check that you copied the Application ID."
                    .into(),
            ),
            Err(_) => Err("Discord did not answer.".into()),
        }
    }

    #[cfg(not(windows))]
    fn open(_: &str) -> Result<Connection, String> {
        Err("Discord status is only available on Windows.".into())
    }

    #[cfg(windows)]
    fn send(&mut self, message: &[u8]) -> std::io::Result<()> {
        self.pipe.write_all(message)?;
        // Discord answers every message; reading it keeps the pipe from filling up.
        discord::read_frame(&mut self.pipe).map(|_| ())
    }

    #[cfg(not(windows))]
    fn send(&mut self, _: &[u8]) -> std::io::Result<()> {
        let _ = std::io::sink().write(&[]);
        Ok(())
    }
}
