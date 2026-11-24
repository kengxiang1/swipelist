//! Swipelist — Tinder-style Spotify playlist curator built in pure Rust with Rune (`wgpu`).
//!
//! Navigation Flow: Source Playlist → Swipe → New Playlist
//!
//! Headless verification commands:
//!   cargo run -- --snap swiping.png --scene swiping
//!   cargo run -- --snap drag_add.png --scene drag-add
//!   cargo run -- --snap drag_skip.png --scene drag-skip
//!   cargo run -- --snap source_picker.png --scene source-picker
//!   cargo run -- --snap review.png --scene review
//!   cargo run -- --snap auth_modal.png --scene auth-modal

#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

mod icons;
mod spotify;

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::{Duration, Instant};

use rune::tok::TextStyle;
use rune::{
    AppSpec, Color, Cursor, Curve, Cx, Drag, Easing, El, FileDialogOptions, Image, ImageCache, Key,
    KeyEvent, Latest, Phase, Player, PlayerState, ScrollSpec, Spring, SurfaceFit, TextEdit,
    Transition, Tx, col, el, oklch, oklcha, row, spacer, text, text_input,
};

use crate::icons::{
    check_icon, chevron_left_icon, external_link_icon, heart_icon, image_icon, music_icon,
    pause_icon, play_icon, plus_icon, search_icon, shuffle_icon, undo_icon, vinyl_cover_art,
    volume_icon, x_icon,
};
use crate::spotify::{
    HistoryEntry, PkceRequest, PlaylistProgress, PlaylistSummary, ProgressStore, REDIRECT_URI,
    SourceFilter, SpotifyAuthSession, SwipeDecision, Track, UserProfile,
    await_callback_and_fetch_library, cancel_pending_oauth_listener, exchange_callback_query,
    fetch_cover_bytes, fetch_playlist_tracks, fetch_track_audio_file, format_seconds,
    format_total_duration, load_config, load_progress_store, prepare_pkce_request,
    restore_saved_session, save_config, save_progress_store, save_selection_to_spotify,
    starter_playlists, starter_tracks_for,
};

const WINDOW_SIZE: (f32, f32) = (1200.0, 760.0);

/// Responsive layout metrics derived proportionally from the live window viewport size.
#[derive(Clone, Copy)]
struct LayoutMetrics {
    win_w: f32,
    stage_w: f32,
    sidebar_w: f32,
    card_w: f32,
    card_h: f32,
    card_pad: f32,
    time_w: f32,
    slider_w: f32,
    vol_w: f32,
    dock_lg: f32,
    dock_sm: f32,
    scale: f32,
}

impl LayoutMetrics {
    fn from_viewport(win_w: f32, win_h: f32) -> Self {
        // Sidebar takes ~25% of window width, clamped cleanly for both laptop and ultrawide displays
        let sidebar_w = (win_w * 0.25).clamp(310.0, 440.0).round();
        let stage_w = (win_w - sidebar_w).max(460.0);

        // Swipe card scales proportionally to ~66% of window height (preserving 0.745 portrait ratio)
        let max_card_h = (win_h - 220.0).max(360.0);
        let target_h = (win_h * 0.66).min(max_card_h).clamp(390.0, 740.0);
        let card_w = (target_h * 0.745)
            .min(stage_w * 0.68)
            .clamp(320.0, 560.0)
            .round();
        let card_h = (card_w / 0.745)
            .min(max_card_h)
            .clamp(380.0, 740.0)
            .round();

        // Subtle proportional UI scale factor (1.0 at 380px card width, up to 1.32 on large screens)
        let scale = (card_w / 380.0).clamp(0.92, 1.32);

        let card_pad = (20.0 * scale).round();
        let inner_w = card_w - card_pad * 2.0;
        let time_w = (30.0 * scale).round();
        let slider_w = (inner_w - time_w * 2.0 - 16.0).max(120.0);
        let vol_w = (inner_w * 0.27).clamp(78.0, 132.0).round();

        let dock_lg = (64.0 * scale).clamp(56.0, 82.0).round();
        let dock_sm = (48.0 * scale).clamp(44.0, 62.0).round();

        Self {
            win_w,
            stage_w,
            sidebar_w,
            card_w,
            card_h,
            card_pad,
            time_w,
            slider_w,
            vol_w,
            dock_lg,
            dock_sm,
            scale,
        }
    }
}

// Apple WWDC "Designing Fluid Interfaces" Spring Presets (SwiftUI duration + bounce):
// - UI / Screen transitions: critically damped (bounce 0.0, response 0.34s) — graceful, no overshoot
// - Snappy tactile controls: critically damped (bounce 0.0, response 0.22s) — instant, responsive
// - Physical gesture / flick: momentum spring (bounce 0.20, response 0.40s) — damping ~0.8
// - Modal / Sheet: soft materialize spring (bounce 0.08, response 0.32s)
fn apple_spring_ui() -> Spring {
    Spring::new(0.34, 0.0)
}
fn apple_spring_snappy() -> Spring {
    Spring::new(0.22, 0.0)
}
fn apple_spring_gesture() -> Spring {
    Spring::new(0.40, 0.20)
}
fn apple_spring_sheet() -> Spring {
    Spring::new(0.32, 0.08)
}

/// Apple's progressive rubber-banding formula for soft drag boundaries.
fn apple_rubber_band(overshoot: f32, dimension: f32) -> f32 {
    let c = 0.55;
    (overshoot * dimension * c) / (dimension + c * overshoot.abs())
}

