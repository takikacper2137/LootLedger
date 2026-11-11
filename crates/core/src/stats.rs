//! Dashboard statistics, a port of lootledger/stats.py and features.py: plain data over openings and
//! matches, no SQL and no UI. Times are local; a day is a calendar date and weeks start on Monday.

use chrono::{DateTime, Datelike, Duration, Local, NaiveDate, Timelike};
use serde_json::Value;
use std::collections::{BTreeMap, HashMap, HashSet};

pub const SESSION_GAP_MINUTES: i64 = 30;
const RECENT: usize = 20;
pub const RARE: [&str; 3] = ["Rare chest", "Relic chest", "Buried chest"];

/// One counted opening.
#[derive(Debug, Clone)]
pub struct Ev {
    pub when: DateTime<Local>,
    pub amount: i64,
    pub chest_type: String,
    pub source: String,
    pub match_id: Option<String>,
}

/// One match as the history query returns it.
#[derive(Debug, Clone)]
pub struct MatchIn {
    pub id: String,
    pub started: DateTime<Local>,
    pub seconds: f64,
    pub total: i64,
    pub broken: i64,
    pub meta: Option<Value>,
    pub note: String,
    pub tags: String,
    pub favorite: bool,
}

/// Counts that keep the first-seen order of keys and sort like Python's Counter.most_common.
#[derive(Debug, Clone, Default)]
pub struct Counter {
    pub items: Vec<(String, i64)>,
}

impl Counter {
    pub fn add(&mut self, key: &str, n: i64) {
        match self.items.iter_mut().find(|(k, _)| k == key) {
            Some((_, v)) => *v += n,
            None => self.items.push((key.to_owned(), n)),
        }
    }
    pub fn get(&self, key: &str) -> i64 {
        self.items
            .iter()
            .find(|(k, _)| k == key)
            .map_or(0, |(_, v)| *v)
    }
    /// Highest first; ties keep insertion order (Python's sort is stable).
    pub fn most_common(&self) -> Vec<(String, i64)> {
        let mut items = self.items.clone();
        items.sort_by_key(|x| std::cmp::Reverse(x.1));
        items
    }
    pub fn total(&self) -> i64 {
        self.items.iter().map(|(_, v)| v).sum()
    }
    pub fn keys(&self) -> impl Iterator<Item = &str> {
        self.items.iter().map(|(k, _)| k.as_str())
    }
}

#[derive(Debug, Clone)]
pub struct SquadMember {
    pub key: String,
    pub name: String,
    pub me: bool,
    pub bot: bool,
    pub anonymous: bool,
    pub unranked: bool,
    pub opened: i64,
    pub broken: i64,
    pub total: i64,
    pub types: Counter,
    pub containers: Counter,
}

#[derive(Debug, Clone)]
pub struct MatchRow {
    pub id: String,
    pub when: DateTime<Local>,
    pub seconds: f64,
    pub count: i64,
    pub broken: i64,
    pub mode: String,
    pub types: Counter,
    pub team: i64,
    pub ammo: i64,
    pub placement: Option<i64>,
    pub players: Option<i64>,
    pub teammates: Vec<String>,
    pub squad: Option<Vec<SquadMember>>,
    pub mvp: Option<String>,
    pub squad_total: i64,
    pub unattributed: i64,
    /// The recorder's match stats (damage, accuracy, materials...), when the replay had them.
    pub stats: Option<Value>,
    /// Set when a partner's recording of this match raised someone's count: what changed, in words.
    pub shared_note: Option<String>,
    pub note: String,
    pub tags: String,
    pub favorite: bool,
}

/// One player's chests over the current party.
#[derive(Debug, Clone, PartialEq)]
pub struct PartyPerson {
    pub name: String,
    pub me: bool,
    pub total: i64,
}

/// The matches you are playing with the same friends right now: everyone's chests, and each person's.
#[derive(Debug, Clone, PartialEq)]
pub struct Party {
    pub matches: usize,
    /// Everyone's chests in the newest match.
    pub last: i64,
    /// Everyone's chests over all the party's matches.
    pub total: i64,
    /// You first, then the others by chests; at most four.
    pub people: Vec<PartyPerson>,
}

/// A party ends when nobody has played a match with it for this long.
const PARTY_GAP_HOURS: i64 = 3;

/// The party you are in: the newest match with teammates, and the matches before it that shared a teammate
/// with it, back to the first one that did not or that lay more than three hours earlier. `rows` is newest
/// first. None when you are playing alone, or the newest match with teammates ended long ago.
pub fn party(rows: &[MatchRow], now: DateTime<Local>) -> Option<Party> {
    let squad_of = |r: &MatchRow| -> Vec<(String, String, bool, i64)> {
        r.squad
            .iter()
            .flatten()
            .filter(|p| !p.bot)
            .map(|p| (p.key.clone(), p.name.clone(), p.me, p.total))
            .collect()
    };
    let has_mates = |r: &MatchRow| squad_of(r).iter().any(|p| !p.2);
    let newest = rows.iter().position(has_mates)?;
    let end_of = |r: &MatchRow| r.when + Duration::milliseconds((r.seconds * 1000.0) as i64);
    if now - end_of(&rows[newest]) > Duration::hours(PARTY_GAP_HOURS) {
        return None;
    }
    let friends: HashSet<String> = squad_of(&rows[newest])
        .into_iter()
        .filter(|p| !p.2)
        .map(|p| p.0)
        .collect();
    let mut people: Vec<(String, PartyPerson)> = Vec::new();
    let (mut matches, mut total, mut earliest) = (0, 0, end_of(&rows[newest]));
    for row in &rows[newest..] {
        let squad = squad_of(row);
        let shares = squad.iter().any(|p| !p.2 && friends.contains(&p.0));
        let close = earliest - end_of(row) <= Duration::hours(PARTY_GAP_HOURS);
        if !shares || !close {
            break;
        }
        earliest = row.when;
        matches += 1;
        for (key, name, me, chests) in squad {
            total += chests;
            match people.iter_mut().find(|(k, _)| *k == key) {
                Some((_, person)) => person.total += chests,
                None => people.push((
                    key,
                    PartyPerson {
                        name,
                        me,
                        total: chests,
                    },
                )),
            }
        }
    }
    let mut people: Vec<PartyPerson> = people.into_iter().map(|(_, p)| p).collect();
    people.sort_by_key(|p| (!p.me, std::cmp::Reverse(p.total)));
    people.truncate(4);
    Some(Party {
        matches,
        last: squad_of(&rows[newest]).iter().map(|p| p.3).sum(),
        total,
        people,
    })
}

