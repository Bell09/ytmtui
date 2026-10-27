use crate::auth::Auth;
use crate::models::{BrowseShelf, Playlist, PlaylistKind, SearchHit, Track};
use reqwest::Client;
use serde_json::{json, Map, Value};

const API_KEY: &str = "AIzaSyAO_FJ2SlqU8Q4STEHLGCilw_Y9_11qcW8";
const CLIENT_VERSION: &str = "1.20240101.01.00";
const BROWSE_URL: &str =
    "https://music.youtube.com/youtubei/v1/browse?prettyPrint=false";
const NEXT_URL: &str = "https://music.youtube.com/youtubei/v1/next?prettyPrint=false";
const LIKE_URL: &str = "https://music.youtube.com/youtubei/v1/like/";
const EDIT_URL: &str = "https://music.youtube.com/youtubei/v1/playlist/edit";

/// A search filter chip (e.g. "Albums") and the opaque `params` that selects it.
#[derive(Debug, Clone)]
pub struct SearchFilter {
    pub title: String,
    pub params: String,
}

/// The result of a search: the rows to show and the available filter chips.
#[derive(Debug, Clone, Default)]
pub struct SearchResults {
    pub hits: Vec<SearchHit>,
    pub filters: Vec<SearchFilter>,
}

#[derive(Clone)]
pub struct YtmApi {
    client: Client,
    headers: Option<Map<String, Value>>,
}

impl YtmApi {
    pub fn new() -> Self {
        // Timeouts on purpose: without them a stalled connection hangs
        // whatever awaits it — including the background library fetch.
        let client = Client::builder()
            .connect_timeout(std::time::Duration::from_secs(5))
            .timeout(std::time::Duration::from_secs(20))
            .build()
            .unwrap_or_default();
        Self {
            client,
            headers: Auth::load_headers(),
        }
    }

    pub fn is_authenticated(&self) -> bool {
        self.headers.is_some()
    }

    /// The signed-in account's display name. Uses this client's headers and
    /// connection pool, so the header's account lookup can share the same TLS
    /// session as the library fetch.
    pub async fn current_account_name(&self) -> Option<String> {
        let headers = self.headers.as_ref()?;
        crate::auth::account_name_with(&self.client, headers)
            .await
            .ok()
            .flatten()
    }

    /// Search YouTube Music. `params` selects a filter chip (album/artist/…);
    /// `None` performs the default (mixed) search. Uses the authenticated
    /// Innertube endpoint when signed in, otherwise falls back to the public
    /// search API.
    pub async fn search(
        &self,
        query: &str,
        params: Option<&str>,
    ) -> anyhow::Result<SearchResults> {
        if let Some(ref headers) = self.headers {
            if let Ok(results) = self.search_authenticated(query, params, headers).await {
                if !results.hits.is_empty() || params.is_some() {
                    return Ok(results);
                }
            }
        }
        if params.is_some() {
            anyhow::bail!("not signed in");
        }
        let tracks = self.search_public(query).await?;
        Ok(SearchResults {
            hits: tracks.into_iter().map(SearchHit::Track).collect(),
            filters: Vec::new(),
        })
    }

    async fn search_authenticated(
        &self,
        query: &str,
        params: Option<&str>,
        headers: &Map<String, Value>,
    ) -> anyhow::Result<SearchResults> {
        let url = "https://music.youtube.com/youtubei/v1/search?prettyPrint=false";
        let mut body = json!({
            "context": context(),
            "query": query
        });
        if let Some(params) = params {
            body["params"] = Value::String(params.to_string());
        }

        let mut req = self.client.post(url).json(&body);
        for (k, v) in headers {
            if let Some(s) = v.as_str() {
                req = req.header(k.as_str(), s);
            }
        }

        let resp = req.send().await?;
        let value: Value = resp.json().await?;

        let mut hits = Vec::new();
        collect_search_hits(&value, &mut hits);
        hits.truncate(50);

        let mut filters = Vec::new();
        collect_filters(&value, &mut filters);

        Ok(SearchResults { hits, filters })
    }

    /// The signed-in user's playlists and albums (including "Liked Music").
    pub async fn library_playlists(&self) -> anyhow::Result<Vec<Playlist>> {
        let mut body = json!({ "context": context(), "browseId": "FEmusic_library_landing" });
        let mut value = self.browse(&body).await?;
        let mut playlists = Vec::new();
        let mut seen_tokens = Vec::new();

        loop {
            collect_two_row_playlists(&value, &mut playlists);
            match find_next_continuation(&value) {
                Some(token) if !seen_tokens.contains(&token) => {
                    seen_tokens.push(token.clone());
                    body = json!({ "context": context(), "continuation": token });
                    // A single flaky continuation should not throw away the
                    // shelves already found; keep what we have.
                    match self.browse(&body).await {
                        Ok(next) => value = next,
                        Err(_) => break,
                    }
                }
                _ => break,
            }
        }

        playlists.dedup_by(|a, b| a.id == b.id);
        // The sidebar lists playlists and albums; subscribed artists (♪)
        // and user channels (▸) don't belong there — only collections
        // that open as a track list.
        playlists.retain(|p| matches!(p.kind, PlaylistKind::Playlist | PlaylistKind::Album));
        // YouTube Music's system "Episodes for Later" list is podcast
        // noise in a music library — hide it from the sidebar.
        playlists.retain(|p| {
            let title = p.title.to_lowercase();
            !(title.contains("episode") && title.contains("later"))
        });
        Ok(playlists)
    }

    /// The "For you" browse page: mixes, trending songs, community
    /// playlists and the mood/genre category grids. Home and explore are
    /// fetched together; optional shelves are skipped if that source fails
    /// rather than making the whole page unavailable.
    pub async fn for_you(&self) -> anyhow::Result<Vec<BrowseShelf>> {
        let (home, trending, categories) = tokio::join!(
            self.home_for_you_shelves(),
            self.trending_songs_shelf(),
            self.moods_and_genres_shelves(),
        );
        let (mix, community) = home?;
        let mut shelves = Vec::new();
        if let Some(shelf) = mix {
            shelves.push(shelf);
        }
        if let Ok(Some(shelf)) = trending {
            shelves.push(shelf);
        }
        if let Some(shelf) = community {
            shelves.push(shelf);
        }
        if let Ok(categories) = categories {
            shelves.extend(categories);
        }
        shelves.retain(|shelf| !shelf.items.is_empty());
        if shelves.is_empty() {
            anyhow::bail!("nothing to browse yet");
        }
        Ok(shelves)
    }