/// Apple WWDC 2018 exponential-decay momentum projection (`deceleration_rate = 0.992`).
fn apple_project_x(velocity_px_per_sec: f32) -> f32 {
    let decel = 0.992_f32;
    (velocity_px_per_sec / 1000.0) * decel / (1.0 - decel)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Screen {
    Swiping,
    SourcePicker,
    Review,
    Saved,
}

enum AsyncResult {
    LoggedIn(Result<SpotifyAuthSession, String>),
    SessionRestored(Result<SpotifyAuthSession, String>),
    TracksLoaded(PlaylistSummary, Result<Vec<Track>, String>),
    PlaylistSaved(Result<String, String>),
}

struct CoverJob {
    req_key: String,
    id: String,
    is_playlist: bool,
    image_url: Option<String>,
    spotify_track_id: Option<String>,
}

struct CoverLoaded {
    req_key: String,
    id: String,
    is_playlist: bool,
    bytes: Option<Vec<u8>>,
}

struct AudioLoaded {
    track_id: String,
    result: Result<PathBuf, String>,
}

struct ExitingCard {
    track: Track,
    index: usize,
    decision: SwipeDecision,
    started_at: Instant,
}

struct State {
    screen: Screen,
    user: Option<UserProfile>,
    access_token: String,
    client_id_edit: TextEdit,
    show_auth_modal: bool,
    auth_waiting: bool,
    pending_pkce: Option<PkceRequest>,
    auth_error: Option<String>,
    copied_redirect_uri: bool,
    status_banner: Option<String>,
    error_banner: Option<String>,

    // Source Playlist & Source Picker
    playlists: Vec<PlaylistSummary>,
    source_playlist: PlaylistSummary,
    source_filter: SourceFilter,
    search_edit: TextEdit,

    // Active Swipe Deck
    tracks: Vec<Track>,
    unshuffled_order: Vec<String>,
    current_index: usize,
    shuffled: bool,
    history: Vec<HistoryEntry>,

    // Selected Songs & New Playlist configuration
    selected_tracks: Vec<Track>,
    hovered_selected_id: Option<String>,
    hovered_btn: Option<&'static str>,
    new_playlist_name: TextEdit,
    new_playlist_desc: TextEdit,
    is_public: bool,
    custom_cover_path: Option<PathBuf>,
    custom_cover_image: Option<Image>,
    cover_palette_seed: usize,
    saved_playlist_url: Option<String>,

    // Auto-Save Progress Store (`swipelist_progress.json`)
    progress_store: ProgressStore,

    // Cover Art Cache
    user_avatar: Option<Image>,
    track_covers: HashMap<String, Image>,
    playlist_covers: HashMap<String, Image>,
    requested_covers: HashSet<String>,
    cover_failures: HashMap<String, u8>,
    cover_tx: Option<Sender<CoverJob>>,
    cover_rx: Option<Receiver<CoverLoaded>>,

    // Audio Preview Player & Draggable Slider
    audio_player: Option<Player>,
    playing_track_id: Option<String>,
    is_playing: bool,
    audio_loading: bool,
    slider_pos: f32,
    slider_dragging: bool,
    was_playing_before_scrub: bool,
    volume: f32,
    muted: bool,
    volume_dragging: bool,
    audio_duration_secs: f32,
    audio_tx: Option<Sender<Track>>,
    audio_rx: Option<Receiver<AudioLoaded>>,

    // Card Drag & Apple Fluid Momentum Physics
    drag_dx: f32,
    drag_dy: f32,
    drag_pivot_top: bool,
    is_dragging: bool,
    drag_samples: Vec<(Instant, f32)>,
    exiting_card: Option<ExitingCard>,

    // Background worker & image cache
    image_cache: Option<ImageCache>,
    waker: Option<rune::runtime::Waker>,
    worker: Latest<AsyncResult>,
}

impl Default for State {
    fn default() -> Self {
        let cfg = load_config();
        let playlists = starter_playlists();
        let source = playlists[0].clone();
        let all_tracks = starter_tracks_for(&source.id);
        let unshuffled_order: Vec<String> = all_tracks.iter().map(|t| t.id.clone()).collect();

        let selected_tracks = vec![
            all_tracks[0].clone(),
            all_tracks[1].clone(),
            all_tracks[2].clone(),
        ];
        let history = vec![
            HistoryEntry {
                track: all_tracks[0].clone(),
                decision: SwipeDecision::Add,
                index: 0,
            },
            HistoryEntry {
                track: all_tracks[1].clone(),
                decision: SwipeDecision::Add,
                index: 1,
            },
            HistoryEntry {
                track: all_tracks[2].clone(),
                decision: SwipeDecision::Add,
                index: 2,
            },
        ];

        let default_new_name = format!("{} • Selection", source.name);

        State {
            screen: Screen::Swiping,
            user: None,
            access_token: cfg.access_token.unwrap_or_default(),
            client_id_edit: TextEdit::new(cfg.client_id),
            show_auth_modal: false,
            auth_waiting: false,
            pending_pkce: None,
            auth_error: None,
            copied_redirect_uri: false,
            status_banner: None,
            error_banner: None,

            playlists,
            source_playlist: source,
            source_filter: SourceFilter::All,
            search_edit: TextEdit::new(""),

            tracks: all_tracks,
            unshuffled_order,
            current_index: 3,
            shuffled: false,
            history,

            selected_tracks,
            hovered_selected_id: None,
            hovered_btn: None,
            new_playlist_name: TextEdit::new(default_new_name),
            new_playlist_desc: TextEdit::new("Curated with Swipelist"),
            is_public: false,
            custom_cover_path: None,
            custom_cover_image: None,
            cover_palette_seed: 0,
            saved_playlist_url: None,

            progress_store: ProgressStore::default(),

            user_avatar: None,
            track_covers: HashMap::new(),
            playlist_covers: HashMap::new(),
            requested_covers: HashSet::new(),
            cover_failures: HashMap::new(),
            cover_tx: None,
            cover_rx: None,

            audio_player: None,
            playing_track_id: None,
            is_playing: false,
            audio_loading: false,
            slider_pos: 0.35,
            slider_dragging: false,
            was_playing_before_scrub: false,
            volume: 0.78,
            muted: false,
            volume_dragging: false,
            audio_duration_secs: 30.0,
            audio_tx: None,
            audio_rx: None,

            drag_dx: 0.0,
            drag_dy: 0.0,
            drag_pivot_top: true,
            is_dragging: false,
            drag_samples: Vec::with_capacity(8),
            exiting_card: None,

            image_cache: None,
            waker: None,
            worker: Latest::default(),
        }
    }
}

impl State {
    fn current_track(&self) -> Option<&Track> {
        self.tracks.get(self.current_index)
    }

    fn record_drag_sample(&mut self, dx: f32) {
        let now = Instant::now();
        self.drag_samples.push((now, dx));
        // Keep only the most recent 100ms of samples for responsive release velocity
        self.drag_samples
            .retain(|(t, _)| now.duration_since(*t) <= Duration::from_millis(100));
        if self.drag_samples.len() > 8 {
            let excess = self.drag_samples.len() - 8;
            self.drag_samples.drain(0..excess);
        }
    }

    fn release_velocity_x(&self) -> f32 {
        let (Some(&(t0, x0)), Some(&(t1, x1))) =
            (self.drag_samples.first(), self.drag_samples.last())
        else {
            return 0.0;
        };
        let dt = t1.duration_since(t0).as_secs_f32();
        if dt < 0.004 {
            return 0.0;
        }
        ((x1 - x0) / dt).clamp(-3200.0, 3200.0)
    }

    fn stop_audio(&mut self) {
        if let Some(player) = &self.audio_player {
            player.pause();
        }
        self.audio_player = None;
        self.playing_track_id = None;
        self.is_playing = false;
        self.audio_loading = false;
        self.slider_pos = 0.0;
        self.slider_dragging = false;
        self.was_playing_before_scrub = false;
        self.audio_duration_secs = 30.0;
    }

    fn toggle_play_current_track(&mut self) {
        let Some(track) = self.current_track().cloned() else {
            return;
        };

        // If already loaded for this track, toggle play/pause
        if self.playing_track_id.as_deref() == Some(track.id.as_str())
            && let Some(player) = &self.audio_player
        {
            if self.is_playing {
                player.pause();
                self.is_playing = false;
            } else {
                let target_sec = f64::from(self.slider_pos * self.audio_duration_secs);
                player.seek(target_sec);
                player.set_muted(false);
                player.set_volume(Self::perceptual_gain(self.volume, self.muted));
                player.play();
                self.is_playing = true;
            }
            return;
        }

        // Request background download of the track's MP3 audio preview
        self.playing_track_id = Some(track.id.clone());
        self.audio_loading = true;
        self.is_playing = false;
        if let Some(tx) = &self.audio_tx {
            let _ = tx.send(track);
        }
    }

    /// Pauses audio immediately when the user starts dragging the seek slider.
    fn begin_scrub(&mut self, norm_pos: f32) {
        self.slider_dragging = true;
        self.was_playing_before_scrub = self.is_playing;
        if self.is_playing {
            if let Some(player) = &self.audio_player {
                player.pause();
            }
            self.is_playing = false;
        }
        self.slider_pos = norm_pos.clamp(0.0, 1.0);
    }

    /// Commits the dragged seek position and resumes playback only if it was playing before dragging.
    fn finish_scrub(&mut self, norm_pos: f32) {
        let clamped = norm_pos.clamp(0.0, 1.0);
        let should_resume = self.was_playing_before_scrub;
        self.slider_dragging = false;
        self.was_playing_before_scrub = false;
        self.slider_pos = clamped;

        let Some(track) = self.current_track().cloned() else {
            return;
        };

        if self.playing_track_id.as_deref() == Some(track.id.as_str())
            && let Some(player) = &self.audio_player
        {
            let target_sec = f64::from(clamped * self.audio_duration_secs);
            player.seek(target_sec);
            if should_resume {
                player.play();
                self.is_playing = true;
            }
        }
    }

    fn cancel_scrub(&mut self, original_pos: f32) {
        let should_resume = self.was_playing_before_scrub;
        self.slider_dragging = false;
        self.was_playing_before_scrub = false;
        self.slider_pos = original_pos.clamp(0.0, 1.0);
        if should_resume
            && let Some(player) = &self.audio_player
        {
            player.play();
            self.is_playing = true;
        }
    }

    /// Maps UI slider [0.0, 1.0] to a perceptual logarithmic-like gain curve (`v^3`, ~60 dB taper)
    /// so volume changes feel even across the full 0%–100% range. Always returns 0.0 when muted
    /// instead of deselecting the Media Foundation audio stream.
    fn perceptual_gain(ui_volume: f32, muted: bool) -> f32 {
        if muted || ui_volume <= 0.005 {
            0.0
        } else {
            let v = ui_volume.clamp(0.0, 1.0);
            v * v * v
        }
    }

    fn apply_audio_volume(&self) {
        if let Some(player) = &self.audio_player {
            player.set_muted(false);
            player.set_volume(Self::perceptual_gain(self.volume, self.muted));
        }
    }

    fn set_volume(&mut self, vol: f32) {
        let clamped = vol.clamp(0.0, 1.0);
        self.volume = clamped;
        self.muted = clamped <= 0.005;
        self.apply_audio_volume();
    }

    fn adjust_volume(&mut self, delta: f32) {
        let base = if self.muted { 0.0 } else { self.volume };
        let next = (base + delta).clamp(0.0, 1.0);
        self.volume = next;
        self.muted = next <= 0.005;
        self.apply_audio_volume();
    }

    fn toggle_mute(&mut self) {
        if self.muted || self.volume <= 0.005 {
            self.muted = false;
            if self.volume <= 0.005 {
                self.volume = 0.65;
            }
        } else {
            self.muted = true;
        }
        self.apply_audio_volume();
    }

    /// Saves current playlist session progress automatically to `swipelist_progress.json`.
    fn auto_save_progress(&mut self) {
        if cfg!(test) || rune::tooling::headless_run().is_some() {
            return;
        }
        let pid = self.source_playlist.id.clone();
        let entry = PlaylistProgress {
            playlist_id: pid.clone(),
            playlist_name: self.source_playlist.name.clone(),
            current_index: self.current_index,
            total_tracks: self.tracks.len(),
            new_playlist_name: self.new_playlist_name.text.clone(),
            new_playlist_desc: self.new_playlist_desc.text.clone(),
            is_public: self.is_public,
            custom_cover_path: self
                .custom_cover_path
                .as_ref()
                .map(|p| p.to_string_lossy().into_owned()),
            selected_tracks: self.selected_tracks.clone(),
            history: self.history.clone(),
        };
        self.progress_store.active_playlist_id = Some(pid.clone());
        self.progress_store.sessions.insert(pid, entry);
        save_progress_store(&self.progress_store);
    }

    /// Restores saved progress for `self.source_playlist.id` if available in `swipelist_progress.json`.
    fn restore_progress_for_current_playlist(&mut self) -> bool {
        let pid = &self.source_playlist.id;
        let Some(saved) = self.progress_store.sessions.get(pid).cloned() else {
            return false;
        };
        if self.tracks.is_empty() {
            return false;
        }
        self.selected_tracks = saved.selected_tracks;
        self.history = saved.history;

        // Keep `current_index` synced with the actual tracks vector even if the playlist
        // order shifted slightly on Spotify or Shuffle was used in an earlier session.
        let mut idx = saved.current_index.min(self.tracks.len());
        if let Some(last) = self.history.last() {
            let expected_prev = idx.checked_sub(1).and_then(|i| self.tracks.get(i));
            if expected_prev.map(|t| t.id.as_str()) != Some(last.track.id.as_str())
                && let Some(found) = self.tracks.iter().position(|t| t.id == last.track.id)
            {
                idx = (found + 1).min(self.tracks.len());
            }
        }
        self.current_index = idx.min(self.tracks.len());
        if !saved.new_playlist_name.trim().is_empty() {
            self.new_playlist_name.replace_text(&saved.new_playlist_name);
        }
        if !saved.new_playlist_desc.trim().is_empty() {
            self.new_playlist_desc.replace_text(&saved.new_playlist_desc);
        }
        self.is_public = saved.is_public;
        if let Some(cover_str) = saved.custom_cover_path {
            let path = PathBuf::from(cover_str);
            if path.exists() {
                if let Ok(bytes) = std::fs::read(&path)
                    && let Some(cache) = &self.image_cache
                {
                    self.custom_cover_image = Some(cache.load(bytes));
                }
                self.custom_cover_path = Some(path);
            }
        }
        true
    }

    fn reset_current_playlist_progress(&mut self) {
        self.stop_audio();
        self.exiting_card = None;
        self.current_index = 0;
        self.selected_tracks.clear();
        self.history.clear();
        let default_name = format!("{} • Selection", self.source_playlist.name);
        self.new_playlist_name.replace_text(&default_name);
        self.auto_save_progress();
    }

    /// Records a failed cover download attempt and removes `req_key` from `requested_covers`
    /// once so a transient error can be retried once without looping infinitely.
    fn record_cover_failure(
        cover_failures: &mut HashMap<String, u8>,
        requested_covers: &mut HashSet<String>,
        req_key: &str,
    ) -> bool {
        let next_count = cover_failures
            .get(req_key)
            .copied()
            .unwrap_or(0)
            .saturating_add(1);
        if next_count < 2 {
            cover_failures.insert(req_key.to_string(), next_count);
            requested_covers.remove(req_key);
            true
        } else {
            cover_failures.remove(req_key);
            false
        }
    }

    /// Queues background cover-art downloads for user avatar, visible cards, selected songs, and source playlists.
    fn queue_visible_covers(&mut self) {
        let Some(tx) = &self.cover_tx else {
            return;
        };

        // 0. Spotify user profile avatar
        if let Some(user) = &self.user
            && let Some(url) = &user.image_url
        {
            let key = format!("avatar:{}", user.id);
            if self.requested_covers.insert(key.clone()) {
                let _ = tx.send(CoverJob {
                    req_key: key,
                    id: "__user_avatar__".to_string(),
                    is_playlist: false,
                    image_url: Some(url.clone()),
                    spotify_track_id: None,
                });
            }
        }

        // 1. Current & upcoming 5 tracks in swipe deck
        for i in self.current_index..=(self.current_index + 5).min(self.tracks.len().saturating_sub(1)) {
            if let Some(t) = self.tracks.get(i) {
                let key = format!("track:{}", t.id);
                if self.requested_covers.insert(key.clone()) {
                    let _ = tx.send(CoverJob {
                        req_key: key,
                        id: t.id.clone(),
                        is_playlist: false,
                        image_url: t.image_url.clone(),
                        spotify_track_id: Some(t.spotify_track_id().to_string()),
                    });
                }
            }
        }

        // 2. Selected songs
        for t in &self.selected_tracks {
            let key = format!("track:{}", t.id);
            if self.requested_covers.insert(key.clone()) {
                let _ = tx.send(CoverJob {
                    req_key: key,
                    id: t.id.clone(),
                    is_playlist: false,
                    image_url: t.image_url.clone(),
                    spotify_track_id: Some(t.spotify_track_id().to_string()),
                });
            }
        }

        // 3. Source playlists with image URLs
        for pl in &self.playlists {
            if pl.image_url.is_some() {
                let key = format!("playlist:{}", pl.id);
                if self.requested_covers.insert(key.clone()) {
                    let _ = tx.send(CoverJob {
                        req_key: key,
                        id: pl.id.clone(),
                        is_playlist: true,
                        image_url: pl.image_url.clone(),
                        spotify_track_id: None,
                    });
                }
            }
        }
    }

    fn open_auth_modal(&mut self) {
        self.show_auth_modal = true;
        self.auth_error = None;
        self.copied_redirect_uri = false;
        self.client_id_edit.focused = true;
    }

    fn close_auth_modal(&mut self) {
        if self.auth_waiting {
            cancel_pending_oauth_listener();
            self.auth_waiting = false;
        }
        self.show_auth_modal = false;
        self.auth_error = None;
        self.client_id_edit.focused = false;
    }

    fn start_spotify_oauth(&mut self, tx: &mut Tx) {
        self.auth_error = None;
        let cid = self.client_id_edit.text.trim().to_string();
        match prepare_pkce_request(&cid) {
            Ok(pkce) => {
                let mut cfg = load_config();
                cfg.client_id = pkce.client_id.clone();
                save_config(&cfg);

                if let Err(e) = tx.open_url(&pkce.auth_url) {
                    self.auth_error = Some(format!("Could not open browser: {e}"));
                    return;
                }

                self.auth_waiting = true;
                self.pending_pkce = Some(pkce.clone());
                self.worker.request(pkce.state.clone(), move |_| {
                    AsyncResult::LoggedIn(await_callback_and_fetch_library(&pkce))
                });
            }
            Err(msg) => {
                self.auth_error = Some(msg);
            }
        }
    }

    fn commit_swipe(&mut self, decision: SwipeDecision) {
        self.commit_swipe_for(decision, None);
    }

    /// Commits a swipe decision, optionally verifying that the active card's track ID matches
    /// the card the user actually interacted with so a stale gesture can never add the wrong song.
    fn commit_swipe_for(&mut self, decision: SwipeDecision, expected_track_id: Option<&str>) {
        let Some(track) = self.tracks.get(self.current_index).cloned() else {
            return;
        };
        if let Some(expected) = expected_track_id
            && track.id != expected
        {
            self.drag_dx = 0.0;
            self.drag_dy = 0.0;
            self.is_dragging = false;
            return;
        }

        self.stop_audio();
        self.drag_dx = 0.0;
        self.drag_dy = 0.0;
        self.is_dragging = false;

        self.exiting_card = Some(ExitingCard {
            track: track.clone(),
            index: self.current_index,
            decision,
            started_at: Instant::now(),
        });

        self.history.push(HistoryEntry {
            track: track.clone(),
            decision,
            index: self.current_index,
        });

        if decision == SwipeDecision::Add
            && !self.selected_tracks.iter().any(|t| t.id == track.id)
        {
            self.selected_tracks.push(track);
        }

        self.current_index += 1;
        self.auto_save_progress();
        self.queue_visible_covers();

        if self.current_index >= self.tracks.len() {
            self.screen = Screen::Review;
        }
    }

    /// Undo returns the last card to the top of the stack and removes it from the selection if it was added.
    fn undo_last_swipe(&mut self) {
        self.stop_audio();
        self.drag_dx = 0.0;
        self.drag_dy = 0.0;
        self.is_dragging = false;
        self.exiting_card = None;

        let Some(last) = self.history.pop() else {
            return;
        };

        if last.decision == SwipeDecision::Add {
            self.selected_tracks.retain(|t| t.id != last.track.id);
        }

        // Guarantee `current_index` points to the exact undone track even if shuffle/order changed
        if self
            .tracks
            .get(last.index)
            .map(|t| t.id.as_str())
            == Some(last.track.id.as_str())
        {
            self.current_index = last.index;
        } else if let Some(actual_pos) = self.tracks.iter().position(|t| t.id == last.track.id) {
            let target = last.index.min(self.tracks.len().saturating_sub(1));
            self.tracks.swap(actual_pos, target);
            self.current_index = target;
        } else {
            self.current_index = last.index.min(self.tracks.len().saturating_sub(1));
        }

        if self.screen == Screen::Review {
            self.screen = Screen::Swiping;
        }
        self.auto_save_progress();
        self.queue_visible_covers();
    }

    fn select_source_playlist(&mut self, playlist: PlaylistSummary) {
        self.stop_audio();
        self.exiting_card = None;
        let new_name = format!("{} • Selection", playlist.name);
        self.new_playlist_name.replace_text(&new_name);
        self.source_playlist = playlist.clone();
        self.drag_dx = 0.0;
        self.drag_dy = 0.0;
        self.is_dragging = false;
        self.history.clear();
        self.selected_tracks.clear();
        self.current_index = 0;
        self.shuffled = false;

        let is_starter_demo = matches!(
            playlist.id.as_str(),
            "late-night-electronic"
                | "indie-synthwave"
                | "deep-focus-instrumental"
                | "weekend-groove-crate"
                | "workout-high-tempo"
        );

        if !self.access_token.is_empty() && !is_starter_demo {
            self.status_banner = Some(format!(
                "Loading all tracks from \"{}\"…",
                playlist.name
            ));
            let token = self.access_token.clone();
            let pl = playlist.clone();
            self.worker.request(pl.id.clone(), move |id| {
                AsyncResult::TracksLoaded(pl, fetch_playlist_tracks(&token, &id))
            });
        } else {
            self.tracks = starter_tracks_for(&playlist.id);
            self.unshuffled_order = self.tracks.iter().map(|t| t.id.clone()).collect();
            self.shuffled = false;
            self.restore_progress_for_current_playlist();
            self.auto_save_progress();
            self.queue_visible_covers();
            self.screen = Screen::Swiping;
        }
    }

    fn toggle_shuffle(&mut self) {
        self.shuffled = !self.shuffled;
        if self.current_index + 1 < self.tracks.len() {
            if self.shuffled {
                let mut rng = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_nanos() as u64
                    ^ (self as *const State as usize as u64);
                if rng == 0 {
                    rng = 0x9e37_79b9_7f4a_7c15;
                }
                let tail = &mut self.tracks[self.current_index + 1..];
                for i in (1..tail.len()).rev() {
                    rng ^= rng << 13;
                    rng ^= rng >> 7;
                    rng ^= rng << 17;
                    let j = (rng as usize) % (i + 1);
                    tail.swap(i, j);
                }
            } else {
                let pos_map: HashMap<&str, usize> = self
                    .unshuffled_order
                    .iter()
                    .enumerate()
                    .map(|(idx, id)| (id.as_str(), idx))
                    .collect();
                let tail = &mut self.tracks[self.current_index + 1..];
                tail.sort_by_key(|t| pos_map.get(t.id.as_str()).copied().unwrap_or(usize::MAX));
            }
        }
        self.queue_visible_covers();
    }

    fn remove_from_selection(&mut self, track_id: &str) {
        self.selected_tracks.retain(|t| t.id != track_id);
        self.auto_save_progress();
    }

    fn trigger_save_to_spotify(&mut self) {
        if self.selected_tracks.is_empty() {
            self.error_banner =
                Some("Add at least one song to your selection before saving.".into());
            return;
        }
        let name = {
            let trimmed = self.new_playlist_name.text.trim();
            if trimmed.is_empty() {
                format!("{} • Selection", self.source_playlist.name)
            } else {
                trimmed.to_string()
            }
        };
        let desc = self.new_playlist_desc.text.trim().to_string();
        let is_public = self.is_public;
        let tracks = self.selected_tracks.clone();
        let token = self.access_token.clone();
        let cover = self.custom_cover_path.clone();

        if token.is_empty() {
            self.saved_playlist_url = Some("https://open.spotify.com".into());
            self.screen = Screen::Saved;
            return;
        }

        self.status_banner = Some(format!("Saving \"{name}\" to Spotify…"));
        self.worker.request(name.clone(), move |_| {
            AsyncResult::PlaylistSaved(save_selection_to_spotify(
                &token,
                &name,
                &desc,
                is_public,
                &tracks,
                cover.as_deref(),
            ))
        });
    }
}

// ============================================================================
// Flat Design Palette (Tonal Surfaces, Zero Clutter Borders)
// ============================================================================

struct Palette {
    bg_app: Color,
    bg_sidebar: Color,
    bg_flat_block: Color,
    bg_flat_pill: Color,
    bg_card: Color,
    text_primary: Color,
    text_secondary: Color,
    text_muted: Color,
    add_green: Color,
    add_green_soft: Color,
    skip_coral: Color,
    skip_coral_soft: Color,
    undo_amber: Color,
    undo_amber_soft: Color,
    hover_fill: Color,
    press_fill: Color,
}

fn palette() -> Palette {
    Palette {
        bg_app: oklch(0.112, 0.006, 265.0),
        bg_sidebar: oklch(0.148, 0.008, 265.0),
        bg_flat_block: oklch(0.182, 0.009, 265.0),
        bg_flat_pill: oklch(0.215, 0.011, 265.0),
        bg_card: oklch(0.175, 0.010, 265.0),
        text_primary: oklch(0.975, 0.003, 265.0),
        text_secondary: oklch(0.74, 0.009, 265.0),
        text_muted: oklch(0.54, 0.009, 265.0),
        add_green: oklch(0.76, 0.20, 152.0),
        add_green_soft: oklcha(0.76, 0.20, 152.0, 0.14),
        skip_coral: oklch(0.68, 0.22, 18.0),
        skip_coral_soft: oklcha(0.68, 0.22, 18.0, 0.15),
        undo_amber: oklch(0.80, 0.17, 75.0),
        undo_amber_soft: oklcha(0.80, 0.17, 75.0, 0.15),
        hover_fill: oklcha(1.0, 0.0, 0.0, 0.055),
        press_fill: oklcha(1.0, 0.0, 0.0, 0.095),
    }
}

// ============================================================================
// Setup, Poll & Keyboard Handlers
// ============================================================================

fn setup(s: &mut State, waker: rune::runtime::Waker) {
    let cache = ImageCache::new(waker.clone());
    s.waker = Some(waker.clone());
    s.worker.set_waker(waker.clone());

    if rune::tooling::headless_run().is_some() {
        for idx in [0usize, 1, 2, 3, 4] {
            if let Some(t) = s.tracks.get(idx)
                && let Some(bytes) =
                    fetch_cover_bytes(t.image_url.as_deref(), Some(t.spotify_track_id()))
            {
                let img = cache.load(bytes);
                let _ = img.wait();
                s.track_covers.insert(t.id.clone(), img);
            }
        }
        s.image_cache = Some(cache);
        return;
    }
    s.image_cache = Some(cache);

    // 1. Restore saved progress from `swipelist_progress.json`
    s.progress_store = load_progress_store();
    s.restore_progress_for_current_playlist();

    // 2. Spawn background Cover Art downloader thread
    let (job_tx, job_rx) = channel::<CoverJob>();
    let (done_tx, done_rx) = channel::<CoverLoaded>();
    let cover_waker = waker.clone();
    std::thread::spawn(move || {
        while let Ok(job) = job_rx.recv() {
            let bytes = fetch_cover_bytes(
                job.image_url.as_deref(),
                job.spotify_track_id.as_deref(),
            );
            let _ = done_tx.send(CoverLoaded {
                req_key: job.req_key,
                id: job.id,
                is_playlist: job.is_playlist,
                bytes,
            });
            cover_waker();
        }
    });
    s.cover_tx = Some(job_tx);
    s.cover_rx = Some(done_rx);

    // 3. Spawn background Audio Preview MP3 downloader thread
    let (aud_job_tx, aud_job_rx) = channel::<Track>();
    let (aud_done_tx, aud_done_rx) = channel::<AudioLoaded>();
    let audio_waker = waker;
    std::thread::spawn(move || {
        while let Ok(track) = aud_job_rx.recv() {
            let res = fetch_track_audio_file(&track);
            let _ = aud_done_tx.send(AudioLoaded {
                track_id: track.id,
                result: res,
            });
            audio_waker();
        }
    });
    s.audio_tx = Some(aud_job_tx);
    s.audio_rx = Some(aud_done_rx);

    s.queue_visible_covers();

    // 4. Automatically restore saved Spotify session in interactive window launches
    if !s.client_id_edit.text.trim().is_empty() {
        let cfg = load_config();
        if cfg.refresh_token.is_some() || cfg.access_token.is_some() {
            s.worker.request("restore".to_string(), |_| {
                AsyncResult::SessionRestored(restore_saved_session())
            });
        }
    }
}

fn poll(s: &mut State, _clock: f64, tx: &mut Tx) -> bool {
    let mut changed = false;

    // 0. Clear finished exiting swipe card once its flight animation settles
    if let Some(exit) = &s.exiting_card {
        if exit.started_at.elapsed() >= Duration::from_millis(380) {
            s.exiting_card = None;
            changed = true;
        } else {
            tx.wake_after(Duration::from_millis(45));
        }
    }

    // 1. Drain newly downloaded cover art bytes into Rune's GPU ImageCache
    let mut retry_covers = false;
    if let (Some(rx), Some(cache)) = (&s.cover_rx, &s.image_cache) {
        while let Ok(loaded) = rx.try_recv() {
            if let Some(bytes) = loaded.bytes {
                let img = cache.load(bytes);
                if loaded.id == "__user_avatar__" {
                    s.user_avatar = Some(img);
                } else if loaded.is_playlist {
                    s.playlist_covers.insert(loaded.id, img);
                } else {
                    s.track_covers.insert(loaded.id, img);
                }
                s.cover_failures.remove(&loaded.req_key);
                changed = true;
            } else if State::record_cover_failure(
                &mut s.cover_failures,
                &mut s.requested_covers,
                &loaded.req_key,
            ) {
                retry_covers = true;
            }
        }
    }
    if retry_covers {
        s.queue_visible_covers();
    }

    // 2. Drain newly downloaded audio MP3 file and start Rune's native audio Player
    if let Some(rx) = &s.audio_rx {
        while let Ok(loaded) = rx.try_recv() {
            if s.playing_track_id.as_deref() == Some(loaded.track_id.as_str()) {
                s.audio_loading = false;
                match loaded.result {
                    Ok(path) => {
                        let player = Player::audio(path);
                        if let Some(w) = &s.waker {
                            player.set_waker(w.clone());
                        }
                        player.set_muted(false);
                        player.set_volume(State::perceptual_gain(s.volume, s.muted));
                        if s.slider_pos > 0.01 {
                            player.seek(f64::from(s.slider_pos * s.audio_duration_secs));
                        }
                        player.play();
                        s.audio_player = Some(player);
                        s.is_playing = true;
                    }
                    Err(e) => {
                        s.error_banner = Some(e);
                        s.is_playing = false;
                    }
                }
                changed = true;
            }
        }
    }

    // 3. Update live audio player progress bar while playing
    if let Some(player) = &s.audio_player {
        if let Some(dur) = player.duration()
            && dur > 1.0
        {
            s.audio_duration_secs = dur as f32;
        }
        match player.state() {
            PlayerState::Ended => {
                if s.is_playing {
                    s.is_playing = false;
                    s.slider_pos = 0.0;
                    changed = true;
                }
            }
            PlayerState::Failed(msg) => {
                s.is_playing = false;
                s.error_banner = Some(format!("Audio playback error: {msg}"));
                s.audio_player = None;
                changed = true;
            }
            _ => {
                if s.is_playing && !s.slider_dragging {
                    let t = player.time() as f32;
                    let dur = s.audio_duration_secs.max(1.0);
                    let next_pos = (t / dur).clamp(0.0, 1.0);
                    if (next_pos - s.slider_pos).abs() > 0.003 {
                        s.slider_pos = next_pos;
                        changed = true;
                    }
                    tx.wake_after(Duration::from_millis(50));
                }
            }
        }
    }

    // 4. Process main worker results (OAuth login, playlist tracks fetch, save playlist)
    if let Some(res) = s.worker.take() {
        s.status_banner = None;
        changed = true;
        match res {
            AsyncResult::LoggedIn(Ok(session)) => {
                s.auth_waiting = false;
                s.pending_pkce = None;
                s.auth_error = None;
                s.user = Some(session.user);
                s.access_token = session.access_token;
                s.playlists = session.playlists;
                s.show_auth_modal = false;
                s.queue_visible_covers();
                let target_pl = s
                    .progress_store
                    .active_playlist_id
                    .as_ref()
                    .and_then(|id| s.playlists.iter().find(|p| &p.id == id).cloned())
                    .or_else(|| s.playlists.first().cloned());
                if let Some(pl) = target_pl {
                    s.select_source_playlist(pl);
                }
            }
            AsyncResult::LoggedIn(Err(e)) => {
                s.auth_waiting = false;
                if e != "OAuth login cancelled." {
                    s.auth_error = Some(e);
                }
            }
            AsyncResult::SessionRestored(Ok(session)) => {
                s.user = Some(session.user);
                s.access_token = session.access_token;
                s.playlists = session.playlists;
                s.queue_visible_covers();
                let target_pl = s
                    .progress_store
                    .active_playlist_id
                    .as_ref()
                    .and_then(|id| s.playlists.iter().find(|p| &p.id == id).cloned())
                    .or_else(|| s.playlists.first().cloned());
                if let Some(pl) = target_pl {
                    s.select_source_playlist(pl);
                }
            }
            AsyncResult::SessionRestored(Err(_)) => {
                s.access_token.clear();
            }
            AsyncResult::TracksLoaded(pl, Ok(tracks)) => {
                if tracks.is_empty() {
                    s.error_banner = Some(format!("\"{}\" has no playable tracks.", pl.name));
                } else {
                    s.source_playlist.track_count = tracks.len();
                    if let Some(item) = s.playlists.iter_mut().find(|p| p.id == pl.id) {
                        item.track_count = tracks.len();
                    }
                    s.tracks = tracks;
                    s.unshuffled_order = s.tracks.iter().map(|t| t.id.clone()).collect();
                    s.current_index = 0;
                    s.shuffled = false;
                    s.restore_progress_for_current_playlist();
                    s.auto_save_progress();
                    s.queue_visible_covers();
                    s.screen = Screen::Swiping;
                }
            }
            AsyncResult::TracksLoaded(_, Err(e)) => {
                s.error_banner = Some(e);
            }
            AsyncResult::PlaylistSaved(Ok(url)) => {
                s.saved_playlist_url = Some(url);
                s.screen = Screen::Saved;
            }
            AsyncResult::PlaylistSaved(Err(e)) => {
                s.error_banner = Some(e);
            }
        }
    }

    changed
}

fn on_key(s: &mut State, event: &KeyEvent, tx: &mut Tx) -> bool {
    if tx.focused().is_some() {
        return false;
    }
    if s.show_auth_modal {
        if matches!(event.key, Key::Escape) {
            s.close_auth_modal();
            tx.animated();
            return true;
        }
        return false;
    }
    if s.screen == Screen::Swiping {
        match &event.key {
            Key::Right => {
                s.commit_swipe(SwipeDecision::Add);
                tx.animated();
                return true;
            }
            Key::Left => {
                s.commit_swipe(SwipeDecision::Skip);
                tx.animated();
                return true;
            }
            Key::Up => {
                s.adjust_volume(0.05);
                tx.animated();
                return true;
            }
            Key::Down => {
                s.adjust_volume(-0.05);
                tx.animated();
                return true;
            }
            Key::Backspace => {
                s.undo_last_swipe();
                tx.animated();
                return true;
            }
            Key::Text(ch) if ch == " " => {
                s.toggle_play_current_track();
                return true;
            }
            Key::Text(ch) if ch.eq_ignore_ascii_case("z") => {
                s.undo_last_swipe();
                tx.animated();
                return true;
            }
            Key::Text(ch) if ch.eq_ignore_ascii_case("o") => {
                if let Some(track) = s.current_track() {
                    let _ = tx.open_url(&track.spotify_url());
                }
                return true;
            }
            _ => {}
        }
    } else if matches!(event.key, Key::Escape) && s.screen != Screen::Swiping {
        s.screen = Screen::Swiping;
        tx.animated();
        return true;
    }
    false
}

// ============================================================================
// Main View Builder: Two-Pane Tinder Studio (Flat Left Panel + Center Stage)
// ============================================================================

fn view(s: &State, cx: &mut Cx<State>) -> El {
    let p = palette();
    let [win_w, win_h] = cx
        .layout_size("app-root")
        .unwrap_or([WINDOW_SIZE.0, WINDOW_SIZE.1]);
    let m = LayoutMetrics::from_viewport(win_w, win_h);

    let mut root = row()
        .key("app-root")
        .w_full()
        .h_full()
        .fill(p.bg_app)
        .child(left_sidebar(s, &p, &m))
        .child(center_stage(s, &p, &m));

    if s.show_auth_modal {
        root = root.child(auth_modal_view(s, &p, &m));
    }

    root
}

/// Helper to render a small cover thumbnail for a track (real cover art if loaded, else flat hue icon).
fn track_thumb_el(s: &State, track: &Track, size: f32, radius: f32) -> El {
    if let Some(img) = s.track_covers.get(&track.id) {
        el().size(size, size)
            .radius(radius)
            .clip_rounded(radius)
            .fill(oklch(0.28, 0.08, track.accent_hue))
            .pixels_fit(img.pixels(), SurfaceFit::Fill)
    } else {
        el().size(size, size)
            .radius(radius)
            .fill(oklch(0.38, 0.14, track.accent_hue))
            .center()
            .child(music_icon(size * 0.44, Color::WHITE))
    }
}

/// Helper to render a cover thumbnail for a playlist (real cover art if loaded, else flat hue icon).
fn playlist_thumb_el(s: &State, pl: &PlaylistSummary, size: f32, radius: f32) -> El {
    if let Some(img) = s.playlist_covers.get(&pl.id) {
        el().size(size, size)
            .radius(radius)
            .clip_rounded(radius)
            .fill(oklch(0.28, 0.08, pl.accent_hue))
            .pixels_fit(img.pixels(), SurfaceFit::Fill)
    } else {
        el().size(size, size)
            .radius(radius)
            .fill(oklch(0.38, 0.15, pl.accent_hue))
            .center()
            .child(if pl.is_liked_songs {
                heart_icon(size * 0.44, Color::WHITE, true)
            } else {
                music_icon(size * 0.44, Color::WHITE)
            })
    }
}

// ============================================================================
// Left Sidebar: Pure Flat Design (Tonal Surfaces, No Outlines)
// ============================================================================

fn left_sidebar(s: &State, p: &Palette, m: &LayoutMetrics) -> El {
    let count = s.selected_tracks.len();
    let duration_str = format_total_duration(&s.selected_tracks);

    // 1. Flat Brand Header + Circular Spotify Profile Picture (or Connect Spotify Pill)
    let auth_control = if let Some(user) = &s.user {
        let initial = user
            .display_name
            .chars()
            .next()
            .unwrap_or('S')
            .to_uppercase()
            .to_string();
        let avatar_hovered = s.hovered_btn == Some("user-avatar");
        let hover_in = Curve::tween(0.09, Easing::cubic(0.42, 0.0, 0.58, 1.0));
        let hover_out = Curve::tween(0.11, Easing::cubic(0.42, 0.0, 0.58, 1.0));
        let bounce = Spring::new(0.18, 0.30);

        let mut avatar = el()
            .key("btn-connect-spotify")
            .role("button")
            .label(format!("{} • Spotify Account", user.display_name))
            .help(format!("{} • Spotify Account", user.display_name))
            .size(34.0, 34.0)
            .radius(17.0)
            .clip_rounded(17.0)
            .center()
            .scale(if avatar_hovered { 1.10 } else { 1.0 })
            .fill(p.add_green_soft)
            .hover(p.hover_fill)
            .hover_motion(hover_in, hover_out)
            .motion(bounce)
            .press_scale(0.93)
            .cursor(Cursor::Pointer)
            .on_hover_change(|s: &mut State, inside: bool, tx: &mut Tx| {
                if inside {
                    s.hovered_btn = Some("user-avatar");
                } else if s.hovered_btn == Some("user-avatar") {
                    s.hovered_btn = None;
                }
                tx.animated();
            })
            .on_press_tx(|s: &mut State, tx: &mut Tx| {
                s.open_auth_modal();
                tx.animated();
            });

        if let Some(img) = &s.user_avatar {
            avatar = avatar.pixels_fit(img.pixels(), SurfaceFit::Fill);
        } else {
            avatar = avatar.child(
                text(initial)
                    .font(TextStyle::new(14.0, 800.0))
                    .color(p.add_green),
            );
        }
        avatar
    } else {
        row()
            .key("btn-connect-spotify")
            .role("button")
            .label("Connect Spotify")
            .h(30.0)
            .px(12.0)
            .radius(15.0)
            .center_y()
            .gap(7.0)
            .fill(p.bg_flat_block)
            .hover(p.hover_fill)
            .press(p.press_fill)
            .press_scale(0.96)
            .cursor(Cursor::Pointer)
            .on_press_tx(|s: &mut State, tx: &mut Tx| {
                s.open_auth_modal();
                tx.animated();
            })
            .child(el().size(7.0, 7.0).radius(3.5).fill(p.undo_amber))
            .child(
                text("Connect Spotify")
                    .font(TextStyle::new(11.5, 650.0))
                    .color(p.text_primary)
                    .truncate(),
            )
    };

    let brand_row = row()
        .key("sidebar-brand-row")
        .w_full()
        .center_y()
        .justify_between()
        .child(
            row()
                .gap(10.0)
                .center_y()
                .child(
                    el().size(30.0, 30.0)
                        .radius(9.0)
                        .fill(p.add_green)
                        .center()
                        .child(heart_icon(16.0, Color::BLACK, true)),
                )
                .child(
                    text("Swipelist")
                        .font(TextStyle::new(17.0, 800.0).tracking(-0.4))
                        .color(p.text_primary),
                ),
        )
        .child(auth_control);

    // 2. Flat Source Playlist Block
    let source_block = col()
        .key("sidebar-source-card")
        .w_full()
        .padding(12.0)
        .gap(9.0)
        .radius(12.0)
        .fill(p.bg_flat_block)
        .child(
            row()
                .w_full()
                .center_y()
                .justify_between()
                .child(
                    text("SOURCE PLAYLIST")
                        .font(TextStyle::new(10.0, 750.0).tracking(0.8))
                        .color(p.text_muted),
                )
                .child(
                    row()
                        .key("btn-change-source-sidebar")
                        .role("button")
                        .label("Change Source")
                        .h(22.0)
                        .px(4.0)
                        .radius(5.0)
                        .center_y()
                        .fill(Color::TRANSPARENT)
                        .hover(p.hover_fill)
                        .hover_motion(
                            Curve::tween(0.09, Easing::cubic(0.42, 0.0, 0.58, 1.0)),
                            Curve::tween(0.11, Easing::cubic(0.42, 0.0, 0.58, 1.0)),
                        )
                        .press(p.press_fill)
                        .press_scale(0.95)
                        .cursor(Cursor::Pointer)
                        .on_press_tx(|s: &mut State, tx: &mut Tx| {
                            s.screen = if s.screen == Screen::SourcePicker {
                                Screen::Swiping
                            } else {
                                Screen::SourcePicker
                            };
                            tx.animated();
                        })
                        .child(
                            text("Change Source ▾")
                                .font(TextStyle::new(11.0, 700.0))
                                .color(p.add_green),
                        ),
                ),
        )
        .child(
            row()
                .w_full()
                .gap(11.0)
                .center_y()
                .child(playlist_thumb_el(s, &s.source_playlist, 38.0, 8.0))
                .child(
                    col()
                        .grow(1.0)
                        .gap(2.0)
                        .child(
                            text(s.source_playlist.name.clone())
                                .font(TextStyle::new(14.0, 700.0))
                                .color(p.text_primary)
                                .truncate(),
                        )
                        .child(
                            text(format!(
                                "{} songs • {}",
                                s.tracks.len(),
                                s.source_playlist.owner_name
                            ))
                            .font(TextStyle::new(11.5, 500.0))
                            .color(p.text_secondary)
                            .truncate(),
                        ),
                ),
        );

    // 3. Flat New Playlist Header
    let new_playlist_section = col()
        .key("sidebar-new-playlist-box")
        .w_full()
        .gap(8.0)
        .child(
            row()
                .w_full()
                .px(2.0)
                .center_y()
                .justify_between()
                .child(
                    text("NEW PLAYLIST")
                        .font(TextStyle::new(10.0, 750.0).tracking(0.8))
                        .color(p.add_green),
                )
                .child(
                    el().key("btn-toggle-privacy-sidebar")
                        .role("button")
                        .label("Toggle playlist privacy")
                        .h(20.0)
                        .px(8.0)
                        .radius(10.0)
                        .fill(p.bg_flat_block)
                        .center()
                        .cursor(Cursor::Pointer)
                        .on_press(|s: &mut State| {
                            s.is_public = !s.is_public;
                            s.auto_save_progress();
                        })
                        .child(
                            text(if s.is_public { "Public" } else { "Private" })
                                .font(TextStyle::new(10.5, 650.0))
                                .color(p.text_secondary),
                        ),
                ),
        )
        .child(
            row()
                .w_full()
                .gap(10.0)
                .center_y()
                .child(playlist_cover_mosaic(s, 46.0, 9.0, true))
                .child(
                    col()
                        .grow(1.0)
                        .gap(4.0)
                        .child(
                            el().key("sidebar-playlist-name-wrap")
                                .w_full()
                                .h(32.0)
                                .px(10.0)
                                .radius(8.0)
                                .fill(p.bg_flat_block)
                                .center_y()
                                .child(
                                    text_input(
                                        &s.new_playlist_name,
                                        TextStyle::new(13.0, 650.0),
                                    )
                                    .key("input-new-playlist-name")
                                    .label("New playlist name")
                                    .single_line()
                                    .w_full()
                                    .color(p.text_primary)
                                    .placeholder("New playlist name…", p.text_muted)
                                    .caret(p.add_green, p.add_green_soft)
                                    .on_edit(|s: &mut State| &mut s.new_playlist_name)
                                    .on_input_change(|s: &mut State, _| s.auto_save_progress()),
                                ),
                        )
                        .child(
                            text("Click cover tile to change artwork")
                                .font(TextStyle::new(10.5, 450.0))
                                .color(p.text_muted),
                        ),
                ),
        );

    // 4. Selected Songs Section Header
    let selected_header = row()
        .key("selected-songs-header")
        .w_full()
        .pt(4.0)
        .px(2.0)
        .center_y()
        .justify_between()
        .child(
            row()
                .gap(8.0)
                .center_y()
                .child(
                    text("Selected Songs")
                        .font(TextStyle::new(13.0, 750.0))
                        .color(p.text_primary),
                )
                .child(
                    el().h(20.0)
                        .px(8.0)
                        .radius(10.0)
                        .fill(p.add_green_soft)
                        .center()
                        .child(
                            text(format!("{count}"))
                                .font(TextStyle::new(11.0, 750.0))
                                .color(p.add_green),
                        ),
                ),
        )
        .child(
            text(duration_str)
                .font(TextStyle::new(11.5, 500.0))
                .color(p.text_muted),
        );

    // 5. Flat Selected Songs List (shows real cover art thumbnail per song)
    let selected_list_content = if s.selected_tracks.is_empty() {
        col()
            .key("selected-empty")
            .w_full()
            .py(36.0)
            .px(16.0)
            .center()
            .gap(8.0)
            .child(music_icon(24.0, p.text_muted))
            .child(
                text("No songs selected yet")
                    .font(TextStyle::new(13.0, 650.0))
                    .color(p.text_secondary),
            )
            .child(
                text("Swipe right or press ADD to include songs in your new playlist.")
                    .font(TextStyle::new(11.5, 450.0))
                    .color(p.text_muted)
                    .wrap(),
            )
    } else {
        let mut list = col().key("selected-rows").w_full().gap(2.0);
        let ease_in_out_in = Curve::tween(0.09, Easing::cubic(0.42, 0.0, 0.58, 1.0));
        let ease_in_out_out = Curve::tween(0.11, Easing::cubic(0.42, 0.0, 0.58, 1.0));
        let hover_bounce_spring = Spring::new(0.18, 0.28);

        for track in s.selected_tracks.iter().rev() {
            let track_id = track.id.clone();
            let hover_id = track.id.clone();
            let is_hovered = s.hovered_selected_id.as_deref() == Some(track.id.as_str());
            let row_item = row()
                .key(format!("sel-row-{}", track.id))
                .w_full()
                .h(46.0)
                .px(8.0)
                .gap(10.0)
                .center_y()
                .radius(8.0)
                .hover_group()
                .hover(oklcha(1.0, 0.0, 0.0, 0.085))
                .hover_motion(ease_in_out_in, ease_in_out_out)
                .scale(if is_hovered { 1.022 } else { 1.0 })
                .offset(if is_hovered { 4.0 } else { 0.0 }, 0.0)
                .motion(hover_bounce_spring)
                .transition(
                    Transition::offset(-16.0, 0.0)
                        .combined(Transition::scale(0.96))
                        .combined(Transition::opacity()),
                )
                .on_hover_change(move |s: &mut State, inside: bool, tx: &mut Tx| {
                    if inside {
                        s.hovered_selected_id = Some(hover_id.clone());
                    } else if s.hovered_selected_id.as_deref() == Some(hover_id.as_str()) {
                        s.hovered_selected_id = None;
                    }
                    tx.animated();
                })
                .child(
                    track_thumb_el(s, track, 34.0, 6.0)
                        .min_w(34.0)
                        .scale(if is_hovered { 1.06 } else { 1.0 })
                        .motion(hover_bounce_spring),
                )
                .child(
                    col()
                        .grow(1.0)
                        .min_w(0.0)
                        .gap(1.5)
                        .child(
                            text(track.name.clone())
                                .w_full()
                                .font(TextStyle::new(13.0, 650.0))
                                .color(p.text_primary)
                                .truncate(),
                        )
                        .child(
                            text(format!(
                                "{} • {}",
                                track.artists,
                                track.formatted_duration()
                            ))
                            .w_full()
                            .font(TextStyle::new(11.0, 500.0))
                            .color(p.text_secondary)
                            .truncate(),
                        ),
                )
                .child(
                    el().key(format!("remove-sel-{}", track.id))
                        .role("button")
                        .label("Remove from selection")
                        .help("Remove from selection")
                        .size(26.0, 26.0)
                        .min_w(26.0)
                        .radius(13.0)
                        .center()
                        .group_opacity(0.0, 1.0)
                        .hover(p.skip_coral_soft)
                        .press_scale(0.90)
                        .cursor(Cursor::Pointer)
                        .on_press_tx(move |s: &mut State, tx: &mut Tx| {
                            s.remove_from_selection(&track_id);
                            tx.animated();
                        })
                        .child(x_icon(13.0, 2.2, p.skip_coral)),
                );
            list = list.child(row_item);
        }
        list
    };

    let scroll_area = col()
        .key("selected-scroll-container")
        .w_full()
        .grow(1.0)
        .min_h(0.0)
        .child(
            selected_list_content
                .key("selected-scroll-view")
                .scroll(ScrollSpec { stick_bottom: false })
                .grow(1.0)
                .min_h(0.0),
        );

    // 6. Flat Bottom Action Pill: "Save to Spotify (N)"
    let save_label = format!("Save to Spotify ({count})");
    let save_btn = row()
        .key("btn-sidebar-save-spotify")
        .role("button")
        .label(&save_label)
        .w_full()
        .h(42.0)
        .radius(21.0)
        .center()
        .gap(8.0)
        .fill(if count > 0 {
            p.add_green
        } else {
            p.bg_flat_block
        })
        .press_scale(0.98)
        .cursor(if count > 0 {
            Cursor::Pointer
        } else {
            Cursor::Default
        })
        .on_press(|s: &mut State| {
            if s.screen == Screen::Review {
                s.trigger_save_to_spotify();
            } else {
                s.screen = Screen::Review;
            }
        })
        .child(check_icon(
            16.0,
            2.5,
            if count > 0 { Color::BLACK } else { p.text_muted },
        ))
        .child(
            text(save_label)
                .font(TextStyle::new(13.5, 750.0))
                .color(if count > 0 { Color::BLACK } else { p.text_muted }),
        );

    col()
        .key("left-sidebar")
        .w(m.sidebar_w)
        .h_full()
        .padding(18.0)
        .gap(14.0)
        .fill(p.bg_sidebar)
        .child(brand_row)
        .child(source_block)
        .child(new_playlist_section)
        .child(selected_header)
        .child(scroll_area)
        .child(save_btn)
}

/// Renders either the user's custom uploaded cover image or a flat 2×2 mosaic of the first 4 songs
/// in `Selected Songs` (using their real album cover art when loaded!).
fn playlist_cover_mosaic(s: &State, size: f32, radius: f32, clickable: bool) -> El {
    let half = (size - 2.0) / 2.0;
    let seed_offset = (s.cover_palette_seed * 45) as f32;

    let inner = if let Some(img) = &s.custom_cover_image {
        el().size(size, size)
            .radius(radius)
            .clip_rounded(radius)
            .pixels_fit(img.pixels(), SurfaceFit::Fill)
    } else if s.selected_tracks.is_empty() {
        let hue = (s.source_playlist.accent_hue + seed_offset) % 360.0;
        el().size(size, size)
            .radius(radius)
            .fill(oklch(0.36, 0.14, hue))
            .center()
            .child(image_icon(size * 0.42, Color::WHITE))
    } else {
        let tile = |idx: usize| -> El {
            if let Some(track) = s.selected_tracks.get(idx)
                && let Some(img) = s.track_covers.get(&track.id)
            {
                return el()
                    .size(half, half)
                    .fill(oklch(0.28, 0.08, track.accent_hue))
                    .pixels_fit(img.pixels(), SurfaceFit::Fill);
            }
            let h = s
                .selected_tracks
                .get(idx)
                .map(|t| (t.accent_hue + seed_offset) % 360.0)
                .unwrap_or(((idx * 65 + 150) as f32 + seed_offset) % 360.0);
            el().size(half, half)
                .fill(oklch(0.42, 0.15, h))
                .center()
                .child(music_icon(half * 0.45, Color::WHITE.alpha(0.9)))
        };
        col()
            .size(size, size)
            .radius(radius)
            .clip_rounded(radius)
            .gap(2.0)
            .child(row().gap(2.0).child(tile(0)).child(tile(1)))
            .child(row().gap(2.0).child(tile(2)).child(tile(3)))
    };

    if !clickable {
        return inner;
    }

    inner
        .key("cover-mosaic-picker")
        .role("button")
        .label("Choose playlist cover image")
        .help("Click to choose a cover image")
        .cursor(Cursor::Pointer)
        .on_press_tx(|s: &mut State, tx: &mut Tx| {
            s.cover_palette_seed = (s.cover_palette_seed + 1) % 8;
            let opts = FileDialogOptions {
                title: "Choose Playlist Cover Image".into(),
                ..Default::default()
            }
            .filter("Images", &["jpg", "jpeg", "png"]);
            tx.open_file(opts, |s: &mut State, res, _| {
                if let Ok(Some(path)) = res {
                    if let Ok(bytes) = std::fs::read(&path)
                        && let Some(cache) = &s.image_cache
                    {
                        s.custom_cover_image = Some(cache.load(bytes));
                    }
                    s.custom_cover_path = Some(path);
                    s.auto_save_progress();
                }
            });
        })
}

// ============================================================================
// Center Stage Router
// ============================================================================

fn center_stage(s: &State, p: &Palette, m: &LayoutMetrics) -> El {
    let mut stage = col()
        .key("center-stage")
        .grow(1.0)
        .h_full()
        .min_h(0.0);

    if let Some(err) = &s.error_banner {
        stage = stage.child(
            row()
                .key("error-banner")
                .w_full()
                .h(38.0)
                .px(24.0)
                .center_y()
                .justify_between()
                .fill(p.skip_coral_soft)
                .child(
                    text(err.clone())
                        .font(TextStyle::new(12.5, 600.0))
                        .color(p.text_primary),
                )
                .child(
                    text("Dismiss")
                        .key("btn-dismiss-error")
                        .font(TextStyle::new(12.0, 700.0))
                        .color(p.skip_coral)
                        .cursor(Cursor::Pointer)
                        .on_press(|s: &mut State| s.error_banner = None),
                ),
        );
    } else if let Some(status) = &s.status_banner {
        stage = stage.child(
            row()
                .key("status-banner")
                .w_full()
                .h(36.0)
                .px(24.0)
                .center()
                .fill(p.add_green_soft)
                .child(
                    text(status.clone())
                        .font(TextStyle::new(12.5, 650.0))
                        .color(p.add_green),
                ),
        );
    }

    let body = match s.screen {
        Screen::Swiping => swiping_stage_view(s, p, m),
        Screen::SourcePicker => source_picker_view(s, p, m),
        Screen::Review => review_stage_view(s, p),
        Screen::Saved => saved_stage_view(s, p, m),
    };

    stage.child(body)
}

// ============================================================================
// Screen 1: Tinder Swipe Deck Stage
// ============================================================================

fn swiping_stage_view(s: &State, p: &Palette, m: &LayoutMetrics) -> El {
    let total = s.tracks.len().max(1);
    let current_num = (s.current_index + 1).min(total);

    // Flat Top Bar with "Card X of <All>", Auto-Save badge, Reset & Review Selection
    let top_bar = row()
        .key("swipe-top-bar")
        .w_full()
        .h(60.0)
        .px(28.0)
        .center_y()
        .justify_between()
        .child(
            row()
                .gap(10.0)
                .center_y()
                .child(
                    row()
                        .key("btn-top-change-source")
                        .role("button")
                        .label("Change Source Playlist")
                        .h(34.0)
                        .px(14.0)
                        .radius(17.0)
                        .center_y()
                        .gap(8.0)
                        .fill(p.bg_card)
                        .hover(p.hover_fill)
                        .press(p.press_fill)
                        .press_scale(0.96)
                        .cursor(Cursor::Pointer)
                        .on_press_tx(|s: &mut State, tx: &mut Tx| {
                            s.screen = Screen::SourcePicker;
                            tx.animated();
                        })
                        .child(
                            text(format!("Source Playlist: {}", s.source_playlist.name))
                                .font(TextStyle::new(12.5, 650.0))
                                .color(p.text_primary),
                        )
                        .child(
                            text("Change Source ▾")
                                .font(TextStyle::new(12.0, 700.0))
                                .color(p.add_green),
                        ),
                )
                .child(
                    row()
                        .key("btn-shuffle-deck")
                        .role("button")
                        .label("Shuffle remaining tracks")
                        .h(34.0)
                        .px(13.0)
                        .radius(17.0)
                        .center_y()
                        .gap(6.0)
                        .fill(if s.shuffled {
                            p.add_green_soft
                        } else {
                            p.bg_card
                        })
                        .hover(p.hover_fill)
                        .cursor(Cursor::Pointer)
                        .on_press(|s: &mut State| s.toggle_shuffle())
                        .child(shuffle_icon(
                            14.0,
                            if s.shuffled {
                                p.add_green
                            } else {
                                p.text_secondary
                            },
                        ))
                        .child(
                            text(if s.shuffled { "Shuffled" } else { "In Order" })
                                .font(TextStyle::new(12.0, 600.0))
                                .color(if s.shuffled {
                                    p.add_green
                                } else {
                                    p.text_secondary
                                }),
                        ),
                ),
        )
        .child(
            row()
                .gap(10.0)
                .center_y()
                .child(
                    text(format!("Card {current_num} of {total}"))
                        .font(TextStyle::new(12.5, 650.0).features("'tnum' 1"))
                        .color(p.text_secondary),
                )
                .children((s.current_index > 0 || !s.selected_tracks.is_empty()).then(|| {
                    el().key("btn-reset-progress")
                        .role("button")
                        .label("Reset playlist progress")
                        .help("Start this Source Playlist over from Card 1")
                        .h(26.0)
                        .px(10.0)
                        .radius(13.0)
                        .fill(p.bg_card)
                        .hover(p.hover_fill)
                        .center()
                        .cursor(Cursor::Pointer)
                        .on_press(|s: &mut State| s.reset_current_playlist_progress())
                        .child(
                            text("Reset")
                                .font(TextStyle::new(11.0, 650.0))
                                .color(p.text_muted),
                        )
                }))
                .child(
                    row()
                        .key("btn-top-review")
                        .role("button")
                        .label("Review Selection")
                        .h(34.0)
                        .px(14.0)
                        .radius(17.0)
                        .center_y()
                        .gap(6.0)
                        .fill(p.bg_card)
                        .hover(p.hover_fill)
                        .press_scale(0.96)
                        .cursor(Cursor::Pointer)
                        .on_press_tx(|s: &mut State, tx: &mut Tx| {
                            s.screen = Screen::Review;
                            tx.animated();
                        })
                        .child(
                            text(format!("Review Selection ({})", s.selected_tracks.len()))
                                .font(TextStyle::new(12.0, 650.0))
                                .color(p.text_primary),
                        ),
                ),
        );

    let Some(active_track) = s.current_track() else {
        return col()
            .key("deck-exhausted")
            .w_full()
            .grow(1.0)
            .center()
            .gap(16.0)
            .child(
                text("All tracks in this Source Playlist have been reviewed!")
                    .font(TextStyle::new(20.0, 750.0))
                    .color(p.text_primary),
            )
            .child(
                row()
                    .gap(12.0)
                    .child(
                        row()
                            .key("btn-exhausted-review")
                            .h(44.0)
                            .px(22.0)
                            .radius(22.0)
                            .center()
                            .fill(p.add_green)
                            .cursor(Cursor::Pointer)
                            .on_press(|s: &mut State| s.screen = Screen::Review)
                            .child(
                                text(format!(
                                    "Save to Spotify ({})",
                                    s.selected_tracks.len()
                                ))
                                .font(TextStyle::new(14.0, 750.0))
                                .color(Color::BLACK),
                            ),
                    )
                    .child(
                        row()
                            .key("btn-exhausted-reset")
                            .h(44.0)
                            .px(20.0)
                            .radius(22.0)
                            .center()
                            .fill(p.bg_card)
                            .cursor(Cursor::Pointer)
                            .on_press(|s: &mut State| s.reset_current_playlist_progress())
                            .child(
                                text("Start Over from Card 1")
                                    .font(TextStyle::new(14.0, 650.0))
                                    .color(p.text_primary),
                            ),
                    ),
            );
    };

    let active_hue = active_track.accent_hue;
    let drag_ratio = (s.drag_dx.abs() / (m.card_w * 0.34)).clamp(0.0, 1.0);
    let swipe_threshold = (m.card_w * 0.22).max(75.0);
    let card_h_for_drag = m.card_h;

    let mut arena = el()
        .key("tinder-card-arena")
        .size(m.card_w, m.card_h)
        .radial_gradient(
            [0.5, 0.48],
            m.card_w * 0.72,
            [
                (0.0, oklcha(0.55, 0.18, active_hue, 0.18)),
                (1.0, Color::TRANSPARENT),
            ],
        );

    // 3rd Card in Stack (current_index + 2) — rises fluidly on Apple's critically-damped spring
    if let Some(t3) = s.tracks.get(s.current_index + 2) {
        let s3 = 0.88 + drag_ratio * 0.06;
        let y3 = (-28.0 + drag_ratio * 14.0) * m.scale;
        let o3 = 0.42 + drag_ratio * 0.28;
        arena = arena.child(
            tinder_track_card(
                s,
                TrackCardProps {
                    track: t3,
                    index: s.current_index + 2,
                    total,
                    drag_dx: 0.0,
                    is_top: false,
                },
                p,
                m,
            )
                .key(format!("deck-card-{}-{}", s.current_index + 2, t3.id))
                .absolute(0.0, 0.0)
                .paint_order(1)
                .scale(s3)
                .offset(0.0, y3)
                .opacity(o3)
                .motion(apple_spring_ui())
                .inert(),
        );
    }

    // 2nd Card in Stack (current_index + 1) — advances forward with gentle physical settle
    if let Some(t2) = s.tracks.get(s.current_index + 1) {
        let s2 = 0.94 + drag_ratio * 0.06;
        let y2 = (-14.0 + drag_ratio * 14.0) * m.scale;
        let o2 = 0.74 + drag_ratio * 0.26;
        arena = arena.child(
            tinder_track_card(
                s,
                TrackCardProps {
                    track: t2,
                    index: s.current_index + 1,
                    total,
                    drag_dx: 0.0,
                    is_top: false,
                },
                p,
                m,
            )
                .key(format!("deck-card-{}-{}", s.current_index + 1, t2.id))
                .absolute(0.0, 0.0)
                .paint_order(2)
                .scale(s2)
                .offset(0.0, y2)
                .opacity(o2)
                .motion(Spring::new(0.38, 0.14))
                .inert(),
        );
    }

    // 1st Card (Active Top Card) — Apple Fluid Interfaces: 1:1 tracking, rubber-band Y, momentum flick projection
    let active_track_id_drag = active_track.id.clone();
    let tilt_sign = if s.drag_pivot_top { 1.0 } else { -0.75 };
    let tilt_rad = (s.drag_dx * 0.0010 * tilt_sign).clamp(-0.35, 0.35);
    let top_card = tinder_track_card(
        s,
        TrackCardProps {
            track: active_track,
            index: s.current_index,
            total,
            drag_dx: s.drag_dx,
            is_top: true,
        },
        p,
        m,
    )
        .key(format!("deck-card-{}-{}", s.current_index, active_track.id))
        .absolute(0.0, 0.0)
        .paint_order(3)
        .scale(1.0)
        .offset(s.drag_dx, s.drag_dy)
        .rotate(tilt_rad)
        .origin(0.5, if s.drag_pivot_top { 0.3 } else { 0.7 })
        .motion(apple_spring_gesture())
        .transition(Transition::scale(0.96).combined(Transition::opacity()))
        .cursor(if s.is_dragging {
            Cursor::Grabbing
        } else {
            Cursor::Grab
        })
        .touch_drags_at_once()
        .on_drag(move |s: &mut State, d: &mut Drag, tx: &mut Tx| match d.phase {
            Phase::Down => {
                s.is_dragging = true;
                s.drag_pivot_top = d.start_y < card_h_for_drag * 0.55;
                s.drag_dx = 0.0;
                s.drag_dy = 0.0;
                s.drag_samples.clear();
                s.record_drag_sample(0.0);
                tx.instant();
            }
            Phase::Move => {
                s.drag_dx = d.dx;
                s.drag_dy = apple_rubber_band(d.dy, 160.0);
                s.record_drag_sample(d.dx);
                tx.instant();
            }
            Phase::Up => {
                s.is_dragging = false;
                let vx = s.release_velocity_x();
                let projected_dx = d.dx + apple_project_x(vx);
                s.drag_samples.clear();
                if !d.cancelled() && projected_dx > swipe_threshold {
                    s.commit_swipe_for(SwipeDecision::Add, Some(&active_track_id_drag));
                    tx.animated();
                } else if !d.cancelled() && projected_dx < -swipe_threshold {
                    s.commit_swipe_for(SwipeDecision::Skip, Some(&active_track_id_drag));
                    tx.animated();
                } else {
                    // Apple momentum snap-back (damping ~0.8, response 0.4s)
                    s.drag_dx = 0.0;
                    s.drag_dy = 0.0;
                    tx.motion(apple_spring_gesture());
                    tx.animated();
                }
            }
        });

    arena = arena.child(top_card);

    // Exiting Card Overlay — keeps the exact same `deck-card-{index}-{id}` key so it flies out
    // continuously from its live drag position in the EXACT committed direction (Left or Right).
    if let Some(exit) = &s.exiting_card
        && (exit.index != s.current_index || exit.track.id != active_track.id)
    {
        let exit_dist = (m.card_w * 1.28).max(460.0);
        let (target_x, target_rot, stamp_dx) = match exit.decision {
            SwipeDecision::Add => (exit_dist, 0.26, 120.0),
            SwipeDecision::Skip => (-exit_dist, -0.26, -120.0),
        };
        arena = arena.child(
            tinder_track_card(
                s,
                TrackCardProps {
                    track: &exit.track,
                    index: exit.index,
                    total,
                    drag_dx: stamp_dx,
                    is_top: true,
                },
                p,
                m,
            )
                .key(format!("deck-card-{}-{}", exit.index, exit.track.id))
                .absolute(0.0, 0.0)
                .paint_order(4)
                .scale(0.92)
                .offset(target_x, 32.0)
                .rotate(target_rot)
                .opacity(0.0)
                .motion(Spring::new(0.36, 0.0))
                .transition(Transition::opacity())
                .inert(),
        );
    }

    // Flat Tinder Floating Action Dock — Apple §8: Hint in the direction of the gesture + bouncy hover
    let can_undo = !s.history.is_empty();
    let track_url = active_track.spotify_url();
    let add_hint_scale = 1.0 + (s.drag_dx / 120.0).clamp(0.0, 1.0) * 0.13;
    let skip_hint_scale = 1.0 + (-s.drag_dx / 120.0).clamp(0.0, 1.0) * 0.13;
    let btn_hover_in = Curve::tween(0.09, Easing::cubic(0.42, 0.0, 0.58, 1.0));
    let btn_hover_out = Curve::tween(0.11, Easing::cubic(0.42, 0.0, 0.58, 1.0));
    let btn_bounce = Spring::new(0.18, 0.30);

    let undo_scale = if can_undo && s.hovered_btn == Some("dock-undo") {
        1.10
    } else {
        1.0
    };
    let skip_scale = skip_hint_scale
        * if s.hovered_btn == Some("dock-skip") {
            1.10
        } else {
            1.0
        };
    let add_scale = add_hint_scale
        * if s.hovered_btn == Some("dock-add") {
            1.10
        } else {
            1.0
        };
    let open_scale = if s.hovered_btn == Some("dock-open") {
        1.10
    } else {
        1.0
    };

    let action_dock = row()
        .key("tinder-action-dock")
        .gap((18.0 * m.scale).round())
        .center_y()
        // 1. Undo Button
        .child(
            el().key("btn-dock-undo")
                .role("button")
                .label("Undo")
                .help("Undo returns the last card to the top of the stack and removes it from the selection if it was added.")
                .size(m.dock_sm, m.dock_sm)
                .radius(m.dock_sm / 2.0)
                .center()
                .scale(undo_scale)
                .fill(if can_undo {
                    p.undo_amber_soft
                } else {
                    p.bg_card
                })
                .hover(if can_undo {
                    oklcha(0.80, 0.17, 75.0, 0.26)
                } else {
                    Color::TRANSPARENT
                })
                .hover_motion(btn_hover_in, btn_hover_out)
                .motion(btn_bounce)
                .press_scale(if can_undo { 0.92 } else { 1.0 })
                .cursor(if can_undo {
                    Cursor::Pointer
                } else {
                    Cursor::Default
                })
                .on_hover_change(move |s: &mut State, inside: bool, tx: &mut Tx| {
                    if inside && can_undo {
                        s.hovered_btn = Some("dock-undo");
                    } else if s.hovered_btn == Some("dock-undo") {
                        s.hovered_btn = None;
                    }
                    tx.animated();
                })
                .on_press_tx(|s: &mut State, tx: &mut Tx| {
                    s.undo_last_swipe();
                    tx.animated();
                })
                .child(undo_icon(
                    (20.0 * m.scale).round(),
                    if can_undo { p.undo_amber } else { p.text_muted },
                )),
        )
        // 2. Large SKIP Button (grows subtly as you drag left or hover)
        .child(
            el().key("btn-dock-skip")
                .role("button")
                .label("SKIP")
                .help("SKIP track (Left Arrow)")
                .size(m.dock_lg, m.dock_lg)
                .radius(m.dock_lg / 2.0)
                .center()
                .scale(skip_scale)
                .motion(btn_bounce)
                .fill(p.skip_coral_soft)
                .hover(oklcha(0.68, 0.22, 18.0, 0.28))
                .hover_motion(btn_hover_in, btn_hover_out)
                .press_scale(0.91)
                .cursor(Cursor::Pointer)
                .on_hover_change(|s: &mut State, inside: bool, tx: &mut Tx| {
                    if inside {
                        s.hovered_btn = Some("dock-skip");
                    } else if s.hovered_btn == Some("dock-skip") {
                        s.hovered_btn = None;
                    }
                    tx.animated();
                })
                .on_press_tx(|s: &mut State, tx: &mut Tx| {
                    s.commit_swipe(SwipeDecision::Skip);
                    tx.animated();
                })
                .child(x_icon((27.0 * m.scale).round(), 2.8, p.skip_coral)),
        )
        // 3. Large ADD Button (grows subtly as you drag right or hover)
        .child(
            el().key("btn-dock-add")
                .role("button")
                .label("ADD")
                .help("ADD track to Selected Songs (Right Arrow)")
                .size(m.dock_lg, m.dock_lg)
                .radius(m.dock_lg / 2.0)
                .center()
                .scale(add_scale)
                .motion(btn_bounce)
                .fill(p.add_green)
                .hover(oklch(0.82, 0.21, 152.0))
                .hover_motion(btn_hover_in, btn_hover_out)
                .press_scale(0.91)
                .cursor(Cursor::Pointer)
                .on_hover_change(|s: &mut State, inside: bool, tx: &mut Tx| {
                    if inside {
                        s.hovered_btn = Some("dock-add");
                    } else if s.hovered_btn == Some("dock-add") {
                        s.hovered_btn = None;
                    }
                    tx.animated();
                })
                .on_press_tx(|s: &mut State, tx: &mut Tx| {
                    s.commit_swipe(SwipeDecision::Add);
                    tx.animated();
                })
                .child(heart_icon((27.0 * m.scale).round(), Color::BLACK, true)),
        )
        // 4. Open in Spotify Button
        .child(
            el().key("btn-dock-open-spotify")
                .role("button")
                .label("Open in Spotify")
                .help("Open track in Spotify (O)")
                .size(m.dock_sm, m.dock_sm)
                .radius(m.dock_sm / 2.0)
                .center()
                .scale(open_scale)
                .motion(btn_bounce)
                .fill(p.bg_card)
                .hover(oklcha(1.0, 0.0, 0.0, 0.12))
                .hover_motion(btn_hover_in, btn_hover_out)
                .press_scale(0.92)
                .cursor(Cursor::Pointer)
                .on_hover_change(|s: &mut State, inside: bool, tx: &mut Tx| {
                    if inside {
                        s.hovered_btn = Some("dock-open");
                    } else if s.hovered_btn == Some("dock-open") {
                        s.hovered_btn = None;
                    }
                    tx.animated();
                })
                .on_press_tx(move |_: &mut State, tx: &mut Tx| {
                    let _ = tx.open_url(&track_url);
                })
                .child(external_link_icon((19.0 * m.scale).round(), p.text_primary)),
        );

    let hints_row = row()
        .key("swipe-hints-row")
        .gap(16.0)
        .center_y()
        .child(kbd_hint("←", "SKIP", p))
        .child(kbd_hint("Space", "Play / Pause", p))
        .child(kbd_hint("↑↓", "Volume", p))
        .child(kbd_hint("Z", "Undo", p))
        .child(kbd_hint("→", "ADD", p))
        .child(kbd_hint("O", "Open in Spotify", p));

    col()
        .key("swiping-stage")
        .w_full()
        .grow(1.0)
        .justify_between()
        .pb(18.0)
        .motion(apple_spring_ui())
        .transition(
            Transition::scale(0.97)
                .combined(Transition::offset(0.0, 12.0))
                .combined(Transition::opacity()),
        )
        .child(top_bar)
        .child(
            col()
                .w_full()
                .grow(1.0)
                .center()
                .gap(16.0)
                .child(arena)
                .child(action_dock)
                .child(hints_row),
        )
}

fn kbd_hint(key_label: &str, action_label: &str, p: &Palette) -> El {
    row()
        .gap(6.0)
        .center_y()
        .child(
            el().h(20.0)
                .px(7.0)
                .radius(5.0)
                .fill(p.bg_card)
                .center()
                .child(
                    text(key_label)
                        .font(TextStyle::new(10.5, 700.0))
                        .color(p.text_secondary),
                ),
        )
        .child(
            text(action_label)
                .font(TextStyle::new(11.5, 550.0))
                .color(p.text_muted),
        )
}

struct TrackCardProps<'a> {
    track: &'a Track,
    index: usize,
    total: usize,
    drag_dx: f32,
    is_top: bool,
}

