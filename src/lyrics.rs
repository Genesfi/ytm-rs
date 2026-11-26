//! Time-synced lyrics from every source the Better Lyrics extension uses, parsed the way
//! its parsers (`@braccato/parsers`) parse them, so any of them can be shown and the
//! listener can switch between them.
//!
//! - The Better Lyrics API streams its providers as server-sent events: Better Lyrics
//!   (Apple-style TTML), BiniLyrics (TTML), Musixmatch (word-by-word and line LRC), LRCLib
//!   (LRC and plain), Legato (KuGou LRC) and Portato (QQ QRC).
//! - Unison, the community lyrics service, answers a plain GET.
//! - YouTube Music's own Lyrics tab, plain text (see `ytm::lyrics`).
//! - LRCLib directly, by the song's bare title, when the stream brought nothing from it.
//!
//! The API needs a JWT, obtained by exchanging a Cloudflare Turnstile token (see
//! turnstile.rs). JWTs last 24 h and are cached on disk.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;

const API: &str = "https://lyrics.api.dacubeking.com/";
const UNISON_API: &str = "https://unison.betterlyrics.org/lyrics";
const LRCLIB_SEARCH: &str = "https://lrclib.net/api/search";

/// One source of lyrics, in the extension's default priority order (its
/// `PROVIDER_CONFIGS`, with YouTube Music's own plain lyrics last instead of its two
/// YouTube sources).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub enum Provider {
    BLyricsRich,
    UnisonRich,
    BinimumRich,
    UnisonWord,
    PortatoRich,
    MusixmatchRich,
    NeteaseRich,
    BLyricsSynced,
    UnisonSynced,
    BinimumSynced,
    NeteaseSynced,
    LrclibSynced,
    PortatoSynced,
    LegatoSynced,
    MusixmatchSynced,
    UnisonPlain,
    LrclibPlain,
    YoutubePlain,
}

/// How finely a provider's lyrics are timed.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SyncKind {
    Syllable,
    Word,
    Line,
    Unsynced,
}

impl Provider {
    pub const ALL: [Provider; 18] = [
        Provider::BLyricsRich,
        Provider::UnisonRich,
        Provider::BinimumRich,
        Provider::UnisonWord,
        Provider::PortatoRich,
        Provider::MusixmatchRich,
        Provider::NeteaseRich,
        Provider::BLyricsSynced,
        Provider::UnisonSynced,
        Provider::BinimumSynced,
        Provider::NeteaseSynced,
        Provider::LrclibSynced,
        Provider::PortatoSynced,
        Provider::LegatoSynced,
        Provider::MusixmatchSynced,
        Provider::UnisonPlain,
        Provider::LrclibPlain,
        Provider::YoutubePlain,
    ];

    /// The extension's key for it (also how the order is stored).
    pub fn key(self) -> &'static str {
        match self {
            Provider::BLyricsRich => "bLyrics-richsynced",
            Provider::UnisonRich => "unison-richsynced",
            Provider::BinimumRich => "binimum-richsynced",
            Provider::UnisonWord => "unison-wordsynced",
            Provider::PortatoRich => "portato-richsynced",
            Provider::MusixmatchRich => "musixmatch-richsync",
            Provider::NeteaseRich => "netease-richsync",
            Provider::BLyricsSynced => "bLyrics-synced",
            Provider::UnisonSynced => "unison-synced",
            Provider::BinimumSynced => "binimum-synced",
            Provider::NeteaseSynced => "netease-synced",
            Provider::LrclibSynced => "lrclib-synced",
            Provider::PortatoSynced => "portato-synced",
            Provider::LegatoSynced => "legato-synced",
            Provider::MusixmatchSynced => "musixmatch-synced",
            Provider::UnisonPlain => "unison-plain",
            Provider::LrclibPlain => "lrclib-plain",
            Provider::YoutubePlain => "yt-lyrics",
        }
    }

    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|p| p.key() == key)
    }

    pub fn name(self) -> &'static str {
        match self {
            Provider::BLyricsRich | Provider::BLyricsSynced => "Better Lyrics",
            Provider::UnisonRich | Provider::UnisonWord | Provider::UnisonSynced | Provider::UnisonPlain => "Unison",
            Provider::BinimumRich | Provider::BinimumSynced => "BiniLyrics",
            Provider::PortatoRich | Provider::PortatoSynced => "QQ Music",
            Provider::MusixmatchRich | Provider::MusixmatchSynced => "Musixmatch",
            Provider::NeteaseRich | Provider::NeteaseSynced => "NetEase",
            Provider::LrclibSynced | Provider::LrclibPlain => "LRCLib",
            Provider::LegatoSynced => "KuGou",
            Provider::YoutubePlain => "YouTube Music",
        }
    }

    pub fn is_unison(self) -> bool {
        matches!(self, Provider::UnisonRich | Provider::UnisonWord | Provider::UnisonSynced | Provider::UnisonPlain)
    }

    pub fn sync(self) -> SyncKind {
        match self {
            Provider::BLyricsRich | Provider::UnisonRich | Provider::BinimumRich => SyncKind::Syllable,
            Provider::UnisonWord | Provider::PortatoRich | Provider::MusixmatchRich | Provider::NeteaseRich => SyncKind::Word,
            Provider::BLyricsSynced
            | Provider::UnisonSynced
            | Provider::BinimumSynced
            | Provider::NeteaseSynced
            | Provider::LrclibSynced
            | Provider::PortatoSynced
            | Provider::LegatoSynced
            | Provider::MusixmatchSynced => SyncKind::Line,
            Provider::UnisonPlain | Provider::LrclibPlain | Provider::YoutubePlain => SyncKind::Unsynced,
        }
    }
}

/// How a set of lines is timed, decided from the lines themselves (`deriveSyncType`).
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug, Serialize, Deserialize)]
pub enum Sync {
    None,
    Line,
    Word,
}

/// A timed piece of a line: a syllable, a word or several words.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Part {
    pub start: i32,
    pub dur: i32,
    pub text: String,
    /// Background vocals, shown on their own row under the line.
    pub bg: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Line {
    pub start: i32,
    pub dur: i32,
    pub text: String,
    /// Empty for line-synced and plain lyrics.
    pub parts: Vec<Part>,
    /// Singer: `v1`, `v2`, … or `v1000` for everyone.
    pub agent: Option<String>,
    /// A gap of more than five seconds nobody sings over.
    pub instrumental: bool,
    /// Translations shipped with the lyrics, by language.
    pub translations: Vec<(String, String)>,
    /// Romanization shipped with the lyrics, and its timing when it has some.
    pub romanization: Option<String>,
    pub timed_roman: Vec<Part>,
}

impl Line {
    pub fn end(&self) -> i32 {
        self.start + self.dur
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Lyrics {
    pub lines: Vec<Line>,
    pub sync: Sync,
    pub provider: Provider,
    /// Language the source says the lyrics are in.
    pub language: Option<String>,
    pub songwriters: Vec<String>,
    /// Unison's credit: its song page and who submitted the lyrics.
    #[serde(default)]
    pub unison: Option<UnisonCredit>,
}

/// Unison asks every app to credit it and the lyrics' submitter, both linked.
#[derive(Clone, Debug, PartialEq, Hash, Serialize, Deserialize)]
pub struct UnisonCredit {
    pub video_id: String,
    /// `submitter.displayName` and `submitter.keyId`.
    pub submitter: Option<(String, String)>,
}

impl UnisonCredit {
    pub fn song_url(&self) -> String {
        format!("https://unison.betterlyrics.org/song/{}", self.video_id)
    }

    pub fn submitter_url(key_id: &str) -> String {
        format!("https://unison.betterlyrics.org/curator/{key_id}")
    }
}

impl Lyrics {
    fn new(provider: Provider, lines: Vec<Line>, language: Option<String>, songwriters: Vec<String>) -> Option<Self> {
        if lines.is_empty() {
            return None;
        }
        let sync = derive_sync(&lines);
        Some(Self { lines, sync, provider, language, songwriters, unison: None })
    }
}

/// `deriveSyncType`: richsync if any sung line has a part with a duration.
fn derive_sync(lines: &[Line]) -> Sync {
    if lines.iter().any(|l| !l.instrumental && l.parts.iter().any(|p| p.dur != 0)) {
        Sync::Word
    } else if lines.iter().all(|l| l.start == 0) {
        Sync::None
    } else {
        Sync::Line
    }
}

/// Every source that had lyrics for a song, in default priority order.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct LyricsSet {
    pub sources: Vec<Lyrics>,
}

impl LyricsSet {
    pub fn get(&self, p: Provider) -> Option<&Lyrics> {
        self.sources.iter().find(|l| l.provider == p)
    }

    /// Unison lyrics saved before they carried its credit get one crediting Unison alone,
    /// linked to `video_id`'s page there: who submitted them comes with the next fresh look.
    pub fn credit_unison(&mut self, video_id: &str) {
        for l in self.sources.iter_mut().filter(|l| l.provider.is_unison() && l.unison.is_none()) {
            l.unison = Some(UnisonCredit { video_id: video_id.to_owned(), submitter: None });
        }
    }
}

/// Lyrics worth showing: some line is sung (the extension's `hideInstrumentalOnly`).
pub fn usable(l: &Lyrics) -> bool {
    l.lines.iter().any(|x| !x.instrumental && !x.text.trim().is_empty())
}

/// Sources that have lyrics for a song, in the listener's `order` (switched-off ones
/// left out).
pub fn available(order: &[(Provider, bool)], set: &LyricsSet) -> Vec<Provider> {
    order.iter().filter(|(p, on)| *on && set.get(*p).is_some_and(usable)).map(|(p, _)| *p).collect()
}

/// The source a song's lyrics come from: its pick when that has lyrics, else the first
/// in the listener's order that does (`orderByPin`, then fall through on failure).
pub fn choose(order: &[(Provider, bool)], pins: &HashMap<String, Provider>, video_id: &str, set: &LyricsSet) -> Option<Provider> {
    let available = available(order, set);
    pins.get(video_id).filter(|p| available.contains(p)).copied().or_else(|| available.first().copied())
}

/// Lyrics as LRC, a `[mm:ss.xx]` line each, for players that read timed lyrics from a
/// file's lyrics tag. A break nobody sings over is an empty timed line, so the last
/// line sung doesn't stay up through it. Lyrics without timing are just their words.
/// `shift_ms` is added to every stamp: the song's own timing, as set in the app.
pub fn to_lrc(l: &Lyrics, shift_ms: i32) -> String {
    if l.sync == Sync::None {
        return plain_words(l);
    }
    let stamp = |ms: i32| {
        let cs = (ms + shift_ms).max(0) / 10;
        format!("[{:02}:{:02}.{:02}]", cs / 6000, cs / 100 % 60, cs % 100)
    };
    let mut out = Vec::with_capacity(l.lines.len());
    for (i, line) in l.lines.iter().enumerate() {
        if line.instrumental {
            out.push(stamp(line.start));
            continue;
        }
        let text = line.text.trim();
        if text.is_empty() {
            out.push(stamp(line.start));
            continue;
        }
        out.push(format!("{}{text}", stamp(line.start)));
        // A long quiet after this line that the lyrics don't mark: clear it at its end.
        let next = l.lines.get(i + 1);
        let quiet = next.map_or(line.dur > 0, |n| !n.instrumental && line.dur > 0 && n.start - line.end() > 5000);
        if quiet {
            out.push(stamp(line.end()));
        }
    }
    out.join("\n")
}

/// The words, a sung line each.
pub fn plain_words(l: &Lyrics) -> String {
    l.lines.iter().filter(|l| !l.instrumental).map(|l| l.text.trim()).collect::<Vec<_>>().join("\n").trim().to_owned()
}

// ------------------------------------------------------------------- API client

pub struct Client {
    http: reqwest::Client,
    jwt_file: PathBuf,
    webview_profile: PathBuf,
    jwt: tokio::sync::Mutex<Option<String>>,
    /// When the Turnstile challenge last failed; it isn't retried for a while, so
    /// every new song doesn't start another WebView2.
    challenge_failed: std::sync::Mutex<Option<std::time::Instant>>,
    /// The last few songs' lyrics, for replays and going back.
    recent: std::sync::Mutex<std::collections::VecDeque<(String, LyricsSet)>>,
}

const CHALLENGE_RETRY: Duration = Duration::from_secs(15 * 60);
const RECENT: usize = 8;

pub struct Query<'a> {
    pub video_id: &'a str,
    pub song: &'a str,
    pub artist: &'a str,
    pub album: Option<&'a str>,
    pub duration: Option<u32>,
    /// The title and artist to match by name alone (LRCLib): see `ytm::lyrics_names`.
    pub bare_song: &'a str,
    pub main_artist: &'a str,
}

fn artist_matches(candidate: &str, target: &str) -> bool {
    let t_low = target.trim().to_lowercase();
    if t_low.is_empty() {
        return true;
    }
    let c_low = clean_noise(&strip_brackets(candidate)).to_lowercase();
    if c_low.is_empty() {
        return false;
    }
    c_low.contains(&t_low) || t_low.contains(&c_low)
}