    /// Walk the home feed until both personalised shelves are found. The
    /// feed returns a few shelves per page, so a small number of
    /// continuations is normal.
    async fn home_for_you_shelves(
        &self,
    ) -> anyhow::Result<(Option<BrowseShelf>, Option<BrowseShelf>)> {
        let mut body = json!({ "context": context(), "browseId": "FEmusic_home" });
        let mut value = self.browse(&body).await?;
        let mut seen_tokens = Vec::new();
        let mut mix = None;
        let mut community = None;

        for _ in 0..12 {
            let mut shelves = Vec::new();
            collect_browse_shelves(&value, &mut shelves);
            for mut shelf in shelves {
                let title = shelf.title.to_lowercase();
                if mix.is_none() && (title == "mixed for you" || title == "mix for you") {
                    shelf.title = "Mix for you".to_string();
                    mix = Some(shelf);
                } else if community.is_none()
                    && (title == "from the community"
                        || title == "from community"
                        || title == "trending community playlists")
                {
                    community = Some(shelf);
                }
            }
            if mix.is_some() && community.is_some() {
                break;
            }
            match section_continuation(&value) {
                Some(token) if !seen_tokens.contains(&token) => {
                    seen_tokens.push(token.clone());
                    body = json!({ "context": context(), "continuation": token });
                    // A single flaky continuation should not throw away the
                    // shelves already found; keep what we have.
                    match self.browse(&body).await {
                        Ok(next) => value = next,
                        Err(_) => break,
                    }
                }
                _ => break,
            }
        }
        Ok((mix, community))
    }

    /// The explore page's "Trending" shelf is made of playable video rows.
    async fn trending_songs_shelf(&self) -> anyhow::Result<Option<BrowseShelf>> {
        let value = self
            .browse(&json!({ "context": context(), "browseId": "FEmusic_explore" }))
            .await?;
        let mut shelves = Vec::new();
        collect_browse_shelves(&value, &mut shelves);
        let Some(mut shelf) = shelves
            .into_iter()
            .find(|shelf| shelf.title.eq_ignore_ascii_case("trending"))
        else {
            return Ok(None);
        };
        shelf.items.retain(|item| item.track().is_some());
        if shelf.items.is_empty() {
            return Ok(None);
        }
        shelf.title = "Trending songs for you".to_string();
        Ok(Some(shelf))
    }

    /// Moods and genres are navigation grids, not playlists. Keep the two
    /// labelled grids and turn each button into an openable category.
    async fn moods_and_genres_shelves(&self) -> anyhow::Result<Vec<BrowseShelf>> {
        let value = self.browse(&json!({
            "context": context(),
            "browseId": "FEmusic_moods_and_genres",
        })).await?;
        let mut grids = Vec::new();
        collect_navigation_grids(&value, &mut grids);
        let mut shelves = Vec::new();
        for (title, buttons) in grids {
            let lower = title.to_lowercase();
            let (label, kind) = if lower.contains("mood") {
                ("Moods", PlaylistKind::Mood)
            } else if lower.contains("genre") {
                ("Genres", PlaylistKind::Genre)
            } else {
                continue;
            };
            let mut items = Vec::new();
            for button in buttons {
                if let Some(playlist) = parse_category_button(button, kind) {
                    items.push(SearchHit::Collection(playlist));
                }
            }
            if !items.is_empty() {
                shelves.push(BrowseShelf {
                    title: label.to_string(),
                    items,
                });
            }
        }
        Ok(shelves)
    }

    /// Open one of the mood/genre buttons: its browse page is a collection
    /// of playlists, surfaced as one flat list.
    pub async fn category_playlists(
        &self,
        browse_id: &str,
        params: &str,
    ) -> anyhow::Result<Vec<Playlist>> {
        let value = self.browse(&json!({
            "context": context(),
            "browseId": browse_id,
            "params": params,
        })).await?;
        let mut playlists = Vec::new();
        collect_two_row_playlists(&value, &mut playlists);
        if playlists.is_empty() {
            let mut hits = Vec::new();
            collect_search_hits(&value, &mut hits);
            playlists.extend(hits.into_iter().filter_map(|hit| hit.collection().cloned()));
        }
        playlists.retain(|p| matches!(p.kind, PlaylistKind::Playlist | PlaylistKind::Album));
        let mut seen = std::collections::HashSet::new();
        playlists.retain(|p| seen.insert(p.id.clone()));
        playlists.truncate(80);
        Ok(playlists)
    }

    /// All tracks in a playlist or auto-playlist (e.g. "VLLM" for liked songs).
    pub async fn playlist_tracks(&self, playlist_id: &str) -> anyhow::Result<Vec<Track>> {
        let body = json!({ "context": context(), "browseId": playlist_id });
        let value = self.browse(&body).await?;
        let mut tracks = Vec::new();
        collect_renderers(&value, "musicResponsiveListItemRenderer", &mut tracks);

        // Album rows omit the artist (it lives in the header strapline), so use
        // that as a fallback for any track missing one.
        let owner = header_owner(&value);
        if let Some(owner) = owner {
            for track in tracks.iter_mut() {
                if track.artist.is_empty() || track.artist == "Unknown Artist" {
                    track.artist = owner.clone();
                }
            }
        }

        // Only page through the playlist's *own* continuation. The
        // section-level continuation on the same page points at an unrelated
        // "suggested" section and would otherwise inject foreign tracks into
        // short playlists (and artists).
        let mut token = find_shelf_continuation(&value);
        let mut seen_tokens = Vec::new();

        while let Some(next) = token {
            if seen_tokens.contains(&next) {
                break;
            }
            seen_tokens.push(next.clone());
            let body = json!({ "context": context(), "continuation": next });
            let value = self.browse(&body).await?;
            collect_renderers(&value, "musicResponsiveListItemRenderer", &mut tracks);
            token = find_next_continuation(&value);
        }

        // Continuations occasionally repeat an item; keep the first occurrence.
        let mut ids = std::collections::HashSet::new();
        tracks.retain(|t| ids.insert(t.id.clone()));
        Ok(tracks)
    }

