//! Listening statistics: every song heard is written down, one line per listen, and the
//! Stats page (and the Wrapped card) are worked out from those lines when asked: the top
//! songs and artists, minutes listened, the favourite hours and days, streaks and new
//! discoveries, for this week, this month, this year or all time.
//!
//! On Windows, the log and hidden list live in `%APPDATA%\NabyTune\listens.jsonl` and
//! `stats_hidden.json` so ytm-rs and NabyTune share one append-only history seamlessly:
//! play in either app and both see the same numbers, streaks, top lists and hidden items.
//! On other platforms (or isolated `YTM_DATA_DIR` test profiles), they live in ytm-rs's
//! own data folder.
//!
//! The log is append-only JSON Lines: nothing is rewritten (unless the listener clears
//! part of it), so a crash can at worst leave half a last line, which loading skips.
//! Years of listening are tens of thousands of short lines; they load and summarise in
//! milliseconds.

use std::collections::{HashMap, HashSet};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde_json::{Value, json};

/// How many songs and artists the top lists hold.
pub const TOP: usize = 10;
/// How many new artists are shown by name.
pub const DISCOVERIES: usize = 5;

/// One time a song was heard.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Listen {
    pub video_id: String,
    pub title: String,
    pub artist: String,
    pub album: String,
    pub thumbnail: Option<String>,
    /// When it started, in Unix seconds.
    pub at: u64,
    /// How long was actually heard, in seconds (pauses and skipped-over parts left out).
    pub seconds: u32,
    /// The song's length in seconds, 0 when unknown.
    pub length: u32,
    /// Optional `(artist_name, browse_id)` pairs so history cards link straight to artist pages.
    pub artist_ids: Vec<(String, String)>,
}

/// A file's lightweight fingerprint (modified time + length) for detecting changes
/// written by another app (NabyTune) sharing the same files.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FileStamp {
    pub exists: bool,
    pub modified: Option<SystemTime>,
    pub len: u64,
}

impl FileStamp {
    pub fn of(path: &Path) -> Self {
        match std::fs::metadata(path) {
            Ok(m) => Self { exists: true, modified: m.modified().ok(), len: m.len() },
            Err(_) => Self::default(),
        }
    }
}

/// The folder holding `listens.jsonl` and `stats_hidden.json`.
///
/// On Windows (outside an isolated `YTM_DATA_DIR` profile), this is `%APPDATA%\NabyTune`
/// so ytm-rs and NabyTune read and write the exact same history.
pub fn stats_dir(data_dir: &Path) -> (PathBuf, bool) {
    #[cfg(windows)]
    if std::env::var_os("YTM_DATA_DIR").is_none()
        && let Some(appdata) = std::env::var_os("APPDATA")
    {
        return (PathBuf::from(appdata).join("NabyTune"), true);
    }
    (data_dir.to_path_buf(), false)
}

/// Where the log is kept (`listens.jsonl`).
pub fn log_path(data_dir: &Path) -> PathBuf {
    stats_dir(data_dir).0.join("listens.jsonl")
}

/// Where the hidden songs and artists are kept (`stats_hidden.json`), beside the log.
pub fn hidden_path(data_dir: &Path) -> PathBuf {
    stats_dir(data_dir).0.join("stats_hidden.json")
}

/// Whether a listen counts as a play: 30 seconds heard, or half the song (for short ones).
/// Shorter listens still add their minutes.
pub fn counts(listen: &Listen) -> bool {
    listen.seconds >= 30 || (listen.length > 0 && listen.seconds >= listen.length / 2)
}

fn to_json(l: &Listen) -> Value {
    let mut v = json!({
        "id": l.video_id,
        "title": l.title,
        "artist": l.artist,
        "album": l.album,
        "at": l.at,
        "sec": l.seconds,
        "len": l.length,
    });
    if let Some(t) = &l.thumbnail {
        v["thumb"] = Value::String(t.clone());
    }
    if !l.artist_ids.is_empty() {
        v["artist_ids"] = Value::Array(l.artist_ids.iter().map(|(n, id)| json!([n, id])).collect());
    }
    v
}

fn from_json(v: &Value) -> Option<Listen> {
    let text = |k: &str| v[k].as_str().unwrap_or_default().to_string();
    let video_id = v["id"].as_str()?.to_string();
    let artist_ids = v["artist_ids"]
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|item| {
                    let pair = item.as_array()?;
                    Some((pair.first()?.as_str()?.to_string(), pair.get(1)?.as_str()?.to_string()))
                })
                .collect()
        })
        .unwrap_or_default();
    Some(Listen {
        video_id,
        title: text("title"),
        artist: text("artist"),
        album: text("album"),
        thumbnail: v["thumb"].as_str().map(str::to_string),
        at: v["at"].as_u64()?,
        seconds: v["sec"].as_u64().unwrap_or(0).min(u32::MAX as u64) as u32,
        length: v["len"].as_u64().unwrap_or(0).min(u32::MAX as u64) as u32,
        artist_ids,
    })
}

/// Adds one listen to the end of the log, making its folder if need be. If the last line
/// was left unfinished (a crash mid-write), the new one starts on a line of its own.
pub fn record(path: &Path, listen: &Listen) -> anyhow::Result<()> {
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let mut file = std::fs::OpenOptions::new().read(true).append(true).create(true).open(path)?;
    let mut line = String::new();
    if file.metadata()?.len() > 0 {
        let mut last = [0u8];
        file.seek(SeekFrom::End(-1))?;
        file.read_exact(&mut last)?;
        if last[0] != b'\n' {
            line.push('\n');
        }
    }
    line.push_str(&to_json(listen).to_string());
    line.push('\n');
    // One write: appended whole, at the end, whatever the position.
    file.write_all(line.as_bytes())?;
    Ok(())
}