#[derive(Debug, Clone)]
pub struct Person {
    pub key: String,
    pub name: String,
    pub me: bool,
    pub bot: bool,
    pub anonymous: bool,
    pub unranked: bool,
    pub matches: i64,
    pub total: i64,
    pub opened: i64,
    pub broken: i64,
    pub types: Counter,
    pub best: i64,
    pub best_when: Option<DateTime<Local>>,
    pub last: Option<DateTime<Local>>,
    pub per_match: f64,
    pub rare: i64,
}

#[derive(Debug, Clone, Copy)]
pub struct Mode {
    pub matches: i64,
    pub chests: i64,
}

#[derive(Debug, Clone)]
pub struct Session {
    pub start: DateTime<Local>,
    pub end: DateTime<Local>,
    pub matches: i64,
    pub chests: i64,
    pub seconds: f64,
    pub per_hour: f64,
}

#[derive(Debug, Clone)]
pub struct Snapshot {
    pub today_date: NaiveDate,
    pub per_day: BTreeMap<NaiveDate, i64>,
    pub all: i64,
    pub broken: i64,
    pub types: Counter,
    pub containers: Counter,
    pub modes: Vec<(String, Mode)>,
    pub today: i64,
    pub today_prev: i64,
    pub week: i64,
    pub week_prev: i64,
    pub month: i64,
    pub month_prev: i64,
    pub session: i64,
    pub matches: i64,
    pub match_chests: i64,
    pub avg: Option<f64>,
    pub median: Option<f64>,
    pub best: Option<i64>,
    pub worst: Option<i64>,
    pub recent_totals: Vec<i64>,
    pub playtime: f64,
    pub per_hour: f64,
    pub last: Option<MatchRow>,
    pub best_day: Option<(NaiveDate, i64)>,
    pub streak_longest: i64,
    pub streak_current: i64,
    pub session_best: Option<Session>,
    pub peak_hour: (i64, Option<DateTime<Local>>),
    pub heat: [[i64; 24]; 7],
    pub heat_matches: [[i64; 24]; 7],
    pub match_days: BTreeMap<NaiveDate, i64>,
    pub team_total: i64,
    /// Teammates' chests per day (from the matches whose replays were read), so a tile can show
    /// "you + team" for any period. Your own `per_day` also holds manual entries; this does not.
    pub team_per_day: BTreeMap<NaiveDate, i64>,
    pub team_today: i64,
    pub team_week: i64,
    pub team_month: i64,
    pub team_session: i64,
    pub team_types: Counter,
    pub team_opened: i64,
    pub team_broken: i64,
    pub team_containers: Counter,
    pub unattributed: i64,
    pub board: Vec<Person>,
    pub older_matches: i64,
    pub team_per_match: Option<f64>,
    pub your_share: Option<f64>,
    pub player: Option<String>,
    pub partial: i64,
    pub match_rows: Vec<MatchRow>,
    /// The party you are in right now, when there is one.
    pub party: Option<Party>,
}

fn span(per_day: &BTreeMap<NaiveDate, i64>, start: NaiveDate, end: NaiveDate) -> i64 {
    if start > end {
        return 0;
    }
    per_day.range(start..=end).map(|(_, v)| v).sum()
}

fn peak_hour(events: &[Ev]) -> (i64, Option<DateTime<Local>>) {
    let mut times: Vec<&Ev> = events.iter().collect();
    times.sort_by_key(|e| e.when);
    let (mut best, mut low, mut window) = ((0, None), 0usize, 0i64);
    for event in &times {
        window += event.amount;
        while event.when - times[low].when >= Duration::hours(1) {
            window -= times[low].amount;
            low += 1;
        }
        if window > best.0 {
            best = (window, Some(event.when));
        }
    }
    best
}

fn streaks(per_day: &BTreeMap<NaiveDate, i64>, today: NaiveDate) -> (i64, i64) {
    let (mut longest, mut run, mut previous) = (0, 0, None::<NaiveDate>);
    for (&day, _) in per_day.iter().filter(|(_, v)| **v > 0) {
        run = if previous.is_some_and(|p| day - p == Duration::days(1)) {
            run + 1
        } else {
            1
        };
        longest = longest.max(run);
        previous = Some(day);
    }
    let positive = |d: &NaiveDate| per_day.get(d).copied().unwrap_or(0) > 0;
    let mut day = if positive(&today) {
        today
    } else {
        today - Duration::days(1)
    };
    let mut current = 0;
    while positive(&day) {
        current += 1;
        day -= Duration::days(1);
    }
    (longest, current)
}

