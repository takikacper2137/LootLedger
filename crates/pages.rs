//! Matches, Openings and Teammates: filters, sorting, selection and the detail panels, as in app.py and
//! teammates.py of LootLedger 0.7.1.

use crate::cards::{color, model, series_path};
use crate::prefs::{Prefs, RANGES, elapsed};
use crate::theme::Theme;
use crate::{AppWindow, BarRow, ChartPoint, Mark, Pages, Row};
use chrono::{DateTime, Duration, Local};
use lootledger_core::stats::{self, MatchRow, Person, Snapshot};
use lootledger_core::store::{Event, local_time};
use serde_json::{Map, Value, json};
use slint::{Color, ComponentHandle, SharedString};

pub const EVENT_FIELDS: [&str; 11] = [
    "id",
    "match_id",
    "created",
    "video_time",
    "status",
    "chest_type",
    "amount",
    "source",
    "confidence",
    "reason",
    "evidence",
];
const PEOPLE_FIELDS: [&str; 11] = [
    "name",
    "you",
    "bot",
    "matches",
    "chests",
    "per_match",
    "searched",
    "broken_open",
    "rare_or_better",
    "best_match",
    "last_together",
];

#[derive(Default)]
pub struct State {
    pub entries: Vec<(Event, DateTime<Local>)>,
    pub match_sort: (i32, bool),
    pub entry_sort: (i32, bool),
    pub team_sort: (i32, bool),
    pub selected_match: String,
    pub selected_entry: String,
    pub selected_person: String,
    pub filters_ready: bool,
    pub modes: Vec<String>,
    pub types: Vec<String>,
    pub match_ids: Vec<String>,
    pub visible_entries: Vec<usize>,
    pub ranked: Vec<Person>,
}

impl State {
    pub fn new() -> State {
        State {
            match_sort: (0, true),
            entry_sort: (0, true),
            team_sort: (2, true),
            ..Default::default()
        }
    }
}

fn s(text: impl Into<SharedString>) -> SharedString {
    text.into()
}

fn row(id: &str, cells: Vec<String>) -> Row {
    Row {
        id: s(id),
        cells: model(cells.into_iter().map(SharedString::from).collect()),
        ink: Color::default(),
        has_ink: false,
        share: 0.0,
    }
}

fn plural(n: i64, word: &str) -> String {
    format!("{n} {word}{}", if n == 1 { "" } else { "es" })
}

fn people(n: usize) -> String {
    if n == 1 {
        "1 person".into()
    } else {
        format!("{n} people")
    }
}

const HOW: [(&str, &str); 4] = [
    ("replay", "Opened"),
    ("replay-broken", "Broken open"),
    ("manual correction", "Manual"),
    ("automatic", "Screen (old)"),
];

fn how(source: &str) -> String {
    HOW.iter()
        .find(|(k, _)| *k == source)
        .map_or(source.to_owned(), |(_, v)| (*v).to_owned())
}

fn status(status: &str) -> String {
    match status {
        "confirmed" => "Counted".into(),
        "undone" => "Removed".into(),
        "review" => "Review".into(),
        "rejected" => "Rejected".into(),
        other => {
            let mut c = other.chars();
            c.next()
                .map(|f| f.to_uppercase().collect::<String>() + c.as_str())
                .unwrap_or_default()
        }
    }
}

/// Today plus the last N-1 days; 0 means all time.
fn since(index: i32, today: chrono::NaiveDate) -> Option<chrono::NaiveDate> {
    let days = RANGES.get(index as usize).copied().unwrap_or(30);
    (days > 0).then(|| today - Duration::days(days - 1))
}

