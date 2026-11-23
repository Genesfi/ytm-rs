//! Stats page & listening tracker: parity with NabyTune's Stats + live shared storage.
//!
//! Every song heard is timed while actually playing (pauses and buffering left out) and
//! appended to the shared `listens.jsonl` when it finishes, changes, or the app closes.
//! Hidden songs/artists live beside it in `stats_hidden.json`. When NabyTune writes to
//! either file, the Stats page detects the change and reloads automatically.

use std::path::PathBuf;
use std::rc::Rc;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use egui::{Align2, Color32, CornerRadius, FontId, Pos2, Rect, RichText, Sense, Stroke, StrokeKind, Ui, Vec2, pos2, vec2};

use super::pages::{divider, page_title, setting_row, setting_row_wide, settings_card};
use super::shell::vgradient;
use super::widgets::{self, icon_at, named, paint_remote, pill, song_menu, text_at};
use super::{Action, App, Page};
use crate::backend;
use crate::model::{Card, Link, Target, Thumb, Track, sized};
use crate::stats::{self, ArtistStat, FileStamp, Hidden, Listen, Period, SongStat, Span, Summary};
use crate::theme::{self, icon};

/// The Wrapped card's size: 4:5, as a shareable post, the same in every style.
const CARD_W: f32 = 480.0;
const CARD_H: f32 = 600.0;
/// How many of each top list show until "Show top 10".
const SHORT_LIST: usize = 5;

/// The song being listened to: the log's entry so far, when it was last counted, and the
/// seconds heard (with their fractions).
pub(super) struct Listening {
    listen: Listen,
    counted: Instant,
    heard: f32,
}

/// How the Wrapped card looks. Each is its own layout, palette and type.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum WrappedStyle {
    Signature,
    Poster,
    Minimal,
    Receipt,
    Neon,
    Aurora,
    Cassette,
    Polaroid,
    Numbers,
    Magazine,
}

impl WrappedStyle {
    pub const ALL: [WrappedStyle; 10] = [
        WrappedStyle::Signature,
        WrappedStyle::Poster,
        WrappedStyle::Minimal,
        WrappedStyle::Receipt,
        WrappedStyle::Neon,
        WrappedStyle::Aurora,
        WrappedStyle::Cassette,
        WrappedStyle::Polaroid,
        WrappedStyle::Numbers,
        WrappedStyle::Magazine,
    ];

    pub fn key(self) -> &'static str {
        match self {
            Self::Signature => "signature",
            Self::Poster => "poster",
            Self::Minimal => "minimal",
            Self::Receipt => "receipt",
            Self::Neon => "neon",
            Self::Aurora => "aurora",
            Self::Cassette => "cassette",
            Self::Polaroid => "polaroid",
            Self::Numbers => "numbers",
            Self::Magazine => "magazine",
        }
    }

    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|s| s.key() == key)
    }

    fn name(self) -> &'static str {
        match self {
            Self::Signature => "Signature",
            Self::Poster => "Poster",
            Self::Minimal => "Minimal",
            Self::Receipt => "Receipt",
            Self::Neon => "Neon",
            Self::Aurora => "Aurora",
            Self::Cassette => "Cassette",
            Self::Polaroid => "Polaroid",
            Self::Numbers => "Big numbers",
            Self::Magazine => "Magazine",
        }
    }
}

#[derive(Clone)]
enum UndoHide {
    Song { id: String, title: String },
    Artist(String),
}

#[derive(Clone, Copy, PartialEq)]
struct CacheKey {
    period: Period,
    back: u32,
    listens: usize,
    rev: u64,
    now: u64,
    playing: Option<(u64, u32)>,
}

struct Cached {
    key: CacheKey,
    summary: Rc<Summary>,
    earliest: Option<u64>,
    offset: i64,
}

#[derive(Clone, PartialEq, Eq)]
struct PlayedLatelyKey {
    rev: u64,
    current_id: Option<String>,
    hide_explicit: bool,
    limit: usize,
}

struct PlayedLatelyCache {
    key: PlayedLatelyKey,
    cards: Rc<Vec<Card>>,
}

struct LoadedPayload {
    listens: Vec<Listen>,
    hidden: Hidden,
    log_stamp: FileStamp,
    hidden_stamp: FileStamp,
}

/// The Stats page's state.
pub(super) struct StatsState {
    pub period: Period,
    pub back: u32,
    pub listens: Option<Vec<Listen>>,
    pub hidden: Hidden,
    pub style: WrappedStyle,
    /// Bumped whenever `listens` or `hidden` mutates, to invalidate `cache`.
    rev: u64,
    expanded: bool,
    show_hidden: bool,
    clearing: bool,
    undo: Option<UndoHide>,
    data_note: Option<String>,
    /// Result of saving the Wrapped card PNG (written from the screenshot callback).
    save_note: Arc<Mutex<Option<String>>>,
    /// Path of the most recently saved Wrapped card PNG (for "Open folder" / "Open image").
    last_saved: Arc<Mutex<Option<PathBuf>>>,
    /// When set, paints the Wrapped card on a 1-frame top overlay so it can be captured from any scroll position.
    capture_overlay: Option<String>,
    /// Last drawn screen rect of the Wrapped card, for cropping the screenshot.
    card_rect: Option<Rect>,
    /// Pending background reload result.
    pending_load: Arc<Mutex<Option<LoadedPayload>>>,
    loading: bool,
    log_stamp: FileStamp,
    hidden_stamp: FileStamp,
    last_sync_check: Option<Instant>,
    cache: Option<Cached>,
    played_lately: Option<PlayedLatelyCache>,
}

impl StatsState {
    pub fn new(saved_style: Option<&str>) -> Self {
        Self {
            period: Period::Month,
            back: 0,
            listens: None,
            hidden: Hidden::default(),
            style: saved_style.and_then(WrappedStyle::from_key).unwrap_or(WrappedStyle::Signature),
            rev: 0,
            expanded: false,
            show_hidden: false,
            clearing: false,
            undo: None,
            data_note: None,
            save_note: Arc::new(Mutex::new(None)),
            last_saved: Arc::new(Mutex::new(None)),
            capture_overlay: None,
            card_rect: None,
            pending_load: Arc::new(Mutex::new(None)),
            loading: false,
            log_stamp: FileStamp::default(),
            hidden_stamp: FileStamp::default(),
            last_sync_check: None,
            cache: None,
            played_lately: None,
        }
    }
}

fn unix_now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

fn is_test_run() -> bool {
    std::env::var_os("YTM_BACKGROUND").is_some() || std::env::var_os("YTM_SMOKE").is_some()
}

fn track_to_listen(track: &Track, duration_secs: u32) -> Listen {
    Listen {
        video_id: track.id.clone(),
        title: track.title.clone(),
        artist: track.artist_line(),
        album: track.album.as_ref().map(|a| a.name.clone()).unwrap_or_default(),
        thumbnail: backend::art_url(track),
        at: unix_now(),
        seconds: 0,
        length: if duration_secs > 0 { duration_secs } else { track.duration.unwrap_or(0) },
    }
}

// ------------------------------------------------------------------ App hooks

impl App {
    /// A new song started: flush the previous listen and begin timing this one.
    pub(super) fn stats_start(&mut self) {
        self.stats_flush();
        if is_test_run() {
            return;
        }
        let Some(track) = self.heard_song().cloned() else { return };
        let dur = (self.audio.status.duration_ms.load(Ordering::Relaxed) / 1000) as u32;
        self.listening = Some(Listening {
            listen: track_to_listen(&track, dur),
            counted: Instant::now(),
            heard: 0.0,
        });
    }

    /// Ensure we have an active `Listening` when resuming a restored track.
    pub(super) fn stats_ensure_started(&mut self) {
        if self.listening.is_none() {
            self.stats_start();
        }
    }

    /// Count elapsed time while actually playing, and poll background reloads / file sync.
    pub(super) fn stats_tick(&mut self, ctx: &egui::Context) {
        let playing = self.playing() && !self.buffering;
        let dur = (self.audio.status.duration_ms.load(Ordering::Relaxed) / 1000) as u32;
        let resolved = if self.playing_as().is_some() { self.heard_song().cloned() } else { None };
        if let Some(l) = self.listening.as_mut() {
            let gap = l.counted.elapsed().min(Duration::from_secs(2));
            l.counted = Instant::now();
            if playing {
                l.heard += gap.as_secs_f32();
                if dur > 0 && l.listen.length == 0 {
                    l.listen.length = dur;
                }
            }
            // If a music video resolved to its matched song version, keep the song metadata.
            if let Some(song) = resolved
                && song.id != l.listen.video_id
            {
                l.listen.video_id = song.id.clone();
                l.listen.title = song.title.clone();
                l.listen.artist = song.artist_line();
                if let Some(a) = &song.album {
                    l.listen.album = a.name.clone();
                }
                if let Some(u) = backend::art_url(&song) {
                    l.listen.thumbnail = Some(u);
                }
            }
        }

        // Apply finished background load if any.
        let loaded = self.stats.pending_load.lock().unwrap().take();
        if let Some(payload) = loaded {
            self.stats.listens = Some(payload.listens);
            self.stats.hidden = payload.hidden;
            self.stats.log_stamp = payload.log_stamp;
            self.stats.hidden_stamp = payload.hidden_stamp;
            self.stats.loading = false;
            self.stats.rev = self.stats.rev.wrapping_add(1);
            self.stats.cache = None;
            ctx.request_repaint();
        }

        // While viewing Stats or Home ("Played lately"), periodically check if NabyTune
        // updated the shared files so recent history stays in sync in real time.
        if matches!(self.page, Page::Stats | Page::Home) {
            let due = self.stats.last_sync_check.is_none_or(|t| t.elapsed() >= Duration::from_secs(3));
            if due {
                self.stats_sync_check(false);
            }
            if !self.window_hidden {
                ctx.request_repaint_after(Duration::from_secs(3));
            }
        }
    }

    /// Flush the current listen to `listens.jsonl` (if at least 1 second was heard).
    pub(super) fn stats_flush(&mut self) {
        let playing = self.playing() && !self.buffering;
        if let Some(l) = self.listening.as_mut() {
            let gap = l.counted.elapsed().min(Duration::from_secs(2));
            l.counted = Instant::now();
            if playing {
                l.heard += gap.as_secs_f32();
            }
        }
        let Some(mut l) = self.listening.take() else { return };
        l.listen.seconds = l.heard as u32;
        if l.listen.seconds == 0 || is_test_run() {
            return;
        }
        let path = stats::log_path(self.backend.data_dir());
        if let Err(e) = stats::record(&path, &l.listen) {
            log::warn!("stats: couldn't log listen: {e:#}");
        } else {
            self.stats.log_stamp = FileStamp::of(&path);
        }
        if let Some(all) = self.stats.listens.as_mut() {
            all.push(l.listen);
            self.stats.rev = self.stats.rev.wrapping_add(1);
            self.stats.cache = None;
        }
    }

    /// Check whether the shared files changed on disk (or haven't been loaded yet) and
    /// kick off a background load if so.
    pub(super) fn stats_sync_check(&mut self, force: bool) {
        self.stats.last_sync_check = Some(Instant::now());
        if self.stats.loading {
            return;
        }
        let log_path = stats::log_path(self.backend.data_dir());
        let hidden_path = stats::hidden_path(self.backend.data_dir());
        let log_stamp = FileStamp::of(&log_path);
        let hidden_stamp = FileStamp::of(&hidden_path);
        let needs = force
            || self.stats.listens.is_none()
            || log_stamp != self.stats.log_stamp
            || hidden_stamp != self.stats.hidden_stamp;
        if !needs {
            return;
        }
        self.stats.loading = true;
        let slot = self.stats.pending_load.clone();
        let backend = self.backend.clone();
        std::thread::spawn(move || {
            let listens = stats::load(&log_path);
            let hidden = Hidden::load(&hidden_path);
            let log_stamp = FileStamp::of(&log_path);
            let hidden_stamp = FileStamp::of(&hidden_path);
            *slot.lock().unwrap() = Some(LoadedPayload { listens, hidden, log_stamp, hidden_stamp });
            backend.wake();
        });
    }

    pub(super) fn invalidate_played_lately(&mut self) {
        self.stats.played_lately = None;
    }

    /// Most recently played unique songs for the Home "Played lately" shelf: the currently
    /// playing track immediately at the front, followed by shared listening history in reverse
    /// chronological order (auto-synced with NabyTune's `listens.jsonl`). Cached across frames
    /// so 60–120 FPS animations on Home do zero per-frame allocations.
    pub(super) fn played_lately_cards(&mut self, limit: usize) -> Rc<Vec<Card>> {
        let current_id = self.current().or_else(|| self.heard_song()).map(|t| t.id.clone());
        let key = PlayedLatelyKey {
            rev: self.stats.rev,
            current_id,
            hide_explicit: self.hide_explicit,
            limit,
        };
        if let Some(c) = self.stats.played_lately.as_ref().filter(|c| c.key == key) {
            return c.cards.clone();
        }
        let mut cards = Vec::with_capacity(limit);
        let mut seen = std::collections::HashSet::with_capacity(limit);

        if let Some(track) = self.current().or_else(|| self.heard_song())
            && !track.id.is_empty()
            && !self.stats.hidden.hides_song(&track.id)
            && !(self.hide_explicit && track.explicit)
            && self.blocklist.is_blocked(&track).is_none()
            && seen.insert(track.id.clone())
        {
            cards.push(Card {
                title: track.title.clone(),
                subtitle: track.artist_line(),
                thumbs: track.thumbs.clone(),
                target: Target::Song(track.clone()),
                round: false,
                links: track.artists.clone(),
            });
        }

        if let Some(listens) = &self.stats.listens {
            for l in listens.iter().rev() {
                if cards.len() >= limit {
                    break;
                }
                if l.video_id.is_empty()
                    || l.title.is_empty()
                    || self.stats.hidden.hides(l)
                    || self.blocklist.is_listen_blocked(&l.video_id, &l.title, &l.artist)
                    || !seen.insert(l.video_id.clone())
                {
                    continue;
                }
                let is_yt_video = l.thumbnail.as_deref().is_some_and(|u| u.contains("i.ytimg.com"));
                let thumbs = match &l.thumbnail {
                    Some(url) if !url.is_empty() => vec![Thumb {
                        url: sized(url, 544),
                        width: if is_yt_video { 480 } else { 544 },
                        height: if is_yt_video { 360 } else { 544 },
                    }],
                    _ => vec![Thumb { url: format!("https://i.ytimg.com/vi/{}/hqdefault.jpg", l.video_id), width: 480, height: 360 }],
                };
                let artists = if l.artist.is_empty() {
                    Vec::new()
                } else {
                    vec![Link { name: l.artist.clone(), id: None }]
                };
                let album = (!l.album.is_empty()).then(|| Link { name: l.album.clone(), id: None });
                let track = Track {
                    id: l.video_id.clone(),
                    title: l.title.clone(),
                    artists,
                    album,
                    duration: (l.length > 0).then_some(l.length),
                    thumbs: thumbs.clone(),
                    music_video: is_yt_video,
                    video: is_yt_video,
                    ..Default::default()
                };
                cards.push(Card {
                    title: l.title.clone(),
                    subtitle: l.artist.clone(),
                    thumbs,
                    target: Target::Song(track),
                    round: false,
                    links: Vec::new(),
                });
            }
        }
        let cards = Rc::new(cards);
        self.stats.played_lately = Some(PlayedLatelyCache { key, cards: cards.clone() });
        cards
    }

    fn stats_summary(&mut self) -> (Rc<Summary>, Option<u64>, i64) {
        let now = unix_now();
        let key = CacheKey {
            period: self.stats.period,
            back: self.stats.back,
            listens: self.stats.listens.as_ref().map_or(0, Vec::len),
            rev: self.stats.rev,
            now: now / 300,
            playing: self.listening.as_ref().map(|l| (l.listen.at, l.heard as u32 / 10)),
        };
        if let Some(c) = self.stats.cache.as_ref().filter(|c| c.key == key) {
            return (c.summary.clone(), c.earliest, c.offset);
        }
        let offset = stats::local_offset();
        let mut all = stats::visible(self.stats.listens.as_deref().unwrap_or_default(), &self.stats.hidden);
        if let Some(l) = &self.listening {
            let mut current = l.listen.clone();
            current.seconds = l.heard as u32;
            if current.seconds > 0 && !self.stats.hidden.hides(&current) {
                all.push(current);
            }
        }
        let earliest = all.iter().map(|l| l.at).min();
        let summary = Rc::new(stats::summary_back(&all, key.period, key.back, now, offset));
        self.stats.cache = Some(Cached { key, summary: summary.clone(), earliest, offset });
        (summary, earliest, offset)
    }