impl Client {
    pub fn new(http: reqwest::Client, data_dir: &Path) -> Self {
        let jwt_file = data_dir.join("lyrics_jwt");
        let cached = std::fs::read_to_string(&jwt_file).ok().filter(|t| jwt_seconds_left(t) > 3600);
        Self {
            http,
            jwt_file,
            webview_profile: data_dir.join("webview-lyrics"),
            jwt: tokio::sync::Mutex::new(cached),
            challenge_failed: Default::default(),
            recent: Default::default(),
        }
    }

    /// Every source's lyrics for a song: the Better Lyrics stream, Unison, NetEase, LRCLib, QQ, KuGou,
    /// and YouTube Music's own lyrics together.
    pub async fn fetch(&self, q: &Query<'_>, youtube: impl Future<Output = anyhow::Result<Option<String>>>) -> anyhow::Result<LyricsSet> {
        if let Some((_, l)) = self.recent.lock().unwrap().iter().find(|(id, _)| id == q.video_id) {
            return Ok(l.clone());
        }
        let (stream, unison, netease, lrclib, qq, kugou, youtube) = tokio::join!(
            self.fetch_stream(q),
            self.fetch_unison(q),
            self.fetch_netease(q),
            self.fetch_lrclib(q),
            self.fetch_qq(q),
            self.fetch_kugou(q),
            youtube,
        );
        let mut sources = Vec::new();
        match stream {
            Ok(s) => sources.extend(s),
            Err(e) => log::warn!("better lyrics failed: {e:#}"),
        }
        match unison {
            Ok(s) => sources.extend(s),
            Err(e) => log::info!("unison: {e:#}"),
        }
        match netease {
            Ok(s) => sources.extend(s),
            Err(e) => log::info!("netease: {e:#}"),
        }
        match lrclib {
            Ok(s) => sources.extend(s),
            Err(e) => log::info!("lrclib: {e:#}"),
        }
        match qq {
            Ok(s) => sources.extend(s),
            Err(e) => log::info!("qq: {e:#}"),
        }
        match kugou {
            Ok(s) => sources.extend(s),
            Err(e) => log::info!("kugou: {e:#}"),
        }
        match youtube {
            Ok(text) => sources.extend(text.and_then(|t| Lyrics::new(Provider::YoutubePlain, parse_plain(&t), None, vec![]))),
            Err(e) => log::info!("youtube lyrics: {e:#}"),
        }
        if sources.is_empty() {
            return Ok(LyricsSet::default());
        }
        sources.sort_by_key(|l| Provider::ALL.iter().position(|p| *p == l.provider));
        let set = LyricsSet { sources };
        let mut recent = self.recent.lock().unwrap();
        if recent.len() == RECENT {
            recent.pop_front();
        }
        recent.push_back((q.video_id.to_owned(), set.clone()));
        Ok(set)
    }

    async fn fetch_stream(&self, q: &Query<'_>) -> anyhow::Result<Vec<Lyrics>> {
        let mut jwt = self.token(false).await?;
        for attempt in 0..2 {
            let mut form = vec![("videoId", q.video_id.to_owned())];
            if !q.song.is_empty() {
                form.push(("song", q.song.to_owned()));
            }
            if !q.artist.is_empty() {
                form.push(("artist", q.artist.to_owned()));
            }
            if let Some(d) = q.duration.filter(|d| *d > 0) {
                form.push(("duration", d.to_string()));
            }
            if let Some(a) = q.album {
                form.push(("album", a.to_owned()));
            }
            form.push(("alwaysFetchMetadata", "false".to_owned()));
            form.push(("token", jwt.clone()));
            let resp = self.http.post(format!("{API}v2/lyrics")).form(&form).timeout(Duration::from_secs(20)).send().await?;
            if resp.status() == reqwest::StatusCode::FORBIDDEN && attempt == 0 {
                jwt = self.token(true).await?;
                continue;
            }
            let body = resp.error_for_status()?.text().await?;
            return Ok(from_stream(&body, q));
        }
        unreachable!()
    }

    async fn fetch_unison(&self, q: &Query<'_>) -> anyhow::Result<Vec<Lyrics>> {
        let title_parts = clean_title_with_artist(q.song, q.artist);
        let mut titles = vec![q.song.to_owned()];
        if title_parts.core != q.song && !title_parts.core.is_empty() {
            titles.push(title_parts.core.clone());
        }
        if let Some(ref alt) = title_parts.alt {
            titles.push(alt.clone());
        }
        titles.dedup();

        for song_title in titles {
            let mut params = vec![("v", q.video_id.to_owned()), ("song", song_title), ("artist", q.artist.to_owned())];
            params.push(("duration", q.duration.unwrap_or(0).to_string()));
            if let Some(a) = q.album {
                params.push(("album", a.to_owned()));
            }
            let Ok(resp) = self.http.get(UNISON_API).query(&params).timeout(Duration::from_secs(8)).send().await else { continue };
            if resp.status() == reqwest::StatusCode::NOT_FOUND {
                continue;
            }
            let Ok(v) = resp.error_for_status()?.json::<Value>().await else { continue };
            let list = from_unison(&v["data"], q.video_id, q.duration.unwrap_or(0) as i32 * 1000);
            if !list.is_empty() {
                return Ok(list);
            }
        }
        Ok(vec![])
    }