/// Refill every page from a new snapshot (and the openings list).
pub fn fill(
    ui: &AppWindow,
    st: &mut State,
    snap: &Snapshot,
    p: &Prefs,
    t: &Theme,
    count_broken: bool,
) {
    let g = ui.global::<Pages>();
    let mut modes: Vec<String> = snap.modes.iter().map(|m| m.0.clone()).collect();
    modes.sort();
    let mut types: Vec<String> = snap
        .types
        .keys()
        .map(str::to_owned)
        .chain(st.entries.iter().map(|(e, _)| e.chest_type.clone()))
        .collect();
    types.sort();
    types.dedup();
    let keep = |old: &[String], index: i32, new: &[String]| -> i32 {
        old.get((index - 1).max(0) as usize)
            .filter(|_| index > 0)
            .and_then(|name| new.iter().position(|n| n == name))
            .map_or(0, |i| i as i32 + 1)
    };
    let (old_modes, old_types) = (std::mem::take(&mut st.modes), std::mem::take(&mut st.types));
    g.set_match_mode(keep(&old_modes, g.get_match_mode(), &modes));
    g.set_team_mode(keep(&old_modes, g.get_team_mode(), &modes));
    g.set_match_type(keep(&old_types, g.get_match_type(), &types));
    g.set_entry_type(keep(&old_types, g.get_entry_type(), &types));
    g.set_modes(model(
        std::iter::once("All modes".to_owned())
            .chain(modes.iter().cloned())
            .map(SharedString::from)
            .collect(),
    ));
    g.set_types(model(
        std::iter::once("All types".to_owned())
            .chain(types.iter().cloned())
            .map(SharedString::from)
            .collect(),
    ));
    st.modes = modes;
    st.types = types;
    let old_match = g.get_entry_match();
    let old_id = st
        .match_ids
        .get((old_match - 1).max(0) as usize)
        .filter(|_| old_match > 0)
        .cloned();
    st.match_ids = snap.match_rows.iter().map(|r| r.id.clone()).collect();
    g.set_match_names(model(
        std::iter::once(s("All matches"))
            .chain(
                snap.match_rows
                    .iter()
                    .map(|r| s(format!("{} · {}", r.mode, p.when(r.when)))),
            )
            .collect(),
    ));
    let index = match old_id {
        Some(id) => st
            .match_ids
            .iter()
            .position(|m| *m == id)
            .map_or(0, |i| i as i32 + 1),
        None if !st.filters_ready && !st.match_ids.is_empty() => 1, // start with the latest match
        None => 0,
    };
    g.set_entry_match(index);
    st.filters_ready = true;
    g.set_chart_style(
        ["bar", "line", "smooth", "area"]
            .iter()
            .position(|x| *x == p.chart_style)
            .unwrap_or(0) as i32,
    );
    matches(ui, st, snap, p, t, count_broken);
    entries(ui, st, snap, p, t);
    team(ui, st, snap, p, t);
}

fn type_chests(row: &MatchRow, kind: Option<&str>) -> i64 {
    kind.map_or(row.count, |k| row.types.get(k))
}

pub fn matches(
    ui: &AppWindow,
    st: &State,
    snap: &Snapshot,
    p: &Prefs,
    t: &Theme,
    count_broken: bool,
) {
    let g = ui.global::<Pages>();
    let kind = (g.get_match_type() > 0)
        .then(|| st.types.get(g.get_match_type() as usize - 1).cloned())
        .flatten();
    let mode = (g.get_match_mode() > 0)
        .then(|| st.modes.get(g.get_match_mode() as usize - 1).cloned())
        .flatten();
    let since = since(g.get_match_range(), snap.today_date);
    let minimum = g.get_match_minimum() as i64;
    let favorites = g.get_match_favorites();
    let text = g.get_match_search().trim().to_lowercase();
    let mut rows: Vec<&MatchRow> = snap
        .match_rows
        .iter()
        .filter(|r| {
            let count = type_chests(r, kind.as_deref());
            !(kind.is_some() && count == 0)
                && count >= minimum
                && mode.as_ref().is_none_or(|m| *m == r.mode)
                && (!favorites || r.favorite)
                && since.is_none_or(|d| r.when.date_naive() >= d)
                && (text.is_empty()
                    || format!(
                        "{} {} {} {}",
                        r.mode,
                        r.when.format("%Y-%m-%d"),
                        r.tags,
                        r.note
                    )
                    .to_lowercase()
                    .contains(&text))
        })
        .collect();
    let (column, descending) = st.match_sort;
    rows.sort_by(|a, b| {
        let key = |r: &MatchRow| -> (f64, String) {
            match column {
                1 => (0.0, r.mode.to_lowercase()),
                2 => (r.placement.unwrap_or(1_000_000) as f64, String::new()),
                3 => (type_chests(r, kind.as_deref()) as f64, String::new()),
                4 => (r.broken as f64, String::new()),
                5 => (r.team as f64, String::new()),
                6 => (r.ammo as f64, String::new()),
                7 => (r.seconds, String::new()),
                _ => (r.when.timestamp() as f64, String::new()),
            }
        };
        let (x, y) = (key(a), key(b));
        let order = x.0.total_cmp(&y.0).then(x.1.cmp(&y.1));
        if descending { order.reverse() } else { order }
    });
    let shown: Vec<Row> = rows
        .iter()
        .map(|r| {
            let place = r.placement.map_or("–".to_owned(), |n| {
                format!("#{n}") + &r.players.map(|t| format!(" / {t}")).unwrap_or_default()
            });
            row(
                &r.id,
                vec![
                    p.when(r.when),
                    format!("{}{}", if r.favorite { "★ " } else { "" }, r.mode),
                    place,
                    p.int(type_chests(r, kind.as_deref())),
                    if count_broken {
                        p.int(r.broken)
                    } else {
                        "–".into()
                    },
                    p.int(r.team),
                    p.int(r.ammo),
                    elapsed(r.seconds),
                ],
            )
        })
        .collect();
    let total = snap.match_rows.len();
    g.set_match_count(s(if total > 0 {
        format!("{} of {total} matches", shown.len())
    } else {
        String::new()
    }));
    g.set_match_empty(total == 0);
    g.set_matches(model(shown));
    g.set_match_sort(column);
    g.set_match_descending(descending);
    let selected = rows.iter().find(|r| r.id == st.selected_match);
    g.set_selected_match(s(selected.map_or("", |r| r.id.as_str())));
    if let Some(r) = selected {
        detail(ui, r, p, t);
    }
}