fn seconds(s: f64) -> Duration {
    Duration::milliseconds((s * 1000.0) as i64)
}

fn longest_session(analyzed: &[&MatchIn]) -> Option<Session> {
    let gap = Duration::minutes(SESSION_GAP_MINUTES);
    let (mut best, mut current): (Option<Session>, Option<Session>) = (None, None);
    for m in analyzed {
        let end = m.started + seconds(m.seconds);
        match current.as_mut() {
            Some(c) if m.started - c.end <= gap => {
                c.end = c.end.max(end);
                c.chests += m.total;
                c.matches += 1;
            }
            _ => {
                current = Some(Session {
                    start: m.started,
                    end,
                    matches: 1,
                    chests: m.total,
                    seconds: 0.0,
                    per_hour: 0.0,
                })
            }
        }
        let c = current.as_ref().expect("session");
        if best
            .as_ref()
            .is_none_or(|b| c.end - c.start > b.end - b.start)
        {
            best = Some(c.clone());
        }
    }
    best
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Grain {
    Day,
    Week,
    Month,
}

fn grain_key(grain: Grain, day: NaiveDate) -> NaiveDate {
    match grain {
        Grain::Day => day,
        Grain::Week => day - Duration::days(day.weekday().num_days_from_monday() as i64),
        Grain::Month => day.with_day(1).expect("first of month"),
    }
}

/// Counts per day, week or month from start to end inclusive, including empty periods.
pub fn series(
    per_day: &BTreeMap<NaiveDate, i64>,
    grain: Grain,
    start: NaiveDate,
    end: NaiveDate,
) -> Vec<(NaiveDate, i64)> {
    let mut buckets = BTreeMap::new();
    let (mut cursor, last) = (grain_key(grain, start), grain_key(grain, end));
    while cursor <= last {
        buckets.insert(cursor, 0);
        cursor = match grain {
            Grain::Day => cursor + Duration::days(1),
            Grain::Week => cursor + Duration::days(7),
            Grain::Month => (cursor.with_day(28).expect("28th") + Duration::days(4))
                .with_day(1)
                .expect("first"),
        };
    }
    for (&day, &value) in per_day {
        if start <= day
            && day <= end
            && let Some(b) = buckets.get_mut(&grain_key(grain, day))
        {
            *b += value;
        }
    }
    buckets.into_iter().collect()
}

/// Chart points for the trend card; `matches` counts matches instead of chests. days=0 means everything.
pub fn trend(snap: &Snapshot, grain: Grain, days: i64, matches: bool) -> Vec<(NaiveDate, i64)> {
    let today = snap.today_date;
    let start = if days > 0 {
        today - Duration::days(days - 1)
    } else {
        snap.per_day.keys().next().copied().unwrap_or(today)
    };
    series(
        if matches {
            &snap.match_days
        } else {
            &snap.per_day
        },
        grain,
        start,
        today,
    )
}

pub fn last_days(snap: &Snapshot, count: i64) -> Vec<i64> {
    (0..count)
        .rev()
        .map(|offset| {
            snap.per_day
                .get(&(snap.today_date - Duration::days(offset)))
                .copied()
                .unwrap_or(0)
        })
        .collect()
}

fn squad(meta: &Value, include_broken: bool) -> Vec<SquadMember> {
    let mut people: Vec<SquadMember> = Vec::new();
    for p in meta["players"].as_array().into_iter().flatten() {
        let mut types = Counter::default();
        let (mut opened, mut broken) = (0, 0);
        for (name, c) in p["counts"].as_object().into_iter().flatten() {
            let o = c["opened"].as_i64().unwrap_or(0);
            let b = if include_broken {
                c["broken"].as_i64().unwrap_or(0)
            } else {
                0
            };
            opened += o;
            broken += b;
            if o + b != 0 {
                types.add(name, o + b);
            }
        }
        let mut containers = Counter::default();
        for (name, n) in p["containers"].as_object().into_iter().flatten() {
            containers.add(name, n.as_i64().unwrap_or(0));
        }
        let key = p["key"].as_str().unwrap_or("").to_owned();
        let bot = p["bot"].as_bool().unwrap_or(false);
        let anonymous = key.starts_with("anon-");
        people.push(SquadMember {
            name: p["name"].as_str().unwrap_or("").to_owned(),
            me: p["me"].as_bool().unwrap_or(false),
            unranked: bot || anonymous,
            key,
            bot,
            anonymous,
            opened,
            broken,
            total: opened + broken,
            types,
            containers,
        });
    }
    people.sort_by(|a, b| {
        (-a.total, -a.opened, !a.me, &a.name).cmp(&(-b.total, -b.opened, !b.me, &b.name))
    });
    people
}

/// Short label after a name: you, bot, or unnamed.
pub fn tag(p: &SquadMember) -> &'static str {
    if p.me {
        " · you"
    } else if p.bot {
        " · bot"
    } else if p.anonymous {
        " · unnamed"
    } else {
        ""
    }
}

fn mvp(squad: &[SquadMember]) -> Option<String> {
    let mut ranked: Vec<&SquadMember> = squad
        .iter()
        .filter(|p| p.total > 0 && !p.unranked)
        .collect();
    ranked.sort_by_key(|p| (-p.total, -p.opened));
    match ranked.as_slice() {
        [] => None,
        [a, b, ..] if (a.total, a.opened) == (b.total, b.opened) => None,
        [a, ..] => Some(a.key.clone()),
    }
}