/// Builds a single Tinder-style full-bleed track card (`380 × 515` pt) with real album cover art,
/// Play/Pause toggle, and a draggable player slider.
fn tinder_track_card(
    s: &State,
    props: TrackCardProps<'_>,
    p: &Palette,
    m: &LayoutMetrics,
) -> El {
    let TrackCardProps {
        track,
        index,
        total,
        drag_dx,
        is_top,
    } = props;
    let hue = track.accent_hue;
    let add_opacity = if is_top {
        (drag_dx / 70.0).clamp(0.0, 1.0)
    } else {
        0.0
    };
    let skip_opacity = if is_top {
        (-drag_dx / 70.0).clamp(0.0, 1.0)
    } else {
        0.0
    };

    let spotify_url = track.spotify_url();

    let top_overlay = col()
        .w_full()
        .padding(16.0)
        .child(
            row()
                .w_full()
                .center_y()
                .justify_between()
                .child(
                    row()
                        .h(26.0)
                        .px(10.0)
                        .radius(13.0)
                        .center_y()
                        .gap(6.0)
                        .fill(Color::BLACK.alpha(0.55))
                        .child(
                            text(format!("#{} of {}", index + 1, total))
                                .font(TextStyle::new(11.0, 700.0).features("'tnum' 1"))
                                .color(Color::WHITE),
                        )
                        .child(
                            text(format!("• {}", track.genre_tag))
                                .font(TextStyle::new(11.0, 550.0))
                                .color(Color::WHITE.alpha(0.82)),
                        ),
                )
                .child(
                    row()
                        .key(format!("card-open-spotify-{}", track.id))
                        .role("button")
                        .label("Open in Spotify")
                        .h(26.0)
                        .px(10.0)
                        .radius(13.0)
                        .center_y()
                        .gap(5.0)
                        .fill(Color::BLACK.alpha(0.55))
                        .hover(Color::BLACK.alpha(0.76))
                        .cursor(Cursor::Pointer)
                        .on_press_tx(move |_: &mut State, tx: &mut Tx| {
                            let _ = tx.open_url(&spotify_url);
                        })
                        .child(
                            text("Open in Spotify")
                                .font(TextStyle::new(10.5, 650.0))
                                .color(Color::WHITE),
                        )
                        .child(external_link_icon(11.0, p.add_green)),
                ),
        );

    let add_stamp = row()
        .key(format!("stamp-add-{index}-{}", track.id))
        .absolute(22.0, 68.0)
        .h(42.0)
        .px(16.0)
        .radius(10.0)
        .center()
        .gap(8.0)
        .fill(p.add_green)
        .rotate(-0.18)
        .opacity(add_opacity)
        .inert()
        .child(plus_icon(20.0, 3.0, Color::BLACK))
        .child(
            text("ADD")
                .font(TextStyle::new(20.0, 850.0).tracking(1.2))
                .color(Color::BLACK),
        );

    let skip_stamp = row()
        .key(format!("stamp-skip-{index}-{}", track.id))
        .absolute(m.card_w - 132.0, 68.0)
        .h(42.0)
        .px(16.0)
        .radius(10.0)
        .center()
        .gap(8.0)
        .fill(p.skip_coral)
        .rotate(0.18)
        .opacity(skip_opacity)
        .inert()
        .child(x_icon(20.0, 3.0, Color::WHITE))
        .child(
            text("SKIP")
                .font(TextStyle::new(20.0, 850.0).tracking(1.2))
                .color(Color::WHITE),
        );

    // Spotify-style Audio Player (Play/Pause + Volume Adjuster + Scrubbing Seek Bar)
    let is_this_playing =
        is_top && s.is_playing && s.playing_track_id.as_deref() == Some(track.id.as_str());
    let is_this_loading =
        is_top && s.audio_loading && s.playing_track_id.as_deref() == Some(track.id.as_str());
    let pos = if is_top {
        s.slider_pos.clamp(0.0, 1.0)
    } else {
        0.0
    };
    let dur_secs = if is_top {
        s.audio_duration_secs.max(1.0)
    } else {
        30.0
    };
    let elapsed_secs = (pos * dur_secs).round() as u32;
    let total_preview_secs = dur_secs.round() as u32;
    let spotify_green = p.add_green;

    // 1. Seek Bar (Spotify style: white fill at rest, #1DB954 green + white knob on hover/drag)
    let slider_w = m.slider_w;
    let knob_d = (12.0 * m.scale).clamp(11.0, 15.0).round();
    let bar_h = (4.0 * m.scale).clamp(4.0, 5.0).round();
    let track_h = (20.0 * m.scale).round();
    let travel = (slider_w - knob_d).max(1.0);
    let fill_w = (pos * slider_w).clamp(0.0, slider_w);
    let seek_active_op = if is_top && s.slider_dragging { 1.0 } else { 0.0 };

    let mut slider_el = el()
        .key(format!("card-audio-slider-{}", track.id))
        .role("slider")
        .label("Audio player position")
        .size(slider_w, track_h)
        .hover_group()
        .cursor(Cursor::Pointer)
        .child(
            el().size(slider_w, bar_h)
                .absolute(0.0, (track_h - bar_h) / 2.0)
                .radius(bar_h / 2.0)
                .fill(Color::WHITE.alpha(0.28))
                .a11y_hidden(),
        )
        .child(
            el().size(fill_w, bar_h)
                .absolute(0.0, (track_h - bar_h) / 2.0)
                .radius(bar_h / 2.0)
                .fill(Color::WHITE)
                .a11y_hidden(),
        )
        .child(
            el().size(fill_w, bar_h)
                .absolute(0.0, (track_h - bar_h) / 2.0)
                .radius(bar_h / 2.0)
                .fill(spotify_green)
                .group_opacity(seek_active_op, 1.0)
                .a11y_hidden(),
        )
        .child(
            el().size(knob_d, knob_d)
                .absolute(pos * travel, (track_h - knob_d) / 2.0)
                .radius(knob_d / 2.0)
                .fill(Color::WHITE)
                .shadow(1.0, 4.0, Color::BLACK.alpha(0.5))
                .group_opacity(seek_active_op, 1.0)
                .a11y_hidden(),
        );

    if is_top {
        slider_el = slider_el.on_drag(move |s: &mut State, d: &mut Drag, tx: &mut Tx| {
            let to_norm = |x: f32| ((x - knob_d / 2.0) / travel).clamp(0.0, 1.0);
            match d.phase {
                Phase::Down => {
                    d.data = s.slider_pos;
                    s.begin_scrub(to_norm(d.x));
                    tx.instant();
                }
                Phase::Move => {
                    s.slider_pos = to_norm(d.x);
                    tx.instant();
                }
                Phase::Up => {
                    if d.cancelled() {
                        s.cancel_scrub(d.data);
                    } else {
                        s.finish_scrub(to_norm(d.x));
                    }
                }
            }
        });
    }

    // 2. Spotify-style Volume Slider & Mute Toggle
    let effective_vol = if s.muted { 0.0 } else { s.volume.clamp(0.0, 1.0) };
    let vol_w = m.vol_w;
    let vol_knob = (11.0 * m.scale).clamp(10.0, 14.0).round();
    let vol_travel = (vol_w - vol_knob).max(1.0);
    let vol_fill_w = (effective_vol * vol_w).clamp(0.0, vol_w);
    let vol_active_op = if is_top && s.volume_dragging { 1.0 } else { 0.0 };

    let mut vol_slider_el = el()
        .key(format!("card-volume-slider-{}", track.id))
        .role("slider")
        .label("Volume")
        .size(vol_w, track_h)
        .hover_group()
        .motion(apple_spring_snappy())
        .cursor(Cursor::Pointer)
        .child(
            el().size(vol_w, bar_h)
                .absolute(0.0, (track_h - bar_h) / 2.0)
                .radius(bar_h / 2.0)
                .fill(Color::WHITE.alpha(0.28))
                .a11y_hidden(),
        )
        .child(
            el().size(vol_fill_w, bar_h)
                .absolute(0.0, (track_h - bar_h) / 2.0)
                .radius(bar_h / 2.0)
                .fill(Color::WHITE)
                .a11y_hidden(),
        )
        .child(
            el().size(vol_fill_w, bar_h)
                .absolute(0.0, (track_h - bar_h) / 2.0)
                .radius(bar_h / 2.0)
                .fill(spotify_green)
                .group_opacity(vol_active_op, 1.0)
                .a11y_hidden(),
        )
        .child(
            el().size(vol_knob, vol_knob)
                .absolute(effective_vol * vol_travel, (track_h - vol_knob) / 2.0)
                .radius(vol_knob / 2.0)
                .fill(Color::WHITE)
                .shadow(1.0, 4.0, Color::BLACK.alpha(0.5))
                .group_opacity(vol_active_op, 1.0)
                .a11y_hidden(),
        );

    if is_top {
        vol_slider_el = vol_slider_el.on_drag(move |s: &mut State, d: &mut Drag, tx: &mut Tx| {
            let to_norm = |x: f32| ((x - vol_knob / 2.0) / vol_travel).clamp(0.0, 1.0);
            match d.phase {
                Phase::Down => {
                    d.data = s.volume;
                    s.volume_dragging = true;
                    s.set_volume(to_norm(d.x));
                    tx.instant();
                }
                Phase::Move => {
                    s.set_volume(to_norm(d.x));
                    tx.instant();
                }
                Phase::Up => {
                    s.volume_dragging = false;
                    if d.cancelled() {
                        s.set_volume(d.data);
                    } else {
                        s.set_volume(to_norm(d.x));
                    }
                }
            }
        });
    }

    let card_btn_hover_in = Curve::tween(0.09, Easing::cubic(0.42, 0.0, 0.58, 1.0));
    let card_btn_hover_out = Curve::tween(0.11, Easing::cubic(0.42, 0.0, 0.58, 1.0));
    let card_btn_bounce = Spring::new(0.18, 0.30);
    let mute_hovered = is_top && s.hovered_btn == Some("card-mute");
    let play_hovered = is_top && s.hovered_btn == Some("card-play");

    let mut mute_btn = el()
        .key(format!("btn-card-mute-{}", track.id))
        .role("button")
        .label(if s.muted || effective_vol <= 0.005 {
            "Unmute"
        } else {
            "Mute"
        })
        .size(26.0, 26.0)
        .radius(13.0)
        .center()
        .scale(if mute_hovered { 1.12 } else { 1.0 })
        .hover(Color::WHITE.alpha(0.14))
        .hover_motion(card_btn_hover_in, card_btn_hover_out)
        .motion(card_btn_bounce)
        .press_scale(0.90)
        .cursor(Cursor::Pointer)
        .on_press(|s: &mut State| {
            s.toggle_mute();
        })
        .child(volume_icon(
            16.0,
            if mute_hovered {
                Color::WHITE
            } else {
                Color::WHITE.alpha(0.84)
            },
            effective_vol,
            s.muted,
        ));
    if is_top {
        mute_btn = mute_btn.on_hover_change(|s: &mut State, inside: bool, tx: &mut Tx| {
            if inside {
                s.hovered_btn = Some("card-mute");
            } else if s.hovered_btn == Some("card-mute") {
                s.hovered_btn = None;
            }
            tx.animated();
        });
    }

    let volume_group = row()
        .key(format!("card-volume-group-{}", track.id))
        .center_y()
        .gap(6.0)
        .child(mute_btn)
        .child(vol_slider_el);

    // 3. Combined Spotify-style Player Block (Transport + Volume on Row 1, Progress Scrubber on Row 2)
    let play_btn_d = (32.0 * m.scale).clamp(30.0, 42.0).round();
    let mut play_btn = el()
        .key(format!("btn-card-play-{}", track.id))
        .role("button")
        .label(if is_this_playing {
            "Pause preview"
        } else {
            "Play preview"
        })
        .size(play_btn_d, play_btn_d)
        .radius(play_btn_d / 2.0)
        .center()
        .scale(if play_hovered { 1.10 } else { 1.0 })
        .fill(Color::WHITE)
        .hover(Color::WHITE.alpha(0.92))
        .hover_motion(card_btn_hover_in, card_btn_hover_out)
        .motion(card_btn_bounce)
        .press_scale(0.93)
        .cursor(Cursor::Pointer)
        .on_press(|s: &mut State| {
            s.toggle_play_current_track();
        })
        .child(if is_this_playing {
            pause_icon((14.0 * m.scale).round(), Color::BLACK)
        } else {
            play_icon((14.0 * m.scale).round(), Color::BLACK)
        });
    if is_top {
        play_btn = play_btn.on_hover_change(|s: &mut State, inside: bool, tx: &mut Tx| {
            if inside {
                s.hovered_btn = Some("card-play");
            } else if s.hovered_btn == Some("card-play") {
                s.hovered_btn = None;
            }
            tx.animated();
        });
    }

    let mut controls_left = row().center_y().gap(10.0).child(play_btn);

    if is_this_loading {
        controls_left = controls_left.child(
            text("Loading…")
                .font(TextStyle::new(11.0 * m.scale, 550.0))
                .color(Color::WHITE.alpha(0.70)),
        );
    }

    let player_bar = col()
        .key(format!("card-player-bar-{}", track.id))
        .w_full()
        .pt(4.0)
        .gap((6.0 * m.scale).round())
        .child(
            row()
                .w_full()
                .h(play_btn_d)
                .center_y()
                .justify_between()
                .child(controls_left)
                .child(volume_group),
        )
        .child(
            row()
                .w_full()
                .h(track_h)
                .center_y()
                .gap(8.0)
                .child(
                    el().w(m.time_w)
                        .center_y()
                        .child(
                            text(format_seconds(elapsed_secs))
                                .font(TextStyle::new((10.5 * m.scale).clamp(10.0, 13.0), 500.0).features("'tnum' 1"))
                                .color(Color::WHITE.alpha(0.72)),
                        ),
                )
                .child(slider_el)
                .child(
                    el().w(m.time_w)
                        .center_y()
                        .justify_end()
                        .child(
                            text(format_seconds(total_preview_secs))
                                .font(TextStyle::new((10.5 * m.scale).clamp(10.0, 13.0), 500.0).features("'tnum' 1"))
                                .color(Color::WHITE.alpha(0.72)),
                        ),
                ),
        );

    let bottom_info = col()
        .w_full()
        .padding(m.card_pad)
        .pt((56.0 * m.scale).round())
        .gap((10.0 * m.scale).round())
        .linear_gradient(
            std::f32::consts::PI,
            [
                (0.0, Color::TRANSPARENT),
                (0.28, oklcha(0.07, 0.01, hue, 0.78)),
                (0.62, oklcha(0.06, 0.01, hue, 0.95)),
                (1.0, oklcha(0.05, 0.01, hue, 0.99)),
            ],
        )
        .child(
            row()
                .w_full()
                .items_end()
                .justify_between()
                .gap(10.0)
                .child(
                    col()
                        .grow(1.0)
                        .min_w(0.0)
                        .gap(3.0)
                        .child(
                            text(track.name.clone())
                                .w_full()
                                .font(TextStyle::new((23.0 * m.scale).clamp(21.0, 30.0), 800.0).tracking(-0.4))
                                .color(Color::WHITE)
                                .max_lines(2),
                        )
                        .child(
                            text(format!(
                                "{} • {} ({})",
                                track.artists, track.album, track.year
                            ))
                            .w_full()
                            .font(TextStyle::new((13.5 * m.scale).clamp(12.5, 17.0), 600.0))
                            .color(p.add_green)
                            .truncate(),
                        ),
                )
                .child(
                    el().h((24.0 * m.scale).round())
                        .px((9.0 * m.scale).round())
                        .radius((12.0 * m.scale).round())
                        .fill(Color::WHITE.alpha(0.14))
                        .center()
                        .child(
                            text(track.formatted_duration())
                                .font(TextStyle::new((11.5 * m.scale).clamp(11.0, 14.0), 700.0).features("'tnum' 1"))
                                .color(Color::WHITE),
                        ),
                ),
        )
        .child(player_bar);

    let card_radius = (24.0 * m.scale).clamp(20.0, 30.0).round();
    let mut card = el()
        .size(m.card_w, m.card_h)
        .radius(card_radius)
        .continuous_corners()
        .clip_rounded(card_radius)
        .fill(oklch(0.24, 0.12, hue))
        .gradient(
            oklch(0.42, 0.17, hue),
            oklch(0.14, 0.08, (hue + 55.0) % 360.0),
        )
        .shadow(18.0, 44.0, Color::BLACK.alpha(0.60))
        .child(vinyl_cover_art(hue, m.card_w, m.card_h).absolute(0.0, 0.0));

    // Overlay real album cover art when available
    if let Some(img) = s.track_covers.get(&track.id) {
        card = card.child(
            el().size(m.card_w, m.card_h)
                .absolute(0.0, 0.0)
                .pixels_fit(img.pixels(), SurfaceFit::Fill)
                .a11y_hidden(),
        );
    }

    card.child(
        col()
            .w_full()
            .h_full()
            .justify_between()
            .child(top_overlay)
            .child(bottom_info),
    )
    .child(add_stamp)
    .child(skip_stamp)
}

