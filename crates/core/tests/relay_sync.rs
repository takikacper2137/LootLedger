//! Friends sharing through an online room: each hands in their matches and gets back the ones they were in.
use lootledger_core::{
    model::{Opening, Player, Replay},
    relay, shared,
    store::Store,
};
use serde_json::{Value, json};
use std::{cell::RefCell, collections::BTreeMap, path::PathBuf};

fn player(epic: &str, name: &str) -> Player {
    Player {
        epic_id: Some(epic.into()),
        name: Some(name.into()),
        team: Some(1),
        bot: Some(false),
    }
}

fn opening(by: &str, who: Option<(&str, &str)>, n: u32) -> Opening {
    Opening {
        t: f64::from(n),
        cls: "Tiered_Chest_Athena_C".into(),
        kind: "opened".into(),
        by: by.into(),
        player: who.map(|w| w.1.into()),
        player_id: who.map(|w| w.0.into()),
        pawn: Some(1),
        actor: n,
        episode: 1,
    }
}

fn recording(
    session: &str,
    owner: (&str, &str),
    partner: (&str, &str),
    own: u32,
    seen: u32,
) -> Replay {
    Replay {
        file: format!("{session}-{}.replay", owner.1),
        ok: true,
        // Recent, because rooms only carry the last few days of matches.
        timestamp: Some(chrono::Utc::now().to_rfc3339()),
        length_seconds: 220.0,
        session_id: Some(session.into()),
        playlist: Some("Playlist_NoBuildBR_Duo".into()),
        owner: Some(player(owner.0, owner.1)),
        teammates: vec![player(partner.0, partner.1)],
        events: (0..own)
            .map(|n| opening("me", None, n))
            .chain((0..seen).map(|n| opening("team", Some(partner), 100 + n)))
            .collect(),
        ..Default::default()
    }
}

const KACPER: (&str, &str) = ("K1", "Kacper");
const OLA: (&str, &str) = ("O1", "Ola");
const STRANGER: (&str, &str) = ("X1", "Stranger");

struct Pc {
    _dir: tempfile::TempDir,
    ledger: PathBuf,
    store: Store,
}

fn pc(replays: &[Replay]) -> Pc {
    let dir = tempfile::tempdir().unwrap();
    let ledger = dir.path().join("ledger.sqlite");
    let mut store = Store::open(&ledger).unwrap();
    for (i, r) in replays.iter().enumerate() {
        store
            .import_replay(r, &format!("{i}.replay"), 1, 1.0)
            .unwrap();
    }
    Pc {
        _dir: dir,
        ledger,
        store,
    }
}

fn squad_total(pc: &Pc, name: &str) -> i64 {
    let snap = pc.store.snapshot(true, None).unwrap();
    let squad = snap.last.unwrap().squad.unwrap();
    squad.iter().find(|p| p.name == name).unwrap().total
}

/// What the room holds for one person: the digest of their list, the list, and their player ids.
type Held = (String, Vec<Value>, Vec<String>);

/// The room server's rule, as in infra/party-relay/src/logic.js.
#[derive(Default)]
struct FakeRoom {
    members: RefCell<BTreeMap<String, Held>>,
    calls: RefCell<Vec<(String, bool)>>,
}

impl FakeRoom {
    fn post(&self, url: &str, body: &[u8]) -> Result<Vec<u8>, String> {
        let req: Value = serde_json::from_slice(body).unwrap();
        let who = req["who"].as_str().unwrap().to_owned();
        let digest = req["digest"].as_str().unwrap().to_owned();
        let keys: Vec<String> = req["keys"]
            .as_array()
            .unwrap()
            .iter()
            .map(|k| k.as_str().unwrap().to_owned())
            .collect();
        let has_list = req["offer"].is_array();
        self.calls.borrow_mut().push((url.to_owned(), has_list));
        let mut members = self.members.borrow_mut();
        if has_list {
            members.insert(
                who.clone(),
                (
                    digest,
                    req["offer"].as_array().unwrap().clone(),
                    keys.clone(),
                ),
            );
        }
        let mut offer = Vec::new();
        for (id, (_, list, _)) in members.iter() {
            if *id == who {
                continue;
            }
            for item in list {
                let shared_match = item["meta"]["players"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|p| keys.iter().any(|k| p["key"].as_str() == Some(k)));
                if shared_match {
                    offer
                        .push(json!({"key": item["key"], "received": "now", "meta": item["meta"]}));
                }
            }
        }
        let have = members.get(&who).map(|m| m.0.clone());
        Ok(json!({"ok": true, "have": have, "offer": offer})
            .to_string()
            .into_bytes())
    }
}

const SERVER: &str = "party.example.workers.dev";
const ROOM: &str = "abcd1234efgh5678";