/// Everyone across these match rows, merged by stable id, most chests first. Bots are listed, not ranked.
pub fn board(rows: &[MatchRow]) -> Vec<Person> {
    let mut order: Vec<&MatchRow> = rows.iter().collect();
    order.sort_by_key(|r| r.when);
    let mut people: Vec<Person> = Vec::new();
    let mut index: HashMap<String, usize> = HashMap::new();
    for row in order {
        for p in row.squad.iter().flatten() {
            let at = *index.entry(p.key.clone()).or_insert_with(|| {
                people.push(Person {
                    key: p.key.clone(),
                    name: p.name.clone(),
                    me: p.me,
                    bot: p.bot,
                    anonymous: p.anonymous,
                    unranked: p.unranked,
                    matches: 0,
                    total: 0,
                    opened: 0,
                    broken: 0,
                    types: Counter::default(),
                    best: 0,
                    best_when: None,
                    last: None,
                    per_match: 0.0,
                    rare: 0,
                });
                people.len() - 1
            });
            let person = &mut people[at];
            person.name = p.name.clone();
            person.matches += 1;
            person.total += p.total;
            person.opened += p.opened;
            person.broken += p.broken;
            for (name, n) in &p.types.items {
                person.types.add(name, *n);
            }
            if p.total > person.best {
                person.best = p.total;
                person.best_when = Some(row.when);
            }
            person.last = Some(row.when);
        }
    }
    people.sort_by(|a, b| {
        (a.unranked, -a.total, -a.matches, &a.name)
            .cmp(&(b.unranked, -b.total, -b.matches, &b.name))
    });
    for person in &mut people {
        person.per_match = person.total as f64 / person.matches as f64;
        person.rare = RARE.iter().map(|n| person.types.get(n)).sum();
    }
    people
}

pub struct Records<'a> {
    pub most_chests: Option<&'a Person>,
    pub best_match: Option<&'a Person>,
    pub most_played: Option<&'a Person>,
}

/// max() that keeps the first of equal values, like Python's.
fn first_max<'a>(items: &[&'a Person], key: impl Fn(&Person) -> i64) -> Option<&'a Person> {
    let mut best: Option<&Person> = None;
    for p in items {
        if best.is_none_or(|b| key(p) > key(b)) {
            best = Some(p);
        }
    }
    best
}

/// Records among teammates (you, bots and unnamed players excluded).
pub fn records(ranked: &[Person]) -> Records<'_> {
    let mates: Vec<&Person> = ranked.iter().filter(|p| !p.me && !p.unranked).collect();
    Records {
        most_chests: first_max(&mates, |p| p.total),
        best_match: first_max(&mates, |p| p.best),
        most_played: first_max(&mates, |p| p.matches),
    }
}

pub struct ProfileRow {
    pub id: String,
    pub when: DateTime<Local>,
    pub mode: String,
    pub total: i64,
    pub opened: i64,
    pub broken: i64,
    pub types: Counter,
    pub mine: i64,
}

/// One player's matches with you, oldest first: their chests and yours in each.
pub fn profile(rows: &[MatchRow], key: &str) -> Vec<ProfileRow> {
    let mut order: Vec<&MatchRow> = rows.iter().collect();
    order.sort_by_key(|r| r.when);
    let mut out = Vec::new();
    for row in order {
        let Some(squad) = &row.squad else { continue };
        for p in squad.iter().filter(|p| p.key == key) {
            out.push(ProfileRow {
                id: row.id.clone(),
                when: row.when,
                mode: row.mode.clone(),
                total: p.total,
                opened: p.opened,
                broken: p.broken,
                types: p.types.clone(),
                mine: squad.iter().find(|q| q.me).map_or(0, |q| q.total),
            });
        }
    }
    out
}

fn median(values: &[i64]) -> Option<f64> {
    let mut v = values.to_vec();
    v.sort();
    let n = v.len();
    match n {
        0 => None,
        _ if n % 2 == 1 => Some(v[n / 2] as f64),
        _ => Some((v[n / 2 - 1] + v[n / 2]) as f64 / 2.0),
    }
}

fn first_of_month(d: NaiveDate) -> NaiveDate {
    d.with_day(1).expect("first of month")
}