/// Every listen in the log, in the order written. Lines that don't read (half-written,
/// edited by hand) are skipped; a missing file is no listens.
pub fn load(path: &Path) -> Vec<Listen> {
    let Ok(bytes) = std::fs::read(path) else { return Vec::new() };
    let text = String::from_utf8_lossy(&bytes);
    text.lines()
        .map(str::trim)
        .filter(|l| l.starts_with('{'))
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .filter_map(|v| from_json(&v))
        .collect()
}

/// Writes `text` to `path` whole: to a file beside it first, then swapped in, so a crash
/// midway leaves the old file as it was, never half of the new one.
fn replace_file(path: &Path, text: &str) -> anyhow::Result<()> {
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(".new");
    let fresh = path.with_file_name(name);
    std::fs::write(&fresh, text)?;
    std::fs::rename(&fresh, path)?;
    Ok(())
}

// ---- Clearing -------------------------------------------------------------------------

/// Deletes the whole log, for good. A log that isn't there is already clear.
pub fn clear_all(path: &Path) -> anyhow::Result<()> {
    match std::fs::remove_file(path) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e.into()),
        _ => Ok(()),
    }
}

/// Takes the listens within `span` out of the log for good and keeps the rest; how many
/// went. This is the one time the log is rewritten: whole, through a file beside it
/// (`replace_file`). Lines that didn't read are dropped with it.
pub fn clear_span(path: &Path, span: Span) -> anyhow::Result<usize> {
    let listens = load(path);
    let before = listens.len();
    let kept: Vec<&Listen> = listens.iter().filter(|l| !span.contains(l.at)).collect();
    let gone = before - kept.len();
    if gone == 0 {
        return Ok(0);
    }
    let mut text = String::new();
    for l in kept {
        text.push_str(&to_json(l).to_string());
        text.push('\n');
    }
    replace_file(path, &text)?;
    Ok(gone)
}

// ---- Hidden songs and artists ---------------------------------------------------------

/// A song's key: its video id, or its title for the odd listen without one.
pub fn song_key(l: &Listen) -> &str {
    if l.video_id.is_empty() { &l.title } else { &l.video_id }
}

/// A song left out of the stats, with what to call it in the list of hidden things.
#[derive(Clone, Debug, PartialEq)]
pub struct HiddenSong {
    /// The song's key (`song_key`).
    pub id: String,
    pub title: String,
    pub artist: String,
}

/// Songs and artists the listener left out of their stats and Wrapped. They're taken out
/// of the listens before anything is counted (`visible`), so they're gone from everything:
/// the top lists, the minutes, the streaks, the discoveries. Hiding an artist takes every
/// song crediting them along, collaborations too. The log itself keeps them: showing one
/// again brings it all back.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Hidden {
    /// Newest last.
    pub songs: Vec<HiddenSong>,
    /// Newest last; matched without regard to case.
    pub artists: Vec<String>,
}

impl Hidden {
    /// The hidden songs and artists in `path`; nothing hidden if it's missing or unreadable.
    pub fn load(path: &Path) -> Hidden {
        let Some(v) = std::fs::read(path).ok().and_then(|b| serde_json::from_slice::<Value>(&b).ok()) else {
            return Hidden::default();
        };
        let text = |v: &Value, k: &str| v[k].as_str().unwrap_or_default().to_string();
        let songs = v["songs"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter(|s| s["id"].as_str().is_some_and(|id| !id.is_empty()))
                    .map(|s| HiddenSong { id: text(s, "id"), title: text(s, "title"), artist: text(s, "artist") })
                    .collect()
            })
            .unwrap_or_default();
        let artists = v["artists"]
            .as_array()
            .map(|a| a.iter().filter_map(Value::as_str).filter(|s| !s.trim().is_empty()).map(str::to_string).collect())
            .unwrap_or_default();
        Hidden { songs, artists }
    }

    pub fn save(&self, path: &Path) -> anyhow::Result<()> {
        let songs: Vec<Value> = self.songs.iter().map(|s| json!({ "id": s.id, "title": s.title, "artist": s.artist })).collect();
        replace_file(path, &serde_json::to_string_pretty(&json!({ "songs": songs, "artists": self.artists }))?)
    }

    /// How many songs and artists are hidden.
    pub fn len(&self) -> usize {
        self.songs.len() + self.artists.len()
    }

    pub fn is_empty(&self) -> bool {
        self.songs.is_empty() && self.artists.is_empty()
    }

    /// Hides a song (by its key); false if it already was.
    pub fn hide_song(&mut self, id: &str, title: &str, artist: &str) -> bool {
        if id.is_empty() || self.hides_song(id) {
            return false;
        }
        self.songs.push(HiddenSong { id: id.to_string(), title: title.to_string(), artist: artist.to_string() });
        true
    }

    /// Hides an artist; false if they already were.
    pub fn hide_artist(&mut self, name: &str) -> bool {
        let name = name.trim();
        if name.is_empty() || self.hides_artist(name) {
            return false;
        }
        self.artists.push(name.to_string());
        true
    }

    /// Shows a song again; false if it wasn't hidden.
    pub fn show_song(&mut self, id: &str) -> bool {
        let before = self.songs.len();
        self.songs.retain(|s| s.id != id);
        self.songs.len() != before
    }

    /// Shows an artist again; false if they weren't hidden.
    pub fn show_artist(&mut self, name: &str) -> bool {
        let before = self.artists.len();
        let name = name.trim().to_lowercase();
        self.artists.retain(|a| a.trim().to_lowercase() != name);
        self.artists.len() != before
    }

    pub fn hides_song(&self, id: &str) -> bool {
        self.songs.iter().any(|s| s.id == id)
    }

    pub fn hides_artist(&self, name: &str) -> bool {
        let name = name.trim().to_lowercase();
        self.artists.iter().any(|a| a.trim().to_lowercase() == name)
    }

    /// Whether a listen is left out: its song is hidden, or an artist it credits is.
    pub fn hides(&self, listen: &Listen) -> bool {
        self.hides_song(song_key(listen)) || (!self.artists.is_empty() && split_artists(&listen.artist).iter().any(|a| self.hides_artist(a)))
    }
}