    /// The user's liked songs (the "Liked Music" auto-playlist).
    pub async fn liked_tracks(&self) -> anyhow::Result<Vec<Track>> {
        self.playlist_tracks("VLLM").await
    }

    async fn browse(&self, body: &Value) -> anyhow::Result<Value> {
        let headers = self
            .headers
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("not signed in"))?;

        let url = format!("{}&key={}", BROWSE_URL, API_KEY);
        let mut req = self.client.post(url).json(body);
        for (k, v) in headers {
            if let Some(s) = v.as_str() {
                req = req.header(k.as_str(), s);
            }
        }

        let resp = req.send().await?;
        Ok(resp.json().await?)
    }

    /// POST to a public Innertube endpoint. The lyrics endpoints work
    /// unauthenticated, and must NOT carry the WEB_REMIX auth headers:
    /// that combination with the ANDROID_MUSIC lyrics browse is rejected
    /// with HTTP 400 (which silently loses the timed-lyrics model).
    async fn post_innertube(&self, base_url: &str, body: &Value) -> anyhow::Result<Value> {
        let url = format!("{}&key={}", base_url, API_KEY);
        let resp = self.client.post(url).json(body).send().await?;
        Ok(resp.json().await?)
    }

    /// Rate the current song for the heart key: `like` adds it to Liked
    /// Music, otherwise the rating is cleared (ytmusicapi's `rate_song` —
    /// the `like/like` and `like/removelike` endpoints, same body).
    pub async fn rate(&self, video_id: &str, like: bool) -> anyhow::Result<()> {
        let headers = self
            .headers
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("not signed in"))?;

        let action = if like { "like" } else { "removelike" };
        let url = format!("{}{}?prettyPrint=false&key={}", LIKE_URL, action, API_KEY);
        let body = json!({ "context": context(), "target": { "videoId": video_id } });
        let mut req = self.client.post(url).json(&body);
        for (k, v) in headers {
            if let Some(s) = v.as_str() {
                req = req.header(k.as_str(), s);
            }
        }

        let resp = req.send().await?;
        let status = resp.status();
        let value: Value = resp.json().await?;
        if !status.is_success() || value.get("error").is_some() {
            anyhow::bail!("HTTP {}", status);
        }
        Ok(())
    }

    /// Append a song to a playlist — the `a` key's add-to-playlist
    /// picker (ytmusicapi's `add_playlist_items`: a single `ACTION_ADD`
    /// edit whose dedupe option lets the server skip duplicates).
    pub async fn add_to_playlist(&self, playlist_id: &str, video_id: &str) -> anyhow::Result<()> {
        let headers = self
            .headers
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("not signed in"))?;

        let url = format!("{}?prettyPrint=false&key={}", EDIT_URL, API_KEY);
        let body = json!({
            "context": context(),
            "playlistId": playlist_id,
            "actions": [{
                "action": "ACTION_ADD",
                "addedVideoIds": [video_id],
                "addedVideoId": video_id,
                "dedupeOption": "DEDUPE_OPTION_SKIP",
            }],
        });
        let mut req = self.client.post(url).json(&body);
        for (k, v) in headers {
            if let Some(s) = v.as_str() {
                req = req.header(k.as_str(), s);
            }
        }

        let resp = req.send().await?;
        let status = resp.status();
        let value: Value = resp.json().await?;
        if !status.is_success() || value.get("error").is_some() {
            anyhow::bail!("HTTP {}", status);
        }
        Ok(())
    }

    /// Drop a song from a playlist (ytmusicapi's `remove_playlist_items`:
    /// an `ACTION_REMOVE` edit listing the video ids to erase).
    /// Server-side this clears every copy whose id matches.
    pub async fn remove_from_playlist(
        &self,
        playlist_id: &str,
        video_id: &str,
    ) -> anyhow::Result<()> {
        let headers = self
            .headers
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("not signed in"))?;

        let url = format!("{}?prettyPrint=false&key={}", EDIT_URL, API_KEY);
        let body = json!({
            "context": context(),
            "playlistId": playlist_id,
            "actions": [{
                "action": "ACTION_REMOVE",
                "removedVideoIds": [video_id],
            }],
        });
        let mut req = self.client.post(url).json(&body);
        for (k, v) in headers {
            if let Some(s) = v.as_str() {
                req = req.header(k.as_str(), s);
            }
        }

        let resp = req.send().await?;
        let status = resp.status();
        let value: Value = resp.json().await?;
        if !status.is_success() || value.get("error").is_some() {
            anyhow::bail!("HTTP {}", status);
        }
        Ok(())
    }

    /// `next` page yields the lyrics tab's browseId (MPLYt…), then a
    /// *mobile* browse returns timed lyrics when available, with the
    /// plain-text shelf as the fallback.
    pub async fn get_lyrics(&self, video_id: &str) -> anyhow::Result<crate::lyrics::YtLyrics> {
        use crate::lyrics::YtLyrics;

        let body = json!({
            "context": context(),
            "videoId": video_id,
            "playlistId": format!("RDAMVM{video_id}"),
            "watchEndpointMusicSupportedConfigs": {
                "watchEndpointMusicConfig": {
                    "hasPersistentPlaylistPanel": true,
                    "musicVideoType": "MUSIC_VIDEO_TYPE_ATV",
                }
            },
        });
        let next = self.post_innertube(NEXT_URL, &body).await?;
        let Some(lyrics_id) = find_page_type(&next, "MUSIC_PAGE_TYPE_TRACK_LYRICS") else {
            return Ok(YtLyrics::None);
        };

        // Timed lyrics live in the mobile client's response — the context
        // swap mirrors ytmusicapi's `as_mobile()`.
        let mobile_body = json!({ "context": mobile_context(), "browseId": lyrics_id });
        if let Ok(resp) = self.post_innertube(BROWSE_URL, &mobile_body).await {
            if let Some(data) = find_json(&resp, "timedLyricsModel")
                .and_then(|m| m.pointer("/lyricsData/timedLyricsData"))
                .and_then(Value::as_array)
            {
                if !data.is_empty() {
                    let all_cued = data.iter().all(|d| {
                        d.get("lyricLine").is_some() && d.get("cueRange").is_some()
                    });
                    if all_cued {
                        let lines: Vec<(f64, String)> = data
                            .iter()
                            .filter_map(|d| {
                                let text = d.get("lyricLine")?.as_str()?.to_string();
                                // The API sends milliseconds as strings.
                                let raw = d.pointer("/cueRange/startTimeMilliseconds")?;
                                let ms = raw.as_u64().or_else(|| {
                                    raw.as_str().and_then(|s| s.parse::<u64>().ok())
                                })?;
                                Some((ms as f64 / 1000.0, text))
                            })
                            .collect();
                        if !lines.is_empty() {
                            return Ok(YtLyrics::Timed(lines));
                        }
                    }
                    // Cues missing: ytmusicapi joins the bare lines instead.
                    let joined = data
                        .iter()
                        .filter_map(|d| d.get("lyricLine").and_then(Value::as_str))
                        .collect::<Vec<_>>()
                        .join("\n");
                    if !joined.is_empty() {
                        return Ok(YtLyrics::Text(joined));
                    }
                }
            }
            if let Some(text) = extract_plain_lyrics(&resp) {
                return Ok(YtLyrics::Text(text));
            }
        }

        // Plain lyrics from a regular web browse (ytmusicapi's retry).
        let web_body = json!({ "context": context(), "browseId": lyrics_id });
        let resp = self.post_innertube(BROWSE_URL, &web_body).await?;
        match extract_plain_lyrics(&resp) {
            Some(text) => Ok(YtLyrics::Text(text)),
            None => Ok(YtLyrics::None),
        }
    }

    async fn search_public(&self, query: &str) -> anyhow::Result<Vec<Track>> {
        let url = format!(
            "https://vid.puffyan.us/api/v1/search?q={}&type=video",
            urlencoding::encode(query)
        );

        if let Ok(response) = self.client.get(&url).send().await {
            if let Ok(json) = response.json::<Vec<Value>>().await {
                let mut tracks = Vec::new();
                for item in json.iter().take(20) {
                    if let Some(id) = item["videoId"].as_str() {
                        let title = item["title"].as_str().unwrap_or("Unknown Title").to_string();
                        let artist = item["author"].as_str().unwrap_or("Unknown Artist").to_string();
                        let duration_secs = item["lengthSeconds"].as_i64().unwrap_or(0);
                        let duration =
                            Some(format!("{:02}:{:02}", duration_secs / 60, duration_secs % 60));
                        tracks.push(Track {
                            id: id.to_string(),
                            title,
                            artist,
                            album: None,
                            duration,
                            thumbnail_url: None,
                            like: None,
                            // Stamped from the registry once stored.
                            downloaded: false,
                        });
                    }
                }
                if !tracks.is_empty() {
                    return Ok(tracks);
                }
            }
        }

        // Offline / unreachable fallback so the UI is never empty.
        Ok(vec![
            Track {
                id: "jfKfPfyJRdk".to_string(),
                title: "Lofi Hip Hop Radio - Beats to Relax/Study to".to_string(),
                artist: "Lofi Girl".to_string(),
                album: Some("Lofi Sessions".to_string()),
                duration: Some("Live".to_string()),
                thumbnail_url: None,
                like: None,
                downloaded: false,
            },
            Track {
                id: "dQw4w9WgXcQ".to_string(),
                title: "Never Gonna Give You Up".to_string(),
                artist: "Rick Astley".to_string(),
                album: Some("Whenever You Need Somebody".to_string()),
                duration: Some("03:33".to_string()),
                thumbnail_url: None,
                like: None,
                downloaded: false,
            },
        ])
    }
}