    fn hide_song(&mut self, song: &SongStat) {
        if self.stats.hidden.hide_song(song.key(), &song.title, &song.artist) {
            self.stats.undo = Some(UndoHide::Song { id: song.key().to_string(), title: song.title.clone() });
            self.stats_hidden_changed();
        }
    }

    fn hide_artist(&mut self, name: &str) {
        if self.stats.hidden.hide_artist(name) {
            self.stats.undo = Some(UndoHide::Artist(name.to_string()));
            self.stats_hidden_changed();
        }
    }

    fn stats_hidden_changed(&mut self) {
        self.stats.rev = self.stats.rev.wrapping_add(1);
        self.stats.cache = None;
        let path = stats::hidden_path(self.backend.data_dir());
        match self.stats.hidden.save(&path) {
            Ok(()) => {
                self.stats.hidden_stamp = FileStamp::of(&path);
            }
            Err(e) => {
                log::warn!("stats: couldn't save hidden list: {e:#}");
                self.stats.data_note = Some(format!("Couldn't save that: {e:#}"));
            }
        }
    }

    fn clear_stats_history(&mut self, span: Option<Span>, label: String) {
        let path = stats::log_path(self.backend.data_dir());
        let result = match span {
            Some(span) => stats::clear_span(&path, span).map(|_| ()),
            None => stats::clear_all(&path),
        };
        match result {
            Ok(()) => {
                self.stats.log_stamp = FileStamp::of(&path);
                if let Some(all) = self.stats.listens.as_mut() {
                    match span {
                        Some(span) => all.retain(|l| !span.contains(l.at)),
                        None => all.clear(),
                    }
                }
                if let Some(l) = self.listening.as_mut()
                    && span.is_none_or(|s| s.contains(l.listen.at))
                {
                    l.heard = 0.0;
                    l.counted = Instant::now();
                    l.listen.at = unix_now();
                }
                self.stats.data_note = Some(match span {
                    Some(_) => format!("{label} is cleared"),
                    None => "Your listening history is cleared".into(),
                });
                if span.is_none() {
                    self.stats.back = 0;
                }
                self.stats.undo = None;
            }
            Err(e) => {
                log::warn!("stats: couldn't clear history: {e:#}");
                self.stats.data_note = Some(format!("Couldn't clear it: {e:#}"));
            }
        }
        self.stats.clearing = false;
        self.stats.rev = self.stats.rev.wrapping_add(1);
        self.stats.cache = None;
    }

    fn save_wrapped_png(&mut self, ctx: &egui::Context, period_label: &str) {
        let screen = ctx.content_rect();
        if let Some(rect) = self.stats.card_rect
            && rect.top() >= screen.top()
            && rect.bottom() <= screen.bottom()
            && rect.left() >= screen.left()
            && rect.right() <= screen.right()
        {
            self.capture_wrapped_rect(ctx, rect, period_label);
            return;
        }
        if screen.width() < CARD_W || screen.height() < CARD_H {
            *self.stats.save_note.lock().unwrap() = Some("Make the window a little bigger (at least 480×600) to save the card.".into());
            return;
        }
        // Card is scrolled partially off-screen: paint it cleanly on a 1-frame overlay inside
        // the viewport so saving always works without asking the user to scroll manually.
        self.stats.capture_overlay = Some(period_label.to_owned());
        *self.stats.save_note.lock().unwrap() = Some("Saving…".into());
        ctx.request_repaint();
    }

    fn capture_wrapped_rect(&mut self, ctx: &egui::Context, rect: Rect, period_label: &str) {
        let stem = format!("YT Music Wrapped {} - {}", period_label.replace(['/', '\\', ':'], "-"), self.stats.style.name());
        let out_path = stats::unique_png_name(&stats::pictures_dir(), &stem);
        let ppp = ctx.pixels_per_point();
        let note_slot = self.stats.save_note.clone();
        let saved_slot = self.stats.last_saved.clone();
        *note_slot.lock().unwrap() = Some("Saving…".into());
        let wake = ctx.clone();
        ctx.request_screenshot(move |img| {
            let res = crop_and_save_png(&img, rect, ppp, &out_path);
            let msg = match res {
                Ok(()) => {
                    *saved_slot.lock().unwrap() = Some(out_path.clone());
                    format!("Saved to {}", out_path.file_name().and_then(|n| n.to_str()).unwrap_or("Pictures"))
                }
                Err(e) => format!("Couldn't save it: {e:#}"),
            };
            *note_slot.lock().unwrap() = Some(msg);
            wake.request_repaint();
        });
    }
}

fn open_saved_image(path: &std::path::Path) {
    #[cfg(target_os = "windows")]
    let _ = std::process::Command::new("explorer").arg(path).spawn();
    #[cfg(target_os = "macos")]
    let _ = std::process::Command::new("open").arg(path).spawn();
    #[cfg(target_os = "linux")]
    let _ = std::process::Command::new("xdg-open").arg(path).spawn();
}

fn crop_and_save_png(img: &egui::ColorImage, rect: Rect, ppp: f32, out: &PathBuf) -> anyhow::Result<()> {
    let [iw, ih] = img.size;
    let x0 = ((rect.left() * ppp).round() as isize).clamp(0, iw as isize) as usize;
    let y0 = ((rect.top() * ppp).round() as isize).clamp(0, ih as isize) as usize;
    let x1 = ((rect.right() * ppp).round() as isize).clamp(x0 as isize, iw as isize) as usize;
    let y1 = ((rect.bottom() * ppp).round() as isize).clamp(y0 as isize, ih as isize) as usize;
    let (w, h) = (x1.saturating_sub(x0), y1.saturating_sub(y0));
    if w == 0 || h == 0 {
        anyhow::bail!("card is outside the window");
    }
    let mut bytes = Vec::with_capacity(w * h * 4);
    for y in y0..y1 {
        let row = &img.pixels[y * iw + x0..y * iw + x1];
        for c in row {
            let [r, g, b, _] = c.to_array();
            bytes.extend_from_slice(&[r, g, b, 255]);
        }
    }
    if let Some(dir) = out.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let rgba = image::RgbaImage::from_raw(w as u32, h as u32, bytes).ok_or_else(|| anyhow::anyhow!("pixel buffer size mismatch"))?;
    rgba.save_with_format(out, image::ImageFormat::Png)?;
    Ok(())
}

// ------------------------------------------------------------------ Page UI

pub fn page(app: &mut App, ui: &mut Ui) {
    if app.stats.listens.is_none() && !app.stats.loading {
        app.stats_sync_check(true);
    }

    let max_w = ui.available_width().min(1160.0);

    ui.vertical(|ui| {
        ui.set_max_width(max_w);
        ui.set_min_width(max_w);

            if app.stats.listens.is_none() {
                page_title(ui, "Stats");
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    widgets::loader(ui);
                    ui.add_space(8.0);
                    ui.label(RichText::new("Reading your listening…").font(theme::regular(14.0)).color(theme::text_dim()));
                });
                return;
            }

            let has_history = app.stats.listens.as_ref().is_some_and(|l| !l.is_empty());
            let live_heard = app.listening.as_ref().is_some_and(|l| l.heard >= 1.0);
            if !has_history && !live_heard {
                page_title(ui, "Stats");
                ui.add_space(4.0);
                ui.label(RichText::new("What you've been listening to, in plain numbers").font(theme::regular(14.0)).color(theme::text_dim()));
                ui.add_space(20.0);
                welcome_card(ui, app.accent_color());
                ui.add_space(28.0);
                manage_section(app, ui, None, false);
                return;
            }

            let (summary, earliest, offset) = app.stats_summary();
            stats_header(app, ui, &summary, earliest, offset);
            ui.add_space(22.0);

            if summary.seconds == 0 {
                empty_period(app, ui, &summary);
            } else {
                stats_hero(app, ui, &summary);
                ui.add_space(18.0);
                number_tiles(ui, &summary, app.accent_color());

                if app.stats.undo.is_some() {
                    ui.add_space(14.0);
                    undo_bar(app, ui);
                }

                ui.add_space(28.0);
                top_lists(app, ui, &summary);

                ui.add_space(28.0);
                when_you_listen(ui, &summary, app.accent_color());

                if summary.period != Period::All && !summary.discoveries.is_empty() {
                    ui.add_space(28.0);
                    discoveries_section(app, ui, &summary);
                }

                ui.add_space(32.0);
                wrapped_section(app, ui, &summary);
            }

            ui.add_space(32.0);
            manage_section(app, ui, Some(&summary), has_history);
        });
}

fn card_fill() -> Color32 {
    if theme::light() {
        theme::glass(0.045)
    } else {
        theme::glass(0.05)
    }
}

fn card_stroke() -> Stroke {
    Stroke::new(1.0, theme::glass(0.10))
}

fn stats_header(app: &mut App, ui: &mut Ui, summary: &Summary, earliest: Option<u64>, offset: i64) {
    let accent = app.accent_color();
    page_title(ui, "Stats");
    ui.add_space(2.0);
    ui.label(RichText::new("What you've been listening to, in plain numbers").font(theme::regular(14.0)).color(theme::text_dim()));
    ui.add_space(18.0);

    let period = app.stats.period;
    let back = app.stats.back;
    let can_go_back = period != Period::All && earliest.is_some_and(|e| e < period.span(back + 1, unix_now(), offset).end);
    let can_go_ahead = back > 0;

    let draw_switcher = |ui: &mut Ui, app: &mut App| {
        egui::Frame::new()
            .fill(if theme::light() { theme::glass(0.07) } else { theme::shade(0.32) })
            .stroke(card_stroke())
            .corner_radius(CornerRadius::same(10))
            .inner_margin(egui::Margin::same(3))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 2.0;
                    for kind in Period::EVERY {
                        let on = kind == period;
                        let font = if on { theme::semibold(13.0) } else { theme::regular(13.0) };
                        let g = ui.painter().layout_no_wrap(kind.tab_name().to_owned(), font.clone(), Color32::WHITE);
                        let (r, resp) = ui.allocate_exact_size(vec2(g.size().x + 30.0, 32.0), Sense::click());
                        let resp = named(resp, kind.tab_name()).on_hover_cursor(egui::CursorIcon::PointingHand);
                        let h = theme::anim_bool(ui.ctx(), resp.id.with("h"), resp.hovered(), 0.14);
                        if on {
                            ui.painter().rect_filled(r, 7.0, accent);
                        } else if h > 0.0 {
                            ui.painter().rect_filled(r, 7.0, theme::glass(0.10 * h));
                        }
                        let fg = if on {
                            widgets::contrast(accent)
                        } else {
                            theme::lerp_color(theme::text_dim(), theme::text(), h)
                        };
                        ui.painter().text(r.center(), Align2::CENTER_CENTER, kind.tab_name(), font, fg);
                        if resp.clicked() && !on {
                            app.stats.period = kind;
                            app.stats.back = 0;
                            *app.stats.save_note.lock().unwrap() = None;
                        }
                    }
                });
            });
    };

    let draw_nav = |ui: &mut Ui, app: &mut App| {
        if period == Period::All {
            return;
        }
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 8.0;
            if circle_arrow(ui, "stats-back", icon::CHEVRON_LEFT, "Previous period", can_go_back).clicked() && can_go_back {
                app.stats.back += 1;
                *app.stats.save_note.lock().unwrap() = None;
            }
            let (lr, _) = ui.allocate_exact_size(vec2(156.0, 32.0), Sense::hover());
            ui.painter().text(lr.center(), Align2::CENTER_CENTER, &summary.label, theme::semibold(15.0), theme::text());
            if circle_arrow(ui, "stats-ahead", icon::CHEVRON_RIGHT, "Next period", can_go_ahead).clicked() && can_go_ahead {
                app.stats.back = app.stats.back.saturating_sub(1);
                *app.stats.save_note.lock().unwrap() = None;
            }
            if back > 0 {
                let g = ui.painter().layout_no_wrap("Back to now".into(), theme::semibold(13.0), accent);
                let (r, resp) = ui.allocate_exact_size(vec2(g.size().x + 8.0, 32.0), Sense::click());
                let resp = named(resp, "Back to now").on_hover_cursor(egui::CursorIcon::PointingHand);
                ui.painter().text(r.center(), Align2::CENTER_CENTER, "Back to now", theme::semibold(13.0), accent);
                if resp.hovered() {
                    ui.painter().hline(r.shrink2(vec2(4.0, 0.0)).x_range(), r.bottom() - 6.0, Stroke::new(1.0, accent));
                }
                if resp.clicked() {
                    app.stats.back = 0;
                    *app.stats.save_note.lock().unwrap() = None;
                }
            }
        });
    };

    if ui.available_width() >= 640.0 {
        ui.horizontal(|ui| {
            draw_switcher(ui, app);
            if period != Period::All {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.spacing_mut().item_spacing.x = 8.0;
                    if back > 0 {
                        let g = ui.painter().layout_no_wrap("Back to now".into(), theme::semibold(13.0), accent);
                        let (r, resp) = ui.allocate_exact_size(vec2(g.size().x + 8.0, 32.0), Sense::click());
                        let resp = named(resp, "Back to now").on_hover_cursor(egui::CursorIcon::PointingHand);
                        ui.painter().text(r.center(), Align2::CENTER_CENTER, "Back to now", theme::semibold(13.0), accent);
                        if resp.hovered() {
                            ui.painter().hline(r.shrink2(vec2(4.0, 0.0)).x_range(), r.bottom() - 6.0, Stroke::new(1.0, accent));
                        }
                        if resp.clicked() {
                            app.stats.back = 0;
                            *app.stats.save_note.lock().unwrap() = None;
                        }
                    }
                    if circle_arrow(ui, "stats-ahead", icon::CHEVRON_RIGHT, "Next period", can_go_ahead).clicked() && can_go_ahead {
                        app.stats.back = app.stats.back.saturating_sub(1);
                        *app.stats.save_note.lock().unwrap() = None;
                    }
                    let (lr, _) = ui.allocate_exact_size(vec2(156.0, 32.0), Sense::hover());
                    ui.painter().text(lr.center(), Align2::CENTER_CENTER, &summary.label, theme::semibold(15.0), theme::text());
                    if circle_arrow(ui, "stats-back", icon::CHEVRON_LEFT, "Previous period", can_go_back).clicked() && can_go_back {
                        app.stats.back += 1;
                        *app.stats.save_note.lock().unwrap() = None;
                    }
                });
            }
        });
    } else {
        draw_switcher(ui, app);
        if period != Period::All {
            ui.add_space(10.0);
            draw_nav(ui, app);
        }
    }
}

fn circle_arrow(ui: &mut Ui, id: &str, glyph: char, name: &str, enabled: bool) -> egui::Response {
    let (r, resp) = ui.allocate_exact_size(Vec2::splat(32.0), if enabled { Sense::click() } else { Sense::hover() });
    let resp = named(resp, name);
    let resp = if enabled { resp.on_hover_cursor(egui::CursorIcon::PointingHand) } else { resp };
    let h = if enabled { theme::anim_bool(ui.ctx(), egui::Id::unique(id), resp.hovered(), 0.14) } else { 0.0 };
    ui.painter().circle_filled(r.center(), 16.0, theme::glass(0.05 + 0.08 * h));
    ui.painter().circle_stroke(r.center(), 15.5, card_stroke());
    let fg = if enabled {
        theme::lerp_color(theme::text_dim(), theme::text(), h)
    } else {
        theme::with_alpha(theme::text_faint(), 0.35)
    };
    icon_at(ui, r.center(), glyph, 15.0, fg);
    resp
}