/// The listens that count: all but those of hidden songs and artists (`Hidden::hides`).
pub fn visible(listens: &[Listen], hidden: &Hidden) -> Vec<Listen> {
    if hidden.is_empty() {
        return listens.to_vec();
    }
    let songs: HashSet<&str> = hidden.songs.iter().map(|s| s.id.as_str()).collect();
    let artists: HashSet<String> = hidden.artists.iter().map(|a| a.trim().to_lowercase()).collect();
    // Each artist line is split once, however many listens share it.
    let mut lines: HashMap<&str, bool> = HashMap::new();
    listens
        .iter()
        .filter(|l| {
            if songs.contains(song_key(l)) {
                return false;
            }
            if artists.is_empty() {
                return true;
            }
            !*lines.entry(l.artist.as_str()).or_insert_with(|| split_artists(&l.artist).iter().any(|a| artists.contains(&a.to_lowercase())))
        })
        .cloned()
        .collect()
}

/// The listener's current offset from UTC, in seconds (east positive: +25200 in Jakarta),
/// daylight saving included. 0 when it can't be read.
#[cfg(windows)]
pub fn local_offset() -> i64 {
    use windows::Win32::System::Registry::{HKEY_LOCAL_MACHINE, RRF_RT_REG_DWORD, RegGetValueW};
    use windows::core::w;
    let mut value = 0u32;
    let mut size = 4u32;
    // SAFETY: a DWORD out-buffer of the size passed; the strings are static.
    let ok = unsafe {
        RegGetValueW(
            HKEY_LOCAL_MACHINE,
            w!(r"SYSTEM\CurrentControlSet\Control\TimeZoneInformation"),
            w!("ActiveTimeBias"),
            RRF_RT_REG_DWORD,
            None,
            Some((&mut value as *mut u32).cast()),
            Some(&mut size),
        )
    };
    // The bias is UTC minus local time, in minutes, stored as a signed number in a DWORD.
    if ok.is_ok() { -(value as i32 as i64) * 60 } else { 0 }
}

#[cfg(unix)]
pub fn local_offset() -> i64 {
    // SAFETY: zeroed `tm` filled by `localtime_r` for the current time.
    unsafe {
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs()) as libc::time_t;
        let mut tm: libc::tm = std::mem::zeroed();
        if libc::localtime_r(&now, &mut tm).is_null() { 0 } else { tm.tm_gmtoff as i64 }
    }
}

#[cfg(not(any(windows, unix)))]
pub fn local_offset() -> i64 {
    0
}

/// The user's Pictures folder (or temp folder if unavailable), for saving Wrapped PNGs.
pub fn pictures_dir() -> PathBuf {
    if let Some(dirs) = directories::UserDirs::new()
        && let Some(p) = dirs.picture_dir()
        && p.is_dir()
    {
        return p.to_path_buf();
    }
    std::env::temp_dir()
}

/// A PNG path in `dir` named after `stem` that isn't taken yet, adding " (2)", " (3)"
/// and so on if it is. Characters Windows doesn't allow in file names become "-".
pub fn unique_png_name(dir: &Path, stem: &str) -> PathBuf {
    let stem: String = stem.chars().map(|c| if c.is_control() || r#"<>:"/\|?*"#.contains(c) { '-' } else { c }).collect();
    let stem = stem.trim().trim_end_matches('.');
    let stem = if stem.is_empty() { "YT Music Wrapped" } else { stem };
    let first = dir.join(format!("{stem}.png"));
    if !first.exists() {
        return first;
    }
    (2..).map(|n| dir.join(format!("{stem} ({n}).png"))).find(|p| !p.exists()).unwrap_or(first)
}

// ---- Calendar -------------------------------------------------------------------------

const DAY: i64 = 86_400;

/// Days since 1970-01-01 of a calendar date (proleptic Gregorian; Howard Hinnant's method).
fn days_from_civil(y: i32, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y as i64 - 1 } else { y as i64 };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let m = m as i64;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// The calendar date of a day number (days since 1970-01-01).
fn civil_from_days(z: i64) -> (i32, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = yoe + era * 400 + if m <= 2 { 1 } else { 0 };
    (y as i32, m, d)
}

/// Local seconds since 1970 of a moment.
fn local_secs(at: u64, offset: i64) -> i64 {
    at as i64 + offset
}

/// The local day number a moment falls on.
fn local_day(at: u64, offset: i64) -> i64 {
    local_secs(at, offset).div_euclid(DAY)
}

/// The moment a local day starts, in Unix seconds (0 for days before 1970).
fn day_start(day: i64, offset: i64) -> u64 {
    (day * DAY - offset).max(0) as u64
}

const MONTHS: [&str; 12] = ["January", "February", "March", "April", "May", "June", "July", "August", "September", "October", "November", "December"];
const WEEKDAYS: [&str; 7] = ["Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday", "Sunday"];

/// A day on the listener's calendar.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Date {
    pub year: i32,
    /// 1 to 12.
    pub month: u32,
    /// 1 to 31.
    pub day: u32,
}

