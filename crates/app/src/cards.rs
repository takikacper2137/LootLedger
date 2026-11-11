//! Dashboard cards from the statistics snapshot: a port of lootledger/panels.py. Text, order and wording
//! follow the 0.7.1 cards; shapes for charts are computed here in a 0..100 view box.

use crate::prefs::Prefs;
use crate::theme::Theme;
use crate::{
    BarRow, CardData, Cell, ChartPoint, Check, HeatCell, Mark, RingData, Slice, TableRow, Trophy,
};
use chrono::{Datelike, Duration, Local, NaiveDate};
use lootledger_core::stats::{self, Grain, MatchRow, Snapshot};
use slint::{Color, ModelRc, SharedString, VecModel};
use std::f64::consts::PI;
use std::rc::Rc;

pub fn model<T: Clone + 'static>(items: Vec<T>) -> ModelRc<T> {
    ModelRc::from(Rc::new(VecModel::from(items)))
}

fn s(text: impl Into<SharedString>) -> SharedString {
    text.into()
}

pub fn color(rgb: [f64; 3]) -> Color {
    Color::from_rgb_u8(
        (rgb[0].clamp(0.0, 1.0) * 255.0).round() as u8,
        (rgb[1].clamp(0.0, 1.0) * 255.0).round() as u8,
        (rgb[2].clamp(0.0, 1.0) * 255.0).round() as u8,
    )
}

fn cell(text: impl Into<SharedString>, tone: i32) -> Cell {
    Cell {
        text: text.into(),
        tone,
    }
}

fn row(cells: Vec<Cell>) -> TableRow {
    TableRow {
        cells: model(cells),
        dot: Color::default(),
        has_dot: false,
    }
}

pub const STAT_TITLES: &[(&str, &str, &str)] = &[
    ("all", "All time", "all_inclusive"),
    ("today", "Today", "today"),
    ("week", "This week", "date_range"),
    ("month", "This month", "calendar_month"),
    ("last", "Last match", "sports_esports"),
    ("avg", "Chests per match", "query_stats"),
    ("best", "Best match", "trophy"),
    ("per_hour", "Chests per hour", "schedule"),
    ("playtime", "Time played", "timer"),
    ("matches", "Matches read", "list_alt"),
    ("session", "This session", "play_circle"),
];
pub const TITLES: &[(&str, &str, &str)] = &[
    ("live", "Current match", "sports_esports"),
    ("trend", "Chests over time", "trending_up"),
    ("types", "By chest type", "category"),
    ("modes", "By game mode", "tune"),
    ("heat", "When you open chests", "grid_on"),
    ("records", "Records", "workspace_premium"),
    ("teammates", "You and your team", "groups"),
    ("broken", "Searched vs broken open", "broken_image"),
    ("health", "Setup and replay health", "health_and_safety"),
    ("sessions", "Sessions", "timelapse"),
    ("goals", "Goals", "flag"),
    ("trophies", "Trophies", "trophy"),
    ("compare", "Compare periods", "compare_arrows"),
    ("stats", "Match stats", "bar_chart"),
];
pub const STATS_ORDER: &[&str] = &[
    "health",
    "trend",
    "types",
    "modes",
    "heat",
    "records",
    "stats",
    "sessions",
    "goals",
    "teammates",
    "broken",
    "compare",
    "trophies",
    "stat:all",
    "stat:today",
    "stat:week",
    "stat:month",
    "stat:last",
    "stat:avg",
    "stat:best",
    "stat:per_hour",
    "stat:playtime",
    "stat:matches",
    "stat:session",
];

pub fn title_of(id: &str) -> (&'static str, &'static str) {
    let found = match id.strip_prefix("stat:") {
        Some(key) => STAT_TITLES.iter().find(|t| t.0 == key),
        None => TITLES.iter().find(|t| t.0 == id),
    };
    found.map_or(("", ""), |t| (t.1, t.2))
}

/// What the cards need besides the snapshot.
pub struct Context<'a> {
    pub snap: &'a Snapshot,
    pub prefs: &'a Prefs,
    pub theme: &'a Theme,
    pub live: LiveView,
    pub health: Option<Health>,
    pub trend: (i64, Grain),
    pub compare: [NaiveDate; 4],
}

#[derive(Clone, Default)]
pub struct LiveView {
    pub state: &'static str,
    pub elapsed: Option<f64>,
    pub recorded: f64,
    pub locked: bool,
    pub live_count: Option<(i64, String)>,
    pub ended: Option<MatchRow>,
    pub marks: Vec<(f64, String, String)>,
    pub pop_id: i32,
}

#[derive(Clone)]
pub struct Health {
    pub checks: Vec<(String, bool, String)>,
    pub replays: usize,
    pub newest: Option<f64>,
    pub read: i64,
    pub unreadable: i64,
    pub watching: String,
    pub last_scan: Option<chrono::DateTime<Local>>,
}

fn blank(id: &str, kind: &str, span: i32, ctx: &Context) -> CardData {
    let (title, icon) = title_of(id);
    CardData {
        id: s(id),
        kind: s(kind),
        title: s(title),
        icon: s(crate::icons::glyph(icon)),
        span,
        live_dot: color(ctx.theme.color("outline")),
        pop_color: color(ctx.theme.color("on_primary_container")),
        dates: model(vec![]),
        ..Default::default()
    }
}

/// A sparkline (line and area) through these values, 4% padding top and bottom.
pub fn spark(values: &[f64]) -> (String, String) {
    if values.is_empty() {
        return (String::new(), String::new());
    }
    let peak = values.iter().cloned().fold(0.0f64, f64::max).max(1e-9);
    let n = values.len();
    let pts: Vec<(f64, f64)> = values
        .iter()
        .enumerate()
        .map(|(i, v)| {
            (
                if n == 1 {
                    50.0
                } else {
                    i as f64 / (n - 1) as f64 * 100.0
                },
                96.0 - v / peak * 92.0,
            )
        })
        .collect();
    let mut line = format!("M {:.2} {:.2}", pts[0].0, pts[0].1);
    if n == 1 {
        line = format!("M 0 {:.2} L 100 {:.2}", pts[0].1, pts[0].1);
    }
    for p in &pts[1..] {
        line.push_str(&format!(" L {:.2} {:.2}", p.0, p.1));
    }
    let (first, last) = if n == 1 {
        (0.0, 100.0)
    } else {
        (pts[0].0, pts[n - 1].0)
    };
    let area = format!("{line} L {last:.2} 100 L {first:.2} 100 Z");
    (line, area)
}