fn context() -> Value {
    json!({
        "client": {
            "clientName": "WEB_REMIX",
            "clientVersion": CLIENT_VERSION,
            "hl": "en"
        },
        "user": { "lockedSafetyMode": false },
        "request": { "useSsl": true }
    })
}

/// The Android Music client context — mirrors ytmusicapi's `as_mobile()`,
/// which is what exposes the timed-lyrics model.
fn mobile_context() -> Value {
    json!({
        "client": {
            "clientName": "ANDROID_MUSIC",
            "clientVersion": "7.21.50",
            "hl": "en"
        },
        "user": { "lockedSafetyMode": false },
        "request": { "useSsl": true }
    })
}

/// First depth-first match for `key` anywhere in the tree.
fn find_json<'a>(value: &'a Value, key: &str) -> Option<&'a Value> {
    match value {
        Value::Object(map) => map
            .get(key)
            .or_else(|| map.values().find_map(|v| find_json(v, key))),
        Value::Array(arr) => arr.iter().find_map(|v| find_json(v, key)),
        _ => None,
    }
}

/// The lyrics tab's browseId: any `browseEndpoint` whose music config
/// declares `MUSIC_PAGE_TYPE_TRACK_LYRICS`.
fn find_page_type(value: &Value, want: &str) -> Option<String> {
    match value {
        Value::Object(map) => {
            if let Some(endpoint) = map.get("browseEndpoint") {
                let page_type = endpoint
                    .pointer(
                        "/browseEndpointContextSupportedConfigs/browseEndpointContextMusicConfig/pageType",
                    )
                    .and_then(Value::as_str);
                if page_type == Some(want) {
                    if let Some(id) = endpoint.get("browseId").and_then(Value::as_str) {
                        return Some(id.to_string());
                    }
                }
            }
            map.values().find_map(|v| find_page_type(v, want))
        }
        Value::Array(arr) => arr.iter().find_map(|v| find_page_type(v, want)),
        _ => None,
    }
}