pub fn compute(
    events: &[Ev],
    matches: &[MatchIn],
    now: DateTime<Local>,
    session_start: Option<DateTime<Local>>,
    include_broken: bool,
) -> Snapshot {
    let today = now.date_naive();
    let mut per_day: BTreeMap<NaiveDate, i64> = BTreeMap::new();
    let mut types = Counter::default();
    let mut match_types: HashMap<&str, Counter> = HashMap::new();
    let mut heat = [[0i64; 24]; 7];
    let mut broken = 0;
    for e in events {
        *per_day.entry(e.when.date_naive()).or_default() += e.amount;
        types.add(&e.chest_type, e.amount);
        heat[e.when.weekday().num_days_from_monday() as usize][e.when.hour() as usize] += e.amount;
        if e.source == "replay-broken" {
            broken += e.amount;
        }
        if let Some(id) = e.match_id.as_deref().filter(|s| !s.is_empty()) {
            match_types
                .entry(id)
                .or_default()
                .add(&e.chest_type, e.amount);
        }
    }

    let mut analyzed: Vec<&MatchIn> = matches.iter().filter(|m| m.meta.is_some()).collect();
    analyzed.sort_by_key(|m| m.started);
    let mut heat_matches = [[0i64; 24]; 7];
    let mut match_days: BTreeMap<NaiveDate, i64> = BTreeMap::new();
    for m in &analyzed {
        heat_matches[m.started.weekday().num_days_from_monday() as usize]
            [m.started.hour() as usize] += 1;
        *match_days.entry(m.started.date_naive()).or_default() += 1;
    }
    let analyzed_ids: HashSet<&str> = analyzed.iter().map(|m| m.id.as_str()).collect();
    let mut modes: Vec<(String, Mode)> = Vec::new();
    let (mut containers, mut team_types, mut team_containers) =
        (Counter::default(), Counter::default(), Counter::default());
    let (mut team_opened, mut team_broken, mut unattributed) = (0, 0, 0);
    let mut team_per_day: BTreeMap<NaiveDate, i64> = BTreeMap::new();
    let mut team_session = 0;
    let mut rows = Vec::new();
    let empty = Value::Null;
    for m in matches {
        let meta = m.meta.as_ref().unwrap_or(&empty);
        let analyzed_row = analyzed_ids.contains(m.id.as_str());
        let mut team = 0;
        for (name, counts) in meta["team"].as_object().into_iter().flatten() {
            let opened_n = counts["opened"].as_i64().unwrap_or(0);
            let broken_n = counts["broken"].as_i64().unwrap_or(0);
            let count = opened_n + if include_broken { broken_n } else { 0 };
            team += count;
            if analyzed_row {
                team_types.add(name, count);
                team_opened += opened_n;
                team_broken += if include_broken { broken_n } else { 0 };
            }
        }
        if analyzed_row && team > 0 {
            *team_per_day.entry(m.started.date_naive()).or_default() += team;
            if session_start.is_some_and(|s| m.started >= s) {
                team_session += team;
            }
        }
        let squad_rows = meta.get("players").map(|_| squad(meta, include_broken));
        let mode_name = meta["mode"]
            .as_str()
            .filter(|s| !s.is_empty())
            .map(str::to_owned);
        if analyzed_row {
            let name = mode_name.clone().unwrap_or_else(|| "Unknown mode".into());
            match modes.iter_mut().find(|(n, _)| *n == name) {
                Some((_, b)) => {
                    b.matches += 1;
                    b.chests += m.total;
                }
                None => modes.push((
                    name,
                    Mode {
                        matches: 1,
                        chests: m.total,
                    },
                )),
            }
            for (name, n) in meta["containers"].as_object().into_iter().flatten() {
                containers.add(name, n.as_i64().unwrap_or(0));
            }
            unattributed += meta["unattributed"].as_i64().unwrap_or(0);
            for p in squad_rows.iter().flatten().filter(|p| !p.me) {
                for (name, n) in &p.containers.items {
                    team_containers.add(name, *n);
                }
            }
        }
        let has_meta = meta.as_object().is_some_and(|o| !o.is_empty());
        rows.push(MatchRow {
            id: m.id.clone(),
            when: m.started,
            seconds: m.seconds,
            count: m.total,
            broken: m.broken,
            mode: mode_name.unwrap_or_else(|| {
                if has_meta {
                    "Unknown mode".into()
                } else {
                    "Manual".into()
                }
            }),
            types: match_types.get(m.id.as_str()).cloned().unwrap_or_default(),
            team,
            ammo: meta["containers"]["Ammo box"].as_i64().unwrap_or(0),
            placement: meta["placement"].as_i64(),
            players: meta["total_players"].as_i64(),
            teammates: meta["teammates"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|n| n.as_str().filter(|s| !s.is_empty()).map(str::to_owned))
                .collect(),
            mvp: squad_rows.as_deref().and_then(mvp),
            squad_total: squad_rows.iter().flatten().map(|p| p.total).sum(),
            squad: squad_rows,
            unattributed: meta["unattributed"].as_i64().unwrap_or(0),
            stats: meta
                .get("stats")
                .filter(|s| s.as_object().is_some_and(|o| !o.is_empty()))
                .cloned(),
            shared_note: meta["shared_note"].as_str().map(str::to_owned),
            note: m.note.clone(),
            tags: m.tags.clone(),
            favorite: m.favorite,
        });
    }
    rows.sort_by_key(|r| std::cmp::Reverse(r.when));

    let totals: Vec<i64> = analyzed.iter().map(|m| m.total).collect();
    let replay_chests: i64 = totals.iter().sum();
    let playtime: f64 = analyzed.iter().map(|m| m.seconds).sum();
    let team_total = team_types.total();
    let mut best_day: Option<(NaiveDate, i64)> = None;
    for (&day, &v) in &per_day {
        if best_day.is_none_or(|(_, b)| v > b) {
            best_day = Some((day, v));
        }
    }
    let (streak_longest, streak_current) = streaks(&per_day, today);
    let month = first_of_month(today);
    let week = today - Duration::days(today.weekday().num_days_from_monday() as i64);
    let last_week = week - Duration::days(7);
    let last_month = first_of_month(month - Duration::days(1));
    let day = Duration::days(1);
    let team_today = team_per_day.get(&today).copied().unwrap_or(0);
    let team_week = span(&team_per_day, week, today);
    let team_month = span(&team_per_day, month, today);
    let n = analyzed.len() as f64;
    let last = rows
        .iter()
        .find(|r| analyzed_ids.contains(r.id.as_str()))
        .cloned();
    let board = board(&rows);
    Snapshot {
        today_date: today,
        all: per_day.values().sum(),
        broken,
        types,
        containers,
        modes,
        today: per_day.get(&today).copied().unwrap_or(0),
        today_prev: per_day.get(&(today - day)).copied().unwrap_or(0),
        week: span(&per_day, week, today),
        week_prev: span(&per_day, last_week, last_week + (today - week)),
        month: span(&per_day, month, today),
        month_prev: span(&per_day, last_month, last_month + (today - month)),
        session: events
            .iter()
            .filter(|e| session_start.is_some_and(|s| e.when >= s))
            .map(|e| e.amount)
            .sum(),
        matches: analyzed.len() as i64,
        match_chests: replay_chests,
        avg: (!analyzed.is_empty()).then(|| replay_chests as f64 / n),
        median: median(&totals),
        best: totals.iter().max().copied(),
        worst: totals.iter().min().copied(),
        recent_totals: totals[totals.len().saturating_sub(RECENT)..].to_vec(),
        playtime,
        per_hour: if playtime > 0.0 {
            replay_chests as f64 * 3600.0 / playtime
        } else {
            0.0
        },
        last,
        best_day: best_day.filter(|(_, v)| *v != 0),
        streak_longest,
        streak_current,
        session_best: longest_session(&analyzed),
        peak_hour: if events.is_empty() {
            (0, None)
        } else {
            peak_hour(events)
        },
        heat,
        heat_matches,
        match_days,
        team_total,
        team_per_day,
        team_today,
        team_week,
        team_month,
        team_session,
        team_types,
        team_opened,
        team_broken,
        team_containers,
        unattributed,
        board,
        older_matches: analyzed
            .iter()
            .filter(|m| m.meta.as_ref().is_some_and(|v| v.get("players").is_none()))
            .count() as i64,
        team_per_match: (!analyzed.is_empty()).then(|| team_total as f64 / n),
        your_share: (replay_chests + team_total != 0)
            .then(|| replay_chests as f64 / (replay_chests + team_total) as f64),
        player: analyzed
            .last()
            .and_then(|m| m.meta.as_ref()?["owner"].as_str().map(str::to_owned)),
        partial: analyzed
            .iter()
            .filter(|m| {
                !m.meta
                    .as_ref()
                    .and_then(|v| v["complete"].as_bool())
                    .unwrap_or(true)
            })
            .count() as i64,
        per_day,
        party: party(&rows, Local::now()),
        match_rows: rows,
    }
}