/// Chart line through bar centres; smooth uses Catmull-Rom curves. Returns (line, area).
pub fn series_path(values: &[f64], smooth: bool) -> (String, String) {
    if values.is_empty() {
        return (String::new(), String::new());
    }
    let peak = values.iter().cloned().fold(0.0f64, f64::max).max(1e-9);
    let n = values.len();
    let pts: Vec<(f64, f64)> = values
        .iter()
        .enumerate()
        .map(|(i, v)| ((i as f64 + 0.5) / n as f64 * 100.0, 100.0 - v / peak * 96.0))
        .collect();
    let mut line = format!("M {:.2} {:.2}", pts[0].0, pts[0].1);
    for i in 1..n {
        if smooth {
            let p0 = if i >= 2 { pts[i - 2] } else { pts[i - 1] };
            let (p1, p2) = (pts[i - 1], pts[i]);
            let p3 = pts[(i + 1).min(n - 1)];
            let c1 = (
                p1.0 + (p2.0 - p0.0) / 6.0,
                (p1.1 + (p2.1 - p0.1) / 6.0).clamp(0.0, 100.0),
            );
            let c2 = (
                p2.0 - (p3.0 - p1.0) / 6.0,
                (p2.1 - (p3.1 - p1.1) / 6.0).clamp(0.0, 100.0),
            );
            line.push_str(&format!(
                " C {:.2} {:.2} {:.2} {:.2} {:.2} {:.2}",
                c1.0, c1.1, c2.0, c2.1, p2.0, p2.1
            ));
        } else {
            line.push_str(&format!(" L {:.2} {:.2}", pts[i].0, pts[i].1));
        }
    }
    let area = format!("{line} L {:.2} 100 L {:.2} 100 Z", pts[n - 1].0, pts[0].0);
    (line, area)
}

/// An arc of a circle centred at 50,50, from fraction `start` for `sweep` of a turn, clockwise from the top.
pub fn arc(radius: f64, start: f64, sweep: f64) -> String {
    let point = |f: f64| {
        (
            50.0 + radius * (2.0 * PI * f).sin(),
            50.0 - radius * (2.0 * PI * f).cos(),
        )
    };
    if sweep >= 0.9999 {
        let (a, b) = (point(start), point(start + 0.5));
        return format!(
            "M {:.3} {:.3} A {r} {r} 0 1 1 {:.3} {:.3} A {r} {r} 0 1 1 {:.3} {:.3}",
            a.0,
            a.1,
            b.0,
            b.1,
            a.0,
            a.1,
            r = radius
        );
    }
    let (a, b) = (point(start), point(start + sweep));
    format!(
        "M {:.3} {:.3} A {r} {r} 0 {} 1 {:.3} {:.3}",
        a.0,
        a.1,
        if sweep > 0.5 { 1 } else { 0 },
        b.0,
        b.1,
        r = radius
    )
}

fn days(count: i64) -> String {
    if count == 1 {
        "1 day".into()
    } else {
        format!("{count} days")
    }
}

fn plural(n: i64, word: &str) -> String {
    format!("{n} {word}{}", if n == 1 { "" } else { "es" })
}

fn clock(seconds: f64) -> String {
    let s = seconds.max(0.0) as i64;
    format!("{}:{:02}", s / 60, s % 60)
}

fn last_days_tips(ctx: &Context, count: i64) -> Vec<SharedString> {
    let today = ctx.snap.today_date;
    (0..count)
        .map(|i| s(ctx.prefs.short_date(today - Duration::days(count - 1 - i))))
        .collect()
}

/// (value, decimals, detail, delta, spark values, spark tips) for one stat tile, as stat_value in panels.py.
pub enum Value {
    Number(f64, usize),
    Text(String),
}

/// The line under a tile's number that adds the teammates' chests: "+640 from teammates · 5,117 together".
/// Empty when no teammate chest was recorded for the period (solo play, or nothing in range of your replay).
fn with_team(mine: i64, team: i64, p: &Prefs) -> String {
    if team <= 0 {
        return String::new();
    }
    format!(
        "+{} from teammates · {} together",
        p.int(team),
        p.int(mine + team)
    )
}

/// "Together: 15": your chests and your teammates' in one match, for the pop-out counter.
/// None when no teammate chest was recorded.
pub fn together(count: i64, team: i64, p: &Prefs) -> Option<String> {
    (team > 0).then(|| format!("Together: {}", p.int(count + team)))
}

/// "All time: 4,477": every chest you have opened, for the pop-out counter.
pub fn all_time(count: i64, p: &Prefs) -> String {
    format!("All time: {}", p.int(count))
}

/// The pop-out counter's party block: the party's chests in its last match and over the whole run, then one
/// line per person (you first, at most four). `live` is what you have opened so far in a match still being
/// played, which is not in the ledger yet.
pub fn party_lines(party: &lootledger_core::stats::Party, live: i64, p: &Prefs) -> Vec<String> {
    let mut lines = vec![format!(
        "Party: last match {} · this session {}",
        p.int(party.last),
        p.int(party.total + live)
    )];
    lines.extend(party.people.iter().map(|person| {
        let total = person.total + if person.me { live } else { 0 };
        format!("{} {}", person.name, p.int(total))
    }));
    lines
}

/// Joins detail lines, skipping empty ones.
fn lines(parts: &[String]) -> String {
    parts
        .iter()
        .filter(|t| !t.is_empty())
        .cloned()
        .collect::<Vec<_>>()
        .join("\n")
}

