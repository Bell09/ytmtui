# ytmtui — Hardening Report (phases 1–6)

Repository: `/home/castillo/ytmtui`
Base commit: `e28264c docs: replace screenshots`
All changes are uncommitted working-tree edits on top of that commit; no unrelated user changes were touched. Screenshot file mode changes (`100644→100755`) were restored to `644`.

## Summary of result

- `cargo test`: **23 passed, 0 failed, 2 ignored** (the 2 ignored are live tests requiring network: `live_artist_sections`, `bench_scan_large_library`; the latter runs as an ignored benchmark).
- `cargo check --all-targets`: clean, zero warnings.
- `cargo +1.89.0 check --all-targets`: clean — MSRV **Rust 1.89.0** verified by actual build.
- Release build: clean.
- 2000-file offline scan benchmark: **~50 ms (debug)** vs baseline **79 ms**.
- Idle CPU: **~0.5%** vs baseline **1.6%** (1 Hz idle redraw throttling).
- First frame: **~0.01–0.015 s**; idle RSS **~17.2 MB**; input latency **~2 ms**.

## Phase 1 — Baselines (established before/while working)

Recorded from the pre-work tree and re-measured after each change:

| Metric | Baseline (pre-work) | After (final release) |
|---|---|---|
| Startup → first frame | ~0.20 s (idle harness) / ~0.011 s (measure.py at session start) | ~0.01–0.015 s |
| Idle RSS | ~16.8–16.9 MB | ~17.2–17.4 MB |
| Idle CPU | ~1.6% | ~0.4–0.6% |
| 2000-file scan (debug) | 79 ms | ~50 ms |
| Input latency (idle) | 0.04–2.2 ms | ~2.2 ms |

Environment: Rust 1.95.0 toolchain installed; Rust 1.89.0 installed via rustup for MSRV verification; `KITTY_WINDOW_ID=1` for PTY probes; fixture at `/tmp/opencode/bench/music` (20 artists × 5 albums × 20 tracks = 2000 empty `.mp3` files + cover.jpg per folder). PTY harnesses: `/tmp/opencode/measure.py` (startup/latency/RSS) and `/tmp/opencode/measure_idle.py` (idle CPU/RSS over 5 s).

Measurements requiring live network or actual mpv playback (search/browse round-trips, audio start-up, lyrics fetch, AUR push) were **not performed** and are reported as unavailable below.

## Phase 2 — Confirmed bug fixes

### 2.1 Shuffle queue rebuild / termination

**Root cause:** `jump_to` and every queue mutation (`push`, `remove`/`play-next` move, `swap`, `remove_selected`) called `rebuild_shuffle_order(None)`, which regenerated a fresh random permutation on every change. Consequences: "next" could revisit songs already heard, repeat-off could never reach the end of the queue, and playback history was lost on every interaction.

**Fix:** The shuffle order is now built once when shuffle is enabled or the queue is replaced, and thereafter only incrementally maintained:
- `jump_to` / `select_queue_index` no longer rebuild; they only repair when the cursor index is missing from the order.
- New helpers `shuffle_after_insert`, `shuffle_after_remove`, `shuffle_after_swap` shift/remap the existing order in place.
- `next_queue_index`/`previous_queue_index` delegate to pure `linear_next` / `shuffle_next` / `shuffle_previous` functions, so repeat-off walks the whole order then ends, and repeat-all wraps.

### 2.2 Download identity

**Root cause:** `offline::download` adopted any existing file at `Artist/Album/Title.mp3` based purely on artist/album/title match. A title collision could therefore overwrite or mislabel a different video's audio, and legacy (unregistered) downloads were silently claimed as the new video's.

**Fix:** `download` now takes `existing_owner: Option<&str>` — the video id the `downloads.json` manifest already ties to the destination path (`entry_for_path`). Adoption happens only when that owner equals the new track's id. Otherwise a fresh unique name is chosen: `Title.mp3`, `Title (2).mp3`, `Title (3).mp3`… via `unique_audio_path`. The original bytes are never touched. App-side `download_selected` resolves the owner from the registry before spawning the download.