#[test]
fn friends_share_matches_through_a_room_and_nothing_is_sent_twice() {
    let room = FakeRoom::default();
    let post = |url: &str, body: &[u8]| room.post(url, body);
    // Kacper also played a match with a stranger who is not in the room.
    let kacper = pc(&[
        recording("s1", KACPER, OLA, 6, 7),
        recording("s2", KACPER, STRANGER, 3, 2),
    ]);
    let ola = pc(&[recording("s1", OLA, KACPER, 2, 1)]);
    let (mut k_ack, mut o_ack) = (None, None);

    let first = relay::sync(&post, SERVER, ROOM, &kacper.ledger, &mut k_ack).unwrap();
    assert_eq!(first.received, 0);
    assert_eq!(first.sent, 2);
    // Ola joins: she gets Kacper's s1 and is raised to what he saw; the stranger's match stays out.
    assert_eq!(squad_total(&ola, "Ola"), 2);
    let second = relay::sync(&post, SERVER, ROOM, &ola.ledger, &mut o_ack).unwrap();
    assert_eq!(second.received, 1);
    assert_eq!(squad_total(&ola, "Ola"), 7);
    assert_eq!(squad_total(&ola, "Kacper"), 6);
    let kept = shared::load(&shared::path_for(&ola.ledger));
    assert_eq!(kept.len(), 1);
    assert!(kept[0].key.starts_with("s1:"));
    // Kacper's next sync brings Ola's recording, without sending his list again.
    let third = relay::sync(&post, SERVER, ROOM, &kacper.ledger, &mut k_ack).unwrap();
    assert_eq!(third.received, 1);
    let calls = room.calls.borrow();
    assert!(!calls.last().unwrap().1);
    assert!(calls[0].0.ends_with(&format!("/v1/rooms/{ROOM}/sync")));
    drop(calls);
    // Syncing again changes nothing: nothing is new, and nothing is stored again.
    let again = relay::sync(&post, SERVER, ROOM, &ola.ledger, &mut o_ack).unwrap();
    assert_eq!(again.received, 0);
    // Nothing reached either ledger.
    assert_eq!(ola.store.totals(true).unwrap().chests, 2);
    assert_eq!(kacper.store.totals(true).unwrap().chests, 9);
}

#[test]
fn a_room_that_forgot_us_gets_the_list_again_at_once() {
    let room = FakeRoom::default();
    let post = |url: &str, body: &[u8]| room.post(url, body);
    let kacper = pc(&[recording("s1", KACPER, OLA, 6, 7)]);
    let mut ack = None;
    relay::sync(&post, SERVER, ROOM, &kacper.ledger, &mut ack).unwrap();
    room.members.borrow_mut().clear();
    room.calls.borrow_mut().clear();
    relay::sync(&post, SERVER, ROOM, &kacper.ledger, &mut ack).unwrap();
    let calls = room.calls.borrow();
    // First without the list (the room was believed to hold it), then again with it.
    assert_eq!(calls.iter().map(|c| c.1).collect::<Vec<_>>(), [false, true]);
    assert_eq!(room.members.borrow().len(), 1);
}

#[test]
fn nothing_is_sent_before_one_of_your_matches_is_known() {
    let room = FakeRoom::default();
    let post = |url: &str, body: &[u8]| room.post(url, body);
    let empty = pc(&[]);
    let moved = relay::sync(&post, SERVER, ROOM, &empty.ledger, &mut None).unwrap();
    assert_eq!((moved.received, moved.sent), (0, 0));
    assert!(room.calls.borrow().is_empty());
}

#[test]
fn server_and_room_problems_are_explained() {
    let kacper = pc(&[recording("s1", KACPER, OLA, 6, 7)]);
    let refuse = |_: &str, _: &[u8]| Ok(br#"{"ok":false,"error":"This room is full"}"#.to_vec());
    let err = relay::sync(&refuse, SERVER, ROOM, &kacper.ledger, &mut None).unwrap_err();
    assert_eq!(err, "This room is full");
    let html = |_: &str, _: &[u8]| Ok(b"<html>Error 1027</html>".to_vec());
    let err = relay::sync(&html, SERVER, ROOM, &kacper.ledger, &mut None).unwrap_err();
    assert!(err.contains("server address"), "{err}");
    let down = |_: &str, _: &[u8]| Err("Could not reach the server".to_owned());
    assert!(relay::sync(&down, SERVER, ROOM, &kacper.ledger, &mut None).is_err());
}

#[test]
fn addresses_and_room_codes_are_checked() {
    assert_eq!(
        relay::endpoint("https://party.example.dev/", "abcd1234efgh5678").unwrap(),
        "https://party.example.dev/v1/rooms/abcd1234efgh5678/sync"
    );
    assert_eq!(
        relay::endpoint(" party.example.dev ", "abcd1234efgh5678").unwrap(),
        "https://party.example.dev/v1/rooms/abcd1234efgh5678/sync"
    );
    assert!(relay::endpoint("http://party.example.dev", "abcd1234efgh5678").is_err());
    assert!(relay::endpoint("", "abcd1234efgh5678").is_err());
    assert!(relay::endpoint("party.example.dev", "short").is_err());
    assert!(relay::endpoint("party.example.dev", "has a space in it!").is_err());
    let code = relay::new_room_code();
    assert!(relay::endpoint("x.dev", &code).is_ok(), "{code}");
}