/// What a stat tile shows: value, detail, change versus before, sparkline values and their tooltips.
pub type StatValue = (
    Value,
    String,
    Option<(i64, i64, &'static str)>,
    Vec<f64>,
    Vec<SharedString>,
);

pub fn stat_value(key: &str, ctx: &Context) -> StatValue {
    let (snap, p) = (ctx.snap, ctx.prefs);
    let fourteen: Vec<f64> = stats::last_days(snap, 14)
        .into_iter()
        .map(|v| v as f64)
        .collect();
    let recent: Vec<f64> = snap.recent_totals.iter().map(|v| *v as f64).collect();
    let recent_tips: Vec<SharedString> = (0..recent.len())
        .map(|i| s(format!("Match {} of your last {}", i + 1, recent.len())))
        .collect();
    let day_tips = || last_days_tips(ctx, 14);
    let tips_for = |values: &[f64], tips: Vec<SharedString>| -> Vec<SharedString> {
        values
            .iter()
            .zip(tips)
            .map(|(v, t)| s(format!("{t} · {}", p.number(*v, 0))))
            .collect()
    };
    match key {
        "all" => {
            let detail = if snap.broken > 0 {
                format!("{} broken open", p.int(snap.broken))
            } else {
                "searched chests".into()
            };
            let tips = tips_for(&fourteen, day_tips());
            (
                Value::Number(snap.all as f64, 0),
                lines(&[detail, with_team(snap.all, snap.team_total, p)]),
                None,
                fourteen,
                tips,
            )
        }
        "today" => {
            let tips = tips_for(&fourteen, day_tips());
            (
                Value::Number(snap.today as f64, 0),
                with_team(snap.today, snap.team_today, p),
                Some((snap.today, snap.today_prev, "vs yesterday")),
                fourteen,
                tips,
            )
        }
        "week" => {
            let tips = tips_for(&fourteen, day_tips());
            (
                Value::Number(snap.week as f64, 0),
                with_team(snap.week, snap.team_week, p),
                Some((snap.week, snap.week_prev, "vs the same days last week")),
                fourteen,
                tips,
            )
        }
        "month" => {
            let tips = tips_for(&fourteen, day_tips());
            (
                Value::Number(snap.month as f64, 0),
                with_team(snap.month, snap.team_month, p),
                Some((snap.month, snap.month_prev, "vs the same days last month")),
                fourteen,
                tips,
            )
        }
        "session" => {
            let tips = tips_for(&fourteen, day_tips());
            (
                Value::Number(snap.session as f64, 0),
                lines(&[
                    "since LootLedger opened".into(),
                    with_team(snap.session, snap.team_session, p),
                ]),
                None,
                fourteen,
                tips,
            )
        }
        "matches" => {
            let tips = tips_for(&recent, recent_tips);
            (
                Value::Number(snap.matches as f64, 0),
                "replays read".into(),
                None,
                recent,
                tips,
            )
        }
        "last" => match &snap.last {
            None => (
                Value::Text("–".into()),
                "No match read yet".into(),
                None,
                vec![],
                vec![],
            ),
            Some(last) => {
                let place = last
                    .placement
                    .map(|n| format!(" · #{n}"))
                    .unwrap_or_default();
                let tips = tips_for(&recent, recent_tips);
                (
                    Value::Number(last.count as f64, 0),
                    lines(&[
                        format!("{}{place}", last.mode),
                        with_team(last.count, last.team, p),
                    ]),
                    None,
                    recent,
                    tips,
                )
            }
        },
        _ if snap.matches == 0 => (
            Value::Text("–".into()),
            "Read a match to see this".into(),
            None,
            vec![],
            vec![],
        ),
        "avg" => {
            let tips = tips_for(&recent, recent_tips);
            (
                Value::Number(snap.avg.unwrap_or(0.0), 1),
                format!("median {}", p.number(snap.median.unwrap_or(0.0), 1)),
                None,
                recent,
                tips,
            )
        }
        "best" => {
            let tips = tips_for(&recent, recent_tips);
            (
                Value::Number(snap.best.unwrap_or(0) as f64, 0),
                format!("lowest {}", p.int(snap.worst.unwrap_or(0))),
                None,
                recent,
                tips,
            )
        }
        "per_hour" => {
            let tips = tips_for(&recent, recent_tips);
            (
                Value::Number(snap.per_hour, 1),
                format!("over {}", p.duration(snap.playtime)),
                None,
                recent,
                tips,
            )
        }
        "playtime" => {
            let tips = tips_for(&recent, recent_tips);
            (
                Value::Text(p.duration(snap.playtime)),
                format!("{} matches", snap.matches),
                None,
                recent,
                tips,
            )
        }
        _ => (Value::Text("–".into()), String::new(), None, vec![], vec![]),
    }
}

fn stat_card(key: &str, span: i32, ctx: &Context) -> CardData {
    let mut card = blank(&format!("stat:{key}"), "stat", span, ctx);
    let (value, detail, delta, sparkv, tips) = stat_value(key, ctx);
    card.value = s(match value {
        Value::Number(v, d) => ctx.prefs.number(v, d),
        Value::Text(t) => t,
    });
    card.detail = s(detail);
    if let Some((current, previous, text)) = delta {
        let diff = current - previous;
        card.delta = s(match diff.signum() {
            1 => format!("▲ +{} {text}", ctx.prefs.int(diff.abs())),
            -1 => format!("▼ −{} {text}", ctx.prefs.int(diff.abs())),
            _ => format!("No change {text}"),
        });
        card.delta_tone = if diff > 0 { 2 } else { 0 };
    }
    let (line, area) = spark(&sparkv);
    card.spark_line = s(line);
    card.spark_area = s(area);
    card.spark_tips = model(tips);
    card
}

fn match_lines(row: &MatchRow, p: &Prefs) -> (String, String) {
    let minutes = row.seconds / 60.0;
    let pace = if minutes > 0.0 {
        format!(" · {} per minute", p.number(row.count as f64 / minutes, 1))
    } else {
        String::new()
    };
    let head = format!(
        "{} · {} searched · {} broken open{pace}",
        row.mode,
        p.int(row.count - row.broken),
        p.int(row.broken)
    );
    let types = row
        .types
        .most_common()
        .iter()
        .map(|(n, c)| format!("{n} {}", p.int(*c)))
        .collect::<Vec<_>>()
        .join(" · ");
    let mates: Vec<String> = row
        .squad
        .iter()
        .flatten()
        .filter(|m| !m.me && m.total > 0)
        .map(|m| format!("{} {}", m.name, p.int(m.total)))
        .collect();
    let team = if mates.is_empty() {
        String::new()
    } else {
        format!("Teammates: {}", mates.join(" · "))
    };
    (
        head,
        [types, team]
            .into_iter()
            .filter(|t| !t.is_empty())
            .collect::<Vec<_>>()
            .join("\n"),
    )
}

const RARITY: [&str; 3] = ["Relic chest", "Buried chest", "Rare chest"];

fn live_card(span: i32, ctx: &Context) -> CardData {
    let mut card = blank("live", "live", span, ctx);
    card.accent = true;
    let (t, p, live) = (ctx.theme, ctx.prefs, &ctx.live);
    let (mut text, role) = match live.state {
        "waiting" => ("Waiting for a match".to_owned(), "secondary"),
        "live" => ("Live".to_owned(), "error"),
        "saving" => ("Match over · reading the replay…".to_owned(), "tertiary"),
        "ended" => ("Match counted".to_owned(), "primary"),
        _ => ("Idle · Fortnite isn't running".to_owned(), "outline"),
    };
    card.live_dot = color(t.color(role));
    card.live_pulse = live.state == "live";
    if live.state == "live"
        && let Some(e) = live.elapsed
    {
        text += &format!(" · {}", clock(e));
    }
    card.detail = s(text);
    card.show_timeline = matches!(live.state, "live" | "ended");
    let mut lines = Vec::new();
    match (live.state, &live.ended) {
        ("live", _) if live.live_count.is_some() && !live.locked => {
            let (count, mode) = live.live_count.clone().unwrap_or_default();
            card.value = s(p.int(count));
            lines.push(cell(
                format!("{mode} · updates every ~20 s, when Fortnite saves the replay"),
                4,
            ));
            card.timeline_end = s(clock(live.recorded.max(1.0)));
        }
        ("live", _) => {
            card.value = s("…");
            lines.push(cell(
                "Chests appear when the match ends. Fortnite encrypts the replay while it records.",
                4,
            ));
            lines.push(cell(
                format!(
                    "Replay saved up to {} · Fortnite saves about every 20 s",
                    clock(live.recorded)
                ),
                4,
            ));
            let span = live.elapsed.unwrap_or(0.0).max(live.recorded).max(1.0);
            card.saved = (live.recorded / span) as f32;
            card.timeline_end = s(clock(span));
        }
        ("ended", Some(row)) => {
            card.value = s(p.int(row.count));
            let (head, detail) = match_lines(row, p);
            lines.push(cell(head, 4));
            if !detail.is_empty() {
                lines.push(cell(detail, 4));
            }
            let length = row.seconds.max(1.0);
            card.marks = model(
                live.marks
                    .iter()
                    .map(|(time, kind, source)| Mark {
                        x: (time / length).clamp(0.0, 1.0) as f32,
                        color: color(t.type_color(kind)),
                        tip: s(format!(
                            "{} · {kind}{}",
                            clock(*time),
                            if source == "replay-broken" {
                                " · broken open"
                            } else {
                                ""
                            }
                        )),
                    })
                    .collect(),
            );
            card.saved = -1.0;
            card.timeline_end = s(clock(row.seconds));
            let rare = RARITY.iter().find(|n| row.types.get(n) > 0);
            card.pop = s(format!("+{}", row.count));
            card.pop_color =
                color(rare.map_or(t.color("on_primary_container"), |n| t.type_color(n)));
            card.pop_id = live.pop_id;
        }
        _ => {
            card.value = s(if live.state == "saving" { "…" } else { "–" });
            lines.push(cell(
                if live.state == "saving" { "Reading the finished replay." } else { "Finished matches are added automatically. Fortnite must have Record Replays on." },
                4,
            ));
        }
    }
    card.lines = model(lines);
    card
}

const GRAINS: [Grain; 3] = [Grain::Day, Grain::Week, Grain::Month];

fn trend_card(span: i32, ctx: &Context) -> CardData {
    let mut card = blank("trend", "trend", span, ctx);
    let (snap, p) = (ctx.snap, ctx.prefs);
    let (days, grain) = ctx.trend;
    card.range_index = stats_range_index(days);
    card.grain_index = GRAINS.iter().position(|g| *g == grain).unwrap_or(0) as i32;
    card.style_index = ["bar", "line", "smooth", "area"]
        .iter()
        .position(|x| *x == p.chart_style)
        .unwrap_or(0) as i32;
    let points = stats::trend(snap, grain, days, false);
    let played = stats::trend(snap, grain, days, true);
    let label = |day: NaiveDate| match grain {
        Grain::Day => p.short_date(day),
        Grain::Week => format!("Week of {}", p.short_date(day)),
        Grain::Month => day.format("%b %Y").to_string(),
    };
    let unit = match grain {
        Grain::Day => "day",
        Grain::Week => "week",
        Grain::Month => "month",
    };
    let peak = points.iter().map(|x| x.1).max().unwrap_or(0).max(1) as f64;
    let chart: Vec<ChartPoint> = points
        .iter()
        .enumerate()
        .map(|(i, (day, value))| {
            let matches = played.get(i).map_or(0, |x| x.1);
            let mut tip = format!(
                "{} · {} chests\n{}",
                label(*day),
                p.int(*value),
                plural(matches, "match")
            );
            if i > 0 {
                let change = value - points[i - 1].1;
                let sign = if change > 0 {
                    "+"
                } else if change < 0 {
                    "−"
                } else {
                    "±"
                };
                tip += &format!("\n{sign}{} vs the previous {unit}", p.int(change.abs()));
            }
            ChartPoint {
                label: s(label(*day)),
                value: (*value as f64 / peak) as f32,
                tip: s(tip),
            }
        })
        .collect();
    let values: Vec<f64> = points.iter().map(|x| x.1 as f64).collect();
    let (line, area) = series_path(&values, p.chart_style == "smooth");
    card.line = s(line);
    card.area = s(area);
    // About five axis labels, evenly spread, that never overlap.
    let n = points.len();
    let shown = n.min(5);
    let axis: Vec<SharedString> = (0..shown)
        .map(|k| {
            let i = if shown <= 1 {
                0
            } else {
                k * (n - 1) / (shown - 1)
            };
            let day = points[i].0;
            s(match grain {
                Grain::Month => day.format("%b %y").to_string(),
                _ => p.short_date(day),
            })
        })
        .collect();
    card.axis = model(axis);
    let total: i64 = values.iter().map(|v| *v as i64).sum();
    card.empty = s(if total == 0 {
        "No chests in this range yet"
    } else {
        ""
    });
    card.points = model(chart);
    card.caption = s(format!("{} chests in this range", p.int(total)));
    card
}

pub fn stats_range_index(days: i64) -> i32 {
    crate::prefs::RANGES
        .iter()
        .position(|d| *d == days)
        .unwrap_or(1) as i32
}

fn types_card(span: i32, ctx: &Context) -> CardData {
    let mut card = blank("types", "types", span, ctx);
    let (snap, p, t) = (ctx.snap, ctx.prefs, ctx.theme);
    let types = snap.types.most_common();
    let total: i64 = types.iter().map(|x| x.1).sum::<i64>().max(1);
    let shown: Vec<&(String, i64)> = types.iter().filter(|x| x.1 > 0).collect();
    let gap = if shown.len() > 1 { 0.006 } else { 0.0 };
    let mut start = 0.0;
    let mut slices = Vec::new();
    for (name, value) in &shown {
        let sweep = *value as f64 / total as f64;
        slices.push(Slice {
            start: start as f32,
            sweep: sweep as f32,
            color: color(t.type_color(name)),
            tip: s(format!(
                "{name} · {} ({}%)",
                p.int(*value),
                (sweep * 100.0).round()
            )),
            path: s(arc(40.0, start + gap / 2.0, (sweep - gap).max(0.002))),
        });
        start += sweep;
    }
    card.slices = model(slices);
    card.center = s(p.int(snap.all));
    card.caption = s("chests");
    let mut names: Vec<String> = types.iter().map(|x| x.0.clone()).collect();
    let mut extra: Vec<String> = snap
        .team_types
        .keys()
        .filter(|k| snap.types.get(k) == 0 && !names.iter().any(|n| n == k))
        .map(str::to_owned)
        .collect();
    extra.sort();
    names.extend(extra);
    let mut rows = vec![row(vec![
        cell("Type", 3),
        cell("You", 3),
        cell("Teammates*", 3),
    ])];
    for name in &names {
        let mut r = row(vec![
            cell(name.clone(), 4),
            cell(p.int(snap.types.get(name)), 1),
            cell(p.int(snap.team_types.get(name)), 0),
        ]);
        r.dot = color(t.type_color(name));
        r.has_dot = true;
        rows.push(r);
    }
    if names.is_empty() {
        rows.push(row(vec![cell("No chests yet", 0)]));
    } else {
        rows.push(row(vec![
            cell("Total", 3),
            cell(p.int(snap.all), 1),
            cell(p.int(snap.team_total), 0),
        ]));
    }
    card.table = model(rows);
    card.note = s("*Only chests your replays recorded. Details on the Teammates page.");
    card
}

fn bars(rows: Vec<(String, i64, String)>) -> ModelRc<BarRow> {
    let peak = rows.iter().map(|r| r.1).max().unwrap_or(0).max(1) as f32;
    model(
        rows.into_iter()
            .map(|(label, value, text)| BarRow {
                label: s(label),
                fraction: value as f32 / peak,
                text: s(text),
                ..Default::default()
            })
            .collect(),
    )
}

fn modes_card(span: i32, ctx: &Context) -> CardData {
    let mut card = blank("modes", "bars", span, ctx);
    let mut modes = ctx.snap.modes.clone();
    modes.sort_by_key(|m| std::cmp::Reverse(m.1.chests));
    modes.truncate(8);
    if modes.is_empty() {
        card.lines = model(vec![cell("Read a match to see your modes", 0)]);
    }
    card.bars = bars(
        modes
            .into_iter()
            .map(|(name, m)| {
                (
                    name,
                    m.chests,
                    format!(
                        "{} · {}",
                        ctx.prefs.int(m.chests),
                        plural(m.matches, "match")
                    ),
                )
            })
            .collect(),
    );
    card
}

fn heat_card(span: i32, ctx: &Context) -> CardData {
    let mut card = blank("heat", "heat", span, ctx);
    let (snap, p) = (ctx.snap, ctx.prefs);
    let peak = snap
        .heat
        .iter()
        .flatten()
        .max()
        .copied()
        .unwrap_or(0)
        .max(1) as f32;
    let names = [
        "Monday",
        "Tuesday",
        "Wednesday",
        "Thursday",
        "Friday",
        "Saturday",
        "Sunday",
    ];
    let mut cells = Vec::with_capacity(168);
    for (d, day) in snap.heat.iter().enumerate() {
        for (h, value) in day.iter().enumerate() {
            let matches = snap.heat_matches[d][h];
            cells.push(HeatCell {
                alpha: *value as f32 / peak,
                tip: s(format!(
                    "{} {:02}:00 · {} chests · {}",
                    names[d],
                    h,
                    p.int(*value),
                    plural(matches, "match")
                )),
            });
        }
    }
    card.heat = model(cells);
    card.note = s("Local time · darker means more chests");
    card
}

fn records_card(span: i32, ctx: &Context) -> CardData {
    let mut card = blank("records", "table", span, ctx);
    let (snap, p) = (ctx.snap, ctx.prefs);
    let mut items = Vec::new();
    if let Some(best) = snap.best {
        items.push(("Best match", format!("{} chests", p.int(best))));
    }
    if let Some((day, count)) = snap.best_day {
        items.push((
            "Best day",
            format!("{} · {}", p.int(count), p.short_date(day)),
        ));
    }
    if let Some(session) = &snap.session_best {
        let length = (session.end - session.start).num_milliseconds() as f64 / 1000.0;
        items.push((
            "Longest session",
            format!("{} · {} chests", p.duration(length), p.int(session.chests)),
        ));
    }
    if snap.peak_hour.0 > 0 {
        items.push(("Most in one hour", p.int(snap.peak_hour.0)));
    }
    items.push(("Longest daily streak", days(snap.streak_longest)));
    items.push(("Current streak", days(snap.streak_current)));
    card.table = model(
        items
            .into_iter()
            .map(|(n, v)| row(vec![cell(n, 0), cell(v, 1)]))
            .collect(),
    );
    card
}

fn team_card(span: i32, ctx: &Context) -> CardData {
    let mut card = blank("teammates", "bars", span, ctx);
    let (snap, p) = (ctx.snap, ctx.prefs);
    if snap.matches == 0 {
        card.lines = model(vec![cell("Read a match with teammates to compare.", 0)]);
        return card;
    }
    let (you, team) = (snap.match_chests, snap.team_total);
    let mut rows = vec![
        (
            "You".to_owned(),
            you,
            format!(
                "{} · {} per match",
                p.int(you),
                p.number(snap.avg.unwrap_or(0.0), 1)
            ),
        ),
        (
            "Teammates".to_owned(),
            team,
            format!(
                "{} · {} per match",
                p.int(team),
                p.number(snap.team_per_match.unwrap_or(0.0), 1)
            ),
        ),
    ];
    for m in snap.board.iter().filter(|m| !m.me && !m.unranked).take(3) {
        rows.push((
            m.name.clone(),
            m.total,
            format!("{} in {}", p.int(m.total), plural(m.matches, "match")),
        ));
    }
    card.bars = bars(rows);
    let share = snap
        .your_share
        .map(|x| {
            format!(
                "You opened {}% of the chests your replays recorded. ",
                (x * 100.0).round()
            )
        })
        .unwrap_or_default();
    card.note = s(share
        + "Teammate counts only include chests your replays recorded. Full squad breakdown: Teammates page.");
    card
}

fn broken_card(span: i32, ctx: &Context) -> CardData {
    let mut card = blank("broken", "table", span, ctx);
    let (snap, p) = (ctx.snap, ctx.prefs);
    let searched = (snap.all - snap.broken, snap.team_opened);
    let broken = (snap.broken, snap.team_broken);
    let total = (searched.0 + broken.0, searched.1 + broken.1);
    let mut rows = vec![row(vec![
        cell("", 3),
        cell("You", 3),
        cell("Teammates*", 3),
    ])];
    for (name, pair) in [
        ("Searched", searched),
        ("Broken open", broken),
        ("Total", total),
    ] {
        rows.push(row(vec![
            cell(name, if name == "Total" { 3 } else { 0 }),
            cell(p.int(pair.0), 1),
            cell(p.int(pair.1), 1),
        ]));
    }
    card.table = model(rows);
    let mut ammo: Vec<String> = snap
        .containers
        .keys()
        .chain(snap.team_containers.keys())
        .map(str::to_owned)
        .collect();
    ammo.sort();
    ammo.dedup();
    let mut notes = Vec::new();
    if !ammo.is_empty() {
        notes.push(format!(
            "Not counted as chests: {}",
            ammo.iter()
                .map(|n| format!(
                    "{n}, you {} / teammates {}",
                    p.int(snap.containers.get(n)),
                    p.int(snap.team_containers.get(n))
                ))
                .collect::<Vec<_>>()
                .join(" · ")
        ));
    }
    notes.push("*Teammates: only chests your replays recorded. Broken open means the chest was destroyed before anyone searched it.".into());
    card.note = s(notes.join("\n"));
    card
}

fn health_card(span: i32, ctx: &Context) -> CardData {
    let mut card = blank("health", "health", span, ctx);
    let Some(h) = &ctx.health else { return card };
    let p = ctx.prefs;
    let mut blocked = false;
    let mut checks = Vec::new();
    for (name, ok, fix) in &h.checks {
        let fix = if *ok {
            String::new()
        } else if blocked {
            "Waiting on the step above.".into()
        } else {
            format!("Fix it: {fix}")
        };
        if !ok {
            blocked = true; // later checks depend on earlier ones: one fix at a time
        }
        checks.push(Check {
            ok: *ok,
            title: s(name.clone()),
            fix: s(fix),
        });
    }
    card.checks = model(checks);
    let checked = h
        .last_scan
        .map_or("not yet".to_owned(), |t| t.format("%H:%M:%S").to_string());
    let newest = h.newest.map_or("none yet".to_owned(), |t| {
        format!(
            "newest {} ago",
            crate::prefs::elapsed(crate::live::seconds(std::time::SystemTime::now()) - t)
        )
    });
    card.note = s(format!(
        "{} · last check {checked}\n{} replays in the folder ({newest}) · {} read · {} could not be read",
        h.watching,
        p.int(h.replays as i64),
        p.int(h.read),
        p.int(h.unreadable)
    ));
    card
}

fn sessions_card(span: i32, ctx: &Context) -> CardData {
    let mut card = blank("sessions", "bars", span, ctx);
    let p = ctx.prefs;
    let found = stats::sessions(&ctx.snap.match_rows);
    let Some(last) = found.first() else {
        card.value = s("–");
        card.lines = model(vec![cell(
            "Play a few matches: matches less than 30 minutes apart make a session.",
            0,
        )]);
        return card;
    };
    card.value = s(p.int(last.chests));
    let mut lines = vec![cell(
        format!(
            "Latest session · {} · {} · {} · {} chests per hour",
            p.short_date(last.start.date_naive()),
            plural(last.matches, "match"),
            p.duration(last.seconds),
            p.number(last.per_hour, 1)
        ),
        0,
    )];
    let average = found.iter().map(|x| x.per_hour).sum::<f64>() / found.len() as f64;
    let mut parts = vec![format!(
        "Average session: {} per hour",
        p.number(average, 1)
    )];
    if let Some(prev) = found.get(1) {
        parts.insert(
            0,
            format!(
                "Previous: {} chests, {} per hour",
                p.int(prev.chests),
                p.number(prev.per_hour, 1)
            ),
        );
    }
    lines.push(cell(parts.join(" · "), 0));
    card.lines = model(lines);
    card.bars = bars(
        found
            .iter()
            .take(5)
            .map(|x| {
                (
                    p.short_date(x.start.date_naive()),
                    x.chests,
                    p.int(x.chests),
                )
            })
            .collect(),
    );
    card
}

fn goals_card(span: i32, ctx: &Context) -> CardData {
    let mut card = blank("goals", "goals", span, ctx);
    let p = ctx.prefs;
    let goals = stats::goals(ctx.snap, p.goal_daily, p.goal_weekly);
    card.rings = model(
        goals
            .iter()
            .map(|g| RingData {
                share: g.share as f32,
                text: s(format!("{}/{}", p.int(g.done), p.int(g.target))),
                caption: s(format!(
                    "{}{}",
                    g.name,
                    if g.reached { " · done" } else { "" }
                )),
                path: s(arc(42.0, 0.0, g.share.max(0.001))),
            })
            .collect(),
    );
    let streak = ctx.snap.streak_current;
    let text = format!(
        "Streak: {streak} day{} in a row with chests.",
        if streak == 1 { "" } else { "s" }
    );
    card.note = s(if goals.is_empty() {
        text + " Set a daily or weekly goal in Settings → Goals."
    } else {
        text
    });
    card
}

fn trophies_card(span: i32, ctx: &Context) -> CardData {
    let mut card = blank("trophies", "trophies", span, ctx);
    let items = stats::achievements(ctx.snap);
    card.note = s(format!(
        "{} of {} unlocked",
        items.iter().filter(|i| i.unlocked).count(),
        items.len()
    ));
    card.trophies = model(
        items
            .into_iter()
            .map(|i| Trophy {
                title: s(i.title),
                detail: s(i.detail),
                unlocked: i.unlocked,
            })
            .collect(),
    );
    card
}

pub fn default_compare(today: NaiveDate) -> [NaiveDate; 4] {
    let monday = today - Duration::days(today.weekday().num_days_from_monday() as i64);
    [
        monday - Duration::days(7),
        monday - Duration::days(1),
        monday,
        today,
    ]
}

fn compare_card(span: i32, ctx: &Context) -> CardData {
    let mut card = blank("compare", "compare", span, ctx);
    let (p, d) = (ctx.prefs, ctx.compare);
    card.dates = model(d.iter().map(|x| s(p.full_date(*x))).collect());
    let mut rows = vec![row(vec![
        cell("", 3),
        cell("First", 3),
        cell("Second", 3),
        cell("Change", 3),
    ])];
    for (name, a, b, change) in stats::compare(ctx.snap, (d[0], d[1]), (d[2], d[3])) {
        let decimals = if name == "Chests per match" { 1 } else { 0 };
        let sign = if change > 0.0 {
            "+"
        } else if change < 0.0 {
            "−"
        } else {
            "±"
        };
        rows.push(row(vec![
            cell(name, 0),
            cell(p.number(a, decimals), 1),
            cell(p.number(b, decimals), 1),
            cell(
                format!("{sign}{}", p.number(change.abs(), decimals)),
                if change > 0.0 { 2 } else { 0 },
            ),
        ]));
    }
    card.table = model(rows);
    card
}

/// Match stats as the replay stores them, in display order: (label, key, how to show it).
pub const MATCH_STATS: &[(&str, &str, StatUnit)] = &[
    ("Damage to players", "damage_to_players", StatUnit::Count),
    ("Damage taken", "damage_taken", StatUnit::Count),
    ("Eliminations", "eliminations", StatUnit::Count),
    ("Assists", "assists", StatUnit::Count),
    ("Accuracy", "accuracy", StatUnit::Percent),
    ("Revives", "revives", StatUnit::Count),
    (
        "Damage to structures",
        "damage_to_structures",
        StatUnit::Count,
    ),
    ("Materials gathered", "materials_gathered", StatUnit::Count),
    ("Materials used", "materials_used", StatUnit::Count),
    ("Distance travelled", "distance", StatUnit::Distance),
];

#[derive(Clone, Copy, PartialEq)]
pub enum StatUnit {
    Count,
    Percent,
    /// Centimetres in the replay, shown in kilometres.
    Distance,
}

pub fn stat_text(p: &Prefs, unit: StatUnit, value: f64) -> String {
    match unit {
        StatUnit::Count => p.number(value, 0),
        StatUnit::Percent => format!("{}%", p.number(value * 100.0, 0)),
        StatUnit::Distance => format!("{} km", p.number(value / 100_000.0, 1)),
    }
}

fn stats_card(span: i32, ctx: &Context) -> CardData {
    let mut card = blank("stats", "table", span, ctx);
    let p = ctx.prefs;
    let with: Vec<&serde_json::Value> = ctx
        .snap
        .match_rows
        .iter()
        .filter_map(|r| r.stats.as_ref())
        .collect();
    if with.is_empty() {
        card.note = s(
            "No match stats yet. They come from replays LootLedger reads from now on, and from older replays Fortnite still keeps.",
        );
        return card;
    }
    let mut rows = vec![row(vec![
        cell("", 3),
        cell("Per match", 3),
        cell("Best", 3),
        cell("Total", 3),
    ])];
    for (label, key, unit) in MATCH_STATS {
        let values: Vec<f64> = with.iter().filter_map(|st| st[*key].as_f64()).collect();
        let total: f64 = values.iter().sum();
        let best = values.iter().cloned().fold(0.0, f64::max);
        let average = total / values.len().max(1) as f64;
        let total_text = if *unit == StatUnit::Percent {
            "–".to_owned()
        } else {
            stat_text(p, *unit, total)
        };
        rows.push(row(vec![
            cell(*label, 0),
            cell(stat_text(p, *unit, average), 1),
            cell(stat_text(p, *unit, best), 4),
            cell(total_text, 0),
        ]));
    }
    card.table = model(rows);
    card.note = s(format!(
        "From {} of {} matches. Matches whose replay Fortnite has already deleted have no stats.",
        with.len(),
        ctx.snap
            .match_rows
            .iter()
            .filter(|r| r.mode != "Manual")
            .count()
    ));
    card
}

pub fn build(id: &str, span: i32, ctx: &Context) -> CardData {
    if let Some(key) = id.strip_prefix("stat:") {
        return stat_card(key, span, ctx);
    }
    match id {
        "live" => live_card(span, ctx),
        "trend" => trend_card(span, ctx),
        "types" => types_card(span, ctx),
        "modes" => modes_card(span, ctx),
        "heat" => heat_card(span, ctx),
        "records" => records_card(span, ctx),
        "teammates" => team_card(span, ctx),
        "broken" => broken_card(span, ctx),
        "health" => health_card(span, ctx),
        "sessions" => sessions_card(span, ctx),
        "goals" => goals_card(span, ctx),
        "trophies" => trophies_card(span, ctx),
        "compare" => compare_card(span, ctx),
        "stats" => stats_card(span, ctx),
        _ => blank(id, "table", span, ctx),
    }
}

/// Place cards into rows of `columns`, as DashGrid.reflow: a card that does not fit starts the next row.
pub fn pack(cards: Vec<CardData>, columns: i32) -> Vec<crate::DashRow> {
    let columns = columns.max(1);
    let mut rows: Vec<(Vec<CardData>, i32, i32)> = Vec::new();
    let mut used = columns;
    for (i, mut card) in cards.into_iter().enumerate() {
        card.span = card.span.min(columns);
        if used + card.span > columns {
            rows.push((Vec::new(), 0, i as i32));
            used = 0;
        }
        used += card.span;
        let last = rows.last_mut().expect("row");
        last.1 += card.span;
        last.0.push(card);
    }
    rows.into_iter()
        .map(|(cards, used, first)| crate::DashRow {
            cards: model(cards),
            free: columns - used,
            first,
        })
        .collect()
}

/// The match pop-up's second line: how this match compares, then today's total.
pub fn popup_detail(count: i64, snap: &Snapshot, p: &Prefs) -> String {
    versus(count, snap.matches, snap.best, snap.avg, snap.today, p)
}

fn versus(
    count: i64,
    matches: i64,
    best: Option<i64>,
    avg: Option<f64>,
    today: i64,
    p: &Prefs,
) -> String {
    let mut parts = Vec::new();
    if matches > 1 && best == Some(count) {
        parts.push("Your best match yet".to_owned());
    } else if let Some(avg) = avg {
        let diff = (count as f64 - avg).round() as i64;
        parts.push(match diff {
            0 => "Right on your average".to_owned(),
            d if d > 0 => format!("+{} vs your average", p.int(d)),
            d => format!("−{} vs your average", p.int(-d)),
        });
    }
    parts.push(format!("Today {}", p.int(today)));
    parts.join(" · ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn popup_compares_with_the_average_or_calls_a_best() {
        let p = Prefs::default();
        let line = |count| versus(count, 10, Some(40), Some(20.4), 1234, &p);
        assert_eq!(line(23), "+3 vs your average · Today 1,234");
        assert_eq!(line(17), "−3 vs your average · Today 1,234");
        assert_eq!(line(20), "Right on your average · Today 1,234");
        assert_eq!(line(40), "Your best match yet · Today 1,234");
        // The first match is not a "best" yet, and with no average only today is shown.
        assert_eq!(versus(5, 1, Some(5), None, 5, &p), "Today 5");
    }

    #[test]
    fn tiles_add_the_teammates_line_only_when_there_is_one() {
        let p = Prefs::default();
        assert_eq!(
            with_team(187, 412, &p),
            "+412 from teammates · 599 together"
        );
        assert_eq!(
            with_team(4477, 640, &p),
            "+640 from teammates · 5,117 together"
        );
        assert_eq!(with_team(187, 0, &p), "");
        assert_eq!(
            lines(&["1,788 broken open".into(), String::new()]),
            "1,788 broken open"
        );
        assert_eq!(lines(&["a".into(), "b".into()]), "a\nb");
        assert_eq!(together(13, 2, &p).as_deref(), Some("Together: 15"));
        assert_eq!(together(13, 0, &p), None);
        assert_eq!(all_time(4477, &p), "All time: 4,477");
    }

    #[test]
    fn the_party_block_lists_everyone_and_counts_the_live_match_for_you() {
        use lootledger_core::stats::{Party, PartyPerson};
        let p = Prefs::default();
        let person = |name: &str, me: bool, total: i64| PartyPerson {
            name: name.into(),
            me,
            total,
        };
        let party = Party {
            matches: 2,
            last: 35,
            total: 58,
            people: vec![
                person("Kacper", true, 35),
                person("Ola", false, 18),
                person("Wolf", false, 5),
            ],
        };
        assert_eq!(
            party_lines(&party, 0, &p),
            [
                "Party: last match 35 · this session 58",
                "Kacper 35",
                "Ola 18",
                "Wolf 5"
            ]
        );
        // Four chests into a live match: yours and the session total grow, the others' stay.
        assert_eq!(
            party_lines(&party, 4, &p),
            [
                "Party: last match 35 · this session 62",
                "Kacper 39",
                "Ola 18",
                "Wolf 5"
            ]
        );
    }

    #[test]
    fn packing_wraps_wide_cards_like_the_old_grid() {
        let card = |span| CardData {
            span,
            ..Default::default()
        };
        let rows = pack(vec![card(2), card(1), card(1), card(2), card(1)], 3);
        let shape: Vec<(usize, i32, i32)> = rows
            .iter()
            .map(|r| (slint::Model::row_count(&r.cards), r.free, r.first))
            .collect();
        assert_eq!(shape, vec![(2, 0, 0), (2, 0, 2), (1, 2, 4)]);
        let narrow = pack(vec![card(2), card(1)], 1);
        assert_eq!(narrow.len(), 2);
    }

    #[test]
    fn arcs_and_lines_are_well_formed() {
        assert!(arc(40.0, 0.0, 0.25).starts_with("M 50.000 10.000 A 40 40 0 0 1 90.000 50.000"));
        assert!(arc(40.0, 0.0, 1.0).matches(" A ").count() == 2);
        let (line, area) = spark(&[0.0, 2.0, 4.0]);
        assert!(line.starts_with("M 0.00 96.00") && area.ends_with('Z'));
        let (smooth, _) = series_path(&[1.0, 3.0, 2.0], true);
        assert_eq!(smooth.matches(" C ").count(), 2);
    }
}
