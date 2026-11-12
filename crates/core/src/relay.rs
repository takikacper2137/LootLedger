//! Sharing matches through a free online room (see `infra/party-relay`), for friends who do not want to set up
//! a direct connection. Everyone types the same room code; each LootLedger hands the room its recent match
//! summaries and gets back the other people's summaries of matches it was in.
//!
//! The HTTPS call itself is a function the caller passes in, so this works (and is tested) without a network.
use crate::{
    net::{self, Exchange},
    shared::{self, Remote},
    store::{Store, local_time},
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::path::Path;

const PROTOCOL: u64 = 1;
const RECENT_DAYS: i64 = 3;
const MAX_OFFER: usize = 60;

/// POST a JSON body to a URL and return the response body, whatever the HTTP status.
pub type Post<'a> = &'a dyn Fn(&str, &[u8]) -> Result<Vec<u8>, String>;

/// A new room code: letters and digits only, long enough to be unguessable.
pub fn new_room_code() -> String {
    uuid::Uuid::new_v4().simple().to_string()[..16].to_owned()
}

/// The address of a room on a server: `https://…/v1/rooms/<code>/sync`.
pub fn endpoint(server: &str, room: &str) -> Result<String, String> {
    let server = server.trim().trim_end_matches('/');
    if server.is_empty() {
        return Err("Enter the server address first".into());
    }
    if server.starts_with("http://") {
        return Err("The server address must start with https://".into());
    }
    let server = if server.starts_with("https://") {
        server.to_owned()
    } else {
        format!("https://{server}")
    };
    let room = room.trim();
    if !(12..=40).contains(&room.len()) || !room.chars().all(|c| c.is_ascii_alphanumeric()) {
        return Err("The room code must be 12 to 40 letters or digits".into());
    }
    Ok(format!("{server}/v1/rooms/{room}/sync"))
}

/// Your recent matches played with other people, newest first.
fn recent_shared_matches(store: &Store) -> Result<Vec<Remote>, String> {
    let now = chrono::Local::now();
    let stamp = now.to_rfc3339();
    let mut out = Vec::new();
    for m in store.history(true, 0, 300).map_err(|e| e.to_string())? {
        let recent = local_time(&m.started)
            .map(|t| (now - t).num_days() < RECENT_DAYS)
            .unwrap_or(false);
        let Some(meta) = m.meta.filter(|_| recent) else {
            continue;
        };
        let Some(key) = meta["key"].as_str().map(str::to_owned) else {
            continue;
        };
        if shared::session_of(&key).is_some()
            && meta["players"].as_array().is_some_and(|p| p.len() > 1)
        {
            out.push(Remote {
                key,
                received: stamp.clone(),
                meta: net::slim(&meta),
            });
        }
        if out.len() >= MAX_OFFER {
            break;
        }
    }
    Ok(out)
}

/// Identifies what is being offered, so an unchanged list is not sent again.
fn digest(keys: &[String], offer: &[Remote]) -> String {
    let body = json!([
        keys,
        offer
            .iter()
            .map(|r| json!([r.key, r.meta]))
            .collect::<Vec<_>>()
    ]);
    Sha256::digest(body.to_string().as_bytes())[..16]
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Swap match summaries with a room. `acked` remembers, between calls, which list of yours the room already
/// holds (leave it `None` the first time); the list is only sent again when it changes.
pub fn sync(
    post: Post,
    server: &str,
    room: &str,
    ledger: &Path,
    acked: &mut Option<String>,
) -> Result<Exchange, String> {
    let url = endpoint(server, room)?;
    let store = Store::read_only(ledger).map_err(|e| e.to_string())?;
    let keys = net::my_keys(&store)?;
    if keys.is_empty() {
        // Nothing of yours is known yet (no match read), so there is nobody to match friends against.
        return Ok(Exchange::default());
    }
    let offer = recent_shared_matches(&store)?;
    let id = digest(&keys, &offer);
    let call = |with_list: bool| -> Result<Value, String> {
        let list = with_list.then(|| {
            offer
                .iter()
                .map(|r| json!({"key": r.key, "meta": r.meta}))
                .collect::<Vec<_>>()
        });
        let body =
            json!({"v": PROTOCOL, "who": keys[0], "keys": keys, "digest": id, "offer": list});
        let bytes = post(&url, body.to_string().as_bytes())?;
        let reply: Value = serde_json::from_slice(&bytes).map_err(|_| {
            "The server did not answer properly. Check the server address".to_owned()
        })?;
        if reply["ok"] != true {
            return Err(reply["error"]
                .as_str()
                .unwrap_or("The server refused the request")
                .to_owned());
        }
        Ok(reply)
    };
    let first = acked.as_deref() != Some(id.as_str());
    let mut reply = call(first)?;
    if !first && reply["have"].as_str() != Some(id.as_str()) {
        // The room forgot us (a day without a sync, or a new room): send the list again right away.
        reply = call(true)?;
    }
    *acked = reply["have"].as_str().map(str::to_owned);
    let received = net::accept(&reply["offer"], &shared::path_for(ledger))?;
    Ok(Exchange {
        received,
        sent: offer.len(),
    })
}