    async fn fetch_netease(&self, q: &Query<'_>) -> anyhow::Result<Vec<Lyrics>> {
        let title_parts = clean_title_with_artist(q.song, q.artist);
        let clean_artist = clean_noise(&strip_brackets(q.artist.split(['/', ',', '&']).next().unwrap_or(q.artist)));
        if title_parts.core.is_empty() && title_parts.full.is_empty() {
            return Ok(vec![]);
        }
        let target_ms = q.duration.unwrap_or(0) as i64 * 1000;
        let mut queries: Vec<(String, bool)> = Vec::new();
        if !clean_artist.is_empty() {
            queries.push((format!("{} {clean_artist}", title_parts.core), true));
            if let Some(ref alt) = title_parts.alt {
                queries.push((format!("{alt} {clean_artist}"), true));
            }
            if q.song.trim() != title_parts.core {
                queries.push((format!("{} {clean_artist}", title_parts.full), true));
            }
        }
        queries.push((title_parts.core.clone(), false));
        if let Some(ref alt) = title_parts.alt {
            queries.push((alt.clone(), false));
        }
        queries.push((title_parts.full.clone(), false));
        queries.dedup_by(|a, b| a.0 == b.0);

        let c_low = title_parts.core.to_lowercase();
        let a_low = clean_artist.to_lowercase();
        let alt_low = title_parts.alt.as_ref().map(|s| s.to_lowercase());
        let full_low = title_parts.full.to_lowercase();
        let mut ranked: Vec<(f32, i64)> = Vec::new();
        let mut seen_ids: Vec<i64> = Vec::new();

        for (query, has_artist) in queries {
            let resp = self
                .http
                .get("https://music.163.com/api/search/get/")
                .query(&[("s", query.as_str()), ("type", "1"), ("limit", "10")])
                .header("Referer", "https://music.163.com")
                .header("Cookie", "os=pc")
                .header("User-Agent", "Mozilla/5.0 (Windows NT 10.0; Win64; x64)")
                .timeout(Duration::from_secs(8))
                .send()
                .await?;
            if !resp.status().is_success() {
                continue;
            }
            let v: Value = resp.json().await?;
            let Some(songs) = v["result"]["songs"].as_array() else { continue };
            let mut strong_hit = false;
            for s in songs {
                let Some(id) = s["id"].as_i64() else { continue };
                if seen_ids.contains(&id) {
                    continue;
                }
                seen_ids.push(id);
                let name = s["name"].as_str().unwrap_or("").to_lowercase();
                let clean_name = clean_noise(&strip_brackets(&name)).to_lowercase();
                let dur = s["duration"].as_i64().unwrap_or(0);
                let artist_match = if a_low.is_empty() {
                    true
                } else {
                    s["artists"]
                        .as_array()
                        .map(|arr| arr.iter().filter_map(|a| a["name"].as_str()).any(|n| artist_matches(n, &clean_artist)))
                        .unwrap_or(false)
                };

                // Reject candidate if searched without artist and singer does not match
                if !has_artist && !a_low.is_empty() && !artist_match {
                    continue;
                }

                if target_ms > 0 && dur > 0 {
                    let diff_s = (dur - target_ms).abs() as f32 / 1000.0;
                    let max_diff = if artist_match { 8.0 } else { 4.0 };
                    if diff_s > max_diff {
                        continue;
                    }
                }

                let is_title_match = name == c_low || clean_name == c_low
                    || alt_low.as_ref().is_some_and(|a| name == *a || clean_name == *a)
                    || name == full_low || clean_name == full_low;
                let is_title_contain = name.contains(&c_low) || c_low.contains(&clean_name)
                    || alt_low.as_ref().is_some_and(|a| name.contains(a) || a.contains(&clean_name))
                    || name.contains(&full_low) || full_low.contains(&clean_name);

                let has_cjk = clean_name.chars().any(|c| (c as u32) >= 0x2E80);
                let min_len = if has_cjk { 1 } else { 2 };

                let mut score = 0.0f32;
                if is_title_match {
                    score += 60.0;
                } else if is_title_contain && clean_name.chars().count() >= min_len {
                    score += 35.0;
                } else {
                    // Reject unrelated song titles
                    continue;
                }
                if artist_match {
                    score += 30.0;
                }
                if target_ms > 0 && dur > 0 {
                    let diff_s = (dur - target_ms).abs() as f32 / 1000.0;
                    if diff_s <= 3.0 {
                        score += 30.0;
                    } else if diff_s <= 8.0 {
                        score += 15.0;
                    }
                }
                if score >= 75.0 {
                    strong_hit = true;
                }
                ranked.push((score, id));
            }
            if strong_hit {
                break;
            }
        }

        ranked.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));

        let song_ms = q.duration.unwrap_or(0) as i32 * 1000;
        let mut found: Vec<Lyrics> = Vec::new();
        for (_, id) in ranked.into_iter().take(4) {
            let resp = self
                .http
                .get("https://music.163.com/api/song/lyric")
                .query(&[
                    ("id", id.to_string()),
                    ("lv", "1".into()),
                    ("kv", "1".into()),
                    ("tv", "-1".into()),
                    ("yv", "1".into()),
                    ("ytv", "-1".into()),
                    ("yrv", "-1".into()),
                ])
                .header("Referer", "https://music.163.com")
                .header("Cookie", "os=pc")
                .header("User-Agent", "Mozilla/5.0 (Windows NT 10.0; Win64; x64)")
                .timeout(Duration::from_secs(8))
                .send()
                .await?;
            if !resp.status().is_success() {
                continue;
            }
            let v: Value = resp.json().await?;
            // 1. Word-by-word karaoke (`yrc.lyric`) when available
            if let Some(yrc_raw) = v["yrc"]["lyric"].as_str().filter(|s| !s.trim().is_empty()) {
                let yrc_lines = parse_yrc(yrc_raw, song_ms);
                if derive_sync(&yrc_lines) == Sync::Word {
                    if let Some(lyrics) = Lyrics::new(Provider::NeteaseRich, yrc_lines, None, vec![]) {
                        found.push(lyrics);
                    }
                }
            }
            // 2. Standard line-synced LRC (`lrc.lyric`)
            if let Some(raw) = v["lrc"]["lyric"].as_str().filter(|s| !s.trim().is_empty()) {
                let mut lines = parse_lrc(raw, song_ms);
                lrc_fixers(&mut lines);
                if derive_sync(&lines) != Sync::None {
                    let writers = lrc_songwriters(raw);
                    if let Some(lyrics) = Lyrics::new(Provider::NeteaseSynced, lines, None, writers) {
                        found.push(lyrics);
                    }
                }
            }
            if !found.is_empty() {
                break;
            }
        }
        Ok(found)
    }

    async fn fetch_lrclib(&self, q: &Query<'_>) -> anyhow::Result<Vec<Lyrics>> {
        let title_parts = clean_title_with_artist(q.song, q.artist);
        let clean_artist_str = clean_noise(&strip_brackets(q.artist.split(['/', ',', '&']).next().unwrap_or(q.artist)));
        let clean_artist = if clean_artist_str.is_empty() { q.main_artist } else { clean_artist_str.as_str() };
        let mut titles = vec![title_parts.core.clone()];
        if !q.bare_song.is_empty() && q.bare_song != title_parts.core {
            titles.push(q.bare_song.to_owned());
        }
        if let Some(ref alt) = title_parts.alt {
            titles.push(alt.clone());
        }
        if title_parts.full != title_parts.core && !title_parts.full.is_empty() {
            titles.push(title_parts.full.clone());
        }
        titles.dedup();

        let c_low = title_parts.core.to_lowercase();
        let a_low = clean_artist.to_lowercase();
        let alt_low = title_parts.alt.as_ref().map(|s| s.to_lowercase());
        let song_ms = q.duration.unwrap_or(0) as i32 * 1000;
        let target_s = q.duration.unwrap_or(0) as f32;

        for t in titles {
            if t.trim().is_empty() {
                continue;
            }
            let mut params = vec![("track_name", t.as_str())];
            if !clean_artist.is_empty() {
                params.push(("artist_name", clean_artist));
            }
            let resp = match self
                .http
                .get(LRCLIB_SEARCH)
                .query(&params)
                .header(reqwest::header::USER_AGENT, concat!("ytm-rs/", env!("CARGO_PKG_VERSION"), " (https://github.com/grapedjuice/ytm-rs)"))
                .timeout(Duration::from_secs(6))
                .send()
                .await
            {
                Ok(r) if r.status().is_success() => r,
                _ => continue,
            };

            let Ok(items) = resp.json::<Vec<Value>>().await else { continue };
            let mut synced_cand = None;
            let mut plain_cand = None;

            for item in items {
                let track = item["trackName"].as_str().or_else(|| item["name"].as_str()).unwrap_or("");
                let t_clean = clean_noise(&strip_brackets(track)).to_lowercase();
                let dur = item["duration"].as_f64().unwrap_or(0.0) as f32;
                let item_artist = item["artistName"].as_str().unwrap_or("");
                let artist_match = if a_low.is_empty() {
                    true
                } else {
                    artist_matches(item_artist, &clean_artist)
                };

                if target_s > 10.0 && dur > 10.0 {
                    let max_diff = if artist_match { 8.0 } else { 4.0 };
                    if (dur - target_s).abs() > max_diff {
                        continue;
                    }
                }
                // Verify candidate actually matches the requested title
                let is_lrclib_match = t_clean == c_low
                    || alt_low.as_ref().is_some_and(|a| t_clean == *a)
                    || t_clean.contains(&c_low) || c_low.contains(&t_clean)
                    || alt_low.as_ref().is_some_and(|a| t_clean.contains(a) || a.contains(&t_clean));
                if !is_lrclib_match && !c_low.is_empty() {
                    continue;
                }

                if let Some(synced) = item["syncedLyrics"].as_str().filter(|s| !s.trim().is_empty()) {
                    let mut lines = parse_lrc(synced, song_ms);
                    lrc_fixers(&mut lines);
                    if derive_sync(&lines) != Sync::None {
                        if let Some(lyrics) = Lyrics::new(Provider::LrclibSynced, lines, None, vec![]) {
                            synced_cand = Some(lyrics);
                            break;
                        }
                    }
                } else if plain_cand.is_none() {
                    if let Some(plain) = item["plainLyrics"].as_str().filter(|s| !s.trim().is_empty()) {
                        let lines = parse_plain(plain);
                        if let Some(lyrics) = Lyrics::new(Provider::LrclibPlain, lines, None, vec![]) {
                            plain_cand = Some(lyrics);
                        }
                    }
                }
            }

            if let Some(synced) = synced_cand {
                return Ok(vec![synced]);
            }
            if let Some(plain) = plain_cand {
                return Ok(vec![plain]);
            }
        }
        Ok(vec![])
    }

    async fn fetch_qq(&self, q: &Query<'_>) -> anyhow::Result<Vec<Lyrics>> {
        let title_parts = clean_title_with_artist(q.song, q.artist);
        let clean_artist = clean_noise(&strip_brackets(q.artist.split(['/', ',', '&']).next().unwrap_or(q.artist)));
        if title_parts.core.is_empty() && title_parts.full.is_empty() {
            return Ok(vec![]);
        }
        let target_s = q.duration.unwrap_or(0) as f32;
        let song_ms = q.duration.unwrap_or(0) as i32 * 1000;

        let mut queries: Vec<(String, bool)> = Vec::new();
        if !clean_artist.is_empty() {
            queries.push((format!("{} {clean_artist}", title_parts.core), true));
            if let Some(ref alt) = title_parts.alt {
                queries.push((format!("{alt} {clean_artist}"), true));
            }
        }
        queries.push((title_parts.core.clone(), false));
        if let Some(ref alt) = title_parts.alt {
            queries.push((alt.clone(), false));
        }
        queries.dedup_by(|a, b| a.0 == b.0);

        let c_low = title_parts.core.to_lowercase();
        let a_low = clean_artist.to_lowercase();
        let alt_low = title_parts.alt.as_ref().map(|s| s.to_lowercase());

        for (query, has_artist) in queries {
            let resp = match self
                .http
                .get("https://c.y.qq.com/soso/fcgi-bin/client_search_cp")
                .query(&[("p", "1"), ("n", "5"), ("w", query.as_str()), ("format", "json")])
                .header("Referer", "https://y.qq.com/portal/player.html")
                .header("User-Agent", "Mozilla/5.0 (Windows NT 10.0; Win64; x64)")
                .timeout(Duration::from_secs(6))
                .send()
                .await
            {
                Ok(r) if r.status().is_success() => r,
                _ => continue,
            };

            let text = resp.text().await.unwrap_or_default();
            let trimmed = text.trim();
            let json_slice = if let (Some(start), Some(end)) = (trimmed.find('{'), trimmed.rfind('}')) {
                &trimmed[start..=end]
            } else {
                trimmed
            };
            let Ok(v) = serde_json::from_str::<Value>(json_slice) else { continue };
            let Some(songs) = v["data"]["song"]["list"].as_array() else { continue };

            for song in songs {
                let songmid = match song["songmid"].as_str() {
                    Some(m) if !m.is_empty() => m,
                    _ => continue,
                };
                let name = song["songname"].as_str().unwrap_or("");
                let clean_name = clean_noise(&strip_brackets(name)).to_lowercase();
                let dur = song["interval"].as_i64().unwrap_or(0) as f32;

                let artist_match = if a_low.is_empty() {
                    true
                } else {
                    song["singer"]
                        .as_array()
                        .map(|arr| arr.iter().filter_map(|s| s["name"].as_str()).any(|n| artist_matches(n, &clean_artist)))
                        .unwrap_or(false)
                };

                // Reject candidate if searched without artist and singer does not match
                if !has_artist && !a_low.is_empty() && !artist_match {
                    continue;
                }

                if target_s > 10.0 && dur > 10.0 {
                    let max_diff = if artist_match { 8.0 } else { 4.0 };
                    if (dur - target_s).abs() > max_diff {
                        continue;
                    }
                }

                let is_title_match = clean_name == c_low
                    || alt_low.as_ref().is_some_and(|a| clean_name == *a)
                    || clean_name.contains(&c_low) || c_low.contains(&clean_name)
                    || alt_low.as_ref().is_some_and(|a| clean_name.contains(a) || a.contains(&clean_name));
                if !is_title_match && !c_low.is_empty() {
                    continue;
                }

                let l_resp = match self
                    .http
                    .get("https://c.y.qq.com/lyric/fcgi-bin/fcg_query_lyric_new.fcg")
                    .query(&[("songmid", songmid), ("format", "json")])
                    .header("Referer", "https://y.qq.com/portal/player.html")
                    .header("User-Agent", "Mozilla/5.0 (Windows NT 10.0; Win64; x64)")
                    .timeout(Duration::from_secs(6))
                    .send()
                    .await
                {
                    Ok(r) if r.status().is_success() => r,
                    _ => continue,
                };

                let l_text = l_resp.text().await.unwrap_or_default();
                let l_trimmed = l_text.trim();
                let l_slice = if let (Some(start), Some(end)) = (l_trimmed.find('{'), l_trimmed.rfind('}')) {
                    &l_trimmed[start..=end]
                } else {
                    l_trimmed
                };
                let Ok(lv) = serde_json::from_str::<Value>(l_slice) else { continue };
                let Some(raw_b64) = lv["lyric"].as_str().filter(|s| !s.trim().is_empty()) else { continue };
                let decoded = decode_qq_lyric(raw_b64);
                if decoded.is_empty() || !decoded.contains('[') {
                    continue;
                }

                let is_qrc = decoded.trim_start().starts_with("<?xml") || decoded.contains("<Lyric_1");
                let mut lines = if is_qrc {
                    parse_qrc(&decoded, song_ms, Some((&title_parts.core, &clean_artist)))
                } else {
                    parse_lrc(&decoded, song_ms)
                };
                lrc_fixers(&mut lines);
                let sync = derive_sync(&lines);
                if sync != Sync::None {
                    let prov = if sync == Sync::Word {
                        Provider::PortatoRich
                    } else {
                        Provider::PortatoSynced
                    };
                    let writers = lrc_songwriters(&decoded);
                    if let Some(lyrics) = Lyrics::new(prov, lines, None, writers) {
                        return Ok(vec![lyrics]);
                    }
                }
            }
        }
        Ok(vec![])
    }

    async fn fetch_kugou(&self, q: &Query<'_>) -> anyhow::Result<Vec<Lyrics>> {
        let title_parts = clean_title_with_artist(q.song, q.artist);
        let clean_artist = clean_noise(&strip_brackets(q.artist.split(['/', ',', '&']).next().unwrap_or(q.artist)));
        if title_parts.core.is_empty() && title_parts.full.is_empty() {
            return Ok(vec![]);
        }
        let target_s = q.duration.unwrap_or(0) as f32;
        let song_ms = q.duration.unwrap_or(0) as i32 * 1000;

        let mut queries: Vec<(String, bool)> = Vec::new();
        if !clean_artist.is_empty() {
            queries.push((format!("{} {clean_artist}", title_parts.core), true));
            if let Some(ref alt) = title_parts.alt {
                queries.push((format!("{alt} {clean_artist}"), true));
            }
        }
        queries.push((title_parts.core.clone(), false));
        if let Some(ref alt) = title_parts.alt {
            queries.push((alt.clone(), false));
        }
        queries.dedup_by(|a, b| a.0 == b.0);

        let c_low = title_parts.core.to_lowercase();
        let a_low = clean_artist.to_lowercase();
        let alt_low = title_parts.alt.as_ref().map(|s| s.to_lowercase());

        for (query, has_artist) in queries {
            let resp = match self
                .http
                .get("https://songsearch.kugou.com/song_search_v2")
                .query(&[("keyword", query.as_str()), ("page", "1"), ("pagesize", "5"), ("platform", "WebFilter"), ("format", "json")])
                .header("User-Agent", "Mozilla/5.0 (Windows NT 10.0; Win64; x64)")
                .timeout(Duration::from_secs(6))
                .send()
                .await
            {
                Ok(r) if r.status().is_success() => r,
                _ => continue,
            };

            let Ok(v) = resp.json::<Value>().await else { continue };
            let Some(lists) = v["data"]["lists"].as_array() else { continue };

            for item in lists {
                let file_hash = match item["FileHash"].as_str() {
                    Some(h) if !h.is_empty() => h,
                    _ => continue,
                };
                let name = item["SongName"].as_str().or_else(|| item["FileName"].as_str()).unwrap_or("");
                let clean_name = clean_noise(&strip_brackets(name)).to_lowercase();
                let dur = item["Duration"].as_i64().unwrap_or(0) as f32;
                let singer = item["SingerName"].as_str().unwrap_or("");
                let artist_match = if a_low.is_empty() {
                    true
                } else {
                    artist_matches(singer, &clean_artist)
                };

                // Reject candidate if searched without artist and singer does not match
                if !has_artist && !a_low.is_empty() && !artist_match {
                    continue;
                }

                if target_s > 10.0 && dur > 10.0 {
                    let max_diff = if artist_match { 8.0 } else { 4.0 };
                    if (dur - target_s).abs() > max_diff {
                        continue;
                    }
                }

                let is_title_match = clean_name == c_low
                    || alt_low.as_ref().is_some_and(|a| clean_name == *a)
                    || clean_name.contains(&c_low) || c_low.contains(&clean_name)
                    || alt_low.as_ref().is_some_and(|a| clean_name.contains(a) || a.contains(&clean_name));
                if !is_title_match && !c_low.is_empty() {
                    continue;
                }

                let dur_ms = (dur as u32) * 1000;
                let k_resp = match self
                    .http
                    .get("http://krcs.kugou.com/search")
                    .query(&[
                        ("ver", "1"),
                        ("man", "yes"),
                        ("client", "mobi"),
                        ("duration", &dur_ms.to_string()),
                        ("hash", file_hash),
                    ])
                    .header("User-Agent", "Mozilla/5.0 (Windows NT 10.0; Win64; x64)")
                    .timeout(Duration::from_secs(6))
                    .send()
                    .await
                {
                    Ok(r) if r.status().is_success() => r,
                    _ => continue,
                };

                let Ok(kv) = k_resp.json::<Value>().await else { continue };
                let Some(cands) = kv["candidates"].as_array() else { continue };
                let Some(first) = cands.first() else { continue };
                let (Some(id), Some(key)) = (
                    first["id"].as_i64().or_else(|| first["id"].as_str().and_then(|s| s.parse().ok())),
                    first["accesskey"].as_str(),
                ) else {
                    continue;
                };

                let dl_resp = match self
                    .http
                    .get("http://lyrics.kugou.com/download")
                    .query(&[
                        ("ver", "1"),
                        ("client", "pc"),
                        ("id", &id.to_string()),
                        ("accesskey", key),
                        ("fmt", "lrc"),
                        ("charset", "utf8"),
                    ])
                    .header("User-Agent", "Mozilla/5.0 (Windows NT 10.0; Win64; x64)")
                    .timeout(Duration::from_secs(6))
                    .send()
                    .await
                {
                    Ok(r) if r.status().is_success() => r,
                    _ => continue,
                };

                let Ok(dl_v) = dl_resp.json::<Value>().await else { continue };
                let Some(b64) = dl_v["content"].as_str().filter(|s| !s.trim().is_empty()) else { continue };
                let decoded = decode_qq_lyric(b64);
                if decoded.is_empty() || !decoded.contains('[') {
                    continue;
                }

                let mut lines = parse_lrc(&decoded, song_ms);
                lrc_fixers(&mut lines);
                if derive_sync(&lines) != Sync::None {
                    let writers = lrc_songwriters(&decoded);
                    if let Some(lyrics) = Lyrics::new(Provider::LegatoSynced, lines, None, writers) {
                        return Ok(vec![lyrics]);
                    }
                }
            }
        }
        Ok(vec![])
    }

    /// A valid JWT, from cache or by running the Turnstile challenge.
    async fn token(&self, force_new: bool) -> anyhow::Result<String> {
        let mut slot = self.jwt.lock().await;
        if let Some(t) = slot.as_ref().filter(|t| !force_new && jwt_seconds_left(t) > 60) {
            return Ok(t.clone());
        }
        if self.challenge_failed.lock().unwrap().is_some_and(|at| at.elapsed() < CHALLENGE_RETRY) {
            anyhow::bail!("lyrics verification failed recently; retrying later");
        }
        let profile = self.webview_profile.clone();
        let turnstile = match tokio::task::spawn_blocking(move || crate::turnstile::obtain(profile)).await? {
            Ok(t) => t,
            Err(e) => {
                *self.challenge_failed.lock().unwrap() = Some(std::time::Instant::now());
                return Err(e);
            }
        };
        let resp: Value = self
            .http
            .post(format!("{API}verify-turnstile"))
            .json(&serde_json::json!({ "token": turnstile }))
            .timeout(Duration::from_secs(15))
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        let jwt = resp["jwt"].as_str().ok_or_else(|| anyhow::anyhow!("no JWT in response"))?.to_owned();
        let _ = std::fs::write(&self.jwt_file, &jwt);
        log::info!("lyrics: new access token ({}h)", jwt_seconds_left(&jwt) / 3600);
        *slot = Some(jwt.clone());
        Ok(jwt)
    }
}