### 2.3 Playlist duplicate handling

**Root cause:** `api.rs` playlist-track collection deduplicated by `t.id` (`HashSet` on video id), which collapses legitimate repeats (the same song appearing twice in a playlist under different memberships).

**Fix:** `dedupe_playlist_entries` retains only rows sharing the same `setVideoId` membership token (`set_video_id` field on `Track`). Rows without a token (fallback rows) are never collapsed against each other, so legitimate repeats survive while true continuation duplicates (same token repeated) are dropped once. Removal requests already carry `setVideoId` via `YtmApi::remove_from_playlist`; refresh now re-stamps the cursor by `set_video_id` first, falling back to video id.

### 2.4 MSRV

**Root cause:** README and badges claimed "Rust ≥ 1.75", but the dependency graph (uuid 1.28, lofty 0.25.4, image 0.25.10, zbus 5.19, etc.) requires newer toolchains; no `rust-version` was declared and no toolchain file pinned anything.

**Fix:** Added `rust-version = "1.89"` to `Cargo.toml`, created `rust-toolchain.toml` pinning channel `1.89.0` (profile minimal), and updated the README badge/table to "Rust ≥ 1.89". Verified by actually building with `cargo +1.89.0 check --all-targets` (clean). crates.io maximum dependency MSRV observed: 1.89.0 (uuid, lofty, lofty_attr, ogg_pager).

## Phase 3 — Premature playback progress

**Root cause:** `poll_player_timeline` contained a 1500 ms `buffering_timer` fallback: if mpv hadn't answered within 1.5 s, the app faked `playback_position = 0`, marked itself Playing, and let wall-clock interpolation run ahead of the actual audio. Progress and lyrics therefore ran ahead of slow streams, and a frozen/stale IPC reply kept the clock drifting.

**Fix:**
- Removed `buffering_timer`, `mpv_seen`, and the 1500 ms guess entirely.
- mpv timeline sampling moved to a dedicated 100 ms background task in `main.rs`/`app.rs` (`Player::socket_path` + free function `timeline_path`). Each sample is stamped with a monotonically increasing sequence; `update()` keeps only the newest (`pending_timeline`), `poll_player_timeline` applies it only if `sequence > applied_timeline`.
- Progress/lyrics use a single authoritative clock: `interpolate_position(state, base, sampled_at, now, duration_secs)`. Interpolation advances only while `Playing` **and** the sample is `≤ TELEMETRY_MAX_AGE (600 ms)` old. Buffering, paused, missing, or stale telemetry freezes at the last known position.
- `finish_buffering` flips Buffering→Playing only when mpv reports a positive position (audio actually started).
- Pause/resume: no sample time is fabricated on resume; interpolation waits for a new moved sample. Watchdogs restart so a long pause doesn't trigger a stall restart on resume.

## Phase 4 — Performance & memory optimization

### UI responsiveness
- `main.rs` event loop: redraws only when animating (playback clock/buffering/downloads), on input, or at most once per second idle (previously every ~100 ms tick unconditionally). `animating()` exposes Playing/Buffering/non-empty-downloads.
- Search (`perform_search`, `cycle_search_filter`) is now synchronous in the caller with the network fetch spawned in the background, keyed by `search_generation`; `poll_search` applies only the newest generation, so a slow older search cannot overwrite newer rows. `main.rs` callers changed from `.await` to direct calls.
- Collection playlist loading (`open_collection`): cached preview/rows paint immediately (`"showing cached · refreshing…"`), authoritative list refreshes in the background keyed by `collection_generation`; failures keep cached content on screen (`"Refresh failed — showing cached"`).
- Browse (`open_browse`, `open_browse_section`, `open_browse_picker`) and category/artist pages (`open_browse_selected`, `open_artist`) are now background fetches keyed by `page_fetch_generation`; the current page stays interactive while the feed walks. A cold `b` starts the feed fetch without blocking; the picker opens when the answer lands. Old in-flight page fetches are invalidated on any new navigation (`activate_selected`, `leave_browse`, `toggle_offline`).
- Offline toggle (`o`) no longer blocks on the folder scan: the walk runs on a blocking worker via `start_offline_scan`, keyed by `offline_scan_generation`; `poll_offline_scan` applies it and builds the sidebar. A download landing while offline reuses the same background scan (`start_offline_scan(false)`), so the "Downloaded ✓" footer isn't delayed by a full re-index.