/// Finished matches grouped into play sessions (gap over 30 minutes starts a new one), newest first.
pub fn sessions(rows: &[MatchRow]) -> Vec<Session> {
    let mut order: Vec<&MatchRow> = rows.iter().filter(|r| r.mode != "Manual").collect();
    order.sort_by_key(|r| r.when);
    let gap = Duration::minutes(SESSION_GAP_MINUTES);
    let mut result: Vec<Session> = Vec::new();
    for row in order {
        let end = row.when + seconds(row.seconds);
        match result.last_mut() {
            Some(c) if row.when - c.end <= gap => {
                c.end = c.end.max(end);
                c.matches += 1;
                c.chests += row.count;
                c.seconds += row.seconds;
            }
            _ => result.push(Session {
                start: row.when,
                end,
                matches: 1,
                chests: row.count,
                seconds: row.seconds,
                per_hour: 0.0,
            }),
        }
    }
    for s in &mut result {
        s.per_hour = if s.seconds > 0.0 {
            s.chests as f64 * 3600.0 / s.seconds
        } else {
            0.0
        };
    }
    result.reverse();
    result
}

pub struct Goal {
    pub name: &'static str,
    pub target: i64,
    pub done: i64,
    pub share: f64,
    pub reached: bool,
}

/// Progress towards the daily and weekly goals (0 = off).
pub fn goals(snap: &Snapshot, daily: i64, weekly: i64) -> Vec<Goal> {
    [
        ("Today", daily, snap.today),
        ("This week", weekly, snap.week),
    ]
    .into_iter()
    .filter(|(_, target, _)| *target > 0)
    .map(|(name, target, done)| Goal {
        name,
        target,
        done,
        share: (done as f64 / target as f64).min(1.0),
        reached: done >= target,
    })
    .collect()
}

pub struct Achievement {
    pub id: String,
    pub title: String,
    pub detail: String,
    pub unlocked: bool,
}

pub const MILESTONES: [i64; 7] = [100, 500, 1000, 2500, 5000, 10000, 25000];
pub const BEST_MATCH: [i64; 3] = [25, 40, 60];