impl Date {
    fn from_day(day: i64) -> Date {
        let (year, month, day) = civil_from_days(day);
        Date { year, month, day }
    }

    fn day_number(&self) -> i64 {
        days_from_civil(self.year, self.month, self.day)
    }

    /// 0 for Monday to 6 for Sunday.
    pub fn weekday(&self) -> usize {
        // 1970-01-01 was a Thursday.
        (self.day_number() + 3).rem_euclid(7) as usize
    }

    /// "Sat 14 Nov 2026".
    pub fn label(&self) -> String {
        format!("{} {} {} {}", &weekday_name(self.weekday())[..3], self.day, &month_name(self.month)[..3], self.year)
    }
}

/// A month's name, 1 for January.
fn month_name(month: u32) -> &'static str {
    MONTHS[(month.clamp(1, 12) - 1) as usize]
}

/// A weekday's name, 0 for Monday.
pub fn weekday_name(weekday: usize) -> &'static str {
    WEEKDAYS[weekday % 7]
}

// ---- Periods --------------------------------------------------------------------------

/// What the Stats page looks back over: the calendar week (from Monday), month or year,
/// in local time, or everything.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Period {
    Week,
    Month,
    Year,
    All,
}

/// A stretch of time, in Unix seconds: from `start`, up to but not including `end`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Span {
    pub start: u64,
    pub end: u64,
}

impl Span {
    pub fn contains(&self, at: u64) -> bool {
        at >= self.start && at < self.end
    }
}

impl Period {
    pub const EVERY: [Period; 4] = [Period::Week, Period::Month, Period::Year, Period::All];

    pub fn tab_name(self) -> &'static str {
        match self {
            Period::Week => "Week",
            Period::Month => "Month",
            Period::Year => "Year",
            Period::All => "All time",
        }
    }

    /// The local day numbers [first, end) of the period `back` periods before the one
    /// holding `now` (0 is the current one). None for All, which has no days of its own.
    fn days(self, back: u32, now: u64, offset: i64) -> Option<(i64, i64)> {
        let today = local_day(now, offset);
        let (y, m, _) = civil_from_days(today);
        let back = back as i64;
        match self {
            Period::Week => {
                let monday = today - (today + 3).rem_euclid(7) - 7 * back;
                Some((monday, monday + 7))
            }
            Period::Month => {
                let index = y as i64 * 12 + (m as i64 - 1) - back;
                let first = |i: i64| days_from_civil(i.div_euclid(12) as i32, (i.rem_euclid(12) + 1) as u32, 1);
                Some((first(index), first(index + 1)))
            }
            Period::Year => {
                let year = (y as i64 - back) as i32;
                Some((days_from_civil(year, 1, 1), days_from_civil(year + 1, 1, 1)))
            }
            Period::All => None,
        }
    }

    /// The period `back` periods before the one holding `now` (0 is the current one), in
    /// Unix seconds. All time is everything, whatever `back` is.
    pub fn span(self, back: u32, now: u64, utc_offset_secs: i64) -> Span {
        match self.days(back, now, utc_offset_secs) {
            Some((first, end)) => Span { start: day_start(first, utc_offset_secs), end: day_start(end, utc_offset_secs) },
            None => Span { start: 0, end: u64::MAX },
        }
    }

    /// The one before `span(back, ..)`, to compare with; None for All time.
    pub fn previous(self, back: u32, now: u64, utc_offset_secs: i64) -> Option<Span> {
        (self != Period::All).then(|| self.span(back.saturating_add(1), now, utc_offset_secs))
    }

    /// What the period is called: "This week", "Last week", "2 – 8 Nov 2026",
    /// "November 2026", "2026", "All time".
    pub fn label(self, back: u32, now: u64, utc_offset_secs: i64) -> String {
        let Some((first, end)) = self.days(back, now, utc_offset_secs) else {
            return "All time".into();
        };
        let a = Date::from_day(first);
        match self {
            Period::Week if back == 0 => "This week".into(),
            Period::Week if back == 1 => "Last week".into(),
            Period::Week => {
                let b = Date::from_day(end - 1);
                let short = |d: &Date| &month_name(d.month)[..3];
                if a.year != b.year {
                    format!("{} {} {} – {} {} {}", a.day, short(&a), a.year, b.day, short(&b), b.year)
                } else if a.month != b.month {
                    format!("{} {} – {} {} {}", a.day, short(&a), b.day, short(&b), b.year)
                } else {
                    format!("{} – {} {} {}", a.day, b.day, short(&b), b.year)
                }
            }
            Period::Month => format!("{} {}", month_name(a.month), a.year),
            Period::Year => a.year.to_string(),
            Period::All => "All time".into(),
        }
    }
}

// ---- Artists --------------------------------------------------------------------------