fn stats_hero(app: &mut App, ui: &mut Ui, s: &Summary) {
    let accent = app.accent_color();
    let top = s.top_songs.first();
    let queue: Vec<Track> = s.top_songs.iter().filter(|x| !is_local_id(&x.video_id)).map(song_track).collect();

    let mut facts = format!("That's {} by {}.", plural(s.plays as usize, "song", "songs"), plural(s.artists as usize, "artist", "artists"));
    if let Some(c) = comparison(s) {
        facts.push(' ');
        facts.push_str(&c);
    }

    let rect_before = ui.painter().add(egui::Shape::Noop);
    let border_before = ui.painter().add(egui::Shape::Noop);

    let inner = egui::Frame::new().inner_margin(egui::Margin::same(24)).show(ui, |ui| {
        ui.set_min_width((ui.available_width()).max(200.0));
        ui.horizontal_top(|ui| {
            ui.spacing_mut().item_spacing.x = 24.0;
            // Left: #1 cover art (136x136)
            let (art_r, _) = ui.allocate_exact_size(Vec2::splat(136.0), Sense::hover());
            if let Some(thumb) = top.and_then(|x| x.thumbnail.as_deref()) {
                widgets::soft_shadow(ui, art_r, 14.0, 22.0, 0.32);
                paint_remote(ui, &sized(thumb, 320), art_r, 14.0, thumb_uv(thumb), Color32::WHITE);
            } else {
                ui.painter().rect_filled(art_r, 14.0, theme::glass(0.09));
                icon_at(ui, art_r.center(), icon::MUSIC, 40.0, theme::text_faint());
            }

            // Right: copy + button
            ui.vertical(|ui| {
                ui.spacing_mut().item_spacing.y = 6.0;
                ui.label(RichText::new(s.label.to_uppercase()).font(theme::semibold(12.0)).color(accent));
                ui.label(RichText::new(headline(s, app.stats.back)).font(theme::bold(28.0)).color(theme::text()));
                ui.label(RichText::new(&facts).font(theme::regular(14.0)).color(theme::text_dim()));
                if let Some(song) = top {
                    let line = format!(
                        "Your #1 song is \u{201c}{}\u{201d} by {} · {}",
                        song.title,
                        song.artist,
                        plural(song.plays as usize, "play", "plays")
                    );
                    ui.label(RichText::new(line).font(theme::regular(14.0)).color(theme::text()));
                }
                if s.plays < 5 {
                    ui.label(
                        RichText::new("Just getting started: the more you listen, the fuller this gets.")
                            .font(theme::regular(12.0))
                            .color(theme::text_faint()),
                    );
                }
                if !queue.is_empty() {
                    ui.add_space(6.0);
                    if accent_pill(ui, "Play your top songs", Some(icon::PLAY), accent).clicked() {
                        app.actions.push(Action::Play(queue, 0));
                    }
                }
            });
        });
    });

    let r = inner.response.rect;
    // Paint subtle accent gradient wash underneath the hero content.
    let left_c = theme::with_alpha(accent, if theme::light() { 0.14 } else { 0.20 });
    let right_c = theme::glass(if theme::light() { 0.03 } else { 0.04 });
    let mut mesh = egui::Mesh::default();
    let mut tess = egui::epaint::Tessellator::new(ui.ctx().pixels_per_point(), ui.ctx().tessellation_options(|o| *o), [1, 1], Vec::new());
    tess.tessellate_shape(egui::epaint::RectShape::filled(r, CornerRadius::same(16), Color32::WHITE).into(), &mut mesh);
    for v in &mut mesh.vertices {
        let t = ((v.pos.x - r.left()) / r.width().max(1.0)).clamp(0.0, 1.0);
        let a = v.color.a() as f32 / 255.0;
        v.color = theme::lerp_color(left_c, right_c, t).gamma_multiply(a);
    }
    ui.painter().set(rect_before, egui::Shape::mesh(mesh));
    ui.painter().set(
        border_before,
        egui::epaint::RectShape::stroke(r, CornerRadius::same(16), Stroke::new(1.0, theme::with_alpha(accent, 0.32)), StrokeKind::Inside),
    );
}

fn accent_pill(ui: &mut Ui, label: &str, glyph: Option<char>, accent: Color32) -> egui::Response {
    let font = theme::semibold(13.5);
    let g = ui.painter().layout_no_wrap(label.to_owned(), font.clone(), Color32::WHITE);
    let icon_w = if glyph.is_some() { 20.0 } else { 0.0 };
    let (r, resp) = ui.allocate_exact_size(vec2(g.size().x + icon_w + 32.0, 36.0), Sense::click());
    let resp = named(resp, label).on_hover_cursor(egui::CursorIcon::PointingHand);
    let h = theme::anim_bool(ui.ctx(), resp.id.with("h"), resp.hovered(), 0.14);
    let fill = theme::lerp_color(accent, Color32::WHITE, 0.08 * h);
    let fg = widgets::contrast(accent);
    ui.painter().rect_filled(r, r.height() / 2.0, fill);
    let mut x = r.left() + 16.0;
    if let Some(ch) = glyph {
        icon_at(ui, pos2(x + 6.0, r.center().y), ch, 14.0, fg);
        x += icon_w;
    }
    ui.painter().text(pos2(x, r.center().y), Align2::LEFT_CENTER, label, font, fg);
    resp
}

fn danger_pill(ui: &mut Ui, label: &str) -> egui::Response {
    let bg = Color32::from_rgb(0xc4, 0x2b, 0x1c);
    let font = theme::semibold(13.0);
    let g = ui.painter().layout_no_wrap(label.to_owned(), font.clone(), Color32::WHITE);
    let (r, resp) = ui.allocate_exact_size(vec2(g.size().x + 48.0, 34.0), Sense::click());
    let resp = named(resp, label).on_hover_cursor(egui::CursorIcon::PointingHand);
    let h = theme::anim_bool(ui.ctx(), resp.id.with("h"), resp.hovered(), 0.14);
    ui.painter().rect_filled(r, r.height() / 2.0, theme::lerp_color(bg, Color32::from_rgb(0xd8, 0x3b, 0x2c), h));
    icon_at(ui, pos2(r.left() + 20.0, r.center().y), icon::TRASH_2, 13.0, Color32::WHITE);
    ui.painter().text(pos2(r.left() + 34.0, r.center().y), Align2::LEFT_CENTER, label, font, Color32::WHITE);
    resp
}

fn number_tiles(ui: &mut Ui, s: &Summary, accent: Color32) {
    let busiest = s.busiest_day.as_ref().map(|b| format!("Most on {} ({} min)", b.date.label(), grouped(b.minutes)));
    let tiles = [
        (icon::HISTORY, "Minutes listened", grouped(s.minutes), busiest),
        (icon::MUSIC, "Songs played", grouped(s.plays), Some(format!("{} different", grouped(s.songs)))),
        (
            icon::USER,
            "Artists",
            grouped(s.artists),
            (s.new_artists > 0 && s.period != Period::All).then(|| format!("{} new to you", grouped(s.new_artists))),
        ),
        (icon::SPARKLES, "Days in a row", s.current_streak.to_string(), Some(format!("Longest: {}", plural(s.longest_streak as usize, "day", "days")))),
    ];

    let gap = 14.0;
    let avail = ui.available_width();
    let cols = if avail >= 740.0 { 4 } else { 2 };
    let tile_w = ((avail - gap * (cols - 1) as f32) / cols as f32).floor();

    for row in tiles.chunks(cols) {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = gap;
            for (glyph, label, value, note) in row {
                let (r, _) = ui.allocate_exact_size(vec2(tile_w, 114.0), Sense::hover());
                ui.painter().rect_filled(r, 14.0, card_fill());
                ui.painter().rect_stroke(r, 14.0, card_stroke(), StrokeKind::Inside);

                let pad = 18.0;
                icon_at(ui, pos2(r.left() + pad + 7.0, r.top() + pad + 8.0), *glyph, 14.0, accent);
                text_at(ui, pos2(r.left() + pad + 22.0, r.top() + pad + 1.0), label, theme::regular(13.0), theme::text_dim(), tile_w - pad * 2.0 - 22.0);
                text_at(ui, pos2(r.left() + pad, r.top() + pad + 26.0), value, theme::bold(30.0), theme::text(), tile_w - pad * 2.0);
                if let Some(n) = note {
                    text_at(ui, pos2(r.left() + pad, r.bottom() - pad - 15.0), n, theme::regular(12.0), theme::text_faint(), tile_w - pad * 2.0);
                }
            }
        });
        if cols == 2 {
            ui.add_space(gap);
        }
    }
}

fn undo_bar(app: &mut App, ui: &mut Ui) {
    let Some(undo) = app.stats.undo.clone() else { return };
    let what = match &undo {
        UndoHide::Song { title, .. } => format!("\u{201c}{title}\u{201d} is hidden from your stats"),
        UndoHide::Artist(name) => format!("{name} is hidden from your stats, with their songs"),
    };
    let accent = app.accent_color();
    egui::Frame::new()
        .fill(theme::glass(0.08))
        .stroke(card_stroke())
        .corner_radius(CornerRadius::same(10))
        .inner_margin(egui::Margin::symmetric(16, 10))
        .show(ui, |ui| {
            ui.set_min_width((ui.available_width()).max(100.0));
            ui.horizontal(|ui| {
                ui.label(RichText::new(what).font(theme::regular(13.0)).color(theme::text()));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if widgets::icon_button(ui, icon::X, 14.0, "Dismiss", false).clicked() {
                        app.stats.undo = None;
                    }
                    let (r, resp) = ui.allocate_exact_size(vec2(56.0, 28.0), Sense::click());
                    let resp = named(resp, "Undo").on_hover_cursor(egui::CursorIcon::PointingHand);
                    if resp.hovered() {
                        ui.painter().rect_filled(r, 14.0, theme::glass(0.10));
                    }
                    ui.painter().text(r.center(), Align2::CENTER_CENTER, "Undo", theme::semibold(13.0), accent);
                    if resp.clicked() {
                        match &undo {
                            UndoHide::Song { id, .. } => {
                                app.stats.hidden.show_song(id);
                            }
                            UndoHide::Artist(name) => {
                                app.stats.hidden.show_artist(name);
                            }
                        }
                        app.stats.undo = None;
                        app.stats_hidden_changed();
                    }
                });
            });
        });
}

fn top_lists(app: &mut App, ui: &mut Ui, s: &Summary) {
    let avail = ui.available_width();
    let wide = avail >= 740.0;
    if wide {
        let gap = 32.0;
        let right_w = ((avail - gap) * 0.42).clamp(280.0, 400.0);
        let left_w = (avail - gap - right_w).max(320.0);
        ui.horizontal_top(|ui| {
            ui.spacing_mut().item_spacing.x = gap;
            ui.allocate_ui_with_layout(vec2(left_w, 0.0), egui::Layout::top_down(egui::Align::Min), |ui| {
                ui.set_min_width(left_w);
                ui.set_max_width(left_w);
                top_songs_col(app, ui, s);
            });
            ui.allocate_ui_with_layout(vec2(right_w, 0.0), egui::Layout::top_down(egui::Align::Min), |ui| {
                ui.set_min_width(right_w);
                ui.set_max_width(right_w);
                top_artists_col(app, ui, s);
            });
        });
    } else {
        top_songs_col(app, ui, s);
        ui.add_space(24.0);
        top_artists_col(app, ui, s);
    }
}

fn section_heading(ui: &mut Ui, title: &str, hint: &str, toggle: Option<(&str, Color32)>) -> bool {
    let mut clicked = false;
    ui.horizontal(|ui| {
        ui.vertical(|ui| {
            ui.spacing_mut().item_spacing.y = 2.0;
            ui.label(RichText::new(title).font(theme::bold(20.0)).color(theme::text()));
            ui.label(RichText::new(hint).font(theme::regular(12.0)).color(theme::text_faint()));
        });
        if let Some((label, accent)) = toggle {
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Max), |ui| {
                let g = ui.painter().layout_no_wrap(label.to_owned(), theme::semibold(13.0), accent);
                let (r, resp) = ui.allocate_exact_size(vec2(g.size().x + 4.0, 24.0), Sense::click());
                let resp = named(resp, label).on_hover_cursor(egui::CursorIcon::PointingHand);
                ui.painter().text(r.center(), Align2::CENTER_CENTER, label, theme::semibold(13.0), accent);
                if resp.hovered() {
                    ui.painter().hline(r.x_range(), r.bottom() - 3.0, Stroke::new(1.0, accent));
                }
                clicked = resp.clicked();
            });
        }
    });
    clicked
}

fn top_songs_col(app: &mut App, ui: &mut Ui, s: &Summary) {
    let accent = app.accent_color();
    let toggle = (s.top_songs.len() > SHORT_LIST).then_some((if app.stats.expanded { "Show fewer" } else { "Show top 10" }, accent));
    if section_heading(ui, "Top songs", "Played the most. Click one to play it.", toggle) {
        app.stats.expanded = !app.stats.expanded;
    }
    ui.add_space(10.0);

    let count = if app.stats.expanded { stats::TOP } else { SHORT_LIST };
    let queue: Vec<Track> = s.top_songs.iter().filter(|x| !is_local_id(&x.video_id)).map(song_track).collect();
    let mut to_hide: Option<SongStat> = None;

    for (n, song) in s.top_songs.iter().enumerate().take(count) {
        let w = ui.available_width();
        let (r, row_resp) = ui.allocate_exact_size(vec2(w, 58.0), Sense::click());
        let hide_w = 60.0;
        let hide_rect = Rect::from_center_size(pos2(r.right() - hide_w / 2.0 - 8.0, r.center().y), vec2(hide_w, 26.0));
        let ptr_in_row = ui.input(|i| i.pointer.hover_pos().is_some_and(|p| r.contains(p)));
        let h = theme::anim_bool(ui.ctx(), row_resp.id.with("h"), ptr_in_row, 0.14);

        if h > 0.0 {
            ui.painter().rect_filled(r, 8.0, theme::glass(0.06 * h));
        }

        // Rank
        let rank_color = if n == 0 { accent } else { theme::text_faint() };
        ui.painter().text(pos2(r.left() + 26.0, r.center().y), Align2::RIGHT_CENTER, format!("{}", n + 1), theme::bold(14.0), rank_color);

        // Cover
        let art_r = Rect::from_center_size(pos2(r.left() + 60.0, r.center().y), Vec2::splat(44.0));
        if let Some(thumb) = song.thumbnail.as_deref() {
            paint_remote(ui, &sized(thumb, 120), art_r, 6.0, thumb_uv(thumb), Color32::WHITE);
        } else {
            ui.painter().rect_filled(art_r, 6.0, theme::glass(0.08));
            icon_at(ui, art_r.center(), icon::MUSIC, 16.0, theme::text_faint());
        }

        // Right side: plays count (shifts left on hover to make room for Hide)
        let plays_text = plural(song.plays as usize, "play", "plays");
        let plays_g = ui.painter().layout_no_wrap(plays_text.clone(), theme::regular(12.0), theme::text_dim());
        let right_edge = r.right() - 12.0 - (hide_w + 8.0) * h;
        ui.painter().text(pos2(right_edge, r.center().y), Align2::RIGHT_CENTER, plays_text, theme::regular(12.0), theme::text_dim());

        // Title + Artist
        let text_x = art_r.right() + 12.0;
        let text_w = (right_edge - plays_g.size().x - 14.0 - text_x).max(40.0);
        text_at(ui, pos2(text_x, r.top() + 11.0), &song.title, theme::semibold(14.0), theme::text(), text_w);
        text_at(ui, pos2(text_x, r.top() + 31.0), &song.artist, theme::regular(12.0), theme::text_dim(), text_w);

        // Hover-revealed Hide button
        let mut hide_clicked = false;
        if h > 0.05 {
            let hresp = ui.interact(hide_rect, row_resp.id.with("hide"), Sense::click());
            let hresp = named(hresp, "Hide song from stats").on_hover_cursor(egui::CursorIcon::PointingHand);
            let bh = theme::anim_bool(ui.ctx(), hresp.id.with("bh"), hresp.hovered(), 0.12);
            ui.painter().rect_filled(hide_rect, 13.0, theme::glass((0.09 + 0.09 * bh) * h));
            let fg = theme::with_alpha(theme::lerp_color(theme::text_dim(), theme::text(), bh), h);
            ui.painter().text(hide_rect.center(), Align2::CENTER_CENTER, "Hide", theme::semibold(11.5), fg);
            hide_clicked = hresp.clicked();
        }

        let row_resp = named(row_resp, &song.title).on_hover_cursor(egui::CursorIcon::PointingHand);
        let track = song_track(song);
        if !is_local_id(&song.video_id) {
            let liked = app.liked.contains(&track.id);
            row_resp.context_menu(|ui| song_menu(ui, &track, liked, None, &mut app.actions));
        }

        if hide_clicked {
            to_hide = Some(song.clone());
        } else if row_resp.clicked() {
            if is_local_id(&song.video_id) {
                let q = format!("{} {}", song.title, song.artist).trim().to_owned();
                if !q.is_empty() {
                    app.actions.push(Action::Go(Page::Search(q)));
                }
            } else if let Some(idx) = queue.iter().position(|t| t.id == song.video_id) {
                app.actions.push(Action::Play(queue.clone(), idx));
            }
        }
    }

    if let Some(song) = to_hide {
        app.hide_song(&song);
    }
}