fn grouped(n: i64) -> String {
    let digits = n.to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// Every achievement, unlocked or not.
pub fn achievements(snap: &Snapshot) -> Vec<Achievement> {
    let (total, best) = (snap.all, snap.best.unwrap_or(0));
    let mut items: Vec<(String, String, String, bool)> = MILESTONES
        .iter()
        .map(|&n| {
            (
                format!("chests-{n}"),
                format!("{} chests", grouped(n)),
                format!("Open {} chests in total", grouped(n)),
                total >= n,
            )
        })
        .collect();
    items.extend(BEST_MATCH.iter().map(|&n| {
        (
            format!("match-{n}"),
            format!("{n} in one match"),
            format!("Open {n} chests in a single match"),
            best >= n,
        )
    }));
    let fixed = [
        (
            "first-relic",
            "First relic chest",
            "Open a relic chest",
            snap.types.get("Relic chest") > 0,
        ),
        (
            "first-buried",
            "Treasure hunter",
            "Dig up a buried chest",
            snap.types.get("Buried chest") > 0,
        ),
        (
            "first-sprite",
            "Sprite poster",
            "Open a sprite poster chest",
            snap.types.get("Sprite poster chest") > 0,
        ),
        (
            "streak-7",
            "Week streak",
            "Open chests 7 days in a row",
            snap.streak_longest >= 7,
        ),
        (
            "matches-100",
            "100 matches",
            "Have 100 matches read from replays",
            snap.matches >= 100,
        ),
    ];
    items.extend(
        fixed
            .iter()
            .map(|(i, t, d, u)| (i.to_string(), t.to_string(), d.to_string(), *u)),
    );
    items
        .into_iter()
        .map(|(id, title, detail, unlocked)| Achievement {
            id,
            title,
            detail,
            unlocked,
        })
        .collect()
}

pub struct Period {
    pub chests: i64,
    pub matches: i64,
    pub broken: i64,
    pub per_match: f64,
}

/// Chests, matches and broken-open chests between two dates (inclusive).
pub fn period(snap: &Snapshot, start: NaiveDate, end: NaiveDate) -> Period {
    let rows: Vec<&MatchRow> = snap
        .match_rows
        .iter()
        .filter(|r| (start..=end).contains(&r.when.date_naive()))
        .collect();
    let played: Vec<&&MatchRow> = rows.iter().filter(|r| r.mode != "Manual").collect();
    let matches = played.len() as i64;
    Period {
        chests: span(&snap.per_day, start, end),
        matches,
        broken: rows.iter().map(|r| r.broken).sum(),
        per_match: if matches > 0 {
            played.iter().map(|r| r.count).sum::<i64>() as f64 / matches as f64
        } else {
            0.0
        },
    }
}

/// Two periods side by side with the change: (name, first, second, change).
pub fn compare(
    snap: &Snapshot,
    first: (NaiveDate, NaiveDate),
    second: (NaiveDate, NaiveDate),
) -> Vec<(&'static str, f64, f64, f64)> {
    let (a, b) = (
        period(snap, first.0, first.1),
        period(snap, second.0, second.1),
    );
    [
        ("Chests", a.chests as f64, b.chests as f64),
        ("Matches", a.matches as f64, b.matches as f64),
        ("Chests per match", a.per_match, b.per_match),
        ("Broken open", a.broken as f64, b.broken as f64),
    ]
    .into_iter()
    .map(|(n, x, y)| (n, x, y, y - x))
    .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn at(d: u32, h: u32, m: u32) -> DateTime<Local> {
        Local.with_ymd_and_hms(2026, 11, d, h, m, 0).unwrap()
    }

    fn ev(when: DateTime<Local>, amount: i64, kind: &str, source: &str) -> Ev {
        Ev {
            when,
            amount,
            chest_type: kind.into(),
            source: source.into(),
            match_id: Some("m1".into()),
        }
    }

    #[test]
    fn totals_streaks_and_periods() {
        let events = vec![
            ev(at(2, 10, 0), 3, "Chest", "replay"),
            ev(at(3, 10, 0), 2, "Rare chest", "replay-broken"),
            ev(at(5, 10, 0), 4, "Chest", "replay"),
            ev(at(6, 10, 30), 5, "Chest", "replay"),
            ev(at(6, 11, 0), 1, "Chest", "replay"),
        ];
        let meta = serde_json::json!({"mode": "Solo", "players": [{"key": "epic-a", "name": "Me", "me": true, "bot": false,
            "counts": {"Chest": {"opened": 9, "broken": 0}}}, {"key": "epic-b", "name": "Mate", "me": false, "bot": false,
            "counts": {"Chest": {"opened": 4, "broken": 1}}}], "team": {"Chest": {"opened": 4, "broken": 1}}});
        let m = MatchIn {
            id: "m1".into(),
            started: at(6, 10, 0),
            seconds: 1200.0,
            total: 15,
            broken: 2,
            meta: Some(meta),
            note: String::new(),
            tags: String::new(),
            favorite: false,
        };
        let s = compute(&events, &[m], at(6, 12, 0), None, true);
        assert_eq!((s.all, s.broken, s.today, s.today_prev), (15, 2, 6, 4));
        assert_eq!((s.streak_longest, s.streak_current), (2, 2));
        assert_eq!(s.week, 15); // Monday 2 Nov to Friday 6 Nov holds every opening
        assert_eq!(s.peak_hour.0, 6);
        assert_eq!(s.team_total, 5);
        assert_eq!(s.board[0].name, "Me");
        assert_eq!(s.last.as_ref().unwrap().mvp.as_deref(), Some("epic-a"));
        assert_eq!(
            series(
                &s.per_day,
                Grain::Week,
                at(2, 0, 0).date_naive(),
                at(6, 0, 0).date_naive()
            ),
            vec![(at(2, 0, 0).date_naive(), 15)]
        );
        assert!(goals(&s, 6, 0)[0].reached);
        assert!(achievements(&s).iter().all(|a| !a.unlocked));
    }

    fn mate_match(id: &str, started: DateTime<Local>, opened: i64, broken: i64) -> MatchIn {
        MatchIn {
            id: id.into(),
            started,
            seconds: 900.0,
            total: 10,
            broken: 0,
            meta: Some(serde_json::json!({"mode": "Squad",
                "team": {"Chest": {"opened": opened, "broken": broken}}})),
            note: String::new(),
            tags: String::new(),
            favorite: false,
        }
    }

    /// A match that started `minutes_ago` minutes before `now` and lasted ten minutes.
    fn party_row(
        now: DateTime<Local>,
        minutes_ago: i64,
        members: &[(&str, &str, bool, bool, i64)],
    ) -> MatchRow {
        let squad: Vec<SquadMember> = members
            .iter()
            .map(|(key, name, me, bot, total)| SquadMember {
                key: (*key).into(),
                name: (*name).into(),
                me: *me,
                bot: *bot,
                anonymous: false,
                unranked: false,
                opened: *total,
                broken: 0,
                total: *total,
                types: Counter::default(),
                containers: Counter::default(),
            })
            .collect();
        MatchRow {
            id: format!("m{minutes_ago}"),
            when: now - Duration::minutes(minutes_ago),
            seconds: 600.0,
            count: 0,
            broken: 0,
            mode: "Duo".into(),
            types: Counter::default(),
            team: 0,
            ammo: 0,
            placement: None,
            players: None,
            teammates: Vec::new(),
            squad_total: squad.iter().map(|p| p.total).sum(),
            squad: Some(squad),
            mvp: None,
            unattributed: 0,
            stats: None,
            shared_note: None,
            note: String::new(),
            tags: String::new(),
            favorite: false,
        }
    }

    #[test]
    fn a_party_is_the_run_of_matches_with_the_same_friends() {
        let now = Local::now();
        let (me, ola, wolf) = (("K", "Kacper"), ("O", "Ola"), ("W", "Wolf"));
        let with =
            |m: (&'static str, &'static str), me_flag: bool, n: i64| (m.0, m.1, me_flag, false, n);
        let rows = vec![
            // newest first: a trio, a duo with Ola, a solo match, an older duo with Ola
            party_row(
                now,
                20,
                &[
                    with(me, true, 20),
                    with(ola, false, 10),
                    with(wolf, false, 5),
                ],
            ),
            party_row(now, 90, &[with(me, true, 15), with(ola, false, 8)]),
            party_row(now, 150, &[with(me, true, 7)]),
            party_row(now, 200, &[with(me, true, 99), with(ola, false, 99)]),
        ];
        let party = party(&rows, now).unwrap();
        // The solo match ends the party, so the older duo is not part of it.
        assert_eq!((party.matches, party.last, party.total), (2, 35, 58));
        let people: Vec<(&str, i64)> = party
            .people
            .iter()
            .map(|p| (p.name.as_str(), p.total))
            .collect();
        assert_eq!(people, [("Kacper", 35), ("Ola", 18), ("Wolf", 5)]);
        assert!(party.people[0].me && !party.people[1].me);
    }

    #[test]
    fn no_party_when_alone_or_when_the_last_one_ended_long_ago() {
        let now = Local::now();
        let me = ("K", "Kacper", true, false, 5);
        let ola = ("O", "Ola", false, false, 4);
        assert_eq!(party(&[party_row(now, 30, &[me])], now), None);
        assert_eq!(party(&[], now), None);
        // Ended five hours ago.
        assert_eq!(party(&[party_row(now, 310, &[me, ola])], now), None);
        // Still a party a little under three hours after the match ended.
        assert!(party(&[party_row(now, 170, &[me, ola])], now).is_some());
        // Two matches with Ola more than three hours apart are two parties.
        let rows = vec![
            party_row(now, 20, &[me, ola]),
            party_row(now, 400, &[me, ola]),
        ];
        assert_eq!(party(&rows, now).unwrap().matches, 1);
    }

    #[test]
    fn a_party_lists_at_most_four_people_and_leaves_out_bots() {
        let now = Local::now();
        let row = party_row(
            now,
            15,
            &[
                ("K", "Kacper", true, false, 1),
                ("A", "A", false, false, 9),
                ("B", "B", false, false, 8),
                ("C", "C", false, false, 7),
                ("D", "D", false, false, 6),
                ("X", "Bot", false, true, 50),
            ],
        );
        let party = party(&[row], now).unwrap();
        let names: Vec<&str> = party.people.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, ["Kacper", "A", "B", "C"]);
        // The total counts everyone who is not a bot, even those cut from the list.
        assert_eq!(party.total, 31);
    }

    #[test]
    fn team_totals_follow_each_period_and_the_broken_setting() {
        let matches = [
            mate_match("a", at(6, 10, 0), 4, 1), // Friday, today
            mate_match("b", at(3, 9, 0), 2, 0),  // Tuesday of the same week
            mate_match("c", at(1, 9, 0), 6, 2),  // Sunday: last week, same month
        ];
        let now = at(6, 12, 0);
        let s = compute(&[], &matches, now, Some(at(6, 9, 0)), true);
        assert_eq!(s.team_today, 5);
        assert_eq!(s.team_week, 7);
        assert_eq!(s.team_month, 15);
        assert_eq!(s.team_total, 15);
        assert_eq!(s.team_session, 5); // only match "a" started after the session did
        assert_eq!(s.team_per_day.len(), 3);

        // With broken-open chests turned off, teammates' broken chests drop out like your own.
        let s = compute(&[], &matches, now, None, false);
        assert_eq!((s.team_today, s.team_week, s.team_month), (4, 6, 12));
        assert_eq!(s.team_session, 0);
    }

    #[test]
    fn solo_and_unread_matches_add_no_team_chests() {
        let mut manual = mate_match("m", at(6, 10, 0), 0, 0);
        manual.meta = None; // a manual entry has no replay data
        let s = compute(&[], &[manual], at(6, 12, 0), None, true);
        assert_eq!((s.team_today, s.team_week, s.team_month), (0, 0, 0));
        assert!(s.team_per_day.is_empty());
    }
}