#[derive(Debug, Clone)]
pub struct CleanTitle {
    pub core: String,
    pub alt: Option<String>,
    pub full: String,
}

pub fn strip_brackets(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut depth = 0i32;
    for ch in s.chars() {
        match ch {
            '(' | '[' | '{' | '（' | '【' | '『' | '「' | '〔' | '〈' | '《' => depth += 1,
            ')' | ']' | '}' | '）' | '】' | '』' | '」' | '〕' | '〉' | '》' => depth = (depth - 1).max(0),
            _ if depth == 0 => out.push(ch),
            _ => {}
        }
    }
    out
}

pub fn clean_noise(s: &str) -> String {
    let mut out = s.to_string();
    let noise_words = [
        "official music video", "official lyric video", "official audio", "official video", "music video",
        "lyric video", "full audio", "audio video", "official", "theme song", "soundtrack",
        "original soundtrack", "ver.", "cover", "covered by", "theme", "audio", "mv",
    ];
    for w in noise_words {
        let lower = out.to_lowercase();
        if let Some(pos) = lower.find(w) {
            if out.is_char_boundary(pos) {
                if let Some(open) = out[..pos].rfind(|c| c == '(' || c == '[' || c == '（' || c == '【' || c == '-' || c == '—' || c == '|' || c == '｜') {
                    out.truncate(open);
                } else if pos > 0 {
                    let prev = out[..pos].chars().last().unwrap_or(' ');
                    if prev.is_whitespace() || prev == '/' || prev == '|' || prev == '-' {
                        out.truncate(pos);
                    }
                }
            }
        }
    }
    let prefixes = ["ost ", "ost: ", "ep ", "ep: ", "album ", "single "];
    for p in prefixes {
        if out.to_lowercase().starts_with(p) {
            out = out[p.len()..].to_string();
        }
    }
    if let Some(stripped) = out.strip_suffix(" EP").or_else(|| out.strip_suffix(" ep")) {
        out = stripped.to_string();
    }
    if let Some(stripped) = out.strip_suffix(" OST").or_else(|| out.strip_suffix(" ost")) {
        out = stripped.to_string();
    }
    out.trim_matches(|c: char| c == '.' || c == '…' || c == '-' || c == '—' || c == '/' || c == '|' || c == '｜' || c.is_whitespace())
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

pub fn clean_title_with_artist(raw: &str, artist: &str) -> CleanTitle {
    let bracketless = strip_brackets(raw);
    let without_pipe = bracketless.split(" | ").next().unwrap_or(&bracketless).split('｜').next().unwrap_or(&bracketless);
    let full = clean_noise(without_pipe);
    let full = if full.is_empty() { clean_noise(raw) } else { full };

    let mut core = full.clone();
    let mut alt = None;

    if let Some((l, r)) = full.split_once(" - ").or_else(|| full.split_once(" — ")) {
        let l_clean = clean_noise(l);
        let r_clean = clean_noise(r);
        let l_raw_low = l.trim().to_lowercase();
        let l_low = l_clean.to_lowercase();
        let a_low = clean_noise(&strip_brackets(artist)).to_lowercase();
        let is_artist_left = !a_low.is_empty() && (l_low == a_low || l_low.contains(&a_low));
        let is_ep_or_ost = l_raw_low.ends_with(" ep") || l_raw_low.ends_with(" ost") || l_raw_low == "ep" || l_raw_low == "ost";

        if is_artist_left || is_ep_or_ost {
            // "Artist - Song" or "EP - Song": The song is on the right!
            // Check if right has sub-title or bilingual like "青 / BLUE"
            if let Some((sub_l, sub_r)) = r_clean.split_once(" / ").or_else(|| r_clean.split_once('／')) {
                core = clean_noise(sub_l);
                alt = Some(clean_noise(sub_r));
            } else {
                core = r_clean;
                alt = if is_ep_or_ost { Some(l_clean) } else { None };
            }
        } else {
            let r_low = r_clean.to_lowercase();
            let is_artist_right = !a_low.is_empty() && (r_low == a_low || r_low.contains(&a_low));
            if is_artist_right {
                core = l_clean;
            } else {
                core = l_clean;
                alt = Some(r_clean);
            }
        }
    } else if let Some((l, r)) = full.split_once(" / ").or_else(|| full.split_once('／')) {
        let left = clean_noise(l);
        let right = clean_noise(r);
        if !left.is_empty() && !right.is_empty() {
            core = left;
            alt = Some(right);
        }
    }

    CleanTitle { core, alt, full }
}

#[allow(dead_code)]
pub fn clean_title(raw: &str) -> CleanTitle {
    clean_title_with_artist(raw, "")
}

/// Parses NetEase word-by-word karaoke (`yrc.lyric`):
/// `[line_start_ms,line_dur_ms](word_start_ms,word_dur_ms,0)word...`
fn parse_yrc(raw: &str, song_ms: i32) -> Vec<Line> {
    fn parse_yrc_token(s: &str) -> Option<(i32, i32, usize)> {
        let rest = s.strip_prefix('(')?;
        let close = rest.find(')')?;
        let inner = &rest[..close];
        let mut nums = inner.split(',');
        let start = nums.next()?.trim().parse::<i32>().ok()?;
        let dur = nums.next()?.trim().parse::<i32>().ok()?;
        // NetEase YRC always has 3 comma-separated integers `(start,dur,0)`
        let _flag = nums.next()?.trim().parse::<i32>().ok()?;
        if nums.next().is_some() {
            return None;
        }
        Some((start, dur.max(1), close + 2))
    }

    let mut lines: Vec<Line> = Vec::new();
    for raw_line in raw.lines() {
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with('{') {
            continue;
        }
        let Some(rest) = line.strip_prefix('[') else { continue };
        let Some(end_bracket) = rest.find(']') else { continue };
        let header = &rest[..end_bracket];
        let mut h_parts = header.split(',');
        let (Some(start_s), Some(dur_s)) = (h_parts.next(), h_parts.next()) else { continue };
        let (Ok(line_start), Ok(line_dur)) = (start_s.trim().parse::<i32>(), dur_s.trim().parse::<i32>()) else { continue };

        let body = &rest[end_bracket + 1..];
        let mut parts: Vec<Part> = Vec::new();
        let mut full_text = String::new();
        let mut pos = 0usize;
        while pos < body.len() {
            if let Some((w_start, w_dur, consumed)) = parse_yrc_token(&body[pos..]) {
                let text_start = pos + consumed;
                let mut text_end = body.len();
                let mut scan = text_start;
                while let Some(rel) = body[scan..].find('(') {
                    let cand = scan + rel;
                    if parse_yrc_token(&body[cand..]).is_some() {
                        text_end = cand;
                        break;
                    }
                    scan = cand + 1;
                }
                let word_text = &body[text_start..text_end];
                if !word_text.is_empty() {
                    full_text.push_str(word_text);
                    parts.push(Part {
                        start: w_start,
                        dur: w_dur,
                        text: word_text.to_owned(),
                        bg: false,
                    });
                }
                pos = text_end;
            } else {
                // Skip any leading non-token characters safely
                let next_char_len = body[pos..].chars().next().map_or(1, char::len_utf8);
                pos += next_char_len;
            }
        }
        if !full_text.trim().is_empty() && !parts.is_empty() {
            lines.push(Line {
                start: line_start,
                dur: line_dur.max(1),
                text: full_text.trim().to_owned(),
                parts,
                ..Default::default()
            });
        }
    }
    lines.sort_by_key(|l| l.start);
    insert_instrumentals(lines, song_ms)
}

fn jwt_seconds_left(jwt: &str) -> i64 {
    use base64::Engine;
    let Some(payload) = jwt.split('.').nth(1) else { return 0 };
    let Ok(bytes) = base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(payload.trim_end_matches('=')) else {
        return 0;
    };
    let exp = serde_json::from_slice::<Value>(&bytes).ok().and_then(|v| v["exp"].as_i64()).unwrap_or(0);
    let now = crate::unix_secs() as i64;
    exp - now
}

// ------------------------------------------------------------------- provider results

/// Every provider's lyrics out of a complete SSE response (`processStreamData`).
pub fn from_stream(body: &str, q: &Query) -> Vec<Lyrics> {
    let mut out: Vec<Lyrics> = Vec::new();
    let mut dur_ms = q.duration.unwrap_or(0) as i32 * 1000;
    let (mut title, mut artist) = (q.song.to_owned(), q.artist.to_owned());
    for block in body.split("\n\n").flat_map(|b| b.split("\r\n\r\n")) {
        let mut event = "";
        let mut data = String::new();
        for line in block.lines() {
            if let Some(e) = line.strip_prefix("event:") {
                event = e.trim();
            } else if let Some(d) = line.strip_prefix("data:") {
                data.push_str(d.trim());
            }
        }
        if data.is_empty() || data == "[DONE]" {
            continue;
        }
        let Ok(v) = serde_json::from_str::<Value>(&data) else { continue };
        if event == "metadata" {
            if let Some(d) = v["duration"].as_str().and_then(|d| d.parse::<f64>().ok()).or_else(|| v["duration"].as_f64()) {
                dur_ms = (d * 1000.0) as i32;
            }
            if let Some(s) = v["song"].as_str() {
                title = s.to_owned();
            }
            if let Some(a) = v["artist"].as_str() {
                artist = a.to_owned();
            }
            continue;
        }
        let r = &v["results"];
        let s = |k: &str| r[k].as_str().filter(|s| !s.is_empty());
        let mut push = |p: Provider, lines: Vec<Line>, lang: Option<String>, writers: Vec<String>| {
            if let Some(l) = Lyrics::new(p, lines, lang, writers) {
                out.retain(|o| o.provider != p);
                out.push(l);
            }
        };
        match v["provider"].as_str().unwrap_or_default() {
            "musixmatch" => {
                if let Some(t) = s("wordByWord") {
                    let mut lines = parse_lrc(t, dur_ms);
                    lrc_fixers(&mut lines);
                    push(Provider::MusixmatchRich, lines, None, lrc_songwriters(t));
                }
                if let Some(t) = s("synced") {
                    push(Provider::MusixmatchSynced, parse_lrc(t, dur_ms), None, lrc_songwriters(t));
                }
            }
            "lrclib" => {
                if let Some(t) = s("synced") {
                    push(Provider::LrclibSynced, parse_lrc(t, dur_ms), None, lrc_songwriters(t));
                }
                if let Some(t) = s("plain") {
                    push(Provider::LrclibPlain, parse_plain(t), None, vec![]);
                }
            }
            "kugou" => {
                if let Some(t) = s("lyrics").and_then(|t| serde_json::from_str::<Value>(t).ok()).and_then(|j| j["lyrics"].as_str().map(str::to_owned)) {
                    push(Provider::LegatoSynced, parse_lrc(&t, dur_ms), None, lrc_songwriters(&t));
                }
            }
            "qq" => {
                if let Some(t) = s("lyrics").and_then(|t| serde_json::from_str::<Value>(t).ok()).and_then(|j| j["lyrics"].as_str().map(str::to_owned)) {
                    push(Provider::PortatoRich, parse_qrc(&t, dur_ms, Some((&title, &artist))), None, qrc_songwriters(&t));
                }
            }
            "golyrics" => {
                if let Some(raw) = s("lyrics") {
                    let ttml = serde_json::from_str::<Value>(raw).ok().and_then(|j| j["ttml"].as_str().map(str::to_owned)).unwrap_or_else(|| raw.to_owned());
                    let t = parse_ttml(&ttml, dur_ms);
                    let p = if t.word_synced { Provider::BLyricsRich } else { Provider::BLyricsSynced };
                    push(p, t.lines, t.language, t.songwriters);
                }
            }
            "binimum" => {
                if let Some(raw) = s("lyrics") {
                    let t = parse_ttml(raw, dur_ms);
                    let word = match r["timingType"].as_str() {
                        Some("syllable") => true,
                        Some("line") => false,
                        _ => t.word_synced,
                    };
                    let p = if word { Provider::BinimumRich } else { Provider::BinimumSynced };
                    push(p, t.lines, t.language, t.songwriters);
                }
            }
            _ => {}
        }
    }
    out
}

/// The best of LRCLib's search hits for a song `dur_ms` long.
#[allow(dead_code)]
fn from_lrclib(hits: &[Value], dur_ms: i32) -> Vec<Lyrics> {
    let off = |h: &Value| h["duration"].as_f64().map_or(i32::MAX, |d| ((d * 1000.0) as i32 - dur_ms).abs());
    let text = |h: &Value, k: &str| h[k].as_str().filter(|t| !t.trim().is_empty()).map(str::to_owned);
    let Some(best) = hits
        .iter()
        .filter(|h| h["instrumental"] != true && off(h) <= 3000)
        .min_by_key(|h| (text(h, "syncedLyrics").is_none(), off(h)))
    else {
        return vec![];
    };
    let mut out = Vec::new();
    if let Some(t) = text(best, "syncedLyrics") {
        out.extend(Lyrics::new(Provider::LrclibSynced, parse_lrc(&t, dur_ms), None, lrc_songwriters(&t)));
    }
    if let Some(t) = text(best, "plainLyrics") {
        out.extend(Lyrics::new(Provider::LrclibPlain, parse_plain(&t), None, vec![]));
    }
    out
}

/// A Unison answer (`unison.ts`): TTML, LRC or plain text.
fn from_unison(data: &Value, video_id: &str, dur_ms: i32) -> Vec<Lyrics> {
    let (Some(format), Some(text)) = (data["format"].as_str(), data["lyrics"].as_str()) else { return vec![] };
    let lyrics = match format {
        "ttml" => {
            let t = parse_ttml(text, dur_ms);
            let p = if t.word_synced { Provider::UnisonRich } else { Provider::UnisonSynced };
            Lyrics::new(p, t.lines, t.language, t.songwriters)
        }
        "lrc" => {
            let p = if data["syncType"] == "richsync" { Provider::UnisonWord } else { Provider::UnisonSynced };
            Lyrics::new(p, parse_lrc(text, dur_ms), data["language"].as_str().map(str::to_owned), lrc_songwriters(text))
        }
        "plain" => Lyrics::new(Provider::UnisonPlain, parse_plain(text), None, vec![]),
        _ => None,
    };
    let s = |v: &Value| v.as_str().filter(|t| !t.trim().is_empty()).map(str::to_owned);
    let sub = &data["submitter"];
    let credit = UnisonCredit {
        video_id: s(&data["videoId"]).unwrap_or_else(|| video_id.to_owned()),
        submitter: s(&sub["displayName"]).zip(s(&sub["keyId"])),
    };
    lyrics.into_iter().map(|l| Lyrics { unison: Some(credit.clone()), ..l }).collect()
}

// ------------------------------------------------------------------- shared helpers

/// Gaps over this long between sung lines become instrumental lines.
const INSTRUMENTAL_GAP_MS: i32 = 5000;

/// `et`: instrumental lines for long gaps before, between and after the sung lines.
fn insert_instrumentals(mut lines: Vec<Line>, song_ms: i32) -> Vec<Line> {
    if lines.is_empty() {
        return lines;
    }
    // A line that ends before it starts (a TTML line with no `end`, or `end="00:00:00.000"`
    // as on Unison's "Cinderella", #254) runs to the next line, the last to the song's end.
    for i in 0..lines.len() {
        if lines[i].dur < 0 {
            let until = lines.get(i + 1).map_or(song_ms, |n| n.start);
            lines[i].dur = (until - lines[i].start).max(0);
        }
    }
    let gap = |start: i32, dur: i32| Line { start, dur, instrumental: true, ..Default::default() };
    let mut out = Vec::with_capacity(lines.len() + 4);
    if lines[0].start > INSTRUMENTAL_GAP_MS {
        out.push(gap(0, lines[0].start));
    }
    let n = lines.len();
    // The latest end, not the last line's: one with a bad end would otherwise put a
    // break spanning the whole song at the bottom, which the view follows all along.
    let last_end = lines.iter().map(Line::end).max().unwrap_or(0);
    for (i, l) in lines.iter().enumerate() {
        out.push(l.clone());
        if i + 1 < n {
            let end = l.end();
            let f = lines[i + 1].start - end;
            if f > INSTRUMENTAL_GAP_MS {
                out.push(gap(end, f));
            }
        }
    }
    if song_ms - last_end > INSTRUMENTAL_GAP_MS {
        out.push(gap(last_end, song_ms - last_end));
    }
    out
}

// Credit lines ("作词 : …", "Written by: …") that some providers put in the lyrics.
const WRITER_TAGS: &[&str] = &["词", "詞", "作词", "作詞", "曲", "作曲", "词曲", "詞曲", "作词作曲", "作詞作曲", "writtenby", "lyricsby", "composedby", "lyricist", "composer"];
const CREDIT_TAGS: &[&str] = &[
    "编曲", "編曲", "和声", "和聲", "混音", "吉他", "制作人", "製作人", "演唱", "原唱", "翻唱", "后期", "後期", "和音", "录音", "錄音", "策划", "策劃", "伴奏", "美工", "海报", "海報", "旁白", "producedby",
    "arrangedby", "mixing", "mastering", "vocal", "vocals", "guitar", "bass", "drums", "producer", "arranger",
];
const CREDIT_SUFFIXES: &[char] = &['词', '詞', '曲', '声', '聲', '音'];

fn norm_tag(t: &str) -> String {
    t.to_lowercase().split_whitespace().collect()
}

fn is_credit_tag(t: &str) -> bool {
    WRITER_TAGS.contains(&t) || CREDIT_TAGS.contains(&t) || (t.chars().count() <= 4 && t.ends_with(CREDIT_SUFFIXES))
}

fn tag_pieces(n: &str) -> Vec<String> {
    let no_latin: String = n.chars().filter(|c| !c.is_ascii_lowercase()).collect();
    no_latin.split(['/', '&', '、', ',', '，', '・', '·']).filter(|s| !s.is_empty()).map(str::to_owned).collect()
}

/// `X`: a label that only names credits.
fn is_credit_label(t: &str) -> bool {
    let n = norm_tag(t);
    if WRITER_TAGS.contains(&n.as_str()) || CREDIT_TAGS.contains(&n.as_str()) {
        return true;
    }
    let pieces = tag_pieces(&n);
    !pieces.is_empty() && pieces.iter().all(|p| is_credit_tag(p))
}

fn is_writer_label(t: &str) -> bool {
    let n = norm_tag(t);
    WRITER_TAGS.contains(&n.as_str()) || (is_credit_label(t) && tag_pieces(&n).iter().any(|p| WRITER_TAGS.contains(&p.as_str())))
}

fn split_label(t: &str) -> Option<(&str, &str)> {
    let i = t.find([':', '：'])?;
    let sep = t[i..].chars().next()?.len_utf8();
    let (name, rest) = (&t[..i], t[i + sep..].trim_start());
    (!name.is_empty() && !rest.is_empty()).then_some((name, rest))
}

/// `yt`: a "Label: names" credit line rather than a lyric.
fn is_credit_line(t: &str) -> bool {
    split_label(t.trim()).is_some_and(|(name, _)| is_credit_label(name))
}

fn split_names(t: &str) -> Vec<String> {
    t.split(['/', '、', '，', ',']).map(str::trim).filter(|s| !s.is_empty()).map(str::to_owned).collect()
}

/// `tt`: the writers named on a credit line.
fn writers_on(t: &str) -> Vec<String> {
    match split_label(t.trim()) {
        Some((name, rest)) if is_writer_label(name) => split_names(rest),
        _ => vec![],
    }
}

fn dedup(names: Vec<String>) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for n in names {
        let n = n.trim().to_owned();
        if !n.is_empty() && !out.contains(&n) {
            out.push(n);
        }
    }
    out
}

