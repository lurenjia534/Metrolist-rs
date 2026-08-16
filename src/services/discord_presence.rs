use std::{
    sync::{
        Arc, Mutex,
        mpsc::{Receiver, SyncSender, TrySendError, sync_channel},
    },
    thread,
    time::Duration,
};

use discord_rich_presence::{
    DiscordIpc as _, DiscordIpcClient,
    activity::{Activity, ActivityType, Assets, Button, Timestamps},
};

use crate::{AppError, Result, domain::Song};

pub const DISCORD_APPLICATION_ID: &str = "1447278780795064401";
const YOUTUBE_WATCH_URL: &str = "https://music.youtube.com/watch?v=";
const METROLIST_URL: &str = "https://github.com/MetrolistGroup/Metrolist";
const RETRY_SECONDS: u64 = 30;
const PAUSE_TIMEOUT_SECONDS: u64 = 60;
const COMMAND_CAPACITY: usize = 8;
const MAX_TEXT_CHARS: usize = 128;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DiscordPresenceState {
    #[default]
    Idle,
    Connecting,
    Active,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct DiscordPresenceSnapshot {
    pub state: DiscordPresenceState,
    pub last_error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct DiscordPresence {
    details: String,
    state: String,
    large_image: Option<String>,
    start: Option<i64>,
    end: Option<i64>,
    listen_url: String,
}

impl DiscordPresence {
    fn from_observation(song: &Song, observation: &DiscordPlaybackObservation<'_>) -> Self {
        let speed = f64::from(observation.tempo_milli) / 1_000.0;
        let speed_suffix = if observation.tempo_milli != 1_000 {
            format!(" [{speed:.2}x]")
        } else {
            String::new()
        };
        let details = bounded_text(&format!("{}{}", song.title, speed_suffix), "Unknown Track");
        let state = bounded_text(&song.artist_line(), "Unknown Artist");
        let large_image = song
            .thumbnail_url
            .as_deref()
            .filter(|url| url.starts_with("https://") && url.len() <= 512)
            .map(str::to_owned);
        let (start, end) = if observation.state == DiscordPlaybackState::Playing {
            let elapsed = observation.position.as_secs_f64() / speed;
            let start = observation
                .unix_seconds
                .saturating_sub(elapsed.max(0.0) as u64);
            let end = observation.duration.and_then(|duration| {
                let remaining = duration.saturating_sub(observation.position);
                let remaining = remaining.as_secs_f64() / speed;
                i64::try_from(
                    observation
                        .unix_seconds
                        .saturating_add(remaining.max(0.0) as u64),
                )
                .ok()
            });
            (i64::try_from(start).ok(), end)
        } else {
            (None, None)
        };
        Self {
            details,
            state,
            large_image,
            start,
            end,
            listen_url: format!("{YOUTUBE_WATCH_URL}{}", song.video_id),
        }
    }

    fn into_activity(self) -> Activity<'static> {
        let mut activity = Activity::new()
            .name("Metrolist")
            .activity_type(ActivityType::Listening)
            .details(self.details)
            .state(self.state)
            .buttons(vec![
                Button::new("Listen on YouTube Music", self.listen_url),
                Button::new("Visit Metrolist", METROLIST_URL),
            ]);
        if self.start.is_some() || self.end.is_some() {
            let mut timestamps = Timestamps::new();
            if let Some(start) = self.start {
                timestamps = timestamps.start(start);
            }
            if let Some(end) = self.end {
                timestamps = timestamps.end(end);
            }
            activity = activity.timestamps(timestamps);
        }
        if let Some(large_image) = self.large_image {
            activity = activity.assets(
                Assets::new()
                    .large_image(large_image)
                    .large_text("Album artwork"),
            );
        }
        activity
    }
}

fn bounded_text(value: &str, fallback: &str) -> String {
    let value = value.trim();
    let value = if value.is_empty() { fallback } else { value };
    value.chars().take(MAX_TEXT_CHARS).collect()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiscordPlaybackState {
    Loading,
    Playing,
    Paused,
    Inactive,
}

#[derive(Debug, Clone, Copy)]
pub struct DiscordPlaybackObservation<'a> {
    pub enabled: bool,
    pub song: Option<&'a Song>,
    pub state: DiscordPlaybackState,
    pub position: Duration,
    pub duration: Option<Duration>,
    pub tempo_milli: u16,
    pub unix_seconds: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DiscordPresenceAction {
    Update,
    Clear,
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct DiscordPresenceTracker {
    video_id: Option<String>,
    state: Option<DiscordPlaybackState>,
    tempo_milli: u16,
    last_update_at: Option<u64>,
    paused_at: Option<u64>,
    presence_visible: bool,
}

impl DiscordPresenceTracker {
    pub fn reset(&mut self) {
        *self = Self::default();
    }

    pub fn observe(
        &mut self,
        observation: DiscordPlaybackObservation<'_>,
    ) -> Option<DiscordPresenceAction> {
        let Some(song) = observation.song.filter(|_| observation.enabled) else {
            return self.clear_once();
        };
        if observation.state == DiscordPlaybackState::Inactive {
            return self.clear_once();
        }

        let changed = self.video_id.as_deref() != Some(&song.video_id)
            || self.state != Some(observation.state)
            || self.tempo_milli != observation.tempo_milli;
        if changed {
            self.video_id = Some(song.video_id.clone());
            self.state = Some(observation.state);
            self.tempo_milli = observation.tempo_milli;
            self.paused_at = (observation.state != DiscordPlaybackState::Playing)
                .then_some(observation.unix_seconds);
        }

        if observation.state != DiscordPlaybackState::Playing
            && self.paused_at.is_some_and(|paused_at| {
                observation.unix_seconds.saturating_sub(paused_at) >= PAUSE_TIMEOUT_SECONDS
            })
        {
            return self.clear_once();
        }

        let retry_due = self.last_update_at.is_none_or(|last_update_at| {
            observation.unix_seconds.saturating_sub(last_update_at) >= RETRY_SECONDS
        });
        if changed || !self.presence_visible || retry_due {
            self.last_update_at = Some(observation.unix_seconds);
            self.presence_visible = true;
            return Some(DiscordPresenceAction::Update);
        }
        None
    }

    fn clear_once(&mut self) -> Option<DiscordPresenceAction> {
        if !self.presence_visible && self.video_id.is_none() {
            return None;
        }
        self.reset();
        Some(DiscordPresenceAction::Clear)
    }
}

enum Command {
    Update(DiscordPresence),
    Clear,
    Shutdown,
}

pub struct DiscordPresenceService {
    sender: SyncSender<Command>,
    snapshot: Arc<Mutex<DiscordPresenceSnapshot>>,
}

impl DiscordPresenceService {
    pub fn new(application_id: &str) -> Result<Self> {
        validate_application_id(application_id)?;
        let (sender, receiver) = sync_channel(COMMAND_CAPACITY);
        let snapshot = Arc::new(Mutex::new(DiscordPresenceSnapshot::default()));
        let worker_snapshot = snapshot.clone();
        let application_id = application_id.to_owned();
        thread::Builder::new()
            .name("metrolist-discord-rpc".into())
            .spawn(move || run_worker(receiver, worker_snapshot, application_id))
            .map_err(|_| AppError::Discord("the background IPC worker could not start".into()))?;
        Ok(Self { sender, snapshot })
    }

    pub fn apply(
        &self,
        action: DiscordPresenceAction,
        observation: DiscordPlaybackObservation<'_>,
    ) -> Result<()> {
        let command = match action {
            DiscordPresenceAction::Update => {
                let song = observation.song.ok_or_else(|| {
                    AppError::Discord("a song is required for a presence update".into())
                })?;
                if song.video_id.trim().is_empty() || observation.tempo_milli == 0 {
                    return Err(AppError::Discord(
                        "the playback state is not valid for Discord".into(),
                    ));
                }
                Command::Update(DiscordPresence::from_observation(song, &observation))
            }
            DiscordPresenceAction::Clear => Command::Clear,
        };
        self.sender.try_send(command).map_err(|error| match error {
            TrySendError::Full(_) => {
                AppError::Discord("the presence update queue is temporarily busy".into())
            }
            TrySendError::Disconnected(_) => {
                AppError::Discord("the presence worker is unavailable".into())
            }
        })
    }

    pub fn snapshot(&self) -> DiscordPresenceSnapshot {
        self.snapshot
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }
}

impl Drop for DiscordPresenceService {
    fn drop(&mut self) {
        let _ = self.sender.try_send(Command::Shutdown);
    }
}

fn validate_application_id(application_id: &str) -> Result<()> {
    if !(17..=20).contains(&application_id.len())
        || !application_id.bytes().all(|byte| byte.is_ascii_digit())
    {
        return Err(AppError::Discord(
            "the Discord application ID is invalid".into(),
        ));
    }
    Ok(())
}

fn run_worker(
    receiver: Receiver<Command>,
    snapshot: Arc<Mutex<DiscordPresenceSnapshot>>,
    application_id: String,
) {
    let mut client = None;
    while let Ok(command) = receiver.recv() {
        match command {
            Command::Update(presence) => {
                set_snapshot(&snapshot, DiscordPresenceState::Connecting, None);
                if client.is_none() {
                    let mut candidate = DiscordIpcClient::new(&application_id);
                    if candidate.connect().is_err() {
                        set_snapshot(
                            &snapshot,
                            DiscordPresenceState::Failed,
                            Some("Discord desktop is not running or its local IPC is unavailable"),
                        );
                        continue;
                    }
                    client = Some(candidate);
                }
                let result = client
                    .as_mut()
                    .expect("Discord IPC client was initialized")
                    .set_activity(presence.into_activity());
                if result.is_ok() {
                    set_snapshot(&snapshot, DiscordPresenceState::Active, None);
                } else {
                    client = None;
                    set_snapshot(
                        &snapshot,
                        DiscordPresenceState::Failed,
                        Some("Discord rejected or lost the local presence connection"),
                    );
                }
            }
            Command::Clear => {
                if let Some(active) = client.as_mut()
                    && active.clear_activity().is_err()
                {
                    client = None;
                }
                set_snapshot(&snapshot, DiscordPresenceState::Idle, None);
            }
            Command::Shutdown => break,
        }
    }
    if let Some(active) = client.as_mut() {
        let _ = active.clear_activity();
        let _ = active.close();
    }
}

fn set_snapshot(
    snapshot: &Mutex<DiscordPresenceSnapshot>,
    state: DiscordPresenceState,
    error: Option<&str>,
) {
    let mut snapshot = snapshot
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    snapshot.state = state;
    snapshot.last_error = error.map(str::to_owned);
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::domain::ArtistCredit;

    fn song() -> Song {
        Song {
            video_id: "fixture-video".into(),
            title: "Fixture Song".into(),
            artists: vec![ArtistCredit {
                id: None,
                name: "Fixture Artist".into(),
            }],
            duration: Some(Duration::from_secs(240)),
            thumbnail_url: Some("https://example.invalid/cover.jpg".into()),
            album: None,
            is_episode: false,
        }
    }

    fn observation<'a>(
        song: &'a Song,
        state: DiscordPlaybackState,
    ) -> DiscordPlaybackObservation<'a> {
        DiscordPlaybackObservation {
            enabled: true,
            song: Some(song),
            state,
            position: Duration::from_secs(60),
            duration: song.duration,
            tempo_milli: 1_000,
            unix_seconds: 1_700_000_000,
        }
    }

    #[test]
    fn activity_matches_discord_rpc_shape_and_uses_second_timestamps() {
        let song = song();
        let activity = DiscordPresence::from_observation(
            &song,
            &observation(&song, DiscordPlaybackState::Playing),
        )
        .into_activity();
        assert_eq!(
            serde_json::to_value(activity).unwrap(),
            json!({
                "name": "Metrolist",
                "state": "Fixture Artist",
                "details": "Fixture Song",
                "timestamps": {"start": 1_699_999_940_i64, "end": 1_700_000_180_i64},
                "assets": {
                    "large_image": "https://example.invalid/cover.jpg",
                    "large_text": "Album artwork"
                },
                "buttons": [
                    {"label": "Listen on YouTube Music", "url": "https://music.youtube.com/watch?v=fixture-video"},
                    {"label": "Visit Metrolist", "url": METROLIST_URL}
                ],
                "type": 2
            })
        );
    }

    #[test]
    fn tracker_deduplicates_retries_and_clears_after_one_minute_paused() {
        let song = song();
        let mut tracker = DiscordPresenceTracker::default();
        let playing = observation(&song, DiscordPlaybackState::Playing);
        assert_eq!(
            tracker.observe(playing),
            Some(DiscordPresenceAction::Update)
        );
        assert_eq!(tracker.observe(playing), None);
        let retry = DiscordPlaybackObservation {
            unix_seconds: playing.unix_seconds + RETRY_SECONDS,
            ..playing
        };
        assert_eq!(tracker.observe(retry), Some(DiscordPresenceAction::Update));

        let paused = DiscordPlaybackObservation {
            state: DiscordPlaybackState::Paused,
            unix_seconds: retry.unix_seconds + 1,
            ..retry
        };
        assert_eq!(tracker.observe(paused), Some(DiscordPresenceAction::Update));
        assert_eq!(
            tracker.observe(DiscordPlaybackObservation {
                unix_seconds: paused.unix_seconds + PAUSE_TIMEOUT_SECONDS - 1,
                ..paused
            }),
            Some(DiscordPresenceAction::Update)
        );
        assert_eq!(
            tracker.observe(DiscordPlaybackObservation {
                unix_seconds: paused.unix_seconds + PAUSE_TIMEOUT_SECONDS,
                ..paused
            }),
            Some(DiscordPresenceAction::Clear)
        );
        assert_eq!(
            tracker.observe(DiscordPlaybackObservation {
                enabled: false,
                ..paused
            }),
            None
        );
    }

    #[test]
    fn paused_activity_omits_timestamps_and_text_is_bounded() {
        let mut song = song();
        song.title = "界".repeat(MAX_TEXT_CHARS + 10);
        let activity = DiscordPresence::from_observation(
            &song,
            &observation(&song, DiscordPlaybackState::Paused),
        )
        .into_activity();
        let value = serde_json::to_value(activity).unwrap();
        assert!(value.get("timestamps").is_none());
        assert_eq!(
            value["details"].as_str().unwrap().chars().count(),
            MAX_TEXT_CHARS
        );
    }
}