fn top_artists_col(app: &mut App, ui: &mut Ui, s: &Summary) {
    let accent = app.accent_color();
    let toggle = (s.top_artists.len() > SHORT_LIST && s.top_songs.len() <= SHORT_LIST)
        .then_some((if app.stats.expanded { "Show fewer" } else { "Show top 10" }, accent));
    if section_heading(ui, "Top artists", "By minutes heard. Click one to find their songs.", toggle) {
        app.stats.expanded = !app.stats.expanded;
    }
    ui.add_space(10.0);

    let count = if app.stats.expanded { stats::TOP } else { SHORT_LIST };
    let most = s.top_artists.first().map_or(1, |a| a.minutes.max(1)) as f32;
    let mut to_hide: Option<String> = None;

    for (n, artist) in s.top_artists.iter().enumerate().take(count) {
        let w = ui.available_width();
        let (r, row_resp) = ui.allocate_exact_size(vec2(w, 58.0), Sense::click());
        let hide_w = 56.0;
        let hide_rect = Rect::from_center_size(pos2(r.right() - hide_w / 2.0 - 8.0, r.center().y), vec2(hide_w, 26.0));
        let ptr_in_row = ui.input(|i| i.pointer.hover_pos().is_some_and(|p| r.contains(p)));
        let h = theme::anim_bool(ui.ctx(), row_resp.id.with("h"), ptr_in_row, 0.14);

        if h > 0.0 {
            ui.painter().rect_filled(r, 8.0, theme::glass(0.06 * h));
        }

        let rank_color = if n == 0 { accent } else { theme::text_faint() };
        ui.painter().text(pos2(r.left() + 26.0, r.center().y), Align2::RIGHT_CENTER, format!("{}", n + 1), theme::bold(14.0), rank_color);

        let art_r = Rect::from_center_size(pos2(r.left() + 58.0, r.center().y), Vec2::splat(40.0));
        if let Some(thumb) = artist.thumbnail.as_deref() {
            paint_remote(ui, &sized(thumb, 120), art_r, 20.0, thumb_uv(thumb), Color32::WHITE);
        } else {
            ui.painter().circle_filled(art_r.center(), 20.0, theme::glass(0.08));
            icon_at(ui, art_r.center(), icon::USER, 16.0, theme::text_faint());
        }

        let bar_left = art_r.right() + 12.0;
        let bar_right = r.right() - 12.0 - (hide_w + 8.0) * h;
        let min_text = format!("{} min", grouped(artist.minutes));
        let min_g = ui.painter().layout_no_wrap(min_text.clone(), theme::regular(12.0), theme::text_dim());
        ui.painter().text(pos2(bar_right, r.top() + 19.0), Align2::RIGHT_CENTER, min_text, theme::regular(12.0), theme::text_dim());

        let name_w = (bar_right - min_g.size().x - 10.0 - bar_left).max(40.0);
        text_at(ui, pos2(bar_left, r.top() + 11.0), &artist.name, theme::semibold(14.0), theme::text(), name_w);

        // Progress bar
        let bar_rect = Rect::from_min_max(pos2(bar_left, r.top() + 36.0), pos2(bar_right.max(bar_left + 10.0), r.top() + 40.0));
        ui.painter().rect_filled(bar_rect, 2.0, theme::glass(0.10));
        let frac = (artist.minutes as f32 / most).clamp(0.02, 1.0);
        let fill_rect = Rect::from_min_size(bar_rect.min, vec2(bar_rect.width() * frac, bar_rect.height()));
        ui.painter().rect_filled(fill_rect, 2.0, accent);

        let mut hide_clicked = false;
        if h > 0.05 {
            let hresp = ui.interact(hide_rect, row_resp.id.with("hide"), Sense::click());
            let hresp = named(hresp, "Hide artist from stats").on_hover_cursor(egui::CursorIcon::PointingHand);
            let bh = theme::anim_bool(ui.ctx(), hresp.id.with("bh"), hresp.hovered(), 0.12);
            ui.painter().rect_filled(hide_rect, 13.0, theme::glass((0.09 + 0.09 * bh) * h));
            let fg = theme::with_alpha(theme::lerp_color(theme::text_dim(), theme::text(), bh), h);
            ui.painter().text(hide_rect.center(), Align2::CENTER_CENTER, "Hide", theme::semibold(11.5), fg);
            hide_clicked = hresp.clicked();
        }

        let row_resp = named(row_resp, &artist.name).on_hover_cursor(egui::CursorIcon::PointingHand);
        if hide_clicked {
            to_hide = Some(artist.name.clone());
        } else if row_resp.clicked() {
            app.actions.push(Action::Go(Page::Search(artist.name.clone())));
        }
    }

    if let Some(name) = to_hide {
        app.hide_artist(&name);
    }
}

fn when_you_listen(ui: &mut Ui, s: &Summary, accent: Color32) {
    ui.label(RichText::new("When you listen").font(theme::bold(20.0)).color(theme::text()));
    ui.add_space(4.0);

    let mut words = Vec::new();
    if let Some(h) = s.favourite_hour() {
        let who = match h {
            5..=11 => "You're a morning listener",
            12..=16 => "You're an afternoon listener",
            17..=21 => "You're an evening listener",
            _ => "You're a night owl",
        };
        words.push(format!("{who}: most of your music plays around {:02}:00.", h));
    }
    if let Some(d) = s.favourite_weekday() {
        words.push(format!("{} is your biggest day of the week.", stats::weekday_name(d)));
    }
    if !words.is_empty() {
        ui.label(RichText::new(words.join(" ")).font(theme::regular(14.0)).color(theme::text_dim()));
    }
    ui.add_space(12.0);

    let avail = ui.available_width();
    let gap = 16.0;
    let wide = avail >= 720.0;
    let hours_labels: Vec<Option<String>> = (0..24).map(|h| (h % 6 == 0).then(|| format!("{h:02}:00"))).collect();
    let hours_tips: Vec<String> = (0..24).map(|h| format!("{h:02}:00 – {:02}:00: {:.0} min", (h + 1) % 24, s.by_hour[h])).collect();
    let days_labels: Vec<Option<String>> = (0..7).map(|d| Some(stats::weekday_name(d)[..3].to_string())).collect();
    let days_tips: Vec<String> = (0..7).map(|d| format!("{}: {:.0} min", stats::weekday_name(d), s.by_weekday[d])).collect();

    if wide {
        let day_w = 300.0;
        let hour_w = (avail - gap - day_w).max(320.0);
        ui.horizontal_top(|ui| {
            ui.spacing_mut().item_spacing.x = gap;
            bar_chart_card(ui, "By hour", &s.by_hour, &hours_labels, &hours_tips, hour_w, accent);
            bar_chart_card(ui, "By day", &s.by_weekday, &days_labels, &days_tips, day_w, accent);
        });
    } else {
        bar_chart_card(ui, "By hour", &s.by_hour, &hours_labels, &hours_tips, avail, accent);
        ui.add_space(gap);
        bar_chart_card(ui, "By day", &s.by_weekday, &days_labels, &days_tips, avail, accent);
    }
}

fn bar_chart_card(ui: &mut Ui, title: &str, values: &[f32], labels: &[Option<String>], tips: &[String], width: f32, accent: Color32) {
    let height = 160.0;
    let (r, _) = ui.allocate_exact_size(vec2(width, height), Sense::hover());
    ui.painter().rect_filled(r, 14.0, card_fill());
    ui.painter().rect_stroke(r, 14.0, card_stroke(), StrokeKind::Inside);

    let pad = 18.0;
    text_at(ui, pos2(r.left() + pad, r.top() + pad), title, theme::regular(12.0), theme::text_dim(), width - pad * 2.0);

    let chart = Rect::from_min_max(pos2(r.left() + pad, r.top() + pad + 26.0), pos2(r.right() - pad, r.bottom() - pad - 16.0));
    let n = values.len().max(1);
    let gap = 4.0;
    let bar_w = ((chart.width() - gap * (n - 1) as f32) / n as f32).max(2.0);
    let most = values.iter().copied().fold(0.0f32, f32::max).max(1.0);

    for (i, &v) in values.iter().enumerate() {
        let x0 = chart.left() + i as f32 * (bar_w + gap);
        let h = (v / most * chart.height()).max(2.0);
        let bar_r = Rect::from_min_max(pos2(x0, chart.bottom() - h), pos2(x0 + bar_w, chart.bottom()));
        let col_r = Rect::from_min_max(pos2(x0, chart.top()), pos2(x0 + bar_w, chart.bottom()));
        let resp = ui.interact(col_r, ui.scope_id().with((title, i)), Sense::hover());
        let is_peak = v > 0.0 && v >= most - 0.001;
        let fill = if is_peak || resp.hovered() {
            accent
        } else {
            theme::with_alpha(accent, 0.38)
        };
        ui.painter().rect_filled(bar_r, CornerRadius { nw: 3, ne: 3, sw: 0, se: 0 }, fill);
        if let Some(tip) = tips.get(i) {
            resp.on_hover_text(tip);
        }
        if let Some(Some(lbl)) = labels.get(i) {
            ui.painter().text(pos2(x0 + bar_w * 0.5, chart.bottom() + 10.0), Align2::CENTER_CENTER, lbl, theme::regular(10.0), theme::text_faint());
        }
    }
}

fn discoveries_section(app: &mut App, ui: &mut Ui, s: &Summary) {
    ui.label(RichText::new("New to you").font(theme::bold(20.0)).color(theme::text()));
    ui.add_space(2.0);
    ui.label(
        RichText::new(format!("{} you hadn't played before", plural(s.new_artists as usize, "artist", "artists")))
            .font(theme::regular(12.0))
            .color(theme::text_faint()),
    );
    ui.add_space(10.0);

    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = vec2(8.0, 8.0);
        for artist in &s.discoveries {
            let min_text = format!("{} min", grouped(artist.minutes));
            let g1 = ui.painter().layout_no_wrap(artist.name.clone(), theme::semibold(13.0), theme::text());
            let g2 = ui.painter().layout_no_wrap(min_text.clone(), theme::regular(13.0), theme::text_dim());
            let (r, resp) = ui.allocate_exact_size(vec2(g1.size().x + g2.size().x + 36.0, 34.0), Sense::click());
            let resp = named(resp, &artist.name).on_hover_cursor(egui::CursorIcon::PointingHand);
            let h = theme::anim_bool(ui.ctx(), resp.id.with("h"), resp.hovered(), 0.14);
            ui.painter().rect_filled(r, 17.0, theme::glass(0.05 + 0.08 * h));
            ui.painter().rect_stroke(r, 17.0, card_stroke(), StrokeKind::Inside);
            ui.painter().text(pos2(r.left() + 14.0, r.center().y), Align2::LEFT_CENTER, &artist.name, theme::semibold(13.0), theme::text());
            ui.painter().text(pos2(r.right() - 14.0, r.center().y), Align2::RIGHT_CENTER, &min_text, theme::regular(13.0), theme::text_dim());
            if resp.clicked() {
                app.actions.push(Action::Go(Page::Search(artist.name.clone())));
            }
        }
    });
}

fn empty_period(app: &mut App, ui: &mut Ui, s: &Summary) {
    let accent = app.accent_color();
    egui::Frame::new()
        .fill(card_fill())
        .stroke(card_stroke())
        .corner_radius(CornerRadius::same(16))
        .inner_margin(egui::Margin::symmetric(24, 48))
        .show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            ui.vertical_centered(|ui| {
                let (ir, _) = ui.allocate_exact_size(Vec2::splat(44.0), Sense::hover());
                icon_at(ui, ir.center(), icon::MUSIC, 34.0, theme::text_faint());
                ui.add_space(10.0);
                ui.label(RichText::new("No music here yet").font(theme::bold(22.0)).color(theme::text()));
                ui.add_space(4.0);
                ui.label(
                    RichText::new("Nothing was played in this stretch. Pick another one above, or see everything at once.")
                        .font(theme::regular(14.0))
                        .color(theme::text_dim()),
                );
                if s.period != Period::All {
                    ui.add_space(16.0);
                    if accent_pill(ui, "See all time", Some(icon::HISTORY), accent).clicked() {
                        app.stats.period = Period::All;
                        app.stats.back = 0;
                    }
                }
            });
        });
}

fn welcome_card(ui: &mut Ui, accent: Color32) {
    egui::Frame::new()
        .fill(card_fill())
        .stroke(card_stroke())
        .corner_radius(CornerRadius::same(16))
        .inner_margin(egui::Margin::symmetric(24, 52))
        .show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            ui.vertical_centered(|ui| {
                let (ir, _) = ui.allocate_exact_size(Vec2::splat(52.0), Sense::hover());
                ui.painter().circle_filled(ir.center(), 26.0, theme::with_alpha(accent, 0.18));
                icon_at(ui, ir.center(), icon::STATS, 24.0, accent);
                ui.add_space(12.0);
                ui.label(RichText::new("Your stats start here").font(theme::bold(24.0)).color(theme::text()));
                ui.add_space(6.0);
                ui.label(
                    RichText::new("Play some music and ytm-rs keeps count: your top songs and artists, when you listen, and a Wrapped card to share.")
                        .font(theme::regular(14.0))
                        .color(theme::text_dim()),
                );
                ui.add_space(6.0);
                ui.label(RichText::new("Synced with NabyTune — it all stays on this PC.").font(theme::regular(12.0)).color(theme::text_faint()));
            });
        });
}

// ------------------------------------------------------------------ Manage section