/// Plain lyrics: the description shelf's runs, joined (ytmusicapi's
/// `DESCRIPTION_SHELF` path).
fn extract_plain_lyrics(value: &Value) -> Option<String> {
    let shelf = find_json(value, "musicDescriptionShelfRenderer")?;
    let description = shelf.get("description")?;
    if let Some(s) = description.as_str() {
        return Some(s.to_string());
    }
    let runs = description.get("runs")?.as_array()?;
    let text: String = runs
        .iter()
        .filter_map(|r| r.get("text").and_then(Value::as_str))
        .collect();
    (!text.is_empty()).then_some(text)
}

/// Recursively collect tracks from `musicResponsiveListItemRenderer` nodes.
fn collect_renderers(value: &Value, key: &str, out: &mut Vec<Track>) {
    match value {
        Value::Object(map) => {
            if let Some(renderer) = map.get(key) {
                if let Some(track) = parse_item(renderer) {
                    out.push(track);
                }
            }
            for v in map.values() {
                collect_renderers(v, key, out);
            }
        }
        Value::Array(arr) => {
            for v in arr {
                collect_renderers(v, key, out);
            }
        }
        _ => {}
    }
}

/// Recursively collect search hits. A responsive row is a playable track when
/// it carries `playlistItemData`, otherwise it is an openable collection
/// (album / artist / playlist).
fn collect_search_hits(value: &Value, out: &mut Vec<SearchHit>) {
    match value {
        Value::Object(map) => {
            if let Some(renderer) = map.get("musicResponsiveListItemRenderer") {
                if let Some(hit) = parse_search_hit(renderer) {
                    let id = match &hit {
                        SearchHit::Track(t) => format!("t:{}", t.id),
                        SearchHit::Collection(p) => format!("c:{}", p.id),
                    };
                    let duplicate = out.iter().any(|h| {
                        let existing = match h {
                            SearchHit::Track(t) => format!("t:{}", t.id),
                            SearchHit::Collection(p) => format!("c:{}", p.id),
                        };
                        existing == id
                    });
                    if !duplicate {
                        out.push(hit);
                    }
                }
            }
            for v in map.values() {
                collect_search_hits(v, out);
            }
        }
        Value::Array(arr) => {
            for v in arr {
                collect_search_hits(v, out);
            }
        }
        _ => {}
    }
}

fn parse_search_hit(renderer: &Value) -> Option<SearchHit> {
    if renderer.get("playlistItemData").is_some() {
        return parse_item(renderer).map(SearchHit::Track);
    }
    parse_collection(renderer).map(SearchHit::Collection)
}

/// Parse an album/artist/playlist row from a search response.
fn parse_collection(renderer: &Value) -> Option<Playlist> {
    let browse_endpoint = renderer.get("navigationEndpoint")?.get("browseEndpoint")?;
    let id = browse_endpoint.get("browseId")?.as_str()?.to_string();
    let page_type = browse_endpoint
        .get("browseEndpointContextSupportedConfigs")?
        .get("browseEndpointContextMusicConfig")?
        .get("pageType")?
        .as_str()?;
    let kind = match page_type {
        "MUSIC_PAGE_TYPE_PLAYLIST" => PlaylistKind::Playlist,
        "MUSIC_PAGE_TYPE_ALBUM" => PlaylistKind::Album,
        "MUSIC_PAGE_TYPE_ARTIST" => PlaylistKind::Artist,
        "MUSIC_PAGE_TYPE_USER_CHANNEL" => PlaylistKind::Channel,
        _ => return None,
    };

    let texts = flex_texts(renderer);
    let title = texts.first().cloned().unwrap_or_else(|| "Unknown".to_string());
    let subtitle = texts.get(1).cloned().unwrap_or_default();
    let track_count = parse_track_count(&subtitle);

    Some(Playlist {
        id,
        title,
        subtitle,
        track_count,
        kind,
    })
}