// ============================================================================
// Screen 2: Source Picker (Flat Grid with Real Playlist Cover Art)
// ============================================================================

fn source_picker_view(s: &State, p: &Palette, m: &LayoutMetrics) -> El {
    let query = s.search_edit.text.trim().to_lowercase();
    let filtered: Vec<&PlaylistSummary> = s
        .playlists
        .iter()
        .filter(|pl| {
            let matches_filter = match s.source_filter {
                SourceFilter::All => true,
                SourceFilter::LikedSongs => pl.is_liked_songs,
                SourceFilter::Playlists => !pl.is_liked_songs,
            };
            let matches_query = query.is_empty()
                || pl.name.to_lowercase().contains(&query)
                || pl.owner_name.to_lowercase().contains(&query)
                || pl.subtitle.to_lowercase().contains(&query);
            matches_filter && matches_query
        })
        .collect();

    let header = col()
        .key("source-picker-header")
        .w_full()
        .px(32.0)
        .pt(26.0)
        .pb(16.0)
        .gap(16.0)
        .child(
            row()
                .w_full()
                .center_y()
                .justify_between()
                .child(
                    col()
                        .gap(4.0)
                        .child(
                            text("Source Picker")
                                .font(TextStyle::new(26.0, 800.0).tracking(-0.4))
                                .color(p.text_primary),
                        )
                        .child(
                            text("Choose a Source Playlist to swipe through and build your New Playlist.")
                                .font(TextStyle::new(13.5, 450.0))
                                .color(p.text_secondary),
                        ),
                )
                .child(
                    row()
                        .key("btn-picker-back-to-swipe")
                        .role("button")
                        .label("Back to Swipe Deck")
                        .h(36.0)
                        .px(16.0)
                        .radius(18.0)
                        .center_y()
                        .gap(6.0)
                        .fill(p.bg_card)
                        .hover(p.hover_fill)
                        .cursor(Cursor::Pointer)
                        .on_press(|s: &mut State| s.screen = Screen::Swiping)
                        .child(chevron_left_icon(15.0, p.text_primary))
                        .child(
                            text("Back to Swipe Deck")
                                .font(TextStyle::new(12.5, 650.0))
                                .color(p.text_primary),
                        ),
                ),
        )
        .child(
            row()
                .w_full()
                .gap(10.0)
                .center_y()
                .child(
                    row()
                        .key("source-search-box")
                        .w(340.0)
                        .h(38.0)
                        .px(14.0)
                        .gap(8.0)
                        .radius(19.0)
                        .center_y()
                        .fill(p.bg_card)
                        .child(search_icon(16.0, p.text_secondary))
                        .child(
                            text_input(&s.search_edit, TextStyle::new(13.0, 500.0))
                                .key("input-source-search")
                                .label("Filter source playlists")
                                .single_line()
                                .grow(1.0)
                                .color(p.text_primary)
                                .placeholder("Filter source playlists…", p.text_muted)
                                .caret(p.add_green, p.add_green_soft)
                                .on_edit(|s: &mut State| &mut s.search_edit),
                        ),
                )
                .child(filter_chip(
                    "filter-all",
                    "All",
                    s.source_filter == SourceFilter::All,
                    SourceFilter::All,
                    p,
                ))
                .child(filter_chip(
                    "filter-liked",
                    "Liked Songs",
                    s.source_filter == SourceFilter::LikedSongs,
                    SourceFilter::LikedSongs,
                    p,
                ))
                .child(filter_chip(
                    "filter-playlists",
                    "Playlists",
                    s.source_filter == SourceFilter::Playlists,
                    SourceFilter::Playlists,
                    p,
                )),
        );

    let mut grid_rows = col()
        .key("source-grid-rows")
        .w_full()
        .px(32.0)
        .pb(28.0)
        .gap(12.0);
    let cols = if m.stage_w >= 1020.0 { 3 } else { 2 };
    for chunk in filtered.chunks(cols) {
        let mut r = row().w_full().gap(12.0);
        for pl in chunk {
            r = r.child(source_playlist_tile(s, pl, s.source_playlist.id == pl.id, p));
        }
        for _ in chunk.len()..cols {
            r = r.child(el().grow(1.0));
        }
        grid_rows = grid_rows.child(r);
    }

    col()
        .key("source-picker-stage")
        .w_full()
        .grow(1.0)
        .min_h(0.0)
        .motion(apple_spring_ui())
        .transition(
            Transition::scale(0.97)
                .combined(Transition::offset(0.0, 12.0))
                .combined(Transition::opacity()),
        )
        .child(header)
        .child(
            col()
                .w_full()
                .grow(1.0)
                .min_h(0.0)
                .child(
                    grid_rows
                        .key("source-picker-scroll")
                        .scroll(ScrollSpec { stick_bottom: false })
                        .grow(1.0)
                        .min_h(0.0),
                ),
        )
}

