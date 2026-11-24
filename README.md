# Swipelist

A desktop playlist curation app for Spotify built in **Rust** with [**Rune**](https://github.com/anomalyco/rune) (`wgpu`).

Swipelist uses a Tinder-style two-pane layout: pick a **Source Playlist**, swipe right (`ADD`) to include tracks in your **New Playlist**, swipe left (`SKIP`) to pass, and click **Save to Spotify (N)** to create the playlist in your Spotify library.

---

## Navigation Flow: Source Playlist → Swipe → New Playlist

### 1. Choose a Source Playlist (`Source Picker`)
* The top of the left sidebar shows the active **Source Playlist** and track count.
* Click **Change Source ▾** to open the **Source Picker** on the center stage, where you can search and filter across *Liked Songs* and your playlists.
* Click any playlist tile to load its tracks into the swipe deck.
* Use the **In Order / Shuffled** toggle above the card deck to control playback order.

### 2. Swipe Tracks (`ADD` / `SKIP`)
* **Album Cover Art & Interactive Audio Player**: Each card displays the song's album cover art and an embedded audio player bar with Play/Pause (`▶` / `⏸` or `Space`) and a draggable scrubber slider so you can seek to any point in the preview.
* **Full Playlist Pagination**: Loads all tracks in the selected **Source Playlist** across all pages (`Card 1 of <Total>`).
* **Swipe Right (`ADD` / `→`)**: Adds the current card to **Selected Songs** in the left sidebar with smooth card drag animation.
* **Swipe Left (`SKIP` / `←`)**: Excludes the track and advances to the next card.
* **Undo (`↺` / `Z` / `Backspace`)**: Undo returns the last card to the top of the stack and removes it from the selection if it was added.
* **Remove from selection (`✕`)**: Hover over any row in **Selected Songs** in the left sidebar and click `✕` to remove that song without losing your place in the deck.
* **Open in Spotify (`↗` / `O`)**: Opens the current track directly in Spotify.
* **Automatic Progress Saving**: Every swipe, undo, removal, and playlist edit is saved automatically to `swipelist_progress.json` (and `%APPDATA%\swipelist\swipelist_progress.json`), restoring your exact position and selection when you reopen the app or switch playlists.

### 3. Customize & Save (`Save to Spotify (N)`)
* **New Playlist Name**: Edit the name of your **New Playlist** at any time in the left sidebar or on the review screen (defaults to `<Source Playlist> • Selection`).
* **Playlist Cover Art**: Displays a 2×2 mosaic of the first four **Selected Songs** by default. Click the cover tile or **Choose Cover…** to pick a custom `.jpg` or `.png` cover image.
* **Save to Spotify (N)**: Click **Save to Spotify (N)** at the bottom of the left sidebar to review your **Selected Songs**, edit the playlist description or privacy (`Private` / `Public`), and save the playlist to your Spotify account.

---

## Controls & Shortcuts

| Action | Pointer | Keyboard |
|---|---|---|
| **ADD to Selected Songs** | Drag card right or click **ADD** (`♥`) | `→` Right Arrow |
| **SKIP Track** | Drag card left or click **SKIP** (`✕`) | `←` Left Arrow |
| **Play / Pause & Seek** | Click `▶` / `⏸` or drag the player slider on the card | `Space` |
| **Undo** | Click **Undo** (`↺`) | `Z` or `Backspace` |
| **Open in Spotify** | Click **Open in Spotify** (`↗`) | `O` |
| **Remove from selection** | Hover row in **Selected Songs** and click `✕` | — |

---

## Build & Run

### Run Interactive Desktop Window
```powershell
cargo run --release
```

### Headless Rendering & Automated Verification
Rune supports headless GPU rendering for automated visual and layout checks:

```powershell
# Render the main Swipe Deck screen to PNG
cargo run -- --snap swiping.png --scene swiping

# Render active rightward drag (ADD stamp) and leftward drag (SKIP stamp)
cargo run -- --snap drag_add.png --scene drag-add
cargo run -- --snap drag_skip.png --scene drag-skip

# Render the Source Picker and Review screens
cargo run -- --snap source_picker.png --scene source-picker
cargo run -- --snap review.png --scene review

# Dump the accessibility & layout element tree as JSON
cargo run -- --tree --scene swiping
```

---

## Spotify API Configuration

1. Open the [Spotify Developer Dashboard](https://developer.spotify.com/dashboard) and create an application.
2. Under **Redirect URIs**, add:
   ```text
   http://127.0.0.1:8888/callback
   ```
3. In Swipelist, click **Connect Spotify** in the top-left corner, paste your **Client ID**, and click **Authorize with Spotify**.
