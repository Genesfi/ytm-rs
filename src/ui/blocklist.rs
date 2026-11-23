//! Blocklist and AI slop blocker: automatically skips and blocks AI-generated
//! artists and tracks manually blocked or crowdsourced via Soul Over AI.

use std::collections::HashSet;
use std::sync::Arc;
use crate::model::Track;

const SOUL_OVER_AI_RAW: &str = include_str!("../../assets/soul_over_ai.json");

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BlockedReason {
    Artist(String),
    SoulOverAi(String),
    Song(String),
    Keyword(String),
}

#[derive(Clone, Debug)]
pub struct Blocklist {
    pub artists: Vec<String>,
    pub tracks: Vec<String>,
    pub keywords: Vec<String>,
    pub soul_over_ai: bool,
    pub auto_dislike: bool,
    user_artists_set: HashSet<String>,
    soul_artists_set: Arc<HashSet<String>>,
}

impl Default for Blocklist {
    fn default() -> Self {
        Self::new(Vec::new(), Vec::new(), Vec::new(), true, true)
    }
}

impl Blocklist {
    pub fn new(
        artists: Vec<String>,
        tracks: Vec<String>,
        keywords: Vec<String>,
        soul_over_ai: bool,
        auto_dislike: bool,
    ) -> Self {
        let user_artists_set = artists.iter().map(|s| s.trim().to_lowercase()).collect();
        let soul_list: Vec<String> = serde_json::from_str(SOUL_OVER_AI_RAW).unwrap_or_default();
        let soul_artists_set = Arc::new(soul_list.into_iter().map(|s| s.trim().to_lowercase()).collect());
        Self {
            artists,
            tracks,
            keywords,
            soul_over_ai,
            auto_dislike,
            user_artists_set,
            soul_artists_set,
        }
    }

    pub fn load(get: impl Fn(&str) -> Option<String>) -> Self {
        let on = |key| get(key).is_none_or(|v| v == "1");
        let artists: Vec<String> = get("blocked_artists")
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        let tracks: Vec<String> = get("blocked_tracks")
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        let keywords: Vec<String> = get("blocked_keywords")
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        let soul_over_ai = on("block_soul_over_ai");
        let auto_dislike = on("block_auto_dislike");
        Self::new(artists, tracks, keywords, soul_over_ai, auto_dislike)
    }

    pub fn save(&self, storage: &mut dyn eframe::Storage) {
        let b = |v| if v { "1" } else { "0" }.to_owned();
        storage.set_string("blocked_artists", serde_json::to_string(&self.artists).unwrap_or_default());
        storage.set_string("blocked_tracks", serde_json::to_string(&self.tracks).unwrap_or_default());
        storage.set_string("blocked_keywords", serde_json::to_string(&self.keywords).unwrap_or_default());
        storage.set_string("block_soul_over_ai", b(self.soul_over_ai));
        storage.set_string("block_auto_dislike", b(self.auto_dislike));
    }

    pub fn soul_artist_count(&self) -> usize {
        self.soul_artists_set.len()
    }

    pub fn is_artist_blocked(&self, name: &str) -> bool {
        let low = name.trim().to_lowercase();
        if low.is_empty() {
            return false;
        }
        if self.user_artists_set.contains(&low) {
            return true;
        }
        if self.soul_over_ai && self.soul_artists_set.contains(&low) {
            return true;
        }
        for a in &self.artists {
            if whole_word_matches(name, a) {
                return true;
            }
        }
        false
    }

    pub fn is_user_artist_blocked(&self, name: &str) -> bool {
        let low = name.trim().to_lowercase();
        self.user_artists_set.contains(&low) || self.artists.iter().any(|a| a.eq_ignore_ascii_case(name.trim()))
    }

    pub fn is_blocked(&self, track: &Track) -> Option<BlockedReason> {
        // 1. Check individual artists in track.artists
        for a in &track.artists {
            let low = a.name.trim().to_lowercase();
            if self.user_artists_set.contains(&low) {
                return Some(BlockedReason::Artist(a.name.clone()));
            }
            if self.soul_over_ai && self.soul_artists_set.contains(&low) {
                return Some(BlockedReason::SoulOverAi(a.name.clone()));
            }
        }

        // 2. Check full artist line against user blocklist
        let line = track.artist_line();
        for a in &self.artists {
            if whole_word_matches(&line, a) {
                return Some(BlockedReason::Artist(a.clone()));
            }
        }

        // 3. Check track ID and title
        for t in &self.tracks {
            if t == &track.id || t.eq_ignore_ascii_case(&track.title) {
                return Some(BlockedReason::Song(track.title.clone()));
            }
        }

        // 4. Check keywords in title
        for kw in &self.keywords {
            if whole_word_matches(&track.title, kw) {
                return Some(BlockedReason::Keyword(kw.clone()));
            }
        }

        None
    }