/// Names with a "&", "," or "+" in them that are one act, not a collaboration; matched
/// without regard to case and never split.
const ONE_ACT: &[&str] = &[
    "earth, wind & fire",
    "crosby, stills, nash & young",
    "crosby, stills & nash",
    "emerson, lake & palmer",
    "tyler, the creator",
    "peter, bjorn and john",
    "simon & garfunkel",
    "hall & oates",
    "daryl hall & john oates",
    "mumford & sons",
    "brooks & dunn",
    "big & rich",
    "dan + shay",
    "florence + the machine",
    "kool & the gang",
    "sly & the family stone",
    "kc & the sunshine band",
    "echo & the bunnymen",
    "hootie & the blowfish",
    "bob marley & the wailers",
    "huey lewis & the news",
    "tom petty & the heartbreakers",
    "prince & the revolution",
    "marina & the diamonds",
    "iron & wine",
    "chase & status",
    "above & beyond",
    "years & years",
    "of mice & men",
    "angus & julia stone",
    "she & him",
    "matt & kim",
    "sam & dave",
    "ashford & simpson",
    "black eyed peas",
    "x japan",
    "malcolm x",
];

/// Where one credited artist ends and the next begins, lower-cased.
const SEPARATORS: &[&str] = &[
    " featuring ", " feat. ", " feat ", " ft. ", " ft ", " (feat. ", " (ft. ", ", ", " & ", " x ", " × ", " vs. ", " vs ", " / ", "、",
];

/// The artists a song credits, from its artist line: "A & B", "A, B & C", "A feat. B",
/// "A x B", "A vs. B" and "A / B" credit each of A, B and C. Only the part before any " • "
/// is read (some lines carry "Artist • 1.2M views"), and YouTube's " - Topic" is dropped.
pub fn split_artists(line: &str) -> Vec<String> {
    let line = line.split(" • ").next().unwrap_or_default().trim();
    let line = line.strip_suffix(" - Topic").unwrap_or(line);
    // ASCII lower-casing keeps every byte where it was, so positions carry over.
    let lower = line.to_ascii_lowercase();
    let mut kept: Vec<(usize, usize)> = Vec::new();
    for name in ONE_ACT {
        let mut from = 0;
        while let Some(i) = lower[from..].find(name) {
            let start = from + i;
            let end = start + name.len();
            // Whole names only: "malcolm x" mustn't match inside "malcolm xavier".
            let before = lower[..start].chars().next_back().map_or(true, |c| !c.is_alphanumeric());
            let after = lower[end..].chars().next().map_or(true, |c| !c.is_alphanumeric());
            if before && after {
                kept.push((start, end));
            }
            from = end;
        }
    }
    let mut out: Vec<String> = Vec::new();
    let mut push = |piece: &str| {
        let piece = piece.trim().trim_matches(|c| c == '(' || c == ')').trim();
        if piece.chars().any(char::is_alphanumeric) && !out.iter().any(|o| o.eq_ignore_ascii_case(piece)) {
            out.push(piece.to_string());
        }
    };
    let bytes = lower.as_bytes();
    let (mut start, mut i) = (0, 0);
    while i < bytes.len() {
        let inside = kept.iter().any(|&(a, b)| i >= a && i < b);
        let sep = if inside { None } else { SEPARATORS.iter().find(|s| bytes[i..].starts_with(s.as_bytes())) };
        match sep {
            Some(s) => {
                push(line.get(start..i).unwrap_or_default());
                i += s.len();
                start = i;
            }
            None => i += 1,
        }
    }
    push(line.get(start..).unwrap_or_default());
    out
}

// ---- Summaries ------------------------------------------------------------------------

/// A song in a top list.
#[derive(Clone, Debug, PartialEq)]
pub struct SongStat {
    pub video_id: String,
    pub title: String,
    pub artist: String,
    pub album: String,
    pub thumbnail: Option<String>,
    pub plays: u32,
    /// Every second heard, plays or not.
    pub seconds: u64,
    pub minutes: u32,
}

impl SongStat {
    /// The song's key, as `song_key` gives it for its listens (what hiding it goes by).
    pub fn key(&self) -> &str {
        if self.video_id.is_empty() { &self.title } else { &self.video_id }
    }
}

/// An artist in a top list.
#[derive(Clone, Debug, PartialEq)]
pub struct ArtistStat {
    pub name: String,
    pub plays: u32,
    pub seconds: u64,
    pub minutes: u32,
    /// The cover of their most-played song in the period.
    pub thumbnail: Option<String>,
}

/// The day most was heard.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BusiestDay {
    pub date: Date,
    pub minutes: u32,
}

