//! Data models, progress auto-saving, cover art & audio preview caching, and Spotify OAuth 2.0 PKCE + Web API client.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use serde::{Deserialize, Serialize};

pub const REDIRECT_URI: &str = "http://127.0.0.1:8888/callback";
const CALLBACK_ADDR: &str = "127.0.0.1:8888";

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Track {
    pub id: String,
    pub uri: String,
    pub name: String,
    pub artists: String,
    pub album: String,
    pub year: String,
    pub genre_tag: String,
    pub duration_ms: u32,
    pub image_url: Option<String>,
    #[serde(default)]
    pub preview_url: Option<String>,
    pub accent_hue: f32,
}

impl Track {
    pub fn formatted_duration(&self) -> String {
        format_seconds(self.duration_ms / 1000)
    }

    pub fn spotify_track_id(&self) -> &str {
        self.uri
            .strip_prefix("spotify:track:")
            .unwrap_or(&self.id)
    }

    pub fn spotify_url(&self) -> String {
        let tid = self.spotify_track_id();
        if tid.starts_with("demo-") {
            let query = url_encode(&format!("{} {}", self.name, self.artists));
            format!("https://open.spotify.com/search/{query}")
        } else {
            format!("https://open.spotify.com/track/{tid}")
        }
    }
}

pub fn format_seconds(total_secs: u32) -> String {
    let mins = total_secs / 60;
    let secs = total_secs % 60;
    format!("{mins}:{secs:02}")
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PlaylistSummary {
    pub id: String,
    pub name: String,
    pub owner_name: String,
    pub track_count: usize,
    pub image_url: Option<String>,
    pub is_liked_songs: bool,
    pub accent_hue: f32,
    pub subtitle: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct UserProfile {
    pub id: String,
    pub display_name: String,
    pub product: String,
    #[serde(default)]
    pub image_url: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum SwipeDecision {
    Add,
    Skip,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct HistoryEntry {
    pub track: Track,
    pub decision: SwipeDecision,
    pub index: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SourceFilter {
    All,
    LikedSongs,
    Playlists,
}

// ============================================================================
// Progress Auto-Save JSON Store (`swipelist_progress.json`)
// ============================================================================

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct PlaylistProgress {
    pub playlist_id: String,
    pub playlist_name: String,
    pub current_index: usize,
    pub total_tracks: usize,
    pub new_playlist_name: String,
    pub new_playlist_desc: String,
    pub is_public: bool,
    pub custom_cover_path: Option<String>,
    pub selected_tracks: Vec<Track>,
    pub history: Vec<HistoryEntry>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ProgressStore {
    pub active_playlist_id: Option<String>,
    pub sessions: HashMap<String, PlaylistProgress>,
}

fn app_data_dir() -> PathBuf {
    let base = std::env::var_os("APPDATA")
        .or_else(|| std::env::var_os("HOME"))
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    let dir = base.join("swipelist");
    let _ = std::fs::create_dir_all(&dir);
    dir
}

pub fn local_progress_file_path() -> PathBuf {
    std::env::current_dir()
        .unwrap_or_else(|_| app_data_dir())
        .join("swipelist_progress.json")
}

fn appdata_progress_file_path() -> PathBuf {
    app_data_dir().join("swipelist_progress.json")
}

pub fn load_progress_store() -> ProgressStore {
    let local = local_progress_file_path();
    if let Ok(raw) = std::fs::read_to_string(&local)
        && let Ok(store) = serde_json::from_str::<ProgressStore>(&raw)
    {
        return store;
    }
    let appdata = appdata_progress_file_path();
    std::fs::read_to_string(appdata)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

pub fn save_progress_store(store: &ProgressStore) {
    if let Ok(json) = serde_json::to_string_pretty(store) {
        let _ = std::fs::write(local_progress_file_path(), &json);
        let _ = std::fs::write(appdata_progress_file_path(), &json);
    }
}

// ============================================================================
// Cover Art & Audio Preview Disk Cache
// ============================================================================

fn simple_hash(s: &str) -> String {
    let digest = sha256(s.as_bytes());
    let mut hex = String::with_capacity(24);
    for b in &digest[..12] {
        hex.push_str(&format!("{b:02x}"));
    }
    hex
}

/// Downloads cover art JPEG bytes for a given image URL (or Spotify track URI fallback via oEmbed)
/// and caches them on disk in `%APPDATA%\swipelist\covers\`.
pub fn fetch_cover_bytes(image_url: Option<&str>, spotify_track_id: Option<&str>) -> Option<Vec<u8>> {
    let covers_dir = app_data_dir().join("covers");
    let _ = std::fs::create_dir_all(&covers_dir);

    let key = image_url
        .filter(|u| !u.is_empty())
        .or(spotify_track_id)
        .unwrap_or("");
    if key.is_empty() {
        return None;
    }

    let cache_file = covers_dir.join(format!("{}.jpg", simple_hash(key)));
    if cache_file.exists()
        && let Ok(bytes) = std::fs::read(&cache_file)
        && bytes.len() > 256
    {
        return Some(bytes);
    }

    // Resolve URL if only a Spotify track ID was available
    let resolved_url = if let Some(u) = image_url.filter(|u| !u.is_empty()) {
        u.to_string()
    } else if let Some(tid) = spotify_track_id.filter(|id| !id.starts_with("demo-")) {
        let oembed_url = format!("https://open.spotify.com/oembed?url=https://open.spotify.com/track/{tid}");
        let raw = run_curl(&["-L", &oembed_url], None).ok()?;
        let val: serde_json::Value = serde_json::from_str(&raw).ok()?;
        val["thumbnail_url"].as_str()?.to_string()
    } else {
        return None;
    };

    let cache_str = cache_file.to_string_lossy().into_owned();
    let _ = run_curl(&["-L", "-o", &cache_str, &resolved_url], None).ok()?;
    let bytes = std::fs::read(&cache_file).ok()?;
    if bytes.len() > 256 {
        Some(bytes)
    } else {
        let _ = std::fs::remove_file(&cache_file);
        None
    }
}

/// Resolves and downloads the 30-second MP3 audio stream for a track (from `track.preview_url` or
/// Spotify's embed player page) and caches it in `%APPDATA%\swipelist\audio\<id>.mp3`.
pub fn fetch_track_audio_file(track: &Track) -> Result<PathBuf, String> {
    let audio_dir = app_data_dir().join("audio");
    let _ = std::fs::create_dir_all(&audio_dir);

    let tid = track.spotify_track_id();
    let mp3_path = audio_dir.join(format!("{}.mp3", simple_hash(tid)));
    if mp3_path.exists()
        && let Ok(meta) = std::fs::metadata(&mp3_path)
        && meta.len() > 1024
    {
        return Ok(mp3_path);
    }

    let preview_url = if let Some(u) = track.preview_url.as_deref().filter(|u| !u.is_empty()) {
        u.to_string()
    } else {
        let embed_url = format!("https://open.spotify.com/embed/track/{tid}");
        let html = run_curl(&["-L", &embed_url], None)?;
        extract_audio_preview_from_embed(&html)
            .ok_or_else(|| format!("No audio stream available for \"{}\".", track.name))?
    };

    let out_str = mp3_path.to_string_lossy().into_owned();
    run_curl(&["-L", "-o", &out_str, &preview_url], None)?;

    let len = std::fs::metadata(&mp3_path).map(|m| m.len()).unwrap_or(0);
    if len <= 1024 {
        let _ = std::fs::remove_file(&mp3_path);
        return Err(format!("Failed to download audio stream for \"{}\".", track.name));
    }

    Ok(mp3_path)
}

fn extract_audio_preview_from_embed(html: &str) -> Option<String> {
    let needle = "\"audioPreview\":{\"url\":\"";
    let start = html.find(needle)? + needle.len();
    let rest = &html[start..];
    let end = rest.find('"')?;
    let raw_url = &rest[..end];
    if raw_url.starts_with("http") {
        Some(raw_url.replace("\\u0026", "&").replace("\\/", "/"))
    } else {
        None
    }
}

// ============================================================================
// Auth Config Persistence
// ============================================================================

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct StoredConfig {
    pub client_id: String,
    pub refresh_token: Option<String>,
    pub access_token: Option<String>,
}

fn config_file_path() -> PathBuf {
    app_data_dir().join("config.json")
}

pub fn load_config() -> StoredConfig {
    let path = config_file_path();
    let mut cfg: StoredConfig = std::fs::read_to_string(path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default();

    if cfg.client_id.trim().is_empty()
        && let Ok(env_id) = std::env::var("SPOTIFY_CLIENT_ID")
            .or_else(|_| std::env::var("VITE_SPOTIFY_CLIENT_ID"))
    {
        cfg.client_id = env_id.trim().to_string();
    }
    cfg
}

pub fn save_config(cfg: &StoredConfig) {
    let path = config_file_path();
    if let Ok(json) = serde_json::to_string_pretty(cfg) {
        let _ = std::fs::write(path, json);
    }
}

pub fn format_total_duration(tracks: &[Track]) -> String {
    let total_ms: u64 = tracks.iter().map(|t| u64::from(t.duration_ms)).sum();
    let total_mins = total_ms / 60_000;
    let hrs = total_mins / 60;
    let mins = total_mins % 60;
    if hrs > 0 {
        format!("{hrs} hr {mins} min")
    } else if mins > 0 {
        format!("{mins} min")
    } else {
        let secs = (total_ms / 1000) % 60;
        format!("{secs} sec")
    }
}

/// Starter Source Playlists available immediately at launch and for deterministic headless scenes.
pub fn starter_playlists() -> Vec<PlaylistSummary> {
    vec![
        PlaylistSummary {
            id: "liked-songs".into(),
            name: "Liked Songs".into(),
            owner_name: "Your Library".into(),
            track_count: 12,
            image_url: None,
            is_liked_songs: true,
            accent_hue: 152.0,
            subtitle: "Pinned • All your saved tracks".into(),
        },
        PlaylistSummary {
            id: "late-night-electronic".into(),
            name: "Late Night Electronic".into(),
            owner_name: "Spotify Curator".into(),
            track_count: 8,
            image_url: None,
            is_liked_songs: false,
            accent_hue: 268.0,
            subtitle: "Deep house, UK garage & melodic techno".into(),
        },
        PlaylistSummary {
            id: "indie-synthwave".into(),
            name: "Indie & Synthwave Essentials".into(),
            owner_name: "Alex Rivera".into(),
            track_count: 8,
            image_url: None,
            is_liked_songs: false,
            accent_hue: 335.0,
            subtitle: "Dream pop, analog synths & driving basslines".into(),
        },
        PlaylistSummary {
            id: "deep-focus-instrumental".into(),
            name: "Deep Focus Instrumental".into(),
            owner_name: "Studio Archive".into(),
            track_count: 6,
            image_url: None,
            is_liked_songs: false,
            accent_hue: 215.0,
            subtitle: "Ambient piano, modular textures & post-rock".into(),
        },
        PlaylistSummary {
            id: "weekend-groove-crate".into(),
            name: "Weekend Groove Crate".into(),
            owner_name: "You".into(),
            track_count: 7,
            image_url: None,
            is_liked_songs: false,
            accent_hue: 38.0,
            subtitle: "Nu-disco, funk edits & warm soul cuts".into(),
        },
        PlaylistSummary {
            id: "workout-high-tempo".into(),
            name: "High Tempo Run Club".into(),
            owner_name: "You".into(),
            track_count: 6,
            image_url: None,
            is_liked_songs: false,
            accent_hue: 14.0,
            subtitle: "128+ BPM club & drum-and-bass energy".into(),
        },
    ]
}

pub fn starter_tracks_for(playlist_id: &str) -> Vec<Track> {
    match playlist_id {
        "late-night-electronic" => vec![
            Track {
                id: "demo-lne-1".into(),
                uri: "spotify:track:4cOdK2wGLETKBW3PvgPWqT".into(),
                name: "Archangel".into(),
                artists: "Burial".into(),
                album: "Untrue".into(),
                year: "2007".into(),
                genre_tag: "UK Garage • Future Garage".into(),
                duration_ms: 238_000,
                image_url: None,
                preview_url: None,
                accent_hue: 255.0,
            },
            Track {
                id: "demo-lne-2".into(),
                uri: "spotify:track:6W21LNLz9Sw7sUSNWMSHRu".into(),
                name: "Emerald Rush".into(),
                artists: "Jon Hopkins".into(),
                album: "Singularity".into(),
                year: "2018".into(),
                genre_tag: "IDM • Microhouse".into(),
                duration_ms: 326_000,
                image_url: None,
                preview_url: None,
                accent_hue: 165.0,
            },
            Track {
                id: "demo-lne-3".into(),
                uri: "spotify:track:2VxeLyX666F8uXCJ0dZF8B".into(),
                name: "LesAlpx".into(),
                artists: "Floating Points".into(),
                album: "Crush".into(),
                year: "2019".into(),
                genre_tag: "Leftfield Electronica".into(),
                duration_ms: 282_000,
                image_url: None,
                preview_url: None,
                accent_hue: 295.0,
            },
            Track {
                id: "demo-lne-4".into(),
                uri: "spotify:track:0VjIjW4GlUZAMYd2vXMi3b".into(),
                name: "Bad Kingdom".into(),
                artists: "Moderat".into(),
                album: "II".into(),
                year: "2013".into(),
                genre_tag: "Berlin Electronica".into(),
                duration_ms: 263_000,
                image_url: None,
                preview_url: None,
                accent_hue: 210.0,
            },
            Track {
                id: "demo-lne-5".into(),
                uri: "spotify:track:7qiZfU4dY1lWllzX7mPBI3".into(),
                name: "So U Kno".into(),
                artists: "Overmono".into(),
                album: "Good Lies".into(),
                year: "2023".into(),
                genre_tag: "Breakbeat • UK Bass".into(),
                duration_ms: 274_000,
                image_url: None,
                preview_url: None,
                accent_hue: 145.0,
            },
            Track {
                id: "demo-lne-6".into(),
                uri: "spotify:track:3jjBPF4NDMCU51psU8JPpR".into(),
                name: "Can't Do Without You".into(),
                artists: "Caribou".into(),
                album: "Our Love".into(),
                year: "2014".into(),
                genre_tag: "Electronic • House".into(),
                duration_ms: 236_000,
                image_url: None,
                preview_url: None,
                accent_hue: 38.0,
            },
            Track {
                id: "demo-lne-7".into(),
                uri: "spotify:track:5HCyWlXZPP0y6Gqq8TgA20".into(),
                name: "Kerala".into(),
                artists: "Bonobo".into(),
                album: "Migration".into(),
                year: "2017".into(),
                genre_tag: "Downtempo • Electronica".into(),
                duration_ms: 238_000,
                image_url: None,
                preview_url: None,
                accent_hue: 192.0,
            },
            Track {
                id: "demo-lne-8".into(),
                uri: "spotify:track:1mea3bSkSGXuIRvnydlB5b".into(),
                name: "Xtal".into(),
                artists: "Aphex Twin".into(),
                album: "Selected Ambient Works 85-92".into(),
                year: "1992".into(),
                genre_tag: "Ambient Techno".into(),
                duration_ms: 294_000,
                image_url: None,
                preview_url: None,
                accent_hue: 240.0,
            },
        ],
        "indie-synthwave" => vec![
            Track {
                id: "demo-isw-1".into(),
                uri: "spotify:track:1eyzqe2QqGZUmfcPZtrIyt".into(),
                name: "Midnight City".into(),
                artists: "M83".into(),
                album: "Hurry Up, We're Dreaming".into(),
                year: "2011".into(),
                genre_tag: "Synthpop • Indie".into(),
                duration_ms: 243_000,
                image_url: None,
                preview_url: None,
                accent_hue: 325.0,
            },
            Track {
                id: "demo-isw-2".into(),
                uri: "spotify:track:0U0ldCRmgCqhVvD6ksG63j".into(),
                name: "Nightcall".into(),
                artists: "Kavinsky".into(),
                album: "OutRun".into(),
                year: "2010".into(),
                genre_tag: "Synthwave • French Electro".into(),
                duration_ms: 258_000,
                image_url: None,
                preview_url: None,
                accent_hue: 355.0,
            },
            Track {
                id: "demo-isw-3".into(),
                uri: "spotify:track:4w3tQBXhn5345eUXDGBWZG".into(),
                name: "Shadow".into(),
                artists: "Chromatics".into(),
                album: "Closer to Grey".into(),
                year: "2015".into(),
                genre_tag: "Dream Pop • Italo Disco".into(),
                duration_ms: 224_000,
                image_url: None,
                preview_url: None,
                accent_hue: 280.0,
            },
            Track {
                id: "demo-isw-4".into(),
                uri: "spotify:track:5O2P9iiztwhomNh8xkR9lJ".into(),
                name: "Walking On A Dream".into(),
                artists: "Empire of the Sun".into(),
                album: "Walking On A Dream".into(),
                year: "2008".into(),
                genre_tag: "Indie Pop • Electropop".into(),
                duration_ms: 198_000,
                image_url: None,
                preview_url: None,
                accent_hue: 32.0,
            },
            Track {
                id: "demo-isw-5".into(),
                uri: "spotify:track:3AA28KZvwAUcZuOKwyblJQ".into(),
                name: "1901".into(),
                artists: "Phoenix".into(),
                album: "Wolfgang Amadeus Phoenix".into(),
                year: "2009".into(),
                genre_tag: "Indie Rock • French Pop".into(),
                duration_ms: 193_000,
                image_url: None,
                preview_url: None,
                accent_hue: 16.0,
            },
            Track {
                id: "demo-isw-6".into(),
                uri: "spotify:track:1jJci4qxiYcOHhQR247rEU".into(),
                name: "Kids".into(),
                artists: "MGMT".into(),
                album: "Oracular Spectacular".into(),
                year: "2007".into(),
                genre_tag: "Indie Pop • Neo-Psychedelia".into(),
                duration_ms: 302_000,
                image_url: None,
                preview_url: None,
                accent_hue: 175.0,
            },
            Track {
                id: "demo-isw-7".into(),
                uri: "spotify:track:2ZwIO3ufWLFYxtEo8927cZ".into(),
                name: "Space Song".into(),
                artists: "Beach House".into(),
                album: "Depression Cherry".into(),
                year: "2015".into(),
                genre_tag: "Dream Pop • Shoegaze".into(),
                duration_ms: 320_000,
                image_url: None,
                preview_url: None,
                accent_hue: 342.0,
            },
            Track {
                id: "demo-isw-8".into(),
                uri: "spotify:track:6ce41JVa6KFaryfmC2bu2W".into(),
                name: "Overnight".into(),
                artists: "Parcels".into(),
                album: "Overnight".into(),
                year: "2017".into(),
                genre_tag: "Indie Disco • Funk".into(),
                duration_ms: 219_000,
                image_url: None,
                preview_url: None,
                accent_hue: 52.0,
            },
        ],
        _ => vec![
            Track {
                id: "demo-lk-1".into(),
                uri: "spotify:track:2LD2gT7gwAurzdQDQtILds".into(),
                name: "Veridis Quo".into(),
                artists: "Daft Punk".into(),
                album: "Discovery".into(),
                year: "2001".into(),
                genre_tag: "French House • Synth".into(),
                duration_ms: 345_000,
                image_url: None,
                preview_url: None,
                accent_hue: 152.0,
            },
            Track {
                id: "demo-lk-2".into(),
                uri: "spotify:track:2X485T9Z5Ly0xyaghN73ed".into(),
                name: "Let It Happen".into(),
                artists: "Tame Impala".into(),
                album: "Currents".into(),
                year: "2015".into(),
                genre_tag: "Psychedelic Pop • Synth".into(),
                duration_ms: 467_000,
                image_url: None,
                preview_url: None,
                accent_hue: 288.0,
            },
            Track {
                id: "demo-lk-3".into(),
                uri: "spotify:track:0wHFktze2PHC5jDt3B17DC".into(),
                name: "Delilah (pull me out of this)".into(),
                artists: "Fred again..".into(),
                album: "Actual Life 3".into(),
                year: "2022".into(),
                genre_tag: "UK House • Dance".into(),
                duration_ms: 251_000,
                image_url: None,
                preview_url: None,
                accent_hue: 218.0,
            },
            Track {
                id: "demo-lk-4".into(),
                uri: "spotify:track:5aAx2yezTd8zXrkmtKl66Z".into(),
                name: "Starboy".into(),
                artists: "The Weeknd, Daft Punk".into(),
                album: "Starboy".into(),
                year: "2016".into(),
                genre_tag: "Alternative R&B • Pop".into(),
                duration_ms: 230_000,
                image_url: Some("https://image-cdn-fa.spotifycdn.com/image/ab67616d00001e020c8599cbde51245c128bcea9".into()),
                preview_url: Some("https://p.scdn.co/mp3-preview/57c1238d183c40da3157c2892346f58445b1377c".into()),
                accent_hue: 352.0,
            },
            Track {
                id: "demo-lk-5".into(),
                uri: "spotify:track:2aJDlirz6v2a4HREki98cP".into(),
                name: "Glue".into(),
                artists: "Bicep".into(),
                album: "Bicep".into(),
                year: "2017".into(),
                genre_tag: "Breakbeat • Melodic House".into(),
                duration_ms: 269_000,
                image_url: None,
                preview_url: None,
                accent_hue: 185.0,
            },
            Track {
                id: "demo-lk-6".into(),
                uri: "spotify:track:3cfOd4CMv2snFaKAnMdnvK".into(),
                name: "Neverender".into(),
                artists: "Justice, Tame Impala".into(),
                album: "Hyperdrama".into(),
                year: "2024".into(),
                genre_tag: "French Electro • Disco".into(),
                duration_ms: 266_000,
                image_url: None,
                preview_url: None,
                accent_hue: 28.0,
            },
            Track {
                id: "demo-lk-7".into(),
                uri: "spotify:track:41SwdQIX8Hy2u6fuEDgvWr".into(),
                name: "10%".into(),
                artists: "KAYTRANADA, Kali Uchis".into(),
                album: "BUBBA".into(),
                year: "2019".into(),
                genre_tag: "House • Neo-Soul".into(),
                duration_ms: 186_000,
                image_url: None,
                preview_url: None,
                accent_hue: 45.0,
            },
            Track {
                id: "demo-lk-8".into(),
                uri: "spotify:track:5CG9Ps5ynNjpKJHm2fK2SC".into(),
                name: "Innerbloom".into(),
                artists: "RÜFÜS DU SOL".into(),
                album: "Bloom".into(),
                year: "2016".into(),
                genre_tag: "Progressive House".into(),
                duration_ms: 578_000,
                image_url: None,
                preview_url: None,
                accent_hue: 318.0,
            },
            Track {
                id: "demo-lk-9".into(),
                uri: "spotify:track:79Jl8KMv2Xedp846p3512B".into(),
                name: "Gosh".into(),
                artists: "Jamie xx".into(),
                album: "In Colour".into(),
                year: "2015".into(),
                genre_tag: "UK Bass • Electronica".into(),
                duration_ms: 291_000,
                image_url: None,
                preview_url: None,
                accent_hue: 138.0,
            },
            Track {
                id: "demo-lk-10".into(),
                uri: "spotify:track:51ODNNDZm21HU7wI7cccRr".into(),
                name: "Latch".into(),
                artists: "Disclosure, Sam Smith".into(),
                album: "Settle".into(),
                year: "2013".into(),
                genre_tag: "UK Garage • Deep House".into(),
                duration_ms: 256_000,
                image_url: None,
                preview_url: None,
                accent_hue: 202.0,
            },
            Track {
                id: "demo-lk-11".into(),
                uri: "spotify:track:5QO79kh1waicV47BqGRL3g".into(),
                name: "(It Goes Like) Nanana".into(),
                artists: "Peggy Gou".into(),
                album: "I Hear You".into(),
                year: "2023".into(),
                genre_tag: "Balearic House • Dance".into(),
                duration_ms: 231_000,
                image_url: None,
                preview_url: None,
                accent_hue: 64.0,
            },
            Track {
                id: "demo-lk-12".into(),
                uri: "spotify:track:4Dvkj6JhhA12EX05fT7y2e".into(),
                name: "Baby again..".into(),
                artists: "Fred again.., Skrillex, Four Tet".into(),
                album: "Baby again..".into(),
                year: "2023".into(),
                genre_tag: "Tech House • Club".into(),
                duration_ms: 319_000,
                image_url: None,
                preview_url: None,
                accent_hue: 162.0,
            },
        ],
    }
}

// ============================================================================
// Pure-Rust SHA-256 & Base64-URL for Spotify OAuth 2.0 PKCE
// ============================================================================

fn sha256(data: &[u8]) -> [u8; 32] {
    const K: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
        0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
        0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
        0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
        0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
        0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
        0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
        0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
    ];
    let mut h: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a,
        0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19,
    ];
    let bit_len = (data.len() as u64) * 8;
    let mut padded = data.to_vec();
    padded.push(0x80);
    while (padded.len() % 64) != 56 {
        padded.push(0x00);
    }
    padded.extend_from_slice(&bit_len.to_be_bytes());

    for chunk in padded.chunks_exact(64) {
        let mut w = [0u32; 64];
        for (i, word) in chunk.chunks_exact(4).enumerate() {
            w[i] = u32::from_be_bytes([word[0], word[1], word[2], word[3]]);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16]
                .wrapping_add(s0)
                .wrapping_add(w[i - 7])
                .wrapping_add(s1);
        }
        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut hh] = h;
        for i in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ ((!e) & g);
            let temp1 = hh
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(K[i])
                .wrapping_add(w[i]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let temp2 = s0.wrapping_add(maj);

            hh = g;
            g = f;
            f = e;
            e = d.wrapping_add(temp1);
            d = c;
            c = b;
            b = a;
            a = temp1.wrapping_add(temp2);
        }
        for (i, val) in [a, b, c, d, e, f, g, hh].into_iter().enumerate() {
            h[i] = h[i].wrapping_add(val);
        }
    }
    let mut out = [0u8; 32];
    for (i, word) in h.into_iter().enumerate() {
        out[i * 4..i * 4 + 4].copy_from_slice(&word.to_be_bytes());
    }
    out
}

fn base64_encode_standard(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = chunk.get(1).copied().unwrap_or(0) as u32;
        let b2 = chunk.get(2).copied().unwrap_or(0) as u32;
        let triple = (b0 << 16) | (b1 << 8) | b2;
        out.push(TABLE[((triple >> 18) & 0x3F) as usize] as char);
        out.push(TABLE[((triple >> 12) & 0x3F) as usize] as char);
        if chunk.len() > 1 {
            out.push(TABLE[((triple >> 6) & 0x3F) as usize] as char);
        } else {
            out.push('=');
        }
        if chunk.len() > 2 {
            out.push(TABLE[(triple & 0x3F) as usize] as char);
        } else {
            out.push('=');
        }
    }
    out
}

fn base64_url_encode(bytes: &[u8]) -> String {
    base64_encode_standard(bytes)
        .replace('+', "-")
        .replace('/', "_")
        .trim_end_matches('=')
        .to_string()
}

pub fn url_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

fn url_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(hex) = std::str::from_utf8(&bytes[i + 1..i + 3])
                && let Ok(val) = u8::from_str_radix(hex, 16)
            {
                out.push(val);
                i += 3;
                continue;
            }
        } else if bytes[i] == b'+' {
            out.push(b' ');
            i += 1;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[derive(Clone, Debug)]
pub struct PkceRequest {
    pub client_id: String,
    pub verifier: String,
    pub state: String,
    pub auth_url: String,
}

/// Generates a fresh OAuth 2.0 PKCE verifier, state, and Spotify authorization URL.
pub fn prepare_pkce_request(client_id: &str) -> Result<PkceRequest, String> {
    let client_id = client_id.trim();
    if client_id.is_empty() {
        return Err("Please enter your Spotify Developer Client ID first.".into());
    }
    if client_id.len() < 16 || !client_id.chars().all(|c| c.is_ascii_alphanumeric()) {
        return Err("Client ID must be a valid alphanumeric string from Spotify Developer Dashboard.".into());
    }

    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let pid = std::process::id();
    let seed = format!("swipelist-pkce-{nanos}-{pid}");
    let h1 = sha256(seed.as_bytes());
    let h2 = sha256(&h1);
    let mut combined = [0u8; 48];
    combined[..32].copy_from_slice(&h1);
    combined[32..].copy_from_slice(&h2[..16]);
    let verifier = base64_url_encode(&combined);
    let state = base64_url_encode(&h2[16..28]);
    let challenge = base64_url_encode(&sha256(verifier.as_bytes()));

    let scopes = "playlist-read-private playlist-read-collaborative playlist-modify-private playlist-modify-public user-library-read user-read-private ugc-image-upload";

    let auth_url = format!(
        "https://accounts.spotify.com/authorize?response_type=code&client_id={}&scope={}&redirect_uri={}&state={}&code_challenge_method=S256&code_challenge={}",
        url_encode(client_id),
        url_encode(scopes),
        url_encode(REDIRECT_URI),
        url_encode(&state),
        url_encode(&challenge),
    );

    Ok(PkceRequest {
        client_id: client_id.to_string(),
        verifier,
        state,
        auth_url,
    })
}

pub fn cancel_pending_oauth_listener() {
    if let Ok(mut stream) = TcpStream::connect_timeout(
        &"127.0.0.1:8888".parse().unwrap(),
        Duration::from_millis(120),
    ) {
        let _ = stream.write_all(b"GET /cancel HTTP/1.1\r\nHost: 127.0.0.1:8888\r\n\r\n");
        let _ = stream.flush();
    }
}

fn run_curl(args: &[&str], body: Option<&[u8]>) -> Result<String, String> {
    let mut cmd = Command::new("curl");
    cmd.args(["-s", "-S"]);
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        cmd.arg("--ssl-no-revoke");
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    cmd.args(args);

    if body.is_some() {
        cmd.arg("--data-binary").arg("@-");
        cmd.stdin(Stdio::piped());
    } else {
        cmd.stdin(Stdio::null());
    }
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());

    let mut child = cmd
        .spawn()
        .map_err(|e| format!("Failed to run curl: {e}"))?;

    if let Some(bytes) = body
        && let Some(mut stdin) = child.stdin.take()
    {
        let _ = stdin.write_all(bytes);
    }

    let output = child
        .wait_with_output()
        .map_err(|e| format!("Network request failed: {e}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("Network request failed: {}", stderr.trim()));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

pub struct SpotifyAuthSession {
    pub user: UserProfile,
    pub playlists: Vec<PlaylistSummary>,
    pub access_token: String,
}

/// Minimalist Spotify-style OAuth callback response page (flat `#121212` surface, official Spotify glyph,
/// no card box, auto-closes tab when allowed by the browser).
const MINIMAL_CALLBACK_HTML: &str = "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nConnection: close\r\n\r\n<!DOCTYPE html><html lang='en'><head><meta charset='utf-8'><title>Spotify</title><style>*{box-sizing:border-box;margin:0;padding:0}body{background:#121212;color:#fff;font-family:-apple-system,BlinkMacSystemFont,'Segoe UI',Roboto,Helvetica,Arial,sans-serif;display:flex;flex-direction:column;align-items:center;justify-content:center;height:100vh;gap:14px;-webkit-font-smoothing:antialiased}svg{width:36px;height:36px;fill:#fff}h1{font-size:16px;font-weight:600;letter-spacing:-0.01em}p{color:#a7a7a7;font-size:13px}</style></head><body><svg viewBox='0 0 24 24' aria-hidden='true'><path d='M12 0C5.4 0 0 5.4 0 12s5.4 12 12 12 12-5.4 12-12S18.66 0 12 0zm5.521 17.34c-.24.359-.66.48-1.021.24-2.82-1.74-6.36-2.101-10.561-1.141-.418.122-.779-.179-.899-.539-.12-.421.18-.78.54-.9 4.56-1.021 8.52-.6 11.64 1.32.42.18.479.659.301 1.02zm1.44-3.3c-.301.42-.841.6-1.262.3-3.239-1.98-8.159-2.58-11.939-1.38-.479.12-1.02-.12-1.14-.6-.12-.48.12-1.021.6-1.141C9.6 9.9 15 10.561 18.72 12.84c.361.181.54.78.241 1.2zm.12-3.36C15.24 8.4 8.82 8.16 5.16 9.301c-.6.179-1.2-.181-1.38-.721-.18-.601.18-1.2.72-1.381 4.26-1.26 11.28-1.02 15.721 1.621.539.3.719 1.02.419 1.56-.299.421-1.02.599-1.559.3z'/></svg><h1>Logged in to Spotify</h1><p>You can close this window.</p><script>setTimeout(function(){window.close()},400);</script></body></html>";

pub fn await_callback_and_fetch_library(req: &PkceRequest) -> Result<SpotifyAuthSession, String> {
    cancel_pending_oauth_listener();
    std::thread::sleep(Duration::from_millis(60));

    let listener = TcpListener::bind(CALLBACK_ADDR).map_err(|e| {
        format!("Could not bind {CALLBACK_ADDR} for Spotify callback: {e}. You can also paste the callback URL below.")
    })?;

    for incoming in listener.incoming() {
        let mut stream = incoming.map_err(|e| format!("OAuth callback socket error: {e}"))?;
        let _ = stream.set_read_timeout(Some(Duration::from_secs(10)));

        let mut buf = [0u8; 4096];
        let n = stream.read(&mut buf).unwrap_or(0);
        if n == 0 {
            continue;
        }
        let request = String::from_utf8_lossy(&buf[..n]);
        let first_line = request.lines().next().unwrap_or("");
        let path_query = first_line.split_whitespace().nth(1).unwrap_or("");

        if path_query.starts_with("/cancel") {
            let _ = stream.write_all(b"HTTP/1.1 200 OK\r\nConnection: close\r\n\r\nCancelled");
            return Err("OAuth login cancelled.".into());
        }
        if !path_query.starts_with("/callback") {
            let _ = stream.write_all(b"HTTP/1.1 404 Not Found\r\nConnection: close\r\n\r\n");
            continue;
        }

        let _ = stream.write_all(MINIMAL_CALLBACK_HTML.as_bytes());
        let _ = stream.flush();

        return exchange_callback_query(req, path_query);
    }

    Err("OAuth listener closed before callback arrived.".into())
}

pub fn exchange_callback_query(
    req: &PkceRequest,
    url_or_path: &str,
) -> Result<SpotifyAuthSession, String> {
    let query = url_or_path
        .split_once('?')
        .map(|(_, q)| q)
        .unwrap_or(url_or_path);
    let query = query.split('#').next().unwrap_or(query);

    let mut code = None;
    let mut returned_state = None;
    for pair in query.split('&') {
        if let Some((k, v)) = pair.split_once('=') {
            let v_decoded = url_decode(v.trim());
            match k.trim() {
                "code" => code = Some(v_decoded),
                "state" => returned_state = Some(v_decoded),
                "error" => return Err(format!("Spotify authorization declined: {v_decoded}")),
                _ => {}
            }
        }
    }

    if let Some(st) = &returned_state
        && st != &req.state
    {
        return Err("OAuth security state mismatch. Please click Authorize with Spotify again.".into());
    }

    let code = code.ok_or_else(|| {
        "No authorization code found in callback URL. Make sure http://127.0.0.1:8888/callback is added to your Spotify App's Redirect URIs.".to_string()
    })?;

    let form = format!(
        "client_id={}&grant_type=authorization_code&code={}&redirect_uri={}&code_verifier={}",
        url_encode(&req.client_id),
        url_encode(&code),
        url_encode(REDIRECT_URI),
        url_encode(&req.verifier)
    );

    let token_raw = run_curl(
        &[
            "-X",
            "POST",
            "https://accounts.spotify.com/api/token",
            "-H",
            "Content-Type: application/x-www-form-urlencoded",
        ],
        Some(form.as_bytes()),
    )?;

    let token_json: serde_json::Value = serde_json::from_str(&token_raw)
        .map_err(|_| format!("Unexpected response from Spotify token endpoint: {token_raw}"))?;

    if let Some(err_desc) = token_json["error_description"].as_str() {
        return Err(format!("Spotify token error: {err_desc}"));
    }

    let access_token = token_json["access_token"]
        .as_str()
        .ok_or_else(|| format!("Spotify token exchange failed: {token_raw}"))?
        .to_string();
    let refresh_token = token_json["refresh_token"].as_str().map(String::from);

    save_config(&StoredConfig {
        client_id: req.client_id.clone(),
        refresh_token,
        access_token: Some(access_token.clone()),
    });

    let (user, playlists) = fetch_profile_and_playlists(&access_token)?;
    Ok(SpotifyAuthSession {
        user,
        playlists,
        access_token,
    })
}

pub fn restore_saved_session() -> Result<SpotifyAuthSession, String> {
    let cfg = load_config();
    if cfg.client_id.trim().is_empty() {
        return Err("No saved Client ID".into());
    }

    if let Some(token) = &cfg.access_token
        && !token.is_empty()
        && let Ok((user, playlists)) = fetch_profile_and_playlists(token)
    {
        return Ok(SpotifyAuthSession {
            user,
            playlists,
            access_token: token.clone(),
        });
    }

    let refresh = cfg
        .refresh_token
        .as_deref()
        .filter(|s| !s.is_empty())
        .ok_or_else(|| "No saved refresh token".to_string())?;

    let form = format!(
        "client_id={}&grant_type=refresh_token&refresh_token={}",
        url_encode(cfg.client_id.trim()),
        url_encode(refresh),
    );

    let token_raw = run_curl(
        &[
            "-X",
            "POST",
            "https://accounts.spotify.com/api/token",
            "-H",
            "Content-Type: application/x-www-form-urlencoded",
        ],
        Some(form.as_bytes()),
    )?;

    let token_json: serde_json::Value =
        serde_json::from_str(&token_raw).map_err(|_| "Invalid refresh token JSON".to_string())?;

    let access_token = token_json["access_token"]
        .as_str()
        .ok_or_else(|| "Refresh token expired".to_string())?
        .to_string();
    let new_refresh = token_json["refresh_token"]
        .as_str()
        .map(String::from)
        .or(cfg.refresh_token);

    save_config(&StoredConfig {
        client_id: cfg.client_id,
        refresh_token: new_refresh,
        access_token: Some(access_token.clone()),
    });

    let (user, playlists) = fetch_profile_and_playlists(&access_token)?;
    Ok(SpotifyAuthSession {
        user,
        playlists,
        access_token,
    })
}

pub fn fetch_profile_and_playlists(
    access_token: &str,
) -> Result<(UserProfile, Vec<PlaylistSummary>), String> {
    let auth_header = format!("Authorization: Bearer {access_token}");
    let me_raw = run_curl(&["https://api.spotify.com/v1/me", "-H", &auth_header], None)?;
    let me: serde_json::Value =
        serde_json::from_str(&me_raw).map_err(|_| format!("Invalid profile JSON: {me_raw}"))?;

    if let Some(err) = me.get("error") {
        let msg = err["message"].as_str().unwrap_or("Unauthorized");
        return Err(format!("Spotify API error: {msg}"));
    }

    let user = UserProfile {
        id: me["id"].as_str().unwrap_or("spotify-user").to_string(),
        display_name: me["display_name"]
            .as_str()
            .or_else(|| me["id"].as_str())
            .unwrap_or("Spotify User")
            .to_string(),
        product: me["product"].as_str().unwrap_or("premium").to_uppercase(),
        image_url: me["images"]
            .as_array()
            .and_then(|arr| arr.first())
            .and_then(|img| img["url"].as_str())
            .map(String::from),
    };

    let mut list = Vec::new();

    if let Ok(liked_raw) = run_curl(
        &[
            "https://api.spotify.com/v1/me/tracks?limit=1",
            "-H",
            &auth_header,
        ],
        None,
    ) && let Ok(liked_val) = serde_json::from_str::<serde_json::Value>(&liked_raw)
        && let Some(total) = liked_val["total"].as_u64()
    {
        list.push(PlaylistSummary {
            id: "liked-songs".into(),
            name: "Liked Songs".into(),
            owner_name: user.display_name.clone(),
            track_count: total as usize,
            image_url: None,
            is_liked_songs: true,
            accent_hue: 152.0,
            subtitle: "Pinned • Saved in your Spotify library".into(),
        });
    }

    // Paginate through all user playlists
    let mut next_url: Option<String> =
        Some("https://api.spotify.com/v1/me/playlists?limit=50".to_string());
    let mut idx = 0usize;
    while let Some(url) = next_url.take() {
        let Ok(pl_raw) = run_curl(&[&url, "-H", &auth_header], None) else {
            break;
        };
        let Ok(pl_val) = serde_json::from_str::<serde_json::Value>(&pl_raw) else {
            break;
        };
        if let Some(items) = pl_val["items"].as_array() {
            for item in items {
                let Some(id) = item["id"].as_str() else { continue };
                let name = item["name"].as_str().unwrap_or("Untitled Playlist").to_string();
                let count = item["items"]["total"]
                    .as_u64()
                    .or_else(|| item["tracks"]["total"].as_u64())
                    .unwrap_or(0) as usize;
                let owner = item["owner"]["display_name"]
                    .as_str()
                    .or_else(|| item["owner"]["id"].as_str())
                    .unwrap_or("Spotify")
                    .to_string();
                let image_url = item["images"]
                    .as_array()
                    .and_then(|a| a.first())
                    .and_then(|img| img["url"].as_str())
                    .map(String::from);
                let hue = ((idx * 47 + 140) % 360) as f32;
                idx += 1;
                list.push(PlaylistSummary {
                    id: id.to_string(),
                    name,
                    owner_name: owner.clone(),
                    track_count: count,
                    image_url,
                    is_liked_songs: false,
                    accent_hue: hue,
                    subtitle: format!("By {owner} • {count} songs"),
                });
            }
        }
        next_url = pl_val["next"].as_str().map(String::from);
    }

    if list.is_empty() {
        list = starter_playlists();
    }

    Ok((user, list))
}

/// Fetches ALL tracks in a playlist or Liked Songs by following Spotify's `next` pagination URLs.
pub fn fetch_playlist_tracks(access_token: &str, playlist_id: &str) -> Result<Vec<Track>, String> {
    let auth_header = format!("Authorization: Bearer {access_token}");
    let first_url = if playlist_id == "liked-songs" {
        "https://api.spotify.com/v1/me/tracks?limit=50".to_string()
    } else {
        format!("https://api.spotify.com/v1/playlists/{playlist_id}/items?limit=100")
    };

    let mut tracks = Vec::new();
    let mut seen = std::collections::HashSet::new();
    let mut next_url: Option<String> = Some(first_url);
    let mut tried_fallback = false;

    while let Some(url) = next_url.take() {
        let raw = run_curl(&[&url, "-H", &auth_header], None)?;
        let mut val: serde_json::Value =
            serde_json::from_str(&raw).map_err(|_| "Failed to parse tracks JSON".to_string())?;

        if val.get("error").is_some() && playlist_id != "liked-songs" && !tried_fallback {
            tried_fallback = true;
            let fallback =
                format!("https://api.spotify.com/v1/playlists/{playlist_id}/tracks?limit=100");
            let raw2 = run_curl(&[&fallback, "-H", &auth_header], None)?;
            val = serde_json::from_str(&raw2)
                .map_err(|_| "Failed to parse tracks JSON".to_string())?;
        }

        if let Some(items) = val["items"].as_array() {
            for entry in items {
                let t = entry
                    .get("item")
                    .or_else(|| entry.get("track"))
                    .unwrap_or(entry);
                if t["is_local"].as_bool() == Some(true) {
                    continue;
                }
                let Some(id) = t["id"].as_str() else { continue };
                if !seen.insert(id.to_string()) {
                    continue;
                }
                let idx = tracks.len();
                let name = t["name"].as_str().unwrap_or("Unknown Track").to_string();
                let uri = t["uri"]
                    .as_str()
                    .map(String::from)
                    .unwrap_or_else(|| format!("spotify:track:{id}"));
                let artists = t["artists"]
                    .as_array()
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|a| a["name"].as_str())
                            .collect::<Vec<_>>()
                            .join(", ")
                    })
                    .unwrap_or_else(|| "Unknown Artist".into());
                let album = t["album"]["name"].as_str().unwrap_or("Single").to_string();
                let year = t["album"]["release_date"]
                    .as_str()
                    .map(|d| d.chars().take(4).collect())
                    .unwrap_or_else(|| "2024".into());
                let duration_ms = t["duration_ms"].as_u64().unwrap_or(210_000) as u32;
                let image_url = t["album"]["images"]
                    .as_array()
                    .and_then(|a| a.first())
                    .and_then(|img| img["url"].as_str())
                    .map(String::from);
                let preview_url = t["preview_url"].as_str().map(String::from);

                tracks.push(Track {
                    id: id.to_string(),
                    uri,
                    name,
                    artists,
                    album,
                    year,
                    genre_tag: "Spotify Track".into(),
                    duration_ms,
                    image_url,
                    preview_url,
                    accent_hue: ((idx * 53 + 150) % 360) as f32,
                });
            }
        }

        next_url = val["next"].as_str().map(String::from);
    }

    Ok(tracks)
}

pub fn save_selection_to_spotify(
    access_token: &str,
    name: &str,
    description: &str,
    is_public: bool,
    tracks: &[Track],
    custom_cover_path: Option<&Path>,
) -> Result<String, String> {
    if access_token.is_empty() {
        return Ok("https://open.spotify.com".into());
    }

    let auth_header = format!("Authorization: Bearer {access_token}");
    let create_body = serde_json::json!({
        "name": name,
        "description": description,
        "public": is_public,
    })
    .to_string();

    let created_raw = run_curl(
        &[
            "-X",
            "POST",
            "https://api.spotify.com/v1/me/playlists",
            "-H",
            &auth_header,
            "-H",
            "Content-Type: application/json",
        ],
        Some(create_body.as_bytes()),
    )?;

    let created: serde_json::Value = serde_json::from_str(&created_raw)
        .map_err(|_| format!("Failed to create playlist: {created_raw}"))?;
    let playlist_id = created["id"]
        .as_str()
        .ok_or_else(|| format!("Spotify error creating playlist: {created_raw}"))?;
    let playlist_url = created["external_urls"]["spotify"]
        .as_str()
        .map(String::from)
        .unwrap_or_else(|| format!("https://open.spotify.com/playlist/{playlist_id}"));

    let uris: Vec<&str> = tracks.iter().map(|t| t.uri.as_str()).collect();
    for batch in uris.chunks(100) {
        let add_body = serde_json::json!({ "uris": batch }).to_string();
        let items_url = format!("https://api.spotify.com/v1/playlists/{playlist_id}/items");
        let res = run_curl(
            &[
                "-X",
                "POST",
                &items_url,
                "-H",
                &auth_header,
                "-H",
                "Content-Type: application/json",
            ],
            Some(add_body.as_bytes()),
        );
        if res.is_err() {
            let fallback_url = format!("https://api.spotify.com/v1/playlists/{playlist_id}/tracks");
            let _ = run_curl(
                &[
                    "-X",
                    "POST",
                    &fallback_url,
                    "-H",
                    &auth_header,
                    "-H",
                    "Content-Type: application/json",
                ],
                Some(add_body.as_bytes()),
            );
        }
    }

    if let Some(cover_path) = custom_cover_path
        && let Ok(bytes) = std::fs::read(cover_path)
    {
        let b64 = base64_encode_standard(&bytes);
        if b64.len() <= 256_000 {
            let img_url = format!("https://api.spotify.com/v1/playlists/{playlist_id}/images");
            let _ = run_curl(
                &[
                    "-X",
                    "PUT",
                    &img_url,
                    "-H",
                    &auth_header,
                    "-H",
                    "Content-Type: image/jpeg",
                ],
                Some(b64.as_bytes()),
            );
        }
    }

    Ok(playlist_url)
}