fn filter_chip(key: &str, label: &str, active: bool, target: SourceFilter, p: &Palette) -> El {
    el().key(key)
        .role("button")
        .label(label)
        .h(36.0)
        .px(16.0)
        .radius(18.0)
        .center()
        .fill(if active { p.add_green } else { p.bg_card })
        .cursor(Cursor::Pointer)
        .on_press(move |s: &mut State| {
            s.source_filter = target;
        })
        .child(
            text(label)
                .font(TextStyle::new(12.5, 700.0))
                .color(if active {
                    Color::BLACK
                } else {
                    p.text_secondary
                }),
        )
}

fn source_playlist_tile(
    s: &State,
    pl: &PlaylistSummary,
    is_current: bool,
    p: &Palette,
) -> El {
    let pl_clone = pl.clone();
    let saved_progress = s.progress_store.sessions.get(&pl.id);

    row()
        .key(format!("source-tile-{}", pl.id))
        .role("button")
        .label(&pl.name)
        .grow(1.0)
        .h(92.0)
        .padding(14.0)
        .gap(14.0)
        .center_y()
        .radius(14.0)
        .fill(if is_current {
            p.bg_flat_pill
        } else {
            p.bg_card
        })
        .hover(p.hover_fill)
        .press_scale(0.98)
        .cursor(Cursor::Pointer)
        .on_press(move |s: &mut State| {
            s.select_source_playlist(pl_clone.clone());
        })
        .child(playlist_thumb_el(s, pl, 64.0, 10.0))
        .child(
            col()
                .grow(1.0)
                .gap(4.0)
                .child(
                    row()
                        .gap(8.0)
                        .center_y()
                        .child(
                            text(pl.name.clone())
                                .font(TextStyle::new(15.5, 750.0))
                                .color(p.text_primary)
                                .truncate(),
                        )
                        .children(is_current.then(|| {
                            el().h(20.0)
                                .px(8.0)
                                .radius(10.0)
                                .fill(p.add_green_soft)
                                .center()
                                .child(
                                    text("ACTIVE SOURCE")
                                        .font(TextStyle::new(9.5, 800.0).tracking(0.6))
                                        .color(p.add_green),
                                )
                        })),
                )
                .child(
                    text(pl.subtitle.clone())
                        .font(TextStyle::new(12.5, 500.0))
                        .color(p.text_secondary)
                        .truncate(),
                )
                .child(
                    text(if let Some(prog) = saved_progress {
                        format!(
                            "{} songs • Resume at Card {} ({} selected)",
                            pl.track_count,
                            prog.current_index + 1,
                            prog.selected_tracks.len()
                        )
                    } else {
                        format!("{} songs • By {}", pl.track_count, pl.owner_name)
                    })
                    .font(TextStyle::new(11.5, 500.0))
                    .color(p.text_muted),
                ),
        )
}