/// Everything the Stats page and the Wrapped card show for one period.
#[derive(Clone, Debug, PartialEq)]
pub struct Summary {
    pub period: Period,
    /// "This week", "November 2026", "2026", "All time".
    pub label: String,
    pub span: Span,
    /// Every second heard in the period, plays or not.
    pub seconds: u64,
    pub minutes: u32,
    /// Listens that count as plays (`counts`).
    pub plays: u32,
    /// Songs and artists with at least one play.
    pub songs: u32,
    pub artists: u32,
    /// Days with at least one play.
    pub days: u32,
    /// Most plays first; ties go to more minutes, then to the most recently heard.
    pub top_songs: Vec<SongStat>,
    pub top_artists: Vec<ArtistStat>,
    /// Minutes heard by the hour a listen started, 0 = midnight to 1 am, local time.
    pub by_hour: [f32; 24],
    /// Minutes heard by weekday, Monday first.
    pub by_weekday: [f32; 7],
    pub busiest_day: Option<BusiestDay>,
    /// Days in a row with a play, up to today (still alive if today has none yet but
    /// yesterday had); for a past period, up to its last day. Counts days before the period too.
    pub current_streak: u32,
    /// The longest run of days in a row with a play, within the period.
    pub longest_streak: u32,
    /// Artists whose first play ever is in this period. (For All time: every artist.)
    pub new_artists: u32,
    /// The most-played of them, up to `DISCOVERIES`.
    pub discoveries: Vec<ArtistStat>,
    /// Minutes in the period before, of the same kind (None for All time).
    pub previous_minutes: Option<u32>,
    /// The change in minutes against the period before, in percent: +25.0 is a quarter
    /// more. None for All time, or when nothing was heard in the period before.
    pub change: Option<f32>,
}

impl Summary {
    /// The hour (0 to 23) with the most minutes, if anything was heard.
    pub fn favourite_hour(&self) -> Option<usize> {
        favourite(&self.by_hour)
    }

    /// The weekday (0 = Monday) with the most minutes, if anything was heard.
    pub fn favourite_weekday(&self) -> Option<usize> {
        favourite(&self.by_weekday)
    }
}

fn favourite(values: &[f32]) -> Option<usize> {
    let mut best: Option<usize> = None;
    for (i, &v) in values.iter().enumerate() {
        if v > 0.0 && best.map_or(true, |b| v > values[b]) {
            best = Some(i);
        }
    }
    best
}

fn minutes(seconds: u64) -> u32 {
    ((seconds + 30) / 60).min(u32::MAX as u64) as u32
}

/// The summary of the current period of a kind: this week, this month, this year, or all time.
#[allow(dead_code)]
pub fn summary(listens: &[Listen], period: Period, now: u64, utc_offset_secs: i64) -> Summary {
    summary_back(listens, period, 0, now, utc_offset_secs)
}