fn detail(ui: &AppWindow, r: &MatchRow, p: &Prefs, t: &Theme) {
    let g = ui.global::<Pages>();
    g.set_detail_title(s(format!("{} · {}", r.mode, p.when(r.when))));
    let place = r.placement.map_or("No placement recorded".to_owned(), |n| {
        format!("#{n}") + &r.players.map(|t| format!(" of {t}")).unwrap_or_default()
    });
    let team = if r.teammates.is_empty() {
        String::new()
    } else {
        format!(
            " · with {}",
            r.teammates
                .iter()
                .take(3)
                .cloned()
                .collect::<Vec<_>>()
                .join(", ")
        )
    };
    g.set_detail_sub(s(format!(
        "{place} · {} long · {} chests{team}",
        elapsed(r.seconds),
        p.int(r.count)
    )));
    g.set_detail_favorite(r.favorite);
    // The recorder's match stats, e.g. "Damage to players 867 · Accuracy 41% · ...".
    let stats = r.stats.as_ref().map_or(String::new(), |st| {
        crate::cards::MATCH_STATS
            .iter()
            .filter_map(|(label, key, unit)| {
                st[*key]
                    .as_f64()
                    .map(|v| format!("{label} {}", crate::cards::stat_text(p, *unit, v)))
            })
            .collect::<Vec<_>>()
            .join(" · ")
    });
    g.set_detail_stats(s(stats));
    g.set_detail_tags(s(r.tags.clone()));
    g.set_detail_note(s(r.note.clone()));
    let types = r.types.most_common();
    let peak = types.iter().map(|x| x.1).max().unwrap_or(0).max(1) as f32;
    let bars: Vec<BarRow> = if types.is_empty() {
        vec![BarRow {
            label: s("No chests opened"),
            ..Default::default()
        }]
    } else {
        types
            .iter()
            .map(|(n, c)| BarRow {
                label: s(n.clone()),
                fraction: *c as f32 / peak,
                text: s(p.int(*c)),
                color: color(t.type_color(n)),
                has_color: true,
            })
            .collect()
    };
    g.set_detail_types(model(bars));
    g.set_detail_end(s(elapsed(r.seconds)));
    g.set_detail_squad_note(s(if r.squad.is_none() {
        "This older match has no per-player detail. It is read again on the next scan, while its replay file is still on disk."
    } else {
        r.shared_note.as_deref().unwrap_or("")
    }));
    let total = r.squad_total.max(1) as f32;
    let squad: Vec<Row> = r
        .squad
        .iter()
        .flatten()
        .map(|m| {
            let percent = (100.0 * m.total as f32 / total).round();
            let types = {
                let mut t = m.types.items.clone();
                t.sort_by_key(|x| std::cmp::Reverse(x.1));
                t.iter()
                    .map(|(n, c)| format!("{n} {}", p.int(*c)))
                    .collect::<Vec<_>>()
                    .join(", ")
            };
            let mvp = if r.mvp.as_deref() == Some(m.key.as_str()) {
                " · MVP"
            } else {
                ""
            };
            let mut x = row(
                &m.key,
                vec![
                    format!("{}{}{mvp}", m.name, stats::tag(m)),
                    p.int(m.total),
                    p.int(m.opened),
                    p.int(m.broken),
                    format!("{percent}%"),
                    types,
                ],
            );
            x.share = percent / 100.0;
            x
        })
        .collect();
    g.set_detail_squad(model(squad));
}