// ------------------------------------------------------------------- LRC

/// `V`: an LRC timestamp (`mm:ss.xx`, `h:mm:ss.xx` or seconds) in ms.
fn lrc_time(t: &str) -> i32 {
    let p: Vec<&str> = t.split(':').collect();
    let s = match p.len() {
        1 => p[0].parse::<f64>().unwrap_or(0.0) * 1000.0,
        2 => p[0].parse::<f64>().unwrap_or(0.0).trunc() * 60_000.0 + p[1].parse::<f64>().unwrap_or(0.0) * 1000.0,
        3 => p[0].parse::<f64>().unwrap_or(0.0).trunc() * 3_600_000.0 + p[1].parse::<f64>().unwrap_or(0.0).trunc() * 60_000.0 + p[2].parse::<f64>().unwrap_or(0.0) * 1000.0,
        _ => 0.0,
    };
    s.round() as i32
}

const LRC_META_TAGS: &[&str] = &["ti", "ar", "al", "au", "lr", "length", "by", "offset", "re", "tool", "ve", "#"];

/// `[mm:ss.xx]` tags: (byte range, time text).
fn line_stamps(s: &str) -> Vec<(std::ops::Range<usize>, &str)> {
    stamps(s, '[', ']')
}

/// `<mm:ss.xx>` word stamps.
fn word_stamps(s: &str) -> Vec<(std::ops::Range<usize>, &str)> {
    stamps(s, '<', '>')
}

fn stamps(s: &str, open: char, close: char) -> Vec<(std::ops::Range<usize>, &str)> {
    let mut out = Vec::new();
    let mut i = 0;
    while let Some(o) = s[i..].find(open) {
        let o = i + o;
        let Some(c) = s[o + 1..].find(close) else { break };
        let inner = &s[o + 1..o + 1 + c];
        let is_time = inner.split_once(':').is_some_and(|(m, sec)| {
            !m.is_empty() && m.bytes().all(|b| b.is_ascii_digit()) && sec.split_once('.').is_some_and(|(a, b)| !a.is_empty() && !b.is_empty() && a.bytes().chain(b.bytes()).all(|b| b.is_ascii_digit()))
        });
        if is_time {
            out.push((o..o + c + 2, inner));
            i = o + c + 2;
        } else {
            i = o + open.len_utf8();
        }
    }
    out
}

/// Text and stamps alternating, the way JS `split` with a capture group returns them.
fn split_stamps(s: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut at = 0;
    for (r, t) in word_stamps(s) {
        out.push(&s[at..r.start]);
        out.push(t);
        at = r.end;
    }
    out.push(&s[at..]);
    out
}

fn remove_ranges(s: &str, ranges: &[(std::ops::Range<usize>, &str)]) -> String {
    let mut out = String::with_capacity(s.len());
    let mut at = 0;
    for (r, _) in ranges {
        out.push_str(&s[at..r.start]);
        at = r.end;
    }
    out.push_str(&s[at..]);
    out
}

/// `parseLRC`: line LRC, enhanced (word-by-word) LRC and `[bg: …]` background vocals.
pub fn parse_lrc(text: &str, song_ms: i32) -> Vec<Line> {
    let mut lines: Vec<Line> = Vec::new();
    let mut meta: HashMap<String, String> = HashMap::new();
    for raw in text.split('\n') {
        let a = raw.trim();
        if let Some(inner) = a.strip_prefix('[').and_then(|r| r.strip_suffix(']'))
            && let Some((tag, val)) = inner.split_once(':')
            && !tag.is_empty()
            && tag.chars().all(|c| c.is_alphanumeric() || c == '_')
            && LRC_META_TAGS.contains(&tag)
        {
            meta.insert(tag.to_owned(), val.to_owned());
            continue;
        }
        let ls = line_stamps(a);
        if ls.is_empty() {
            continue;
        }
        let times: Vec<i32> = ls.iter().map(|(_, t)| lrc_time(t)).collect();
        let (c, u) = (*times.iter().min().unwrap(), *times.iter().max().unwrap());
        let g = remove_ranges(a, &ls).trim().to_owned();
        // `[bg: …]` at the end of the line.
        let (main, bg) = match g.rfind("[bg:") {
            Some(i) if g[i..].trim_end().ends_with(']') => {
                let inner = g[i + 4..].trim_end();
                (g[..i].to_owned(), Some(inner[..inner.len() - 1].to_owned()))
            }
            _ => (g.clone(), None),
        };
        let o = split_stamps(&main);
        // Whitespace-only text between two stamps: the words are spaced by their own segments.
        let spaced = o.iter().enumerate().any(|(e, w)| e % 2 == 0 && e > 0 && e < o.len() - 1 && !w.is_empty() && w.trim().is_empty());
        let mut parts: Vec<Part> = Vec::new();
        let mut prev: Option<i32> = None;
        let mut b = String::new();
        let mut walk = |w: &str, is_bg: bool, parts: &mut Vec<Part>, b: &mut String| {
            for (m, seg) in split_stamps(w).into_iter().enumerate() {
                if m % 2 == 0 {
                    let mut d = seg.to_owned();
                    if spaced {
                        let t = d.trim();
                        d = if t.is_empty() && !d.is_empty() { " ".to_owned() } else { t.to_owned() };
                    }
                    b.push_str(&d);
                    if let Some(last) = parts.last_mut() {
                        last.text.push_str(&d);
                    }
                } else {
                    let d = lrc_time(seg);
                    if let (Some(p), Some(last)) = (prev, parts.last_mut()) {
                        last.dur = d - p;
                    }
                    parts.push(Part { start: d, dur: 0, text: String::new(), bg: is_bg });
                    prev = Some(d);
                }
            }
        };
        walk(&main, false, &mut parts, &mut b);
        if let Some(bg) = &bg {
            walk(bg, true, &mut parts, &mut b);
        }
        let plain: String = remove_ranges(&main, &word_stamps(&main));
        if is_credit_line(&plain) {
            continue;
        }
        lines.push(Line { start: c, dur: u - c, text: b.trim().to_owned(), parts, ..Default::default() });
    }
    let n = lines.len();
    for l in 0..n {
        if l + 1 < n {
            let next_start = lines[l + 1].start;
            let a = &mut lines[l];
            if a.dur == 0 {
                a.dur = (next_start - a.start).max(0);
            }
            if !a.parts.is_empty() {
                let mut c = next_start;
                for f in &a.parts {
                    c = c.max(f.start);
                }
                let last = a.parts.last_mut().unwrap();
                last.dur = (next_start - last.start).max(0);
                a.dur = (c - a.start).max(0);
            }
        } else {
            let a = &mut lines[l];
            let i = song_ms - a.start;
            if a.dur == 0 && i > 0 {
                a.dur = i;
            }
            if let Some(last) = a.parts.last_mut() {
                let u = song_ms - last.start;
                if u > 0 {
                    last.dur = u;
                }
            }
        }
    }
    if let Some(off) = meta.get("offset") {
        let off = (off.trim().parse::<f64>().unwrap_or(0.0) * 1000.0) as i32;
        for a in &mut lines {
            a.start -= off;
            for p in &mut a.parts {
                p.start -= off;
            }
        }
    }
    lines
}