/// The summary of the period `back` periods before the current one (0 is the current one;
/// `Period::Month` with 1 is last month), for paging back through history.
pub fn summary_back(listens: &[Listen], period: Period, back: u32, now: u64, utc_offset_secs: i64) -> Summary {
    let offset = utc_offset_secs;
    let span = period.span(back, now, offset);
    let previous = period.previous(back, now, offset);

    let key = song_key;

    /// A song over all history: its newest listen (for the latest title and cover) and its
    /// first play (for discoveries).
    struct Ever {
        newest: usize,
        first_play: Option<u64>,
    }
    /// A song within the period.
    #[derive(Default)]
    struct InPeriod {
        plays: u32,
        seconds: u64,
        last: u64,
    }

    let mut ever: HashMap<&str, Ever> = HashMap::new();
    let mut in_period: HashMap<&str, InPeriod> = HashMap::new();
    let mut play_days: HashSet<i64> = HashSet::new();
    let mut day_seconds: HashMap<i64, u64> = HashMap::new();
    let mut by_hour = [0u64; 24];
    let mut by_weekday = [0u64; 7];
    let (mut seconds, mut plays, mut previous_seconds) = (0u64, 0u32, 0u64);

    for (i, l) in listens.iter().enumerate() {
        let k = key(l);
        let played = counts(l);
        let e = ever.entry(k).or_insert(Ever { newest: i, first_play: None });
        if l.at >= listens[e.newest].at {
            e.newest = i;
        }
        if played {
            e.first_play = Some(e.first_play.map_or(l.at, |f| f.min(l.at)));
            play_days.insert(local_day(l.at, offset));
        }
        if previous.is_some_and(|p| p.contains(l.at)) {
            previous_seconds += l.seconds as u64;
        }
        if !span.contains(l.at) {
            continue;
        }
        let s = l.seconds as u64;
        seconds += s;
        let local = local_secs(l.at, offset);
        let day = local.div_euclid(DAY);
        by_hour[(local.rem_euclid(DAY) / 3600) as usize] += s;
        by_weekday[(day + 3).rem_euclid(7) as usize] += s;
        *day_seconds.entry(day).or_default() += s;
        let p = in_period.entry(k).or_default();
        p.seconds += s;
        p.last = p.last.max(l.at);
        if played {
            p.plays += 1;
            plays += 1;
        }
    }

    // Songs.
    let song_of = |k: &str, p: &InPeriod| {
        let l = &listens[ever[k].newest];
        SongStat {
            video_id: l.video_id.clone(),
            title: l.title.clone(),
            artist: l.artist.clone(),
            album: l.album.clone(),
            thumbnail: l.thumbnail.clone(),
            plays: p.plays,
            seconds: p.seconds,
            minutes: minutes(p.seconds),
        }
    };
    let mut ranked: Vec<(&str, &InPeriod)> = in_period.iter().filter(|(_, p)| p.plays > 0).map(|(k, p)| (*k, p)).collect();
    ranked.sort_by(|a, b| (b.1.plays, b.1.seconds, b.1.last).cmp(&(a.1.plays, a.1.seconds, a.1.last)).then_with(|| a.0.cmp(b.0)));
    let songs = ranked.len() as u32;
    let top_songs: Vec<SongStat> = ranked.iter().take(TOP).map(|(k, p)| song_of(k, p)).collect();

    // Artists, credited through each song's latest artist line.
    struct Artist {
        name: String,
        plays: u32,
        seconds: u64,
        last: u64,
        /// (plays, seconds) of their best song so far, and its cover.
        best: (u32, u64),
        thumbnail: Option<String>,
    }
    let mut artists: HashMap<String, Artist> = HashMap::new();
    for (k, p) in &in_period {
        let l = &listens[ever[k].newest];
        for name in split_artists(&l.artist) {
            let a = artists.entry(name.to_lowercase()).or_insert_with(|| Artist {
                name: name.clone(),
                plays: 0,
                seconds: 0,
                last: 0,
                best: (0, 0),
                thumbnail: None,
            });
            a.plays += p.plays;
            a.seconds += p.seconds;
            if p.last > a.last {
                // The spelling heard most recently.
                a.last = p.last;
                a.name = name;
            }
            if (p.plays, p.seconds) > a.best || a.thumbnail.is_none() && l.thumbnail.is_some() && (p.plays, p.seconds) == a.best {
                a.best = (p.plays, p.seconds);
                a.thumbnail = l.thumbnail.clone();
            }
        }
    }
    let stat = |a: &Artist| ArtistStat {
        name: a.name.clone(),
        plays: a.plays,
        seconds: a.seconds,
        minutes: minutes(a.seconds),
        thumbnail: a.thumbnail.clone(),
    };
    let rank = |list: &mut Vec<(&String, &Artist)>| {
        list.sort_by(|a, b| (b.1.plays, b.1.seconds, b.1.last).cmp(&(a.1.plays, a.1.seconds, a.1.last)).then_with(|| a.0.cmp(b.0)));
    };
    let mut played_artists: Vec<(&String, &Artist)> = artists.iter().filter(|(_, a)| a.plays > 0).collect();
    rank(&mut played_artists);
    let top_artists: Vec<ArtistStat> = played_artists.iter().take(TOP).map(|(_, a)| stat(a)).collect();

    // Discoveries: when each artist was first played, over all history.
    let mut first_play: HashMap<String, u64> = HashMap::new();
    for e in ever.values() {
        let Some(at) = e.first_play else { continue };
        for name in split_artists(&listens[e.newest].artist) {
            let f = first_play.entry(name.to_lowercase()).or_insert(at);
            *f = (*f).min(at);
        }
    }
    let mut new: Vec<(&String, &Artist)> =
        played_artists.iter().copied().filter(|(k, _)| first_play.get(*k).is_some_and(|&at| span.contains(at))).collect();
    rank(&mut new);

    // Days and streaks.
    let busiest_day = day_seconds
        .iter()
        .max_by(|a, b| a.1.cmp(b.1).then_with(|| b.0.cmp(a.0)))
        .map(|(&day, &s)| BusiestDay { date: Date::from_day(day), minutes: minutes(s) });
    let mut days_played: Vec<i64> = play_days.iter().copied().filter(|&d| span.contains(day_start(d, offset))).collect();
    days_played.sort_unstable();
    let mut longest_streak = 0u32;
    let mut run = 0u32;
    for (i, d) in days_played.iter().enumerate() {
        run = if i > 0 && days_played[i - 1] + 1 == *d { run + 1 } else { 1 };
        longest_streak = longest_streak.max(run);
    }
    let today = local_day(now, offset);
    let mut day = match period.days(back, now, offset) {
        Some((_, end)) if back > 0 => end - 1,
        _ => today,
    };
    if back == 0 && !play_days.contains(&day) {
        day -= 1;
    }
    let mut current_streak = 0u32;
    while play_days.contains(&day) {
        current_streak += 1;
        day -= 1;
    }

    let change = previous_seconds_change(seconds, previous.map(|_| previous_seconds));
    Summary {
        period,
        label: period.label(back, now, offset),
        span,
        seconds,
        minutes: minutes(seconds),
        plays,
        songs,
        artists: played_artists.len() as u32,
        days: days_played.len() as u32,
        top_songs,
        top_artists,
        by_hour: by_hour.map(|s| s as f32 / 60.0),
        by_weekday: by_weekday.map(|s| s as f32 / 60.0),
        busiest_day,
        current_streak,
        longest_streak,
        new_artists: new.len() as u32,
        discoveries: new.iter().take(DISCOVERIES).map(|(_, a)| stat(a)).collect(),
        previous_minutes: previous.map(|_| minutes(previous_seconds)),
        change,
    }
}

/// The change from `previous` to `now`, in percent; None when there's nothing before.
fn previous_seconds_change(now: u64, previous: Option<u64>) -> Option<f32> {
    let previous = previous.filter(|&p| p > 0)?;
    Some(((now as f64 - previous as f64) / previous as f64 * 100.0) as f32)
}

#[cfg(test)]
mod tests {
    use super::*;

    const WIB: i64 = 7 * 3600;

    fn ts(y: i32, m: u32, d: u32, h: i64, min: i64, offset: i64) -> u64 {
        (days_from_civil(y, m, d) * DAY + h * 3600 + min * 60 - offset) as u64
    }

    fn listen(id: &str, artist: &str, at: u64, seconds: u32) -> Listen {
        Listen {
            video_id: id.into(),
            title: format!("Song {id}"),
            artist: artist.into(),
            album: String::new(),
            thumbnail: Some(format!("https://img/{id}")),
            at,
            seconds,
            length: 200,
            artist_ids: Vec::new(),
        }
    }