// ============================================================================
// Screen 3: Review & Save New Playlist to Spotify (Flat Design)
// ============================================================================

fn review_stage_view(s: &State, p: &Palette) -> El {
    let count = s.selected_tracks.len();
    let total_dur = format_total_duration(&s.selected_tracks);
    let save_label = format!("Save to Spotify ({count})");

    let banner = row()
        .key("review-banner")
        .w_full()
        .padding(28.0)
        .gap(24.0)
        .items_center()
        .fill(p.bg_sidebar)
        .child(
            col()
                .gap(8.0)
                .items_center()
                .child(playlist_cover_mosaic(s, 132.0, 14.0, true))
                .child(
                    row()
                        .key("btn-choose-cover-review")
                        .role("button")
                        .label("Choose Cover Image")
                        .h(26.0)
                        .px(10.0)
                        .radius(13.0)
                        .center_y()
                        .gap(5.0)
                        .fill(p.bg_flat_block)
                        .hover(p.hover_fill)
                        .cursor(Cursor::Pointer)
                        .on_press_tx(|s: &mut State, tx: &mut Tx| {
                            s.cover_palette_seed = (s.cover_palette_seed + 1) % 8;
                            let opts = FileDialogOptions {
                                title: "Choose Playlist Cover Image".into(),
                                ..Default::default()
                            }
                            .filter("Images", &["jpg", "jpeg", "png"]);
                            tx.open_file(opts, |s: &mut State, res, _| {
                                if let Ok(Some(path)) = res {
                                    if let Ok(bytes) = std::fs::read(&path)
                                        && let Some(cache) = &s.image_cache
                                    {
                                        s.custom_cover_image = Some(cache.load(bytes));
                                    }
                                    s.custom_cover_path = Some(path);
                                    s.auto_save_progress();
                                }
                            });
                        })
                        .child(image_icon(12.0, p.text_secondary))
                        .child(
                            text("Choose Cover…")
                                .font(TextStyle::new(11.0, 600.0))
                                .color(p.text_secondary),
                        ),
                ),
        )
        .child(
            col()
                .grow(1.0)
                .gap(10.0)
                .child(
                    row()
                        .gap(10.0)
                        .center_y()
                        .child(
                            text("NEW PLAYLIST")
                                .font(TextStyle::new(11.0, 800.0).tracking(0.8))
                                .color(p.add_green),
                        )
                        .child(
                            text(format!("• From Source Playlist: {}", s.source_playlist.name))
                                .font(TextStyle::new(11.5, 550.0))
                                .color(p.text_secondary),
                        ),
                )
                .child(
                    el().key("review-name-input-box")
                        .w_full()
                        .h(44.0)
                        .px(14.0)
                        .radius(10.0)
                        .fill(p.bg_flat_block)
                        .center_y()
                        .child(
                            text_input(&s.new_playlist_name, TextStyle::new(20.0, 750.0))
                                .key("input-review-playlist-name")
                                .label("New playlist name")
                                .single_line()
                                .w_full()
                                .color(p.text_primary)
                                .placeholder("Enter new playlist name…", p.text_muted)
                                .caret(p.add_green, p.add_green_soft)
                                .on_edit(|s: &mut State| &mut s.new_playlist_name)
                                .on_input_change(|s: &mut State, _| s.auto_save_progress()),
                        ),
                )
                .child(
                    el().key("review-desc-input-box")
                        .w_full()
                        .h(36.0)
                        .px(14.0)
                        .radius(8.0)
                        .fill(p.bg_flat_block)
                        .center_y()
                        .child(
                            text_input(&s.new_playlist_desc, TextStyle::new(13.0, 500.0))
                                .key("input-review-playlist-desc")
                                .label("Playlist description")
                                .single_line()
                                .w_full()
                                .color(p.text_secondary)
                                .placeholder("Add an optional playlist description…", p.text_muted)
                                .caret(p.add_green, p.add_green_soft)
                                .on_edit(|s: &mut State| &mut s.new_playlist_desc)
                                .on_input_change(|s: &mut State, _| s.auto_save_progress()),
                        ),
                )
                .child(
                    row()
                        .w_full()
                        .center_y()
                        .justify_between()
                        .child(
                            text(format!("{count} selected songs • {total_dur}"))
                                .font(TextStyle::new(12.5, 600.0))
                                .color(p.text_secondary),
                        )
                        .child(
                            row()
                                .gap(10.0)
                                .center_y()
                                .child(
                                    row()
                                        .key("btn-review-resume-swiping")
                                        .role("button")
                                        .label("Resume Swiping")
                                        .h(38.0)
                                        .px(16.0)
                                        .radius(19.0)
                                        .center_y()
                                        .gap(6.0)
                                        .fill(p.bg_flat_block)
                                        .hover(p.hover_fill)
                                        .cursor(Cursor::Pointer)
                                        .on_press(|s: &mut State| {
                                            if s.current_index >= s.tracks.len()
                                                && !s.tracks.is_empty()
                                            {
                                                s.current_index = s.tracks.len() - 1;
                                            }
                                            s.screen = Screen::Swiping;
                                        })
                                        .child(chevron_left_icon(15.0, p.text_primary))
                                        .child(
                                            text("Resume Swiping")
                                                .font(TextStyle::new(13.0, 650.0))
                                                .color(p.text_primary),
                                        ),
                                )
                                .child(
                                    row()
                                        .key("btn-review-save-spotify")
                                        .role("button")
                                        .label(&save_label)
                                        .h(38.0)
                                        .px(20.0)
                                        .radius(19.0)
                                        .center_y()
                                        .gap(8.0)
                                        .fill(p.add_green)
                                        .press_scale(0.97)
                                        .cursor(Cursor::Pointer)
                                        .on_press(|s: &mut State| s.trigger_save_to_spotify())
                                        .child(check_icon(16.0, 2.5, Color::BLACK))
                                        .child(
                                            text(save_label)
                                                .font(TextStyle::new(13.5, 750.0))
                                                .color(Color::BLACK),
                                        ),
                                ),
                        ),
                ),
        );

    let mut table = col()
        .key("review-tracklist")
        .w_full()
        .padding(28.0)
        .gap(4.0)
        .child(
            row()
                .w_full()
                .h(32.0)
                .px(12.0)
                .center_y()
                .child(
                    text("#")
                        .w(32.0)
                        .font(TextStyle::new(11.5, 700.0))
                        .color(p.text_muted),
                )
                .child(
                    text("Title")
                        .w(0.0)
                        .grow(1.5)
                        .font(TextStyle::new(11.5, 700.0))
                        .color(p.text_muted),
                )
                .child(
                    text("Album")
                        .w(0.0)
                        .grow(1.0)
                        .font(TextStyle::new(11.5, 700.0))
                        .color(p.text_muted),
                )
                .child(
                    text("Time")
                        .w(56.0)
                        .font(TextStyle::new(11.5, 700.0))
                        .color(p.text_muted),
                )
                .child(el().w(34.0)),
        );

    for (idx, track) in s.selected_tracks.iter().enumerate() {
        let track_id = track.id.clone();
        table = table.child(
            row()
                .key(format!("review-row-{}", track.id))
                .w_full()
                .h(48.0)
                .px(12.0)
                .center_y()
                .radius(8.0)
                .hover_group()
                .hover(p.hover_fill)
                .child(
                    text(format!("{}", idx + 1))
                        .w(32.0)
                        .font(TextStyle::new(12.5, 500.0).features("'tnum' 1"))
                        .color(p.text_muted),
                )
                .child(
                    row()
                        .w(0.0)
                        .grow(1.5)
                        .gap(10.0)
                        .center_y()
                        .pr(12.0)
                        .child(track_thumb_el(s, track, 34.0, 6.0))
                        .child(
                            col()
                                .grow(1.0)
                                .gap(1.0)
                                .child(
                                    text(track.name.clone())
                                        .font(TextStyle::new(13.5, 650.0))
                                        .color(p.text_primary)
                                        .truncate(),
                                )
                                .child(
                                    text(track.artists.clone())
                                        .font(TextStyle::new(11.5, 500.0))
                                        .color(p.text_secondary)
                                        .truncate(),
                                ),
                        ),
                )
                .child(
                    text(track.album.clone())
                        .w(0.0)
                        .grow(1.0)
                        .pr(12.0)
                        .font(TextStyle::new(12.5, 500.0))
                        .color(p.text_secondary)
                        .truncate(),
                )
                .child(
                    text(track.formatted_duration())
                        .w(56.0)
                        .font(TextStyle::new(12.5, 500.0).features("'tnum' 1"))
                        .color(p.text_muted),
                )
                .child(
                    el().key(format!("review-remove-{}", track.id))
                        .role("button")
                        .label("Remove from selection")
                        .help("Remove from selection")
                        .size(28.0, 28.0)
                        .radius(14.0)
                        .center()
                        .group_opacity(0.0, 1.0)
                        .hover(p.skip_coral_soft)
                        .cursor(Cursor::Pointer)
                        .on_press(move |s: &mut State| {
                            s.remove_from_selection(&track_id);
                        })
                        .child(x_icon(14.0, 2.2, p.skip_coral)),
                ),
        );
    }

    col()
        .key("review-stage")
        .w_full()
        .grow(1.0)
        .min_h(0.0)
        .motion(apple_spring_ui())
        .transition(
            Transition::scale(0.97)
                .combined(Transition::offset(0.0, 12.0))
                .combined(Transition::opacity()),
        )
        .child(banner)
        .child(
            col()
                .w_full()
                .grow(1.0)
                .min_h(0.0)
                .child(
                    table
                        .key("review-table-scroll")
                        .scroll(ScrollSpec { stick_bottom: false })
                        .grow(1.0)
                        .min_h(0.0),
                ),
        )
}