    pub fn block_artist(&mut self, name: &str) -> bool {
        let trimmed = name.trim();
        if trimmed.is_empty() {
            return false;
        }
        let low = trimmed.to_lowercase();
        if self.user_artists_set.contains(&low) {
            return false;
        }
        self.artists.push(trimmed.to_string());
        self.user_artists_set.insert(low);
        true
    }

    pub fn unblock_artist(&mut self, name: &str) -> bool {
        let trimmed = name.trim();
        let low = trimmed.to_lowercase();
        let had = self.user_artists_set.remove(&low);
        self.artists.retain(|a| !a.eq_ignore_ascii_case(trimmed));
        had
    }

    pub fn clear_artists(&mut self) -> bool {
        if self.artists.is_empty() {
            return false;
        }
        self.artists.clear();
        self.user_artists_set.clear();
        true
    }

    pub fn is_listen_blocked(&self, video_id: &str, title: &str, artist: &str) -> bool {
        if !artist.is_empty() && self.is_artist_blocked(artist) {
            return true;
        }
        if !title.is_empty() && (self.is_artist_blocked(title) || self.tracks.iter().any(|t| t == video_id || t.eq_ignore_ascii_case(title))) {
            return true;
        }
        false
    }

    pub fn block_song(&mut self, track: &Track) -> bool {
        if self.tracks.iter().any(|t| t == &track.id || t.eq_ignore_ascii_case(&track.title)) {
            return false;
        }
        self.tracks.push(track.title.clone());
        true
    }

    pub fn unblock_song(&mut self, id_or_title: &str) -> bool {
        let before = self.tracks.len();
        self.tracks.retain(|t| t != id_or_title && !t.eq_ignore_ascii_case(id_or_title));
        self.tracks.len() < before
    }

    pub fn share_with_ui(&self, ctx: &egui::Context) {
        ctx.data_mut(|d| {
            d.insert_temp(egui::Id::unique("active-blocklist"), Arc::new(self.clone()));
        });
    }
}

pub fn active_blocklist(ctx: &egui::Context) -> Option<Arc<Blocklist>> {
    ctx.data(|d| d.get_temp::<Arc<Blocklist>>(egui::Id::unique("active-blocklist")))
}

#[allow(dead_code)]
pub fn is_artist_blocked_ui(ctx: &egui::Context, artist: &str) -> bool {
    active_blocklist(ctx).is_some_and(|b| b.is_artist_blocked(artist))
}

pub fn is_user_artist_blocked_ui(ctx: &egui::Context, artist: &str) -> bool {
    active_blocklist(ctx).is_some_and(|b| b.is_user_artist_blocked(artist))
}

fn whole_word_matches(text: &str, term: &str) -> bool {
    let t_low = text.to_lowercase();
    let term_low = term.to_lowercase();
    if term_low.is_empty() {
        return false;
    }
    for (start, _) in t_low.match_indices(&term_low) {
        let end = start + term_low.len();
        let before_ok = start == 0 || !t_low[..start].chars().next_back().is_some_and(|c| c.is_alphanumeric());
        let after_ok = end == t_low.len() || !t_low[end..].chars().next().is_some_and(|c| c.is_alphanumeric());
        if before_ok && after_ok {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Link;

    fn make_track(title: &str, artist: &str) -> Track {
        Track {
            id: format!("id_{title}"),
            title: title.to_string(),
            artists: vec![Link { name: artist.to_string(), id: None }],
            album: None,
            duration: Some(180),
            thumbs: vec![],
            track_nr: None,
            plays: None,
            music_video: false,
            video: false,
            explicit: false,
            stands_for: None,
        }
    }

    #[test]
    fn test_word_matching() {
        assert!(whole_word_matches("Poppo - Song", "Poppo"));
        assert!(whole_word_matches("Poppo", "Poppo"));
        assert!(whole_word_matches("Song (feat. Poppo)", "Poppo"));
        assert!(!whole_word_matches("Poppop - Song", "Poppo"));
    }

    #[test]
    fn test_blocking_artist() {
        let mut bl = Blocklist::default();
        let track = make_track("Niji iro no kakehashi", "Poppo");
        assert_eq!(bl.is_blocked(&track), None);

        bl.block_artist("Poppo");
        assert_eq!(bl.is_blocked(&track), Some(BlockedReason::Artist("Poppo".into())));

        bl.unblock_artist("Poppo");
        assert_eq!(bl.is_blocked(&track), None);
    }

    #[test]
    fn test_soul_over_ai_blocking() {
        let bl = Blocklist::default();
        // 10mm is in the Soul Over AI list
        let track = make_track("Some Track", "10mm");
        assert_eq!(bl.is_blocked(&track), Some(BlockedReason::SoulOverAi("10mm".into())));

        let mut bl_disabled = bl.clone();
        bl_disabled.soul_over_ai = false;
        assert_eq!(bl_disabled.is_blocked(&track), None);
    }
}