/// `lrcFixers`: Musixmatch word-by-word quirks (spaces carrying a word's tail, and
/// files where most words were given a token 0–100 ms).
pub fn lrc_fixers(lines: &mut [Line]) {
    for e in lines.iter_mut() {
        for r in 1..e.parts.len() {
            let (l, o) = {
                let (a, b) = e.parts.split_at_mut(r);
                (&mut a[r - 1], &mut b[0])
            };
            if o.text == " " && l.text != " " {
                let a = o.dur - l.dur;
                if a.abs() <= 15 || o.dur <= 100 {
                    let i = o.dur;
                    l.dur += i;
                    o.dur -= i;
                    o.start += i;
                }
            }
        }
    }
    let (mut short, mut total) = (0, 0);
    for e in lines.iter() {
        if e.parts.is_empty() {
            continue;
        }
        for o in e.parts.iter().take(e.parts.len().saturating_sub(2)) {
            if o.text != " " {
                if o.dur <= 100 {
                    short += 1;
                }
                total += 1;
            }
        }
    }
    if total > 0 && short as f32 / total as f32 > 0.5 {
        for e in 0..lines.len() {
            for o in 0..lines[e].parts.len() {
                if lines[e].parts[o].text == " " || lines[e].parts[o].dur > 400 {
                    continue;
                }
                let next = if o + 1 < lines[e].parts.len() {
                    Some((e, o + 1))
                } else if e + 1 < lines.len() && !lines[e + 1].parts.is_empty() {
                    Some((e + 1, 0))
                } else {
                    None
                };
                match next {
                    None => lines[e].parts[o].dur = 300,
                    Some((ne, no)) => {
                        let a = lines[ne].parts[no].clone();
                        if a.text == " " {
                            lines[e].parts[o].dur += a.dur;
                            let a = &mut lines[ne].parts[no];
                            a.start += a.dur;
                            a.dur = 0;
                        } else {
                            lines[e].parts[o].dur = a.start - lines[e].parts[o].start;
                        }
                    }
                }
            }
        }
    }
}

fn lrc_songwriters(text: &str) -> Vec<String> {
    let mut n = Vec::new();
    for s in text.split('\n') {
        let e = s.trim();
        if let Some(inner) = e.strip_prefix('[').and_then(|r| r.strip_suffix(']'))
            && let Some((tag, val)) = inner.split_once(':')
            && (tag == "au" || tag == "lr")
        {
            n.extend(split_names(val));
            continue;
        }
        let ls = line_stamps(e);
        if !ls.is_empty() {
            let o = remove_ranges(e, &ls);
            let o = match o.rfind("[bg:") {
                Some(i) => o[..i].to_owned(),
                None => o,
            };
            n.extend(writers_on(&remove_ranges(&o, &word_stamps(&o))));
        }
    }
    dedup(n)
}

pub fn parse_plain(text: &str) -> Vec<Line> {
    text.split('\n').map(|s| Line { text: s.to_owned(), ..Default::default() }).collect()
}

// ------------------------------------------------------------------- QRC

struct QrcLine {
    time: i32,
    duration: i32,
    text: String,
    syllables: Vec<(String, i32, i32)>,
    agent: Option<String>,
}

struct Singers {
    aliases: HashMap<String, String>,
    next: u32,
    current: Option<String>,
}

fn qrc_content(t: &str) -> String {
    let Some(i) = t.find("LyricContent=\"") else { return t.to_owned() };
    let rest = &t[i + 14..];
    // Up to the closing quote that ends the attribute (`"/>`, `">` or `" name=`).
    let mut end = None;
    for (j, _) in rest.match_indices('"') {
        let after = rest[j + 1..].trim_start();
        if after.starts_with("/>") || after.starts_with('>') || after.split_once('=').is_some_and(|(k, _)| !k.is_empty() && k.chars().all(|c| c.is_ascii_alphabetic())) {
            end = Some(j);
            break;
        }
    }
    let Some(end) = end else { return t.to_owned() };
    rest[..end].replace("&quot;", "\"").replace("&amp;", "&")
}

/// `[start,duration]` at the start of a QRC line.
fn qrc_header(t: &str) -> Option<(i32, i32, &str)> {
    let inner = t.strip_prefix('[')?;
    let close = inner.find(']')?;
    let (a, b) = inner[..close].split_once(',')?;
    Some((a.parse().ok()?, b.parse().ok()?, &inner[close + 1..]))
}

/// `lt`: `text(start,duration)` syllables.
fn qrc_syllables(t: &str) -> Vec<(String, i32, i32)> {
    let mut out = Vec::new();
    let mut s = 0;
    let mut i = 0;
    while let Some(o) = t[i..].find('(') {
        let o = i + o;
        let parsed = t[o + 1..].find(')').and_then(|c| {
            let (a, b) = t[o + 1..o + 1 + c].split_once(',')?;
            let ok = |x: &str| !x.is_empty() && x.bytes().all(|b| b.is_ascii_digit());
            (ok(a) && ok(b)).then(|| (a.parse::<i32>().ok(), b.parse::<i32>().ok(), o + 2 + c))
        });
        match parsed {
            Some((Some(a), Some(b), end)) => {
                out.push((t[s..o].to_owned(), a, b));
                s = end;
                i = end;
            }
            _ => i = o + 1,
        }
    }
    out
}

fn assign_singer(name: &str, line: &mut QrcLine, st: &mut Singers) {
    if !st.aliases.contains_key(name) {
        let up = name.to_uppercase();
        let group = up == "合" || up == "ALL" || up == "合唱";
        let id = if group {
            "v1000".to_owned()
        } else {
            let id = format!("v{}", st.next);
            st.next += 1;
            id
        };
        st.aliases.insert(name.to_owned(), id);
    }
    line.agent = st.aliases.get(name).cloned();
}

fn retime(line: &mut QrcLine) {
    if let (Some(f), Some(l)) = (line.syllables.first(), line.syllables.last()) {
        line.time = f.1;
        line.duration = l.1 + l.2 - f.1;
    }
}

/// `jt`: Sørensen–Dice similarity of character bigrams.
fn similarity(a: &str, b: &str) -> f32 {
    let (a, b): (Vec<char>, Vec<char>) = (a.to_lowercase().chars().collect(), b.to_lowercase().chars().collect());
    if a.len() < 2 || b.len() < 2 {
        return 0.0;
    }
    let mut map: HashMap<(char, char), i32> = HashMap::new();
    for w in a.windows(2) {
        *map.entry((w[0], w[1])).or_default() += 1;
    }
    let mut hits = 0;
    for w in b.windows(2) {
        if let Some(c) = map.get_mut(&(w[0], w[1]))
            && *c > 0
        {
            *c -= 1;
            hits += 1;
        }
    }
    hits as f32 * 2.0 / (a.len() + b.len() - 2) as f32
}

/// `Bt`: keep a QRC line? Drops title/credit lines and pulls "Singer: …" prefixes into agents.
fn keep_qrc_line(t: &mut QrcLine, st: &mut Singers, early: bool, meta: Option<(&str, &str)>) -> bool {
    if t.syllables.is_empty() {
        if let Some(c) = &st.current {
            t.agent = Some(c.clone());
        }
        return true;
    }
    if early {
        if let Some((title, artist)) = meta
            && (!title.is_empty() || !artist.is_empty())
        {
            let b = |p: &str| p.to_lowercase().chars().filter(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || ('\u{3000}'..='\u{9fff}').contains(c) || ('\u{ac00}'..='\u{d7af}').contains(c)).collect::<String>();
            let (h, tt, r) = (b(&t.text), b(title), b(artist));
            if (!tt.is_empty() && h.contains(&tt) && !r.is_empty() && h.contains(&r))
                || (!tt.is_empty() && h.contains(&tt) && r.is_empty() && h.chars().count() < tt.chars().count() + 15)
                || (!r.is_empty() && h == r)
            {
                return false;
            }
            if similarity(&t.text, &format!("{title} {artist}")) > 0.5 {
                return false;
            }
        }
        if t.syllables.len() > 2 && t.syllables.iter().all(|s| (s.2 - t.syllables[0].2).abs() < 10) {
            return false;
        }
    }
    if let Some(c) = &st.current {
        t.agent = Some(c.clone());
    }
    let mut r = String::new();
    let mut o = 0;
    for p in &t.syllables {
        r.push_str(&p.0);
        o += 1;
        if r.contains(':') || r.contains('：') {
            break;
        }
        if r.chars().count() > 40 {
            return true;
        }
    }
    let colon = r.find([':', '：']);
    let Some(ci) = colon else { return true };
    let name = r[..ci].trim().to_owned();
    let after = &r[ci + r[ci..].chars().next().unwrap().len_utf8()..];
    if name.is_empty() {
        return true;
    }
    if after.trim().is_empty() {
        // "Singer:" on its own.
        if is_credit_label(&name) {
            return false;
        }
        if name.chars().count() <= 30 {
            t.syllables.drain(..o);
            t.text = t.text.get(r.len()..).unwrap_or_default().to_owned();
            assign_singer(&name, t, st);
            st.current = st.aliases.get(&name).cloned();
            retime(t);
        }
        return true;
    }
    if is_credit_label(&name) {
        return false;
    }
    if name.chars().count() > 20 {
        return true;
    }
    let rest = after.trim_start().to_owned();
    let u = o - 1;
    let tail = t.syllables[u].0.find([':', '：']).map(|i| t.syllables[u].0[i..].chars().skip(1).collect::<String>().trim_start().to_owned()).unwrap_or_default();
    if !tail.is_empty() {
        t.syllables[u].0 = tail;
        t.syllables.drain(..u);
    } else {
        t.syllables.drain(..o);
    }
    assign_singer(&name, t, st);
    st.current = st.aliases.get(&name).cloned();
    t.text = rest;
    retime(t);
    true
}

fn decode_qq_lyric(raw: &str) -> String {
    use base64::Engine;
    let trimmed = raw.trim();
    let decoded = base64::engine::general_purpose::STANDARD
        .decode(trimmed)
        .or_else(|_| base64::engine::general_purpose::STANDARD_NO_PAD.decode(trimmed))
        .or_else(|_| base64::engine::general_purpose::URL_SAFE.decode(trimmed))
        .or_else(|_| base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(trimmed));
    let s = match decoded {
        Ok(bytes) => String::from_utf8_lossy(&bytes).to_string(),
        Err(_) => raw.to_string(),
    };
    s.replace("&#58;", ":")
        .replace("&#10;", "\n")
        .replace("&#32;", " ")
        .replace("&#39;", "'")
        .replace("&apos;", "'")
        .replace("&quot;", "\"")
        .replace("&amp;", "&")
}