// ============================================================================
// Screen 4: Saved Confirmation
// ============================================================================

fn saved_stage_view(s: &State, p: &Palette, m: &LayoutMetrics) -> El {
    let url = s
        .saved_playlist_url
        .clone()
        .unwrap_or_else(|| "https://open.spotify.com".into());
    let card_w = (m.win_w * 0.34).clamp(420.0, 560.0).round();

    col()
        .key("saved-stage")
        .w_full()
        .grow(1.0)
        .center()
        .motion(apple_spring_ui())
        .transition(
            Transition::scale(0.95)
                .combined(Transition::offset(0.0, 14.0))
                .combined(Transition::opacity()),
        )
        .child(
            col()
                .w(card_w)
                .padding(36.0)
                .gap(18.0)
                .items_center()
                .radius(20.0)
                .fill(p.bg_card)
                .shadow(20.0, 48.0, Color::BLACK.alpha(0.5))
                .child(
                    el().size(68.0, 68.0)
                        .radius(34.0)
                        .fill(p.add_green)
                        .center()
                        .child(check_icon(34.0, 3.2, Color::BLACK)),
                )
                .child(
                    text("New Playlist Saved")
                        .font(TextStyle::new(26.0, 800.0).tracking(-0.4))
                        .color(p.text_primary),
                )
                .child(
                    text(format!(
                        "\"{}\" ({} selected songs) is ready.",
                        s.new_playlist_name.text.trim(),
                        s.selected_tracks.len()
                    ))
                    .font(TextStyle::new(14.0, 500.0))
                    .color(p.text_secondary)
                    .wrap(),
                )
                .child(
                    row()
                        .key("btn-saved-open-spotify")
                        .role("button")
                        .label("Open Playlist in Spotify")
                        .w_full()
                        .h(44.0)
                        .radius(22.0)
                        .center()
                        .gap(8.0)
                        .fill(p.add_green)
                        .cursor(Cursor::Pointer)
                        .on_press_tx(move |_: &mut State, tx: &mut Tx| {
                            let _ = tx.open_url(&url);
                        })
                        .child(external_link_icon(17.0, Color::BLACK))
                        .child(
                            text("Open Playlist in Spotify")
                                .font(TextStyle::new(14.0, 750.0))
                                .color(Color::BLACK),
                        ),
                )
                .child(
                    row()
                        .key("btn-saved-pick-another")
                        .role("button")
                        .label("Choose Another Source Playlist")
                        .w_full()
                        .h(42.0)
                        .radius(21.0)
                        .center()
                        .fill(p.bg_flat_pill)
                        .hover(p.hover_fill)
                        .cursor(Cursor::Pointer)
                        .on_press(|s: &mut State| {
                            s.screen = Screen::SourcePicker;
                        })
                        .child(
                            text("Choose Another Source Playlist")
                                .font(TextStyle::new(13.5, 650.0))
                                .color(p.text_primary),
                        ),
                ),
        )
}