fn manage_section(app: &mut App, ui: &mut Ui, summary: Option<&Summary>, has_history: bool) {
    ui.label(RichText::new("Manage your stats").font(theme::bold(20.0)).color(theme::text()));
    ui.add_space(10.0);

    let (dir, synced_with_nabytune) = stats::stats_dir(app.backend.data_dir());
    let hidden_count = app.stats.hidden.len();
    let mut unhide_song: Option<String> = None;
    let mut unhide_artist: Option<String> = None;
    let mut do_clear: Option<(Option<Span>, String)> = None;
    let mut do_reload = false;
    let mut do_open_dir = false;

    settings_card(ui, |ui| {
        // Row 1: Synced storage with NabyTune
        let sync_title = if synced_with_nabytune { "Shared storage with NabyTune" } else { "Listening history file" };
        let sync_desc = if synced_with_nabytune {
            format!("Both ytm-rs and NabyTune read & write {}.", dir.join("listens.jsonl").display())
        } else {
            format!("Stored at {}.", dir.join("listens.jsonl").display())
        };
        setting_row_wide(ui, sync_title, Some(&sync_desc), 260.0, |ui| {
            ui.spacing_mut().item_spacing.x = 8.0;
            if pill(ui, "Sync now", None, false).clicked() {
                do_reload = true;
            }
            if pill(ui, "Open folder", Some(icon::FOLDER_OPEN), false).clicked() {
                do_open_dir = true;
            }
        });

        divider(ui);

        // Row 2: Hidden songs & artists
        setting_row(
            ui,
            "Hidden songs and artists",
            Some("Left out of everything here, minutes and Wrapped too. Hiding an artist hides songs they're on. Hover a song or artist above and press Hide."),
            |ui| {
                if hidden_count == 0 {
                    ui.label(RichText::new("None").font(theme::regular(13.0)).color(theme::text_faint()));
                } else {
                    let lbl = if app.stats.show_hidden { "Close".to_string() } else { format!("Show all {hidden_count}") };
                    let glyph = if app.stats.show_hidden { icon::CHEVRON_UP } else { icon::CHEVRON_DOWN };
                    if pill(ui, &lbl, Some(glyph), false).clicked() {
                        app.stats.show_hidden = !app.stats.show_hidden;
                    }
                }
            },
        );

        if app.stats.show_hidden && hidden_count > 0 {
            ui.add_space(4.0);
            for s in &app.stats.hidden.songs {
                let label = if s.artist.is_empty() { s.title.clone() } else { format!("{} — {}", s.title, s.artist) };
                ui.horizontal(|ui| {
                    ui.add_space(12.0);
                    let (ir, _) = ui.allocate_exact_size(Vec2::splat(20.0), Sense::hover());
                    icon_at(ui, ir.center(), icon::MUSIC, 13.0, theme::text_faint());
                    ui.label(RichText::new(label).font(theme::regular(13.0)).color(theme::text()));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if pill(ui, "Show again", Some(icon::CHECK), false).clicked() {
                            unhide_song = Some(s.id.clone());
                        }
                        ui.label(RichText::new("Song").font(theme::regular(12.0)).color(theme::text_faint()));
                    });
                });
                ui.add_space(4.0);
            }
            for a in &app.stats.hidden.artists {
                ui.horizontal(|ui| {
                    ui.add_space(12.0);
                    let (ir, _) = ui.allocate_exact_size(Vec2::splat(20.0), Sense::hover());
                    icon_at(ui, ir.center(), icon::USER, 13.0, theme::text_faint());
                    ui.label(RichText::new(a).font(theme::regular(13.0)).color(theme::text()));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if pill(ui, "Show again", Some(icon::CHECK), false).clicked() {
                            unhide_artist = Some(a.clone());
                        }
                        ui.label(RichText::new("Artist").font(theme::regular(12.0)).color(theme::text_faint()));
                    });
                });
                ui.add_space(4.0);
            }
            ui.add_space(6.0);
        }

        // Row 3: Clear listening history
        if has_history {
            divider(ui);
            let armed = app.stats.clearing;
            setting_row(
                ui,
                "Clear listening history",
                Some("Start your stats and Wrapped over. Your playlists, likes and songs stay as they are."),
                |ui| {
                    let lbl = if armed { "Cancel" } else { "Clear…" };
                    if pill(ui, lbl, Some(icon::TRASH_2), false).clicked() {
                        app.stats.clearing = !app.stats.clearing;
                        app.stats.data_note = None;
                    }
                },
            );

            if armed {
                ui.add_space(6.0);
                egui::Frame::new()
                    .fill(Color32::from_rgba_unmultiplied(0xc4, 0x2b, 0x1c, 34))
                    .stroke(Stroke::new(1.0, Color32::from_rgba_unmultiplied(0xc4, 0x2b, 0x1c, 110)))
                    .corner_radius(CornerRadius::same(10))
                    .inner_margin(egui::Margin::same(14))
                    .show(ui, |ui| {
                        ui.set_min_width(ui.available_width());
                        ui.label(RichText::new("Clear your listening history for good?").font(theme::semibold(14.0)).color(theme::text()));
                        ui.label(
                            RichText::new("This can't be undone and clears the shared log for both apps. Hidden songs and artists stay hidden.")
                                .font(theme::regular(12.0))
                                .color(theme::text_dim()),
                        );
                        ui.add_space(10.0);
                        ui.horizontal_wrapped(|ui| {
                            ui.spacing_mut().item_spacing = vec2(8.0, 8.0);
                            if let Some(s) = summary.filter(|_| app.stats.period != Period::All) {
                                let btn_lbl = clear_label(app.stats.period, app.stats.back, &s.label);
                                if danger_pill(ui, &btn_lbl).clicked() {
                                    do_clear = Some((Some(s.span), s.label.clone()));
                                }
                            }
                            if danger_pill(ui, "Clear everything").clicked() {
                                do_clear = Some((None, String::new()));
                            }
                            if pill(ui, "Cancel", Some(icon::X), false).clicked() {
                                app.stats.clearing = false;
                            }
                        });
                    });
                ui.add_space(4.0);
            }
        }
    });

    if do_open_dir {
        let _ = std::fs::create_dir_all(&dir);
        #[cfg(windows)]
        let _ = std::process::Command::new("explorer").arg(&dir).spawn();
        #[cfg(target_os = "macos")]
        let _ = std::process::Command::new("open").arg(&dir).spawn();
        #[cfg(all(unix, not(target_os = "macos")))]
        let _ = std::process::Command::new("xdg-open").arg(&dir).spawn();
    }
    if do_reload {
        app.stats_sync_check(true);
        app.toast("Synced listening stats");
    }
    if let Some(id) = unhide_song {
        app.stats.hidden.show_song(&id);
        app.stats.undo = None;
        app.stats_hidden_changed();
        if app.stats.hidden.is_empty() {
            app.stats.show_hidden = false;
        }
    }
    if let Some(name) = unhide_artist {
        app.stats.hidden.show_artist(&name);
        app.stats.undo = None;
        app.stats_hidden_changed();
        if app.stats.hidden.is_empty() {
            app.stats.show_hidden = false;
        }
    }
    if let Some((span, label)) = do_clear {
        app.clear_stats_history(span, label);
    }

    if let Some(note) = &app.stats.data_note {
        ui.add_space(8.0);
        ui.label(RichText::new(note).font(theme::regular(12.0)).color(theme::text_dim()));
    }
    ui.add_space(8.0);
    ui.label(
        RichText::new("How it's counted: a song counts as played once you've heard 30 seconds of it (or half, for a short one). Minutes count everything you heard. It all stays on this PC.")
            .font(theme::regular(12.0))
            .color(theme::text_faint()),
    );
}

// ------------------------------------------------------------------ Wrapped section & 10 card styles

fn wrapped_section(app: &mut App, ui: &mut Ui, s: &Summary) {
    let accent = app.accent_color();
    ui.label(RichText::new("Your Wrapped").font(theme::bold(20.0)).color(theme::text()));
    ui.add_space(2.0);
    ui.label(
        RichText::new("Your stretch on one card. Pick a style, then save it as a picture to share.")
            .font(theme::regular(12.0))
            .color(theme::text_faint()),
    );
    ui.add_space(16.0);

    let data = CardData::from_summary(s, accent);
    let style = app.stats.style;
    let wide = ui.available_width() >= CARD_W + 280.0;
    let mut save_clicked = false;
    let mut picked_style: Option<WrappedStyle> = None;

    let draw_left = |ui: &mut Ui, card_rect_out: &mut Option<Rect>, save_clicked: &mut bool, save_note: &Option<String>, last_saved: &Option<PathBuf>| {
        ui.vertical(|ui| {
            let (rect, _) = ui.allocate_exact_size(vec2(CARD_W, CARD_H), Sense::hover());
            *card_rect_out = Some(rect);
            widgets::soft_shadow(ui, rect, 2.0, 28.0, 0.35);
            draw_wrapped_card(ui, rect, style, &data);

            ui.add_space(14.0);
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing = vec2(10.0, 8.0);
                if accent_pill(ui, "Save as picture", Some(icon::DOWNLOAD), accent).clicked() {
                    *save_clicked = true;
                }
                if let Some(path) = last_saved {
                    if pill(ui, "Open folder", Some(icon::FOLDER_OPEN), false).clicked() {
                        crate::downloads::show_file(path);
                    }
                    if pill(ui, "Open image", None, false).clicked() {
                        open_saved_image(path);
                    }
                }
            });
            if let Some(note) = save_note {
                ui.add_space(6.0);
                ui.label(RichText::new(note).font(theme::regular(12.0)).color(theme::text_dim()));
            }
        });
    };

    let draw_picker = |ui: &mut Ui, picked: &mut Option<WrappedStyle>| {
        ui.vertical(|ui| {
            ui.label(RichText::new("Card style").font(theme::semibold(14.0)).color(theme::text()));
            ui.add_space(10.0);
            for row in WrappedStyle::ALL.chunks(2) {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 8.0;
                    for &opt in row {
                        let on = opt == style;
                        let (r, resp) = ui.allocate_exact_size(vec2(118.0, 100.0), Sense::click());
                        let resp = named(resp, opt.name()).on_hover_cursor(egui::CursorIcon::PointingHand);
                        let h = theme::anim_bool(ui.ctx(), resp.id.with("h"), resp.hovered(), 0.14);
                        if h > 0.0 && !on {
                            ui.painter().rect_filled(r, 11.0, theme::glass(0.06 * h));
                        }
                        if on {
                            ui.painter().rect_stroke(r, 11.0, Stroke::new(2.0, accent), StrokeKind::Inside);
                        }
                        let sw = Rect::from_min_size(r.min + vec2(5.0, 5.0), vec2(108.0, 68.0));
                        draw_swatch(ui, sw, opt, accent);
                        let lbl_color = if on { theme::text() } else { theme::text_dim() };
                        let lbl_font = if on { theme::semibold(12.0) } else { theme::regular(12.0) };
                        ui.painter().text(pos2(r.center().x, sw.bottom() + 14.0), Align2::CENTER_CENTER, opt.name(), lbl_font, lbl_color);
                        if resp.clicked() {
                            *picked = Some(opt);
                        }
                    }
                });
                ui.add_space(6.0);
            }
            ui.add_space(6.0);
            ui.allocate_ui(vec2(244.0, 0.0), |ui| {
                ui.label(
                    RichText::new("Something on the card you'd rather not share? Hover it in Top songs or Top artists and press Hide.")
                        .font(theme::regular(12.0))
                        .color(theme::text_faint()),
                );
            });
        });
    };

    let save_note = app.stats.save_note.lock().unwrap().clone();
    let last_saved = app.stats.last_saved.lock().unwrap().clone();
    let mut card_rect = None;
    if wide {
        ui.horizontal_top(|ui| {
            ui.spacing_mut().item_spacing.x = 32.0;
            draw_left(ui, &mut card_rect, &mut save_clicked, &save_note, &last_saved);
            draw_picker(ui, &mut picked_style);
        });
    } else {
        draw_left(ui, &mut card_rect, &mut save_clicked, &save_note, &last_saved);
        ui.add_space(20.0);
        draw_picker(ui, &mut picked_style);
    }

    app.stats.card_rect = card_rect;
    if let Some(opt) = picked_style {
        app.stats.style = opt;
        *app.stats.save_note.lock().unwrap() = None;
        *app.stats.last_saved.lock().unwrap() = None;
    }
    if save_clicked {
        app.save_wrapped_png(ui.ctx(), &s.label);
    }
    if let Some(label) = app.stats.capture_overlay.take() {
        let screen = ui.ctx().content_rect();
        let origin = pos2(
            (screen.center().x - CARD_W * 0.5).clamp(screen.left(), (screen.right() - CARD_W).max(screen.left())),
            (screen.center().y - CARD_H * 0.5).clamp(screen.top(), (screen.bottom() - CARD_H).max(screen.top())),
        );
        let overlay_rect = Rect::from_min_size(origin, vec2(CARD_W, CARD_H));
        egui::Area::new(egui::Id::unique("wrapped-capture-overlay"))
            .order(egui::Order::Tooltip)
            .fixed_pos(origin)
            .interactable(false)
            .show(ui.ctx(), |ui| {
                let (rect, _) = ui.allocate_exact_size(vec2(CARD_W, CARD_H), Sense::hover());
                draw_wrapped_card(ui, rect, style, &data);
            });
        app.capture_wrapped_rect(ui.ctx(), overlay_rect, &label);
    }
}

struct CardData {
    label: String,
    minutes: u32,
    plays: u32,
    songs: u32,
    artists: u32,
    streak: u32,
    hour: Option<usize>,
    songs_top: Vec<SongStat>,
    artists_top: Vec<ArtistStat>,
    accent: Color32,
}

impl CardData {
    fn from_summary(s: &Summary, accent: Color32) -> Self {
        Self {
            label: s.label.clone(),
            minutes: s.minutes,
            plays: s.plays,
            songs: s.songs,
            artists: s.artists,
            streak: s.longest_streak,
            hour: s.favourite_hour(),
            songs_top: s.top_songs.iter().take(5).cloned().collect(),
            artists_top: s.top_artists.iter().take(5).cloned().collect(),
            accent,
        }
    }

    fn hour_text(&self) -> String {
        self.hour.map(|h| format!("{h:02}:00")).unwrap_or_else(|| "—".into())
    }

    fn seed(&self) -> u64 {
        (self.minutes as u64) << 20 ^ (self.plays as u64) << 8 ^ self.songs as u64 ^ 0x9e37_79b9
    }
}

fn rgb(hex: u32) -> Color32 {
    Color32::from_rgb(((hex >> 16) & 0xff) as u8, ((hex >> 8) & 0xff) as u8, (hex & 0xff) as u8)
}

fn fit_size(text: &str, width: f32, em: f32, max: f32) -> f32 {
    (width / (text.chars().count().max(1) as f32 * em)).min(max)
}

fn paint_soft_orb(painter: &egui::Painter, center: Pos2, radius: f32, color: Color32, alpha: f32) {
    for (scale, a) in [(1.0, 0.14), (0.75, 0.24), (0.5, 0.36), (0.28, 0.45)] {
        painter.circle_filled(center, radius * scale, theme::with_alpha(color, alpha * a));
    }
}

fn paint_barcode(painter: &egui::Painter, rect: Rect, seed: u64, color: Color32, bars: usize) {
    let mut x = seed | 1;
    let mut cx = rect.left();
    for _ in 0..bars {
        x = x.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1_442_695_040_888_963_407);
        let w = 1.0 + ((x >> 33) % 3) as f32;
        let gap = 1.0 + ((x >> 41) % 3) as f32;
        if cx + w > rect.right() {
            break;
        }
        painter.rect_filled(Rect::from_min_max(pos2(cx, rect.top()), pos2(cx + w, rect.bottom())), 0.0, color);
        cx += w + gap;
    }
}

fn paint_card_cover(ui: &Ui, thumb: Option<&str>, rect: Rect, radius: f32, px: u32, fallback_bg: Color32) {
    if let Some(u) = thumb {
        paint_remote(ui, &sized(u, px), rect, radius, thumb_uv(u), Color32::WHITE);
    } else {
        ui.painter().rect_filled(rect, radius, fallback_bg);
        icon_at(ui, rect.center(), icon::MUSIC, (rect.width() * 0.36).clamp(14.0, 44.0), Color32::from_white_alpha(120));
    }
}

/// Square-crop UV for remote thumbnails, trimming black bars on 4:3/16:9 YouTube video thumbs.
fn thumb_uv(url: &str) -> Rect {
    if url.contains("i.ytimg.com") {
        let wide = url.contains("maxresdefault") || url.contains("mqdefault") || url.contains("hq720");
        let (w, h) = if wide { (1280, 720) } else { (480, 360) };
        return Thumb { url: String::new(), width: w, height: h }.square_uv();
    }
    Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0))
}

fn draw_wrapped_card(ui: &Ui, r: Rect, style: WrappedStyle, d: &CardData) {
    let p = ui.painter().with_clip_rect(r);
    match style {
        WrappedStyle::Signature => card_signature(ui, &p, r, d),
        WrappedStyle::Poster => card_poster(ui, &p, r, d),
        WrappedStyle::Minimal => card_minimal(ui, &p, r, d),
        WrappedStyle::Receipt => card_receipt(ui, &p, r, d),
        WrappedStyle::Neon => card_neon(ui, &p, r, d),
        WrappedStyle::Aurora => card_aurora(ui, &p, r, d),
        WrappedStyle::Cassette => card_cassette(ui, &p, r, d),
        WrappedStyle::Polaroid => card_polaroid(ui, &p, r, d),
        WrappedStyle::Numbers => card_big_numbers(ui, &p, r, d),
        WrappedStyle::Magazine => card_magazine(ui, &p, r, d),
    }
}

