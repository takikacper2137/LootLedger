<p align="center">
  <img src=".github/readme/icon.png" width="96" alt="LootLedger icon">
</p>

<h1 align="center">LootLedger</h1>

<p align="center">
  <b>Count every Fortnite chest you open: per match, per day, per type, all time.</b><br>
  Reads the replays Fortnite already saves on your PC. No account, no setup, no game hooks.
</p>

<p align="center">
  <a href="https://github.com/femboypuppy/LootLedger/releases/latest"><b>⬇ Download LootLedger.exe</b></a>
  &nbsp;·&nbsp; Windows 10/11 &nbsp;·&nbsp; one 13 MB file &nbsp;·&nbsp; free and open source
</p>

<p align="center">
  <img src=".github/readme/tour.gif" width="900" alt="A tour of LootLedger: Overview, Matches, Openings, Statistics, Teammates and Settings">
</p>

<sub>Screenshots use made-up demo data.</sub>

## Get started

1. In Fortnite: **Settings → Game → Replays → Record Replays: On**.
2. Download **[LootLedger.exe](https://github.com/femboypuppy/LootLedger/releases/latest)** and run it. Nothing to install. SmartScreen may warn because the exe is unsigned: click **More info → Run anyway**.
3. Play. Each finished match is counted a few seconds after it ends.

LootLedger never touches the game. It doesn't read game memory, inject anything or automate gameplay. It only reads the `.replay` files Fortnite writes once a match is over. Your data stays on your PC. The only internet traffic is the update check, plus the optional match sharing with friends (off by default; see below).

## What you get

### A dashboard you can arrange

Today, this week, all time, last match, chests per match and per hour, goals, sessions, records and more. Click the tune button to add, remove, resize and drag cards.

<img src=".github/readme/overview.png" width="900" alt="The Overview dashboard in light mode">

### Every match, with the details

Search and filter your matches. Each one shows:
- your chests by type, and a timeline of when you opened them;
- your squad, with each player's chests and the match MVP;
- your match stats: damage, eliminations, accuracy, materials, distance travelled and more;
- your notes, tags and favourites.

<img src=".github/readme/matches.png" width="900" alt="The Matches page with a match opened">

### Statistics and teammates

Charts over any range, a breakdown by chest type and game mode, a weekday-by-hour heatmap, records, trophies, and a comparison of any two periods. The Teammates page ranks everyone you played with and has a profile for each player.

<p>
  <img src=".github/readme/statistics.png" width="445" alt="The Statistics page">
  <img src=".github/readme/teammates.png" width="445" alt="The Teammates page">
</p>

### Make it yours

Light or dark, six accent colours or your own, plus contrast, corner style, density, text size, chart style and animation settings.

<img src=".github/readme/themes.gif" width="720" alt="The Overview page cycling through accent colours in light and dark mode">

### Share your week

Save or copy a summary card or a weekly recap image, ready to post.

<img src=".github/readme/recap.png" width="600" alt="A weekly recap image">

### Also included

- **Pop-up after each match:** your chests, placement and how the match compares, in the corner for a few seconds, without taking focus from the game.
- **Live match card** and a pop-out, always-on-top **counter** for a second screen.
- **Corrections:** remove, restore or fix any entry, add a manual +1, and undo. Every change is kept in an audit trail, and re-reading replays never overwrites your edits.
- **Export** to JSON or CSV, **backups** and **restore**. **Clear all data** saves a backup first.
- **Global shortcuts**, a command palette (**Ctrl+K**) and keyboard navigation (**Ctrl+1** to **Ctrl+6**; **?** or **F1** lists all shortcuts).
- **Automatic updates:** new versions download from this project's GitHub releases. Each download is checked against GitHub's published SHA-256 and installed when you close the app. You can turn this off in **Settings → About**.

## What counts as a chest

| Shown as | Fortnite classes (from the replay) | In chest total |
|---|---|---|
| Chest | `Tiered_Chest_Athena_C`, `AlwaysSpawn_NormalChest_C` | yes |
| Rare chest | `AlwaysSpawn_RareChest_C` | yes |
| Relic chest | `B_TieredChest_Relics_C`, `…_Relics_Variant1_C` | yes |
| Buried chest | `B_Athena_BuriedChest_Chest_C`, `…_RelicFinder_C` | yes |
| Sprite poster chest | `B_SpritePosters_Booty_Chest_C` | yes |
| Other chest | any other class with "Chest" in its name | yes |
| Ammo box / Food box / Fishing barrel | `Tiered_Ammo_Athena_C`, … | no, listed separately |

**Broken open** means your damage destroyed the chest before anyone searched it, so its loot spilled. These count by default; you can turn that off in Settings.

**Teammates** only includes chests your replay recorded. Fortnite records what happens near you, so a teammate looting far away won't appear. Chests that were already open when your game first saw them count in match totals but aren't credited to anyone.

Fortnite keeps only about your 100 most recent replays. LootLedger keeps every count once it has read a replay. Your data lives in `%LOCALAPPDATA%\LootLedger`.

## Build from source

Requirements: Windows 10/11 x64 and [Rust](https://rustup.rs). The Rust version is pinned in `rust-toolchain.toml`, so `rustup` installs the right one.

```powershell
cargo build --release -p lootledger   # target\release\lootledger.exe
cargo test --workspace
```

Pushing a `v*` tag runs `.github/workflows/release.yml`. It checks formatting, lints and tests, builds the exe, confirms it imports only Windows system DLLs, and publishes it to a GitHub release with `.github/release-notes.md`.

### Layout

- `crates/core` holds the logic, with no UI:
  - `container.rs`: replay file format (header, chunks, decryption, Oodle).
  - `replay.rs` and `bits.rs`: the Unreal replay decoder and chest attribution.
  - `store.rs`: the SQLite ledger.
  - `stats.rs`: statistics.
- `crates/app` is the [Slint](https://slint.dev) app:
  - `ui/*.slint`: the interface, with all colours and sizes in `tokens.slint`.
  - `main.rs`: wiring.
  - `cards.rs`: dashboard cards.
  - `pages.rs`: list pages.
  - `live.rs`: live match.
  - `update.rs`: updates.
  - `native.rs`: Win32 window, taskbar icon and hotkeys.
- `scripts/` holds two helpers:
  - `subset_icons.py`: rebuilds the icon font.
  - `check_pe_runtime.py`: the runtime-DLL check used in CI.

[Made with Slint](https://slint.dev). Licences are in [THIRD_PARTY.md](THIRD_PARTY.md); test notes are in [VALIDATION.md](VALIDATION.md). Fortnite is a trademark of Epic Games; LootLedger is not affiliated with or endorsed by Epic.

## Sharing matches with friends

Played a match together? Each of you records your own chests exactly, but only part of your teammates' (and a slow PC may record very little). Match sharing combines the recordings so everyone's number comes from the best one. It is off by default.

- **By file:** the **Party** page (left menu) → *Add partner replay…* and pick your friend's replay of the same match.
- **Over the internet, no files:** both of you install the free [Tailscale](https://tailscale.com/) app and sign in. One of you turns on *Let friends connect to me* and tells the other the Tailscale address and the room password; the other enters them under *Connect to a friend*. Matches then sync about once a minute.

Only the match summary (nicknames, player ids and chest counts) is exchanged, never a replay file, and only for matches you played together. Friends' numbers are kept beside your ledger and never count towards your own totals. Connections need the room password and go through Tailscale's encrypted network.