/// The text of each `musicResponsiveListItemFlexColumnRenderer` column.
fn flex_texts(renderer: &Value) -> Vec<String> {
    renderer
        .get("flexColumns")
        .and_then(|c| c.as_array())
        .map(|columns| {
            columns
                .iter()
                .filter_map(|col| {
                    col.get("musicResponsiveListItemFlexColumnRenderer")?
                        .get("text")?
                        .get("runs")
                        .and_then(|r| r.as_array())
                        .map(|runs| {
                            runs.iter()
                                .filter_map(|r| r.get("text").and_then(|t| t.as_str()))
                                .collect::<String>()
                        })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Collect the search filter chips (e.g. "Albums", "Artists").
fn collect_filters(value: &Value, out: &mut Vec<SearchFilter>) {
    match value {
        Value::Object(map) => {
            if let Some(chips) = map
                .get("chipCloudRenderer")
                .and_then(|c| c.get("chips"))
                .and_then(|c| c.as_array())
            {
                for chip in chips {
                    let Some(c) = chip.get("chipCloudChipRenderer") else {
                        continue;
                    };
                    let title = runs_text(c.get("text")).unwrap_or_default();
                    let params = c
                        .get("navigationEndpoint")
                        .and_then(|n| n.get("searchEndpoint"))
                        .and_then(|s| s.get("params"))
                        .and_then(|p| p.as_str())
                        .unwrap_or_default()
                        .to_string();
                    if !title.is_empty() && !params.is_empty() {
                        out.push(SearchFilter { title, params });
                    }
                }
            }
            for v in map.values() {
                collect_filters(v, out);
            }
        }
        Value::Array(arr) => {
            for v in arr {
                collect_filters(v, out);
            }
        }
        _ => {}
    }
}

/// Recursively collect playlists from `musicTwoRowItemRenderer` nodes.
fn collect_two_row_playlists(value: &Value, out: &mut Vec<Playlist>) {
    match value {
        Value::Object(map) => {
            if let Some(renderer) = map.get("musicTwoRowItemRenderer") {
                if let Some(playlist) = parse_playlist(renderer) {
                    out.push(playlist);
                }
            }
            for v in map.values() {
                collect_two_row_playlists(v, out);
            }
        }
        Value::Array(arr) => {
            for v in arr {
                collect_two_row_playlists(v, out);
            }
        }
        _ => {}
    }
}

/// Prefix used in a synthetic playlist id for a mood/genre button. The id
/// is never sent to Innertube as a playlist; the app recognises it before
/// opening and fetches the category instead.
const CATEGORY_PREFIX: &str = "ytmtui-category:";

pub fn category_id(browse_id: &str, params: &str) -> String {
    format!("{CATEGORY_PREFIX}{browse_id}|{params}")
}

/// Split a synthetic category id back into `(browseId, params)`.
pub fn category_parts(id: &str) -> Option<(&str, &str)> {
    id.strip_prefix(CATEGORY_PREFIX)?.split_once('|')
}

/// Recursively collect titled shelves from a home/explore response.
fn collect_browse_shelves(value: &Value, out: &mut Vec<BrowseShelf>) {
    match value {
        Value::Object(map) => {
            if let Some(shelf) = map.get("musicCarouselShelfRenderer") {
                if let Some(parsed) = parse_browse_shelf(shelf) {
                    out.push(parsed);
                }
            } else if let Some(shelf) = map.get("musicShelfRenderer") {
                if let Some(parsed) = parse_browse_shelf(shelf) {
                    out.push(parsed);
                }
            }
            for v in map.values() {
                collect_browse_shelves(v, out);
            }
        }
        Value::Array(arr) => {
            for v in arr {
                collect_browse_shelves(v, out);
            }
        }
        _ => {}
    }
}

fn parse_browse_shelf(shelf: &Value) -> Option<BrowseShelf> {
    let title = shelf_title(shelf)?;
    let mut items = Vec::new();
    for item in shelf.get("contents")?.as_array()? {
        if let Some(hit) = parse_browse_item(item) {
            items.push(hit);
        }
    }
    if items.is_empty() {
        return None;
    }
    Some(BrowseShelf { title, items })
}

fn shelf_title(shelf: &Value) -> Option<String> {
    let header = shelf.get("header")?;
    runs_text(
        header
            .pointer("/musicCarouselShelfBasicHeaderRenderer/title")
            .or_else(|| header.pointer("/musicShelfBasicHeaderRenderer/title"))
            .or_else(|| header.get("title")),
    )
}

fn parse_browse_item(item: &Value) -> Option<SearchHit> {
    if let Some(renderer) = item.get("musicTwoRowItemRenderer") {
        return parse_playlist(renderer).map(SearchHit::Collection);
    }
    if let Some(renderer) = item.get("musicResponsiveListItemRenderer") {
        return parse_item(renderer).map(SearchHit::Track);
    }
    None
}

/// Collect each labelled navigation grid on the moods/genres page.
fn collect_navigation_grids<'a>(value: &'a Value, out: &mut Vec<(String, Vec<&'a Value>)>) {
    match value {
        Value::Object(map) => {
            if let Some(grid) = map.get("gridRenderer") {
                let title = runs_text(
                    grid.pointer("/header/gridHeaderRenderer/title"),
                )
                .unwrap_or_default();
                let buttons: Vec<&Value> = grid
                    .get("items")
                    .and_then(Value::as_array)
                    .map(|items| {
                        items
                            .iter()
                            .filter_map(|item| item.get("musicNavigationButtonRenderer"))
                            .collect()
                    })
                    .unwrap_or_default();
                if !title.is_empty() && !buttons.is_empty() {
                    out.push((title, buttons));
                }
            }
            for v in map.values() {
                collect_navigation_grids(v, out);
            }
        }
        Value::Array(arr) => {
            for v in arr {
                collect_navigation_grids(v, out);
            }
        }
        _ => {}
    }
}

fn parse_category_button(renderer: &Value, kind: PlaylistKind) -> Option<Playlist> {
    let title = runs_text(renderer.get("buttonText"))?;
    let endpoint = renderer.pointer("/clickCommand/browseEndpoint")?;
    let browse_id = endpoint.get("browseId")?.as_str()?;
    let params = endpoint.get("params")?.as_str()?;
    if browse_id != "FEmusic_moods_and_genres_category" {
        return None;
    }
    Some(Playlist {
        id: category_id(browse_id, params),
        title,
        subtitle: kind_label(kind).to_string(),
        track_count: None,
        kind,
    })
}

fn kind_label(kind: PlaylistKind) -> &'static str {
    match kind {
        PlaylistKind::Playlist => "Playlist",
        PlaylistKind::Album => "Album",
        PlaylistKind::Artist => "Artist",
        PlaylistKind::Channel => "Channel",
        PlaylistKind::Mood => "Mood",
        PlaylistKind::Genre => "Genre",
    }
}

/// The section-level continuation on home/explore pages. A generic
/// `find_next_continuation` can pick up a shelf's own token instead.
fn section_continuation(value: &Value) -> Option<String> {
    let section = find_json(value, "sectionListContinuation")
        .or_else(|| find_json(value, "sectionListRenderer"))?;
    section
        .get("continuations")?
        .as_array()?
        .first()?
        .get("nextContinuationData")?
        .get("continuation")?
        .as_str()
        .map(str::to_string)
}

fn parse_playlist(renderer: &Value) -> Option<Playlist> {
    let browse_endpoint = renderer.get("navigationEndpoint")?.get("browseEndpoint")?;
    let id = browse_endpoint.get("browseId")?.as_str()?.to_string();

    // The library grid mixes playlists, subscribed artists and user channels.
    let page_type = browse_endpoint
        .get("browseEndpointContextSupportedConfigs")?
        .get("browseEndpointContextMusicConfig")?
        .get("pageType")?
        .as_str()?;
    let kind = match page_type {
        "MUSIC_PAGE_TYPE_PLAYLIST" => PlaylistKind::Playlist,
        "MUSIC_PAGE_TYPE_ALBUM" => PlaylistKind::Album,
        "MUSIC_PAGE_TYPE_ARTIST" => PlaylistKind::Artist,
        "MUSIC_PAGE_TYPE_USER_CHANNEL" => PlaylistKind::Channel,
        _ => return None,
    };

    let title = runs_text(renderer.get("title")).unwrap_or_else(|| "Unknown Playlist".to_string());
    let subtitle = runs_text(renderer.get("subtitle")).unwrap_or_default();
    let track_count = parse_track_count(&subtitle);

    Some(Playlist {
        id,
        title,
        subtitle,
        track_count,
        kind,
    })
}

/// Find the continuation token belonging to the playlist shelf itself. Unlike
/// `find_next_continuation`, this ignores the section-level token that points
/// at an unrelated "suggested" section.
fn find_shelf_continuation(value: &Value) -> Option<String> {
    fn find_shelf(v: &Value) -> Option<&Value> {
        match v {
            Value::Object(map) => {
                if let Some(shelf) = map.get("musicPlaylistShelfRenderer") {
                    return Some(shelf);
                }
                map.values().find_map(find_shelf)
            }
            Value::Array(arr) => arr.iter().find_map(find_shelf),
            _ => None,
        }
    }

    find_shelf(value)?
        .get("continuations")?
        .as_array()?
        .first()?
        .get("nextContinuationData")?
        .get("continuation")?
        .as_str()
        .map(|s| s.to_string())
}

/// Find the next-page continuation token, ignoring filter chips and other
/// unrelated continuations (which live under `header` / `frameworkUpdates`).
fn find_next_continuation(value: &Value) -> Option<String> {
    fn walk(v: &Value) -> Option<String> {
        match v {
            Value::Object(map) => {
                if let Some(token) = map
                    .get("nextContinuationData")
                    .and_then(|n| n.get("continuation"))
                    .and_then(|c| c.as_str())
                {
                    return Some(token.to_string());
                }
                for (k, val) in map {
                    if k == "header" || k == "frameworkUpdates" || k == "menu" {
                        continue;
                    }
                    if let Some(token) = walk(val) {
                        return Some(token);
                    }
                }
                None
            }
            Value::Array(arr) => arr.iter().find_map(walk),
            _ => None,
        }
    }
    walk(value)
}

/// The owning artist from an album/detail header, if present.
fn header_owner(value: &Value) -> Option<String> {
    fn find_header(v: &Value) -> Option<&Value> {
        match v {
            Value::Object(map) => {
                if let Some(header) = map.get("musicResponsiveHeaderRenderer") {
                    return Some(header);
                }
                if let Some(header) = map.get("musicDetailHeaderRenderer") {
                    return Some(header);
                }
                map.values().find_map(find_header)
            }
            Value::Array(arr) => arr.iter().find_map(find_header),
            _ => None,
        }
    }
    let header = find_header(value)?;
    runs_text(header.get("straplineTextOne"))
        .filter(|s| !s.is_empty())
        .or_else(|| runs_text(header.get("straplineTextTwo")).filter(|s| !s.is_empty()))
}

fn runs_text(node: Option<&Value>) -> Option<String> {
    let runs = node?.get("runs")?.as_array()?;
    let text: String = runs
        .iter()
        .filter_map(|r| r.get("text").and_then(|t| t.as_str()))
        .collect();
    if text.is_empty() {
        None
    } else {
        Some(text)
    }
}

fn parse_track_count(subtitle: &str) -> Option<usize> {
    let idx = subtitle.find("track")?;
    subtitle[..idx]
        .split_whitespace()
        .last()
        .and_then(|n| n.parse().ok())
}

fn parse_item(renderer: &Value) -> Option<Track> {
    let id = item_video_id(renderer)?;

    let columns = renderer.get("flexColumns")?.as_array()?;

    let text_of = |idx: usize| -> Option<String> {
        runs_text(
            columns
                .get(idx)?
                .get("musicResponsiveListItemFlexColumnRenderer")?
                .get("text"),
        )
    };

    let title = text_of(0).unwrap_or_else(|| "Unknown Title".to_string());

    // Duration may live in a fixed column (playlist/album rows).
    let fixed_duration = renderer
        .get("fixedColumns")
        .and_then(|f| f.get(0))
        .and_then(|c| c.get("musicResponsiveListItemFixedColumnRenderer"))
        .and_then(|r| r.get("text"))
        .and_then(|t| runs_text(Some(t)))
        .filter(|s| is_duration(s));

    // Subtitle columns can be either "Artist • Album • Duration" (search) or
    // separate artist / album columns (playlists).
    let mut fields: Vec<String> = Vec::new();
    for idx in 1..columns.len().max(1) {
        if let Some(text) = text_of(idx) {
            if text.contains('•') {
                fields.extend(text.split('•').map(|s| s.trim().to_string()));
            } else if !text.trim().is_empty() {
                fields.push(text.trim().to_string());
            }
        }
    }
    fields.retain(|s| !s.is_empty());
    // Drop statistics like "724 plays" / "1.4M views" so they aren't mistaken
    // for an artist or album.
    fields.retain(|s| !is_stat_label(s));

    // Search rows prepend a media-type badge ("Song", "Video", …) to the artist
    // column; drop it so the artist is parsed correctly.
    if fields
        .first()
        .map(|f| is_type_label(f))
        .unwrap_or(false)
    {
        fields.remove(0);
    }

    // Never mistake a duration for the artist: Top results for some videos
    // put the duration right after the type badge ("Song • 0:51").
    let artist = fields
        .first()
        .filter(|f| !is_duration(f))
        .cloned()
        .unwrap_or_default();

    let mut album = None;
    let mut duration = fixed_duration;
    for field in fields.iter().skip(1) {
        if is_duration(field) {
            if duration.is_none() {
                duration = Some(field.clone());
            }
        } else if album.is_none() {
            album = Some(field.clone());
        }
    }
    if duration.is_none() {
        duration = fields.iter().find(|f| is_duration(f)).cloned();
    }

    // The row's like button states the current rating directly — that's
    // what the `h` heart key reads before toggling.
    let like = renderer
        .pointer("/menu/menuRenderer/topLevelButtons/0/likeButtonRenderer/likeStatus")
        .and_then(Value::as_str)
        .map(|status| status == "LIKE");

    Some(Track {
        id,
        title,
        artist,
        album,
        duration,
        thumbnail_url: renderer_thumb(renderer),
        like,
        downloaded: false,
    })
}

/// Video id for a responsive row. Playlist/album rows carry
/// `playlistItemData`; explore/home rows often expose the id only on the
/// thumbnail's play button.
fn item_video_id(renderer: &Value) -> Option<String> {
    let direct = renderer
        .pointer("/playlistItemData/videoId")
        .or_else(|| renderer.pointer("/navigationEndpoint/watchEndpoint/videoId"))
        .or_else(|| {
            renderer.pointer(
                "/overlay/musicItemThumbnailOverlayRenderer/content/musicPlayButtonRenderer/playNavigationEndpoint/watchEndpoint/videoId",
            )
        })
        .and_then(Value::as_str);
    if let Some(id) = direct {
        return Some(id.to_string());
    }
    find_json(renderer, "watchEndpoint")?
        .get("videoId")?
        .as_str()
        .map(str::to_string)
}

/// Largest thumbnail URL carried by a list renderer, when present.
fn renderer_thumb(renderer: &Value) -> Option<String> {
    let thumbs = renderer
        .pointer("/thumbnail/thumbnails")
        .or_else(|| renderer.pointer("/thumbnailRenderer/musicThumbnailRenderer/thumbnail/thumbnails"))?
        .as_array()?;
    let mut best: Option<(&str, u64)> = None;
    for t in thumbs {
        let url = match t.get("url").and_then(|u| u.as_str()) {
            Some(u) => u,
            None => continue,
        };
        let w = t.get("width").and_then(|v| v.as_u64()).unwrap_or(0);
        if best.is_none_or(|(_, bw)| w >= bw) {
            best = Some((url, w));
        }
    }
    best.map(|(u, _)| u.to_string())
}

/// Media-type badges that YouTube Music prepends to search result subtitles.
fn is_type_label(s: &str) -> bool {
    matches!(
        s,
        "Song"
            | "Video"
            | "Episode"
            | "Album"
            | "Single"
            | "EP"
            | "Playlist"
            | "Artist"
            | "Profile"
            | "Podcast"
            | "Community playlist"
            | "Featured playlist"
    )
}

/// Labels like "724 plays", "1.4M views", "36 songs" — count statistics rather
/// than an artist or album name.
fn is_stat_label(s: &str) -> bool {
    let low = s.to_lowercase();
    for word in ["plays", "views", "songs", "subscribers", "audience", "listeners"] {
        if let Some(prefix) = low.strip_suffix(word) {
            let prefix = prefix.trim();
            if prefix.chars().next().map(|c| c.is_ascii_digit()).unwrap_or(false) {
                return true;
            }
        }
    }
    false
}

fn is_duration(s: &str) -> bool {
    let parts: Vec<&str> = s.split(':').collect();
    (parts.len() == 2 || parts.len() == 3)
        && parts
            .iter()
            .all(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()))
}

#[cfg(test)]
mod browse_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_home_shelf_with_collections_and_songs() {
        let value = json!({
            "header": {
                "musicCarouselShelfBasicHeaderRenderer": {
                    "title": { "runs": [{ "text": "Mixed for you" }] }
                }
            },
            "contents": [
                {
                    "musicTwoRowItemRenderer": {
                        "title": { "runs": [{ "text": "My Mix" }] },
                        "subtitle": { "runs": [{ "text": "Artist A" }] },
                        "navigationEndpoint": {
                            "browseEndpoint": {
                                "browseId": "VLRDMIX",
                                "browseEndpointContextSupportedConfigs": {
                                    "browseEndpointContextMusicConfig": {
                                        "pageType": "MUSIC_PAGE_TYPE_PLAYLIST"
                                    }
                                }
                            }
                        }
                    }
                },
                {
                    "musicResponsiveListItemRenderer": {
                        "overlay": {
                            "musicItemThumbnailOverlayRenderer": {
                                "content": {
                                    "musicPlayButtonRenderer": {
                                        "playNavigationEndpoint": {
                                            "watchEndpoint": { "videoId": "vid123" }
                                        }
                                    }
                                }
                            }
                        },
                        "flexColumns": [
                            {
                                "musicResponsiveListItemFlexColumnRenderer": {
                                    "text": { "runs": [{ "text": "Trending song" }] }
                                }
                            },
                            {
                                "musicResponsiveListItemFlexColumnRenderer": {
                                    "text": { "runs": [{ "text": "Some Artist • 1.2M views" }] }
                                }
                            }
                        ]
                    }
                }
            ]
        });

        let shelf = parse_browse_shelf(&value).expect("shelf");
        assert_eq!(shelf.title, "Mixed for you");
        assert_eq!(shelf.items.len(), 2);
        match &shelf.items[0] {
            SearchHit::Collection(playlist) => assert_eq!(playlist.id, "VLRDMIX"),
            other => panic!("expected collection, got {other:?}"),
        }
        match &shelf.items[1] {
            SearchHit::Track(track) => {
                assert_eq!(track.id, "vid123");
                assert_eq!(track.title, "Trending song");
                assert_eq!(track.artist, "Some Artist");
            }
            other => panic!("expected track, got {other:?}"),
        }
    }

    #[test]
    fn parses_mood_and_genre_categories() {
        let value = json!({
            "gridRenderer": {
                "header": {
                    "gridHeaderRenderer": {
                        "title": { "runs": [{ "text": "Moods & moments" }] }
                    }
                },
                "items": [{
                    "musicNavigationButtonRenderer": {
                        "buttonText": { "runs": [{ "text": "Chill" }] },
                        "clickCommand": {
                            "browseEndpoint": {
                                "browseId": "FEmusic_moods_and_genres_category",
                                "params": "category-params"
                            }
                        }
                    }
                }]
            }
        });

        let mut grids = Vec::new();
        collect_navigation_grids(&value, &mut grids);
        assert_eq!(grids.len(), 1);
        assert_eq!(grids[0].0, "Moods & moments");
        let playlist = parse_category_button(grids[0].1[0], PlaylistKind::Mood).expect("button");
        assert_eq!(playlist.title, "Chill");
        assert_eq!(playlist.kind, PlaylistKind::Mood);
        assert_eq!(
            category_parts(&playlist.id),
            Some(("FEmusic_moods_and_genres_category", "category-params"))
        );
    }
}