/// 1. Signature: dark, lit by the accent colour.
fn card_signature(ui: &Ui, p: &egui::Painter, r: Rect, d: &CardData) {
    vgradient(p, r, rgb(0x1a1a1d), rgb(0x08080a));
    paint_soft_orb(p, pos2(r.right() - 40.0, r.top() + 20.0), 190.0, d.accent, 0.55);
    paint_soft_orb(p, pos2(r.left() + 20.0, r.bottom() - 20.0), 180.0, d.accent, 0.32);

    let pad = 28.0;
    let x = r.left() + pad;
    let w = r.width() - pad * 2.0;

    // Header
    icon_at(ui, pos2(x + 9.0, r.top() + pad + 9.0), icon::STATS, 16.0, d.accent);
    p.text(pos2(x + 26.0, r.top() + pad + 9.0), Align2::LEFT_CENTER, "YT Music", theme::semibold(15.0), Color32::WHITE);
    p.text(pos2(x + 98.0, r.top() + pad + 9.0), Align2::LEFT_CENTER, "Wrapped", theme::semibold(15.0), d.accent);
    p.text(pos2(r.right() - pad, r.top() + pad + 9.0), Align2::RIGHT_CENTER, &d.label, theme::regular(13.0), Color32::from_white_alpha(180));

    // Big minutes
    let mins = grouped(d.minutes);
    let msize = fit_size(&mins, 250.0, 0.56, 72.0);
    let mg = p.layout_no_wrap(mins.clone(), theme::bold(msize), d.accent);
    p.galley(pos2(x, r.top() + 72.0), mg.clone(), d.accent);
    p.text(pos2(x + mg.size().x + 12.0, r.top() + 72.0 + mg.size().y - 10.0), Align2::LEFT_BOTTOM, "minutes of music", theme::regular(15.0), Color32::from_white_alpha(205));

    // #1 Song
    let y_song = r.top() + 178.0;
    if let Some(top) = d.songs_top.first() {
        let cov = Rect::from_min_size(pos2(x, y_song), Vec2::splat(76.0));
        paint_card_cover(ui, top.thumbnail.as_deref(), cov, 10.0, 200, rgb(0x28282c));
        let tx = cov.right() + 16.0;
        let tw = (r.right() - pad - tx).max(40.0);
        p.text(pos2(tx, y_song + 4.0), Align2::LEFT_TOP, "YOUR SONG", theme::semibold(11.0), Color32::from_white_alpha(140));
        let tg = widgets::galley(ui, &top.title, theme::bold(19.0), Color32::WHITE, tw);
        p.galley(pos2(tx, y_song + 22.0), tg, Color32::WHITE);
        let sub = format!("{} · {}", top.artist, plural(top.plays as usize, "play", "plays"));
        let sg = widgets::galley(ui, &sub, theme::regular(13.0), Color32::from_white_alpha(180), tw);
        p.galley(pos2(tx, y_song + 49.0), sg, Color32::from_white_alpha(180));
    }

    // Top songs + Top artists columns
    let y_list = r.top() + 286.0;
    let col_gap = 24.0;
    let artist_w = 160.0;
    let song_w = w - artist_w - col_gap;
    p.text(pos2(x, y_list), Align2::LEFT_TOP, "TOP SONGS", theme::semibold(11.0), Color32::from_white_alpha(140));
    p.text(pos2(x + song_w + col_gap, y_list), Align2::LEFT_TOP, "TOP ARTISTS", theme::semibold(11.0), Color32::from_white_alpha(140));
    for (n, s) in d.songs_top.iter().enumerate() {
        let yy = y_list + 22.0 + n as f32 * 26.0;
        p.text(pos2(x, yy), Align2::LEFT_TOP, format!("{}", n + 1), theme::bold(13.0), d.accent);
        let g = widgets::galley(ui, &s.title, theme::regular(13.0), Color32::WHITE, song_w - 20.0);
        p.galley(pos2(x + 18.0, yy), g, Color32::WHITE);
    }
    for (n, a) in d.artists_top.iter().enumerate() {
        let yy = y_list + 22.0 + n as f32 * 26.0;
        let ax = x + song_w + col_gap;
        p.text(pos2(ax, yy), Align2::LEFT_TOP, format!("{}", n + 1), theme::bold(13.0), d.accent);
        let g = widgets::galley(ui, &a.name, theme::regular(13.0), Color32::WHITE, artist_w - 20.0);
        p.galley(pos2(ax + 18.0, yy), g, Color32::WHITE);
    }

    // Bottom 4 stats
    let y_bot = r.bottom() - pad - 44.0;
    let stats_row = [
        ("SONGS", grouped(d.songs)),
        ("ARTISTS", grouped(d.artists)),
        ("BEST HOUR", d.hour_text()),
        ("DAY STREAK", d.streak.to_string()),
    ];
    let step = w / 4.0;
    for (i, (k, v)) in stats_row.iter().enumerate() {
        let sx = x + i as f32 * step;
        p.text(pos2(sx, y_bot), Align2::LEFT_TOP, *k, theme::semibold(10.5), Color32::from_white_alpha(140));
        p.text(pos2(sx, y_bot + 16.0), Align2::LEFT_TOP, v, theme::bold(21.0), Color32::WHITE);
    }
}

/// 2. Poster: bold orange-red, huge uppercase condensed typography.
fn card_poster(ui: &Ui, p: &egui::Painter, r: Rect, d: &CardData) {
    let red = rgb(0xff4a1c);
    let ink = rgb(0x150d0a);
    let cream = rgb(0xfff1de);
    p.rect_filled(r, 0.0, red);
    let pad = 28.0;
    let x = r.left() + pad;
    let w = r.width() - pad * 2.0;

    p.text(pos2(x, r.top() + pad), Align2::LEFT_TOP, "YT MUSIC WRAPPED", theme::bold(15.0), ink);
    p.text(pos2(r.right() - pad, r.top() + pad), Align2::RIGHT_TOP, d.label.to_uppercase(), theme::bold(15.0), cream);

    let mins = grouped(d.minutes);
    let big = fit_size(&mins, w, 0.56, 120.0);
    p.text(pos2(x, r.top() + 56.0), Align2::LEFT_TOP, &mins, theme::bold(big), ink);
    p.text(pos2(x, r.top() + 56.0 + big + 2.0), Align2::LEFT_TOP, "MINUTES OF MUSIC", theme::bold(24.0), cream);

    let rule_y = r.top() + 234.0;
    p.rect_filled(Rect::from_min_size(pos2(x, rule_y), vec2(w, 7.0)), 0.0, ink);

    if let Some(top) = d.songs_top.first() {
        p.text(pos2(x, rule_y + 18.0), Align2::LEFT_TOP, "#1", theme::bold(16.0), cream);
        let tg = widgets::galley(ui, &top.title.to_uppercase(), theme::bold(32.0), ink, w);
        p.galley(pos2(x, rule_y + 38.0), tg, ink);
        let ag = widgets::galley(ui, &top.artist, theme::semibold(15.0), ink, w);
        p.galley(pos2(x, rule_y + 78.0), ag, ink);
    }

    let mut y = r.bottom() - pad - (d.artists_top.len() as f32 * 34.0);
    for (n, a) in d.artists_top.iter().enumerate() {
        p.text(pos2(x, y + 6.0), Align2::LEFT_TOP, format!("{:02}", n + 1), theme::bold(15.0), cream);
        let g = widgets::galley(ui, &a.name.to_uppercase(), theme::bold(24.0), ink, w - 34.0);
        p.galley(pos2(x + 32.0, y), g, ink);
        y += 34.0;
    }
}

/// 3. Minimal: warm paper, thin rules, crisp typography.
fn card_minimal(ui: &Ui, p: &egui::Painter, r: Rect, d: &CardData) {
    let paper = rgb(0xf5f4f0);
    let ink = rgb(0x161616);
    let soft = rgb(0x8b8a85);
    let rule = rgb(0xdcdad3);
    p.rect_filled(r, 0.0, paper);

    let pad = 38.0;
    let x = r.left() + pad;
    let w = r.width() - pad * 2.0;

    p.text(pos2(x, r.top() + pad), Align2::LEFT_TOP, "Wrapped", theme::semibold(14.0), ink);
    p.text(pos2(r.right() - pad, r.top() + pad), Align2::RIGHT_TOP, &d.label, theme::regular(13.0), soft);

    let mins = grouped(d.minutes);
    let sz = fit_size(&mins, w, 0.56, 84.0);
    p.text(pos2(x, r.top() + 94.0), Align2::LEFT_TOP, &mins, theme::weighted(crate::preset::Weight::Light, sz), ink);
    p.text(pos2(x, r.top() + 94.0 + sz + 8.0), Align2::LEFT_TOP, "minutes of music", theme::regular(15.0), soft);

    let list_top = r.top() + 286.0;
    p.text(pos2(x, list_top), Align2::LEFT_TOP, "TOP SONGS", FontId::monospace(10.5), soft);
    p.hline(x..=x + w, list_top + 18.0, Stroke::new(1.0, ink));

    for (n, s) in d.songs_top.iter().enumerate() {
        let yy = list_top + 20.0 + n as f32 * 34.0;
        p.text(pos2(x, yy + 10.0), Align2::LEFT_TOP, format!("{:02}", n + 1), FontId::monospace(11.5), soft);
        let ag = widgets::galley(ui, &s.artist, theme::regular(12.0), soft, 130.0);
        p.galley(pos2(x + w - ag.size().x, yy + 10.0), ag.clone(), soft);
        let tg = widgets::galley(ui, &s.title, theme::regular(13.5), ink, (w - 36.0 - ag.size().x).max(40.0));
        p.galley(pos2(x + 28.0, yy + 9.0), tg, ink);
        p.hline(x..=x + w, yy + 34.0, Stroke::new(1.0, rule));
    }

    let y_bot = r.bottom() - pad - 42.0;
    let items = [("SONGS", grouped(d.songs)), ("ARTISTS", grouped(d.artists)), ("BEST HOUR", d.hour_text())];
    for (i, (k, v)) in items.iter().enumerate() {
        let sx = x + i as f32 * (w / 3.0);
        p.text(pos2(sx, y_bot), Align2::LEFT_TOP, *k, FontId::monospace(10.0), soft);
        p.text(pos2(sx, y_bot + 16.0), Align2::LEFT_TOP, v, FontId::monospace(19.0), ink);
    }
}

/// 4. Receipt: till receipt coming out of a printer slot.
fn card_receipt(ui: &Ui, p: &egui::Painter, r: Rect, d: &CardData) {
    vgradient(p, r, rgb(0x34434b), rgb(0x161d21));
    let slot = Rect::from_center_size(pos2(r.center().x, r.top() + 18.0), vec2(392.0, 14.0));
    p.rect_filled(slot, 7.0, rgb(0x0b0f11));

    let paper = Rect::from_min_max(pos2(r.center().x - 176.0, r.top() + 18.0), pos2(r.center().x + 176.0, r.bottom()));
    p.rect_filled(paper, 0.0, rgb(0xfbf7ee));
    let ink = rgb(0x2a2620);
    let faded = rgb(0x857e72);
    let x = paper.left() + 22.0;
    let rx = paper.right() - 22.0;
    let w = rx - x;
    let mut y = paper.top() + 22.0;

    p.text(pos2(paper.center().x, y), Align2::CENTER_TOP, "YT MUSIC", FontId::monospace(24.0), ink);
    y += 30.0;
    p.text(pos2(paper.center().x, y), Align2::CENTER_TOP, "* WRAPPED *", FontId::monospace(12.5), ink);
    y += 18.0;
    p.text(pos2(paper.center().x, y), Align2::CENTER_TOP, d.label.to_uppercase(), FontId::monospace(11.5), faded);
    y += 22.0;

    let dashed = |p: &egui::Painter, y: f32| {
        let mut cx = x;
        while cx < rx {
            p.hline(cx..=(cx + 5.0).min(rx), y, Stroke::new(1.2, faded));
            cx += 9.0;
        }
    };
    let row = |p: &egui::Painter, y: f32, left: &str, right: &str| {
        let rg = p.layout_no_wrap(right.to_owned(), FontId::monospace(12.0), ink);
        p.galley(pos2(rx - rg.size().x, y), rg.clone(), ink);
        let lg = widgets::galley(ui, left, FontId::monospace(12.0), ink, (w - rg.size().x - 10.0).max(20.0));
        p.galley(pos2(x, y), lg, ink);
    };

    dashed(p, y);
    y += 10.0;
    row(p, y, "TOP SONGS", "×");
    y += 18.0;
    for (n, s) in d.songs_top.iter().enumerate() {
        row(p, y, &format!("{} {}", n + 1, s.title.to_uppercase()), &s.plays.to_string());
        y += 17.0;
    }
    y += 6.0;
    dashed(p, y);
    y += 10.0;
    row(p, y, "TOP ARTISTS", "");
    y += 18.0;
    for (n, a) in d.artists_top.iter().take(3).enumerate() {
        row(p, y, &format!("{} {}", n + 1, a.name.to_uppercase()), &format!("{} min", a.minutes));
        y += 17.0;
    }
    y += 6.0;
    dashed(p, y);
    y += 10.0;
    row(p, y, "SONGS", &grouped(d.songs));
    y += 17.0;
    row(p, y, "ARTISTS", &grouped(d.artists));
    y += 17.0;
    row(p, y, "BEST HOUR", &d.hour_text());
    y += 17.0;
    row(p, y, "DAY STREAK", &d.streak.to_string());
    y += 22.0;
    dashed(p, y);
    y += 10.0;
    p.text(pos2(x, y), Align2::LEFT_TOP, "TOTAL", FontId::monospace(17.0), ink);
    p.text(pos2(rx, y), Align2::RIGHT_TOP, format!("{} MIN", grouped(d.minutes)), FontId::monospace(17.0), ink);
    y += 28.0;
    dashed(p, y);
    y += 10.0;
    p.text(pos2(paper.center().x, y), Align2::CENTER_TOP, "THANK YOU FOR LISTENING", FontId::monospace(11.0), ink);
    y += 20.0;
    let bar_rect = Rect::from_center_size(pos2(paper.center().x, y + 18.0), vec2(210.0, 34.0));
    paint_barcode(p, bar_rect, d.seed(), ink, 46);
    p.text(pos2(paper.center().x, bar_rect.bottom() + 6.0), Align2::CENTER_TOP, "#YTMusicWrapped", FontId::monospace(10.5), faded);
}

/// 5. Neon: glowing pink & cyan on night-purple with synthwave floor lines.
fn card_neon(ui: &Ui, p: &egui::Painter, r: Rect, d: &CardData) {
    let pink = rgb(0xff3ec8);
    let cyan = rgb(0x3ff0ff);
    let white = rgb(0xf3e9ff);
    vgradient(p, r, rgb(0x170b30), rgb(0x05030b));
    paint_soft_orb(p, pos2(r.left() + 40.0, r.top() + 40.0), 170.0, pink, 0.35);
    paint_soft_orb(p, pos2(r.right() - 40.0, r.bottom() - 80.0), 170.0, cyan, 0.25);

    // Horizon grid lines at the bottom
    let mut fy = r.bottom() - 12.0;
    for i in 0..7 {
        p.hline(r.left()..=r.right(), fy, Stroke::new(1.0, theme::with_alpha(pink, 0.32)));
        fy -= 5.0 + i as f32 * 3.5;
    }

    let glow = |p: &egui::Painter, pos: Pos2, text: &str, font: FontId, col: Color32| {
        for off in [vec2(-1.5, 0.0), vec2(1.5, 0.0), vec2(0.0, -1.5), vec2(0.0, 1.5)] {
            p.text(pos + off, Align2::CENTER_TOP, text, font.clone(), theme::with_alpha(col, 0.28));
        }
        p.text(pos, Align2::CENTER_TOP, text, font, col);
    };

    glow(p, pos2(r.center().x, r.top() + 26.0), "WRAPPED", theme::bold(42.0), pink);
    p.text(pos2(r.center().x, r.top() + 78.0), Align2::CENTER_TOP, d.label.to_uppercase(), theme::semibold(12.5), cyan);

    let mins = grouped(d.minutes);
    let msize = fit_size(&mins, 380.0, 0.56, 76.0);
    glow(p, pos2(r.center().x, r.top() + 106.0), &mins, theme::bold(msize), cyan);
    p.text(pos2(r.center().x, r.top() + 106.0 + msize + 6.0), Align2::CENTER_TOP, "MINUTES OF MUSIC", theme::semibold(11.5), Color32::from_white_alpha(190));

    if let Some(top) = d.songs_top.first() {
        let box_r = Rect::from_center_size(pos2(r.center().x, r.top() + 266.0), vec2(396.0, 78.0));
        p.rect_filled(box_r, 14.0, Color32::from_black_alpha(110));
        p.rect_stroke(box_r, 14.0, Stroke::new(2.0, pink), StrokeKind::Inside);
        let cov = Rect::from_min_size(box_r.min + vec2(14.0, 12.0), Vec2::splat(54.0));
        paint_card_cover(ui, top.thumbnail.as_deref(), cov, 8.0, 160, rgb(0x22103a));
        let tx = cov.right() + 14.0;
        let tw = (box_r.right() - 14.0 - tx).max(40.0);
        p.text(pos2(tx, box_r.top() + 11.0), Align2::LEFT_TOP, "#1", theme::bold(10.5), pink);
        let tg = widgets::galley(ui, &top.title, theme::semibold(17.0), white, tw);
        p.galley(pos2(tx, box_r.top() + 25.0), tg, white);
        let ag = widgets::galley(ui, &top.artist, theme::regular(12.5), cyan, tw);
        p.galley(pos2(tx, box_r.top() + 48.0), ag, cyan);
    }

    let pad = 32.0;
    let y_list = r.top() + 336.0;
    let col_w = (r.width() - pad * 2.0 - 24.0) / 2.0;
    p.text(pos2(r.left() + pad, y_list), Align2::LEFT_TOP, "TOP SONGS", theme::semibold(11.0), cyan);
    p.text(pos2(r.left() + pad + col_w + 24.0, y_list), Align2::LEFT_TOP, "TOP ARTISTS", theme::semibold(11.0), cyan);
    for (n, s) in d.songs_top.iter().enumerate() {
        let yy = y_list + 22.0 + n as f32 * 26.0;
        p.text(pos2(r.left() + pad, yy), Align2::LEFT_TOP, format!("{}", n + 1), theme::bold(13.0), pink);
        let g = widgets::galley(ui, &s.title, theme::regular(13.0), white, col_w - 18.0);
        p.galley(pos2(r.left() + pad + 16.0, yy), g, white);
    }
    for (n, a) in d.artists_top.iter().enumerate() {
        let yy = y_list + 22.0 + n as f32 * 26.0;
        let ax = r.left() + pad + col_w + 24.0;
        p.text(pos2(ax, yy), Align2::LEFT_TOP, format!("{}", n + 1), theme::bold(13.0), pink);
        let g = widgets::galley(ui, &a.name, theme::regular(13.0), white, col_w - 18.0);
        p.galley(pos2(ax + 16.0, yy), g, white);
    }
}