// ============================================================================
// Modal: Spotify OAuth 2.0 PKCE Setup
// ============================================================================

fn auth_modal_view(s: &State, p: &Palette, m: &LayoutMetrics) -> El {
    let modal_w = (m.win_w * 0.38).clamp(460.0, 580.0).round();
    let mut dialog = col()
        .key("auth-modal-dialog")
        .label("Connect Spotify Account")
        .w(modal_w)
        .padding(28.0)
        .gap(16.0)
        .radius(18.0)
        .fill(p.bg_sidebar)
        .shadow(24.0, 56.0, Color::BLACK.alpha(0.75))
        .motion(apple_spring_sheet())
        .transition(
            Transition::scale(0.95)
                .combined(Transition::offset(0.0, 16.0))
                .combined(Transition::opacity()),
        )
        .modal(true, |s: &mut State| s.close_auth_modal())
        .child(
            row()
                .w_full()
                .center_y()
                .justify_between()
                .child(
                    text("Connect Spotify Account")
                        .font(TextStyle::new(20.0, 800.0))
                        .color(p.text_primary),
                )
                .child(
                    el().key("btn-close-auth-modal")
                        .role("button")
                        .label("Close")
                        .size(30.0, 30.0)
                        .radius(15.0)
                        .center()
                        .fill(p.bg_flat_block)
                        .hover(p.hover_fill)
                        .cursor(Cursor::Pointer)
                        .on_press(|s: &mut State| s.close_auth_modal())
                        .child(x_icon(14.0, 2.2, p.text_secondary)),
                ),
        )
        .child(
            text("Enter your Spotify Developer Client ID below. Swipelist connects via OAuth 2.0 PKCE on 127.0.0.1:8888 to load your playlists and save selections.")
                .font(TextStyle::new(13.0, 450.0))
                .color(p.text_secondary)
                .wrap(),
        );

    if let Some(err) = &s.auth_error {
        dialog = dialog.child(
            row()
                .key("auth-modal-error")
                .w_full()
                .padding(12.0)
                .radius(10.0)
                .fill(p.skip_coral_soft)
                .child(
                    text(err.clone())
                        .font(TextStyle::new(12.5, 600.0))
                        .color(p.skip_coral)
                        .wrap(),
                ),
        );
    }

    if s.auth_waiting {
        dialog = dialog.child(
            col()
                .key("auth-modal-waiting")
                .w_full()
                .padding(12.0)
                .gap(8.0)
                .radius(10.0)
                .fill(p.add_green_soft)
                .child(
                    text("Browser opened — waiting for Spotify authorization on http://127.0.0.1:8888/callback…")
                        .font(TextStyle::new(12.5, 650.0))
                        .color(p.add_green)
                        .wrap(),
                )
                .child(
                    row()
                        .w_full()
                        .center_y()
                        .justify_between()
                        .child(
                            text("If your browser did not redirect automatically:")
                                .font(TextStyle::new(11.5, 500.0))
                                .color(p.text_secondary),
                        )
                        .child(
                            el().key("btn-paste-callback-url")
                                .role("button")
                                .label("Paste Callback URL from Clipboard")
                                .h(26.0)
                                .px(10.0)
                                .radius(13.0)
                                .fill(p.bg_flat_block)
                                .center()
                                .cursor(Cursor::Pointer)
                                .on_press_tx(|_: &mut State, tx: &mut Tx| {
                                    tx.read_clipboard(|s: &mut State, content, _| {
                                        let pasted = content.text().trim().to_string();
                                        if pasted.is_empty() {
                                            s.auth_error = Some("Clipboard is empty. Copy the http://127.0.0.1:8888/callback?code=... URL from your browser first.".into());
                                            return;
                                        }
                                        if let Some(pkce) = s.pending_pkce.clone() {
                                            cancel_pending_oauth_listener();
                                            s.worker.request(pasted.clone(), move |url| {
                                                AsyncResult::LoggedIn(exchange_callback_query(&pkce, &url))
                                            });
                                        }
                                    });
                                })
                                .child(
                                    text("Paste Callback URL")
                                        .font(TextStyle::new(11.0, 700.0))
                                        .color(p.add_green),
                                ),
                        ),
                ),
        );
    }

    let client_id_section = col()
        .w_full()
        .gap(6.0)
        .child(
            row()
                .w_full()
                .center_y()
                .justify_between()
                .child(
                    text("Spotify Developer Client ID")
                        .font(TextStyle::new(12.0, 700.0))
                        .color(p.text_primary),
                )
                .child(
                    el().key("btn-paste-client-id")
                        .role("button")
                        .label("Paste Client ID from Clipboard")
                        .h(24.0)
                        .px(10.0)
                        .radius(12.0)
                        .fill(p.bg_flat_block)
                        .hover(p.hover_fill)
                        .center()
                        .cursor(Cursor::Pointer)
                        .on_press_tx(|_: &mut State, tx: &mut Tx| {
                            tx.read_clipboard(|s: &mut State, content, _| {
                                let text = content.text().trim();
                                if !text.is_empty() {
                                    s.client_id_edit.replace_text(text);
                                    s.auth_error = None;
                                }
                            });
                        })
                        .child(
                            text("Paste from Clipboard")
                                .font(TextStyle::new(11.0, 650.0))
                                .color(p.add_green),
                        ),
                ),
        )
        .child(
            el().w_full()
                .h(42.0)
                .px(14.0)
                .radius(10.0)
                .fill(p.bg_flat_block)
                .center_y()
                .child(
                    text_input(&s.client_id_edit, TextStyle::new(13.5, 550.0))
                        .key("input-spotify-client-id")
                        .label("Spotify Developer Client ID")
                        .autofocus()
                        .single_line()
                        .w_full()
                        .color(p.text_primary)
                        .placeholder("Paste your 32-character Client ID…", p.text_muted)
                        .caret(p.add_green, p.add_green_soft)
                        .on_edit(|s: &mut State| &mut s.client_id_edit)
                        .on_submit(|s: &mut State, tx: &mut Tx| {
                            s.start_spotify_oauth(tx);
                        }),
                ),
        );

    let redirect_box = col()
        .w_full()
        .padding(12.0)
        .gap(6.0)
        .radius(10.0)
        .fill(p.bg_flat_block)
        .child(
            row()
                .w_full()
                .center_y()
                .justify_between()
                .child(
                    text("Redirect URI (add in Spotify App Settings):")
                        .font(TextStyle::new(11.5, 600.0))
                        .color(p.text_secondary),
                )
                .child(
                    text(if s.copied_redirect_uri {
                        "Copied!"
                    } else {
                        "Copy URI"
                    })
                    .key("btn-copy-redirect-uri")
                    .font(TextStyle::new(11.5, 700.0))
                    .color(p.add_green)
                    .cursor(Cursor::Pointer)
                    .on_press_tx(|s: &mut State, tx: &mut Tx| {
                        tx.copy(REDIRECT_URI);
                        s.copied_redirect_uri = true;
                    }),
                ),
        )
        .child(
            text(REDIRECT_URI)
                .font(TextStyle::new(12.0, 600.0).mono())
                .color(p.add_green),
        );

    let actions_row = row()
        .w_full()
        .gap(10.0)
        .center_y()
        .child(
            row()
                .key("btn-open-spotify-dashboard")
                .role("button")
                .label("Open Spotify Developer Dashboard")
                .h(40.0)
                .px(16.0)
                .radius(20.0)
                .center_y()
                .gap(6.0)
                .fill(p.bg_flat_block)
                .hover(p.hover_fill)
                .cursor(Cursor::Pointer)
                .on_press_tx(|_: &mut State, tx: &mut Tx| {
                    let _ = tx.open_url("https://developer.spotify.com/dashboard");
                })
                .child(
                    text("Spotify Dashboard")
                        .font(TextStyle::new(12.5, 650.0))
                        .color(p.text_primary),
                )
                .child(external_link_icon(13.0, p.text_secondary)),
        )
        .child(spacer())
        .children(s.auth_waiting.then(|| {
            row()
                .key("btn-cancel-oauth")
                .role("button")
                .label("Cancel login")
                .h(40.0)
                .px(16.0)
                .radius(20.0)
                .center()
                .fill(p.skip_coral_soft)
                .cursor(Cursor::Pointer)
                .on_press(|s: &mut State| {
                    cancel_pending_oauth_listener();
                    s.auth_waiting = false;
                })
                .child(
                    text("Cancel")
                        .font(TextStyle::new(12.5, 700.0))
                        .color(p.skip_coral),
                )
        }))
        .child(
            row()
                .key("btn-start-oauth-login")
                .role("button")
                .label("Authorize with Spotify")
                .default_button()
                .h(40.0)
                .px(20.0)
                .radius(20.0)
                .center()
                .fill(p.add_green)
                .press_scale(0.97)
                .cursor(Cursor::Pointer)
                .on_press_tx(|s: &mut State, tx: &mut Tx| {
                    s.start_spotify_oauth(tx);
                })
                .child(
                    text(if s.auth_waiting {
                        "Re-open Spotify Login"
                    } else {
                        "Authorize with Spotify"
                    })
                    .font(TextStyle::new(13.0, 750.0))
                    .color(Color::BLACK),
                ),
        );

    dialog = dialog
        .child(client_id_section)
        .child(redirect_box)
        .child(actions_row);

    el().key("auth-modal-backdrop")
        .pin(Some(0.0), Some(0.0), Some(0.0), Some(0.0))
        .fill(Color::BLACK.alpha(0.70))
        .backdrop_blur(10.0)
        .center()
        .child(dialog)
}

fn main() {
    rune::run(AppSpec::<State> {
        min_size: Some((960.0, 640.0)),
        setup: Some(setup),
        poll: Some(poll),
        on_key: Some(on_key),
        scenes: &[
            ("swiping", |_| {}),
            ("drag-add", |s| {
                s.is_dragging = true;
                s.drag_dx = 115.0;
                s.drag_dy = -14.0;
                s.drag_pivot_top = true;
            }),
            ("drag-skip", |s| {
                s.is_dragging = true;
                s.drag_dx = -115.0;
                s.drag_dy = 12.0;
                s.drag_pivot_top = true;
            }),
            ("source-picker", |s| {
                s.screen = Screen::SourcePicker;
            }),
            ("review", |s| {
                s.screen = Screen::Review;
            }),
            ("saved", |s| {
                s.screen = Screen::Saved;
            }),
            ("auth-modal", |s| {
                s.open_auth_modal();
            }),
        ],
        ..AppSpec::new("Swipelist", WINDOW_SIZE, State::default, view)
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shuffle_and_restore_preserves_prefix_and_original_order() {
        let mut s = State::default();
        let all_ids: Vec<String> = s.tracks.iter().map(|t| t.id.clone()).collect();
        assert_eq!(s.unshuffled_order, all_ids);
        assert!(!s.shuffled);

        let prefix_before: Vec<String> = s.tracks[..=s.current_index]
            .iter()
            .map(|t| t.id.clone())
            .collect();
        let mut expected_tail_sorted: Vec<String> = s.tracks[s.current_index + 1..]
            .iter()
            .map(|t| t.id.clone())
            .collect();
        expected_tail_sorted.sort();

        s.toggle_shuffle();
        assert!(s.shuffled);

        let prefix_after_shuffle: Vec<String> = s.tracks[..=s.current_index]
            .iter()
            .map(|t| t.id.clone())
            .collect();
        assert_eq!(prefix_after_shuffle, prefix_before);

        let mut actual_tail_sorted: Vec<String> = s.tracks[s.current_index + 1..]
            .iter()
            .map(|t| t.id.clone())
            .collect();
        actual_tail_sorted.sort();
        assert_eq!(actual_tail_sorted, expected_tail_sorted);

        // Simulate advancing past one more track while shuffled
        if s.current_index + 1 < s.tracks.len() {
            s.current_index += 1;
        }
        let active_id_before_unshuffle = s.tracks.get(s.current_index).map(|t| t.id.clone());

        s.toggle_shuffle();
        assert!(!s.shuffled);
        assert_eq!(
            s.tracks.get(s.current_index).map(|t| t.id.clone()),
            active_id_before_unshuffle
        );

        let restored_tail_positions: Vec<usize> = s.tracks[s.current_index + 1..]
            .iter()
            .filter_map(|t| s.unshuffled_order.iter().position(|id| id == &t.id))
            .collect();
        assert_eq!(restored_tail_positions.len(), s.tracks.len() - s.current_index - 1);
        assert!(restored_tail_positions.windows(2).all(|w| w[0] < w[1]));
    }

    #[test]
    fn select_source_playlist_resets_shuffle_and_updates_order() {
        let mut s = State::default();
        s.toggle_shuffle();
        assert!(s.shuffled);

        let next_pl = s.playlists[1].clone();
        s.select_source_playlist(next_pl);
        assert!(!s.shuffled);

        let track_ids: Vec<String> = s.tracks.iter().map(|t| t.id.clone()).collect();
        assert_eq!(s.unshuffled_order, track_ids);
    }

    #[test]
    fn restore_progress_preserves_completed_deck() {
        let mut s = State::default();
        let total = s.tracks.len();
        assert!(total > 0);
        let last_track = s.tracks[total - 1].clone();

        let entry = PlaylistProgress {
            playlist_id: s.source_playlist.id.clone(),
            playlist_name: s.source_playlist.name.clone(),
            current_index: total,
            total_tracks: total,
            new_playlist_name: "Saved Name".to_string(),
            new_playlist_desc: "Saved Desc".to_string(),
            is_public: false,
            custom_cover_path: None,
            selected_tracks: vec![last_track.clone()],
            history: vec![HistoryEntry {
                track: last_track,
                decision: SwipeDecision::Add,
                index: total - 1,
            }],
        };
        s.progress_store
            .sessions
            .insert(s.source_playlist.id.clone(), entry);

        assert!(s.restore_progress_for_current_playlist());
        assert_eq!(s.current_index, total);
        assert!(s.current_track().is_none());
    }

    #[test]
    fn cover_failure_retries_once_and_stops() {
        let mut s = State::default();
        let key = "track:demo-1".to_string();
        s.requested_covers.insert(key.clone());

        assert!(State::record_cover_failure(
            &mut s.cover_failures,
            &mut s.requested_covers,
            &key
        ));
        assert!(!s.requested_covers.contains(&key));
        assert_eq!(s.cover_failures.get(&key).copied(), Some(1));

        s.requested_covers.insert(key.clone());
        assert!(!State::record_cover_failure(
            &mut s.cover_failures,
            &mut s.requested_covers,
            &key
        ));
        assert!(s.requested_covers.contains(&key));
        assert!(!s.cover_failures.contains_key(&key));
    }
}
