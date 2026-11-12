//! Sharing matches with friends over the network, with no server of anyone's: one player hosts, the others connect.
//!
//! Only the per-match summary travels (nicknames, player ids and chest counts), never the replay file, and only
//! for matches the other player was in. A connection starts with a password check, then both sides send what the
//! other one should have. Received summaries go to `shared.json` exactly like an added partner replay.
use crate::{
    shared::{self, Remote},
    store::Store,
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    io::{BufRead, BufReader, Read, Write},
    net::{TcpListener, TcpStream, ToSocketAddrs},
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

/// The port a host listens on unless told otherwise.
pub const PORT: u16 = 47321;
const PROTOCOL: u64 = 1;
const MAX_LINE: u64 = 8 * 1024 * 1024;
const MAX_OFFERS: usize = 300;
const RECENT_MATCHES: u32 = 300;
const IO_TIMEOUT: Duration = Duration::from_secs(20);

/// What one connection moved.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Exchange {
    /// Match summaries from the other side that were new or different from what was kept.
    pub received: usize,
    /// Match summaries given to the other side.
    pub sent: usize,
}

fn proof(nonce: &str, password: &str) -> String {
    Sha256::digest(format!("lootledger:{nonce}:{password}").as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn write_line(stream: &mut TcpStream, value: &Value) -> Result<(), String> {
    let mut line = serde_json::to_vec(value).map_err(|e| e.to_string())?;
    line.push(b'\n');
    stream
        .write_all(&line)
        .and_then(|_| stream.flush())
        .map_err(|e| e.to_string())
}

fn read_line(reader: &mut BufReader<TcpStream>) -> Result<Value, String> {
    let mut line = String::new();
    let n = reader
        .by_ref()
        .take(MAX_LINE)
        .read_line(&mut line)
        .map_err(|e| e.to_string())?;
    if n == 0 {
        return Err("The other side closed the connection".into());
    }
    if !line.ends_with('\n') {
        return Err("The other side sent too much data".into());
    }
    serde_json::from_str(&line).map_err(|_| "The other side did not speak LootLedger".to_owned())
}

fn strings(value: &Value) -> Vec<String> {
    value
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|v| v.as_str().map(str::to_owned))
        .take(64)
        .collect()
}

/// The ids you play as in the recent matches of this ledger.
pub(crate) fn my_keys(store: &Store) -> Result<Vec<String>, String> {
    let mut keys: Vec<String> = Vec::new();
    for m in store
        .history(true, 0, RECENT_MATCHES)
        .map_err(|e| e.to_string())?
    {
        for p in m
            .meta
            .iter()
            .flat_map(|m| m["players"].as_array())
            .flatten()
        {
            if let Some(k) = p["key"].as_str().filter(|_| p["me"] == true)
                && !keys.iter().any(|x| x == k)
            {
                keys.push(k.to_owned());
            }
        }
    }
    Ok(keys)
}

/// What the other side needs of a match summary: which match it is and what each player's count was.
pub(crate) fn slim(meta: &Value) -> Value {
    json!({"key": meta["key"], "players": meta["players"]})
}

/// Your recent matches that the player with these ids took part in.
fn offer_for(store: &Store, peer_keys: &[String]) -> Result<Vec<Remote>, String> {
    let now = chrono::Local::now().to_rfc3339();
    let mut offer = Vec::new();
    for m in store
        .history(true, 0, RECENT_MATCHES)
        .map_err(|e| e.to_string())?
    {
        let Some(meta) = m.meta else { continue };
        let Some(key) = meta["key"].as_str().map(str::to_owned) else {
            continue;
        };
        let together = meta["players"].as_array().into_iter().flatten().any(|p| {
            p["key"]
                .as_str()
                .is_some_and(|k| peer_keys.iter().any(|x| x == k))
        });
        if together && shared::session_of(&key).is_some() {
            offer.push(Remote {
                key,
                received: now.clone(),
                meta: slim(&meta),
            });
        }
        if offer.len() >= MAX_OFFERS {
            break;
        }
    }
    Ok(offer)
}

/// Keep the well-formed summaries the other side offered; returns how many were new or different.
pub(crate) fn accept(offer: &Value, target: &Path) -> Result<usize, String> {
    let mut good = Vec::new();
    for item in offer.as_array().into_iter().flatten().take(MAX_OFFERS) {
        let Ok(remote) = serde_json::from_value::<Remote>(item.clone()) else {
            continue;
        };
        if shared::session_of(&remote.key).is_some()
            && remote.meta["key"].as_str() == Some(remote.key.as_str())
            && remote.meta["players"].is_array()
        {
            good.push(remote);
        }
    }
    shared::add_many(target, good).map_err(|e| e.to_string())
}

fn handle(mut stream: TcpStream, password: &str, ledger: &Path) -> Result<Exchange, String> {
    stream.set_read_timeout(Some(IO_TIMEOUT)).ok();
    stream.set_write_timeout(Some(IO_TIMEOUT)).ok();
    let mut reader = BufReader::new(stream.try_clone().map_err(|e| e.to_string())?);
    let nonce = uuid::Uuid::new_v4().simple().to_string();
    write_line(
        &mut stream,
        &json!({"hello": "lootledger", "v": PROTOCOL, "nonce": nonce}),
    )?;
    let auth = read_line(&mut reader)?;
    if auth["proof"].as_str() != Some(proof(&nonce, password).as_str()) {
        // Slows down anyone guessing passwords.
        std::thread::sleep(Duration::from_secs(1));
        write_line(&mut stream, &json!({"error": "Wrong room password"}))?;
        return Err("Someone tried the wrong room password".into());
    }
    let store = Store::read_only(ledger).map_err(|e| e.to_string())?;
    let offer = offer_for(&store, &strings(&auth["keys"]))?;
    let sent = offer.len();
    write_line(
        &mut stream,
        &json!({"ok": true, "keys": my_keys(&store)?, "offer": offer}),
    )?;
    let theirs = read_line(&mut reader)?;
    let received = accept(&theirs["offer"], &shared::path_for(ledger))?;
    write_line(&mut stream, &json!({"done": true, "received": received}))?;
    Ok(Exchange { received, sent })
}

/// Connect to a host and swap match summaries. `address` is a host name or IP, with an optional `:port`.
pub fn sync(address: &str, password: &str, ledger: &Path) -> Result<Exchange, String> {
    let address = address.trim();
    if address.is_empty() {
        return Err("Enter your friend's address first".into());
    }
    let target = if address.contains(':') {
        address.to_owned()
    } else {
        format!("{address}:{PORT}")
    };
    let socket = target
        .to_socket_addrs()
        .map_err(|_| format!("Could not find \"{address}\". Check the address in Tailscale"))?
        .next()
        .ok_or_else(|| format!("Could not find \"{address}\""))?;
    let mut stream = TcpStream::connect_timeout(&socket, Duration::from_secs(8)).map_err(|_| {
        "Could not reach your friend. Is the app running there with sharing on, and are you both in Tailscale?"
            .to_owned()
    })?;
    stream.set_read_timeout(Some(IO_TIMEOUT)).ok();
    stream.set_write_timeout(Some(IO_TIMEOUT)).ok();
    let mut reader = BufReader::new(stream.try_clone().map_err(|e| e.to_string())?);
    let hello = read_line(&mut reader)?;
    if hello["hello"] != "lootledger" || hello["v"].as_u64() != Some(PROTOCOL) {
        return Err("That is not a LootLedger host, or it is a different version".into());
    }
    let nonce = hello["nonce"].as_str().unwrap_or("");
    let store = Store::read_only(ledger).map_err(|e| e.to_string())?;
    write_line(
        &mut stream,
        &json!({"proof": proof(nonce, password), "keys": my_keys(&store)?}),
    )?;
    let reply = read_line(&mut reader)?;
    if let Some(error) = reply["error"].as_str() {
        return Err(error.to_owned());
    }
    let received = accept(&reply["offer"], &shared::path_for(ledger))?;
    let offer = offer_for(&store, &strings(&reply["keys"]))?;
    let sent = offer.len();
    write_line(&mut stream, &json!({"offer": offer}))?;
    // The host's confirmation is a courtesy; the data is already stored on both sides.
    let _ = read_line(&mut reader);
    Ok(Exchange { received, sent })
}

/// A running host. Dropping it stops listening.
pub struct Host {
    stop: Arc<AtomicBool>,
    port: u16,
}

impl Host {
    /// Start listening on `port` (0 picks a free one). `notify` is told about every finished connection.
    pub fn start(
        ledger: PathBuf,
        port: u16,
        password: String,
        notify: impl Fn(Result<Exchange, String>) + Send + 'static,
    ) -> Result<Host, String> {
        // A host that was just stopped may still hold the port for a moment.
        let mut tries = 0;
        let listener = loop {
            match TcpListener::bind(("0.0.0.0", port)) {
                Ok(listener) => break listener,
                Err(_) if tries < 15 => {
                    tries += 1;
                    std::thread::sleep(Duration::from_millis(100));
                }
                Err(e) => {
                    return Err(format!(
                        "Could not start sharing on port {port}: {e}. Is another LootLedger already hosting?"
                    ));
                }
            }
        };
        listener.set_nonblocking(true).map_err(|e| e.to_string())?;
        let port = listener.local_addr().map_err(|e| e.to_string())?.port();
        let stop = Arc::new(AtomicBool::new(false));
        let flag = stop.clone();
        std::thread::spawn(move || {
            while !flag.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((stream, _)) => {
                        stream.set_nonblocking(false).ok();
                        notify(handle(stream, &password, &ledger));
                    }
                    Err(_) => std::thread::sleep(Duration::from_millis(200)),
                }
            }
        });
        Ok(Host { stop, port })
    }

    pub fn port(&self) -> u16 {
        self.port
    }
}

impl Drop for Host {
    /// Asks the listener to stop; it ends within a moment, after any connection it is serving.
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

/// A new room password: short enough to say out loud.
pub fn new_password() -> String {
    uuid::Uuid::new_v4().simple().to_string()[..8].to_owned()
}