/// 6. Aurora: northern lights orbs behind frosted glass panels.
fn card_aurora(ui: &Ui, p: &egui::Painter, r: Rect, d: &CardData) {
    p.rect_filled(r, 0.0, rgb(0x070b1c));
    paint_soft_orb(p, pos2(r.left() + 80.0, r.top() + 90.0), 190.0, rgb(0x19d3b5), 0.72);
    paint_soft_orb(p, pos2(r.right() - 70.0, r.top() + 190.0), 200.0, rgb(0x7a4dff), 0.72);
    paint_soft_orb(p, pos2(r.left() + 140.0, r.bottom() - 90.0), 170.0, rgb(0x5cff8a), 0.45);
    paint_soft_orb(p, pos2(r.right() - 60.0, r.bottom() - 50.0), 170.0, rgb(0xff5fa8), 0.48);

    let pad = 30.0;
    let x = r.left() + pad;
    let w = r.width() - pad * 2.0;

    p.text(pos2(r.center().x, r.top() + pad), Align2::CENTER_TOP, "YT Music Wrapped", theme::semibold(13.0), Color32::from_white_alpha(210));
    p.text(pos2(r.center().x, r.top() + pad + 20.0), Align2::CENTER_TOP, &d.label, theme::regular(14.0), Color32::from_white_alpha(180));

    let mins = grouped(d.minutes);
    let msize = fit_size(&mins, 400.0, 0.55, 86.0);
    p.text(pos2(r.center().x, r.top() + 102.0), Align2::CENTER_TOP, &mins, theme::bold(msize), Color32::WHITE);
    p.text(pos2(r.center().x, r.top() + 102.0 + msize + 6.0), Align2::CENTER_TOP, "minutes of music", theme::regular(16.0), Color32::from_white_alpha(205));

    // Glass main card
    let glass_r = Rect::from_min_max(pos2(x, r.top() + 280.0), pos2(x + w, r.bottom() - pad - 82.0));
    p.rect_filled(glass_r, 18.0, Color32::from_white_alpha(22));
    p.rect_stroke(glass_r, 18.0, Stroke::new(1.0, Color32::from_white_alpha(48)), StrokeKind::Inside);

    let mut cy = glass_r.top() + 18.0;
    if let Some(top) = d.songs_top.first() {
        let cov = Rect::from_min_size(pos2(glass_r.left() + 18.0, cy), Vec2::splat(64.0));
        paint_card_cover(ui, top.thumbnail.as_deref(), cov, 12.0, 180, rgb(0x182040));
        let tx = cov.right() + 14.0;
        let tw = (glass_r.right() - 18.0 - tx).max(40.0);
        p.text(pos2(tx, cy + 2.0), Align2::LEFT_TOP, "YOUR SONG", theme::semibold(10.5), Color32::from_white_alpha(170));
        let tg = widgets::galley(ui, &top.title, theme::semibold(18.0), Color32::WHITE, tw);
        p.galley(pos2(tx, cy + 18.0), tg, Color32::WHITE);
        let ag = widgets::galley(ui, &top.artist, theme::regular(13.0), Color32::from_white_alpha(185), tw);
        p.galley(pos2(tx, cy + 42.0), ag, Color32::from_white_alpha(185));
        cy = cov.bottom() + 14.0;
        p.hline(glass_r.left() + 18.0..=glass_r.right() - 18.0, cy, Stroke::new(1.0, Color32::from_white_alpha(38)));
        cy += 12.0;
    }
    p.text(pos2(glass_r.left() + 18.0, cy), Align2::LEFT_TOP, "TOP ARTISTS", theme::semibold(10.5), Color32::from_white_alpha(170));
    cy += 20.0;
    let mut px_cur = glass_r.left() + 18.0;
    for a in d.artists_top.iter().take(4) {
        let g = widgets::galley(ui, &a.name, theme::regular(12.5), Color32::WHITE, 165.0);
        let pw = g.size().x + 22.0;
        if px_cur + pw > glass_r.right() - 18.0 {
            px_cur = glass_r.left() + 18.0;
            cy += 34.0;
        }
        if cy + 28.0 <= glass_r.bottom() - 8.0 {
            let pr = Rect::from_min_size(pos2(px_cur, cy), vec2(pw, 26.0));
            p.rect_filled(pr, 13.0, Color32::from_white_alpha(28));
            p.galley(pos2(pr.left() + 11.0, pr.center().y - g.size().y * 0.5), g, Color32::WHITE);
            px_cur += pw + 8.0;
        }
    }

    // Bottom 3 glass pills
    let pill_y = r.bottom() - pad - 68.0;
    let pw = (w - 24.0) / 3.0;
    let pills = [("SONGS", grouped(d.songs)), ("ARTISTS", grouped(d.artists)), ("BEST HOUR", d.hour_text())];
    for (i, (k, v)) in pills.iter().enumerate() {
        let pr = Rect::from_min_size(pos2(x + i as f32 * (pw + 12.0), pill_y), vec2(pw, 66.0));
        p.rect_filled(pr, 16.0, Color32::from_white_alpha(22));
        p.rect_stroke(pr, 16.0, Stroke::new(1.0, Color32::from_white_alpha(48)), StrokeKind::Inside);
        p.text(pos2(pr.center().x, pr.top() + 12.0), Align2::CENTER_TOP, v, theme::bold(21.0), Color32::WHITE);
        p.text(pos2(pr.center().x, pr.bottom() - 14.0), Align2::CENTER_BOTTOM, *k, theme::semibold(10.0), Color32::from_white_alpha(170));
    }
}

/// 7. Cassette: retro 70s mixtape with Side A (songs) & Side B (artists).
fn card_cassette(ui: &Ui, p: &egui::Painter, r: Rect, d: &CardData) {
    let cream = rgb(0xf1e5c9);
    let ink = rgb(0x2e2015);
    let body_c = rgb(0x2b2521);
    let label_c = rgb(0xf8f1df);
    let rust = rgb(0xb84a1c);
    p.rect_filled(r, 0.0, cream);

    let pad = 28.0;
    let x = r.left() + pad;
    let w = r.width() - pad * 2.0;

    p.text(pos2(x, r.top() + pad), Align2::LEFT_TOP, "YT MUSIC WRAPPED", FontId::monospace(12.0), ink);
    p.text(pos2(r.right() - pad, r.top() + pad), Align2::RIGHT_TOP, "STEREO", FontId::monospace(12.0), rust);

    // Cassette body
    let body = Rect::from_min_size(pos2(x, r.top() + 56.0), vec2(w, 266.0));
    p.rect_filled(body, 18.0, body_c);
    // Screws
    for pos in [
        pos2(body.left() + 10.0, body.top() + 10.0),
        pos2(body.right() - 10.0, body.top() + 10.0),
        pos2(body.left() + 10.0, body.bottom() - 10.0),
        pos2(body.right() - 10.0, body.bottom() - 10.0),
    ] {
        p.circle_filled(pos, 4.0, rgb(0x6b625a));
    }
    // Sticker label
    let lbl = Rect::from_min_size(body.min + vec2(14.0, 14.0), vec2(w - 28.0, 174.0));
    p.rect_filled(lbl, 10.0, label_c);
    let badge = Rect::from_min_size(lbl.min + vec2(12.0, 12.0), Vec2::splat(26.0));
    p.rect_filled(badge, 4.0, ink);
    p.text(badge.center(), Align2::CENTER_CENTER, "A", FontId::monospace(15.0), label_c);
    let title_g = widgets::galley(ui, &d.label, theme::bold(19.0), ink, lbl.width() - 120.0);
    p.galley(pos2(badge.right() + 10.0, badge.top() + 2.0), title_g, ink);
    p.text(pos2(lbl.right() - 12.0, badge.center().y), Align2::RIGHT_CENTER, format!("C-{}", d.minutes), FontId::monospace(13.0), ink);

    // Retro 3-color stripes
    for (i, hex) in [0xe8622c, 0xf3a53a, 0xf6d36b].iter().enumerate() {
        let sy = lbl.top() + 48.0 + i as f32 * 9.0;
        p.rect_filled(Rect::from_min_size(pos2(lbl.left() + 12.0, sy), vec2(lbl.width() - 24.0, 6.0)), 0.0, rgb(*hex));
    }

    // Tape window & reels
    let win = Rect::from_center_size(pos2(lbl.center().x, lbl.bottom() - 46.0), vec2(264.0, 64.0));
    p.rect_filled(win, 32.0, rgb(0x1a1512));
    p.rect_filled(Rect::from_center_size(win.center(), vec2(150.0, 28.0)), 0.0, rgb(0x5b3a26));
    for rx_c in [win.left() + 34.0, win.right() - 34.0] {
        let c = pos2(rx_c, win.center().y);
        p.circle_filled(c, 25.0, label_c);
        p.circle_stroke(c, 24.0, Stroke::new(3.5, rgb(0x4a3f38)));
        p.circle_filled(c, 9.0, body_c);
        p.circle_stroke(c, 9.0, Stroke::new(2.0, rgb(0x8a7f74)));
    }

    // Bottom trapezoid ridge & holes
    let ridge = Rect::from_min_max(pos2(body.center().x - 130.0, body.bottom() - 52.0), pos2(body.center().x + 130.0, body.bottom()));
    p.rect_filled(ridge, CornerRadius { nw: 12, ne: 12, sw: 0, se: 0 }, rgb(0x3a332e));
    p.circle_filled(pos2(ridge.center().x - 46.0, ridge.center().y), 6.0, rgb(0x14100e));
    p.circle_filled(pos2(ridge.center().x + 46.0, ridge.center().y), 6.0, rgb(0x14100e));

    // Side A & Side B tracklist
    let y_list = body.bottom() + 20.0;
    let col_w = (w - 20.0) / 2.0;
    p.text(pos2(x, y_list), Align2::LEFT_TOP, "SIDE A · TOP SONGS", FontId::monospace(10.5), rust);
    p.text(pos2(x + col_w + 20.0, y_list), Align2::LEFT_TOP, "SIDE B · TOP ARTISTS", FontId::monospace(10.5), rust);
    for (n, s) in d.songs_top.iter().enumerate() {
        let g = widgets::galley(ui, &format!("{}. {}", n + 1, s.title), FontId::monospace(11.5), ink, col_w);
        p.galley(pos2(x, y_list + 20.0 + n as f32 * 21.0), g, ink);
    }
    for (n, a) in d.artists_top.iter().enumerate() {
        let g = widgets::galley(ui, &format!("{}. {}", n + 1, a.name), FontId::monospace(11.5), ink, col_w);
        p.galley(pos2(x + col_w + 20.0, y_list + 20.0 + n as f32 * 21.0), g, ink);
    }

    let foot = format!("{} min · {} · {}", grouped(d.minutes), plural(d.songs as usize, "song", "songs"), plural(d.artists as usize, "artist", "artists"));
    p.text(pos2(x, r.bottom() - pad), Align2::LEFT_BOTTOM, foot, FontId::monospace(10.5), rgb(0x7a6a58));
}

/// 8. Polaroid: instant photo frame, yellow minutes sticker, mini polaroids below.
fn card_polaroid(ui: &Ui, p: &egui::Painter, r: Rect, d: &CardData) {
    vgradient(p, r, rgb(0xd8ccb6), rgb(0xb9a688));
    let frame_c = rgb(0xfcfbf7);
    let ink = rgb(0x2b2b2b);

    let photo_s = 296.0;
    let frame = Rect::from_min_size(pos2(r.center().x - (photo_s + 28.0) * 0.5, r.top() + 30.0), vec2(photo_s + 28.0, photo_s + 80.0));
    widgets::soft_shadow(ui, frame, 2.0, 22.0, 0.28);
    p.rect_filled(frame, 2.0, frame_c);

    let photo = Rect::from_min_size(frame.min + vec2(14.0, 14.0), Vec2::splat(photo_s));
    paint_card_cover(ui, d.songs_top.first().and_then(|s| s.thumbnail.as_deref()), photo, 0.0, 544, rgb(0x2a2a2a));

    if let Some(top) = d.songs_top.first() {
        let tg = widgets::galley(ui, &top.title, theme::bold(20.0), ink, photo_s);
        p.galley(pos2(frame.center().x - tg.size().x * 0.5, photo.bottom() + 12.0), tg, ink);
        let ag = widgets::galley(ui, &top.artist, theme::regular(13.5), rgb(0x5a5a5a), photo_s);
        p.galley(pos2(frame.center().x - ag.size().x * 0.5, photo.bottom() + 38.0), ag, rgb(0x5a5a5a));
    }

    // Yellow circular sticker on the top-right corner
    let sticker_c = pos2(frame.right() - 12.0, frame.top() + 20.0);
    p.circle_filled(sticker_c + vec2(0.0, 3.0), 46.0, Color32::from_black_alpha(45));
    p.circle_filled(sticker_c, 46.0, rgb(0xffd23f));
    p.circle_stroke(sticker_c, 45.0, Stroke::new(2.0, rgb(0xf2b705)));
    let mins = grouped(d.minutes);
    p.text(pos2(sticker_c.x, sticker_c.y - 6.0), Align2::CENTER_CENTER, &mins, theme::bold(fit_size(&mins, 74.0, 0.58, 22.0)), rgb(0x231a00));
    p.text(pos2(sticker_c.x, sticker_c.y + 14.0), Align2::CENTER_CENTER, "minutes", theme::semibold(11.0), rgb(0x231a00));

    // Caption
    let cap = format!("My No. 1 · {}", d.label);
    let cg = widgets::galley(ui, &cap, theme::semibold(18.0), rgb(0x3a2f1f), 420.0);
    p.galley(pos2(r.center().x - cg.size().x * 0.5, frame.bottom() + 14.0), cg, rgb(0x3a2f1f));

    // Mini polaroids #2..#5
    let count = d.songs_top.len().saturating_sub(1).min(4);
    if count > 0 {
        let mw = 76.0;
        let gap = 14.0;
        let total_w = count as f32 * mw + (count - 1) as f32 * gap;
        let start_x = r.center().x - total_w * 0.5;
        let my = frame.bottom() + 50.0;
        for i in 1..=count {
            let mr = Rect::from_min_size(pos2(start_x + (i - 1) as f32 * (mw + gap), my), vec2(mw, 92.0));
            widgets::soft_shadow(ui, mr, 2.0, 12.0, 0.24);
            p.rect_filled(mr, 2.0, frame_c);
            let mp = Rect::from_min_size(mr.min + vec2(5.0, 5.0), Vec2::splat(66.0));
            paint_card_cover(ui, d.songs_top[i].thumbnail.as_deref(), mp, 0.0, 160, rgb(0x2a2a2a));
            p.text(pos2(mr.center().x, mr.bottom() - 11.0), Align2::CENTER_CENTER, format!("#{}", i + 1), theme::bold(12.0), ink);
        }
    }
}

