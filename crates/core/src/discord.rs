//! Discord "Rich Presence": the line under your name in Discord, e.g. "8,838 chests · today 640".
//! Discord's desktop app listens on a local pipe; this module only builds and reads the messages (no network,
//! nothing leaves the PC except to the Discord app running on it), so it can be tested anywhere.
use serde_json::{Value, json};
use std::io::{self, Read};

const OP_HANDSHAKE: u32 = 0;
const OP_FRAME: u32 = 1;
/// The name the picture must be uploaded under in the Discord developer portal (Rich Presence > Art Assets).
pub const IMAGE_KEY: &str = "lootledger";

/// What is shown under the player's name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Activity {
    /// First line, e.g. "In a match · all time 8,838".
    pub details: String,
    /// Second line, e.g. "Today 640 · best match 111".
    pub state: String,
    /// Unix seconds the current match started (Discord then shows an elapsed timer).
    pub started: Option<i64>,
}

/// Application ids are 17–20 digits (the number shown in the Discord developer portal).
pub fn valid_client_id(id: &str) -> bool {
    (17..=20).contains(&id.len()) && id.bytes().all(|b| b.is_ascii_digit())
}

fn frame(op: u32, payload: &Value) -> Vec<u8> {
    let body = payload.to_string().into_bytes();
    let mut out = Vec::with_capacity(8 + body.len());
    out.extend(op.to_le_bytes());
    out.extend((body.len() as u32).to_le_bytes());
    out.extend(body);
    out
}

/// The first message after connecting.
pub fn handshake(client_id: &str) -> Vec<u8> {
    frame(OP_HANDSHAKE, &json!({"v": 1, "client_id": client_id}))
}

/// Set (or, with `None`, clear) the shown activity. `nonce` can be any text that differs per message.
pub fn set_activity(pid: u32, nonce: &str, activity: Option<&Activity>) -> Vec<u8> {
    let activity = activity.map(|a| {
        // The picture is an "art asset" uploaded to the Discord application under this key.
        let mut v = json!({
            "details": clip(&a.details),
            "state": clip(&a.state),
            "assets": {"large_image": IMAGE_KEY, "large_text": "LootLedger"},
        });
        if let Some(start) = a.started {
            v["timestamps"] = json!({"start": start});
        }
        v
    });
    frame(
        OP_FRAME,
        &json!({
            "cmd": "SET_ACTIVITY",
            "args": {"pid": pid, "activity": activity},
            "nonce": nonce,
        }),
    )
}

/// Discord shows at most 128 characters per line and refuses empty ones.
fn clip(text: &str) -> String {
    let text: String = text.chars().take(128).collect();
    if text.chars().count() < 2 {
        format!("{text}  ")
    } else {
        text
    }
}

/// Read one message from Discord: its opcode and JSON body.
pub fn read_frame(from: &mut impl Read) -> io::Result<(u32, Value)> {
    let mut head = [0u8; 8];
    from.read_exact(&mut head)?;
    let op = u32::from_le_bytes([head[0], head[1], head[2], head[3]]);
    let len = u32::from_le_bytes([head[4], head[5], head[6], head[7]]) as usize;
    if len > 64 * 1024 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Discord sent an unexpectedly large message",
        ));
    }
    let mut body = vec![0u8; len];
    from.read_exact(&mut body)?;
    let value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    Ok((op, value))
}

/// The two lines for the current moment: while a match runs and otherwise.
/// `live_secs` is how long the running match has been going, if one is.
pub fn presence(
    all: &str,
    today: &str,
    best: Option<&str>,
    live_secs: Option<i64>,
    now: i64,
) -> Activity {
    match live_secs {
        Some(secs) => Activity {
            details: "In a match".to_owned(),
            state: format!("{all} chests all time"),
            started: Some(now - secs.max(0)),
        },
        None => Activity {
            details: format!("{all} chests opened"),
            state: match best {
                Some(b) => format!("Today {today} · best match {b}"),
                None => format!("Today {today}"),
            },
            started: None,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn messages_are_framed_the_way_discord_expects() {
        let hello = handshake("123456789012345678");
        assert_eq!(&hello[..4], &0u32.to_le_bytes());
        let len = u32::from_le_bytes(hello[4..8].try_into().unwrap()) as usize;
        assert_eq!(len, hello.len() - 8);
        let body: Value = serde_json::from_slice(&hello[8..]).unwrap();
        assert_eq!(body["client_id"], "123456789012345678");
        assert_eq!(body["v"], 1);

        let a = Activity {
            details: "8,838 chests opened".into(),
            state: "Today 640".into(),
            started: Some(1_800_000_000),
        };
        let msg = set_activity(42, "n1", Some(&a));
        let mut cursor = Cursor::new(msg);
        let (op, v) = read_frame(&mut cursor).unwrap();
        assert_eq!(op, 1);
        assert_eq!(v["cmd"], "SET_ACTIVITY");
        assert_eq!(v["args"]["pid"], 42);
        assert_eq!(v["args"]["activity"]["details"], "8,838 chests opened");
        assert_eq!(v["args"]["activity"]["assets"]["large_image"], IMAGE_KEY);
        assert_eq!(v["args"]["activity"]["timestamps"]["start"], 1_800_000_000);

        let cleared: Value = {
            let mut c = Cursor::new(set_activity(42, "n2", None));
            read_frame(&mut c).unwrap().1
        };
        assert!(cleared["args"]["activity"].is_null());
    }

    #[test]
    fn long_or_empty_lines_are_made_acceptable() {
        let a = Activity {
            details: "x".repeat(300),
            state: String::new(),
            started: None,
        };
        let mut c = Cursor::new(set_activity(1, "n", Some(&a)));
        let (_, v) = read_frame(&mut c).unwrap();
        assert_eq!(
            v["args"]["activity"]["details"].as_str().unwrap().len(),
            128
        );
        assert!(v["args"]["activity"]["state"].as_str().unwrap().len() >= 2);
        assert!(v["args"]["activity"].get("timestamps").is_none());
    }

    #[test]
    fn oversized_or_cut_off_messages_are_refused() {
        let mut big = Vec::new();
        big.extend(1u32.to_le_bytes());
        big.extend((10_000_000u32).to_le_bytes());
        assert!(read_frame(&mut Cursor::new(big)).is_err());
        assert!(read_frame(&mut Cursor::new(vec![1, 0, 0])).is_err());
    }

    #[test]
    fn application_ids_are_long_numbers() {
        assert!(valid_client_id("123456789012345678"));
        assert!(!valid_client_id("1234"));
        assert!(!valid_client_id("12345678901234567x"));
        assert!(!valid_client_id(""));
    }

    #[test]
    fn the_lines_follow_the_match_state() {
        let idle = presence("8,838", "640", Some("111"), None, 1000);
        assert_eq!(idle.details, "8,838 chests opened");
        assert_eq!(idle.state, "Today 640 · best match 111");
        assert_eq!(idle.started, None);
        let live = presence("8,838", "640", Some("111"), Some(90), 1000);
        assert_eq!(live.details, "In a match");
        assert_eq!(live.started, Some(910));
    }
}