pub fn set_detail_marks(
    ui: &AppWindow,
    marks: &[(f64, String, String)],
    seconds: f64,
    t: &Theme,
    target: &str,
) {
    let length = seconds.max(1.0);
    let marks: Vec<Mark> = marks
        .iter()
        .map(|(time, kind, source)| Mark {
            x: (time / length).clamp(0.0, 1.0) as f32,
            color: color(t.type_color(kind)),
            tip: s(format!(
                "{} · {kind}{}",
                elapsed(*time),
                if source == "replay-broken" {
                    " · broken open"
                } else {
                    ""
                }
            )),
        })
        .collect();
    let g = ui.global::<Pages>();
    if target == "detail" {
        g.set_detail_marks(model(marks))
    } else {
        g.set_open_marks(model(marks))
    }
}

pub fn entries(ui: &AppWindow, st: &mut State, snap: &Snapshot, p: &Prefs, t: &Theme) {
    let g = ui.global::<Pages>();
    let kind = (g.get_entry_type() > 0)
        .then(|| st.types.get(g.get_entry_type() as usize - 1).cloned())
        .flatten();
    let match_id = (g.get_entry_match() > 0)
        .then(|| st.match_ids.get(g.get_entry_match() as usize - 1).cloned())
        .flatten();
    let wanted = ["confirmed", "undone", ""][(g.get_entry_status().clamp(0, 2)) as usize];
    let text = g.get_entry_search().trim().to_lowercase();
    let mut visible: Vec<usize> = st
        .entries
        .iter()
        .enumerate()
        .filter(|(_, (e, _))| {
            kind.as_ref().is_none_or(|k| *k == e.chest_type)
                && match_id.as_ref().is_none_or(|m| *m == e.match_id)
                && (wanted.is_empty() || e.status == wanted)
                && (text.is_empty()
                    || format!("{} {}", e.chest_type, e.reason)
                        .to_lowercase()
                        .contains(&text))
        })
        .map(|(i, _)| i)
        .collect();
    let (column, descending) = st.entry_sort;
    visible.sort_by(|&a, &b| {
        let (ea, eb) = (&st.entries[a], &st.entries[b]);
        let order = match column {
            1 => ea.0.chest_type.cmp(&eb.0.chest_type),
            2 => how(&ea.0.source).cmp(&how(&eb.0.source)),
            3 => ea.0.amount.cmp(&eb.0.amount),
            4 => ea.0.status.cmp(&eb.0.status),
            5 => ea.0.reason.cmp(&eb.0.reason),
            _ => ea.1.cmp(&eb.1),
        };
        if descending { order.reverse() } else { order }
    });
    let rows: Vec<Row> = visible
        .iter()
        .map(|&i| {
            let (e, when) = &st.entries[i];
            let mut x = row(
                &e.id,
                vec![
                    p.when(*when),
                    e.chest_type.clone(),
                    how(&e.source),
                    p.int(e.amount),
                    status(&e.status),
                    e.reason
                        .split_once(" · ")
                        .map_or(e.reason.as_str(), |(_, r)| r)
                        .to_owned(),
                ],
            );
            x.ink = color(t.type_color(&e.chest_type)); // the same rarity colour as in the charts
            x.has_ink = true;
            x
        })
        .collect();
    g.set_entry_empty(st.entries.is_empty());
    g.set_entries(model(rows));
    g.set_entry_sort(column);
    g.set_entry_descending(descending);
    if !visible
        .iter()
        .any(|&i| st.entries[i].0.id == st.selected_entry)
    {
        st.selected_entry.clear();
    }
    g.set_selected_entry(s(st.selected_entry.clone()));
    st.visible_entries = visible;
    match match_id.and_then(|id| snap.match_rows.iter().find(|r| r.id == id)) {
        Some(r) => {
            g.set_open_title(s(format!(
                "{} · {} · {} chests",
                r.mode,
                p.when(r.when),
                p.int(r.count)
            )));
            g.set_open_end(s(elapsed(r.seconds)));
        }
        None => {
            g.set_open_title(s("Choose a match above to see when its openings happened"));
            g.set_open_end(s(""));
            g.set_open_marks(model(vec![]));
        }
    }
}

