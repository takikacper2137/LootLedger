//! Shared matches: replays recorded by several players of the same match, merged into one set of numbers.
//!
//! Every recording knows its owner's chests exactly and sees only some of a teammate's, and a slow PC may
//! record very little. So each player's merged count is the highest any recording has for them, never a sum
//! (two recordings of the same chest must not count twice). A partner's recording is kept in `shared.json`
//! beside the ledger and never enters the ledger, so it cannot change your own totals.
use crate::model::Replay;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use std::path::{Path, PathBuf};

/// What is kept of one partner's recording: its match summary, keyed like the ledger (`session:owner`).
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct Remote {
    pub key: String,
    pub received: String,
    pub meta: Value,
}

/// Where the partner recordings live for a ledger at `ledger`.
pub fn path_for(ledger: &Path) -> PathBuf {
    ledger.with_file_name("shared.json")
}

/// The game session a ledger key belongs to; `None` for keys built from a file name.
pub fn session_of(key: &str) -> Option<&str> {
    let (session, _) = key.split_once(':')?;
    (!session.is_empty() && session != "file").then_some(session)
}

/// A partner's recording, ready to store. Refused when it cannot be tied to a game session or has no owner.
pub fn remote_from_replay(replay: &Replay, received: &str) -> Result<Remote, String> {
    if !replay.ok {
        return Err(replay
            .error
            .clone()
            .unwrap_or_else(|| "That replay could not be read".into()));
    }
    let owner = replay.owner.as_ref().and_then(|p| p.epic_id.as_deref());
    if owner.is_none_or(str::is_empty) {
        return Err("That replay does not say who recorded it".into());
    }
    let key = replay.key();
    if session_of(&key).is_none() {
        return Err("That replay has no match id, so it cannot be matched with yours".into());
    }
    let (_, mut meta) = replay.summarize();
    meta["key"] = json!(key);
    Ok(Remote {
        key,
        received: received.to_owned(),
        meta,
    })
}

