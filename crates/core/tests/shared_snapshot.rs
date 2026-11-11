//! A partner's recording raises the teammates' numbers in the dashboard without touching the ledger.
use lootledger_core::{
    model::{Opening, Player, Replay},
    shared,
    store::Store,
};

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

fn recording(owner: (&str, &str), partner: (&str, &str), own: u32, seen: u32) -> Replay {
    Replay {
        file: format!("{}.replay", owner.1),
        ok: true,
        timestamp: Some("2026-11-11T03:31:13Z".into()),
        length_seconds: 220.0,
        session_id: Some("97c8efa57b8e45258b8a4e739699cd29".into()),
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

#[test]
fn the_dashboard_uses_the_best_recording_and_the_ledger_is_untouched() {
    let dir = tempfile::tempdir().unwrap();
    let ledger = dir.path().join("ledger.sqlite");
    let mut store = Store::open(&ledger).unwrap();
    let (kacper, ola) = (("K1", "Kacper"), ("O1", "Ola"));
    // Ola's PC kept only 2 of her own chests; Kacper watched her open 7.
    store
        .import_replay(&recording(ola, kacper, 2, 1), "ola.replay", 1, 1.0)
        .unwrap();
    let alone = store.snapshot(true, None).unwrap();
    assert_eq!((alone.all, alone.team_total), (2, 1));

    let theirs = shared::remote_from_replay(&recording(kacper, ola, 6, 7), "now").unwrap();
    shared::add(&shared::path_for(&ledger), theirs).unwrap();
    let merged = store.snapshot(true, None).unwrap();

    // Teammate numbers use Kacper's own 6, but Ola's chests in the ledger are still just her own 2.
    assert_eq!(merged.team_total, 6);
    assert_eq!(merged.all, 2);
    let squad = merged.last.as_ref().unwrap().squad.as_ref().unwrap();
    let total = |name: &str| squad.iter().find(|p| p.name == name).unwrap().total;
    assert_eq!((total("Ola"), total("Kacper")), (7, 6));
    assert!(
        merged
            .last
            .as_ref()
            .unwrap()
            .shared_note
            .as_deref()
            .is_some_and(|n| n.contains("you: 2 → 7"))
    );
    assert_eq!(store.totals(true).unwrap().chests, 2);
}
