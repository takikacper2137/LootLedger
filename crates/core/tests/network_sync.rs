//! One player hosts, another connects: both end up with the best recording of every match they played together.
use lootledger_core::{
    model::{Opening, Player, Replay},
    net, shared,
    store::Store,
};
use std::path::PathBuf;
use std::sync::mpsc;

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
        timestamp: Some("2026-11-11T03:31:13Z".into()),
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

#[test]
fn a_friend_connecting_to_the_host_shares_only_the_matches_they_played_together() {
    // Kacper (host) also played a match with a stranger; Ola must not receive that one.
    let kacper = pc(&[
        recording("s1", KACPER, OLA, 6, 7),
        recording("s2", KACPER, STRANGER, 3, 2),
    ]);
    let ola = pc(&[recording("s1", OLA, KACPER, 2, 1)]);
    let (tx, rx) = mpsc::channel();
    let host = net::Host::start(kacper.ledger.clone(), 0, "secret".into(), move |r| {
        tx.send(r).ok();
    })
    .unwrap();
    let address = format!("127.0.0.1:{}", host.port());

    assert_eq!(squad_total(&ola, "Ola"), 2);
    let moved = net::sync(&address, "secret", &ola.ledger).unwrap();
    // Ola got Kacper's s1 summary only; Kacper got Ola's.
    assert_eq!(
        moved,
        net::Exchange {
            received: 1,
            sent: 1
        }
    );
    assert_eq!(
        rx.recv().unwrap().unwrap(),
        net::Exchange {
            received: 1,
            sent: 1
        }
    );

    // Ola's slow recording is raised to what Kacper saw, and Kacper's own count is shown too.
    assert_eq!(squad_total(&ola, "Ola"), 7);
    assert_eq!(squad_total(&ola, "Kacper"), 6);
    let kept = shared::load(&shared::path_for(&ola.ledger));
    assert_eq!(kept.len(), 1);
    assert!(kept[0].key.starts_with("s1:"));
    // Nothing reached either ledger.
    assert_eq!(ola.store.totals(true).unwrap().chests, 2);
    assert_eq!(kacper.store.totals(true).unwrap().chests, 9);
    // Kacper holds Ola's recording of s1 too.
    assert_eq!(shared::load(&shared::path_for(&kacper.ledger)).len(), 1);
}

#[test]
fn a_wrong_room_password_is_refused_and_nothing_is_stored() {
    let kacper = pc(&[recording("s1", KACPER, OLA, 6, 7)]);
    let ola = pc(&[recording("s1", OLA, KACPER, 2, 1)]);
    let (tx, rx) = mpsc::channel();
    let host = net::Host::start(kacper.ledger.clone(), 0, "secret".into(), move |r| {
        tx.send(r).ok();
    })
    .unwrap();
    let address = format!("127.0.0.1:{}", host.port());
    let err = net::sync(&address, "guess", &ola.ledger).unwrap_err();
    assert!(err.contains("Wrong room password"), "{err}");
    assert!(rx.recv().unwrap().is_err());
    assert!(shared::load(&shared::path_for(&ola.ledger)).is_empty());
    assert_eq!(squad_total(&ola, "Ola"), 2);
}

#[test]
fn problems_are_explained_in_plain_words() {
    let ola = pc(&[recording("s1", OLA, KACPER, 2, 1)]);
    assert!(
        net::sync("", "x", &ola.ledger)
            .unwrap_err()
            .contains("address")
    );
    // Nothing listens on this port.
    let err = net::sync("127.0.0.1:1", "x", &ola.ledger).unwrap_err();
    assert!(err.contains("Could not reach"), "{err}");
}

#[test]
fn a_stopped_host_lets_go_of_its_port() {
    let kacper = pc(&[recording("s1", KACPER, OLA, 6, 7)]);
    let first = net::Host::start(kacper.ledger.clone(), 0, "a".into(), |_| {}).unwrap();
    let port = first.port();
    drop(first);
    // Starting again on the same port works once the old listener has noticed it was stopped.
    let again = net::Host::start(kacper.ledger.clone(), port, "a".into(), |_| {}).unwrap();
    assert_eq!(again.port(), port);
}