pub fn selected_entry_match(ui: &AppWindow, st: &State) -> Option<String> {
    let g = ui.global::<Pages>();
    (g.get_entry_match() > 0)
        .then(|| st.match_ids.get(g.get_entry_match() as usize - 1).cloned())
        .flatten()
}

pub fn team(ui: &AppWindow, st: &mut State, snap: &Snapshot, p: &Prefs, t: &Theme) {
    let g = ui.global::<Pages>();
    let mode = (g.get_team_mode() > 0)
        .then(|| st.modes.get(g.get_team_mode() as usize - 1).cloned())
        .flatten();
    let since = since(g.get_team_range(), snap.today_date);
    let minimum = g.get_team_minimum() as i64;
    let rows: Vec<MatchRow> = snap
        .match_rows
        .iter()
        .filter(|r| {
            r.squad.is_some()
                && mode.as_ref().is_none_or(|m| *m == r.mode)
                && since.is_none_or(|d| r.when.date_naive() >= d)
        })
        .cloned()
        .collect();
    let mut ranked: Vec<Person> = stats::board(&rows)
        .into_iter()
        .filter(|x| x.matches >= minimum)
        .collect();
    let best = stats::records(&ranked);
    let mut records = Vec::new();
    for (key, person) in [
        ("most", best.most_chests),
        ("best", best.best_match),
        ("played", best.most_played),
    ] {
        match person {
            None => records.extend(["–".to_owned(), "No teammates in this view".to_owned()]),
            Some(x) if key == "most" => records.extend([
                format!("{} · {} chests", x.name, p.int(x.total)),
                format!("in {}", plural(x.matches, "match")),
            ]),
            Some(x) if key == "best" => records.extend([
                format!("{} · {} in one match", x.name, p.int(x.best)),
                x.best_when.map(|w| p.when(w)).unwrap_or_default(),
            ]),
            Some(x) => records.extend([
                x.name.clone(),
                format!("{} together", plural(x.matches, "match")),
            ]),
        }
    }
    g.set_records(model(records.into_iter().map(SharedString::from).collect()));
    g.set_team_count(s(format!(
        "{} across {} in this view",
        people(ranked.len()),
        plural(rows.len() as i64, "match")
    )));
    let mut notes = Vec::new();
    if snap.older_matches > 0 {
        notes.push(format!(
            "{} older matches have no per-player detail yet. They are read again on the next scan while their replay files are still on disk.",
            snap.older_matches
        ));
    }
    notes.push(format!("{} chests were already open when your game first showed them, so nobody is credited for them.", p.int(snap.unattributed)));
    g.set_team_note(s(notes.join(" ")));
    let (column, descending) = st.team_sort;
    ranked.sort_by(|a, b| {
        let order = match column {
            0 => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
            1 => a.matches.cmp(&b.matches),
            3 => a.per_match.total_cmp(&b.per_match),
            4 => a.opened.cmp(&b.opened),
            5 => a.broken.cmp(&b.broken),
            6 => a.rare.cmp(&b.rare),
            7 => a.best.cmp(&b.best),
            _ => a.total.cmp(&b.total),
        };
        if descending { order.reverse() } else { order }
    });
    let tag = |x: &Person| {
        if x.me {
            " · you"
        } else if x.bot {
            " · bot"
        } else if x.anonymous {
            " · unnamed"
        } else {
            ""
        }
    };
    g.set_people(model(
        ranked
            .iter()
            .map(|x| {
                row(
                    &x.key,
                    vec![
                        format!("{}{}", x.name, tag(x)),
                        p.int(x.matches),
                        p.int(x.total),
                        p.number(x.per_match, 1),
                        p.int(x.opened),
                        p.int(x.broken),
                        p.int(x.rare),
                        p.int(x.best),
                    ],
                )
            })
            .collect(),
    ));
    g.set_team_sort(column);
    g.set_team_descending(descending);
    let person = ranked.iter().find(|x| x.key == st.selected_person);
    if person.is_none() {
        st.selected_person.clear();
    }
    g.set_selected_person(s(st.selected_person.clone()));
    if let Some(x) = person {
        let history = stats::profile(&rows, &x.key);
        let span = match (history.first(), history.last()) {
            (Some(a), Some(b)) => format!(
                " · first {} · last {}",
                p.short_date(a.when.date_naive()),
                p.short_date(b.when.date_naive())
            ),
            _ => String::new(),
        };
        g.set_profile_title(s(format!("{}{}", x.name, tag(x))));
        g.set_profile_sub(s(format!("{} together{span}", plural(x.matches, "match"))));
        g.set_profile_stats(s(format!(
            "{} chests · {} per match · {} searched · {} broken open · {} rare or better · best {}",
            p.int(x.total),
            p.number(x.per_match, 1),
            p.int(x.opened),
            p.int(x.broken),
            p.int(x.rare),
            p.int(x.best)
        )));
        let peak = history.iter().map(|h| h.total).max().unwrap_or(0).max(1) as f32;
        g.set_profile_points(model(
            history
                .iter()
                .map(|h| ChartPoint {
                    label: s(p.short_date(h.when.date_naive())),
                    value: h.total as f32 / peak,
                    tip: s(format!(
                        "{} · {} · {} chests (you {})",
                        p.when(h.when),
                        h.mode,
                        p.int(h.total),
                        p.int(h.mine)
                    )),
                })
                .collect(),
        ));
        let n = history.len();
        let shown = n.min(5);
        g.set_profile_axis(model(
            (0..shown)
                .map(|k| {
                    s(p.short_date(
                        history[if shown <= 1 {
                            0
                        } else {
                            k * (n - 1) / (shown - 1)
                        }]
                        .when
                        .date_naive(),
                    ))
                })
                .collect(),
        ));
        let values: Vec<f64> = history.iter().map(|h| h.total as f64).collect();
        let (line, area) = series_path(&values, p.chart_style == "smooth");
        g.set_profile_line(s(line));
        g.set_profile_area(s(area));
        let mut types = stats::Counter::default();
        for h in &history {
            for (n, c) in &h.types.items {
                types.add(n, *c);
            }
        }
        let common = types.most_common();
        let tpeak = common.iter().map(|c| c.1).max().unwrap_or(0).max(1) as f32;
        g.set_profile_types(model(if common.is_empty() {
            vec![BarRow {
                label: s("No chests"),
                ..Default::default()
            }]
        } else {
            common
                .iter()
                .map(|(n, c)| BarRow {
                    label: s(n.clone()),
                    fraction: *c as f32 / tpeak,
                    text: s(p.int(*c)),
                    color: color(t.type_color(n)),
                    has_color: true,
                })
                .collect()
        }));
        g.set_profile_matches(model(
            history
                .iter()
                .rev()
                .map(|h| {
                    row(
                        &h.id,
                        vec![
                            p.when(h.when),
                            h.mode.clone(),
                            p.int(h.total),
                            p.int(h.mine),
                        ],
                    )
                })
                .collect(),
        ));
    }
    st.ranked = ranked;
}