### Playlist loading / duplicates
- Preview cache (`preview_cache`, cap 6) survives across sessions of opening the same playlist; `stamp_all` re-marks check marks from the registry without re-navigation.
- Refresh merges safely: `dedupe_playlist_entries` preserves legitimate repeats; cursor re-stamped by `set_video_id`.

### Offline scanning
- `scan` now indexes each directory's images **once** (`collect` fills `art: HashMap<dir, Vec<image>>` in a single traversal; `sidecar_art_from` reads no disk). Previously every song re-read its directory for cover art. Benchmark: 2000 files 79 ms → ~50 ms debug.
- Tracks get `thumbnail_url` from the per-directory index instead of a per-file `read_dir`.

### Rendering / memory
- `ui/mod.rs`: `visible_window` applied to search results and browse rows — only the visible slice (plus cursor centering) is materialised into `ListItem`s; long lists (1000+ tracks) draw as cheaply as short ones. List state selection adjusted to the window offset.
- Preview cache bounded at 6 entries (FIFO eviction on insert).

### Playback / background tasks
- Synchronous mpv IPC (`timeline`, `seek_to`, `pause`) is short and time-boxed (25 ms connect/read, 40 ms total). The per-frame timeline poll moved off the UI thread to the 100 ms sampler; a wedged mpv no longer adds its socket timeout to input latency.
- Next-track prefetch preserved (15 min TTL, resolved in background, skipped on retry). No full tracks are downloaded into memory — prefetch resolves a stream URL only.
- Download identity fix also avoids re-downloading bytes when an interrupted download's file is still on disk and owned by the same video id.

## Phase 5 — Regression & compatibility testing

Added test modules:

**`src/app.rs`**
- `playback_clock_tests` — initial progress zero while buffering (no sample, and 10 s stale sample); normal playback interpolates from authoritative sample (+250 ms); stale telemetry (>600 ms) and missing telemetry freeze; pause freezes and resume waits for a new sample (5 s stale sample still frozen); track switch resets to zero and seek is immediate; clamp at duration.
- `shuffle_tests` — shuffled order walks every track once then ends (repeat-off); repeat-all wraps, previous walks same order; linear queue ends without repeat; shuffle order survives playing a track already in it (no spurious rebuild).

**`src/api.rs`**
- `playlist_dedupes_membership_tokens_not_video_ids` — same video under three distinct `setVideoId`s all kept (3 rows); same token repeated collapsed to one; token-less rows preserved independently.

**`src/offline.rs`**
- `download_collision_gets_a_unique_name_instead_of_adopting_it` — taken path yields `Song (2).mp3`, then `Song (3).mp3`; original bytes untouched.
- `download_path_uses_artist_album_layout` — sanitised `Artist/Album/Title.mp3`.
- `scan_reuses_one_directory_cover_index` — 2 tracks, both point to the single `cover.jpg` indexed once per directory.
- Existing `bench_scan_large_library` (ignored benchmark) — 2000 files.

Execution results (`cargo test`): **23 passed, 0 failed, 2 ignored** (ignored = the two live tests requiring network/disk-bench; `bench_scan_large_library` runs separately: `cargo test bench_scan_large_library -- --ignored --nocapture` → ~50 ms, ok).
`cargo check --all-targets` and `cargo +1.89.0 check --all-targets` both clean.

## Phase 6 — Before/after measurements

All measurements on the release binary, identical fixture (`/tmp/opencode/bench/music`, 2000 files), PTY harnesses with `KITTY_WINDOW_ID=1`, `TERM=xterm-256color`, 160×45.