/// `parseQRC`: QQ Music's syllable format.
pub fn parse_qrc(text: &str, song_ms: i32, meta: Option<(&str, &str)>) -> Vec<Line> {
    if text.is_empty() {
        return vec![];
    }
    let content = qrc_content(text);
    let mut r: Vec<QrcLine> = Vec::new();
    let mut st = Singers { aliases: HashMap::new(), next: 1, current: None };
    for a in content.split('\n') {
        let i = a.trim();
        if i.is_empty() || (i.starts_with('[') && i[1..].find(':').is_some_and(|c| c > 0 && i[1..1 + c].chars().all(|ch| ch.is_ascii_alphabetic()))) {
            continue;
        }
        let Some((time, duration, rest)) = qrc_header(i) else { continue };
        let syllables = qrc_syllables(rest);
        let text = syllables.iter().map(|s| s.0.as_str()).collect::<String>();
        let mut g = QrcLine { time, duration, text, syllables, agent: None };
        let early = r.len() < 5;
        if keep_qrc_line(&mut g, &mut st, early, meta) {
            r.push(g);
        }
    }
    if r.is_empty() {
        return vec![];
    }
    let n = r.len();
    let lines = (0..n)
        .map(|i| {
            let a = &r[i];
            let mut parts: Vec<Part> = a.syllables.iter().map(|(t, s, d)| Part { start: *s, dur: *d, text: t.clone(), bg: false }).collect();
            let mut u = a.duration;
            if u == 0 && i + 1 < n {
                u = (r[i + 1].time - a.time).max(0);
            } else if u == 0 {
                u = song_ms - a.time;
            }
            if let Some(f) = parts.last_mut()
                && f.dur == 0
            {
                f.dur = if i + 1 < n { (r[i + 1].time - f.start).max(0) } else { song_ms - f.start };
            }
            Line { start: a.time, dur: u, text: a.text.clone(), parts, agent: a.agent.clone(), ..Default::default() }
        })
        .collect();
    insert_instrumentals(lines, song_ms)
}

fn qrc_songwriters(text: &str) -> Vec<String> {
    let mut n = Vec::new();
    for s in qrc_content(text).split('\n') {
        if let Some((_, _, rest)) = qrc_header(s.trim()) {
            let t: String = qrc_syllables(rest).into_iter().map(|s| s.0).collect();
            n.extend(writers_on(&t));
        }
    }
    dedup(n)
}

// ------------------------------------------------------------------- TTML

pub struct Ttml {
    pub lines: Vec<Line>,
    pub word_synced: bool,
    pub language: Option<String>,
    pub songwriters: Vec<String>,
}

/// `C` (parseTTMLTime): `1.5s` / `2m` / `1h` / `250ms`, or `h:m:s.fff`.
pub fn ttml_time(t: &str) -> i32 {
    let t = t.trim();
    if t.is_empty() {
        return 0;
    }
    for (suffix, mul) in [("ms", 1.0), ("h", 3_600_000.0), ("m", 60_000.0), ("s", 1000.0)] {
        if let Some(n) = t.strip_suffix(suffix)
            && !n.is_empty()
            && n.bytes().all(|b| b.is_ascii_digit() || b == b'.')
        {
            return (n.parse::<f64>().unwrap_or(0.0) * mul).round() as i32;
        }
    }
    let s: Vec<f64> = t.split(':').map(|p| p.chars().filter(|c| c.is_ascii_digit() || *c == '.').collect::<String>().parse::<f64>().unwrap_or(0.0)).collect();
    let ms = match s.len() {
        1 => s[0] * 1000.0,
        2 => s[0].trunc() * 60_000.0 + s[1] * 1000.0,
        3 => s[0].trunc() * 3_600_000.0 + s[1].trunc() * 60_000.0 + s[2] * 1000.0,
        _ => 0.0,
    };
    ms.round() as i32
}

/// `At`: declare prefixes the file uses without binding (an XML parser refuses them).
fn bind_prefixes(t: &str) -> String {
    let Some(start) = t.find("<tt") else { return t.to_owned() };
    let Some(len) = t[start..].find('>') else { return t.to_owned() };
    let tag = &t[start..start + len + 1];
    let mut declared: Vec<String> = vec!["xml".into(), "xmlns".into()];
    for (i, _) in tag.match_indices("xmlns:") {
        let name: String = tag[i + 6..].chars().take_while(|c| c.is_alphanumeric() || matches!(c, '_' | '.' | '-')).collect();
        declared.push(name);
    }
    let mut used: Vec<String> = Vec::new();
    let bytes: Vec<char> = t.chars().collect();
    let mut i = 0;
    while i < bytes.len() {
        // `<prefix:` / `</prefix:` and ` prefix:attr=`
        let at_tag = bytes[i] == '<' || (bytes[i] == '/' && i > 0 && bytes[i - 1] == '<');
        let at_attr = bytes[i].is_whitespace();
        if at_tag || at_attr {
            let mut j = i + 1;
            let mut name = String::new();
            while j < bytes.len() && (bytes[j].is_alphanumeric() || matches!(bytes[j], '_' | '.' | '-')) {
                name.push(bytes[j]);
                j += 1;
            }
            if !name.is_empty() && name.starts_with(|c: char| c.is_ascii_alphabetic()) && j < bytes.len() && bytes[j] == ':' {
                let ok = if at_attr {
                    let mut k = j + 1;
                    while k < bytes.len() && (bytes[k].is_alphanumeric() || matches!(bytes[k], '_' | '.' | '-')) {
                        k += 1;
                    }
                    while k < bytes.len() && bytes[k].is_whitespace() {
                        k += 1;
                    }
                    k < bytes.len() && bytes[k] == '='
                } else {
                    true
                };
                if ok && !used.contains(&name) {
                    used.push(name);
                }
            }
        }
        i += 1;
    }
    let missing: Vec<&String> = used.iter().filter(|u| !declared.contains(u)).collect();
    if missing.is_empty() {
        return t.to_owned();
    }
    let extra: String = missing.iter().map(|m| format!(" xmlns:{m}=\"urn:braccato:unbound:{m}\"")).collect();
    let new_tag = format!("{}{}>", &tag[..tag.len() - 1], extra);
    t.replacen(tag, &new_tag, 1)
}

fn attr<'a>(n: roxmltree::Node<'a, '_>, name: &str) -> Option<&'a str> {
    n.attributes().find(|a| a.name() == name).map(|a| a.value())
}

/// `D`: text of a node and its spans.
fn span_text(n: roxmltree::Node) -> String {
    let mut s = String::new();
    for c in n.children() {
        if let Some(t) = c.text().filter(|_| c.is_text()) {
            s.push_str(t);
        } else if c.is_element() && c.tag_name().name() == "span" {
            s.push_str(&span_text(c));
        }
    }
    s
}

/// `U`: a paragraph's (or transliteration's) children as parts and text.
fn ttml_parts(node: roxmltree::Node, line_start: i32) -> (Vec<Part>, String, bool) {
    let mut s = String::new();
    let mut e: Vec<Part> = Vec::new();
    let mut timed = false;
    for u in node.children() {
        let bg = u.is_element() && attr(u, "role") == Some("x-bg");
        let group: Vec<roxmltree::Node> = if bg { u.children().collect() } else { vec![u] };
        for m in group {
            if m.is_text() {
                let t = m.text().unwrap_or_default();
                if t.is_empty() {
                    continue;
                }
                s.push_str(t);
                let start = e.last().map_or(line_start, |p| p.start + p.dur);
                e.push(Part { start, dur: 0, text: t.to_owned(), bg });
            } else if m.is_element() && m.tag_name().name() == "span" {
                let p = span_text(m);
                if p.is_empty() {
                    continue;
                }
                let b = ttml_time(attr(m, "begin").unwrap_or_default());
                let h = ttml_time(attr(m, "end").unwrap_or_default());
                e.push(Part { start: b, dur: h - b, text: p.clone(), bg });
                s.push_str(&p);
                timed = true;
            }
        }
    }
    if !timed {
        e.clear();
    }
    (e, s, timed)
}