/// Rows for exporting the openings currently shown.
pub fn visible_entry_rows(st: &State) -> Vec<Map<String, Value>> {
    st.visible_entries
        .iter()
        .filter_map(|&i| serde_json::to_value(&st.entries[i].0).ok())
        .filter_map(|v| v.as_object().cloned())
        .collect()
}

pub fn people_rows(st: &State) -> Vec<Map<String, Value>> {
    st.ranked
        .iter()
        .filter_map(|x| {
            json!({"name": x.name, "you": x.me, "bot": x.bot, "matches": x.matches, "chests": x.total,
                "per_match": (x.per_match * 100.0).round() / 100.0, "searched": x.opened, "broken_open": x.broken,
                "rare_or_better": x.rare, "best_match": x.best,
                "last_together": x.last.map(|w| w.date_naive().to_string()).unwrap_or_default()})
            .as_object()
            .cloned()
        })
        .collect()
}

pub fn people_fields() -> &'static [&'static str] {
    &PEOPLE_FIELDS
}

/// All openings, newest first, with their local times.
pub fn load_entries(store: &lootledger_core::store::Store) -> Vec<(Event, DateTime<Local>)> {
    store
        .entries(0, u32::MAX)
        .unwrap_or_default()
        .into_iter()
        .filter_map(|e| local_time(&e.created).ok().map(|w| (e, w)))
        .collect()
}
