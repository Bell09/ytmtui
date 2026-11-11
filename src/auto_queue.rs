//! Auto Queue: extend a finished queue with similar tracks.
//!
//! Recommendations are fetched off the UI thread as soon as the last track in
//! the playback order starts. Only one request may be in flight, and the reply
//! is accepted only while the queue version and anchor track still match, so a
//! manual queue edit cannot inject stale rows. The module keeps the policy and
//! deduplication pure so it is cheap to test without the TUI or the network.

use std::collections::HashSet;

use serde_json::Value;

use crate::app::RepeatMode;
use crate::models::Track;

/// Candidates requested from YouTube Music. A few extra leave room for rows
/// already present in the queue without asking for a second page.
pub const FETCH_LIMIT: usize = 10;
/// Tracks appended per recommendation round. Keeping this small bounds both
/// the reply we hold in memory and how far the queue can grow in one burst.
pub const ADD_LIMIT: usize = 5;

/// Context for deciding whether a new recommendation request is useful.
pub struct RequestContext<'a> {
    pub queue_version: u64,
    pub anchor: &'a str,
    pub has_next: bool,
    pub playing: bool,
    pub repeat: RepeatMode,
    pub offline: bool,
}

/// Context against which a finished reply is validated.
pub struct ApplyContext<'a> {
    pub queue_version: u64,
    pub current_id: Option<&'a str>,
    /// Playback is still on the anchor track (playing, buffering or paused).
    /// A reply that lands after the queue stopped or was skipped is dropped.
    pub active: bool,
    pub repeat: RepeatMode,
    pub offline: bool,
}

#[derive(Debug)]
struct Request {
    seq: u64,
    anchor: String,
    queue_version: u64,
}

/// Small state machine for recommendation requests.
pub struct AutoQueue {
    enabled: bool,
    in_flight: Option<Request>,
    /// The last `(anchor, queue_version)` we asked about. It stops a failed or
    /// empty response from being retried on every UI tick.
    attempted: Option<(String, u64)>,
    next_seq: u64,
}

impl AutoQueue {
    pub fn new(enabled: bool) -> Self {
        Self {
            enabled,
            in_flight: None,
            attempted: None,
            next_seq: 1,
        }
    }

    pub fn enabled(&self) -> bool {
        self.enabled
    }

    /// Toggle Auto Queue. Changing the mode invalidates an outstanding reply
    /// so a late result cannot extend the queue after it was switched off.
    pub fn set_enabled(&mut self, enabled: bool) {
        if self.enabled != enabled {
            self.enabled = enabled;
            self.invalidate();
        }
    }

    /// Forget the last attempt so a mode change can be reconsidered, but keep
    /// an actual request tracked: the network task cannot be cancelled, and
    /// starting another would break the one-request-at-a-time bound. The
    /// accept-time checks drop its reply if the mode really changed.
    pub fn invalidate(&mut self) {
        self.attempted = None;
    }

    /// Return the sequence number for a newly started request, or `None` when
    /// the queue is not at its end / Auto Queue must stay quiet.
    pub fn should_request(&mut self, ctx: RequestContext<'_>) -> Option<u64> {
        if !self.enabled
            || ctx.offline
            || ctx.repeat != RepeatMode::Off
            || !ctx.playing
            || ctx.has_next
            || ctx.anchor.trim().is_empty()
            || crate::offline::is_local(ctx.anchor)
        {
            return None;
        }
        if self.in_flight.is_some() {
            return None;
        }
        if self
            .attempted
            .as_ref()
            .is_some_and(|(anchor, version)| {
                anchor == ctx.anchor && *version == ctx.queue_version
            })
        {
            return None;
        }

        let seq = self.next_seq;
        self.next_seq = self.next_seq.wrapping_add(1);
        self.in_flight = Some(Request {
            seq,
            anchor: ctx.anchor.to_string(),
            queue_version: ctx.queue_version,
        });
        Some(seq)
    }