/// `parseTTMLContent`: Apple-style TTML with agents, background vocals, and the
/// translations and transliterations in its iTunes metadata.
pub fn parse_ttml(text: &str, song_ms: i32) -> Ttml {
    let empty = |language| Ttml { lines: vec![], word_synced: false, language, songwriters: vec![] };
    let src = bind_prefixes(&text.replace("\\\"", "\""));
    let Ok(doc) = roxmltree::Document::parse_with_options(&src, roxmltree::ParsingOptions { allow_dtd: true, ..Default::default() }) else {
        return empty(None);
    };
    let tt = doc.root_element();
    if tt.tag_name().name() != "tt" {
        return empty(None);
    }
    let body = tt.children().find(|n| n.tag_name().name() == "body");
    let language = attr(tt, "lang").or_else(|| body.and_then(|b| attr(b, "lang"))).map(str::to_owned);
    let Some(body) = body else { return empty(language) };
    let head_meta = tt.children().find(|n| n.tag_name().name() == "head").and_then(|h| h.children().find(|n| n.tag_name().name() == "metadata"));
    // Agents: people and characters are v1, v2, …; anything else sings for everyone.
    let mut agents: HashMap<String, String> = HashMap::new();
    if let Some(m) = head_meta {
        let mut k = 0;
        for a in m.children().filter(|n| n.tag_name().name() == "agent") {
            let Some(id) = attr(a, "id") else { continue };
            let kind = attr(a, "type").unwrap_or_default();
            if kind == "person" || kind == "character" {
                k += 1;
                agents.insert(id.to_owned(), format!("v{k}"));
            } else {
                agents.insert(id.to_owned(), "v1000".to_owned());
            }
        }
    }
    let ps: Vec<roxmltree::Node> = body.children().filter(|n| n.tag_name().name() == "div").flat_map(|d| d.children().filter(|n| n.tag_name().name() == "p")).collect();
    if ps.is_empty() {
        return empty(language);
    }
    let mut lines: Vec<Line> = Vec::new();
    let mut keys: HashMap<String, Vec<usize>> = HashMap::new();
    let mut word_synced = false;
    for p in ps {
        let Some(begin) = attr(p, "begin") else { continue };
        let start = ttml_time(begin);
        let end = ttml_time(attr(p, "end").unwrap_or_default());
        let (parts, text, timed) = ttml_parts(p, start);
        word_synced |= timed;
        let agent = attr(p, "agent").map(|a| agents.get(a).cloned().unwrap_or_else(|| a.to_owned()));
        if let Some(k) = attr(p, "key") {
            keys.entry(k.to_owned()).or_default().push(lines.len());
        }
        lines.push(Line { start, dur: end - start, text, parts, agent, ..Default::default() });
    }
    // Translations and transliterations, matched to lines by key.
    let meta_root = head_meta.and_then(|m| m.descendants().find(|n| n.tag_name().name() == "iTunesMetadata")).or(head_meta);
    if let Some(m) = meta_root {
        if let Some(ts) = m.descendants().find(|n| n.tag_name().name() == "translations") {
            let default_lang = attr(ts, "lang");
            for t in ts.children().filter(|n| n.tag_name().name() == "translation") {
                let lang = attr(t, "lang").or(default_lang);
                for item in t.children().filter(|n| n.is_element()) {
                    let Some(key) = attr(item, "for").or_else(|| attr(t, "for")) else { continue };
                    let txt = span_text(item);
                    let (Some(lang), false) = (lang, txt.is_empty()) else { continue };
                    for &i in keys.get(key).map(Vec::as_slice).unwrap_or_default() {
                        lines[i].translations.push((lang.to_owned(), txt.clone()));
                    }
                }
            }
        }
        if let Some(ts) = m.descendants().find(|n| n.tag_name().name() == "transliterations") {
            for t in ts.children().filter(|n| n.tag_name().name() == "transliteration") {
                for item in t.children().filter(|n| n.is_element()) {
                    let Some(key) = attr(item, "for") else { continue };
                    for &i in keys.get(key).map(Vec::as_slice).unwrap_or_default() {
                        let (parts, text, _) = ttml_parts(item, lines[i].start);
                        if text.is_empty() {
                            continue;
                        }
                        lines[i].romanization = Some(text);
                        lines[i].timed_roman = parts;
                    }
                }
            }
        }
    }
    let song = attr(body, "dur").map(ttml_time).unwrap_or(song_ms);
    let songwriters = {
        let mut n = Vec::new();
        if let Some(m) = head_meta {
            if let Some(sw) = m.descendants().find(|n| n.tag_name().name() == "songwriters") {
                for w in sw.children().filter(|n| n.tag_name().name() == "songwriter") {
                    n.push(span_text(w));
                }
            }
            for meta in m.children().filter(|n| n.tag_name().name() == "meta") {
                if matches!(attr(meta, "key"), Some("songwriter" | "songwriters"))
                    && let Some(v) = attr(meta, "value")
                {
                    n.push(v.to_owned());
                }
            }
        }
        dedup(n)
    };
    Ttml { lines: insert_instrumentals(lines, song), word_synced, language, songwriters }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn timed(lines: &[(i32, i32, &str, bool)], sync: Sync) -> Lyrics {
        let lines = lines.iter().map(|&(start, dur, text, instrumental)| Line { start, dur, text: text.into(), instrumental, ..Default::default() }).collect();
        Lyrics { lines, sync, provider: Provider::LrclibSynced, language: None, songwriters: vec![], unison: None }
    }

    #[test]
    fn lrc_has_a_timed_line_each_and_clears_the_quiet() {
        let l = timed(
            &[(1500, 2000, " Hello ", false), (3500, 1000, "world", false), (4500, 8000, "", true), (12500, 1000, "again", false), (61_230, 500, "late", false)],
            Sync::Line,
        );
        assert_eq!(to_lrc(&l, 0), "[00:01.50]Hello
[00:03.50]world
[00:04.50]
[00:12.50]again
[00:13.50]
[01:01.23]late
[01:01.73]");
        // Shown 0.5 s later in the app: so in the file; never before the start.
        assert_eq!(to_lrc(&l, 500).lines().next(), Some("[00:02.00]Hello"));
        assert_eq!(to_lrc(&l, -2000).lines().next(), Some("[00:00.00]Hello"));
        // Word-timed lyrics go in by line: what every LRC reader takes.
        assert_eq!(to_lrc(&timed(&[(0, 900, "Hi", false)], Sync::Word), 0), "[00:00.00]Hi
[00:00.90]");
        // Without timing, just the words.
        assert_eq!(to_lrc(&timed(&[(0, 0, "One", false), (0, 0, "Two", false)], Sync::None), 0), "One
Two");
    }

    #[test]
    fn enhanced_lrc_parts() {
        let l = parse_lrc("[00:31.69] <00:31.69> Like <00:35.63>   <00:35.64> the <00:35.69>\n[00:40.00] Next", 60_000);
        assert_eq!(l[0].text, "Like the");
        assert_eq!(l[0].parts[0].start, 31690);
        // Spaced files: word segments are trimmed, spaces get their own parts.
        assert_eq!(l[0].parts[0].text, "Like");
        assert_eq!(l[0].parts[1].text, " ");
        assert_eq!(l[0].parts[0].dur, 3940);
        assert_eq!(l[1].start, 40000);
        // Last line runs to the end of the song.
        assert_eq!(l[1].dur, 20000);
    }

    #[test]
    fn lrc_skips_credits_and_reads_offset() {
        // The extension reads `offset` in seconds.
        let l = parse_lrc("[offset:+0.5]\n[00:01.00]作词 : 野田洋次郎\n[00:05.00]Hello\n[00:07.00]World", 10_000);
        assert_eq!(l.len(), 2);
        assert_eq!(l[0].start, 4500);
        assert_eq!(lrc_songwriters("[00:01.00]作词 : 野田洋次郎\n"), vec!["野田洋次郎"]);
    }

    #[test]
    fn lrc_fixers_move_space_time_to_word() {
        let mut l = parse_lrc("[00:01.00]<00:01.00>a<00:01.50> <00:02.00>b<00:02.50>\n[00:03.00]x", 5000);
        lrc_fixers(&mut l);
        assert_eq!(l[0].parts[0].dur, 1000);
    }

    #[test]
    fn qrc_drops_title_and_reads_syllables() {
        let xml = "<Lyric_1 LyricContent=\"[ti:x]\n[0,6350]Song - Artist(0,198)\n[1000,2000]Get(1000,500) (1500,10)Lucky(1510,490)\n\"/>";
        let l = parse_qrc(xml, 3000, Some(("Song", "Artist")));
        let sung: Vec<&Line> = l.iter().filter(|l| !l.instrumental).collect();
        assert_eq!(sung.len(), 1);
        assert_eq!(sung[0].text, "Get Lucky");
        assert_eq!(sung[0].parts[2].start, 1510);
    }

    #[test]
    fn ttml_parts_background_agents_and_extras() {
        let xml = r#"<tt xmlns="http://www.w3.org/ns/ttml" xmlns:itunes="http://music.apple.com/lyric-ttml-internal" xmlns:ttm="http://www.w3.org/ns/ttml#metadata" xml:lang="ja"><head><metadata><ttm:agent type="person" xml:id="v1"/><ttm:agent type="person" xml:id="v2"/><iTunesMetadata xmlns="http://music.apple.com/lyric-ttml-internal"><translations><translation xml:lang="en"><text for="L1">Hello world</text></translation></translations><songwriters><songwriter>野田洋次郎</songwriter></songwriters><transliterations><transliteration xml:lang="ja-Latn"><text for="L1"><span begin="1.0" end="2.0">kon</span> <span begin="2.0" end="3.0">nichiwa</span></text></transliteration></transliterations></iTunesMetadata></metadata></head><body dur="20.0"><div>
<p begin="1.0" end="3.0" itunes:key="L1" ttm:agent="v1"><span begin="1.0" end="1.5">こん</span><span begin="1.5" end="2.0">に</span> <span begin="2.0" end="3.0">ちは</span><span ttm:role="x-bg"><span begin="2.5" end="3.5">(yeah)</span></span></p>
<p begin="10.0" end="11.0" itunes:key="L2" ttm:agent="v2">Line two</p></div></body></tt>"#;
        let t = parse_ttml(xml, 0);
        assert!(t.word_synced);
        assert_eq!(t.language.as_deref(), Some("ja"));
        assert_eq!(t.songwriters, vec!["野田洋次郎"]);
        let l = &t.lines[0];
        assert_eq!(l.text, "こんに ちは(yeah)");
        assert_eq!(l.parts.len(), 5);
        assert_eq!(l.parts[2], Part { start: 2000, dur: 0, text: " ".into(), bg: false });
        assert!(l.parts[4].bg && l.parts[4].start == 2500);
        assert_eq!(l.agent.as_deref(), Some("v1"));
        assert_eq!(l.translations, vec![("en".to_owned(), "Hello world".to_owned())]);
        assert_eq!(l.romanization.as_deref(), Some("kon nichiwa"));
        assert_eq!(l.timed_roman.len(), 3);
        // A gap of more than five seconds becomes an instrumental line.
        assert!(t.lines[1].instrumental && t.lines[1].start == 3000);
        assert_eq!(t.lines[2].agent.as_deref(), Some("v2"));
        assert!(t.lines[2].parts.is_empty());
        assert!(t.lines.last().unwrap().instrumental);
    }

    #[test]
    fn ttml_line_ending_before_it_starts_runs_to_the_next() {
        // Unison's "Cinderella" (#254) ends on an empty line whose end is 0: the break
        // after the last line must come after it, not span the whole song from 0.
        let xml = r#"<tt xmlns="http://www.w3.org/ns/ttml"><body dur="00:08:00.000"><div>
<p begin="00:07:48.320" end="00:07:54.200"><span begin="00:07:48.320" end="00:07:54.200">rain</span></p>
<p begin="00:07:54.200"><span begin="00:07:54.200" end="00:07:55.000">la</span></p>
<p begin="00:07:56.000" end="00:00:00.000"></p></div></body></tt>"#;
        let lines = parse_ttml(xml, 0).lines;
        assert!(lines.windows(2).all(|w| w[0].start <= w[1].start), "{lines:?}");
        assert!(lines.iter().all(|l| l.dur >= 0), "{lines:?}");
        // (lines[0] is the break before the first line.) A line without an end runs to
        // the next one, and the empty last line to the song's end.
        assert_eq!((lines[2].start, lines[2].dur), (474_200, 1800));
        assert_eq!((lines[3].start, lines[3].dur), (476_000, 4000));
        assert_eq!(lines.len(), 4);
    }

    #[test]
    fn break_after_the_last_line_follows_the_latest_end() {
        // The last line ends inside the one before it.
        let line = |start, dur| Line { start, dur, text: "x".into(), ..Default::default() };
        let out = insert_instrumentals(vec![line(1000, 20_000), line(2000, 1000)], 60_000);
        let gap = out.last().unwrap();
        assert!(gap.instrumental && gap.start == 21_000 && gap.dur == 39_000, "{out:?}");
    }

    #[test]
    fn lrclib_picks_the_closest_synced_hit() {
        let hits: Vec<Value> = serde_json::from_value(serde_json::json!([
            { "duration": 200.0, "plainLyrics": "far off" },
            { "duration": 181.0, "plainLyrics": "plain only" },
            { "duration": 182.5, "syncedLyrics": "[00:01.00]synced", "plainLyrics": "synced" },
            { "duration": 180.0, "instrumental": true },
        ]))
        .unwrap();
        let got = from_lrclib(&hits, 180_000);
        assert_eq!(got.iter().map(|l| l.provider).collect::<Vec<_>>(), [Provider::LrclibSynced, Provider::LrclibPlain]);
        assert_eq!(got[0].lines.iter().find(|l| !l.instrumental).unwrap().text, "synced");
        // Nothing within 3 s: nothing, rather than another song's lyrics.
        assert!(from_lrclib(&hits, 120_000).is_empty());
    }

    #[test]
    fn ttml_times() {
        assert_eq!(ttml_time("1:02.345"), 62345);
        assert_eq!(ttml_time("12.5s"), 12500);
        assert_eq!(ttml_time("250ms"), 250);
        assert_eq!(ttml_time("01:00:01.000"), 3_601_000);
    }

    #[test]
    fn clean_title_handles_brackets_noise_and_alt_titles() {
        let t1 = clean_title("《Arknights》OST [ Тысячу лет спустя ] Toska | Время, вперед Theme");
        assert_eq!(t1.core, "Toska");

        let t2 = clean_title("Тоска EP - Тысячу лет спустя | Arknights");
        assert_eq!(t2.core, "Тысячу лет спустя");
        assert_eq!(t2.alt.as_deref(), Some("Тоска"));

        let t3 = clean_title("Whiteness");
        assert_eq!(t3.core, "Whiteness");
        assert!(t3.alt.is_none());

        let t4 = clean_title("ソラニン / ASIAN KUNG-FU GENERATION (cover)");
        assert_eq!(t4.core, "ソラニン");
        assert_eq!(t4.alt.as_deref(), Some("ASIAN KUNG-FU GENERATION"));

        let t5 = clean_title_with_artist("DUSTCELL - 青 / BLUE", "DUSTCELL");
        assert_eq!(t5.core, "青");
        assert_eq!(t5.alt.as_deref(), Some("BLUE"));
    }

    #[test]
    fn unison_lyrics_saved_without_a_credit_credit_unison_alone() {
        let lyrics = |provider, unison| Lyrics { lines: vec![], sync: Sync::None, provider, language: None, songwriters: vec![], unison };
        let had = UnisonCredit { video_id: "v".into(), submitter: Some(("boidu".into(), "k".into())) };
        let mut set = LyricsSet { sources: vec![lyrics(Provider::UnisonSynced, None), lyrics(Provider::UnisonPlain, Some(had.clone())), lyrics(Provider::LrclibSynced, None)] };
        set.credit_unison("v");
        assert_eq!(set.sources[0].unison, Some(UnisonCredit { video_id: "v".into(), submitter: None }));
        // One that has its credit keeps it, and other sources get none.
        assert_eq!((set.sources[1].unison.as_ref(), set.sources[2].unison.as_ref()), (Some(&had), None));
    }

    #[test]
    fn unison_credits_the_song_and_submitter() {
        let v: Value = serde_json::from_str(include_str!("testdata/unison-hey-boyyy.json")).unwrap();
        let got = from_unison(&v["data"], "asked", 180_000);
        let c = got[0].unison.as_ref().unwrap();
        assert_eq!(got[0].provider, Provider::UnisonRich);
        assert_eq!(c.song_url(), "https://unison.betterlyrics.org/song/Xk_vH2uOb2c");
        let (name, key) = c.submitter.as_ref().unwrap();
        assert_eq!(name, "boidu");
        assert_eq!(UnisonCredit::submitter_url(key), "https://unison.betterlyrics.org/curator/a80e8e35cf2d400a2d6a52abb66118690f0c81a7bfad4c4a1603d61e85ae6ea2");
        // No submitter or video id: Unison alone, linked to the song asked for.
        let bare = serde_json::json!({ "format": "plain", "lyrics": "la la" });
        let c = from_unison(&bare, "asked", 0)[0].unison.clone().unwrap();
        assert_eq!(c, UnisonCredit { video_id: "asked".into(), submitter: None });
    }

    #[test]
    fn real_stream_parses_every_provider() {
        let Ok(body) = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/research/nandemonaiya.sse")) else { return };
        let q = Query { video_id: "n89SKAymNfA", song: "Nandemonaiya - movie ver.", artist: "RADWIMPS", album: None, duration: Some(343), bare_song: "", main_artist: "" };
        let got = from_stream(&body, &q);
        let keys: Vec<&str> = got.iter().map(|l| l.provider.key()).collect();
        assert!(keys.contains(&"bLyrics-richsynced"), "{keys:?}");
        let bl = got.iter().find(|l| l.provider == Provider::BLyricsRich).unwrap();
        assert_eq!(bl.sync, Sync::Word);
        assert!(bl.lines.iter().any(|l| l.timed_roman.len() > 3));
        assert_eq!(bl.songwriters, vec!["野田洋次郎"]);
    }

    #[test]
    fn artist_matching_rejects_unrelated_artists() {
        assert!(!artist_matches("Sia", "HIBANA"));
        assert!(!artist_matches("KAITO", "HIBANA"));
        assert!(artist_matches("Sia", "Sia"));
        assert!(artist_matches("Sia, Belinda", "Sia"));
        assert!(artist_matches("YOASOBI (ヨアソビ)", "YOASOBI"));
        assert!(artist_matches("HIBANA", ""));
    }

    #[test]
    fn portato_provider_variants() {
        assert_eq!(Provider::PortatoRich.name(), "QQ Music");
        assert_eq!(Provider::PortatoSynced.name(), "QQ Music");
        assert_eq!(Provider::PortatoRich.sync(), SyncKind::Word);
        assert_eq!(Provider::PortatoSynced.sync(), SyncKind::Line);
        assert!(Provider::ALL.contains(&Provider::PortatoRich));
        assert!(Provider::ALL.contains(&Provider::PortatoSynced));
    }
}