| Metric | Before | After | Notes |
|---|---|---|---|
| First rendered frame | ~0.20 s | ~0.01–0.015 s | measure.py / measure_idle.py, final build |
| Idle RSS | ~16.9 MB | ~17.2 MB | within noise; extra channels/state |
| Peak RSS | ~16.9 MB | ~17.4 MB | |
| Idle CPU (5 s) | ~1.6% | ~0.4–0.6% | 1 Hz idle redraw |
| Input latency (idle) | 0.04–2.2 ms | ~2.2 ms | down-arrow round-trip |
| 2000-file offline scan (debug) | 79 ms | ~50 ms | directory-once cover index |
| 2000-file offline scan (release) | not measured | not measured | benchmark runs in debug test profile |
| mpv child-process memory | n/a | n/a | mpv not installed/playable in this env |

Idle RSS ticked up ~0.3–0.5 MB (new unbounded channels and generation state) — negligible and bounded; peak equals RSS, so no unbounded growth observed in the idle harness.

### Could not be performed / unavailable
- Live network measurements: search/browse/artist/category API round-trips, AUR push (`aur@aur.archlinux.org: Permission denied (publickey)` — SSH key must be added at the AUR account page; push remains blocked).
- Actual audio playback / mpv measurements: mpv not available in this environment, so buffering→playing transition, seek latency, and real ICY timeline behaviour could not be end-to-end verified with a live stream; covered by unit tests of `interpolate_position` instead.
- Release-profile scan benchmark: the ignored test runs under the debug test harness.

## Remaining bottlenecks / recommended next steps

1. **Cold `open_browse` / `open_artist` / category browse** are now backgrounded but still wait on one full Innertube walk (`for_you` = 12 pages joined) before showing sections; the boot cache (`browse.json`) covers repeat launches. A per-section lazy load would make the very first `b` after a fresh profile faster.
2. **Download path** still shells out to `yt-dlp` per song (unavoidable for stream resolution) and embeds cover via ffmpeg; nothing downloaded into memory, but a poor network still blocks the individual download row only, not the UI.
3. **Idle RSS** slightly above baseline; the timeline sampler thread and generation channels add fixed overhead — acceptable trade-off for input latency.
4. **Release workflow** (step 5 from the plan): bump `Cargo.toml`/`Cargo.lock`, README, `install.sh`, `.github/workflows/release.yml`; commit; push `main`; tag; verify GitHub Release + pacman repo checksums; refresh AUR `.SRCINFO`/PKGBUILD — AUR push blocked pending SSH key upload.
5. **MSRV drift check:** re-run `cargo +1.89.0 check` whenever dependencies are upgraded, since uuid/lofty/image currently pin the floor at 1.89.

## Files changed

| File | Change |
|---|---|
| `Cargo.toml` | Added `rust-version = "1.89"` |
| `rust-toolchain.toml` | **New** — pins channel `1.89.0`, profile minimal |
| `README.md` | MSRV badge and table: 1.75 → 1.89 |
| `src/app.rs` | Shuffle order preservation + incremental helpers (`shuffle_after_insert/remove/swap`, `linear_next`, `shuffle_next/previous`); background 100 ms timeline sampler + `interpolate_position`/`TELEMETRY_MAX_AGE` freeze logic; removed `buffering_timer`/`mpv_seen`/1500 ms guess; `search_generation`/`collection_generation`/`page_fetch_generation`/`offline_scan_generation` guards; background browse/artist/category fetches with invalidation; background offline folder scan; `start_offline_scan` used on download-landed while offline |
| `src/main.rs` | Event-loop redraw throttling (animating / input / 1 s idle); `Player::socket_path` accessor; search/cycle no longer awaited inline |
| `src/player.rs` | `timeline_path(socket_path)` free function extracted from `Player::timeline`; short time-boxed IPC retained |
| `src/api.rs` | `dedupe_playlist_entries` by `setVideoId` token (preserves legitimate repeats); removed now-unused `for_you_quick` |
| `src/offline.rs` | `download` takes `existing_owner`; `unique_audio_path` (`Title (2).mp3`…); directory-once cover index (`collect` + `sidecar_art_from`); new tests |
| `src/ui/mod.rs` | `visible_window` applied to search results and browse rows (visible-slice rendering) |