/// Stored partner recordings; a missing or damaged file reads as none.
pub fn load(path: &Path) -> Vec<Remote> {
    std::fs::read(path)
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

/// Store a partner recording (replacing an earlier one of the same match and owner). Written atomically.
pub fn add(path: &Path, remote: Remote) -> std::io::Result<()> {
    let mut all = load(path);
    all.retain(|r| r.key != remote.key);
    all.push(remote);
    let temp = path.with_extension("tmp");
    std::fs::create_dir_all(path.parent().unwrap_or(Path::new(".")))?;
    std::fs::write(
        &temp,
        serde_json::to_vec_pretty(&all).map_err(std::io::Error::other)?,
    )?;
    std::fs::rename(&temp, path)
}

fn counted(counts: &Value, include_broken: bool) -> i64 {
    counts
        .as_object()
        .into_iter()
        .flatten()
        .map(|(_, c)| {
            c["opened"].as_i64().unwrap_or(0)
                + if include_broken {
                    c["broken"].as_i64().unwrap_or(0)
                } else {
                    0
                }
        })
        .sum()
}

/// Raise each player's counts in `meta` to the best any partner recording of the same match has for them.
/// Rewrites the teammate totals to match, and leaves a plain-words note in `meta["shared_note"]`.
/// Returns how many players were raised.
pub fn enrich(meta: &mut Value, remotes: &[Remote], include_broken: bool) -> usize {
    let Some(own_key) = meta["key"].as_str().map(str::to_owned) else {
        return 0;
    };
    let Some(session) = session_of(&own_key) else {
        return 0;
    };
    let partners: Vec<&Remote> = remotes
        .iter()
        .filter(|r| r.key != own_key && session_of(&r.key) == Some(session))
        .collect();
    if partners.is_empty() || !meta["players"].is_array() {
        return 0;
    }
    let mut raised: Vec<(String, bool, i64, i64)> = Vec::new();
    for player in meta["players"].as_array_mut().into_iter().flatten() {
        let key = player["key"].as_str().unwrap_or("").to_owned();
        if key.is_empty() {
            continue;
        }
        let before = counted(&player["counts"], include_broken);
        let mut best = (before, None::<Value>);
        for partner in &partners {
            for other in partner.meta["players"].as_array().into_iter().flatten() {
                if other["key"].as_str() != Some(key.as_str()) {
                    continue;
                }
                let total = counted(&other["counts"], include_broken);
                if total > best.0 {
                    best = (total, Some(other["counts"].clone()));
                }
            }
        }
        if let (after, Some(counts)) = best {
            player["counts"] = counts;
            raised.push((
                player["name"].as_str().unwrap_or("Teammate").to_owned(),
                player["me"].as_bool().unwrap_or(false),
                before,
                after,
            ));
        }
    }
    // The teammate totals are the sum of every player but you.
    let (mut team, mut team_players) = (Map::new(), Map::new());
    for player in meta["players"].as_array().into_iter().flatten() {
        if player["me"].as_bool().unwrap_or(false) {
            continue;
        }
        let mut sum = 0;
        for (name, c) in player["counts"].as_object().into_iter().flatten() {
            let (o, b) = (
                c["opened"].as_i64().unwrap_or(0),
                c["broken"].as_i64().unwrap_or(0),
            );
            let t = team
                .entry(name.clone())
                .or_insert(json!({"opened": 0, "broken": 0}));
            t["opened"] = json!(t["opened"].as_i64().unwrap_or(0) + o);
            t["broken"] = json!(t["broken"].as_i64().unwrap_or(0) + b);
            sum += o + b;
        }
        if sum > 0 {
            team_players.insert(
                player["name"].as_str().unwrap_or("Teammate").to_owned(),
                json!(sum),
            );
        }
    }
    meta["team"] = Value::Object(team);
    meta["team_players"] = Value::Object(team_players);
    meta["shared_note"] = json!(note(partners.len(), &raised));
    raised.len()
}

fn note(partners: usize, raised: &[(String, bool, i64, i64)]) -> String {
    let head = format!(
        "Shared match: merged with {partners} other recording{}.",
        if partners == 1 { "" } else { "s" }
    );
    if raised.is_empty() {
        return format!("{head} Your recording already had everyone's chests.");
    }
    let parts: Vec<String> = raised
        .iter()
        .map(|(name, me, before, after)| {
            let who = if *me { "you" } else { name.as_str() };
            format!("{who}: {before} → {after}")
        })
        .collect();
    format!(
        "{head} Raised from the other recordings: {}.",
        parts.join(", ")
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Opening, Player};

    fn player(epic: &str, name: &str) -> Player {
        Player {
            epic_id: Some(epic.into()),
            name: Some(name.into()),
            team: Some(1),
            bot: Some(false),
        }
    }
    fn opening(by: &str, who: Option<(&str, &str)>, kind: &str, n: u32) -> Opening {
        Opening {
            t: f64::from(n),
            cls: "Tiered_Chest_Athena_C".into(),
            kind: kind.into(),
            by: by.into(),
            player: who.map(|w| w.1.into()),
            player_id: who.map(|w| w.0.into()),
            pawn: Some(1),
            actor: n,
            episode: 1,
        }
    }
    /// A replay of `owner` that records `own` of their own chests and sees `seen` of the partner's.
    fn recording(
        session: &str,
        owner: (&str, &str),
        partner: (&str, &str),
        own: u32,
        seen: u32,
    ) -> Replay {
        let events = (0..own)
            .map(|n| opening("me", None, "opened", n))
            .chain((0..seen).map(|n| opening("team", Some(partner), "opened", 100 + n)))
            .collect();
        Replay {
            file: format!("{session}-{}.replay", owner.1),
            ok: true,
            session_id: Some(session.into()),
            owner: Some(player(owner.0, owner.1)),
            teammates: vec![player(partner.0, partner.1)],
            events,
            ..Default::default()
        }
    }
    fn meta_of(replay: &Replay) -> Value {
        let (_, mut meta) = replay.summarize();
        meta["key"] = json!(replay.key());
        meta
    }
    fn total_of(meta: &Value, name: &str) -> i64 {
        let p = meta["players"]
            .as_array()
            .unwrap()
            .iter()
            .find(|p| p["name"] == name)
            .unwrap();
        counted(&p["counts"], true)
    }

    const KACPER: (&str, &str) = ("K1", "Kacper");
    const OLA: (&str, &str) = ("O1", "Ola");

    #[test]
    fn a_slow_partners_chests_come_from_what_you_saw() {
        // The real pair: Kacper's replay has 6 own and 7 of Ola's; Ola's slow PC kept 2 own and 1 of Kacper's.
        let kacper = recording("s1", KACPER, OLA, 6, 7);
        let ola = recording("s1", OLA, KACPER, 2, 1);
        let remote = remote_from_replay(&ola, "now").unwrap();
        let mut meta = meta_of(&kacper);
        assert_eq!(
            enrich(&mut meta, &[remote], true),
            0,
            "nothing to raise on the good recording"
        );
        assert_eq!((total_of(&meta, "Kacper"), total_of(&meta, "Ola")), (6, 7));
        assert_eq!(meta["team"]["Chest"]["opened"], 7);
        assert_eq!(meta["team_players"]["Ola"], 7);
    }

    #[test]
    fn the_slow_partner_sees_the_full_picture_from_your_recording() {
        let kacper = recording("s1", KACPER, OLA, 6, 7);
        let ola = recording("s1", OLA, KACPER, 2, 1);
        let remote = remote_from_replay(&kacper, "now").unwrap();
        let mut meta = meta_of(&ola);
        assert_eq!(enrich(&mut meta, &[remote], true), 2);
        // Ola's own count was 2 but Kacper watched 7; Kacper's 1 seen by Ola is raised to his own 6.
        assert_eq!((total_of(&meta, "Ola"), total_of(&meta, "Kacper")), (7, 6));
        assert_eq!(meta["team"]["Chest"]["opened"], 6);
        assert_eq!(meta["team_players"]["Kacper"], 6);
        let note = meta["shared_note"].as_str().unwrap();
        assert!(
            note.contains("you: 2 → 7") && note.contains("Kacper: 1 → 6"),
            "{note}"
        );
    }

    #[test]
    fn other_matches_and_your_own_recording_change_nothing() {
        let kacper = recording("s1", KACPER, OLA, 6, 7);
        let elsewhere = remote_from_replay(&recording("s2", OLA, KACPER, 20, 20), "now").unwrap();
        let itself = remote_from_replay(&kacper, "now").unwrap();
        let mut meta = meta_of(&kacper);
        let before = meta.clone();
        assert_eq!(enrich(&mut meta, &[elsewhere, itself], true), 0);
        assert_eq!(meta, before);
    }

    #[test]
    fn broken_chests_follow_the_setting() {
        let mut ola = recording("s1", OLA, KACPER, 0, 0);
        ola.events = vec![
            opening("me", None, "broken", 1),
            opening("me", None, "broken", 2),
        ];
        let remote = remote_from_replay(&ola, "now").unwrap();
        let kacper = recording("s1", KACPER, OLA, 1, 1);
        let mut counted_broken = meta_of(&kacper);
        assert_eq!(
            enrich(&mut counted_broken, std::slice::from_ref(&remote), true),
            1
        );
        assert_eq!(total_of(&counted_broken, "Ola"), 2);
        let mut ignored = meta_of(&kacper);
        assert_eq!(
            enrich(&mut ignored, &[remote], false),
            0,
            "2 broken do not beat 1 opened"
        );
    }

    #[test]
    fn unusable_recordings_are_refused_with_a_reason() {
        let mut bad = recording("s1", OLA, KACPER, 1, 0);
        bad.ok = false;
        bad.error = Some("Encrypted".into());
        assert_eq!(remote_from_replay(&bad, "now").unwrap_err(), "Encrypted");
        let mut anonymous = recording("s1", OLA, KACPER, 1, 0);
        anonymous.owner = None;
        assert!(remote_from_replay(&anonymous, "now").is_err());
        let mut no_session = recording("s1", OLA, KACPER, 1, 0);
        no_session.session_id = None;
        assert!(
            remote_from_replay(&no_session, "now")
                .unwrap_err()
                .contains("match id")
        );
        assert_eq!(session_of("file:abc.replay"), None);
        assert_eq!(session_of("abc:EPIC"), Some("abc"));
    }

    #[test]
    fn stored_recordings_survive_a_restart_and_are_replaced_by_the_same_owner() {
        let dir = tempfile::tempdir().unwrap();
        let path = path_for(&dir.path().join("ledger.sqlite"));
        assert!(load(&path).is_empty());
        let first = remote_from_replay(&recording("s1", OLA, KACPER, 2, 1), "one").unwrap();
        add(&path, first).unwrap();
        let again = remote_from_replay(&recording("s1", OLA, KACPER, 5, 1), "two").unwrap();
        add(&path, again.clone()).unwrap();
        add(
            &path,
            remote_from_replay(&recording("s2", OLA, KACPER, 1, 1), "three").unwrap(),
        )
        .unwrap();
        let all = load(&path);
        assert_eq!(all.len(), 2);
        assert!(all.contains(&again));
        std::fs::write(&path, b"not json").unwrap();
        assert!(load(&path).is_empty());
    }
}
