use std::{
    collections::VecDeque,
    io::{ErrorKind, Read, Write},
    net::{IpAddr, TcpStream},
    sync::{
        Arc, Mutex,
        mpsc::{self, Receiver, SyncSender, TryRecvError, TrySendError},
    },
    thread,
    time::{Duration, Instant},
};

use flate2::{Compression, read::GzDecoder, write::GzEncoder};
use http_client::Url;
use prost::Message as ProstMessage;
use tungstenite::{
    Message, WebSocket, client::IntoClientRequest, protocol::WebSocketConfig,
    stream::MaybeTlsStream,
};
use zeroize::Zeroizing;

use crate::{AppError, Result, domain::Song};

pub const DEFAULT_LISTEN_TOGETHER_SERVER_URL: &str = "wss://metroserverx.meowery.eu/ws";

const COMMAND_CAPACITY: usize = 128;
const EVENT_CAPACITY: usize = 512;
const MAX_MESSAGE_BYTES: usize = 1024 * 1024;
const MAX_DECOMPRESSED_BYTES: usize = 4 * 1024 * 1024;
const MAX_QUEUE_ITEMS: usize = 2_000;
const MAX_TEXT_CHARS: usize = 512;
const COMPRESSION_THRESHOLD: usize = 100;
const SOCKET_POLL_INTERVAL: Duration = Duration::from_millis(100);
const PING_INTERVAL: Duration = Duration::from_secs(25);
const INITIAL_PING_INTERVAL: Duration = Duration::from_millis(250);
const SESSION_GRACE_PERIOD: Duration = Duration::from_secs(10 * 60);
const INITIAL_RECONNECT_DELAY: Duration = Duration::from_secs(1);
const MAX_RECONNECT_DELAY: Duration = Duration::from_secs(120);
const MAX_RECONNECT_ATTEMPTS: u8 = 15;
const EPISODE_THUMBNAIL_FRAGMENT_TOKEN: &str = "metrolist_media=episode";

mod message_type {
    pub const CREATE_ROOM: &str = "create_room";
    pub const JOIN_ROOM: &str = "join_room";
    pub const LEAVE_ROOM: &str = "leave_room";
    pub const APPROVE_JOIN: &str = "approve_join";
    pub const REJECT_JOIN: &str = "reject_join";
    pub const PLAYBACK_ACTION: &str = "playback_action";
    pub const BUFFER_READY: &str = "buffer_ready";
    pub const KICK_USER: &str = "kick_user";
    pub const TRANSFER_HOST: &str = "transfer_host";
    pub const PING: &str = "ping";
    pub const REQUEST_SYNC: &str = "request_sync";
    pub const RECONNECT: &str = "reconnect";
    pub const SUGGEST_TRACK: &str = "suggest_track";
    pub const APPROVE_SUGGESTION: &str = "approve_suggestion";
    pub const REJECT_SUGGESTION: &str = "reject_suggestion";