    /// Validate and consume a recommendation reply. Returns the fresh tracks
    /// only when the request is still current; failures and stale replies are
    /// silently discarded so playback is never interrupted.
    pub fn accept(
        &mut self,
        seq: u64,
        result: Result<Vec<Track>, String>,
        ctx: ApplyContext<'_>,
    ) -> Option<Vec<Track>> {
        let request = self.in_flight.as_ref()?;
        if request.seq != seq {
            return None;
        }
        let request = self.in_flight.take()?;
        self.attempted = Some((request.anchor.clone(), request.queue_version));

        if !self.enabled
            || ctx.offline
            || ctx.repeat != RepeatMode::Off
            || !ctx.active
            || ctx.queue_version != request.queue_version
            || ctx.current_id != Some(request.anchor.as_str())
        {
            return None;
        }
        result.ok()
    }
}

/// Append only tracks whose ids are not already in the queue. Existing
/// duplicates are deliberately left alone: Auto Queue must not rewrite a
/// queue the user built. Returns the number of rows appended.
pub fn append_unique(
    queue: &mut Vec<Track>,
    candidates: impl IntoIterator<Item = Track>,
    limit: usize,
) -> usize {
    if limit == 0 {
        return 0;
    }
    // Borrowed queue ids avoid cloning the whole queue; only the handful of
    // candidate ids that survive deduplication get cloned.
    let existing: HashSet<&str> = queue.iter().map(|track| track.id.as_str()).collect();
    let mut seen: HashSet<String> = HashSet::with_capacity(limit);
    let mut accepted = Vec::with_capacity(limit);
    for track in candidates {
        if accepted.len() >= limit {
            break;
        }
        if track.id.trim().is_empty()
            || existing.contains(track.id.as_str())
            || !seen.insert(track.id.clone())
        {
            continue;
        }
        accepted.push(track);
    }
    let added = accepted.len();
    queue.extend(accepted);
    added
}

/// Whether Auto Queue is enabled in `config.json`. A missing or malformed
/// config keeps the feature on; an explicit boolean wins.
pub fn enabled_from_config() -> bool {
    std::fs::read_to_string(crate::auth::config_dir().join("config.json"))
        .ok()
        .map(|text| enabled_from_json(&text))
        .unwrap_or(true)
}