    #[test]
    fn calendar_round_trips() {
        assert_eq!(days_from_civil(1970, 1, 1), 0);
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(days_from_civil(2000, 3, 1), 11_017);
        for d in -800_000..800_000i64 {
            if d % 997 == 0 {
                let (y, m, dd) = civil_from_days(d);
                assert_eq!(days_from_civil(y, m, dd), d);
            }
        }
        assert_eq!(Date { year: 2026, month: 11, day: 15 }.weekday(), 6);
        assert_eq!(Date { year: 2024, month: 1, day: 1 }.weekday(), 0);
        assert_eq!(civil_from_days(days_from_civil(2028, 2, 29) + 1), (2028, 3, 1));
        assert_eq!(Date { year: 2026, month: 11, day: 14 }.label(), "Sat 14 Nov 2026");
    }

    #[test]
    fn week_starts_on_monday_in_local_time() {
        let now = ts(2026, 11, 15, 12, 0, 0);
        let s = Period::Week.span(0, now, 0);
        assert_eq!(s.start, ts(2026, 11, 9, 0, 0, 0));
        assert_eq!(s.end, ts(2026, 11, 16, 0, 0, 0));
        let monday = ts(2026, 11, 16, 0, 0, 0);
        assert_eq!(Period::Week.span(0, monday, 0).start, monday);
        let evening = ts(2026, 11, 15, 18, 0, 0);
        assert_eq!(Period::Week.span(0, evening, WIB).start, ts(2026, 11, 16, 0, 0, WIB));
        assert_eq!(Period::Week.span(0, evening, 0).start, ts(2026, 11, 9, 0, 0, 0));
        let listens = vec![listen("a", "A", ts(2026, 11, 8, 18, 0, 0), 120)];
        assert_eq!(summary(&listens, Period::Week, now, WIB).plays, 1);
        assert_eq!(summary(&listens, Period::Week, now, 0).plays, 0);
        assert_eq!(Period::Week.label(0, now, 0), "This week");
        assert_eq!(Period::Week.label(1, now, 0), "Last week");
        assert_eq!(Period::Week.label(2, now, 0), "26 Oct – 1 Nov 2026");
        assert_eq!(Period::Week.label(3, now, 0), "19 – 25 Oct 2026");
    }

    #[test]
    fn what_counts_as_a_play() {
        let mut l = listen("a", "A", 0, 29);
        assert!(!counts(&l));
        l.seconds = 30;
        assert!(counts(&l));
        l.length = 40;
        l.seconds = 20;
        assert!(counts(&l));
        l.seconds = 19;
        assert!(!counts(&l));
        l.length = 0;
        l.seconds = 0;
        assert!(!counts(&l));
    }

    #[test]
    fn top_lists_streaks_and_discoveries() {
        let now = ts(2026, 11, 15, 23, 0, WIB);
        let at = |d: u32, h: i64| ts(2026, 11, d, h, 0, WIB);
        let mut listens = Vec::new();
        for d in [10, 11, 12] {
            listens.push(listen("c", "Tulus", at(d, 8), 100));
        }
        listens.push(listen("a", "Hindia & Tulus", at(13, 22), 300));
        listens.push(listen("a", "Hindia & Tulus", at(13, 23), 300));
        listens.push(listen("b", "Nadin Amizah", at(14, 22), 60));
        listens.push(listen("b", "Nadin Amizah", at(14, 22), 60));
        listens.push(listen("d", "Nadin Amizah", at(15, 7), 60));
        let s = summary(&listens, Period::Week, now, WIB);
        let ids: Vec<&str> = s.top_songs.iter().map(|x| x.video_id.as_str()).collect();
        assert_eq!(ids, ["c", "a", "b", "d"]);
        let names: Vec<(&str, u32)> = s.top_artists.iter().map(|a| (a.name.as_str(), a.plays)).collect();
        assert_eq!(names, [("Tulus", 5), ("Nadin Amizah", 3), ("Hindia", 2)]);
        assert_eq!(s.favourite_hour(), Some(22));
        assert_eq!(s.favourite_weekday(), Some(4));
        assert_eq!(s.current_streak, 6);
        assert_eq!(s.longest_streak, 6);
        assert_eq!(s.new_artists, 3);
    }

    #[test]
    fn log_and_hidden_round_trip() {
        let dir = std::env::temp_dir().join(format!("ytm-rs-stats-test-{}", std::process::id()));
        let path = dir.join("listens.jsonl");
        let hpath = dir.join("stats_hidden.json");
        let _ = std::fs::remove_dir_all(&dir);
        let a = listen("a", "Tulus", 1_700_000_000, 123);
        let b = listen("b", "Earth, Wind & Fire", 1_700_000_500, 45);
        record(&path, &a).unwrap();
        std::fs::OpenOptions::new().append(true).open(&path).unwrap().write_all(b"{\"id\":\"half").unwrap();
        record(&path, &b).unwrap();
        assert_eq!(load(&path), vec![a.clone(), b.clone()]);

        let mut hidden = Hidden::default();
        assert!(hidden.hide_song("a", "Song a", "Tulus"));
        assert!(hidden.hide_artist("Earth, Wind & Fire"));
        hidden.save(&hpath).unwrap();
        let loaded = Hidden::load(&hpath);
        assert_eq!(loaded, hidden);
        assert!(visible(&[a, b], &loaded).is_empty());

        clear_all(&path).unwrap();
        assert!(load(&path).is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn local_offset_is_sane() {
        let o = local_offset();
        assert!(o % 900 == 0 && (-12 * 3600..=14 * 3600).contains(&o), "{o}");
    }
}