    pub const ROOM_CREATED: &str = "room_created";
    pub const JOIN_REQUEST: &str = "join_request";
    pub const JOIN_APPROVED: &str = "join_approved";
    pub const JOIN_REJECTED: &str = "join_rejected";
    pub const USER_JOINED: &str = "user_joined";
    pub const USER_LEFT: &str = "user_left";
    pub const SYNC_PLAYBACK: &str = "sync_playback";
    pub const BUFFER_WAIT: &str = "buffer_wait";
    pub const BUFFER_COMPLETE: &str = "buffer_complete";
    pub const ERROR: &str = "error";
    pub const PONG: &str = "pong";
    pub const HOST_CHANGED: &str = "host_changed";
    pub const KICKED: &str = "kicked";
    pub const SYNC_STATE: &str = "sync_state";
    pub const RECONNECTED: &str = "reconnected";
    pub const USER_RECONNECTED: &str = "user_reconnected";
    pub const USER_DISCONNECTED: &str = "user_disconnected";
    pub const SUGGESTION_RECEIVED: &str = "suggestion_received";
    pub const SUGGESTION_APPROVED: &str = "suggestion_approved";
    pub const SUGGESTION_REJECTED: &str = "suggestion_rejected";
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ListenTogetherConnectionState {
    #[default]
    Disconnected,
    Connecting,
    Connected,
    Reconnecting {
        attempt: u8,
        max_attempts: u8,
    },
    Error,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ListenTogetherRoomRole {
    Host,
    Guest,
    #[default]
    None,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListenTogetherTrack {
    pub id: String,
    pub title: String,
    pub artist: String,
    pub album: Option<String>,
    pub duration_ms: i64,
    pub thumbnail: Option<String>,
    pub suggested_by: Option<String>,
    pub is_episode: bool,
}

impl ListenTogetherTrack {
    pub fn from_song(song: &Song) -> Self {
        Self {
            id: song.video_id.clone(),
            title: song.title.clone(),
            artist: song.artist_line(),
            album: None,
            duration_ms: song
                .duration
                .unwrap_or(Duration::from_secs(180))
                .as_millis()
                .min(i64::MAX as u128) as i64,
            thumbnail: song.thumbnail_url.clone(),
            suggested_by: None,
            is_episode: song.is_episode,
        }
    }

    pub fn to_song(&self) -> Song {
        Song {
            video_id: self.id.clone(),
            title: self.title.clone(),
            artists: self
                .artist
                .split(',')
                .map(str::trim)
                .filter(|artist| !artist.is_empty())
                .map(|name| crate::domain::ArtistCredit {
                    id: None,
                    name: name.to_owned(),
                })
                .collect(),
            duration: (self.duration_ms > 0)
                .then(|| Duration::from_millis(self.duration_ms as u64)),
            thumbnail_url: self.thumbnail.clone(),
            album: None,
            is_episode: self.is_episode,
        }
    }

    fn validate(&self) -> Result<()> {
        validate_identifier("track ID", &self.id, 256)?;
        validate_text("track title", &self.title, MAX_TEXT_CHARS, false)?;
        validate_text("track artist", &self.artist, MAX_TEXT_CHARS, false)?;
        if let Some(album) = self.album.as_deref() {
            validate_text("track album", album, MAX_TEXT_CHARS, true)?;
        }
        if self.duration_ms < 0 || self.duration_ms > 7 * 24 * 60 * 60 * 1_000 {
            return Err(protocol_error(
                "track duration is outside the supported range",
            ));
        }
        if let Some(thumbnail) = self.thumbnail.as_deref() {
            validate_public_url("track thumbnail", thumbnail)?;
        }
        if let Some(suggested_by) = self.suggested_by.as_deref() {
            validate_text("suggested-by name", suggested_by, 128, false)?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListenTogetherUser {
    pub user_id: String,
    pub username: String,
    pub is_host: bool,
    pub is_connected: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ListenTogetherRoomState {
    pub room_code: String,
    pub host_id: String,
    pub users: Vec<ListenTogetherUser>,
    pub current_track: Option<ListenTogetherTrack>,
    pub is_playing: bool,
    pub position_ms: i64,
    pub last_update_ms: i64,
    pub volume: f32,
    /// The server protocol stores the upcoming queue, excluding `current_track`.
    pub queue: Vec<ListenTogetherTrack>,
    pub revision: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListenTogetherJoinRequest {
    pub user_id: String,
    pub username: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListenTogetherSuggestion {
    pub suggestion_id: String,
    pub from_user_id: String,
    pub from_username: String,
    pub track: ListenTogetherTrack,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ListenTogetherSnapshot {
    pub connection: ListenTogetherConnectionState,
    pub role: ListenTogetherRoomRole,
    pub user_id: Option<String>,
    pub room: Option<ListenTogetherRoomState>,
    pub pending_join_requests: Vec<ListenTogetherJoinRequest>,
    pub buffering_users: Vec<String>,
    pub pending_suggestions: Vec<ListenTogetherSuggestion>,
    pub last_error: Option<String>,
}

impl Default for ListenTogetherSnapshot {
    fn default() -> Self {
        Self {
            connection: ListenTogetherConnectionState::Disconnected,
            role: ListenTogetherRoomRole::None,
            user_id: None,
            room: None,
            pending_join_requests: Vec::new(),
            buffering_users: Vec::new(),
            pending_suggestions: Vec::new(),
            last_error: None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ListenTogetherPlaybackAction {
    Play,
    Pause,
    Seek,
    SkipNext,
    SkipPrevious,
    ChangeTrack,
    QueueAdd,
    QueueRemove,
    QueueClear,
    SyncQueue,
    SetVolume,
}

impl ListenTogetherPlaybackAction {
    fn as_str(self) -> &'static str {
        match self {
            Self::Play => "play",
            Self::Pause => "pause",
            Self::Seek => "seek",
            Self::SkipNext => "skip_next",
            Self::SkipPrevious => "skip_prev",
            Self::ChangeTrack => "change_track",
            Self::QueueAdd => "queue_add",
            Self::QueueRemove => "queue_remove",
            Self::QueueClear => "queue_clear",
            Self::SyncQueue => "sync_queue",
            Self::SetVolume => "set_volume",
        }
    }

    fn from_str(value: &str) -> Result<Self> {
        match value {
            "play" => Ok(Self::Play),
            "pause" => Ok(Self::Pause),
            "seek" => Ok(Self::Seek),
            "skip_next" => Ok(Self::SkipNext),
            "skip_prev" => Ok(Self::SkipPrevious),
            "change_track" => Ok(Self::ChangeTrack),
            "queue_add" => Ok(Self::QueueAdd),
            "queue_remove" => Ok(Self::QueueRemove),
            "queue_clear" => Ok(Self::QueueClear),
            "sync_queue" => Ok(Self::SyncQueue),
            "set_volume" => Ok(Self::SetVolume),
            _ => Err(protocol_error("server sent an unknown playback action")),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ListenTogetherPlaybackActionPayload {
    pub action: ListenTogetherPlaybackAction,
    pub track_id: Option<String>,
    pub position_ms: Option<i64>,
    pub track: Option<ListenTogetherTrack>,
    pub insert_next: Option<bool>,
    pub queue: Option<Vec<ListenTogetherTrack>>,
    pub queue_title: Option<String>,
    pub volume: Option<f32>,
    pub server_time_ms: Option<i64>,
    pub revision: u64,
    pub captured_at_server_time_ms: Option<i64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ListenTogetherLocalPlaybackState {
    Inactive,
    Loading,
    Playing,
    Paused,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ListenTogetherPlaybackObservation {
    pub is_host: bool,
    pub current_track: Option<ListenTogetherTrack>,
    pub upcoming_queue: Vec<ListenTogetherTrack>,
    pub state: ListenTogetherLocalPlaybackState,
    pub position_ms: i64,
    pub volume: f32,
    pub sync_volume: bool,
    pub tempo_milli: u16,
}

pub struct ListenTogetherPlaybackTracker {
    last_track: Option<ListenTogetherTrack>,
    last_queue: Vec<ListenTogetherTrack>,
    last_state: ListenTogetherLocalPlaybackState,
    last_position_ms: i64,
    last_observed_at: Option<Instant>,
    last_heartbeat_at: Option<Instant>,
    last_volume: Option<f32>,
}

impl Default for ListenTogetherPlaybackTracker {
    fn default() -> Self {
        Self {
            last_track: None,
            last_queue: Vec::new(),
            last_state: ListenTogetherLocalPlaybackState::Inactive,
            last_position_ms: 0,
            last_observed_at: None,
            last_heartbeat_at: None,
            last_volume: None,
        }
    }
}

impl ListenTogetherPlaybackTracker {
    pub fn reset(&mut self) {
        *self = Self::default();
    }

    pub fn observe(
        &mut self,
        observation: ListenTogetherPlaybackObservation,
        now: Instant,
    ) -> Vec<ListenTogetherPlaybackActionPayload> {
        if !observation.is_host {
            self.reset();
            return Vec::new();
        }

        let current_changed = self.last_track != observation.current_track;
        let queue_changed = self.last_queue != observation.upcoming_queue;
        let state_changed = self.last_state != observation.state;
        let mut actions = Vec::new();

        if current_changed {
            if let Some(track) = observation.current_track.clone() {
                let mut change = ListenTogetherPlaybackActionPayload::new(
                    ListenTogetherPlaybackAction::ChangeTrack,
                );
                change.track_id = Some(track.id.clone());
                change.track = Some(track);
                change.queue = Some(observation.upcoming_queue.clone());
                change.queue_title = Some("Listen Together".into());
                actions.push(change);
                if observation.state == ListenTogetherLocalPlaybackState::Playing {
                    actions.push(position_action(
                        ListenTogetherPlaybackAction::Play,
                        observation.current_track.as_ref(),
                        observation.position_ms,
                    ));
                }
            } else if self.last_track.is_some() {
                let mut clear = ListenTogetherPlaybackActionPayload::new(
                    ListenTogetherPlaybackAction::QueueClear,
                );
                clear.queue = Some(Vec::new());
                actions.push(clear);
            }
        } else {
            if queue_changed {
                let mut sync = ListenTogetherPlaybackActionPayload::new(
                    ListenTogetherPlaybackAction::SyncQueue,
                );
                sync.queue = Some(observation.upcoming_queue.clone());
                sync.queue_title = Some("Listen Together".into());
                actions.push(sync);
            }

            if state_changed && observation.current_track.is_some() {
                match observation.state {
                    ListenTogetherLocalPlaybackState::Playing => actions.push(position_action(
                        ListenTogetherPlaybackAction::Play,
                        observation.current_track.as_ref(),
                        observation.position_ms,
                    )),
                    ListenTogetherLocalPlaybackState::Paused => actions.push(position_action(
                        ListenTogetherPlaybackAction::Pause,
                        observation.current_track.as_ref(),
                        observation.position_ms,
                    )),
                    ListenTogetherLocalPlaybackState::Inactive
                    | ListenTogetherLocalPlaybackState::Loading => {}
                }
            } else if matches!(
                observation.state,
                ListenTogetherLocalPlaybackState::Playing
                    | ListenTogetherLocalPlaybackState::Paused
            ) && observation.current_track.is_some()
            {
                let elapsed_ms = self
                    .last_observed_at
                    .map_or(0, |last| now.saturating_duration_since(last).as_millis())
                    .min(i64::MAX as u128) as i64;
                let expected = if self.last_state == ListenTogetherLocalPlaybackState::Playing {
                    self.last_position_ms.saturating_add(
                        elapsed_ms.saturating_mul(i64::from(observation.tempo_milli)) / 1_000,
                    )
                } else {
                    self.last_position_ms
                };
                let threshold = if observation.state == ListenTogetherLocalPlaybackState::Paused {
                    50
                } else {
                    750
                };
                if (observation.position_ms - expected).abs() > threshold {
                    actions.push(position_action(
                        ListenTogetherPlaybackAction::Seek,
                        observation.current_track.as_ref(),
                        observation.position_ms,
                    ));
                } else if observation.state == ListenTogetherLocalPlaybackState::Playing
                    && self.last_heartbeat_at.is_none_or(|last| {
                        now.saturating_duration_since(last) >= Duration::from_secs(4)
                    })
                {
                    actions.push(position_action(
                        ListenTogetherPlaybackAction::Play,
                        observation.current_track.as_ref(),
                        observation.position_ms,
                    ));
                }
            }
        }

        if observation.sync_volume
            && observation.volume.is_finite()
            && self
                .last_volume
                .is_none_or(|last| (last - observation.volume).abs() >= 0.01)
        {
            let mut volume =
                ListenTogetherPlaybackActionPayload::new(ListenTogetherPlaybackAction::SetVolume);
            volume.volume = Some(observation.volume.clamp(0.0, 1.0));
            actions.push(volume);
            self.last_volume = Some(observation.volume.clamp(0.0, 1.0));
        }

        if actions
            .iter()
            .any(|action| action.action == ListenTogetherPlaybackAction::Play)
        {
            self.last_heartbeat_at = Some(now);
        }
        self.last_track = observation.current_track;
        self.last_queue = observation.upcoming_queue;
        self.last_state = observation.state;
        self.last_position_ms = observation.position_ms;
        self.last_observed_at = Some(now);
        actions
    }
}

fn position_action(
    action: ListenTogetherPlaybackAction,
    track: Option<&ListenTogetherTrack>,
    position_ms: i64,
) -> ListenTogetherPlaybackActionPayload {
    let mut payload = ListenTogetherPlaybackActionPayload::new(action);
    payload.track_id = track.map(|track| track.id.clone());
    payload.position_ms = Some(position_ms.max(0));
    payload
}

impl ListenTogetherPlaybackActionPayload {
    pub fn new(action: ListenTogetherPlaybackAction) -> Self {
        Self {
            action,
            track_id: None,
            position_ms: None,
            track: None,
            insert_next: None,
            queue: None,
            queue_title: None,
            volume: None,
            server_time_ms: None,
            revision: 0,
            captured_at_server_time_ms: None,
        }
    }

    fn validate(&self) -> Result<()> {
        if let Some(track_id) = self.track_id.as_deref() {
            validate_identifier("track ID", track_id, 256)?;
        }
        if let Some(position) = self.position_ms
            && position < 0
        {
            return Err(protocol_error("playback position cannot be negative"));
        }
        if let Some(track) = self.track.as_ref() {
            track.validate()?;
        }
        if let Some(queue) = self.queue.as_ref() {
            validate_track_queue(queue)?;
        }
        if let Some(title) = self.queue_title.as_deref() {
            validate_text("queue title", title, MAX_TEXT_CHARS, true)?;
        }
        if let Some(volume) = self.volume
            && (!volume.is_finite() || !(0.0..=1.0).contains(&volume))
        {
            return Err(protocol_error("playback volume must be between 0 and 1"));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ListenTogetherSyncState {
    pub current_track: Option<ListenTogetherTrack>,
    pub is_playing: bool,
    pub position_ms: i64,
    pub last_update_ms: i64,
    pub queue: Vec<ListenTogetherTrack>,
    pub volume: f32,
    pub revision: u64,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ListenTogetherEvent {
    RoomCreated {
        room_code: String,
    },
    JoinRequest(ListenTogetherJoinRequest),
    JoinApproved(ListenTogetherRoomState),
    JoinRejected {
        reason: String,
    },
    UserJoined(ListenTogetherUser),
    UserLeft {
        user_id: String,
        username: String,
    },
    PlaybackSync(ListenTogetherPlaybackActionPayload),
    BufferWait {
        track_id: String,
        waiting_for: Vec<String>,
    },
    BufferComplete {
        track_id: String,
    },
    SyncState(ListenTogetherSyncState),
    HostChanged {
        new_host_id: String,
        new_host_name: String,
    },
    Kicked {
        reason: String,
    },
    Reconnected(ListenTogetherRoomState),
    UserReconnected {
        user_id: String,
        username: String,
    },
    UserDisconnected {
        user_id: String,
        username: String,
    },
    SuggestionReceived(ListenTogetherSuggestion),
    SuggestionApproved {
        suggestion_id: String,
        track: ListenTogetherTrack,
    },
    SuggestionRejected {
        suggestion_id: String,
        reason: Option<String>,
    },
    ServerError {
        code: String,
        message: String,
    },
    ConnectionError {
        message: String,
    },
    Disconnected,
}

struct SharedState {
    snapshot: Mutex<ListenTogetherSnapshot>,
    events: Mutex<VecDeque<ListenTogetherEvent>>,
    clock: Mutex<ServerClock>,
}

impl Default for SharedState {
    fn default() -> Self {
        Self {
            snapshot: Mutex::new(ListenTogetherSnapshot::default()),
            events: Mutex::new(VecDeque::new()),
            clock: Mutex::new(ServerClock::default()),
        }
    }
}

pub struct ListenTogetherClient {
    commands: SyncSender<WorkerCommand>,
    shared: Arc<SharedState>,
}

impl ListenTogetherClient {
    pub fn new(server_url: impl Into<String>) -> Result<Self> {
        let server_url = normalize_server_url(server_url.into())?;
        let (commands, receiver) = mpsc::sync_channel(COMMAND_CAPACITY);
        let shared = Arc::new(SharedState::default());
        let worker_shared = shared.clone();
        thread::Builder::new()
            .name("metrolist-listen-together".into())
            .spawn(move || Worker::new(server_url, receiver, worker_shared).run())
            .map_err(|error| {
                AppError::ListenTogether(format!("could not start the connection worker: {error}"))
            })?;
        Ok(Self { commands, shared })
    }

    pub fn snapshot(&self) -> ListenTogetherSnapshot {
        lock_unpoisoned(&self.shared.snapshot).clone()
    }

    pub fn drain_events(&self) -> Vec<ListenTogetherEvent> {
        lock_unpoisoned(&self.shared.events).drain(..).collect()
    }

    pub fn connect(&self) -> Result<()> {
        self.enqueue(WorkerCommand::Connect)
    }

    pub fn disconnect(&self) -> Result<()> {
        self.enqueue(WorkerCommand::Disconnect)
    }

    pub fn force_reconnect(&self) -> Result<()> {
        self.enqueue(WorkerCommand::ForceReconnect)
    }

    pub fn create_room(&self, username: impl Into<String>) -> Result<()> {
        let username = normalize_username(username.into())?;
        self.enqueue(WorkerCommand::CreateRoom { username })
    }

    pub fn join_room(
        &self,
        room_code: impl Into<String>,
        username: impl Into<String>,
    ) -> Result<()> {
        let room_code = normalize_room_code(room_code.into())?;
        let username = normalize_username(username.into())?;
        self.enqueue(WorkerCommand::JoinRoom {
            room_code,
            username,
        })
    }

    pub fn leave_room(&self) -> Result<()> {
        self.enqueue(WorkerCommand::LeaveRoom)
    }

    pub fn approve_join(&self, user_id: impl Into<String>) -> Result<()> {
        self.require_host()?;
        let user_id = normalize_identifier("user ID", user_id.into(), 256)?;
        self.enqueue(WorkerCommand::ApproveJoin { user_id })
    }

    pub fn reject_join(&self, user_id: impl Into<String>, reason: Option<String>) -> Result<()> {
        self.require_host()?;
        let user_id = normalize_identifier("user ID", user_id.into(), 256)?;
        let reason = normalize_optional_text("rejection reason", reason, MAX_TEXT_CHARS)?;
        self.enqueue(WorkerCommand::RejectJoin { user_id, reason })
    }

    pub fn kick_user(&self, user_id: impl Into<String>, reason: Option<String>) -> Result<()> {
        self.require_host()?;
        let user_id = normalize_identifier("user ID", user_id.into(), 256)?;
        let reason = normalize_optional_text("kick reason", reason, MAX_TEXT_CHARS)?;
        self.enqueue(WorkerCommand::KickUser { user_id, reason })
    }

    pub fn transfer_host(&self, user_id: impl Into<String>) -> Result<()> {
        self.require_host()?;
        let user_id = normalize_identifier("user ID", user_id.into(), 256)?;
        self.enqueue(WorkerCommand::TransferHost { user_id })
    }

    pub fn send_playback_action(
        &self,
        mut action: ListenTogetherPlaybackActionPayload,
    ) -> Result<()> {
        self.require_host()?;
        action.validate()?;
        if action.position_ms.is_some()
            && matches!(
                action.action,
                ListenTogetherPlaybackAction::Play
                    | ListenTogetherPlaybackAction::Pause
                    | ListenTogetherPlaybackAction::Seek
            )
        {
            action.captured_at_server_time_ms = self.server_time_now_ms();
        }
        self.enqueue(WorkerCommand::Playback(action))
    }

    pub fn send_buffer_ready(&self, track_id: impl Into<String>) -> Result<()> {
        let track_id = normalize_identifier("track ID", track_id.into(), 256)?;
        self.enqueue(WorkerCommand::BufferReady { track_id })
    }

    pub fn request_sync(&self) -> Result<()> {
        if self.snapshot().room.is_none() {
            return Err(AppError::ListenTogether(
                "cannot request playback sync before joining a room".into(),
            ));
        }
        self.enqueue(WorkerCommand::RequestSync)
    }

    pub fn suggest_track(&self, track: ListenTogetherTrack) -> Result<()> {
        if self.snapshot().role != ListenTogetherRoomRole::Guest {
            return Err(AppError::ListenTogether(
                "only room guests can suggest a track".into(),
            ));
        }
        track.validate()?;
        self.enqueue(WorkerCommand::SuggestTrack { track })
    }

    pub fn approve_suggestion(&self, suggestion_id: impl Into<String>) -> Result<()> {
        self.require_host()?;
        let suggestion_id = normalize_identifier("suggestion ID", suggestion_id.into(), 256)?;
        self.enqueue(WorkerCommand::ApproveSuggestion { suggestion_id })
    }

    pub fn reject_suggestion(
        &self,
        suggestion_id: impl Into<String>,
        reason: Option<String>,
    ) -> Result<()> {
        self.require_host()?;
        let suggestion_id = normalize_identifier("suggestion ID", suggestion_id.into(), 256)?;
        let reason = normalize_optional_text("rejection reason", reason, MAX_TEXT_CHARS)?;
        self.enqueue(WorkerCommand::RejectSuggestion {
            suggestion_id,
            reason,
        })
    }

    pub fn server_time_now_ms(&self) -> Option<i64> {
        lock_unpoisoned(&self.shared.clock).now_ms()
    }

    pub fn position_at_server_time(
        &self,
        position_ms: i64,
        effective_at_server_time_ms: Option<i64>,
        is_playing: bool,
    ) -> i64 {
        lock_unpoisoned(&self.shared.clock).position_at(
            position_ms,
            effective_at_server_time_ms,
            is_playing,
        )
    }

    fn require_host(&self) -> Result<()> {
        if self.snapshot().role != ListenTogetherRoomRole::Host {
            return Err(AppError::ListenTogether(
                "only the room host can perform this action".into(),
            ));
        }
        Ok(())
    }

    fn enqueue(&self, command: WorkerCommand) -> Result<()> {
        self.commands
            .try_send(command)
            .map_err(|error| match error {
                TrySendError::Full(_) => {
                    AppError::ListenTogether("the connection command queue is busy".into())
                }
                TrySendError::Disconnected(_) => {
                    AppError::ListenTogether("the connection worker has stopped".into())
                }
            })
    }
}

impl Drop for ListenTogetherClient {
    fn drop(&mut self) {
        let _ = self.commands.try_send(WorkerCommand::Shutdown);
    }
}

enum PendingRoomAction {
    Create { username: String },
    Join { room_code: String, username: String },
}

enum WorkerCommand {
    Connect,
    Disconnect,
    ForceReconnect,
    CreateRoom {
        username: String,
    },
    JoinRoom {
        room_code: String,
        username: String,
    },
    LeaveRoom,
    ApproveJoin {
        user_id: String,
    },
    RejectJoin {
        user_id: String,
        reason: Option<String>,
    },
    KickUser {
        user_id: String,
        reason: Option<String>,
    },
    TransferHost {
        user_id: String,
    },
    Playback(ListenTogetherPlaybackActionPayload),
    BufferReady {
        track_id: String,
    },
    RequestSync,
    SuggestTrack {
        track: ListenTogetherTrack,
    },
    ApproveSuggestion {
        suggestion_id: String,
    },
    RejectSuggestion {
        suggestion_id: String,
        reason: Option<String>,
    },
    Shutdown,
}

type Socket = WebSocket<MaybeTlsStream<TcpStream>>;

struct Worker {
    server_url: String,
    receiver: Receiver<WorkerCommand>,
    shared: Arc<SharedState>,
    socket: Option<Socket>,
    pending_room_action: Option<PendingRoomAction>,
    connect_requested: bool,
    reconnect_attempt: u8,
    reconnect_at: Option<Instant>,
    next_ping_at: Option<Instant>,
    initial_pings_remaining: u8,
    ping_sequence: u64,
    last_revision: u64,
    session_token: Option<Zeroizing<String>>,
    session_received_at: Option<Instant>,
    stored_room_code: Option<String>,
    stored_username: Option<String>,
    was_host: bool,
    shutting_down: bool,
}

impl Worker {
    fn new(
        server_url: String,
        receiver: Receiver<WorkerCommand>,
        shared: Arc<SharedState>,
    ) -> Self {
        Self {
            server_url,
            receiver,
            shared,
            socket: None,
            pending_room_action: None,
            connect_requested: false,
            reconnect_attempt: 0,
            reconnect_at: None,
            next_ping_at: None,
            initial_pings_remaining: 0,
            ping_sequence: 0,
            last_revision: 0,
            session_token: None,
            session_received_at: None,
            stored_room_code: None,
            stored_username: None,
            was_host: false,
            shutting_down: false,
        }
    }

    fn run(mut self) {
        while !self.shutting_down {
            loop {
                match self.receiver.try_recv() {
                    Ok(command) => self.handle_command(command),
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => {
                        self.shutting_down = true;
                        break;
                    }
                }
                if self.shutting_down {
                    break;
                }
            }
            if self.shutting_down {
                break;
            }

            if self.socket.is_none()
                && self.connect_requested
                && self
                    .reconnect_at
                    .is_none_or(|deadline| Instant::now() >= deadline)
            {
                self.establish_connection();
                continue;
            }

            if self.socket.is_some() {
                self.maybe_send_ping();
                self.read_once();
            } else {
                match self.receiver.recv_timeout(SOCKET_POLL_INTERVAL) {
                    Ok(command) => self.handle_command(command),
                    Err(mpsc::RecvTimeoutError::Timeout) => {}
                    Err(mpsc::RecvTimeoutError::Disconnected) => self.shutting_down = true,
                }
            }
        }

        if let Some(mut socket) = self.socket.take() {
            let _ = socket.close(None);
        }
        self.clear_session_and_room();
        self.set_connection(ListenTogetherConnectionState::Disconnected, None);
    }

    fn handle_command(&mut self, command: WorkerCommand) {
        match command {
            WorkerCommand::Connect => {
                self.connect_requested = true;
                self.reconnect_attempt = 0;
                self.reconnect_at = Some(Instant::now());
            }
            WorkerCommand::Disconnect => self.explicit_disconnect(),
            WorkerCommand::ForceReconnect => {
                if let Some(mut socket) = self.socket.take() {
                    let _ = socket.close(None);
                }
                self.connect_requested = true;
                self.reconnect_attempt = 0;
                self.reconnect_at = Some(Instant::now());
                self.set_connection(ListenTogetherConnectionState::Disconnected, None);
                lock_unpoisoned(&self.shared.clock).reset();
            }
            WorkerCommand::CreateRoom { username } => {
                self.clear_session_and_room();
                self.stored_username = Some(username.clone());
                self.pending_room_action = Some(PendingRoomAction::Create { username });
                self.connect_requested = true;
                self.reconnect_attempt = 0;
                self.reconnect_at = Some(Instant::now());
                if self.socket.is_some() {
                    self.execute_pending_room_action();
                }
            }
            WorkerCommand::JoinRoom {
                room_code,
                username,
            } => {
                self.clear_session_and_room();
                self.stored_username = Some(username.clone());
                self.pending_room_action = Some(PendingRoomAction::Join {
                    room_code,
                    username,
                });
                self.connect_requested = true;
                self.reconnect_attempt = 0;
                self.reconnect_at = Some(Instant::now());
                if self.socket.is_some() {
                    self.execute_pending_room_action();
                }
            }
            WorkerCommand::LeaveRoom => {
                let _ = self.send_empty(message_type::LEAVE_ROOM);
                self.clear_session_and_room();
            }
            WorkerCommand::ApproveJoin { user_id } => {
                if self
                    .send_proto(
                        message_type::APPROVE_JOIN,
                        proto::ApproveJoinPayload {
                            user_id: user_id.clone(),
                        },
                    )
                    .is_ok()
                {
                    lock_unpoisoned(&self.shared.snapshot)
                        .pending_join_requests
                        .retain(|request| request.user_id != user_id);
                }
            }
            WorkerCommand::RejectJoin { user_id, reason } => {
                if self
                    .send_proto(
                        message_type::REJECT_JOIN,
                        proto::RejectJoinPayload {
                            user_id: user_id.clone(),
                            reason: reason.unwrap_or_default(),
                        },
                    )
                    .is_ok()
                {
                    lock_unpoisoned(&self.shared.snapshot)
                        .pending_join_requests
                        .retain(|request| request.user_id != user_id);
                }
            }
            WorkerCommand::KickUser { user_id, reason } => {
                let _ = self.send_proto(
                    message_type::KICK_USER,
                    proto::KickUserPayload {
                        user_id,
                        reason: reason.unwrap_or_default(),
                    },
                );
            }
            WorkerCommand::TransferHost { user_id } => {
                let _ = self.send_proto(
                    message_type::TRANSFER_HOST,
                    proto::TransferHostPayload {
                        new_host_id: user_id,
                    },
                );
            }
            WorkerCommand::Playback(action) => {
                let _ = self.send_proto(message_type::PLAYBACK_ACTION, action_to_proto(action));
            }
            WorkerCommand::BufferReady { track_id } => {
                let _ = self.send_proto(
                    message_type::BUFFER_READY,
                    proto::BufferReadyPayload { track_id },
                );
            }
            WorkerCommand::RequestSync => {
                let _ = self.send_empty(message_type::REQUEST_SYNC);
            }
            WorkerCommand::SuggestTrack { track } => {
                let _ = self.send_proto(
                    message_type::SUGGEST_TRACK,
                    proto::SuggestTrackPayload {
                        track_info: Some(track_to_proto(track)),
                    },
                );
            }
            WorkerCommand::ApproveSuggestion { suggestion_id } => {
                if self
                    .send_proto(
                        message_type::APPROVE_SUGGESTION,
                        proto::ApproveSuggestionPayload {
                            suggestion_id: suggestion_id.clone(),
                        },
                    )
                    .is_ok()
                {
                    lock_unpoisoned(&self.shared.snapshot)
                        .pending_suggestions
                        .retain(|suggestion| suggestion.suggestion_id != suggestion_id);
                }
            }
            WorkerCommand::RejectSuggestion {
                suggestion_id,
                reason,
            } => {
                if self
                    .send_proto(
                        message_type::REJECT_SUGGESTION,
                        proto::RejectSuggestionPayload {
                            suggestion_id: suggestion_id.clone(),
                            reason: reason.unwrap_or_default(),
                        },
                    )
                    .is_ok()
                {
                    lock_unpoisoned(&self.shared.snapshot)
                        .pending_suggestions
                        .retain(|suggestion| suggestion.suggestion_id != suggestion_id);
                }
            }
            WorkerCommand::Shutdown => self.shutting_down = true,
        }
    }

    fn establish_connection(&mut self) {
        self.reconnect_at = None;
        let state = if self.reconnect_attempt == 0 {
            ListenTogetherConnectionState::Connecting
        } else {
            ListenTogetherConnectionState::Reconnecting {
                attempt: self.reconnect_attempt,
                max_attempts: MAX_RECONNECT_ATTEMPTS,
            }
        };
        self.set_connection(state, None);

        let request = match self.server_url.as_str().into_client_request() {
            Ok(mut request) => {
                request.headers_mut().insert(
                    tungstenite::http::header::USER_AGENT,
                    tungstenite::http::HeaderValue::from_static("Metrolist-rs/0.1"),
                );
                request
            }
            Err(error) => {
                self.fail_connection(format!("invalid WebSocket request: {error}"));
                return;
            }
        };
        let config = WebSocketConfig::default()
            .read_buffer_size(16 * 1024)
            .write_buffer_size(0)
            .max_write_buffer_size(MAX_MESSAGE_BYTES * 2)
            .max_message_size(Some(MAX_MESSAGE_BYTES))
            .max_frame_size(Some(MAX_MESSAGE_BYTES));

        match tungstenite::client::connect_with_config(request, Some(config), 3) {
            Ok((mut socket, _)) => {
                if let Err(error) = configure_socket(&mut socket) {
                    self.fail_connection(format!("could not configure WebSocket: {error}"));
                    return;
                }
                self.socket = Some(socket);
                self.reconnect_attempt = 0;
                self.set_connection(ListenTogetherConnectionState::Connected, None);
                self.initial_pings_remaining = 3;
                self.next_ping_at = Some(Instant::now());
                lock_unpoisoned(&self.shared.clock).reset();

                if self.session_is_fresh() {
                    if let Some(token) = self.session_token.as_ref() {
                        let token = token.to_string();
                        let _ = self.send_proto(
                            message_type::RECONNECT,
                            proto::ReconnectPayload {
                                session_token: token,
                            },
                        );
                    }
                } else {
                    self.session_token = None;
                    self.session_received_at = None;
                    self.execute_pending_room_action();
                }
            }
            Err(error) => self.fail_connection(sanitize_transport_error(&error)),
        }
    }

    fn execute_pending_room_action(&mut self) {
        let Some(action) = self.pending_room_action.take() else {
            return;
        };
        let result = match action {
            PendingRoomAction::Create { username } => self.send_proto(
                message_type::CREATE_ROOM,
                proto::CreateRoomPayload { username },
            ),
            PendingRoomAction::Join {
                room_code,
                username,
            } => self.send_proto(
                message_type::JOIN_ROOM,
                proto::JoinRoomPayload {
                    room_code,
                    username,
                },
            ),
        };
        if let Err(error) = result {
            self.push_event(ListenTogetherEvent::ConnectionError {
                message: error.to_string(),
            });
        }
    }

    fn maybe_send_ping(&mut self) {
        if self
            .next_ping_at
            .is_none_or(|deadline| Instant::now() < deadline)
        {
            return;
        }
        self.ping_sequence = self.ping_sequence.wrapping_add(1);
        let payload = proto::PingPayload {
            client_time: lock_unpoisoned(&self.shared.clock).elapsed_ms(),
            sequence: self.ping_sequence,
        };
        if self.send_proto(message_type::PING, payload).is_err() {
            return;
        }
        if self.initial_pings_remaining > 0 {
            self.initial_pings_remaining -= 1;
        }
        self.next_ping_at = Some(
            Instant::now()
                + if self.initial_pings_remaining > 0 {
                    INITIAL_PING_INTERVAL
                } else {
                    PING_INTERVAL
                },
        );
    }

    fn read_once(&mut self) {
        let result = self.socket.as_mut().expect("socket was checked").read();
        match result {
            Ok(Message::Binary(bytes)) => {
                if let Err(error) = self.handle_binary(&bytes) {
                    let message = error.to_string();
                    self.set_last_error(Some(message.clone()));
                    self.push_event(ListenTogetherEvent::ServerError {
                        code: "invalid_message".into(),
                        message,
                    });
                }
            }
            Ok(Message::Close(_)) => self.connection_lost("server closed the connection".into()),
            Ok(Message::Ping(_) | Message::Pong(_)) => {
                if let Some(socket) = self.socket.as_mut() {
                    let _ = socket.flush();
                }
            }
            Ok(Message::Text(_) | Message::Frame(_)) => {}
            Err(tungstenite::Error::Io(error))
                if matches!(error.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {}
            Err(tungstenite::Error::ConnectionClosed | tungstenite::Error::AlreadyClosed) => {
                self.connection_lost("connection closed".into())
            }
            Err(error) => self.connection_lost(sanitize_transport_error(&error)),
        }
    }

    fn handle_binary(&mut self, bytes: &[u8]) -> Result<()> {
        let (kind, payload) = decode_envelope(bytes)?;
        match kind.as_str() {
            message_type::ROOM_CREATED => {
                let payload = decode_payload::<proto::RoomCreatedPayload>(&payload)?;
                validate_room_code(&payload.room_code)?;
                validate_identifier("user ID", &payload.user_id, 256)?;
                validate_session_token(&payload.session_token)?;
                let username = self.stored_username.clone().unwrap_or_default();
                let room = ListenTogetherRoomState {
                    room_code: payload.room_code.clone(),
                    host_id: payload.user_id.clone(),
                    users: vec![ListenTogetherUser {
                        user_id: payload.user_id.clone(),
                        username,
                        is_host: true,
                        is_connected: true,
                    }],
                    current_track: None,
                    is_playing: false,
                    position_ms: 0,
                    last_update_ms: 0,
                    volume: 1.0,
                    queue: Vec::new(),
                    revision: 0,
                };
                self.session_token = Some(Zeroizing::new(payload.session_token));
                self.session_received_at = Some(Instant::now());
                self.stored_room_code = Some(payload.room_code.clone());
                self.was_host = true;
                self.last_revision = 0;
                self.set_room(
                    ListenTogetherRoomRole::Host,
                    Some(payload.user_id),
                    Some(room),
                );
                self.push_event(ListenTogetherEvent::RoomCreated {
                    room_code: payload.room_code,
                });
            }
            message_type::JOIN_REQUEST => {
                let payload = decode_payload::<proto::JoinRequestPayload>(&payload)?;
                let request = ListenTogetherJoinRequest {
                    user_id: normalize_identifier("user ID", payload.user_id, 256)?,
                    username: normalize_server_text("username", payload.username, 128, false)?,
                };
                lock_unpoisoned(&self.shared.snapshot)
                    .pending_join_requests
                    .retain(|existing| existing.user_id != request.user_id);
                lock_unpoisoned(&self.shared.snapshot)
                    .pending_join_requests
                    .push(request.clone());
                self.push_event(ListenTogetherEvent::JoinRequest(request));
            }
            message_type::JOIN_APPROVED => {
                let payload = decode_payload::<proto::JoinApprovedPayload>(&payload)?;
                let room_code = normalize_room_code(payload.room_code)?;
                let user_id = normalize_identifier("user ID", payload.user_id, 256)?;
                validate_session_token(&payload.session_token)?;
                let room = room_from_proto(required(payload.state, "join state")?)?;
                if room.room_code != room_code {
                    return Err(protocol_error(
                        "join response room code does not match room state",
                    ));
                }
                self.last_revision = room.revision;
                self.session_token = Some(Zeroizing::new(payload.session_token));
                self.session_received_at = Some(Instant::now());
                self.stored_room_code = Some(room_code);
                self.was_host = false;
                self.set_room(
                    ListenTogetherRoomRole::Guest,
                    Some(user_id),
                    Some(room.clone()),
                );
                self.push_event(ListenTogetherEvent::JoinApproved(room));
            }
            message_type::JOIN_REJECTED => {
                let payload = decode_payload::<proto::JoinRejectedPayload>(&payload)?;
                let reason =
                    normalize_server_text("join rejection", payload.reason, MAX_TEXT_CHARS, true)?;
                self.push_event(ListenTogetherEvent::JoinRejected { reason });
            }
            message_type::USER_JOINED => {
                let payload = decode_payload::<proto::UserJoinedPayload>(&payload)?;
                let user = ListenTogetherUser {
                    user_id: normalize_identifier("user ID", payload.user_id, 256)?,
                    username: normalize_server_text("username", payload.username, 128, false)?,
                    is_host: false,
                    is_connected: true,
                };
                let mut snapshot = lock_unpoisoned(&self.shared.snapshot);
                snapshot
                    .pending_join_requests
                    .retain(|request| request.user_id != user.user_id);
                if let Some(room) = snapshot.room.as_mut() {
                    room.users
                        .retain(|existing| existing.user_id != user.user_id);
                    room.users.push(user.clone());
                }
                drop(snapshot);
                self.push_event(ListenTogetherEvent::UserJoined(user));
            }
            message_type::USER_LEFT => {
                let payload = decode_payload::<proto::UserLeftPayload>(&payload)?;
                let user_id = normalize_identifier("user ID", payload.user_id, 256)?;
                let username = normalize_server_text("username", payload.username, 128, false)?;
                if let Some(room) = lock_unpoisoned(&self.shared.snapshot).room.as_mut() {
                    room.users.retain(|user| user.user_id != user_id);
                }
                self.push_event(ListenTogetherEvent::UserLeft { user_id, username });
            }
            message_type::SYNC_PLAYBACK => {
                let action =
                    action_from_proto(decode_payload::<proto::PlaybackActionPayload>(&payload)?)?;
                if !self.accept_revision(action.revision) {
                    return Ok(());
                }
                self.apply_action_to_room(&action);
                self.push_event(ListenTogetherEvent::PlaybackSync(action));
            }
            message_type::BUFFER_WAIT => {
                let payload = decode_payload::<proto::BufferWaitPayload>(&payload)?;
                let track_id = normalize_identifier("track ID", payload.track_id, 256)?;
                let waiting_for = validate_identifiers("buffering user ID", payload.waiting_for)?;
                lock_unpoisoned(&self.shared.snapshot).buffering_users = waiting_for.clone();
                self.push_event(ListenTogetherEvent::BufferWait {
                    track_id,
                    waiting_for,
                });
            }
            message_type::BUFFER_COMPLETE => {
                let payload = decode_payload::<proto::BufferCompletePayload>(&payload)?;
                let track_id = normalize_identifier("track ID", payload.track_id, 256)?;
                lock_unpoisoned(&self.shared.snapshot)
                    .buffering_users
                    .clear();
                self.push_event(ListenTogetherEvent::BufferComplete { track_id });
            }
            message_type::ERROR => {
                let payload = decode_payload::<proto::ErrorPayload>(&payload)?;
                let code = normalize_server_text("server error code", payload.code, 128, false)?;
                let message = normalize_server_text(
                    "server error message",
                    payload.message,
                    MAX_TEXT_CHARS,
                    true,
                )?;
                if code == "session_not_found" {
                    self.session_token = None;
                    self.session_received_at = None;
                    self.set_room(ListenTogetherRoomRole::None, None, None);
                    if !self.was_host
                        && let (Some(room_code), Some(username)) =
                            (self.stored_room_code.clone(), self.stored_username.clone())
                    {
                        self.pending_room_action = Some(PendingRoomAction::Join {
                            room_code,
                            username,
                        });
                        self.execute_pending_room_action();
                    }
                }
                self.set_last_error(Some(format!("{code}: {message}")));
                self.push_event(ListenTogetherEvent::ServerError { code, message });
            }
            message_type::PONG => {
                let payload = decode_payload::<proto::PongPayload>(&payload)?;
                let first = lock_unpoisoned(&self.shared.clock).record_pong(
                    payload.client_time,
                    payload.server_receive_time,
                    payload.server_send_time,
                );
                if first && self.snapshot_role() == ListenTogetherRoomRole::Guest {
                    let _ = self.send_empty(message_type::REQUEST_SYNC);
                }
            }
            message_type::HOST_CHANGED => {
                let payload = decode_payload::<proto::HostChangedPayload>(&payload)?;
                let new_host_id = normalize_identifier("user ID", payload.new_host_id, 256)?;
                let new_host_name =
                    normalize_server_text("username", payload.new_host_name, 128, false)?;
                let mut snapshot = lock_unpoisoned(&self.shared.snapshot);
                if let Some(room) = snapshot.room.as_mut() {
                    room.host_id.clone_from(&new_host_id);
                    for user in &mut room.users {
                        user.is_host = user.user_id == new_host_id;
                    }
                }
                snapshot.role = if snapshot.user_id.as_deref() == Some(new_host_id.as_str()) {
                    ListenTogetherRoomRole::Host
                } else {
                    ListenTogetherRoomRole::Guest
                };
                self.was_host = snapshot.role == ListenTogetherRoomRole::Host;
                drop(snapshot);
                self.push_event(ListenTogetherEvent::HostChanged {
                    new_host_id,
                    new_host_name,
                });
            }
            message_type::KICKED => {
                let payload = decode_payload::<proto::KickedPayload>(&payload)?;
                let reason =
                    normalize_server_text("kick reason", payload.reason, MAX_TEXT_CHARS, true)?;
                self.clear_session_and_room();
                self.push_event(ListenTogetherEvent::Kicked { reason });
            }
            message_type::SYNC_STATE => {
                let state =
                    sync_state_from_proto(decode_payload::<proto::SyncStatePayload>(&payload)?)?;
                if !self.accept_revision(state.revision) {
                    return Ok(());
                }
                self.apply_sync_state_to_room(&state);
                self.push_event(ListenTogetherEvent::SyncState(state));
            }
            message_type::RECONNECTED => {
                let payload = decode_payload::<proto::ReconnectedPayload>(&payload)?;
                let room_code = normalize_room_code(payload.room_code)?;
                let user_id = normalize_identifier("user ID", payload.user_id, 256)?;
                let room = room_from_proto(required(payload.state, "reconnected room state")?)?;
                if room.room_code != room_code {
                    return Err(protocol_error(
                        "reconnect response room code does not match room state",
                    ));
                }
                self.last_revision = room.revision;
                self.stored_room_code = Some(room_code);
                self.session_received_at = Some(Instant::now());
                self.was_host = payload.is_host;
                self.set_room(
                    if payload.is_host {
                        ListenTogetherRoomRole::Host
                    } else {
                        ListenTogetherRoomRole::Guest
                    },
                    Some(user_id),
                    Some(room.clone()),
                );
                self.push_event(ListenTogetherEvent::Reconnected(room));
            }
            message_type::USER_RECONNECTED | message_type::USER_DISCONNECTED => {
                let connected = kind == message_type::USER_RECONNECTED;
                let (user_id, username) = if connected {
                    let payload = decode_payload::<proto::UserReconnectedPayload>(&payload)?;
                    (payload.user_id, payload.username)
                } else {
                    let payload = decode_payload::<proto::UserDisconnectedPayload>(&payload)?;
                    (payload.user_id, payload.username)
                };
                let user_id = normalize_identifier("user ID", user_id, 256)?;
                let username = normalize_server_text("username", username, 128, false)?;
                if let Some(user) = lock_unpoisoned(&self.shared.snapshot)
                    .room
                    .as_mut()
                    .and_then(|room| room.users.iter_mut().find(|user| user.user_id == user_id))
                {
                    user.is_connected = connected;
                }
                self.push_event(if connected {
                    ListenTogetherEvent::UserReconnected { user_id, username }
                } else {
                    ListenTogetherEvent::UserDisconnected { user_id, username }
                });
            }
            message_type::SUGGESTION_RECEIVED => {
                let payload = decode_payload::<proto::SuggestionReceivedPayload>(&payload)?;
                let suggestion = ListenTogetherSuggestion {
                    suggestion_id: normalize_identifier(
                        "suggestion ID",
                        payload.suggestion_id,
                        256,
                    )?,
                    from_user_id: normalize_identifier(
                        "suggestion user ID",
                        payload.from_user_id,
                        256,
                    )?,
                    from_username: normalize_server_text(
                        "username",
                        payload.from_username,
                        128,
                        false,
                    )?,
                    track: track_from_proto(required(payload.track_info, "suggested track")?)?,
                };
                let mut snapshot = lock_unpoisoned(&self.shared.snapshot);
                snapshot
                    .pending_suggestions
                    .retain(|existing| existing.suggestion_id != suggestion.suggestion_id);
                snapshot.pending_suggestions.push(suggestion.clone());
                drop(snapshot);
                self.push_event(ListenTogetherEvent::SuggestionReceived(suggestion));
            }
            message_type::SUGGESTION_APPROVED => {
                let payload = decode_payload::<proto::SuggestionApprovedPayload>(&payload)?;
                let suggestion_id =
                    normalize_identifier("suggestion ID", payload.suggestion_id, 256)?;
                let track = track_from_proto(required(payload.track_info, "approved track")?)?;
                lock_unpoisoned(&self.shared.snapshot)
                    .pending_suggestions
                    .retain(|suggestion| suggestion.suggestion_id != suggestion_id);
                self.push_event(ListenTogetherEvent::SuggestionApproved {
                    suggestion_id,
                    track,
                });
            }
            message_type::SUGGESTION_REJECTED => {
                let payload = decode_payload::<proto::SuggestionRejectedPayload>(&payload)?;
                let suggestion_id =
                    normalize_identifier("suggestion ID", payload.suggestion_id, 256)?;
                let reason = normalize_optional_text(
                    "suggestion rejection",
                    (!payload.reason.is_empty()).then_some(payload.reason),
                    MAX_TEXT_CHARS,
                )?;
                lock_unpoisoned(&self.shared.snapshot)
                    .pending_suggestions
                    .retain(|suggestion| suggestion.suggestion_id != suggestion_id);
                self.push_event(ListenTogetherEvent::SuggestionRejected {
                    suggestion_id,
                    reason,
                });
            }
            _ => {}
        }
        Ok(())
    }

    fn apply_action_to_room(&mut self, action: &ListenTogetherPlaybackActionPayload) {
        let mut snapshot = lock_unpoisoned(&self.shared.snapshot);
        let Some(room) = snapshot.room.as_mut() else {
            return;
        };
        room.revision = room.revision.max(action.revision);
        match action.action {
            ListenTogetherPlaybackAction::Play => {
                room.is_playing = true;
                room.position_ms = action.position_ms.unwrap_or(room.position_ms);
                room.last_update_ms = action
                    .server_time_ms
                    .or(action.captured_at_server_time_ms)
                    .unwrap_or(room.last_update_ms);
            }
            ListenTogetherPlaybackAction::Pause => {
                room.is_playing = false;
                room.position_ms = action.position_ms.unwrap_or(room.position_ms);
                room.last_update_ms = action
                    .server_time_ms
                    .or(action.captured_at_server_time_ms)
                    .unwrap_or(room.last_update_ms);
            }
            ListenTogetherPlaybackAction::Seek => {
                room.position_ms = action.position_ms.unwrap_or(room.position_ms);
                room.last_update_ms = action
                    .server_time_ms
                    .or(action.captured_at_server_time_ms)
                    .unwrap_or(room.last_update_ms);
            }
            ListenTogetherPlaybackAction::ChangeTrack => {
                room.current_track.clone_from(&action.track);
                room.is_playing = false;
                room.position_ms = 0;
                if let Some(queue) = action.queue.as_ref() {
                    room.queue.clone_from(queue);
                }
            }
            ListenTogetherPlaybackAction::SyncQueue
            | ListenTogetherPlaybackAction::QueueAdd
            | ListenTogetherPlaybackAction::QueueRemove
            | ListenTogetherPlaybackAction::QueueClear => {
                if let Some(queue) = action.queue.as_ref() {
                    room.queue.clone_from(queue);
                }
            }
            ListenTogetherPlaybackAction::SetVolume => {
                if let Some(volume) = action.volume {
                    room.volume = volume;
                }
            }
            ListenTogetherPlaybackAction::SkipNext | ListenTogetherPlaybackAction::SkipPrevious => {
            }
        }
    }

    fn apply_sync_state_to_room(&mut self, state: &ListenTogetherSyncState) {
        let mut snapshot = lock_unpoisoned(&self.shared.snapshot);
        let Some(room) = snapshot.room.as_mut() else {
            return;
        };
        room.current_track.clone_from(&state.current_track);
        room.is_playing = state.is_playing;
        room.position_ms = state.position_ms;
        room.last_update_ms = state.last_update_ms;
        room.queue.clone_from(&state.queue);
        room.volume = state.volume;
        room.revision = room.revision.max(state.revision);
    }

    fn accept_revision(&mut self, revision: u64) -> bool {
        if revision == 0 {
            return true;
        }
        if revision < self.last_revision {
            return false;
        }
        self.last_revision = revision;
        true
    }

    fn send_empty(&mut self, kind: &'static str) -> Result<()> {
        self.send_envelope(kind, Vec::new())
    }

    fn send_proto<M: ProstMessage>(&mut self, kind: &'static str, payload: M) -> Result<()> {
        self.send_envelope(kind, payload.encode_to_vec())
    }

    fn send_envelope(&mut self, kind: &'static str, payload: Vec<u8>) -> Result<()> {
        let bytes = encode_envelope(kind, payload, true)?;
        let Some(socket) = self.socket.as_mut() else {
            return Err(AppError::ListenTogether(
                "the room server is not connected".into(),
            ));
        };
        if let Err(error) = socket.send(Message::binary(bytes)) {
            let message = sanitize_transport_error(&error);
            self.connection_lost(message.clone());
            return Err(AppError::ListenTogether(message));
        }
        Ok(())
    }

    fn connection_lost(&mut self, message: String) {
        self.socket = None;
        self.next_ping_at = None;
        lock_unpoisoned(&self.shared.clock).reset();
        if self.connect_requested
            && (self.session_is_fresh() || self.pending_room_action.is_some())
            && self.reconnect_attempt < MAX_RECONNECT_ATTEMPTS
        {
            self.reconnect_attempt += 1;
            let delay = reconnect_delay(self.reconnect_attempt);
            self.reconnect_at = Some(Instant::now() + delay);
            self.set_connection(
                ListenTogetherConnectionState::Reconnecting {
                    attempt: self.reconnect_attempt,
                    max_attempts: MAX_RECONNECT_ATTEMPTS,
                },
                Some(message),
            );
        } else {
            self.connect_requested = false;
            self.reconnect_at = None;
            self.set_connection(ListenTogetherConnectionState::Error, Some(message.clone()));
            self.push_event(ListenTogetherEvent::ConnectionError { message });
        }
    }

    fn fail_connection(&mut self, message: String) {
        self.connection_lost(message);
    }

    fn explicit_disconnect(&mut self) {
        self.connect_requested = false;
        self.reconnect_at = None;
        self.reconnect_attempt = 0;
        if let Some(mut socket) = self.socket.take() {
            let _ = socket.close(None);
        }
        lock_unpoisoned(&self.shared.clock).reset();
        self.clear_session_and_room();
        self.set_connection(ListenTogetherConnectionState::Disconnected, None);
        self.push_event(ListenTogetherEvent::Disconnected);
    }

    fn clear_session_and_room(&mut self) {
        self.session_token = None;
        self.session_received_at = None;
        self.stored_room_code = None;
        self.was_host = false;
        self.last_revision = 0;
        self.set_room(ListenTogetherRoomRole::None, None, None);
    }

    fn session_is_fresh(&self) -> bool {
        self.session_token.is_some()
            && self
                .session_received_at
                .is_some_and(|received| received.elapsed() < SESSION_GRACE_PERIOD)
    }

    fn set_connection(&self, connection: ListenTogetherConnectionState, error: Option<String>) {
        let mut snapshot = lock_unpoisoned(&self.shared.snapshot);
        snapshot.connection = connection;
        snapshot.last_error = error;
    }

    fn set_last_error(&self, error: Option<String>) {
        lock_unpoisoned(&self.shared.snapshot).last_error = error;
    }

    fn set_room(
        &self,
        role: ListenTogetherRoomRole,
        user_id: Option<String>,
        room: Option<ListenTogetherRoomState>,
    ) {
        let mut snapshot = lock_unpoisoned(&self.shared.snapshot);
        snapshot.role = role;
        snapshot.user_id = user_id;
        snapshot.room = room;
        snapshot.pending_join_requests.clear();
        snapshot.buffering_users.clear();
        snapshot.pending_suggestions.clear();
    }

    fn snapshot_role(&self) -> ListenTogetherRoomRole {
        lock_unpoisoned(&self.shared.snapshot).role
    }

    fn push_event(&self, event: ListenTogetherEvent) {
        let mut events = lock_unpoisoned(&self.shared.events);
        if events.len() == EVENT_CAPACITY {
            events.pop_front();
        }
        events.push_back(event);
    }
}

fn configure_socket(socket: &mut Socket) -> std::io::Result<()> {
    fn configure(stream: &TcpStream) -> std::io::Result<()> {
        stream.set_read_timeout(Some(SOCKET_POLL_INTERVAL))?;
        stream.set_write_timeout(Some(Duration::from_secs(10)))?;
        stream.set_nodelay(true)
    }

    match socket.get_mut() {
        MaybeTlsStream::Plain(stream) => configure(stream),
        MaybeTlsStream::Rustls(stream) => configure(&stream.sock),
        _ => Err(std::io::Error::new(
            ErrorKind::Unsupported,
            "unsupported TLS transport",
        )),
    }
}

fn reconnect_delay(attempt: u8) -> Duration {
    let shift = u32::from(attempt.saturating_sub(1).min(5));
    let base = INITIAL_RECONNECT_DELAY
        .saturating_mul(1_u32 << shift)
        .min(MAX_RECONNECT_DELAY);
    let jitter_millis = fastrand::u64(0..=(base.as_millis() as u64 / 5));
    base.saturating_add(Duration::from_millis(jitter_millis))
        .min(MAX_RECONNECT_DELAY)
}

fn encode_envelope(kind: &str, payload: Vec<u8>, compress: bool) -> Result<Vec<u8>> {
    validate_text("message type", kind, 128, false)?;
    let (payload, compressed) = if compress && payload.len() > COMPRESSION_THRESHOLD {
        let mut encoder = GzEncoder::new(Vec::new(), Compression::fast());
        encoder.write_all(&payload).map_err(|error| {
            AppError::ListenTogether(format!("could not compress protocol message: {error}"))
        })?;
        let compressed = encoder.finish().map_err(|error| {
            AppError::ListenTogether(format!("could not finish protocol compression: {error}"))
        })?;
        if compressed.len() < payload.len() {
            (compressed, true)
        } else {
            (payload, false)
        }
    } else {
        (payload, false)
    };
    let encoded = proto::Envelope {
        r#type: kind.to_owned(),
        payload,
        compressed,
    }
    .encode_to_vec();
    if encoded.len() > MAX_MESSAGE_BYTES {
        return Err(AppError::ListenTogether(
            "outgoing protocol message is too large".into(),
        ));
    }
    Ok(encoded)
}

fn decode_envelope(bytes: &[u8]) -> Result<(String, Vec<u8>)> {
    if bytes.len() > MAX_MESSAGE_BYTES {
        return Err(protocol_error("incoming protocol message is too large"));
    }
    let envelope = proto::Envelope::decode(bytes)
        .map_err(|_| protocol_error("incoming WebSocket frame is not a valid envelope"))?;
    validate_text("message type", &envelope.r#type, 128, false)?;
    let payload = if envelope.compressed {
        let mut decoder = GzDecoder::new(envelope.payload.as_slice()).take(
            u64::try_from(MAX_DECOMPRESSED_BYTES + 1).expect("decompression size limit fits u64"),
        );
        let mut decoded = Vec::new();
        decoder
            .read_to_end(&mut decoded)
            .map_err(|_| protocol_error("compressed protocol payload is invalid"))?;
        if decoded.len() > MAX_DECOMPRESSED_BYTES {
            return Err(protocol_error("decompressed protocol payload is too large"));
        }
        decoded
    } else {
        envelope.payload
    };
    Ok((envelope.r#type, payload))
}

fn decode_payload<M: ProstMessage + Default>(bytes: &[u8]) -> Result<M> {
    M::decode(bytes).map_err(|_| protocol_error("server payload did not match its message type"))
}

fn action_to_proto(action: ListenTogetherPlaybackActionPayload) -> proto::PlaybackActionPayload {
    proto::PlaybackActionPayload {
        action: action.action.as_str().into(),
        track_id: action.track_id.unwrap_or_default(),
        position: action.position_ms.unwrap_or_default(),
        track_info: action.track.map(track_to_proto),
        insert_next: action.insert_next.unwrap_or_default(),
        queue: action
            .queue
            .unwrap_or_default()
            .into_iter()
            .map(track_to_proto)
            .collect(),
        queue_title: action.queue_title.unwrap_or_default(),
        volume: action.volume.unwrap_or(1.0),
        server_time: action.server_time_ms.unwrap_or_default(),
        revision: action.revision,
        captured_at_server_time: action.captured_at_server_time_ms.unwrap_or_default(),
    }
}

fn action_from_proto(
    action: proto::PlaybackActionPayload,
) -> Result<ListenTogetherPlaybackActionPayload> {
    let action_type = ListenTogetherPlaybackAction::from_str(&action.action)?;
    let position_ms = (action.position != 0
        || matches!(
            action_type,
            ListenTogetherPlaybackAction::Play
                | ListenTogetherPlaybackAction::Pause
                | ListenTogetherPlaybackAction::Seek
        ))
    .then_some(action.position);
    let payload = ListenTogetherPlaybackActionPayload {
        action: action_type,
        track_id: non_empty(action.track_id),
        position_ms,
        track: action.track_info.map(track_from_proto).transpose()?,
        insert_next: action.insert_next.then_some(true),
        queue: (!action.queue.is_empty())
            .then(|| {
                action
                    .queue
                    .into_iter()
                    .map(track_from_proto)
                    .collect::<Result<Vec<_>>>()
            })
            .transpose()?,
        queue_title: non_empty(action.queue_title),
        volume: (action_type == ListenTogetherPlaybackAction::SetVolume).then_some(action.volume),
        server_time_ms: (action.server_time > 0).then_some(action.server_time),
        revision: action.revision,
        captured_at_server_time_ms: (action.captured_at_server_time > 0)
            .then_some(action.captured_at_server_time),
    };
    payload.validate()?;
    Ok(payload)
}

fn track_to_proto(track: ListenTogetherTrack) -> proto::TrackInfo {
    let thumbnail = encode_track_thumbnail(track.thumbnail, track.is_episode, &track.id);
    proto::TrackInfo {
        id: track.id,
        title: track.title,
        artist: track.artist,
        album: track.album.unwrap_or_default(),
        duration: track.duration_ms,
        thumbnail: thumbnail.unwrap_or_default(),
        suggested_by: track.suggested_by.unwrap_or_default(),
        is_episode: track.is_episode,
    }
}

fn track_from_proto(track: proto::TrackInfo) -> Result<ListenTogetherTrack> {
    let (thumbnail, legacy_episode_marker) = normalize_incoming_thumbnail(track.thumbnail)?;
    let track = ListenTogetherTrack {
        id: track.id,
        title: track.title,
        artist: track.artist,
        album: non_empty(track.album),
        duration_ms: track.duration,
        thumbnail,
        suggested_by: non_empty(track.suggested_by),
        is_episode: track.is_episode || legacy_episode_marker,
    };
    track.validate()?;
    Ok(track)
}

fn user_from_proto(user: proto::UserInfo) -> Result<ListenTogetherUser> {
    Ok(ListenTogetherUser {
        user_id: normalize_identifier("user ID", user.user_id, 256)?,
        username: normalize_server_text("username", user.username, 128, false)?,
        is_host: user.is_host,
        is_connected: user.is_connected,
    })
}

fn room_from_proto(room: proto::RoomState) -> Result<ListenTogetherRoomState> {
    let room = ListenTogetherRoomState {
        room_code: normalize_room_code(room.room_code)?,
        host_id: normalize_identifier("host ID", room.host_id, 256)?,
        users: room
            .users
            .into_iter()
            .map(user_from_proto)
            .collect::<Result<Vec<_>>>()?,
        current_track: room.current_track.map(track_from_proto).transpose()?,
        is_playing: room.is_playing,
        position_ms: room.position,
        last_update_ms: room.last_update,
        volume: room.volume,
        queue: room
            .queue
            .into_iter()
            .map(track_from_proto)
            .collect::<Result<Vec<_>>>()?,
        revision: room.revision,
    };
    validate_room(&room)?;
    Ok(room)
}

fn sync_state_from_proto(state: proto::SyncStatePayload) -> Result<ListenTogetherSyncState> {
    let state = ListenTogetherSyncState {
        current_track: state.current_track.map(track_from_proto).transpose()?,
        is_playing: state.is_playing,
        position_ms: state.position,
        last_update_ms: state.last_update,
        queue: state
            .queue
            .into_iter()
            .map(track_from_proto)
            .collect::<Result<Vec<_>>>()?,
        volume: state.volume,
        revision: state.revision,
    };
    if state.position_ms < 0 || !state.volume.is_finite() || !(0.0..=1.0).contains(&state.volume) {
        return Err(protocol_error("sync state has invalid playback values"));
    }
    validate_track_queue(&state.queue)?;
    Ok(state)
}

fn validate_room(room: &ListenTogetherRoomState) -> Result<()> {
    validate_room_code(&room.room_code)?;
    validate_identifier("host ID", &room.host_id, 256)?;
    if room.users.len() > 10_000 {
        return Err(protocol_error("room contains too many users"));
    }
    if room.position_ms < 0 || !room.volume.is_finite() || !(0.0..=1.0).contains(&room.volume) {
        return Err(protocol_error("room has invalid playback values"));
    }
    validate_track_queue(&room.queue)
}

fn validate_track_queue(queue: &[ListenTogetherTrack]) -> Result<()> {
    if queue.len() > MAX_QUEUE_ITEMS {
        return Err(protocol_error("room queue is too large"));
    }
    for track in queue {
        track.validate()?;
    }
    Ok(())
}

fn normalize_server_url(value: String) -> Result<String> {
    let value = value.trim();
    let url = Url::parse(value).map_err(|_| {
        AppError::InvalidConfig("Listen Together server must be a valid ws:// or wss:// URL".into())
    })?;
    if !matches!(url.scheme(), "ws" | "wss")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
    {
        return Err(AppError::InvalidConfig(
            "Listen Together server must be a credential-free ws:// or wss:// URL with a host"
                .into(),
        ));
    }
    Ok(url.to_string())
}

fn normalize_username(value: String) -> Result<String> {
    let value = value.trim().to_owned();
    validate_text("Listen Together username", &value, 128, false)?;
    Ok(value)
}

fn normalize_room_code(value: String) -> Result<String> {
    let value = value.trim().to_ascii_uppercase();
    validate_room_code(&value)?;
    Ok(value)
}

fn validate_room_code(value: &str) -> Result<()> {
    if value.len() != 8 || !value.bytes().all(|byte| byte.is_ascii_alphanumeric()) {
        return Err(AppError::ListenTogether(
            "room code must contain exactly 8 ASCII letters or digits".into(),
        ));
    }
    Ok(())
}

fn validate_session_token(value: &str) -> Result<()> {
    validate_text("session token", value, 4_096, false)
}

fn normalize_identifier(label: &str, value: String, max_chars: usize) -> Result<String> {
    let value = value.trim().to_owned();
    validate_identifier(label, &value, max_chars)?;
    Ok(value)
}

fn validate_identifier(label: &str, value: &str, max_chars: usize) -> Result<()> {
    validate_text(label, value, max_chars, false)
}

fn normalize_optional_text(
    label: &str,
    value: Option<String>,
    max_chars: usize,
) -> Result<Option<String>> {
    value
        .map(|value| normalize_server_text(label, value, max_chars, true))
        .transpose()
}

fn normalize_server_text(
    label: &str,
    value: String,
    max_chars: usize,
    allow_empty: bool,
) -> Result<String> {
    let value = value.trim().to_owned();
    validate_text(label, &value, max_chars, allow_empty)?;
    Ok(value)
}

fn validate_text(label: &str, value: &str, max_chars: usize, allow_empty: bool) -> Result<()> {
    let chars = value.chars().count();
    if (!allow_empty && value.is_empty())
        || chars > max_chars
        || value.chars().any(char::is_control)
    {
        return Err(protocol_error(format!(
            "{label} is empty, too long, or contains control characters"
        )));
    }
    Ok(())
}

fn validate_public_url(label: &str, value: &str) -> Result<()> {
    let url =
        Url::parse(value).map_err(|_| protocol_error(format!("{label} is not a valid URL")))?;
    let host = url.host_str().unwrap_or_default().trim_end_matches('.');
    if url.scheme() != "https"
        || host.is_empty()
        || !url.username().is_empty()
        || url.password().is_some()
        || host.eq_ignore_ascii_case("localhost")
        || host.to_ascii_lowercase().ends_with(".localhost")
        || host.to_ascii_lowercase().ends_with(".local")
        || host.parse::<IpAddr>().is_ok()
    {
        return Err(protocol_error(format!(
            "{label} must be a credential-free HTTPS URL with a public hostname"
        )));
    }
    Ok(())
}

fn encode_track_thumbnail(
    thumbnail: Option<String>,
    is_episode: bool,
    track_id: &str,
) -> Option<String> {
    let mut thumbnail = match thumbnail {
        Some(thumbnail) => thumbnail,
        None if is_episode => {
            let mut fallback = Url::parse("https://i.ytimg.com/")
                .expect("the fixed YouTube thumbnail base URL parses");
            fallback
                .path_segments_mut()
                .expect("the fixed YouTube thumbnail URL supports path segments")
                .extend(["vi", track_id, "default.jpg"]);
            fallback.to_string()
        }
        None => return None,
    };
    if !is_episode {
        return Some(thumbnail);
    }
    let Ok(mut url) = Url::parse(&thumbnail) else {
        return Some(thumbnail);
    };
    let mut fragments = url
        .fragment()
        .unwrap_or_default()
        .split('&')
        .filter(|fragment| !fragment.is_empty())
        .map(str::to_owned)
        .collect::<Vec<_>>();
    if !fragments
        .iter()
        .any(|fragment| fragment == EPISODE_THUMBNAIL_FRAGMENT_TOKEN)
    {
        fragments.push(EPISODE_THUMBNAIL_FRAGMENT_TOKEN.into());
    }
    url.set_fragment(Some(&fragments.join("&")));
    thumbnail = url.to_string();
    Some(thumbnail)
}

fn normalize_incoming_thumbnail(value: String) -> Result<(Option<String>, bool)> {
    let Some(value) = non_empty(value) else {
        return Ok((None, false));
    };
    validate_public_url("track thumbnail", &value)?;
    let mut url = Url::parse(&value).expect("validated thumbnail URL parses");
    let host = url.host_str().unwrap_or_default().to_ascii_lowercase();
    let trusted_youtube_cdn = ["ytimg.com", "ggpht.com", "googleusercontent.com"]
        .iter()
        .any(|suffix| host == *suffix || host.ends_with(&format!(".{suffix}")));
    if !trusted_youtube_cdn {
        return Ok((None, false));
    }

    let mut is_episode = false;
    let retained_fragments = url
        .fragment()
        .unwrap_or_default()
        .split('&')
        .filter(|fragment| {
            if *fragment == EPISODE_THUMBNAIL_FRAGMENT_TOKEN {
                is_episode = true;
                false
            } else {
                !fragment.is_empty()
            }
        })
        .collect::<Vec<_>>();
    if is_episode {
        if retained_fragments.is_empty() {
            url.set_fragment(None);
        } else {
            url.set_fragment(Some(&retained_fragments.join("&")));
        }
    }
    Ok((Some(url.to_string()), is_episode))
}

fn validate_identifiers(label: &str, values: Vec<String>) -> Result<Vec<String>> {
    if values.len() > 10_000 {
        return Err(protocol_error("server returned too many identifiers"));
    }
    values
        .into_iter()
        .map(|value| normalize_identifier(label, value, 256))
        .collect()
}

fn non_empty(value: String) -> Option<String> {
    (!value.is_empty()).then_some(value)
}

fn required<T>(value: Option<T>, label: &str) -> Result<T> {
    value.ok_or_else(|| protocol_error(format!("server omitted {label}")))
}

fn protocol_error(message: impl Into<String>) -> AppError {
    AppError::ListenTogether(message.into())
}

fn sanitize_transport_error(error: &tungstenite::Error) -> String {
    match error {
        tungstenite::Error::Http(response) => {
            format!(
                "room server rejected the WebSocket handshake ({})",
                response.status()
            )
        }
        tungstenite::Error::Io(error) => format!("room server transport failed: {}", error.kind()),
        tungstenite::Error::Tls(_) => "room server TLS negotiation failed".into(),
        tungstenite::Error::Url(_) => "room server URL is unsupported".into(),
        tungstenite::Error::ConnectionClosed | tungstenite::Error::AlreadyClosed => {
            "room server connection closed".into()
        }
        _ => "room server WebSocket protocol failed".into(),
    }
}

fn lock_unpoisoned<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

struct ServerClock {
    epoch: Instant,
    server_offset_ms: Option<f64>,
    best_round_trip_ms: i64,
}

impl Default for ServerClock {
    fn default() -> Self {
        Self {
            epoch: Instant::now(),
            server_offset_ms: None,
            best_round_trip_ms: i64::MAX,
        }
    }
}

impl ServerClock {
    fn reset(&mut self) {
        self.epoch = Instant::now();
        self.server_offset_ms = None;
        self.best_round_trip_ms = i64::MAX;
    }

    fn elapsed_ms(&self) -> i64 {
        self.epoch.elapsed().as_millis().min(i64::MAX as u128) as i64
    }

    fn record_pong(
        &mut self,
        client_time: i64,
        server_receive_time: i64,
        server_send_time: i64,
    ) -> bool {
        let received_at = self.elapsed_ms();
        if client_time <= 0
            || client_time > received_at
            || received_at - client_time > 60_000
            || server_receive_time <= 0
            || server_send_time < server_receive_time
        {
            return false;
        }
        let round_trip = received_at - client_time;
        let server_processing = server_send_time - server_receive_time;
        let network_round_trip = (round_trip - server_processing).max(0);
        let sample_offset =
            server_send_time as f64 + network_round_trip as f64 / 2.0 - received_at as f64;
        let previous = self.server_offset_ms;
        self.best_round_trip_ms = self.best_round_trip_ms.min(network_round_trip);
        let weight = if network_round_trip <= self.best_round_trip_ms + 50 {
            0.25
        } else {
            0.05
        };
        self.server_offset_ms = Some(previous.map_or(sample_offset, |previous| {
            previous + weight * (sample_offset - previous)
        }));
        previous.is_none()
    }

    fn now_ms(&self) -> Option<i64> {
        self.server_offset_ms
            .map(|offset| (self.elapsed_ms() as f64 + offset) as i64)
    }

    fn position_at(
        &self,
        position_ms: i64,
        effective_at_server_time_ms: Option<i64>,
        is_playing: bool,
    ) -> i64 {
        if !is_playing {
            return position_ms.max(0);
        }
        let Some(effective) = effective_at_server_time_ms.filter(|value| *value > 0) else {
            return position_ms.max(0);
        };
        let Some(now) = self.now_ms() else {
            return position_ms.max(0);
        };
        position_ms.saturating_add((now - effective).max(0)).max(0)
    }
}

mod proto {
    #[derive(Clone, PartialEq, prost::Message)]
    pub struct Envelope {
        #[prost(string, tag = "1")]
        pub r#type: String,
        #[prost(bytes = "vec", tag = "2")]
        pub payload: Vec<u8>,
        #[prost(bool, tag = "3")]
        pub compressed: bool,
    }

    #[derive(Clone, PartialEq, prost::Message)]
    pub struct TrackInfo {
        #[prost(string, tag = "1")]
        pub id: String,
        #[prost(string, tag = "2")]
        pub title: String,
        #[prost(string, tag = "3")]
        pub artist: String,
        #[prost(string, tag = "4")]
        pub album: String,
        #[prost(int64, tag = "5")]
        pub duration: i64,
        #[prost(string, tag = "6")]
        pub thumbnail: String,
        #[prost(string, tag = "7")]
        pub suggested_by: String,
        #[prost(bool, tag = "8")]
        pub is_episode: bool,
    }

    #[derive(Clone, PartialEq, prost::Message)]
    pub struct UserInfo {
        #[prost(string, tag = "1")]
        pub user_id: String,
        #[prost(string, tag = "2")]
        pub username: String,
        #[prost(bool, tag = "3")]
        pub is_host: bool,
        #[prost(bool, tag = "4")]
        pub is_connected: bool,
    }

    #[derive(Clone, PartialEq, prost::Message)]
    pub struct RoomState {
        #[prost(string, tag = "1")]
        pub room_code: String,
        #[prost(string, tag = "2")]
        pub host_id: String,
        #[prost(message, repeated, tag = "3")]
        pub users: Vec<UserInfo>,
        #[prost(message, optional, tag = "4")]
        pub current_track: Option<TrackInfo>,
        #[prost(bool, tag = "5")]
        pub is_playing: bool,
        #[prost(int64, tag = "6")]
        pub position: i64,
        #[prost(int64, tag = "7")]
        pub last_update: i64,
        #[prost(float, tag = "8")]
        pub volume: f32,
        #[prost(message, repeated, tag = "9")]
        pub queue: Vec<TrackInfo>,
        #[prost(uint64, tag = "10")]
        pub revision: u64,
    }

    #[derive(Clone, PartialEq, prost::Message)]
    pub struct CreateRoomPayload {
        #[prost(string, tag = "1")]
        pub username: String,
    }
    #[derive(Clone, PartialEq, prost::Message)]
    pub struct JoinRoomPayload {
        #[prost(string, tag = "1")]
        pub room_code: String,
        #[prost(string, tag = "2")]
        pub username: String,
    }
    #[derive(Clone, PartialEq, prost::Message)]
    pub struct ApproveJoinPayload {
        #[prost(string, tag = "1")]
        pub user_id: String,
    }
    #[derive(Clone, PartialEq, prost::Message)]
    pub struct RejectJoinPayload {
        #[prost(string, tag = "1")]
        pub user_id: String,
        #[prost(string, tag = "2")]
        pub reason: String,
    }
    #[derive(Clone, PartialEq, prost::Message)]
    pub struct PlaybackActionPayload {
        #[prost(string, tag = "1")]
        pub action: String,
        #[prost(string, tag = "2")]
        pub track_id: String,
        #[prost(int64, tag = "3")]
        pub position: i64,
        #[prost(message, optional, tag = "4")]
        pub track_info: Option<TrackInfo>,
        #[prost(bool, tag = "5")]
        pub insert_next: bool,
        #[prost(message, repeated, tag = "6")]
        pub queue: Vec<TrackInfo>,
        #[prost(string, tag = "7")]
        pub queue_title: String,
        #[prost(float, tag = "8")]
        pub volume: f32,
        #[prost(int64, tag = "9")]
        pub server_time: i64,
        #[prost(uint64, tag = "10")]
        pub revision: u64,
        #[prost(int64, tag = "11")]
        pub captured_at_server_time: i64,
    }
    #[derive(Clone, PartialEq, prost::Message)]
    pub struct PingPayload {
        #[prost(int64, tag = "1")]
        pub client_time: i64,
        #[prost(uint64, tag = "2")]
        pub sequence: u64,
    }
    #[derive(Clone, PartialEq, prost::Message)]
    pub struct BufferReadyPayload {
        #[prost(string, tag = "1")]
        pub track_id: String,
    }
    #[derive(Clone, PartialEq, prost::Message)]
    pub struct KickUserPayload {
        #[prost(string, tag = "1")]
        pub user_id: String,
        #[prost(string, tag = "2")]
        pub reason: String,
    }
    #[derive(Clone, PartialEq, prost::Message)]
    pub struct TransferHostPayload {
        #[prost(string, tag = "1")]
        pub new_host_id: String,
    }
    #[derive(Clone, PartialEq, prost::Message)]
    pub struct SuggestTrackPayload {
        #[prost(message, optional, tag = "1")]
        pub track_info: Option<TrackInfo>,
    }
    #[derive(Clone, PartialEq, prost::Message)]
    pub struct ApproveSuggestionPayload {
        #[prost(string, tag = "1")]
        pub suggestion_id: String,
    }
    #[derive(Clone, PartialEq, prost::Message)]
    pub struct RejectSuggestionPayload {
        #[prost(string, tag = "1")]
        pub suggestion_id: String,
        #[prost(string, tag = "2")]
        pub reason: String,
    }
    #[derive(Clone, PartialEq, prost::Message)]
    pub struct ReconnectPayload {
        #[prost(string, tag = "1")]
        pub session_token: String,
    }

    #[derive(Clone, PartialEq, prost::Message)]
    pub struct RoomCreatedPayload {
        #[prost(string, tag = "1")]
        pub room_code: String,
        #[prost(string, tag = "2")]
        pub user_id: String,
        #[prost(string, tag = "3")]
        pub session_token: String,
    }
    #[derive(Clone, PartialEq, prost::Message)]
    pub struct JoinRequestPayload {
        #[prost(string, tag = "1")]
        pub user_id: String,
        #[prost(string, tag = "2")]
        pub username: String,
    }
    #[derive(Clone, PartialEq, prost::Message)]
    pub struct JoinApprovedPayload {
        #[prost(string, tag = "1")]
        pub room_code: String,
        #[prost(string, tag = "2")]
        pub user_id: String,
        #[prost(string, tag = "3")]
        pub session_token: String,
        #[prost(message, optional, tag = "4")]
        pub state: Option<RoomState>,
    }
    #[derive(Clone, PartialEq, prost::Message)]
    pub struct JoinRejectedPayload {
        #[prost(string, tag = "1")]
        pub reason: String,
    }
    #[derive(Clone, PartialEq, prost::Message)]
    pub struct UserJoinedPayload {
        #[prost(string, tag = "1")]
        pub user_id: String,
        #[prost(string, tag = "2")]
        pub username: String,
    }
    #[derive(Clone, PartialEq, prost::Message)]
    pub struct UserLeftPayload {
        #[prost(string, tag = "1")]
        pub user_id: String,
        #[prost(string, tag = "2")]
        pub username: String,
    }
    #[derive(Clone, PartialEq, prost::Message)]
    pub struct BufferWaitPayload {
        #[prost(string, tag = "1")]
        pub track_id: String,
        #[prost(string, repeated, tag = "2")]
        pub waiting_for: Vec<String>,
    }
    #[derive(Clone, PartialEq, prost::Message)]
    pub struct BufferCompletePayload {
        #[prost(string, tag = "1")]
        pub track_id: String,
    }
    #[derive(Clone, PartialEq, prost::Message)]
    pub struct ErrorPayload {
        #[prost(string, tag = "1")]
        pub code: String,
        #[prost(string, tag = "2")]
        pub message: String,
    }
    #[derive(Clone, PartialEq, prost::Message)]
    pub struct HostChangedPayload {
        #[prost(string, tag = "1")]
        pub new_host_id: String,
        #[prost(string, tag = "2")]
        pub new_host_name: String,
    }
    #[derive(Clone, PartialEq, prost::Message)]
    pub struct KickedPayload {
        #[prost(string, tag = "1")]
        pub reason: String,
    }
    #[derive(Clone, PartialEq, prost::Message)]
    pub struct SyncStatePayload {
        #[prost(message, optional, tag = "1")]
        pub current_track: Option<TrackInfo>,
        #[prost(bool, tag = "2")]
        pub is_playing: bool,
        #[prost(int64, tag = "3")]
        pub position: i64,
        #[prost(int64, tag = "4")]
        pub last_update: i64,
        #[prost(message, repeated, tag = "5")]
        pub queue: Vec<TrackInfo>,
        #[prost(float, tag = "6")]
        pub volume: f32,
        #[prost(uint64, tag = "7")]
        pub revision: u64,
    }
    #[derive(Clone, PartialEq, prost::Message)]
    pub struct PongPayload {
        #[prost(int64, tag = "1")]
        pub client_time: i64,
        #[prost(int64, tag = "2")]
        pub server_receive_time: i64,
        #[prost(int64, tag = "3")]
        pub server_send_time: i64,
        #[prost(uint64, tag = "4")]
        pub sequence: u64,
    }
    #[derive(Clone, PartialEq, prost::Message)]
    pub struct ReconnectedPayload {
        #[prost(string, tag = "1")]
        pub room_code: String,
        #[prost(string, tag = "2")]
        pub user_id: String,
        #[prost(message, optional, tag = "3")]
        pub state: Option<RoomState>,
        #[prost(bool, tag = "4")]
        pub is_host: bool,
    }
    #[derive(Clone, PartialEq, prost::Message)]
    pub struct UserReconnectedPayload {
        #[prost(string, tag = "1")]
        pub user_id: String,
        #[prost(string, tag = "2")]
        pub username: String,
    }
    #[derive(Clone, PartialEq, prost::Message)]
    pub struct UserDisconnectedPayload {
        #[prost(string, tag = "1")]
        pub user_id: String,
        #[prost(string, tag = "2")]
        pub username: String,
    }
    #[derive(Clone, PartialEq, prost::Message)]
    pub struct SuggestionReceivedPayload {
        #[prost(string, tag = "1")]
        pub suggestion_id: String,
        #[prost(string, tag = "2")]
        pub from_user_id: String,
        #[prost(string, tag = "3")]
        pub from_username: String,
        #[prost(message, optional, tag = "4")]
        pub track_info: Option<TrackInfo>,
    }
    #[derive(Clone, PartialEq, prost::Message)]
    pub struct SuggestionApprovedPayload {
        #[prost(string, tag = "1")]
        pub suggestion_id: String,
        #[prost(message, optional, tag = "2")]
        pub track_info: Option<TrackInfo>,
    }
    #[derive(Clone, PartialEq, prost::Message)]
    pub struct SuggestionRejectedPayload {
        #[prost(string, tag = "1")]
        pub suggestion_id: String,
        #[prost(string, tag = "2")]
        pub reason: String,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wait_until(timeout: Duration, mut condition: impl FnMut() -> bool) {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            if condition() {
                return;
            }
            thread::sleep(Duration::from_millis(10));
        }
        assert!(condition(), "condition was not met within {timeout:?}");
    }

    fn track(id: &str) -> ListenTogetherTrack {
        ListenTogetherTrack {
            id: id.into(),
            title: "Track".into(),
            artist: "Artist".into(),
            album: Some("Album".into()),
            duration_ms: 123_000,
            thumbnail: Some("https://example.com/cover.jpg".into()),
            suggested_by: None,
            is_episode: false,
        }
    }

    #[test]
    fn protobuf_envelope_matches_android_wire_tags() {
        let payload = proto::CreateRoomPayload {
            username: "Ada".into(),
        }
        .encode_to_vec();
        assert_eq!(payload, b"\x0a\x03Ada");
        let envelope = encode_envelope(message_type::CREATE_ROOM, payload, false).unwrap();
        assert_eq!(envelope, b"\x0a\x0bcreate_room\x12\x05\x0a\x03Ada");
    }

    #[test]
    fn compressed_envelope_round_trips_and_is_bounded() {
        let payload = vec![b'a'; 4_096];
        let envelope = encode_envelope(message_type::SYNC_PLAYBACK, payload.clone(), true).unwrap();
        let decoded_envelope = proto::Envelope::decode(envelope.as_slice()).unwrap();
        assert!(decoded_envelope.compressed);
        let (kind, decoded) = decode_envelope(&envelope).unwrap();
        assert_eq!(kind, message_type::SYNC_PLAYBACK);
        assert_eq!(decoded, payload);
    }

    #[test]
    fn playback_timing_and_revision_round_trip() {
        let mut action =
            ListenTogetherPlaybackActionPayload::new(ListenTogetherPlaybackAction::Play);
        action.track_id = Some("track".into());
        action.position_ms = Some(1_234);
        action.server_time_ms = Some(9_000);
        action.revision = 12;
        action.captured_at_server_time_ms = Some(8_950);

        let decoded = action_from_proto(action_to_proto(action.clone())).unwrap();
        assert_eq!(decoded, action);
    }

    #[test]
    fn room_code_username_and_server_validation_are_strict() {
        assert_eq!(normalize_room_code("ab12cd34".into()).unwrap(), "AB12CD34");
        assert!(normalize_room_code("short".into()).is_err());
        assert!(normalize_username("\n".into()).is_err());
        assert!(normalize_server_url(DEFAULT_LISTEN_TOGETHER_SERVER_URL.into()).is_ok());
        assert!(normalize_server_url("https://example.com/ws".into()).is_err());
        assert!(normalize_server_url("wss://user:pass@example.com/ws".into()).is_err());
    }

    #[test]
    fn tracks_convert_to_and_from_domain_without_transient_urls() {
        let original = track("video-id");
        let song = original.to_song();
        assert_eq!(song.video_id, "video-id");
        assert_eq!(song.artist_line(), "Artist");
        assert_eq!(ListenTogetherTrack::from_song(&song).id, "video-id");
    }

    #[test]
    fn episode_type_survives_a_legacy_server_that_drops_unknown_proto_fields() {
        #[derive(Clone, PartialEq, prost::Message)]
        struct LegacyTrackInfo {
            #[prost(string, tag = "1")]
            id: String,
            #[prost(string, tag = "2")]
            title: String,
            #[prost(string, tag = "3")]
            artist: String,
            #[prost(string, tag = "4")]
            album: String,
            #[prost(int64, tag = "5")]
            duration: i64,
            #[prost(string, tag = "6")]
            thumbnail: String,
            #[prost(string, tag = "7")]
            suggested_by: String,
        }

        let mut episode = track("episode-id");
        episode.thumbnail = Some("https://i.ytimg.com/vi/episode-id/default.jpg".into());
        episode.is_episode = true;
        let encoded = track_to_proto(episode).encode_to_vec();

        let legacy = LegacyTrackInfo::decode(encoded.as_slice()).unwrap();
        assert!(legacy.thumbnail.ends_with("#metrolist_media=episode"));
        let relayed = proto::TrackInfo::decode(legacy.encode_to_vec().as_slice()).unwrap();
        assert!(!relayed.is_episode);

        let decoded = track_from_proto(relayed).unwrap();
        assert!(decoded.is_episode);
        assert_eq!(
            decoded.thumbnail.as_deref(),
            Some("https://i.ytimg.com/vi/episode-id/default.jpg")
        );
        assert!(decoded.to_song().is_episode);
    }

    #[test]
    fn thumbnailless_episode_gets_a_safe_legacy_transport_marker() {
        let mut episode = track("episode/no-cover");
        episode.thumbnail = None;
        episode.is_episode = true;

        let encoded = track_to_proto(episode);
        assert!(encoded.is_episode);
        let decoded = track_from_proto(proto::TrackInfo {
            is_episode: false,
            ..encoded
        })
        .unwrap();
        assert!(decoded.is_episode);
        assert_eq!(
            decoded.thumbnail.as_deref(),
            Some("https://i.ytimg.com/vi/episode%2Fno-cover/default.jpg")
        );
    }

    #[test]
    fn server_clock_rejects_bad_samples_and_advances_playing_position() {
        let mut clock = ServerClock::default();
        thread::sleep(Duration::from_millis(2));
        let sent = clock.elapsed_ms();
        assert!(!clock.record_pong(sent + 10, 1_000, 1_001));
        thread::sleep(Duration::from_millis(2));
        assert!(clock.record_pong(sent, 10_000, 10_001));
        let effective = clock.now_ms().unwrap();
        thread::sleep(Duration::from_millis(3));
        assert!(clock.position_at(500, Some(effective), true) >= 502);
        assert_eq!(clock.position_at(500, Some(effective), false), 500);
    }

    #[test]
    fn stale_revisions_are_rejected_but_legacy_zero_is_accepted() {
        let (_sender, receiver) = mpsc::sync_channel(1);
        let mut worker = Worker::new(
            DEFAULT_LISTEN_TOGETHER_SERVER_URL.into(),
            receiver,
            Arc::new(SharedState::default()),
        );
        assert!(worker.accept_revision(4));
        assert!(!worker.accept_revision(3));
        assert!(worker.accept_revision(4));
        assert!(worker.accept_revision(0));
    }

    #[test]
    fn action_validation_rejects_untrusted_values() {
        let mut action =
            ListenTogetherPlaybackActionPayload::new(ListenTogetherPlaybackAction::SetVolume);
        action.volume = Some(f32::NAN);
        assert!(action.validate().is_err());

        let mut invalid_track = track("id");
        invalid_track.thumbnail = Some("http://example.com/cover.jpg".into());
        assert!(invalid_track.validate().is_err());
    }

    #[test]
    fn incoming_thumbnails_cannot_target_local_networks_or_untrusted_redirectors() {
        let incoming = |thumbnail: &str| proto::TrackInfo {
            id: "video-id".into(),
            title: "Track".into(),
            artist: "Artist".into(),
            album: String::new(),
            duration: 123_000,
            thumbnail: thumbnail.into(),
            suggested_by: String::new(),
            is_episode: false,
        };

        assert!(track_from_proto(incoming("https://127.0.0.1/cover.jpg")).is_err());
        assert!(track_from_proto(incoming("https://metadata.local/cover.jpg")).is_err());
        assert_eq!(
            track_from_proto(incoming("https://example.com/redirect"))
                .unwrap()
                .thumbnail,
            None
        );
        assert_eq!(
            track_from_proto(incoming("https://i.ytimg.com/vi/video-id/default.jpg"))
                .unwrap()
                .thumbnail
                .as_deref(),
            Some("https://i.ytimg.com/vi/video-id/default.jpg")
        );
    }

    #[test]
    fn join_response_room_code_must_match_embedded_room_state() {
        let (_sender, receiver) = mpsc::sync_channel(1);
        let mut worker = Worker::new(
            DEFAULT_LISTEN_TOGETHER_SERVER_URL.into(),
            receiver,
            Arc::new(SharedState::default()),
        );
        let response = proto::JoinApprovedPayload {
            room_code: "AB12CD34".into(),
            user_id: "guest-1".into(),
            session_token: "test-session-token".into(),
            state: Some(proto::RoomState {
                room_code: "ZX98YU76".into(),
                host_id: "host-1".into(),
                users: Vec::new(),
                current_track: None,
                is_playing: false,
                position: 0,
                last_update: 0,
                volume: 1.0,
                queue: Vec::new(),
                revision: 1,
            }),
        };
        let envelope =
            encode_envelope(message_type::JOIN_APPROVED, response.encode_to_vec(), false).unwrap();

        assert!(worker.handle_binary(&envelope).is_err());
        assert_eq!(worker.snapshot_role(), ListenTogetherRoomRole::None);
    }

    #[test]
    fn host_tracker_orders_track_play_queue_seek_volume_and_heartbeat() {
        let mut tracker = ListenTogetherPlaybackTracker::default();
        let started = Instant::now();
        let first = tracker.observe(
            ListenTogetherPlaybackObservation {
                is_host: true,
                current_track: Some(track("one")),
                upcoming_queue: vec![track("two")],
                state: ListenTogetherLocalPlaybackState::Playing,
                position_ms: 1_000,
                volume: 0.8,
                sync_volume: true,
                tempo_milli: 1_000,
            },
            started,
        );
        assert_eq!(
            first.iter().map(|action| action.action).collect::<Vec<_>>(),
            vec![
                ListenTogetherPlaybackAction::ChangeTrack,
                ListenTogetherPlaybackAction::Play,
                ListenTogetherPlaybackAction::SetVolume,
            ]
        );

        let quiet = tracker.observe(
            ListenTogetherPlaybackObservation {
                is_host: true,
                current_track: Some(track("one")),
                upcoming_queue: vec![track("two")],
                state: ListenTogetherLocalPlaybackState::Playing,
                position_ms: 1_250,
                volume: 0.8,
                sync_volume: true,
                tempo_milli: 1_000,
            },
            started + Duration::from_millis(250),
        );
        assert!(quiet.is_empty());

        let updated = tracker.observe(
            ListenTogetherPlaybackObservation {
                is_host: true,
                current_track: Some(track("one")),
                upcoming_queue: vec![track("three")],
                state: ListenTogetherLocalPlaybackState::Playing,
                position_ms: 9_000,
                volume: 0.7,
                sync_volume: true,
                tempo_milli: 1_000,
            },
            started + Duration::from_secs(1),
        );
        assert_eq!(
            updated
                .iter()
                .map(|action| action.action)
                .collect::<Vec<_>>(),
            vec![
                ListenTogetherPlaybackAction::SyncQueue,
                ListenTogetherPlaybackAction::Seek,
                ListenTogetherPlaybackAction::SetVolume,
            ]
        );

        let heartbeat = tracker.observe(
            ListenTogetherPlaybackObservation {
                is_host: true,
                current_track: Some(track("one")),
                upcoming_queue: vec![track("three")],
                state: ListenTogetherLocalPlaybackState::Playing,
                position_ms: 12_100,
                volume: 0.7,
                sync_volume: true,
                tempo_milli: 1_000,
            },
            started + Duration::from_millis(4_100),
        );
        assert_eq!(heartbeat.len(), 1);
        assert_eq!(heartbeat[0].action, ListenTogetherPlaybackAction::Play);
    }

    #[test]
    fn guest_observation_resets_host_tracker_without_emitting() {
        let mut tracker = ListenTogetherPlaybackTracker::default();
        let now = Instant::now();
        let _ = tracker.observe(
            ListenTogetherPlaybackObservation {
                is_host: true,
                current_track: Some(track("one")),
                upcoming_queue: Vec::new(),
                state: ListenTogetherLocalPlaybackState::Paused,
                position_ms: 0,
                volume: 1.0,
                sync_volume: false,
                tempo_milli: 1_000,
            },
            now,
        );
        assert!(
            tracker
                .observe(
                    ListenTogetherPlaybackObservation {
                        is_host: false,
                        current_track: Some(track("one")),
                        upcoming_queue: Vec::new(),
                        state: ListenTogetherLocalPlaybackState::Playing,
                        position_ms: 500,
                        volume: 1.0,
                        sync_volume: false,
                        tempo_milli: 1_000,
                    },
                    now + Duration::from_secs(1),
                )
                .is_empty()
        );
    }

    #[test]
    fn client_interoperates_with_a_local_protobuf_websocket_server() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut socket = tungstenite::accept(stream).unwrap();

            loop {
                let Message::Binary(bytes) = socket.read().unwrap() else {
                    continue;
                };
                let (kind, payload) = decode_envelope(&bytes).unwrap();
                if kind == message_type::PING {
                    continue;
                }
                assert_eq!(kind, message_type::CREATE_ROOM);
                let create = proto::CreateRoomPayload::decode(payload.as_slice()).unwrap();
                assert_eq!(create.username, "Ada");
                break;
            }

            let room_created = proto::RoomCreatedPayload {
                room_code: "AB12CD34".into(),
                user_id: "host-1".into(),
                session_token: "test-session-token".into(),
            };
            socket
                .send(Message::binary(
                    encode_envelope(
                        message_type::ROOM_CREATED,
                        room_created.encode_to_vec(),
                        false,
                    )
                    .unwrap(),
                ))
                .unwrap();
            socket
                .send(Message::binary(
                    encode_envelope(
                        message_type::JOIN_REQUEST,
                        proto::JoinRequestPayload {
                            user_id: "guest-1".into(),
                            username: "Grace".into(),
                        }
                        .encode_to_vec(),
                        false,
                    )
                    .unwrap(),
                ))
                .unwrap();

            loop {
                let Message::Binary(bytes) = socket.read().unwrap() else {
                    continue;
                };
                let (kind, payload) = decode_envelope(&bytes).unwrap();
                if kind == message_type::PING {
                    continue;
                }
                assert_eq!(kind, message_type::APPROVE_JOIN);
                let approval = proto::ApproveJoinPayload::decode(payload.as_slice()).unwrap();
                assert_eq!(approval.user_id, "guest-1");
                break;
            }

            socket
                .send(Message::binary(
                    encode_envelope(
                        message_type::USER_JOINED,
                        proto::UserJoinedPayload {
                            user_id: "guest-1".into(),
                            username: "Grace".into(),
                        }
                        .encode_to_vec(),
                        false,
                    )
                    .unwrap(),
                ))
                .unwrap();

            loop {
                let Message::Binary(bytes) = socket.read().unwrap() else {
                    continue;
                };
                let (kind, _) = decode_envelope(&bytes).unwrap();
                if kind == message_type::PING {
                    continue;
                }
                assert_eq!(kind, message_type::LEAVE_ROOM);
                break;
            }
        });

        let client = ListenTogetherClient::new(format!("ws://{address}")).unwrap();
        client.create_room("Ada").unwrap();
        wait_until(Duration::from_secs(3), || {
            let snapshot = client.snapshot();
            snapshot.role == ListenTogetherRoomRole::Host
                && snapshot.room.as_ref().map(|room| room.room_code.as_str()) == Some("AB12CD34")
                && snapshot.pending_join_requests.len() == 1
        });
        client.approve_join("guest-1").unwrap();
        wait_until(Duration::from_secs(3), || {
            let snapshot = client.snapshot();
            snapshot.pending_join_requests.is_empty()
                && snapshot
                    .room
                    .as_ref()
                    .is_some_and(|room| room.users.iter().any(|user| user.user_id == "guest-1"))
        });
        client.leave_room().unwrap();
        server.join().unwrap();
        client.disconnect().unwrap();
    }
}