fn enabled_from_json(text: &str) -> bool {
    serde_json::from_str::<Value>(text)
        .ok()
        .and_then(|value| value.get("auto_queue").and_then(Value::as_bool))
        .unwrap_or(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn track(id: &str) -> Track {
        Track {
            id: id.to_string(),
            title: format!("Track {id}"),
            artist: "Artist".to_string(),
            album: None,
            duration: Some("3:00".to_string()),
            thumbnail_url: None,
            like: None,
            downloaded: false,
            set_video_id: None,
        }
    }

    fn request_ctx<'a>(anchor: &'a str, version: u64) -> RequestContext<'a> {
        RequestContext {
            queue_version: version,
            anchor,
            has_next: false,
            playing: true,
            repeat: RepeatMode::Off,
            offline: false,
        }
    }

    fn apply_ctx<'a>(anchor: &'a str, version: u64) -> ApplyContext<'a> {
        ApplyContext {
            queue_version: version,
            current_id: Some(anchor),
            active: true,
            repeat: RepeatMode::Off,
            offline: false,
        }
    }

    #[test]
    fn extension_appends_only_new_tracks_and_keeps_existing_duplicates() {
        let mut queue = vec![track("a"), track("a")];
        let added = append_unique(
            &mut queue,
            vec![track("a"), track("b"), track("b"), track("c")],
            ADD_LIMIT,
        );
        assert_eq!(added, 2);
        assert_eq!(
            queue.iter().map(|t| t.id.as_str()).collect::<Vec<_>>(),
            ["a", "a", "b", "c"]
        );
    }

    #[test]
    fn extension_caps_the_batch_and_ignores_empty_ids() {
        let mut queue = vec![track("a")];
        let added = append_unique(
            &mut queue,
            vec![track(""), track("b"), track("c"), track("d")],
            2,
        );
        assert_eq!(added, 2);
        assert_eq!(
            queue.iter().map(|t| t.id.as_str()).collect::<Vec<_>>(),
            ["a", "b", "c"]
        );
    }

    #[test]
    fn disabled_auto_queue_never_requests() {
        let mut auto = AutoQueue::new(false);
        assert!(auto.should_request(request_ctx("a", 1)).is_none());
        auto.set_enabled(true);
        assert!(auto.should_request(request_ctx("a", 1)).is_some());
    }

    #[test]
    fn repeat_all_and_one_do_not_extend_the_queue() {
        for repeat in [RepeatMode::All, RepeatMode::One] {
            let mut auto = AutoQueue::new(true);
            let mut ctx = request_ctx("a", 1);
            ctx.repeat = repeat;
            assert!(auto.should_request(ctx).is_none());
        }
        let mut auto = AutoQueue::new(true);
        assert!(auto.should_request(request_ctx("a", 1)).is_some());
    }

    #[test]
    fn stale_results_are_discarded_after_a_queue_change() {
        let mut auto = AutoQueue::new(true);
        let seq = auto.should_request(request_ctx("a", 4)).expect("request");
        assert!(
            auto.accept(seq, Ok(vec![track("b")]), apply_ctx("a", 5))
                .is_none()
        );
        // The changed queue is eligible for a fresh request.
        assert!(auto.should_request(request_ctx("a", 5)).is_some());
    }

    #[test]
    fn stale_results_are_discarded_after_the_anchor_changes() {
        let mut auto = AutoQueue::new(true);
        let seq = auto.should_request(request_ctx("a", 4)).expect("request");
        assert!(
            auto.accept(seq, Ok(vec![track("b")]), apply_ctx("c", 4))
                .is_none()
        );
    }

    #[test]
    fn a_reply_that_arrives_after_playback_stopped_is_discarded() {
        let mut auto = AutoQueue::new(true);
        let seq = auto.should_request(request_ctx("a", 6)).expect("request");
        let mut ctx = apply_ctx("a", 6);
        ctx.active = false;
        assert!(auto.accept(seq, Ok(vec![track("b")]), ctx).is_none());
        // The stopped queue is not asked about again.
        let mut request = request_ctx("a", 6);
        request.playing = false;
        assert!(auto.should_request(request).is_none());
    }

    #[test]
    fn invalidating_a_request_cannot_start_a_second_one() {
        let mut auto = AutoQueue::new(true);
        let seq = auto.should_request(request_ctx("a", 2)).expect("request");
        auto.invalidate();
        assert!(auto.should_request(request_ctx("a", 2)).is_none());
        assert!(auto.accept(seq, Err("superseded".into()), apply_ctx("a", 2)).is_none());
    }

    #[test]
    fn recommendation_failure_is_silent_and_not_retried_for_the_same_queue() {
        let mut auto = AutoQueue::new(true);
        let seq = auto.should_request(request_ctx("a", 7)).expect("request");
        assert!(auto.accept(seq, Err("network down".into()), apply_ctx("a", 7)).is_none());
        assert!(auto.should_request(request_ctx("a", 7)).is_none());
        assert!(auto.should_request(request_ctx("a", 8)).is_some());
    }

    #[test]
    fn empty_recommendations_are_a_no_op_and_not_retried() {
        let mut auto = AutoQueue::new(true);
        let seq = auto.should_request(request_ctx("a", 3)).expect("request");
        assert_eq!(
            auto.accept(seq, Ok(Vec::new()), apply_ctx("a", 3)),
            Some(Vec::new())
        );
        assert!(auto.should_request(request_ctx("a", 3)).is_none());
    }

    #[test]
    fn auto_queue_config_defaults_on_and_honours_false() {
        assert!(enabled_from_json("{}"));
        assert!(enabled_from_json("not json"));
        assert!(enabled_from_json(r#"{"auto_queue": true}"#));
        assert!(!enabled_from_json(r#"{"auto_queue": false}"#));
    }
}