/// 9. Big numbers: colorful bento grid on near-black.
fn card_big_numbers(ui: &Ui, p: &egui::Painter, r: Rect, d: &CardData) {
    p.rect_filled(r, 0.0, rgb(0x0e0e10));
    let pad = 20.0;
    let x = r.left() + pad;
    let w = r.width() - pad * 2.0;
    let gap = 10.0;

    icon_at(ui, pos2(x + 8.0, r.top() + pad + 9.0), icon::STATS, 15.0, Color32::WHITE);
    p.text(pos2(x + 24.0, r.top() + pad + 9.0), Align2::LEFT_CENTER, "Wrapped", theme::semibold(15.0), Color32::WHITE);
    p.text(pos2(r.right() - pad, r.top() + pad + 9.0), Align2::RIGHT_CENTER, &d.label, theme::regular(13.0), Color32::from_white_alpha(160));

    let tile = |rect: Rect, bg: u32, ink: u32, label: &str, value: &str, max_sz: f32| {
        p.rect_filled(rect, 18.0, rgb(bg));
        p.text(pos2(rect.left() + 16.0, rect.top() + 14.0), Align2::LEFT_TOP, label, theme::semibold(12.0), rgb(ink));
        let sz = fit_size(value, rect.width() - 32.0, 0.56, max_sz);
        p.text(pos2(rect.left() + 16.0, rect.bottom() - 14.0), Align2::LEFT_BOTTOM, value, theme::bold(sz), rgb(ink));
    };

    let y0 = r.top() + 52.0;
    tile(Rect::from_min_size(pos2(x, y0), vec2(w, 150.0)), 0xc6f24e, 0x10140a, "Minutes listened", &grouped(d.minutes), 94.0);

    let hw = (w - gap) / 2.0;
    let y1 = y0 + 150.0 + gap;
    tile(Rect::from_min_size(pos2(x, y1), vec2(hw, 112.0)), 0x4cc2ff, 0x05202e, "Songs played", &grouped(d.plays), 48.0);
    tile(Rect::from_min_size(pos2(x + hw + gap, y1), vec2(hw, 112.0)), 0xff6f59, 0x2b0b05, "Different songs", &grouped(d.songs), 48.0);

    let y2 = y1 + 112.0 + gap;
    tile(Rect::from_min_size(pos2(x, y2), vec2(hw, 112.0)), 0xa99bff, 0x140f33, "Artists", &grouped(d.artists), 48.0);
    tile(Rect::from_min_size(pos2(x + hw + gap, y2), vec2(hw, 112.0)), 0xffb938, 0x2e1c00, "Days in a row", &d.streak.to_string(), 48.0);

    let y3 = y2 + 112.0 + gap;
    let bot = Rect::from_min_max(pos2(x, y3), pos2(x + w, r.bottom() - pad));
    p.rect_filled(bot, 18.0, rgb(0x1f1f23));
    let cov = Rect::from_center_size(pos2(bot.left() + 42.0, bot.center().y), Vec2::splat(52.0));
    paint_card_cover(ui, d.songs_top.first().and_then(|s| s.thumbnail.as_deref()), cov, 10.0, 160, rgb(0x2e2e34));

    let tx = cov.right() + 12.0;
    let tw = (bot.right() - 104.0 - tx).max(40.0);
    p.text(pos2(tx, bot.top() + 16.0), Align2::LEFT_TOP, "YOUR SONG", theme::semibold(10.5), Color32::from_white_alpha(150));
    if let Some(top) = d.songs_top.first() {
        let tg = widgets::galley(ui, &top.title, theme::semibold(15.5), Color32::WHITE, tw);
        p.galley(pos2(tx, bot.top() + 33.0), tg, Color32::WHITE);
        let ag = widgets::galley(ui, &top.artist, theme::regular(12.0), Color32::from_white_alpha(180), tw);
        p.galley(pos2(tx, bot.top() + 55.0), ag, Color32::from_white_alpha(180));
    }
    p.text(pos2(bot.right() - 16.0, bot.top() + 20.0), Align2::RIGHT_TOP, "BEST HOUR", theme::semibold(10.5), Color32::from_white_alpha(150));
    p.text(pos2(bot.right() - 16.0, bot.top() + 38.0), Align2::RIGHT_TOP, d.hour_text(), theme::bold(21.0), Color32::WHITE);
}

/// 10. Magazine: full-bleed cover, masthead, gold accents, barcode.
fn card_magazine(ui: &Ui, p: &egui::Painter, r: Rect, d: &CardData) {
    let gold = rgb(0xffd53d);
    vgradient(p, r, rgb(0x8c1d2c), rgb(0x1c070b));
    if let Some(u) = d.songs_top.first().and_then(|s| s.thumbnail.as_deref()) {
        // Center-crop square cover into 4:5 vertical frame
        paint_remote(ui, &sized(u, 640), r, 0.0, Rect::from_min_max(pos2(0.1, 0.0), pos2(0.9, 1.0)), Color32::WHITE);
    }
    vgradient(p, Rect::from_min_size(r.min, vec2(r.width(), 250.0)), Color32::from_black_alpha(180), Color32::TRANSPARENT);
    vgradient(p, Rect::from_min_max(pos2(r.left(), r.bottom() - 390.0), r.max), Color32::TRANSPARENT, Color32::from_black_alpha(236));

    let pad = 26.0;
    let x = r.left() + pad;
    let w = r.width() - pad * 2.0;

    p.text(pos2(x, r.top() + pad), Align2::LEFT_TOP, "YT MUSIC", theme::bold(11.0), Color32::WHITE);
    p.text(pos2(r.right() - pad, r.top() + pad), Align2::RIGHT_TOP, d.label.to_uppercase(), theme::bold(11.0), Color32::WHITE);
    p.text(pos2(x, r.top() + 44.0), Align2::LEFT_TOP, "WRAPPED", theme::bold(72.0), Color32::WHITE);

    let line_y = r.top() + 132.0;
    p.hline(x..=r.right() - pad - 122.0, line_y, Stroke::new(2.0, Color32::WHITE));
    p.text(pos2(r.right() - pad, line_y), Align2::RIGHT_CENTER, "THE MUSIC ISSUE", theme::bold(11.0), gold);

    let mut y = r.top() + 290.0;
    p.text(pos2(x, y), Align2::LEFT_TOP, grouped(d.minutes), theme::bold(54.0), gold);
    y += 58.0;
    p.text(pos2(x, y), Align2::LEFT_TOP, "minutes of music", theme::regular(19.0), Color32::WHITE);
    y += 38.0;

    if let Some(top) = d.songs_top.first() {
        p.text(pos2(x, y), Align2::LEFT_TOP, "COVER STORY", theme::bold(11.0), gold);
        y += 16.0;
        let tg = widgets::galley(ui, &top.title, theme::bold(28.0), Color32::WHITE, w);
        p.galley(pos2(x, y), tg, Color32::WHITE);
        y += 36.0;
        let ag = widgets::galley(ui, &top.artist, theme::regular(14.5), Color32::from_white_alpha(205), w);
        p.galley(pos2(x, y), ag, Color32::from_white_alpha(205));
        y += 32.0;
    }

    let also: Vec<&str> = d.artists_top.iter().take(4).map(|a| a.name.as_str()).collect();
    if !also.is_empty() {
        p.text(pos2(x, y), Align2::LEFT_TOP, "ALSO INSIDE", theme::bold(11.0), gold);
        let ag = widgets::galley(ui, &also.join(" · "), theme::regular(14.0), Color32::WHITE, w);
        p.galley(pos2(x, y + 16.0), ag, Color32::WHITE);
    }

    let foot = format!("{} · {}", plural(d.songs as usize, "song", "songs"), plural(d.artists as usize, "artist", "artists")).to_uppercase();
    let fg = widgets::galley(ui, &foot, theme::bold(10.0), Color32::from_white_alpha(180), w - 108.0);
    p.galley(pos2(x, r.bottom() - pad - 14.0), fg, Color32::from_white_alpha(180));
    let bc_box = Rect::from_min_max(pos2(r.right() - pad - 96.0, r.bottom() - pad - 32.0), pos2(r.right() - pad, r.bottom() - pad));
    p.rect_filled(bc_box, 2.0, Color32::WHITE);
    paint_barcode(p, bc_box.shrink(4.0), d.seed(), Color32::BLACK, 22);
}

fn draw_swatch(ui: &Ui, r: Rect, style: WrappedStyle, accent: Color32) {
    let p = ui.painter().with_clip_rect(r);
    match style {
        WrappedStyle::Signature => {
            vgradient(&p, r, rgb(0x1d1d20), rgb(0x08080a));
            paint_soft_orb(&p, pos2(r.right() - 12.0, r.top() + 8.0), 42.0, accent, 0.7);
            p.text(r.center(), Align2::CENTER_CENTER, "1,234", theme::bold(19.0), accent);
        }
        WrappedStyle::Poster => {
            p.rect_filled(r, 8.0, rgb(0xff4a1c));
            p.text(r.center(), Align2::CENTER_CENTER, "123", theme::bold(30.0), rgb(0x150d0a));
        }
        WrappedStyle::Minimal => {
            p.rect_filled(r, 8.0, rgb(0xf5f4f0));
            p.text(r.center(), Align2::CENTER_CENTER, "1234", theme::weighted(crate::preset::Weight::Light, 24.0), rgb(0x161616));
        }
        WrappedStyle::Receipt => {
            p.rect_filled(r, 8.0, rgb(0x2e3b42));
            let paper = Rect::from_min_max(pos2(r.center().x - 24.0, r.top() + 8.0), pos2(r.center().x + 24.0, r.bottom()));
            p.rect_filled(paper, 0.0, rgb(0xfbf7ee));
            for i in 0..6 {
                let lw = if i == 0 { 24.0 } else { 34.0 };
                let yy = paper.top() + 8.0 + i as f32 * 8.0;
                p.hline(paper.center().x - lw * 0.5..=paper.center().x + lw * 0.5, yy, Stroke::new(2.0, rgb(0x857e72)));
            }
        }
        WrappedStyle::Neon => {
            p.rect_filled(r, 8.0, rgb(0x0d0720));
            p.text(r.center(), Align2::CENTER_CENTER, "NEON", theme::bold(18.0), rgb(0xff3ec8));
        }
        WrappedStyle::Aurora => {
            p.rect_filled(r, 8.0, rgb(0x070b1c));
            paint_soft_orb(&p, pos2(r.left() + 20.0, r.top() + 14.0), 38.0, rgb(0x19d3b5), 0.85);
            paint_soft_orb(&p, pos2(r.right() - 20.0, r.top() + 34.0), 38.0, rgb(0x7a4dff), 0.85);
            paint_soft_orb(&p, pos2(r.center().x, r.bottom() - 8.0), 32.0, rgb(0xff5fa8), 0.7);
        }
        WrappedStyle::Cassette => {
            p.rect_filled(r, 8.0, rgb(0xf1e5c9));
            let b = Rect::from_center_size(r.center(), vec2(74.0, 46.0));
            p.rect_filled(b, 6.0, rgb(0x2b2521));
            p.rect_filled(Rect::from_center_size(b.center() - vec2(0.0, 5.0), vec2(56.0, 18.0)), 3.0, rgb(0xf8f1df));
            p.circle_filled(b.center() + vec2(-14.0, -5.0), 5.0, rgb(0x2b2521));
            p.circle_filled(b.center() + vec2(14.0, -5.0), 5.0, rgb(0x2b2521));
        }
        WrappedStyle::Polaroid => {
            p.rect_filled(r, 8.0, rgb(0xcabaa0));
            let f = Rect::from_center_size(r.center(), vec2(44.0, 52.0));
            p.rect_filled(f, 2.0, rgb(0xfcfbf7));
            p.rect_filled(Rect::from_min_size(f.min + vec2(4.0, 4.0), Vec2::splat(36.0)), 0.0, rgb(0x3a3a42));
            p.circle_filled(pos2(f.right(), f.top() + 4.0), 8.0, rgb(0xffd23f));
        }
        WrappedStyle::Numbers => {
            p.rect_filled(r, 8.0, rgb(0x0e0e10));
            let w = r.width() - 16.0;
            p.rect_filled(Rect::from_min_size(r.min + vec2(8.0, 8.0), vec2(w, 22.0)), 5.0, rgb(0xc6f24e));
            let hw = (w - 4.0) / 2.0;
            p.rect_filled(Rect::from_min_size(r.min + vec2(8.0, 34.0), vec2(hw, 14.0)), 4.0, rgb(0x4cc2ff));
            p.rect_filled(Rect::from_min_size(r.min + vec2(12.0 + hw, 34.0), vec2(hw, 14.0)), 4.0, rgb(0xff6f59));
            p.rect_filled(Rect::from_min_size(r.min + vec2(8.0, 51.0), vec2(hw, 11.0)), 4.0, rgb(0xa99bff));
            p.rect_filled(Rect::from_min_size(r.min + vec2(12.0 + hw, 51.0), vec2(hw, 11.0)), 4.0, rgb(0xffb938));
        }
        WrappedStyle::Magazine => {
            vgradient(&p, r, rgb(0x8c1d2c), rgb(0x1c070b));
            p.text(pos2(r.center().x, r.top() + 18.0), Align2::CENTER_CENTER, "WRAPPED", theme::bold(14.0), Color32::WHITE);
            p.hline(r.left() + 14.0..=r.right() - 14.0, r.top() + 30.0, Stroke::new(1.5, rgb(0xffd53d)));
            p.text(pos2(r.center().x, r.bottom() - 16.0), Align2::CENTER_CENTER, "2026", theme::bold(16.0), rgb(0xffd53d));
        }
    }
    let _ = ui;
    p.rect_stroke(r, 8.0, Stroke::new(1.0, Color32::from_white_alpha(26)), StrokeKind::Inside);
}

// ------------------------------------------------------------------ Formatting helpers

fn is_local_id(id: &str) -> bool {
    id.starts_with("local:") || id.is_empty()
}

fn song_track(s: &SongStat) -> Track {
    Track {
        id: s.video_id.clone(),
        title: s.title.clone(),
        artists: if s.artist.is_empty() {
            Vec::new()
        } else {
            vec![Link { name: s.artist.clone(), id: None }]
        },
        album: (!s.album.is_empty()).then(|| Link { name: s.album.clone(), id: None }),
        duration: None,
        thumbs: s.thumbnail.as_ref().map(|u| vec![Thumb { url: u.clone(), width: 544, height: 544 }]).unwrap_or_default(),
        ..Default::default()
    }
}

fn plural(n: usize, one: &str, many: &str) -> String {
    if n == 1 {
        format!("{} {one}", grouped(n as u32))
    } else {
        format!("{} {many}", grouped(n as u32))
    }
}

fn grouped(n: u32) -> String {
    let digits = n.to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    out
}

fn friendly_time(minutes: u32) -> String {
    if minutes < 60 {
        return plural(minutes as usize, "minute", "minutes");
    }
    let (hours, rest) = ((minutes / 60) as usize, (minutes % 60) as usize);
    if hours < 10 && rest > 0 {
        format!("{} {}", plural(hours, "hour", "hours"), plural(rest, "minute", "minutes"))
    } else {
        plural(((minutes + 30) / 60) as usize, "hour", "hours")
    }
}

fn headline(s: &Summary, back: u32) -> String {
    let time = friendly_time(s.minutes);
    match (s.period, back) {
        (Period::All, _) => format!("You've listened for {time} in all"),
        (Period::Week, 0) => format!("You listened for {time} this week"),
        (Period::Week, 1) => format!("You listened for {time} last week"),
        (Period::Month, 0) => format!("You listened for {time} this month"),
        (Period::Year, 0) => format!("You listened for {time} this year"),
        _ => format!("You listened for {time} in {}", s.label),
    }
}

fn comparison(s: &Summary) -> Option<String> {
    let change = s.change?;
    let pct = format!("{:.0}", change.abs());
    let unit = match s.period {
        Period::Week => "week",
        Period::Month => "month",
        Period::Year => "year",
        Period::All => return None,
    };
    Some(if change.abs() < 2.0 {
        format!("About the same as the {unit} before.")
    } else if change > 0.0 {
        format!("{pct}% more than the {unit} before.")
    } else {
        format!("{pct}% less than the {unit} before.")
    })
}

fn clear_label(period: Period, back: u32, label: &str) -> String {
    match (period, back) {
        (Period::Week, 0) => "Clear this week".into(),
        (Period::Week, 1) => "Clear last week".into(),
        (Period::Month, 0) => "Clear this month".into(),
        (Period::Year, 0) => "Clear this year".into(),
        _ => format!("Clear {label}"),
    }
}
