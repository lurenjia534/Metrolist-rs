use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    sync::{Arc, Mutex, mpsc},
    thread,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use futures::{FutureExt as _, channel::oneshot, future::BoxFuture};
use rusqlite::{Connection, OptionalExtension as _, Transaction, params};

use crate::config::{
    AppSettings, AppTheme, AudioQuality, EqualizerProfile, EqualizerSettings,
    ListenTogetherSettings, LoudnessLevel, ParametricEqualizer, PlaybackParameters, ProxyKind,
    ProxySettings,
};
#[cfg(test)]
use crate::domain::ArtistCredit;
use crate::domain::{BrowseItem, BrowseKind, LyricsDocument, LyricsLine, Song};
use crate::services::{RecognitionResult, RepeatMode};
use crate::{AppError, Result};

const SCHEMA_VERSION: i64 = 23;
const FORGOTTEN_FAVORITES_WINDOW_MS: i64 = 30 * 24 * 60 * 60 * 1_000;
const KEEP_LISTENING_WINDOW_MS: i64 = 14 * 24 * 60 * 60 * 1_000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistoryEntry {
    pub id: i64,
    pub song: Song,
    pub played_at_ms: i64,
    pub play_time: Duration,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SongListeningStats {
    pub song: Song,
    pub play_count: u64,
    pub play_time: Duration,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtistListeningStats {
    pub id: Option<String>,
    pub name: String,
    pub play_count: u64,
    pub play_time: Duration,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AlbumListeningStats {
    pub browse_id: String,
    pub title: String,
    pub thumbnail_url: Option<String>,
    pub play_count: u64,
    pub play_time: Duration,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListeningStats {
    pub play_count: u64,
    pub unique_songs: usize,
    pub unique_artists: usize,
    pub unique_albums: usize,
    pub play_time: Duration,
    pub top_songs: Vec<SongListeningStats>,
    pub top_artists: Vec<ArtistListeningStats>,
    pub top_albums: Vec<AlbumListeningStats>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecognitionHistoryEntry {
    pub id: i64,
    pub result: RecognitionResult,
    pub recognized_at_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchHistoryEntry {
    pub id: i64,
    pub query: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FavoriteEntry {
    pub song: Song,
    pub liked_at_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PodcastSubscription {
    pub podcast_id: String,
    pub title: String,
    pub author: Option<String>,
    pub thumbnail_url: Option<String>,
    pub channel_id: Option<String>,
    pub subscribed_at_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SavedEpisode {
    pub song: Song,
    pub saved_at_ms: i64,
    pub playback_position: Option<Duration>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PodcastLibraryReconcileSummary {
    pub podcast_count: usize,
    pub episode_count: usize,
    pub removed_podcast_count: usize,
    pub removed_episode_count: usize,
    pub skipped_podcast_tombstones: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DownloadState {
    Queued,
    Downloading,
    Paused,
    Completed,
    Failed,
}

impl DownloadState {
    pub const fn label(self) -> &'static str {
        match self {
            Self::Queued => "Queued",
            Self::Downloading => "Downloading",
            Self::Paused => "Paused",
            Self::Completed => "Downloaded",
            Self::Failed => "Failed",
        }
    }

    const fn storage_value(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Downloading => "downloading",
            Self::Paused => "paused",
            Self::Completed => "completed",
            Self::Failed => "failed",
        }
    }

    fn from_storage(value: &str) -> Result<Self> {
        match value {
            "queued" => Ok(Self::Queued),
            "downloading" => Ok(Self::Downloading),
            "paused" => Ok(Self::Paused),
            "completed" => Ok(Self::Completed),
            "failed" => Ok(Self::Failed),
            _ => Err(AppError::Storage(format!(
                "unknown stored download state '{value}'"
            ))),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioDownload {
    pub song: Song,
    pub audio_quality: AudioQuality,
    pub mime_type: Option<String>,
    pub content_length: Option<u64>,
    pub loudness_lufs_mb: Option<i32>,
    pub downloaded_bytes: u64,
    pub state: DownloadState,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
    pub completed_at_ms: Option<i64>,
    pub last_error: Option<String>,
}

impl AudioDownload {
    pub fn cache_key(&self) -> String {
        self.audio_quality.playback_cache_key(&self.song.video_id)
    }

    pub fn resource_key(&self) -> Option<String> {
        self.content_length
            .map(|content_length| format!("{}-{content_length}", self.cache_key()))
    }

    pub fn is_complete(&self) -> bool {
        self.state == DownloadState::Completed
            && self
                .mime_type
                .as_deref()
                .is_some_and(|mime| mime.starts_with("audio/"))
            && self
                .content_length
                .is_some_and(|length| length > 0 && self.downloaded_bytes == length)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalPlaylist {
    pub id: i64,
    pub name: String,
    pub song_count: usize,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PlaylistSort {
    #[default]
    CreatedAt,
    Name,
    SongCount,
    UpdatedAt,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SortDirection {
    Ascending,
    #[default]
    Descending,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PersistedSession {
    pub queue: Vec<Song>,
    pub current_index: Option<usize>,
    pub position: Duration,
    pub volume: f32,
    pub repeat_mode: RepeatMode,
    pub shuffle_enabled: bool,
    pub playback_source: Option<PersistedPlaybackSource>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PersistedPlaybackSource {
    pub video_id: String,
    pub mime_type: String,
    pub content_length: u64,
    pub loudness_lufs_mb: Option<i32>,
    pub resolved_at_ms: i64,
    pub expires_at_ms: i64,
}

enum StoreCommand {
    RecordHistory {
        song: Song,
        play_time: Duration,
        reply: oneshot::Sender<Result<()>>,
    },
    RecentHistory {
        limit: usize,
        reply: oneshot::Sender<Result<Vec<HistoryEntry>>>,
    },
    ForgottenFavorites {
        limit: usize,
        reply: oneshot::Sender<Result<Vec<Song>>>,
    },
    KeepListening {
        limit: usize,
        offset: usize,
        reply: oneshot::Sender<Result<Vec<Song>>>,
    },
    ClearHistory {
        reply: oneshot::Sender<Result<()>>,
    },
    DeleteHistoryEntry {
        id: i64,
        reply: oneshot::Sender<Result<()>>,
    },
    ListeningStats {
        start_ms: i64,
        limit: usize,
        reply: oneshot::Sender<Result<ListeningStats>>,
    },
    RecordRecognition {
        result: RecognitionResult,
        reply: oneshot::Sender<Result<RecognitionHistoryEntry>>,
    },
    RecognitionHistory {
        limit: usize,
        reply: oneshot::Sender<Result<Vec<RecognitionHistoryEntry>>>,
    },
    DeleteRecognitionHistory {
        id: i64,
        reply: oneshot::Sender<Result<()>>,
    },
    ClearRecognitionHistory {
        reply: oneshot::Sender<Result<()>>,
    },
    RecordSearchQuery {
        query: String,
        reply: oneshot::Sender<Result<SearchHistoryEntry>>,
    },
    SearchHistory {
        limit: usize,
        reply: oneshot::Sender<Result<Vec<SearchHistoryEntry>>>,
    },
    DeleteSearchHistory {
        id: i64,
        reply: oneshot::Sender<Result<()>>,
    },
    ClearSearchHistory {
        reply: oneshot::Sender<Result<()>>,
    },
    RememberCatalogItems {
        items: Vec<BrowseItem>,
        reply: oneshot::Sender<Result<()>>,
    },
    CatalogItems {
        limit: usize,
        reply: oneshot::Sender<Result<Vec<BrowseItem>>>,
    },
    SetFavorite {
        song: Song,
        favorite: bool,
        reply: oneshot::Sender<Result<()>>,
    },
    Favorites {
        limit: usize,
        reply: oneshot::Sender<Result<Vec<FavoriteEntry>>>,
    },
    SetPodcastSubscription {
        podcast: PodcastSubscription,
        subscribed: bool,
        reply: oneshot::Sender<Result<()>>,
    },
    PodcastSubscriptions {
        reply: oneshot::Sender<Result<Vec<PodcastSubscription>>>,
    },
    SetEpisodeForLater {
        song: Song,
        saved: bool,
        reply: oneshot::Sender<Result<()>>,
    },
    EpisodesForLater {
        reply: oneshot::Sender<Result<Vec<SavedEpisode>>>,
    },
    SaveEpisodePlaybackPosition {
        song: Song,
        position: Duration,
        reply: oneshot::Sender<Result<()>>,
    },
    EpisodePlaybackPosition {
        video_id: String,
        reply: oneshot::Sender<Result<Option<Duration>>>,
    },
    ReconcilePodcastLibrary {
        podcasts: Vec<PodcastSubscription>,
        episodes: Vec<Song>,
        reply: oneshot::Sender<Result<PodcastLibraryReconcileSummary>>,
    },
    CreatePlaylist {
        name: String,
        reply: oneshot::Sender<Result<LocalPlaylist>>,
    },
    RenamePlaylist {
        playlist_id: i64,
        name: String,
        reply: oneshot::Sender<Result<LocalPlaylist>>,
    },
    Playlists {
        sort: PlaylistSort,
        direction: SortDirection,
        reply: oneshot::Sender<Result<Vec<LocalPlaylist>>>,
    },
    AddToPlaylist {
        playlist_id: i64,
        song: Song,
        reply: oneshot::Sender<Result<()>>,
    },
    PlaylistSongs {
        playlist_id: i64,
        reply: oneshot::Sender<Result<Vec<Song>>>,
    },
    RemoveFromPlaylist {
        playlist_id: i64,
        video_id: String,
        reply: oneshot::Sender<Result<()>>,
    },
    DeletePlaylist {
        playlist_id: i64,
        reply: oneshot::Sender<Result<()>>,
    },
    SaveSession {
        session: PersistedSession,
        reply: oneshot::Sender<Result<()>>,
    },
    LoadSession {
        reply: oneshot::Sender<Result<Option<PersistedSession>>>,
    },
    SaveLyrics {
        song: Song,
        document: LyricsDocument,
        reply: oneshot::Sender<Result<()>>,
    },
    LoadLyrics {
        video_id: String,
        reply: oneshot::Sender<Result<Option<LyricsDocument>>>,
    },
    SaveSettings {
        settings: Box<AppSettings>,
        reply: oneshot::Sender<Result<()>>,
    },
    LoadSettings {
        reply: oneshot::Sender<Result<Option<AppSettings>>>,
    },
    SaveEqualizerProfile {
        profile: EqualizerProfile,
        reply: oneshot::Sender<Result<()>>,
    },
    SaveEqualizerProfiles {
        profiles: Vec<EqualizerProfile>,
        reply: oneshot::Sender<Result<()>>,
    },
    EqualizerProfiles {
        reply: oneshot::Sender<Result<Vec<EqualizerProfile>>>,
    },
    DeleteEqualizerProfile {
        profile_id: String,
        reply: oneshot::Sender<Result<()>>,
    },
    QueueDownload {
        song: Song,
        audio_quality: AudioQuality,
        reply: oneshot::Sender<Result<AudioDownload>>,
    },
    MarkDownloadStarted {
        video_id: String,
        mime_type: String,
        content_length: u64,
        downloaded_bytes: u64,
        loudness_lufs_mb: Option<i32>,
        reply: oneshot::Sender<Result<()>>,
    },
    UpdateDownloadProgress {
        video_id: String,
        downloaded_bytes: u64,
        reply: oneshot::Sender<Result<()>>,
    },
    FinishDownload {
        video_id: String,
        reply: oneshot::Sender<Result<()>>,
    },
    StopDownload {
        video_id: String,
        state: DownloadState,
        error: Option<String>,
        reply: oneshot::Sender<Result<()>>,
    },
    Downloads {
        reply: oneshot::Sender<Result<Vec<AudioDownload>>>,
    },
    DeleteDownload {
        video_id: String,
        reply: oneshot::Sender<Result<()>>,
    },
    Shutdown,
}

struct StoreInner {
    commands: mpsc::Sender<StoreCommand>,
    worker: Mutex<Option<thread::JoinHandle<()>>>,
}

impl Drop for StoreInner {
    fn drop(&mut self) {
        let _ = self.commands.send(StoreCommand::Shutdown);
        if let Ok(worker) = self.worker.get_mut()
            && let Some(worker) = worker.take()
        {
            let _ = worker.join();
        }
    }
}

#[derive(Clone)]
pub struct DesktopStore {
    inner: Arc<StoreInner>,
}

impl DesktopStore {
    pub fn open_default() -> Result<Self> {
        let path = dirs::data_local_dir()
            .ok_or_else(|| AppError::Storage("the operating system has no data directory".into()))?
            .join("metrolist")
            .join("metrolist.sqlite3");
        Self::open(path)
    }

    pub fn open(path: impl Into<PathBuf>) -> Result<Self> {
        let path = path.into();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|error| AppError::Storage(error.to_string()))?;
        }

        let (commands, receiver) = mpsc::channel();
        let (ready_tx, ready_rx) = mpsc::sync_channel(1);
        let worker = thread::Builder::new()
            .name("metrolist-storage".into())
            .spawn(move || run_store_worker(&path, receiver, ready_tx))
            .map_err(|error| AppError::Storage(error.to_string()))?;
        match ready_rx.recv() {
            Ok(Ok(())) => Ok(Self {
                inner: Arc::new(StoreInner {
                    commands,
                    worker: Mutex::new(Some(worker)),
                }),
            }),
            Ok(Err(error)) => {
                let _ = worker.join();
                Err(error)
            }
            Err(_) => {
                let _ = worker.join();
                Err(AppError::Storage(
                    "storage worker stopped during initialization".into(),
                ))
            }
        }
    }

    pub fn record_history(
        &self,
        song: Song,
        play_time: Duration,
    ) -> BoxFuture<'static, Result<()>> {
        let (reply, receiver) = oneshot::channel();
        if self
            .inner
            .commands
            .send(StoreCommand::RecordHistory {
                song,
                play_time,
                reply,
            })
            .is_err()
        {
            return futures::future::ready(Err(storage_stopped())).boxed();
        }
        async move { receiver.await.map_err(|_| storage_stopped())? }.boxed()
    }

    pub fn recent_history(&self, limit: usize) -> BoxFuture<'static, Result<Vec<HistoryEntry>>> {
        let (reply, receiver) = oneshot::channel();
        if self
            .inner
            .commands
            .send(StoreCommand::RecentHistory { limit, reply })
            .is_err()
        {
            return futures::future::ready(Err(storage_stopped())).boxed();
        }
        async move { receiver.await.map_err(|_| storage_stopped())? }.boxed()
    }

    pub fn forgotten_favorites(&self, limit: usize) -> BoxFuture<'static, Result<Vec<Song>>> {
        let (reply, receiver) = oneshot::channel();
        if self
            .inner
            .commands
            .send(StoreCommand::ForgottenFavorites { limit, reply })
            .is_err()
        {
            return futures::future::ready(Err(storage_stopped())).boxed();
        }
        async move { receiver.await.map_err(|_| storage_stopped())? }.boxed()
    }

    pub fn keep_listening(
        &self,
        limit: usize,
        offset: usize,
    ) -> BoxFuture<'static, Result<Vec<Song>>> {
        let (reply, receiver) = oneshot::channel();
        if self
            .inner
            .commands
            .send(StoreCommand::KeepListening {
                limit,
                offset,
                reply,
            })
            .is_err()
        {
            return futures::future::ready(Err(storage_stopped())).boxed();
        }
        async move { receiver.await.map_err(|_| storage_stopped())? }.boxed()
    }

    pub fn clear_history(&self) -> BoxFuture<'static, Result<()>> {
        let (reply, receiver) = oneshot::channel();
        if self
            .inner
            .commands
            .send(StoreCommand::ClearHistory { reply })
            .is_err()
        {
            return futures::future::ready(Err(storage_stopped())).boxed();
        }
        async move { receiver.await.map_err(|_| storage_stopped())? }.boxed()
    }

    pub fn delete_history_entry(&self, id: i64) -> BoxFuture<'static, Result<()>> {
        let (reply, receiver) = oneshot::channel();
        if self
            .inner
            .commands
            .send(StoreCommand::DeleteHistoryEntry { id, reply })
            .is_err()
        {
            return futures::future::ready(Err(storage_stopped())).boxed();
        }
        async move { receiver.await.map_err(|_| storage_stopped())? }.boxed()
    }

    pub fn listening_stats(
        &self,
        start_ms: i64,
        limit: usize,
    ) -> BoxFuture<'static, Result<ListeningStats>> {
        let (reply, receiver) = oneshot::channel();
        if self
            .inner
            .commands
            .send(StoreCommand::ListeningStats {
                start_ms,
                limit,
                reply,
            })
            .is_err()
        {
            return futures::future::ready(Err(storage_stopped())).boxed();
        }
        async move { receiver.await.map_err(|_| storage_stopped())? }.boxed()
    }

    pub fn record_recognition(
        &self,
        result: RecognitionResult,
    ) -> BoxFuture<'static, Result<RecognitionHistoryEntry>> {
        let (reply, receiver) = oneshot::channel();
        if self
            .inner
            .commands
            .send(StoreCommand::RecordRecognition { result, reply })
            .is_err()
        {
            return futures::future::ready(Err(storage_stopped())).boxed();
        }
        async move { receiver.await.map_err(|_| storage_stopped())? }.boxed()
    }

    pub fn recognition_history(
        &self,
        limit: usize,
    ) -> BoxFuture<'static, Result<Vec<RecognitionHistoryEntry>>> {
        let (reply, receiver) = oneshot::channel();
        if self
            .inner
            .commands
            .send(StoreCommand::RecognitionHistory { limit, reply })
            .is_err()
        {
            return futures::future::ready(Err(storage_stopped())).boxed();
        }
        async move { receiver.await.map_err(|_| storage_stopped())? }.boxed()
    }

    pub fn delete_recognition_history(&self, id: i64) -> BoxFuture<'static, Result<()>> {
        let (reply, receiver) = oneshot::channel();
        if self
            .inner
            .commands
            .send(StoreCommand::DeleteRecognitionHistory { id, reply })
            .is_err()
        {
            return futures::future::ready(Err(storage_stopped())).boxed();
        }
        async move { receiver.await.map_err(|_| storage_stopped())? }.boxed()
    }

    pub fn clear_recognition_history(&self) -> BoxFuture<'static, Result<()>> {
        let (reply, receiver) = oneshot::channel();
        if self
            .inner
            .commands
            .send(StoreCommand::ClearRecognitionHistory { reply })
            .is_err()
        {
            return futures::future::ready(Err(storage_stopped())).boxed();
        }
        async move { receiver.await.map_err(|_| storage_stopped())? }.boxed()
    }

    pub fn record_search_query(
        &self,
        query: String,
    ) -> BoxFuture<'static, Result<SearchHistoryEntry>> {
        let (reply, receiver) = oneshot::channel();
        if self
            .inner
            .commands
            .send(StoreCommand::RecordSearchQuery { query, reply })
            .is_err()
        {
            return futures::future::ready(Err(storage_stopped())).boxed();
        }
        async move { receiver.await.map_err(|_| storage_stopped())? }.boxed()
    }

    pub fn search_history(
        &self,
        limit: usize,
    ) -> BoxFuture<'static, Result<Vec<SearchHistoryEntry>>> {
        let (reply, receiver) = oneshot::channel();
        if self
            .inner
            .commands
            .send(StoreCommand::SearchHistory { limit, reply })
            .is_err()
        {
            return futures::future::ready(Err(storage_stopped())).boxed();
        }
        async move { receiver.await.map_err(|_| storage_stopped())? }.boxed()
    }

    pub fn delete_search_history(&self, id: i64) -> BoxFuture<'static, Result<()>> {
        let (reply, receiver) = oneshot::channel();
        if self
            .inner
            .commands
            .send(StoreCommand::DeleteSearchHistory { id, reply })
            .is_err()
        {
            return futures::future::ready(Err(storage_stopped())).boxed();
        }
        async move { receiver.await.map_err(|_| storage_stopped())? }.boxed()
    }

    pub fn clear_search_history(&self) -> BoxFuture<'static, Result<()>> {
        let (reply, receiver) = oneshot::channel();
        if self
            .inner
            .commands
            .send(StoreCommand::ClearSearchHistory { reply })
            .is_err()
        {
            return futures::future::ready(Err(storage_stopped())).boxed();
        }
        async move { receiver.await.map_err(|_| storage_stopped())? }.boxed()
    }

    pub fn remember_catalog_items(&self, items: Vec<BrowseItem>) -> BoxFuture<'static, Result<()>> {
        let (reply, receiver) = oneshot::channel();
        if self
            .inner
            .commands
            .send(StoreCommand::RememberCatalogItems { items, reply })
            .is_err()
        {
            return futures::future::ready(Err(storage_stopped())).boxed();
        }
        async move { receiver.await.map_err(|_| storage_stopped())? }.boxed()
    }

    pub fn catalog_items(&self, limit: usize) -> BoxFuture<'static, Result<Vec<BrowseItem>>> {
        let (reply, receiver) = oneshot::channel();
        if self
            .inner
            .commands
            .send(StoreCommand::CatalogItems { limit, reply })
            .is_err()
        {
            return futures::future::ready(Err(storage_stopped())).boxed();
        }
        async move { receiver.await.map_err(|_| storage_stopped())? }.boxed()
    }

    pub fn set_favorite(&self, song: Song, favorite: bool) -> BoxFuture<'static, Result<()>> {
        let (reply, receiver) = oneshot::channel();
        if self
            .inner
            .commands
            .send(StoreCommand::SetFavorite {
                song,
                favorite,
                reply,
            })
            .is_err()
        {
            return futures::future::ready(Err(storage_stopped())).boxed();
        }
        async move { receiver.await.map_err(|_| storage_stopped())? }.boxed()
    }

    pub fn favorites(&self, limit: usize) -> BoxFuture<'static, Result<Vec<FavoriteEntry>>> {
        let (reply, receiver) = oneshot::channel();
        if self
            .inner
            .commands
            .send(StoreCommand::Favorites { limit, reply })
            .is_err()
        {
            return futures::future::ready(Err(storage_stopped())).boxed();
        }
        async move { receiver.await.map_err(|_| storage_stopped())? }.boxed()
    }

    pub fn set_podcast_subscription(
        &self,
        podcast: PodcastSubscription,
        subscribed: bool,
    ) -> BoxFuture<'static, Result<()>> {
        let (reply, receiver) = oneshot::channel();
        if self
            .inner
            .commands
            .send(StoreCommand::SetPodcastSubscription {
                podcast,
                subscribed,
                reply,
            })
            .is_err()
        {
            return futures::future::ready(Err(storage_stopped())).boxed();
        }
        async move { receiver.await.map_err(|_| storage_stopped())? }.boxed()
    }

    pub fn podcast_subscriptions(&self) -> BoxFuture<'static, Result<Vec<PodcastSubscription>>> {
        let (reply, receiver) = oneshot::channel();
        if self
            .inner
            .commands
            .send(StoreCommand::PodcastSubscriptions { reply })
            .is_err()
        {
            return futures::future::ready(Err(storage_stopped())).boxed();
        }
        async move { receiver.await.map_err(|_| storage_stopped())? }.boxed()
    }

    pub fn set_episode_for_later(&self, song: Song, saved: bool) -> BoxFuture<'static, Result<()>> {
        let (reply, receiver) = oneshot::channel();
        if self
            .inner
            .commands
            .send(StoreCommand::SetEpisodeForLater { song, saved, reply })
            .is_err()
        {
            return futures::future::ready(Err(storage_stopped())).boxed();
        }
        async move { receiver.await.map_err(|_| storage_stopped())? }.boxed()
    }

    pub fn episodes_for_later(&self) -> BoxFuture<'static, Result<Vec<SavedEpisode>>> {
        let (reply, receiver) = oneshot::channel();
        if self
            .inner
            .commands
            .send(StoreCommand::EpisodesForLater { reply })
            .is_err()
        {
            return futures::future::ready(Err(storage_stopped())).boxed();
        }
        async move { receiver.await.map_err(|_| storage_stopped())? }.boxed()
    }

    pub fn save_episode_playback_position(
        &self,
        song: Song,
        position: Duration,
    ) -> BoxFuture<'static, Result<()>> {
        let (reply, receiver) = oneshot::channel();
        if self
            .inner
            .commands
            .send(StoreCommand::SaveEpisodePlaybackPosition {
                song,
                position,
                reply,
            })
            .is_err()
        {
            return futures::future::ready(Err(storage_stopped())).boxed();
        }
        async move { receiver.await.map_err(|_| storage_stopped())? }.boxed()
    }

    pub fn episode_playback_position(
        &self,
        video_id: String,
    ) -> BoxFuture<'static, Result<Option<Duration>>> {
        let (reply, receiver) = oneshot::channel();
        if self
            .inner
            .commands
            .send(StoreCommand::EpisodePlaybackPosition { video_id, reply })
            .is_err()
        {
            return futures::future::ready(Err(storage_stopped())).boxed();
        }
        async move { receiver.await.map_err(|_| storage_stopped())? }.boxed()
    }

    pub fn reconcile_podcast_library(
        &self,
        podcasts: Vec<PodcastSubscription>,
        episodes: Vec<Song>,
    ) -> BoxFuture<'static, Result<PodcastLibraryReconcileSummary>> {
        let (reply, receiver) = oneshot::channel();
        if self
            .inner
            .commands
            .send(StoreCommand::ReconcilePodcastLibrary {
                podcasts,
                episodes,
                reply,
            })
            .is_err()
        {
            return futures::future::ready(Err(storage_stopped())).boxed();
        }
        async move { receiver.await.map_err(|_| storage_stopped())? }.boxed()
    }

    pub fn create_playlist(&self, name: String) -> BoxFuture<'static, Result<LocalPlaylist>> {
        let (reply, receiver) = oneshot::channel();
        if self
            .inner
            .commands
            .send(StoreCommand::CreatePlaylist { name, reply })
            .is_err()
        {
            return futures::future::ready(Err(storage_stopped())).boxed();
        }
        async move { receiver.await.map_err(|_| storage_stopped())? }.boxed()
    }

    pub fn rename_playlist(
        &self,
        playlist_id: i64,
        name: String,
    ) -> BoxFuture<'static, Result<LocalPlaylist>> {
        let (reply, receiver) = oneshot::channel();
        if self
            .inner
            .commands
            .send(StoreCommand::RenamePlaylist {
                playlist_id,
                name,
                reply,
            })
            .is_err()
        {
            return futures::future::ready(Err(storage_stopped())).boxed();
        }
        async move { receiver.await.map_err(|_| storage_stopped())? }.boxed()
    }

    pub fn playlists(&self) -> BoxFuture<'static, Result<Vec<LocalPlaylist>>> {
        self.playlists_sorted(PlaylistSort::default(), SortDirection::default())
    }

    pub fn playlists_sorted(
        &self,
        sort: PlaylistSort,
        direction: SortDirection,
    ) -> BoxFuture<'static, Result<Vec<LocalPlaylist>>> {
        let (reply, receiver) = oneshot::channel();
        if self
            .inner
            .commands
            .send(StoreCommand::Playlists {
                sort,
                direction,
                reply,
            })
            .is_err()
        {
            return futures::future::ready(Err(storage_stopped())).boxed();
        }
        async move { receiver.await.map_err(|_| storage_stopped())? }.boxed()
    }

    pub fn add_to_playlist(&self, playlist_id: i64, song: Song) -> BoxFuture<'static, Result<()>> {
        let (reply, receiver) = oneshot::channel();
        if self
            .inner
            .commands
            .send(StoreCommand::AddToPlaylist {
                playlist_id,
                song,
                reply,
            })
            .is_err()
        {
            return futures::future::ready(Err(storage_stopped())).boxed();
        }
        async move { receiver.await.map_err(|_| storage_stopped())? }.boxed()
    }

    pub fn playlist_songs(&self, playlist_id: i64) -> BoxFuture<'static, Result<Vec<Song>>> {
        let (reply, receiver) = oneshot::channel();
        if self
            .inner
            .commands
            .send(StoreCommand::PlaylistSongs { playlist_id, reply })
            .is_err()
        {
            return futures::future::ready(Err(storage_stopped())).boxed();
        }
        async move { receiver.await.map_err(|_| storage_stopped())? }.boxed()
    }

    pub fn remove_from_playlist(
        &self,
        playlist_id: i64,
        video_id: String,
    ) -> BoxFuture<'static, Result<()>> {
        let (reply, receiver) = oneshot::channel();
        if self
            .inner
            .commands
            .send(StoreCommand::RemoveFromPlaylist {
                playlist_id,
                video_id,
                reply,
            })
            .is_err()
        {
            return futures::future::ready(Err(storage_stopped())).boxed();
        }
        async move { receiver.await.map_err(|_| storage_stopped())? }.boxed()
    }

    pub fn delete_playlist(&self, playlist_id: i64) -> BoxFuture<'static, Result<()>> {
        let (reply, receiver) = oneshot::channel();
        if self
            .inner
            .commands
            .send(StoreCommand::DeletePlaylist { playlist_id, reply })
            .is_err()
        {
            return futures::future::ready(Err(storage_stopped())).boxed();
        }
        async move { receiver.await.map_err(|_| storage_stopped())? }.boxed()
    }

    pub fn save_session(&self, session: PersistedSession) -> BoxFuture<'static, Result<()>> {
        let (reply, receiver) = oneshot::channel();
        if self
            .inner
            .commands
            .send(StoreCommand::SaveSession { session, reply })
            .is_err()
        {
            return futures::future::ready(Err(storage_stopped())).boxed();
        }
        async move { receiver.await.map_err(|_| storage_stopped())? }.boxed()
    }

    pub fn load_session(&self) -> BoxFuture<'static, Result<Option<PersistedSession>>> {
        let (reply, receiver) = oneshot::channel();
        if self
            .inner
            .commands
            .send(StoreCommand::LoadSession { reply })
            .is_err()
        {
            return futures::future::ready(Err(storage_stopped())).boxed();
        }
        async move { receiver.await.map_err(|_| storage_stopped())? }.boxed()
    }

    pub fn save_lyrics(
        &self,
        song: Song,
        document: LyricsDocument,
    ) -> BoxFuture<'static, Result<()>> {
        let (reply, receiver) = oneshot::channel();
        if self
            .inner
            .commands
            .send(StoreCommand::SaveLyrics {
                song,
                document,
                reply,
            })
            .is_err()
        {
            return futures::future::ready(Err(storage_stopped())).boxed();
        }
        async move { receiver.await.map_err(|_| storage_stopped())? }.boxed()
    }

    pub fn load_lyrics(
        &self,
        video_id: String,
    ) -> BoxFuture<'static, Result<Option<LyricsDocument>>> {
        let (reply, receiver) = oneshot::channel();
        if self
            .inner
            .commands
            .send(StoreCommand::LoadLyrics { video_id, reply })
            .is_err()
        {
            return futures::future::ready(Err(storage_stopped())).boxed();
        }
        async move { receiver.await.map_err(|_| storage_stopped())? }.boxed()
    }

    pub fn save_settings(&self, settings: AppSettings) -> BoxFuture<'static, Result<()>> {
        let (reply, receiver) = oneshot::channel();
        if self
            .inner
            .commands
            .send(StoreCommand::SaveSettings {
                settings: Box::new(settings),
                reply,
            })
            .is_err()
        {
            return futures::future::ready(Err(storage_stopped())).boxed();
        }
        async move { receiver.await.map_err(|_| storage_stopped())? }.boxed()
    }

    pub fn load_settings(&self) -> BoxFuture<'static, Result<Option<AppSettings>>> {
        let (reply, receiver) = oneshot::channel();
        if self
            .inner
            .commands
            .send(StoreCommand::LoadSettings { reply })
            .is_err()
        {
            return futures::future::ready(Err(storage_stopped())).boxed();
        }
        async move { receiver.await.map_err(|_| storage_stopped())? }.boxed()
    }

    pub fn save_equalizer_profile(
        &self,
        profile: EqualizerProfile,
    ) -> BoxFuture<'static, Result<()>> {
        let (reply, receiver) = oneshot::channel();
        if self
            .inner
            .commands
            .send(StoreCommand::SaveEqualizerProfile { profile, reply })
            .is_err()
        {
            return futures::future::ready(Err(storage_stopped())).boxed();
        }
        async move { receiver.await.map_err(|_| storage_stopped())? }.boxed()
    }

    pub fn save_equalizer_profiles(
        &self,
        profiles: Vec<EqualizerProfile>,
    ) -> BoxFuture<'static, Result<()>> {
        let (reply, receiver) = oneshot::channel();
        if self
            .inner
            .commands
            .send(StoreCommand::SaveEqualizerProfiles { profiles, reply })
            .is_err()
        {
            return futures::future::ready(Err(storage_stopped())).boxed();
        }
        async move { receiver.await.map_err(|_| storage_stopped())? }.boxed()
    }

    pub fn equalizer_profiles(&self) -> BoxFuture<'static, Result<Vec<EqualizerProfile>>> {
        let (reply, receiver) = oneshot::channel();
        if self
            .inner
            .commands
            .send(StoreCommand::EqualizerProfiles { reply })
            .is_err()
        {
            return futures::future::ready(Err(storage_stopped())).boxed();
        }
        async move { receiver.await.map_err(|_| storage_stopped())? }.boxed()
    }

    pub fn delete_equalizer_profile(&self, profile_id: String) -> BoxFuture<'static, Result<()>> {
        let (reply, receiver) = oneshot::channel();
        if self
            .inner
            .commands
            .send(StoreCommand::DeleteEqualizerProfile { profile_id, reply })
            .is_err()
        {
            return futures::future::ready(Err(storage_stopped())).boxed();
        }
        async move { receiver.await.map_err(|_| storage_stopped())? }.boxed()
    }

    pub fn queue_download(
        &self,
        song: Song,
        audio_quality: AudioQuality,
    ) -> BoxFuture<'static, Result<AudioDownload>> {
        let (reply, receiver) = oneshot::channel();
        if self
            .inner
            .commands
            .send(StoreCommand::QueueDownload {
                song,
                audio_quality,
                reply,
            })
            .is_err()
        {
            return futures::future::ready(Err(storage_stopped())).boxed();
        }
        async move { receiver.await.map_err(|_| storage_stopped())? }.boxed()
    }

    pub fn mark_download_started(
        &self,
        video_id: String,
        mime_type: String,
        content_length: u64,
        downloaded_bytes: u64,
        loudness_lufs_mb: Option<i32>,
    ) -> BoxFuture<'static, Result<()>> {
        let (reply, receiver) = oneshot::channel();
        if self
            .inner
            .commands
            .send(StoreCommand::MarkDownloadStarted {
                video_id,
                mime_type,
                content_length,
                downloaded_bytes,
                loudness_lufs_mb,
                reply,
            })
            .is_err()
        {
            return futures::future::ready(Err(storage_stopped())).boxed();
        }
        async move { receiver.await.map_err(|_| storage_stopped())? }.boxed()
    }

    pub fn update_download_progress(
        &self,
        video_id: String,
        downloaded_bytes: u64,
    ) -> BoxFuture<'static, Result<()>> {
        let (reply, receiver) = oneshot::channel();
        if self
            .inner
            .commands
            .send(StoreCommand::UpdateDownloadProgress {
                video_id,
                downloaded_bytes,
                reply,
            })
            .is_err()
        {
            return futures::future::ready(Err(storage_stopped())).boxed();
        }
        async move { receiver.await.map_err(|_| storage_stopped())? }.boxed()
    }

    pub fn finish_download(&self, video_id: String) -> BoxFuture<'static, Result<()>> {
        let (reply, receiver) = oneshot::channel();
        if self
            .inner
            .commands
            .send(StoreCommand::FinishDownload { video_id, reply })
            .is_err()
        {
            return futures::future::ready(Err(storage_stopped())).boxed();
        }
        async move { receiver.await.map_err(|_| storage_stopped())? }.boxed()
    }

    pub fn stop_download(
        &self,
        video_id: String,
        state: DownloadState,
        error: Option<String>,
    ) -> BoxFuture<'static, Result<()>> {
        let (reply, receiver) = oneshot::channel();
        if self
            .inner
            .commands
            .send(StoreCommand::StopDownload {
                video_id,
                state,
                error,
                reply,
            })
            .is_err()
        {
            return futures::future::ready(Err(storage_stopped())).boxed();
        }
        async move { receiver.await.map_err(|_| storage_stopped())? }.boxed()
    }

    pub fn downloads(&self) -> BoxFuture<'static, Result<Vec<AudioDownload>>> {
        let (reply, receiver) = oneshot::channel();
        if self
            .inner
            .commands
            .send(StoreCommand::Downloads { reply })
            .is_err()
        {
            return futures::future::ready(Err(storage_stopped())).boxed();
        }
        async move { receiver.await.map_err(|_| storage_stopped())? }.boxed()
    }

    pub fn delete_download(&self, video_id: String) -> BoxFuture<'static, Result<()>> {
        let (reply, receiver) = oneshot::channel();
        if self
            .inner
            .commands
            .send(StoreCommand::DeleteDownload { video_id, reply })
            .is_err()
        {
            return futures::future::ready(Err(storage_stopped())).boxed();
        }
        async move { receiver.await.map_err(|_| storage_stopped())? }.boxed()
    }
}

fn run_store_worker(
    path: &Path,
    receiver: mpsc::Receiver<StoreCommand>,
    ready: mpsc::SyncSender<Result<()>>,
) {
    let mut connection = match open_and_migrate(path) {
        Ok(connection) => {
            let _ = ready.send(Ok(()));
            connection
        }
        Err(error) => {
            let _ = ready.send(Err(error));
            return;
        }
    };

    while let Ok(command) = receiver.recv() {
        match command {
            StoreCommand::RecordHistory {
                song,
                play_time,
                reply,
            } => {
                let _ = reply.send(record_history(&mut connection, &song, play_time));
            }
            StoreCommand::RecentHistory { limit, reply } => {
                let _ = reply.send(recent_history(&connection, limit));
            }
            StoreCommand::ForgottenFavorites { limit, reply } => {
                let _ = reply.send(forgotten_favorites(&connection, limit));
            }
            StoreCommand::KeepListening {
                limit,
                offset,
                reply,
            } => {
                let _ = reply.send(keep_listening(&connection, limit, offset));
            }
            StoreCommand::ClearHistory { reply } => {
                let _ = reply.send(clear_history(&mut connection));
            }
            StoreCommand::DeleteHistoryEntry { id, reply } => {
                let _ = reply.send(delete_history_entry(&connection, id));
            }
            StoreCommand::ListeningStats {
                start_ms,
                limit,
                reply,
            } => {
                let _ = reply.send(listening_stats(&connection, start_ms, limit));
            }
            StoreCommand::RecordRecognition { result, reply } => {
                let _ = reply.send(record_recognition(&mut connection, result));
            }
            StoreCommand::RecognitionHistory { limit, reply } => {
                let _ = reply.send(recognition_history(&connection, limit));
            }
            StoreCommand::DeleteRecognitionHistory { id, reply } => {
                let _ = reply.send(delete_recognition_history(&connection, id));
            }
            StoreCommand::ClearRecognitionHistory { reply } => {
                let _ = reply.send(clear_recognition_history(&connection));
            }
            StoreCommand::RecordSearchQuery { query, reply } => {
                let _ = reply.send(record_search_query(&connection, query));
            }
            StoreCommand::SearchHistory { limit, reply } => {
                let _ = reply.send(search_history(&connection, limit));
            }
            StoreCommand::DeleteSearchHistory { id, reply } => {
                let _ = reply.send(delete_search_history(&connection, id));
            }
            StoreCommand::ClearSearchHistory { reply } => {
                let _ = reply.send(clear_search_history(&connection));
            }
            StoreCommand::RememberCatalogItems { items, reply } => {
                let _ = reply.send(remember_catalog_items(&mut connection, &items));
            }
            StoreCommand::CatalogItems { limit, reply } => {
                let _ = reply.send(catalog_items(&connection, limit));
            }
            StoreCommand::SetFavorite {
                song,
                favorite,
                reply,
            } => {
                let _ = reply.send(set_favorite(&mut connection, &song, favorite));
            }
            StoreCommand::Favorites { limit, reply } => {
                let _ = reply.send(favorites(&connection, limit));
            }
            StoreCommand::SetPodcastSubscription {
                podcast,
                subscribed,
                reply,
            } => {
                let _ = reply.send(set_podcast_subscription(
                    &mut connection,
                    &podcast,
                    subscribed,
                ));
            }
            StoreCommand::PodcastSubscriptions { reply } => {
                let _ = reply.send(podcast_subscriptions(&connection));
            }
            StoreCommand::SetEpisodeForLater { song, saved, reply } => {
                let _ = reply.send(set_episode_for_later(&mut connection, &song, saved));
            }
            StoreCommand::EpisodesForLater { reply } => {
                let _ = reply.send(episodes_for_later(&connection));
            }
            StoreCommand::SaveEpisodePlaybackPosition {
                song,
                position,
                reply,
            } => {
                let _ = reply.send(save_episode_playback_position(
                    &mut connection,
                    &song,
                    position,
                ));
            }
            StoreCommand::EpisodePlaybackPosition { video_id, reply } => {
                let _ = reply.send(episode_playback_position(&connection, &video_id));
            }
            StoreCommand::ReconcilePodcastLibrary {
                podcasts,
                episodes,
                reply,
            } => {
                let _ = reply.send(reconcile_podcast_library(
                    &mut connection,
                    &podcasts,
                    &episodes,
                ));
            }
            StoreCommand::CreatePlaylist { name, reply } => {
                let _ = reply.send(create_playlist(&mut connection, &name));
            }
            StoreCommand::RenamePlaylist {
                playlist_id,
                name,
                reply,
            } => {
                let _ = reply.send(rename_playlist(&mut connection, playlist_id, &name));
            }
            StoreCommand::Playlists {
                sort,
                direction,
                reply,
            } => {
                let _ = reply.send(playlists(&connection, sort, direction));
            }
            StoreCommand::AddToPlaylist {
                playlist_id,
                song,
                reply,
            } => {
                let _ = reply.send(add_to_playlist(&mut connection, playlist_id, &song));
            }
            StoreCommand::PlaylistSongs { playlist_id, reply } => {
                let _ = reply.send(playlist_songs(&connection, playlist_id));
            }
            StoreCommand::RemoveFromPlaylist {
                playlist_id,
                video_id,
                reply,
            } => {
                let _ = reply.send(remove_from_playlist(
                    &mut connection,
                    playlist_id,
                    &video_id,
                ));
            }
            StoreCommand::DeletePlaylist { playlist_id, reply } => {
                let _ = reply.send(delete_playlist(&mut connection, playlist_id));
            }
            StoreCommand::SaveSession { session, reply } => {
                let _ = reply.send(save_session(&mut connection, &session));
            }
            StoreCommand::LoadSession { reply } => {
                let _ = reply.send(load_session(&connection));
            }
            StoreCommand::SaveLyrics {
                song,
                document,
                reply,
            } => {
                let _ = reply.send(save_lyrics(&mut connection, &song, &document));
            }
            StoreCommand::LoadLyrics { video_id, reply } => {
                let _ = reply.send(load_lyrics(&connection, &video_id));
            }
            StoreCommand::SaveSettings { settings, reply } => {
                let _ = reply.send(save_settings(&mut connection, *settings));
            }
            StoreCommand::LoadSettings { reply } => {
                let _ = reply.send(load_settings(&connection));
            }
            StoreCommand::SaveEqualizerProfile { profile, reply } => {
                let _ = reply.send(save_equalizer_profile(&mut connection, &profile));
            }
            StoreCommand::SaveEqualizerProfiles { profiles, reply } => {
                let _ = reply.send(save_equalizer_profiles(&mut connection, &profiles));
            }
            StoreCommand::EqualizerProfiles { reply } => {
                let _ = reply.send(equalizer_profiles(&connection));
            }
            StoreCommand::DeleteEqualizerProfile { profile_id, reply } => {
                let _ = reply.send(delete_equalizer_profile(&connection, &profile_id));
            }
            StoreCommand::QueueDownload {
                song,
                audio_quality,
                reply,
            } => {
                let _ = reply.send(queue_download(&mut connection, &song, audio_quality));
            }
            StoreCommand::MarkDownloadStarted {
                video_id,
                mime_type,
                content_length,
                downloaded_bytes,
                loudness_lufs_mb,
                reply,
            } => {
                let _ = reply.send(mark_download_started(
                    &connection,
                    &video_id,
                    &mime_type,
                    content_length,
                    downloaded_bytes,
                    loudness_lufs_mb,
                ));
            }
            StoreCommand::UpdateDownloadProgress {
                video_id,
                downloaded_bytes,
                reply,
            } => {
                let _ = reply.send(update_download_progress(
                    &connection,
                    &video_id,
                    downloaded_bytes,
                ));
            }
            StoreCommand::FinishDownload { video_id, reply } => {
                let _ = reply.send(finish_download(&connection, &video_id));
            }
            StoreCommand::StopDownload {
                video_id,
                state,
                error,
                reply,
            } => {
                let _ = reply.send(stop_download(
                    &connection,
                    &video_id,
                    state,
                    error.as_deref(),
                ));
            }
            StoreCommand::Downloads { reply } => {
                let _ = reply.send(downloads(&connection));
            }
            StoreCommand::DeleteDownload { video_id, reply } => {
                let _ = reply.send(delete_download(&connection, &video_id));
            }
            StoreCommand::Shutdown => break,
        }
    }
}

fn open_and_migrate(path: &Path) -> Result<Connection> {
    let mut connection = Connection::open(path).map_err(storage_error)?;
    connection
        .busy_timeout(Duration::from_secs(5))
        .map_err(storage_error)?;
    connection
        .execute_batch(
            "PRAGMA foreign_keys = ON;
             PRAGMA journal_mode = WAL;
             PRAGMA synchronous = NORMAL;",
        )
        .map_err(storage_error)?;

    connection
        .execute_batch(
            "CREATE TABLE IF NOT EXISTS schema_migration (
                 version INTEGER PRIMARY KEY,
                 applied_at_ms INTEGER NOT NULL
             );",
        )
        .map_err(storage_error)?;
    let current_version = connection
        .query_row("SELECT MAX(version) FROM schema_migration", [], |row| {
            row.get::<_, Option<i64>>(0)
        })
        .map_err(storage_error)?
        .unwrap_or_default();
    if current_version > SCHEMA_VERSION {
        return Err(AppError::Storage(format!(
            "database schema version {current_version} is newer than supported version {SCHEMA_VERSION}"
        )));
    }
    if current_version < 1 {
        migrate_to_v1(&mut connection)?;
    }
    if current_version < 2 {
        migrate_to_v2(&mut connection)?;
    }
    if current_version < 3 {
        migrate_to_v3(&mut connection)?;
    }
    if current_version < 4 {
        migrate_to_v4(&mut connection)?;
    }
    if current_version < 5 {
        migrate_to_v5(&mut connection)?;
    }
    if current_version < 6 {
        migrate_to_v6(&mut connection)?;
    }
    if current_version < 7 {
        migrate_to_v7(&mut connection)?;
    }
    if current_version < 8 {
        migrate_to_v8(&mut connection)?;
    }
    if current_version < 9 {
        migrate_to_v9(&mut connection)?;
    }
    if current_version < 10 {
        migrate_to_v10(&mut connection)?;
    }
    if current_version < 11 {
        migrate_to_v11(&mut connection)?;
    }
    if current_version < 12 {
        migrate_to_v12(&mut connection)?;
    }
    if current_version < 13 {
        migrate_to_v13(&mut connection)?;
    }
    if current_version < 14 {
        migrate_to_v14(&mut connection)?;
    }
    if current_version < 15 {
        migrate_to_v15(&mut connection)?;
    }
    if current_version < 16 {
        migrate_to_v16(&mut connection)?;
    }
    if current_version < 17 {
        migrate_to_v17(&mut connection)?;
    }
    if current_version < 18 {
        migrate_to_v18(&mut connection)?;
    }
    if current_version < 19 {
        migrate_to_v19(&mut connection)?;
    }
    if current_version < 20 {
        migrate_to_v20(&mut connection)?;
    }
    if current_version < 21 {
        migrate_to_v21(&mut connection)?;
    }
    if current_version < 22 {
        migrate_to_v22(&mut connection)?;
    }
    if current_version < 23 {
        migrate_to_v23(&mut connection)?;
    }
    Ok(connection)
}

fn migrate_to_v1(connection: &mut Connection) -> Result<()> {
    let transaction = connection.transaction().map_err(storage_error)?;
    transaction
        .execute_batch(
            "CREATE TABLE song (
                 video_id TEXT PRIMARY KEY,
                 title TEXT NOT NULL,
                 artists_json TEXT NOT NULL,
                 duration_ms INTEGER,
                 thumbnail_url TEXT,
                 created_at_ms INTEGER NOT NULL,
                 updated_at_ms INTEGER NOT NULL
             );
             CREATE TABLE play_history (
                 id INTEGER PRIMARY KEY AUTOINCREMENT,
                 video_id TEXT NOT NULL REFERENCES song(video_id) ON DELETE CASCADE,
                 played_at_ms INTEGER NOT NULL,
                 play_time_ms INTEGER NOT NULL CHECK(play_time_ms >= 0)
             );
             CREATE INDEX play_history_played_at
                 ON play_history(played_at_ms DESC, id DESC);
             CREATE INDEX play_history_video_id
                 ON play_history(video_id);
             CREATE TABLE playback_session (
                 singleton INTEGER PRIMARY KEY CHECK(singleton = 1),
                 current_index INTEGER,
                 position_ms INTEGER NOT NULL,
                 volume REAL NOT NULL,
                 updated_at_ms INTEGER NOT NULL
             );
             CREATE TABLE queue_item (
                 position INTEGER PRIMARY KEY CHECK(position >= 0),
                 video_id TEXT NOT NULL REFERENCES song(video_id) ON DELETE CASCADE
             );",
        )
        .map_err(storage_error)?;
    transaction
        .execute(
            "INSERT INTO schema_migration(version, applied_at_ms) VALUES (?1, ?2)",
            params![1, now_ms()],
        )
        .map_err(storage_error)?;
    transaction.commit().map_err(storage_error)?;
    Ok(())
}

fn migrate_to_v2(connection: &mut Connection) -> Result<()> {
    let transaction = connection.transaction().map_err(storage_error)?;
    transaction
        .execute_batch(
            "CREATE TABLE favorite_song (
                 video_id TEXT PRIMARY KEY REFERENCES song(video_id) ON DELETE CASCADE,
                 liked_at_ms INTEGER NOT NULL
             );
             CREATE INDEX favorite_song_liked_at
                 ON favorite_song(liked_at_ms DESC);
             CREATE TABLE local_playlist (
                 id INTEGER PRIMARY KEY AUTOINCREMENT,
                 name TEXT NOT NULL COLLATE NOCASE UNIQUE CHECK(length(trim(name)) > 0),
                 created_at_ms INTEGER NOT NULL,
                 updated_at_ms INTEGER NOT NULL
             );
             CREATE TABLE local_playlist_song (
                 playlist_id INTEGER NOT NULL
                     REFERENCES local_playlist(id) ON DELETE CASCADE,
                 video_id TEXT NOT NULL REFERENCES song(video_id) ON DELETE CASCADE,
                 position INTEGER NOT NULL CHECK(position >= 0),
                 added_at_ms INTEGER NOT NULL,
                 PRIMARY KEY(playlist_id, video_id),
                 UNIQUE(playlist_id, position)
             );
             CREATE INDEX local_playlist_song_video_id
                 ON local_playlist_song(video_id);",
        )
        .map_err(storage_error)?;
    transaction
        .execute(
            "INSERT INTO schema_migration(version, applied_at_ms) VALUES (2, ?1)",
            [now_ms()],
        )
        .map_err(storage_error)?;
    transaction.commit().map_err(storage_error)
}

fn migrate_to_v3(connection: &mut Connection) -> Result<()> {
    let transaction = connection.transaction().map_err(storage_error)?;
    transaction
        .execute_batch(
            "CREATE TABLE playback_source_session (
                 singleton INTEGER PRIMARY KEY CHECK(singleton = 1)
                     REFERENCES playback_session(singleton) ON DELETE CASCADE,
                 video_id TEXT NOT NULL REFERENCES song(video_id) ON DELETE CASCADE,
                 mime_type TEXT NOT NULL CHECK(length(trim(mime_type)) > 0),
                 content_length INTEGER NOT NULL CHECK(content_length > 0),
                 resolved_at_ms INTEGER NOT NULL CHECK(resolved_at_ms >= 0),
                 expires_at_ms INTEGER NOT NULL CHECK(expires_at_ms >= resolved_at_ms)
             );",
        )
        .map_err(storage_error)?;
    transaction
        .execute(
            "INSERT INTO schema_migration(version, applied_at_ms) VALUES (3, ?1)",
            [now_ms()],
        )
        .map_err(storage_error)?;
    transaction.commit().map_err(storage_error)
}

fn migrate_to_v4(connection: &mut Connection) -> Result<()> {
    let transaction = connection.transaction().map_err(storage_error)?;
    transaction
        .execute_batch(
            "CREATE TABLE lyrics_document (
                 video_id TEXT PRIMARY KEY REFERENCES song(video_id) ON DELETE CASCADE,
                 provider TEXT NOT NULL CHECK(length(trim(provider)) > 0),
                 is_synced INTEGER NOT NULL CHECK(is_synced IN (0, 1)),
                 fetched_at_ms INTEGER NOT NULL CHECK(fetched_at_ms >= 0)
             );
             CREATE TABLE lyrics_line (
                 video_id TEXT NOT NULL
                     REFERENCES lyrics_document(video_id) ON DELETE CASCADE,
                 position INTEGER NOT NULL CHECK(position >= 0),
                 start_ms INTEGER CHECK(start_ms IS NULL OR start_ms >= 0),
                 text TEXT NOT NULL CHECK(length(trim(text)) > 0),
                 PRIMARY KEY(video_id, position)
             );",
        )
        .map_err(storage_error)?;
    transaction
        .execute(
            "INSERT INTO schema_migration(version, applied_at_ms) VALUES (4, ?1)",
            [now_ms()],
        )
        .map_err(storage_error)?;
    transaction.commit().map_err(storage_error)
}

fn migrate_to_v5(connection: &mut Connection) -> Result<()> {
    let transaction = connection.transaction().map_err(storage_error)?;
    transaction
        .execute_batch(
            "CREATE TABLE app_settings (
                 singleton INTEGER PRIMARY KEY CHECK(singleton = 1),
                 proxy_enabled INTEGER NOT NULL CHECK(proxy_enabled IN (0, 1)),
                 proxy_kind TEXT NOT NULL CHECK(proxy_kind IN ('http', 'socks5')),
                 proxy_address TEXT NOT NULL,
                 proxy_username TEXT NOT NULL,
                 proxy_password TEXT NOT NULL,
                 audio_quality TEXT NOT NULL CHECK(audio_quality IN ('auto', 'low', 'high')),
                 cache_root TEXT NOT NULL CHECK(length(trim(cache_root)) > 0),
                 audio_cache_bytes INTEGER NOT NULL CHECK(audio_cache_bytes > 0),
                 theme TEXT NOT NULL CHECK(theme IN ('light', 'dark')),
                 updated_at_ms INTEGER NOT NULL CHECK(updated_at_ms >= 0)
             );",
        )
        .map_err(storage_error)?;
    transaction
        .execute(
            "INSERT INTO schema_migration(version, applied_at_ms) VALUES (5, ?1)",
            [now_ms()],
        )
        .map_err(storage_error)?;
    transaction.commit().map_err(storage_error)
}

fn migrate_to_v6(connection: &mut Connection) -> Result<()> {
    let transaction = connection.transaction().map_err(storage_error)?;
    transaction
        .execute_batch(
            "ALTER TABLE app_settings
                 ADD COLUMN auto_radio INTEGER NOT NULL DEFAULT 1
                 CHECK(auto_radio IN (0, 1));",
        )
        .map_err(storage_error)?;
    transaction
        .execute(
            "INSERT INTO schema_migration(version, applied_at_ms) VALUES (6, ?1)",
            [now_ms()],
        )
        .map_err(storage_error)?;
    transaction.commit().map_err(storage_error)
}

fn migrate_to_v7(connection: &mut Connection) -> Result<()> {
    let transaction = connection.transaction().map_err(storage_error)?;
    transaction
        .execute_batch(
            "ALTER TABLE app_settings
                 ADD COLUMN youtube_history_sync INTEGER NOT NULL DEFAULT 1
                 CHECK(youtube_history_sync IN (0, 1));",
        )
        .map_err(storage_error)?;
    transaction
        .execute(
            "INSERT INTO schema_migration(version, applied_at_ms) VALUES (7, ?1)",
            [now_ms()],
        )
        .map_err(storage_error)?;
    transaction.commit().map_err(storage_error)
}

fn migrate_to_v8(connection: &mut Connection) -> Result<()> {
    let transaction = connection.transaction().map_err(storage_error)?;
    transaction
        .execute_batch(
            "CREATE TABLE audio_download (
                 video_id TEXT PRIMARY KEY REFERENCES song(video_id) ON DELETE CASCADE,
                 audio_quality TEXT NOT NULL CHECK(audio_quality IN ('auto', 'low', 'high')),
                 mime_type TEXT CHECK(mime_type IS NULL OR mime_type LIKE 'audio/%'),
                 content_length INTEGER CHECK(content_length IS NULL OR content_length > 0),
                 downloaded_bytes INTEGER NOT NULL DEFAULT 0 CHECK(downloaded_bytes >= 0),
                 state TEXT NOT NULL CHECK(state IN ('queued', 'downloading', 'paused', 'completed', 'failed')),
                 created_at_ms INTEGER NOT NULL CHECK(created_at_ms >= 0),
                 updated_at_ms INTEGER NOT NULL CHECK(updated_at_ms >= 0),
                 completed_at_ms INTEGER CHECK(completed_at_ms IS NULL OR completed_at_ms >= 0),
                 last_error TEXT CHECK(last_error IS NULL OR length(last_error) <= 2048),
                 CHECK(content_length IS NULL OR downloaded_bytes <= content_length),
                 CHECK(state != 'completed' OR (
                     mime_type IS NOT NULL AND
                     content_length IS NOT NULL AND
                     downloaded_bytes = content_length AND
                     completed_at_ms IS NOT NULL AND
                     last_error IS NULL
                 ))
             );
             CREATE INDEX audio_download_state_updated
                 ON audio_download(state, updated_at_ms DESC);",
        )
        .map_err(storage_error)?;
    transaction
        .execute(
            "INSERT INTO schema_migration(version, applied_at_ms) VALUES (8, ?1)",
            [now_ms()],
        )
        .map_err(storage_error)?;
    transaction.commit().map_err(storage_error)
}

fn migrate_to_v9(connection: &mut Connection) -> Result<()> {
    let transaction = connection.transaction().map_err(storage_error)?;
    transaction
        .execute_batch(
            "ALTER TABLE app_settings
                 ADD COLUMN audio_normalization INTEGER NOT NULL DEFAULT 1
                 CHECK(audio_normalization IN (0, 1));
             ALTER TABLE app_settings
                 ADD COLUMN loudness_level TEXT NOT NULL DEFAULT 'balanced'
                 CHECK(loudness_level IN ('aggressive', 'loud', 'balanced', 'quiet'));
             ALTER TABLE playback_source_session
                 ADD COLUMN loudness_lufs_mb INTEGER
                 CHECK(loudness_lufs_mb IS NULL OR loudness_lufs_mb BETWEEN -10000 AND 2000);
             ALTER TABLE audio_download
                 ADD COLUMN loudness_lufs_mb INTEGER
                 CHECK(loudness_lufs_mb IS NULL OR loudness_lufs_mb BETWEEN -10000 AND 2000);",
        )
        .map_err(storage_error)?;
    transaction
        .execute(
            "INSERT INTO schema_migration(version, applied_at_ms) VALUES (9, ?1)",
            [now_ms()],
        )
        .map_err(storage_error)?;
    transaction.commit().map_err(storage_error)
}

fn migrate_to_v10(connection: &mut Connection) -> Result<()> {
    let transaction = connection.transaction().map_err(storage_error)?;
    transaction
        .execute_batch(
            "ALTER TABLE app_settings
                 ADD COLUMN equalizer_enabled INTEGER NOT NULL DEFAULT 0
                 CHECK(equalizer_enabled IN (0, 1));
             ALTER TABLE app_settings
                 ADD COLUMN equalizer_gains_json TEXT NOT NULL
                 DEFAULT '[0,0,0,0,0,0,0,0,0,0]'
                 CHECK(length(equalizer_gains_json) BETWEEN 21 AND 81);",
        )
        .map_err(storage_error)?;
    transaction
        .execute(
            "INSERT INTO schema_migration(version, applied_at_ms) VALUES (10, ?1)",
            [now_ms()],
        )
        .map_err(storage_error)?;
    transaction.commit().map_err(storage_error)
}

fn migrate_to_v11(connection: &mut Connection) -> Result<()> {
    let transaction = connection.transaction().map_err(storage_error)?;
    transaction
        .execute_batch(
            "ALTER TABLE app_settings
                 ADD COLUMN playback_varispeed INTEGER NOT NULL DEFAULT 0
                 CHECK(playback_varispeed IN (0, 1));
             ALTER TABLE app_settings
                 ADD COLUMN playback_tempo_milli INTEGER NOT NULL DEFAULT 1000
                 CHECK(playback_tempo_milli BETWEEN 250 AND 2000
                       AND (playback_tempo_milli - 250) % 50 = 0);
             ALTER TABLE app_settings
                 ADD COLUMN playback_transpose_semitones INTEGER NOT NULL DEFAULT 0
                 CHECK(playback_transpose_semitones BETWEEN -12 AND 12);",
        )
        .map_err(storage_error)?;
    transaction
        .execute(
            "INSERT INTO schema_migration(version, applied_at_ms) VALUES (11, ?1)",
            [now_ms()],
        )
        .map_err(storage_error)?;
    transaction.commit().map_err(storage_error)
}

fn migrate_to_v12(connection: &mut Connection) -> Result<()> {
    let transaction = connection.transaction().map_err(storage_error)?;
    transaction
        .execute_batch(
            "ALTER TABLE app_settings
                 ADD COLUMN lastfm_scrobbling INTEGER NOT NULL DEFAULT 0
                 CHECK(lastfm_scrobbling IN (0, 1));
             ALTER TABLE app_settings
                 ADD COLUMN lastfm_now_playing INTEGER NOT NULL DEFAULT 0
                 CHECK(lastfm_now_playing IN (0, 1));
             ALTER TABLE app_settings
                 ADD COLUMN lastfm_sync_likes INTEGER NOT NULL DEFAULT 0
                 CHECK(lastfm_sync_likes IN (0, 1));
             ALTER TABLE app_settings
                 ADD COLUMN lastfm_min_track_seconds INTEGER NOT NULL DEFAULT 30
                 CHECK(lastfm_min_track_seconds BETWEEN 10 AND 60);
             ALTER TABLE app_settings
                 ADD COLUMN lastfm_delay_percent_milli INTEGER NOT NULL DEFAULT 500
                 CHECK(lastfm_delay_percent_milli BETWEEN 300 AND 950);
             ALTER TABLE app_settings
                 ADD COLUMN lastfm_max_delay_seconds INTEGER NOT NULL DEFAULT 180
                 CHECK(lastfm_max_delay_seconds BETWEEN 30 AND 360);",
        )
        .map_err(storage_error)?;
    transaction
        .execute(
            "INSERT INTO schema_migration(version, applied_at_ms) VALUES (12, ?1)",
            [now_ms()],
        )
        .map_err(storage_error)?;
    transaction.commit().map_err(storage_error)
}

fn migrate_to_v13(connection: &mut Connection) -> Result<()> {
    let transaction = connection.transaction().map_err(storage_error)?;
    transaction
        .execute_batch(
            "ALTER TABLE app_settings
                 ADD COLUMN discord_rich_presence INTEGER NOT NULL DEFAULT 0
                 CHECK(discord_rich_presence IN (0, 1));",
        )
        .map_err(storage_error)?;
    transaction
        .execute(
            "INSERT INTO schema_migration(version, applied_at_ms) VALUES (13, ?1)",
            [now_ms()],
        )
        .map_err(storage_error)?;
    transaction.commit().map_err(storage_error)
}

fn migrate_to_v14(connection: &mut Connection) -> Result<()> {
    let transaction = connection.transaction().map_err(storage_error)?;
    transaction
        .execute_batch(
            "ALTER TABLE app_settings
                 ADD COLUMN listen_together_server_url TEXT NOT NULL
                 DEFAULT 'wss://metroserverx.meowery.eu/ws'
                 CHECK(length(trim(listen_together_server_url)) BETWEEN 1 AND 2048);
             ALTER TABLE app_settings
                 ADD COLUMN listen_together_username TEXT NOT NULL DEFAULT ''
                 CHECK(length(listen_together_username) <= 128);
             ALTER TABLE app_settings
                 ADD COLUMN listen_together_auto_approve_joins INTEGER NOT NULL DEFAULT 0
                 CHECK(listen_together_auto_approve_joins IN (0, 1));
             ALTER TABLE app_settings
                 ADD COLUMN listen_together_auto_approve_suggestions INTEGER NOT NULL DEFAULT 0
                 CHECK(listen_together_auto_approve_suggestions IN (0, 1));
             ALTER TABLE app_settings
                 ADD COLUMN listen_together_sync_host_volume INTEGER NOT NULL DEFAULT 1
                 CHECK(listen_together_sync_host_volume IN (0, 1));",
        )
        .map_err(storage_error)?;
    transaction
        .execute(
            "INSERT INTO schema_migration(version, applied_at_ms) VALUES (14, ?1)",
            [now_ms()],
        )
        .map_err(storage_error)?;
    transaction.commit().map_err(storage_error)
}

fn migrate_to_v15(connection: &mut Connection) -> Result<()> {
    let transaction = connection.transaction().map_err(storage_error)?;
    transaction
        .execute_batch(
            "ALTER TABLE song
                 ADD COLUMN is_episode INTEGER NOT NULL DEFAULT 0
                 CHECK(is_episode IN (0, 1));",
        )
        .map_err(storage_error)?;
    transaction
        .execute(
            "INSERT INTO schema_migration(version, applied_at_ms) VALUES (15, ?1)",
            [now_ms()],
        )
        .map_err(storage_error)?;
    transaction.commit().map_err(storage_error)
}

fn migrate_to_v16(connection: &mut Connection) -> Result<()> {
    let transaction = connection.transaction().map_err(storage_error)?;
    transaction
        .execute_batch(
            "CREATE TABLE equalizer_profile (
                 id TEXT PRIMARY KEY CHECK(length(trim(id)) BETWEEN 1 AND 256),
                 name TEXT NOT NULL CHECK(length(trim(name)) BETWEEN 1 AND 256),
                 device_model TEXT NOT NULL CHECK(length(trim(device_model)) BETWEEN 1 AND 256),
                 equalizer_json TEXT NOT NULL CHECK(length(equalizer_json) BETWEEN 2 AND 131072),
                 source TEXT NOT NULL CHECK(length(trim(source)) BETWEEN 1 AND 256),
                 rig TEXT NOT NULL CHECK(length(trim(rig)) BETWEEN 1 AND 256),
                 is_custom INTEGER NOT NULL CHECK(is_custom IN (0, 1)),
                 added_at_ms INTEGER NOT NULL CHECK(added_at_ms >= 0)
             );
             CREATE INDEX equalizer_profile_added_at
                 ON equalizer_profile(added_at_ms DESC, id ASC);
             ALTER TABLE app_settings
                 ADD COLUMN equalizer_active_profile_json TEXT
                 CHECK(equalizer_active_profile_json IS NULL OR
                       length(equalizer_active_profile_json) BETWEEN 2 AND 131072);",
        )
        .map_err(storage_error)?;
    transaction
        .execute(
            "INSERT INTO schema_migration(version, applied_at_ms) VALUES (16, ?1)",
            [now_ms()],
        )
        .map_err(storage_error)?;
    transaction.commit().map_err(storage_error)
}

fn migrate_to_v17(connection: &mut Connection) -> Result<()> {
    let transaction = connection.transaction().map_err(storage_error)?;
    transaction
        .execute_batch(
            "CREATE TABLE podcast_subscription (
                 podcast_id TEXT PRIMARY KEY CHECK(length(trim(podcast_id)) BETWEEN 1 AND 2048),
                 title TEXT NOT NULL CHECK(length(trim(title)) BETWEEN 1 AND 512),
                 author TEXT,
                 thumbnail_url TEXT,
                 channel_id TEXT,
                 subscribed_at_ms INTEGER NOT NULL CHECK(subscribed_at_ms >= 0),
                 updated_at_ms INTEGER NOT NULL CHECK(updated_at_ms >= 0)
             );
             CREATE INDEX podcast_subscription_subscribed_at
                 ON podcast_subscription(subscribed_at_ms DESC, podcast_id ASC);
             CREATE TABLE episode_for_later (
                 video_id TEXT PRIMARY KEY REFERENCES song(video_id) ON DELETE CASCADE,
                 saved_at_ms INTEGER NOT NULL CHECK(saved_at_ms >= 0)
             );
             CREATE INDEX episode_for_later_saved_at
                 ON episode_for_later(saved_at_ms DESC, video_id ASC);
             CREATE TABLE episode_playback_position (
                 video_id TEXT PRIMARY KEY REFERENCES song(video_id) ON DELETE CASCADE,
                 position_ms INTEGER NOT NULL CHECK(position_ms >= 3000),
                 updated_at_ms INTEGER NOT NULL CHECK(updated_at_ms >= 0)
             );",
        )
        .map_err(storage_error)?;
    transaction
        .execute(
            "INSERT INTO schema_migration(version, applied_at_ms) VALUES (17, ?1)",
            [now_ms()],
        )
        .map_err(storage_error)?;
    transaction.commit().map_err(storage_error)
}

fn migrate_to_v18(connection: &mut Connection) -> Result<()> {
    let transaction = connection.transaction().map_err(storage_error)?;
    transaction
        .execute_batch(
            "CREATE TABLE podcast_subscription_tombstone (
                 podcast_id TEXT PRIMARY KEY CHECK(length(trim(podcast_id)) BETWEEN 1 AND 2048),
                 removed_at_ms INTEGER NOT NULL CHECK(removed_at_ms >= 0)
             );",
        )
        .map_err(storage_error)?;
    transaction
        .execute(
            "INSERT INTO schema_migration(version, applied_at_ms) VALUES (18, ?1)",
            [now_ms()],
        )
        .map_err(storage_error)?;
    transaction.commit().map_err(storage_error)
}

fn migrate_to_v19(connection: &mut Connection) -> Result<()> {
    let transaction = connection.transaction().map_err(storage_error)?;
    transaction
        .execute_batch(
            "ALTER TABLE playback_session
                 ADD COLUMN repeat_mode INTEGER NOT NULL DEFAULT 0
                 CHECK(repeat_mode IN (0, 1, 2));
             ALTER TABLE playback_session
                 ADD COLUMN shuffle_enabled INTEGER NOT NULL DEFAULT 0
                 CHECK(shuffle_enabled IN (0, 1));",
        )
        .map_err(storage_error)?;
    transaction
        .execute(
            "INSERT INTO schema_migration(version, applied_at_ms) VALUES (19, ?1)",
            [now_ms()],
        )
        .map_err(storage_error)?;
    transaction.commit().map_err(storage_error)
}

fn migrate_to_v20(connection: &mut Connection) -> Result<()> {
    let transaction = connection.transaction().map_err(storage_error)?;
    transaction
        .execute_batch(
            "CREATE TABLE recognition_history (
                 id INTEGER PRIMARY KEY AUTOINCREMENT,
                 track_id TEXT NOT NULL,
                 title TEXT NOT NULL CHECK(length(trim(title)) > 0),
                 artist TEXT NOT NULL CHECK(length(trim(artist)) > 0),
                 album TEXT,
                 cover_art_url TEXT,
                 genre TEXT,
                 release_date TEXT,
                 label TEXT,
                 shazam_url TEXT,
                 isrc TEXT,
                 youtube_video_id TEXT,
                 recognized_at_ms INTEGER NOT NULL CHECK(recognized_at_ms >= 0)
             );
             CREATE INDEX recognition_history_recognized_at
                 ON recognition_history(recognized_at_ms DESC, id DESC);",
        )
        .map_err(storage_error)?;
    transaction
        .execute(
            "INSERT INTO schema_migration(version, applied_at_ms) VALUES (20, ?1)",
            [now_ms()],
        )
        .map_err(storage_error)?;
    transaction.commit().map_err(storage_error)
}

fn migrate_to_v21(connection: &mut Connection) -> Result<()> {
    let transaction = connection.transaction().map_err(storage_error)?;
    transaction
        .execute_batch(
            "CREATE TABLE search_history (
                 id INTEGER PRIMARY KEY AUTOINCREMENT,
                 query TEXT NOT NULL UNIQUE CHECK(length(trim(query)) > 0)
             );",
        )
        .map_err(storage_error)?;
    transaction
        .execute(
            "INSERT INTO schema_migration(version, applied_at_ms) VALUES (21, ?1)",
            [now_ms()],
        )
        .map_err(storage_error)?;
    transaction.commit().map_err(storage_error)
}

fn migrate_to_v22(connection: &mut Connection) -> Result<()> {
    let transaction = connection.transaction().map_err(storage_error)?;
    transaction
        .execute_batch(
            "CREATE TABLE local_catalog_item (
                 browse_id TEXT NOT NULL CHECK(length(trim(browse_id)) > 0),
                 kind TEXT NOT NULL CHECK(kind IN ('album', 'artist')),
                 title TEXT NOT NULL CHECK(length(trim(title)) > 0),
                 subtitle TEXT NOT NULL,
                 thumbnail_url TEXT,
                 params TEXT,
                 last_seen_ms INTEGER NOT NULL,
                 PRIMARY KEY(browse_id, kind)
             );
             CREATE INDEX local_catalog_item_last_seen
                 ON local_catalog_item(last_seen_ms DESC);",
        )
        .map_err(storage_error)?;
    transaction
        .execute(
            "INSERT INTO schema_migration(version, applied_at_ms) VALUES (22, ?1)",
            [now_ms()],
        )
        .map_err(storage_error)?;
    transaction.commit().map_err(storage_error)
}

fn migrate_to_v23(connection: &mut Connection) -> Result<()> {
    let transaction = connection.transaction().map_err(storage_error)?;
    transaction
        .execute_batch(
            "CREATE TABLE song_album (
                 video_id TEXT PRIMARY KEY REFERENCES song(video_id) ON DELETE CASCADE,
                 browse_id TEXT NOT NULL CHECK(length(trim(browse_id)) > 0),
                 title TEXT NOT NULL CHECK(length(trim(title)) > 0),
                 thumbnail_url TEXT
             );
             CREATE INDEX song_album_browse_id ON song_album(browse_id);",
        )
        .map_err(storage_error)?;
    transaction
        .execute(
            "INSERT INTO schema_migration(version, applied_at_ms) VALUES (23, ?1)",
            [now_ms()],
        )
        .map_err(storage_error)?;
    transaction.commit().map_err(storage_error)
}

fn record_history(connection: &mut Connection, song: &Song, play_time: Duration) -> Result<()> {
    let transaction = connection.transaction().map_err(storage_error)?;
    upsert_song(&transaction, song)?;
    transaction
        .execute(
            "INSERT INTO play_history(video_id, played_at_ms, play_time_ms)
             VALUES (?1, ?2, ?3)",
            params![song.video_id, now_ms(), duration_to_i64_ms(play_time)?],
        )
        .map_err(storage_error)?;
    transaction.commit().map_err(storage_error)
}

fn recent_history(connection: &Connection, limit: usize) -> Result<Vec<HistoryEntry>> {
    let limit = i64::try_from(limit).unwrap_or(i64::MAX);
    let mut statement = connection
        .prepare(
            "SELECT h.id, h.played_at_ms, h.play_time_ms,
                    s.video_id, s.title, s.artists_json, s.duration_ms, s.thumbnail_url,
                    s.is_episode
             FROM play_history h
             JOIN song s ON s.video_id = h.video_id
             ORDER BY h.played_at_ms DESC, h.id DESC
             LIMIT ?1",
        )
        .map_err(storage_error)?;
    let rows = statement
        .query_map([limit], |row| {
            let play_time_ms: i64 = row.get(2)?;
            Ok(HistoryEntry {
                id: row.get(0)?,
                played_at_ms: row.get(1)?,
                play_time: Duration::from_millis(
                    u64::try_from(play_time_ms)
                        .map_err(|_| rusqlite::Error::IntegralValueOutOfRange(2, play_time_ms))?,
                ),
                song: song_from_row(row, 3)?,
            })
        })
        .map_err(storage_error)?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(storage_error)
}

fn forgotten_favorites(connection: &Connection, limit: usize) -> Result<Vec<Song>> {
    let cutoff_ms = now_ms().saturating_sub(FORGOTTEN_FAVORITES_WINDOW_MS);
    let limit = i64::try_from(limit).unwrap_or(i64::MAX);
    let mut statement = connection
        .prepare(
            "WITH play_totals AS (
                 SELECT video_id,
                        SUM(CASE WHEN played_at_ms < ?1 THEN play_time_ms ELSE 0 END)
                            AS old_play_time,
                        SUM(CASE WHEN played_at_ms >= ?1 THEN play_time_ms ELSE 0 END)
                            AS recent_play_time
                 FROM play_history
                 GROUP BY video_id
             )
             SELECT s.video_id, s.title, s.artists_json, s.duration_ms, s.thumbnail_url,
                    s.is_episode
             FROM play_totals totals
             JOIN song s ON s.video_id = totals.video_id
             WHERE totals.recent_play_time > 0
               AND totals.old_play_time > totals.recent_play_time * 5
             ORDER BY totals.old_play_time DESC, s.video_id ASC
             LIMIT ?2",
        )
        .map_err(storage_error)?;
    let rows = statement
        .query_map(params![cutoff_ms, limit], |row| song_from_row(row, 0))
        .map_err(storage_error)?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(storage_error)
}

fn keep_listening(connection: &Connection, limit: usize, offset: usize) -> Result<Vec<Song>> {
    let now = now_ms();
    let cutoff_ms = now.saturating_sub(KEEP_LISTENING_WINDOW_MS);
    let limit = i64::try_from(limit).unwrap_or(i64::MAX);
    let offset = i64::try_from(offset).unwrap_or(i64::MAX);
    let mut statement = connection
        .prepare(
            "WITH top_songs AS (
                 SELECT video_id, SUM(play_time_ms) AS time_listened
                 FROM play_history
                 WHERE played_at_ms > ?1 AND played_at_ms <= ?2
                 GROUP BY video_id
                 ORDER BY time_listened DESC
                 LIMIT ?3 OFFSET ?4
             )
             SELECT s.video_id, s.title, s.artists_json, s.duration_ms, s.thumbnail_url,
                    s.is_episode
             FROM top_songs
             JOIN song s ON s.video_id = top_songs.video_id
             ORDER BY top_songs.time_listened DESC, s.video_id ASC",
        )
        .map_err(storage_error)?;
    let rows = statement
        .query_map(params![cutoff_ms, now, limit, offset], |row| {
            song_from_row(row, 0)
        })
        .map_err(storage_error)?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(storage_error)
}

fn clear_history(connection: &mut Connection) -> Result<()> {
    connection
        .execute("DELETE FROM play_history", [])
        .map_err(storage_error)?;
    Ok(())
}

fn delete_history_entry(connection: &Connection, id: i64) -> Result<()> {
    connection
        .execute("DELETE FROM play_history WHERE id = ?1", [id])
        .map_err(storage_error)?;
    Ok(())
}

fn listening_stats(connection: &Connection, start_ms: i64, limit: usize) -> Result<ListeningStats> {
    let mut statement = connection
        .prepare(
            "SELECT COUNT(h.id), SUM(h.play_time_ms),
                    s.video_id, s.title, s.artists_json, s.duration_ms, s.thumbnail_url,
                    s.is_episode
             FROM play_history h
             JOIN song s ON s.video_id = h.video_id
             WHERE h.played_at_ms >= ?1
             GROUP BY s.video_id
             ORDER BY SUM(h.play_time_ms) DESC, COUNT(h.id) DESC, s.video_id ASC",
        )
        .map_err(storage_error)?;
    let rows = statement
        .query_map([start_ms.max(0)], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, i64>(1)?,
                song_from_row(row, 2)?,
            ))
        })
        .map_err(storage_error)?;

    let mut play_count = 0_u64;
    let mut play_time_ms = 0_u64;
    let mut songs = Vec::new();
    let mut artists: HashMap<(Option<String>, String), (u64, u64)> = HashMap::new();
    for row in rows {
        let (song_play_count, song_play_time_ms, song) = row.map_err(storage_error)?;
        let song_play_count = u64::try_from(song_play_count)
            .map_err(|_| AppError::Storage("negative song play count in history".into()))?;
        let song_play_time_ms = u64::try_from(song_play_time_ms)
            .map_err(|_| AppError::Storage("negative song play time in history".into()))?;
        play_count = play_count.saturating_add(song_play_count);
        play_time_ms = play_time_ms.saturating_add(song_play_time_ms);
        for artist in &song.artists {
            let totals = artists
                .entry((artist.id.clone(), artist.name.clone()))
                .or_default();
            totals.0 = totals.0.saturating_add(song_play_count);
            totals.1 = totals.1.saturating_add(song_play_time_ms);
        }
        songs.push(SongListeningStats {
            song,
            play_count: song_play_count,
            play_time: Duration::from_millis(song_play_time_ms),
        });
    }

    let unique_songs = songs.len();
    let unique_artists = artists.len();
    songs.truncate(limit);
    let mut top_artists = artists
        .into_iter()
        .map(
            |((id, name), (play_count, play_time_ms))| ArtistListeningStats {
                id,
                name,
                play_count,
                play_time: Duration::from_millis(play_time_ms),
            },
        )
        .collect::<Vec<_>>();
    top_artists.sort_by(|left, right| {
        right
            .play_time
            .cmp(&left.play_time)
            .then_with(|| right.play_count.cmp(&left.play_count))
            .then_with(|| left.name.cmp(&right.name))
    });
    top_artists.truncate(limit);

    let mut album_statement = connection
        .prepare(
            "SELECT a.browse_id, a.title, a.thumbnail_url,
                    COUNT(h.id), SUM(h.play_time_ms)
             FROM play_history h
             JOIN song_album a ON a.video_id = h.video_id
             WHERE h.played_at_ms >= ?1
             GROUP BY a.browse_id
             ORDER BY SUM(h.play_time_ms) DESC, COUNT(h.id) DESC, a.browse_id ASC",
        )
        .map_err(storage_error)?;
    let album_rows = album_statement
        .query_map([start_ms.max(0)], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Option<String>>(2)?,
                row.get::<_, i64>(3)?,
                row.get::<_, i64>(4)?,
            ))
        })
        .map_err(storage_error)?;
    let mut top_albums = Vec::new();
    for row in album_rows {
        let (browse_id, title, thumbnail_url, album_play_count, album_play_time_ms) =
            row.map_err(storage_error)?;
        top_albums.push(AlbumListeningStats {
            browse_id,
            title,
            thumbnail_url,
            play_count: u64::try_from(album_play_count)
                .map_err(|_| AppError::Storage("negative album play count in history".into()))?,
            play_time: Duration::from_millis(
                u64::try_from(album_play_time_ms)
                    .map_err(|_| AppError::Storage("negative album play time in history".into()))?,
            ),
        });
    }
    let unique_albums = top_albums.len();
    top_albums.truncate(limit);

    Ok(ListeningStats {
        play_count,
        unique_songs,
        unique_artists,
        unique_albums,
        play_time: Duration::from_millis(play_time_ms),
        top_songs: songs,
        top_artists,
        top_albums,
    })
}

fn record_recognition(
    connection: &mut Connection,
    result: RecognitionResult,
) -> Result<RecognitionHistoryEntry> {
    let recognized_at_ms = now_ms();
    connection
        .execute(
            "INSERT INTO recognition_history(
                 track_id, title, artist, album, cover_art_url, genre, release_date,
                 label, shazam_url, isrc, youtube_video_id, recognized_at_ms
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
            params![
                &result.track_id,
                &result.title,
                &result.artist,
                result.album.as_deref(),
                result.cover_art_url.as_deref(),
                result.genre.as_deref(),
                result.release_date.as_deref(),
                result.label.as_deref(),
                result.shazam_url.as_deref(),
                result.isrc.as_deref(),
                result.youtube_video_id.as_deref(),
                recognized_at_ms,
            ],
        )
        .map_err(storage_error)?;
    Ok(RecognitionHistoryEntry {
        id: connection.last_insert_rowid(),
        result,
        recognized_at_ms,
    })
}

fn recognition_history(
    connection: &Connection,
    limit: usize,
) -> Result<Vec<RecognitionHistoryEntry>> {
    let limit = i64::try_from(limit).unwrap_or(i64::MAX);
    let mut statement = connection
        .prepare(
            "SELECT id, track_id, title, artist, album, cover_art_url, genre, release_date,
                    label, shazam_url, isrc, youtube_video_id, recognized_at_ms
             FROM recognition_history
             ORDER BY recognized_at_ms DESC, id DESC
             LIMIT ?1",
        )
        .map_err(storage_error)?;
    let rows = statement
        .query_map([limit], |row| {
            Ok(RecognitionHistoryEntry {
                id: row.get(0)?,
                result: RecognitionResult {
                    track_id: row.get(1)?,
                    title: row.get(2)?,
                    artist: row.get(3)?,
                    album: row.get(4)?,
                    cover_art_url: row.get(5)?,
                    genre: row.get(6)?,
                    release_date: row.get(7)?,
                    label: row.get(8)?,
                    shazam_url: row.get(9)?,
                    isrc: row.get(10)?,
                    youtube_video_id: row.get(11)?,
                },
                recognized_at_ms: row.get(12)?,
            })
        })
        .map_err(storage_error)?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(storage_error)
}

fn delete_recognition_history(connection: &Connection, id: i64) -> Result<()> {
    connection
        .execute("DELETE FROM recognition_history WHERE id = ?1", [id])
        .map_err(storage_error)?;
    Ok(())
}

fn clear_recognition_history(connection: &Connection) -> Result<()> {
    connection
        .execute("DELETE FROM recognition_history", [])
        .map_err(storage_error)?;
    Ok(())
}

fn record_search_query(connection: &Connection, query: String) -> Result<SearchHistoryEntry> {
    let query = query.trim().to_owned();
    if query.is_empty() {
        return Err(AppError::Storage("search query cannot be empty".into()));
    }
    connection
        .execute(
            "INSERT OR REPLACE INTO search_history(query) VALUES (?1)",
            [&query],
        )
        .map_err(storage_error)?;
    Ok(SearchHistoryEntry {
        id: connection.last_insert_rowid(),
        query,
    })
}

fn search_history(connection: &Connection, limit: usize) -> Result<Vec<SearchHistoryEntry>> {
    let limit = i64::try_from(limit).unwrap_or(i64::MAX);
    let mut statement = connection
        .prepare(
            "SELECT id, query
             FROM search_history
             ORDER BY id DESC
             LIMIT ?1",
        )
        .map_err(storage_error)?;
    let rows = statement
        .query_map([limit], |row| {
            Ok(SearchHistoryEntry {
                id: row.get(0)?,
                query: row.get(1)?,
            })
        })
        .map_err(storage_error)?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(storage_error)
}

fn delete_search_history(connection: &Connection, id: i64) -> Result<()> {
    connection
        .execute("DELETE FROM search_history WHERE id = ?1", [id])
        .map_err(storage_error)?;
    Ok(())
}

fn clear_search_history(connection: &Connection) -> Result<()> {
    connection
        .execute("DELETE FROM search_history", [])
        .map_err(storage_error)?;
    Ok(())
}

fn remember_catalog_items(connection: &mut Connection, items: &[BrowseItem]) -> Result<()> {
    let transaction = connection.transaction().map_err(storage_error)?;
    let last_seen_ms = now_ms();
    for item in items {
        let kind = match item.kind {
            BrowseKind::Album => "album",
            BrowseKind::Artist => "artist",
            BrowseKind::Playlist | BrowseKind::Podcast | BrowseKind::Category => continue,
        };
        if item.browse_id.trim().is_empty() || item.title.trim().is_empty() {
            continue;
        }
        transaction
            .execute(
                "INSERT INTO local_catalog_item(
                     browse_id, kind, title, subtitle, thumbnail_url, params, last_seen_ms
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
                 ON CONFLICT(browse_id, kind) DO UPDATE SET
                     title = excluded.title,
                     subtitle = excluded.subtitle,
                     thumbnail_url = excluded.thumbnail_url,
                     params = excluded.params,
                     last_seen_ms = excluded.last_seen_ms",
                params![
                    item.browse_id,
                    kind,
                    item.title,
                    item.subtitle,
                    item.thumbnail_url,
                    item.params,
                    last_seen_ms,
                ],
            )
            .map_err(storage_error)?;
    }
    transaction.commit().map_err(storage_error)
}

fn catalog_items(connection: &Connection, limit: usize) -> Result<Vec<BrowseItem>> {
    let limit = i64::try_from(limit).unwrap_or(i64::MAX);
    let mut statement = connection
        .prepare(
            "SELECT browse_id, kind, title, subtitle, thumbnail_url, params
             FROM local_catalog_item
             ORDER BY last_seen_ms DESC, title COLLATE NOCASE ASC
             LIMIT ?1",
        )
        .map_err(storage_error)?;
    let rows = statement
        .query_map([limit], |row| {
            let kind: String = row.get(1)?;
            let kind = match kind.as_str() {
                "album" => BrowseKind::Album,
                "artist" => BrowseKind::Artist,
                _ => return Err(rusqlite::Error::InvalidQuery),
            };
            Ok(BrowseItem {
                browse_id: row.get(0)?,
                kind,
                title: row.get(2)?,
                subtitle: row.get(3)?,
                thumbnail_url: row.get(4)?,
                params: row.get(5)?,
                editable: false,
            })
        })
        .map_err(storage_error)?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(storage_error)
}

fn set_favorite(connection: &mut Connection, song: &Song, favorite: bool) -> Result<()> {
    let transaction = connection.transaction().map_err(storage_error)?;
    if favorite {
        upsert_song(&transaction, song)?;
        transaction
            .execute(
                "INSERT INTO favorite_song(video_id, liked_at_ms) VALUES (?1, ?2)
                 ON CONFLICT(video_id) DO NOTHING",
                params![song.video_id, now_ms()],
            )
            .map_err(storage_error)?;
    } else {
        transaction
            .execute(
                "DELETE FROM favorite_song WHERE video_id = ?1",
                [&song.video_id],
            )
            .map_err(storage_error)?;
    }
    transaction.commit().map_err(storage_error)
}

fn favorites(connection: &Connection, limit: usize) -> Result<Vec<FavoriteEntry>> {
    let limit = i64::try_from(limit).unwrap_or(i64::MAX);
    let mut statement = connection
        .prepare(
            "SELECT f.liked_at_ms,
                    s.video_id, s.title, s.artists_json, s.duration_ms, s.thumbnail_url,
                    s.is_episode
             FROM favorite_song f
             JOIN song s ON s.video_id = f.video_id
             ORDER BY f.liked_at_ms DESC, f.video_id ASC
             LIMIT ?1",
        )
        .map_err(storage_error)?;
    let rows = statement
        .query_map([limit], |row| {
            Ok(FavoriteEntry {
                liked_at_ms: row.get(0)?,
                song: song_from_row(row, 1)?,
            })
        })
        .map_err(storage_error)?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(storage_error)
}

fn set_podcast_subscription(
    connection: &mut Connection,
    podcast: &PodcastSubscription,
    subscribed: bool,
) -> Result<()> {
    validate_podcast_id(&podcast.podcast_id)?;
    let transaction = connection.transaction().map_err(storage_error)?;
    if !subscribed {
        transaction
            .execute(
                "DELETE FROM podcast_subscription WHERE podcast_id = ?1",
                [podcast.podcast_id.trim()],
            )
            .map_err(storage_error)?;
        transaction
            .execute(
                "INSERT INTO podcast_subscription_tombstone(podcast_id, removed_at_ms)
                 VALUES (?1, ?2)
                 ON CONFLICT(podcast_id) DO UPDATE SET removed_at_ms = excluded.removed_at_ms",
                params![podcast.podcast_id.trim(), now_ms()],
            )
            .map_err(storage_error)?;
        return transaction.commit().map_err(storage_error);
    }

    validate_podcast_subscription(podcast)?;
    transaction
        .execute(
            "DELETE FROM podcast_subscription_tombstone WHERE podcast_id = ?1",
            [podcast.podcast_id.trim()],
        )
        .map_err(storage_error)?;
    upsert_podcast_subscription(&transaction, podcast, false)?;
    transaction.commit().map_err(storage_error)
}

fn validate_podcast_id(podcast_id: &str) -> Result<()> {
    let podcast_id = podcast_id.trim();
    if podcast_id.is_empty() || podcast_id.len() > 2_048 || podcast_id.chars().any(char::is_control)
    {
        return Err(AppError::Storage("podcast id is invalid".into()));
    }
    Ok(())
}

fn validate_podcast_subscription(podcast: &PodcastSubscription) -> Result<()> {
    validate_podcast_id(&podcast.podcast_id)?;
    let title = podcast.title.trim();
    if title.is_empty() || title.len() > 512 || title.chars().any(char::is_control) {
        return Err(AppError::Storage("podcast title is invalid".into()));
    }
    normalized_optional_metadata(podcast.author.as_deref(), 512, "podcast author")?;
    normalized_optional_metadata(
        podcast.thumbnail_url.as_deref(),
        4_096,
        "podcast thumbnail URL",
    )?;
    normalized_optional_metadata(podcast.channel_id.as_deref(), 2_048, "podcast channel id")?;
    Ok(())
}

fn upsert_podcast_subscription(
    connection: &Connection,
    podcast: &PodcastSubscription,
    preserve_subscribed_at: bool,
) -> Result<()> {
    let author = normalized_optional_metadata(podcast.author.as_deref(), 512, "podcast author")?;
    let thumbnail_url = normalized_optional_metadata(
        podcast.thumbnail_url.as_deref(),
        4_096,
        "podcast thumbnail URL",
    )?;
    let channel_id =
        normalized_optional_metadata(podcast.channel_id.as_deref(), 2_048, "podcast channel id")?;
    let subscribed_at_ms = if podcast.subscribed_at_ms >= 0 {
        podcast.subscribed_at_ms
    } else {
        now_ms()
    };
    let update_subscribed_at = if preserve_subscribed_at {
        "podcast_subscription.subscribed_at_ms"
    } else {
        "excluded.subscribed_at_ms"
    };
    connection
        .execute(
            &format!(
                "INSERT INTO podcast_subscription(
                 podcast_id, title, author, thumbnail_url, channel_id,
                 subscribed_at_ms, updated_at_ms
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
             ON CONFLICT(podcast_id) DO UPDATE SET
                 title = excluded.title,
                 author = excluded.author,
                 thumbnail_url = excluded.thumbnail_url,
                 channel_id = excluded.channel_id,
                 subscribed_at_ms = {update_subscribed_at},
                 updated_at_ms = excluded.updated_at_ms"
            ),
            params![
                podcast.podcast_id.trim(),
                podcast.title.trim(),
                author,
                thumbnail_url,
                channel_id,
                subscribed_at_ms,
                now_ms(),
            ],
        )
        .map_err(storage_error)?;
    Ok(())
}

fn podcast_subscriptions(connection: &Connection) -> Result<Vec<PodcastSubscription>> {
    let mut statement = connection
        .prepare(
            "SELECT podcast_id, title, author, thumbnail_url, channel_id, subscribed_at_ms
             FROM podcast_subscription
             ORDER BY subscribed_at_ms DESC, podcast_id ASC",
        )
        .map_err(storage_error)?;
    let rows = statement
        .query_map([], |row| {
            Ok(PodcastSubscription {
                podcast_id: row.get(0)?,
                title: row.get(1)?,
                author: row.get(2)?,
                thumbnail_url: row.get(3)?,
                channel_id: row.get(4)?,
                subscribed_at_ms: row.get(5)?,
            })
        })
        .map_err(storage_error)?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(storage_error)
}

fn set_episode_for_later(connection: &mut Connection, song: &Song, saved: bool) -> Result<()> {
    validate_episode_song(song)?;
    let transaction = connection.transaction().map_err(storage_error)?;
    if saved {
        upsert_song(&transaction, song)?;
        transaction
            .execute(
                "INSERT INTO episode_for_later(video_id, saved_at_ms) VALUES (?1, ?2)
                 ON CONFLICT(video_id) DO NOTHING",
                params![song.video_id, now_ms()],
            )
            .map_err(storage_error)?;
    } else {
        transaction
            .execute(
                "DELETE FROM episode_for_later WHERE video_id = ?1",
                [&song.video_id],
            )
            .map_err(storage_error)?;
    }
    transaction.commit().map_err(storage_error)
}

fn episodes_for_later(connection: &Connection) -> Result<Vec<SavedEpisode>> {
    let mut statement = connection
        .prepare(
            "SELECT e.saved_at_ms, p.position_ms,
                    s.video_id, s.title, s.artists_json, s.duration_ms, s.thumbnail_url,
                    s.is_episode
             FROM episode_for_later e
             JOIN song s ON s.video_id = e.video_id
             LEFT JOIN episode_playback_position p ON p.video_id = e.video_id
             ORDER BY e.saved_at_ms DESC, e.video_id ASC",
        )
        .map_err(storage_error)?;
    let rows = statement
        .query_map([], |row| {
            let position_ms: Option<i64> = row.get(1)?;
            Ok(SavedEpisode {
                saved_at_ms: row.get(0)?,
                playback_position: position_ms
                    .and_then(|value| u64::try_from(value).ok())
                    .map(Duration::from_millis),
                song: song_from_row(row, 2)?,
            })
        })
        .map_err(storage_error)?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(storage_error)
}

fn save_episode_playback_position(
    connection: &mut Connection,
    song: &Song,
    position: Duration,
) -> Result<()> {
    validate_episode_song(song)?;
    if position < Duration::from_secs(3) {
        return Ok(());
    }
    let position_ms = duration_to_i64_ms(position)?;
    let transaction = connection.transaction().map_err(storage_error)?;
    upsert_song(&transaction, song)?;
    transaction
        .execute(
            "INSERT INTO episode_playback_position(video_id, position_ms, updated_at_ms)
             VALUES (?1, ?2, ?3)
             ON CONFLICT(video_id) DO UPDATE SET
                 position_ms = excluded.position_ms,
                 updated_at_ms = excluded.updated_at_ms",
            params![song.video_id, position_ms, now_ms()],
        )
        .map_err(storage_error)?;
    transaction.commit().map_err(storage_error)
}

fn episode_playback_position(connection: &Connection, video_id: &str) -> Result<Option<Duration>> {
    let position_ms = connection
        .query_row(
            "SELECT position_ms FROM episode_playback_position WHERE video_id = ?1",
            [video_id],
            |row| row.get::<_, i64>(0),
        )
        .optional()
        .map_err(storage_error)?;
    position_ms
        .map(|value| {
            u64::try_from(value)
                .map(Duration::from_millis)
                .map_err(|_| AppError::Storage("stored episode position is negative".into()))
        })
        .transpose()
}

fn reconcile_podcast_library(
    connection: &mut Connection,
    podcasts: &[PodcastSubscription],
    episodes: &[Song],
) -> Result<PodcastLibraryReconcileSummary> {
    let mut remote_podcast_ids = HashSet::with_capacity(podcasts.len());
    for podcast in podcasts {
        validate_podcast_subscription(podcast)?;
        if !remote_podcast_ids.insert(podcast.podcast_id.trim().to_owned()) {
            return Err(AppError::Storage(
                "remote podcast snapshot contains duplicate ids".into(),
            ));
        }
    }
    let mut remote_episode_ids = HashSet::with_capacity(episodes.len());
    for episode in episodes {
        validate_episode_song(episode)?;
        if !remote_episode_ids.insert(episode.video_id.trim().to_owned()) {
            return Err(AppError::Storage(
                "remote episode snapshot contains duplicate ids".into(),
            ));
        }
    }

    let transaction = connection.transaction().map_err(storage_error)?;
    let local_podcast_ids = {
        let mut statement = transaction
            .prepare("SELECT podcast_id FROM podcast_subscription")
            .map_err(storage_error)?;
        let rows = statement
            .query_map([], |row| row.get::<_, String>(0))
            .map_err(storage_error)?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(storage_error)?
    };
    let mut skipped_podcast_tombstones = 0;
    for podcast in podcasts {
        let tombstoned = transaction
            .query_row(
                "SELECT 1 FROM podcast_subscription_tombstone WHERE podcast_id = ?1",
                [podcast.podcast_id.trim()],
                |_| Ok(()),
            )
            .optional()
            .map_err(storage_error)?
            .is_some();
        if tombstoned {
            skipped_podcast_tombstones += 1;
        } else {
            upsert_podcast_subscription(&transaction, podcast, true)?;
        }
    }
    let mut removed_podcast_count = 0;
    for podcast_id in local_podcast_ids {
        if remote_podcast_ids.contains(&podcast_id) {
            continue;
        }
        transaction
            .execute(
                "DELETE FROM podcast_subscription WHERE podcast_id = ?1",
                [&podcast_id],
            )
            .map_err(storage_error)?;
        transaction
            .execute(
                "INSERT INTO podcast_subscription_tombstone(podcast_id, removed_at_ms)
                 VALUES (?1, ?2)
                 ON CONFLICT(podcast_id) DO UPDATE SET removed_at_ms = excluded.removed_at_ms",
                params![podcast_id, now_ms()],
            )
            .map_err(storage_error)?;
        removed_podcast_count += 1;
    }

    let local_episode_ids = {
        let mut statement = transaction
            .prepare("SELECT video_id FROM episode_for_later")
            .map_err(storage_error)?;
        let rows = statement
            .query_map([], |row| row.get::<_, String>(0))
            .map_err(storage_error)?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(storage_error)?
    };
    for episode in episodes {
        upsert_song(&transaction, episode)?;
        transaction
            .execute(
                "INSERT INTO episode_for_later(video_id, saved_at_ms) VALUES (?1, ?2)
                 ON CONFLICT(video_id) DO NOTHING",
                params![episode.video_id, now_ms()],
            )
            .map_err(storage_error)?;
    }
    let mut removed_episode_count = 0;
    for video_id in local_episode_ids {
        if remote_episode_ids.contains(&video_id) {
            continue;
        }
        transaction
            .execute(
                "DELETE FROM episode_for_later WHERE video_id = ?1",
                [&video_id],
            )
            .map_err(storage_error)?;
        removed_episode_count += 1;
    }

    let podcast_count = transaction
        .query_row("SELECT COUNT(*) FROM podcast_subscription", [], |row| {
            row.get::<_, i64>(0)
        })
        .map_err(storage_error)?;
    let episode_count = transaction
        .query_row("SELECT COUNT(*) FROM episode_for_later", [], |row| {
            row.get::<_, i64>(0)
        })
        .map_err(storage_error)?;
    transaction.commit().map_err(storage_error)?;
    Ok(PodcastLibraryReconcileSummary {
        podcast_count: usize::try_from(podcast_count).unwrap_or(usize::MAX),
        episode_count: usize::try_from(episode_count).unwrap_or(usize::MAX),
        removed_podcast_count,
        removed_episode_count,
        skipped_podcast_tombstones,
    })
}

fn validate_episode_song(song: &Song) -> Result<()> {
    if !song.is_episode {
        return Err(AppError::Storage(
            "only podcast episodes can use episode state".into(),
        ));
    }
    if song.video_id.trim().is_empty()
        || song.video_id.len() > 2_048
        || song.video_id.chars().any(char::is_control)
    {
        return Err(AppError::Storage("episode video id is invalid".into()));
    }
    Ok(())
}

fn normalized_optional_metadata(
    value: Option<&str>,
    max_len: usize,
    label: &str,
) -> Result<Option<String>> {
    let Some(value) = value.map(str::trim).filter(|value| !value.is_empty()) else {
        return Ok(None);
    };
    if value.len() > max_len || value.chars().any(char::is_control) {
        return Err(AppError::Storage(format!("{label} is invalid")));
    }
    Ok(Some(value.to_owned()))
}

fn create_playlist(connection: &mut Connection, name: &str) -> Result<LocalPlaylist> {
    let name = name.trim();
    if name.is_empty() {
        return Err(AppError::Storage("playlist name cannot be empty".into()));
    }
    let timestamp = now_ms();
    connection
        .execute(
            "INSERT INTO local_playlist(name, created_at_ms, updated_at_ms)
             VALUES (?1, ?2, ?2)",
            params![name, timestamp],
        )
        .map_err(playlist_name_error)?;
    Ok(LocalPlaylist {
        id: connection.last_insert_rowid(),
        name: name.into(),
        song_count: 0,
        created_at_ms: timestamp,
        updated_at_ms: timestamp,
    })
}

fn rename_playlist(
    connection: &mut Connection,
    playlist_id: i64,
    name: &str,
) -> Result<LocalPlaylist> {
    let name = name.trim();
    if name.is_empty() {
        return Err(AppError::Storage("playlist name cannot be empty".into()));
    }
    let timestamp = now_ms();
    let updated = connection
        .execute(
            "UPDATE local_playlist SET name = ?2, updated_at_ms = ?3 WHERE id = ?1",
            params![playlist_id, name, timestamp],
        )
        .map_err(playlist_name_error)?;
    if updated == 0 {
        return Err(AppError::Storage("playlist no longer exists".into()));
    }
    playlist(connection, playlist_id)?
        .ok_or_else(|| AppError::Storage("playlist disappeared after it was renamed".into()))
}

fn playlist(connection: &Connection, playlist_id: i64) -> Result<Option<LocalPlaylist>> {
    connection
        .query_row(
            "SELECT p.id, p.name, COUNT(ps.video_id), p.created_at_ms, p.updated_at_ms
             FROM local_playlist p
             LEFT JOIN local_playlist_song ps ON ps.playlist_id = p.id
             WHERE p.id = ?1
             GROUP BY p.id",
            [playlist_id],
            local_playlist_from_row,
        )
        .optional()
        .map_err(storage_error)
}

fn playlists(
    connection: &Connection,
    sort: PlaylistSort,
    direction: SortDirection,
) -> Result<Vec<LocalPlaylist>> {
    let sort_column = match sort {
        PlaylistSort::CreatedAt => "p.created_at_ms",
        PlaylistSort::Name => "p.name COLLATE NOCASE",
        PlaylistSort::SongCount => "COUNT(ps.video_id)",
        PlaylistSort::UpdatedAt => "p.updated_at_ms",
    };
    let direction = match direction {
        SortDirection::Ascending => "ASC",
        SortDirection::Descending => "DESC",
    };
    let query = format!(
        "SELECT p.id, p.name, COUNT(ps.video_id), p.created_at_ms, p.updated_at_ms
         FROM local_playlist p
         LEFT JOIN local_playlist_song ps ON ps.playlist_id = p.id
         GROUP BY p.id
         ORDER BY {sort_column} {direction}, p.id {direction}"
    );
    let mut statement = connection.prepare(&query).map_err(storage_error)?;
    let rows = statement
        .query_map([], local_playlist_from_row)
        .map_err(storage_error)?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(storage_error)
}

fn local_playlist_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<LocalPlaylist> {
    let song_count: i64 = row.get(2)?;
    Ok(LocalPlaylist {
        id: row.get(0)?,
        name: row.get(1)?,
        song_count: usize::try_from(song_count)
            .map_err(|_| rusqlite::Error::IntegralValueOutOfRange(2, song_count))?,
        created_at_ms: row.get(3)?,
        updated_at_ms: row.get(4)?,
    })
}

fn add_to_playlist(connection: &mut Connection, playlist_id: i64, song: &Song) -> Result<()> {
    let transaction = connection.transaction().map_err(storage_error)?;
    upsert_song(&transaction, song)?;
    let timestamp = now_ms();
    let inserted = transaction
        .execute(
            "INSERT OR IGNORE INTO local_playlist_song(
                 playlist_id, video_id, position, added_at_ms
             )
             SELECT ?1, ?2, COALESCE(MAX(position) + 1, 0), ?3
             FROM local_playlist_song
             WHERE playlist_id = ?1",
            params![playlist_id, song.video_id, timestamp],
        )
        .map_err(storage_error)?;
    if inserted > 0 {
        transaction
            .execute(
                "UPDATE local_playlist SET updated_at_ms = ?2 WHERE id = ?1",
                params![playlist_id, timestamp],
            )
            .map_err(storage_error)?;
    }
    transaction.commit().map_err(storage_error)
}

fn playlist_songs(connection: &Connection, playlist_id: i64) -> Result<Vec<Song>> {
    let mut statement = connection
        .prepare(
            "SELECT s.video_id, s.title, s.artists_json, s.duration_ms, s.thumbnail_url,
                    s.is_episode
             FROM local_playlist_song ps
             JOIN song s ON s.video_id = ps.video_id
             WHERE ps.playlist_id = ?1
             ORDER BY ps.position ASC",
        )
        .map_err(storage_error)?;
    let rows = statement
        .query_map([playlist_id], |row| song_from_row(row, 0))
        .map_err(storage_error)?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(storage_error)
}

fn remove_from_playlist(
    connection: &mut Connection,
    playlist_id: i64,
    video_id: &str,
) -> Result<()> {
    let transaction = connection.transaction().map_err(storage_error)?;
    let removed = transaction
        .execute(
            "DELETE FROM local_playlist_song WHERE playlist_id = ?1 AND video_id = ?2",
            params![playlist_id, video_id],
        )
        .map_err(storage_error)?;
    if removed > 0 {
        transaction
            .execute(
                "UPDATE local_playlist SET updated_at_ms = ?2 WHERE id = ?1",
                params![playlist_id, now_ms()],
            )
            .map_err(storage_error)?;
    }
    transaction.commit().map_err(storage_error)
}

fn delete_playlist(connection: &mut Connection, playlist_id: i64) -> Result<()> {
    connection
        .execute("DELETE FROM local_playlist WHERE id = ?1", [playlist_id])
        .map_err(storage_error)?;
    Ok(())
}

fn save_session(connection: &mut Connection, session: &PersistedSession) -> Result<()> {
    if session
        .current_index
        .is_some_and(|index| index >= session.queue.len())
    {
        return Err(AppError::Storage(
            "playback session current index is outside its queue".into(),
        ));
    }
    validate_playback_source(session)?;
    let transaction = connection.transaction().map_err(storage_error)?;
    transaction
        .execute("DELETE FROM queue_item", [])
        .map_err(storage_error)?;
    for (position, song) in session.queue.iter().enumerate() {
        upsert_song(&transaction, song)?;
        transaction
            .execute(
                "INSERT INTO queue_item(position, video_id) VALUES (?1, ?2)",
                params![i64::try_from(position).unwrap_or(i64::MAX), song.video_id],
            )
            .map_err(storage_error)?;
    }
    transaction
        .execute(
            "INSERT INTO playback_session(
                 singleton, current_index, position_ms, volume, updated_at_ms,
                 repeat_mode, shuffle_enabled
             ) VALUES (1, ?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(singleton) DO UPDATE SET
                 current_index = excluded.current_index,
                 position_ms = excluded.position_ms,
                 volume = excluded.volume,
                 updated_at_ms = excluded.updated_at_ms,
                 repeat_mode = excluded.repeat_mode,
                 shuffle_enabled = excluded.shuffle_enabled",
            params![
                session
                    .current_index
                    .and_then(|index| i64::try_from(index).ok()),
                duration_to_i64_ms(session.position)?,
                f64::from(session.volume.clamp(0.0, 1.0)),
                now_ms(),
                session.repeat_mode.storage_value(),
                session.shuffle_enabled,
            ],
        )
        .map_err(storage_error)?;
    transaction
        .execute("DELETE FROM playback_source_session", [])
        .map_err(storage_error)?;
    if let Some(source) = &session.playback_source {
        transaction
            .execute(
                "INSERT INTO playback_source_session(
                     singleton, video_id, mime_type, content_length,
                     resolved_at_ms, expires_at_ms, loudness_lufs_mb
                 ) VALUES (1, ?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    source.video_id,
                    source.mime_type,
                    i64::try_from(source.content_length).map_err(|_| AppError::Storage(
                        "playback source content length is too large for SQLite".into()
                    ))?,
                    source.resolved_at_ms,
                    source.expires_at_ms,
                    source.loudness_lufs_mb,
                ],
            )
            .map_err(storage_error)?;
    }
    transaction.commit().map_err(storage_error)
}

fn validate_playback_source(session: &PersistedSession) -> Result<()> {
    let Some(source) = &session.playback_source else {
        return Ok(());
    };
    let current_song = session
        .current_index
        .and_then(|index| session.queue.get(index))
        .ok_or_else(|| {
            AppError::Storage("playback source metadata requires a current queue item".into())
        })?;
    if current_song.video_id != source.video_id {
        return Err(AppError::Storage(
            "playback source metadata does not match the current queue item".into(),
        ));
    }
    if source.mime_type.trim().is_empty() || !source.mime_type.starts_with("audio/") {
        return Err(AppError::Storage(
            "playback source metadata has an invalid audio MIME type".into(),
        ));
    }
    if source.content_length == 0 {
        return Err(AppError::Storage(
            "playback source metadata has an empty resource".into(),
        ));
    }
    if source.resolved_at_ms < 0 || source.expires_at_ms < source.resolved_at_ms {
        return Err(AppError::Storage(
            "playback source metadata has an invalid lifetime".into(),
        ));
    }
    if !valid_loudness_lufs_mb(source.loudness_lufs_mb) {
        return Err(AppError::Storage(
            "playback source metadata has an invalid loudness value".into(),
        ));
    }
    Ok(())
}

fn load_session(connection: &Connection) -> Result<Option<PersistedSession>> {
    let session = connection
        .query_row(
            "SELECT current_index, position_ms, volume, repeat_mode, shuffle_enabled
             FROM playback_session WHERE singleton = 1",
            [],
            |row| {
                Ok((
                    row.get::<_, Option<i64>>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, f64>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, i64>(4)?,
                ))
            },
        )
        .optional()
        .map_err(storage_error)?;
    let Some((current_index, position_ms, volume, repeat_mode, shuffle_enabled)) = session else {
        return Ok(None);
    };
    let repeat_mode = RepeatMode::from_storage_value(repeat_mode)
        .ok_or_else(|| AppError::Storage("persisted repeat mode is invalid".into()))?;
    let shuffle_enabled = match shuffle_enabled {
        0 => false,
        1 => true,
        _ => {
            return Err(AppError::Storage(
                "persisted shuffle state is invalid".into(),
            ));
        }
    };

    let mut statement = connection
        .prepare(
            "SELECT s.video_id, s.title, s.artists_json, s.duration_ms, s.thumbnail_url,
                    s.is_episode
             FROM queue_item q
             JOIN song s ON s.video_id = q.video_id
             ORDER BY q.position ASC",
        )
        .map_err(storage_error)?;
    let queue = statement
        .query_map([], |row| song_from_row(row, 0))
        .map_err(storage_error)?
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(storage_error)?;
    let current_index = current_index.and_then(|index| usize::try_from(index).ok());
    if current_index.is_some_and(|index| index >= queue.len()) {
        return Err(AppError::Storage(
            "persisted playback index is outside its queue".into(),
        ));
    }
    let playback_source = connection
        .query_row(
            "SELECT video_id, mime_type, content_length, resolved_at_ms, expires_at_ms,
                    loudness_lufs_mb
             FROM playback_source_session WHERE singleton = 1",
            [],
            |row| {
                let content_length: i64 = row.get(2)?;
                Ok(PersistedPlaybackSource {
                    video_id: row.get(0)?,
                    mime_type: row.get(1)?,
                    content_length: u64::try_from(content_length)
                        .map_err(|_| rusqlite::Error::IntegralValueOutOfRange(2, content_length))?,
                    resolved_at_ms: row.get(3)?,
                    expires_at_ms: row.get(4)?,
                    loudness_lufs_mb: row.get(5)?,
                })
            },
        )
        .optional()
        .map_err(storage_error)?;
    let session = PersistedSession {
        queue,
        current_index,
        position: Duration::from_millis(u64::try_from(position_ms).unwrap_or_default()),
        volume: (volume as f32).clamp(0.0, 1.0),
        repeat_mode,
        shuffle_enabled,
        playback_source,
    };
    validate_playback_source(&session)?;
    Ok(Some(session))
}

fn validate_lyrics_document(document: &LyricsDocument) -> Result<bool> {
    if document.provider.trim().is_empty() {
        return Err(AppError::Storage("lyrics provider cannot be empty".into()));
    }
    if document.lines.is_empty() {
        return Err(AppError::Storage("lyrics document cannot be empty".into()));
    }
    if document
        .lines
        .iter()
        .any(|line| line.text.trim().is_empty())
    {
        return Err(AppError::Storage(
            "lyrics lines cannot contain empty text".into(),
        ));
    }
    let is_synced = document.lines[0].start.is_some();
    if document
        .lines
        .iter()
        .any(|line| line.start.is_some() != is_synced)
    {
        return Err(AppError::Storage(
            "lyrics document cannot mix timed and untimed lines".into(),
        ));
    }
    if is_synced
        && document.lines.windows(2).any(|lines| {
            lines[0]
                .start
                .zip(lines[1].start)
                .is_some_and(|(left, right)| left > right)
        })
    {
        return Err(AppError::Storage(
            "timed lyrics must be ordered by start time".into(),
        ));
    }
    Ok(is_synced)
}

fn save_lyrics(connection: &mut Connection, song: &Song, document: &LyricsDocument) -> Result<()> {
    let is_synced = validate_lyrics_document(document)?;
    let transaction = connection.transaction().map_err(storage_error)?;
    upsert_song(&transaction, song)?;
    transaction
        .execute(
            "INSERT INTO lyrics_document(video_id, provider, is_synced, fetched_at_ms)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(video_id) DO UPDATE SET
                 provider = excluded.provider,
                 is_synced = excluded.is_synced,
                 fetched_at_ms = excluded.fetched_at_ms",
            params![song.video_id, document.provider, is_synced, now_ms(),],
        )
        .map_err(storage_error)?;
    transaction
        .execute(
            "DELETE FROM lyrics_line WHERE video_id = ?1",
            [&song.video_id],
        )
        .map_err(storage_error)?;
    for (position, line) in document.lines.iter().enumerate() {
        transaction
            .execute(
                "INSERT INTO lyrics_line(video_id, position, start_ms, text)
                 VALUES (?1, ?2, ?3, ?4)",
                params![
                    song.video_id,
                    i64::try_from(position).unwrap_or(i64::MAX),
                    line.start.map(duration_to_i64_ms).transpose()?,
                    line.text,
                ],
            )
            .map_err(storage_error)?;
    }
    transaction.commit().map_err(storage_error)
}

fn load_lyrics(connection: &Connection, video_id: &str) -> Result<Option<LyricsDocument>> {
    let metadata = connection
        .query_row(
            "SELECT provider, is_synced FROM lyrics_document WHERE video_id = ?1",
            [video_id],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, bool>(1)?)),
        )
        .optional()
        .map_err(storage_error)?;
    let Some((provider, stored_is_synced)) = metadata else {
        return Ok(None);
    };
    let mut statement = connection
        .prepare(
            "SELECT start_ms, text
             FROM lyrics_line
             WHERE video_id = ?1
             ORDER BY position ASC",
        )
        .map_err(storage_error)?;
    let lines = statement
        .query_map([video_id], |row| {
            let start_ms: Option<i64> = row.get(0)?;
            Ok(LyricsLine {
                start: start_ms
                    .map(|milliseconds| u64::try_from(milliseconds).map(Duration::from_millis))
                    .transpose()
                    .map_err(|_| {
                        rusqlite::Error::IntegralValueOutOfRange(0, start_ms.unwrap_or_default())
                    })?,
                text: row.get(1)?,
            })
        })
        .map_err(storage_error)?
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(storage_error)?;
    let document = LyricsDocument { provider, lines };
    let loaded_is_synced = validate_lyrics_document(&document)?;
    if loaded_is_synced != stored_is_synced {
        return Err(AppError::Storage(
            "cached lyrics timeline metadata is inconsistent".into(),
        ));
    }
    Ok(Some(document))
}

fn save_settings(connection: &mut Connection, settings: AppSettings) -> Result<()> {
    let settings = settings.validate()?;
    let equalizer_gains_json =
        serde_json::to_string(&settings.equalizer.gains_mb).map_err(|error| {
            AppError::Storage(format!("equalizer settings could not be saved: {error}"))
        })?;
    let cache_root = settings
        .cache_root
        .to_str()
        .ok_or_else(|| AppError::Storage("cache location cannot be represented as UTF-8".into()))?;
    let audio_cache_bytes = i64::try_from(settings.audio_cache_bytes)
        .map_err(|_| AppError::Storage("audio cache capacity is too large".into()))?;
    let equalizer_active_profile_json = settings
        .equalizer
        .active_profile
        .as_ref()
        .map(serde_json::to_string)
        .transpose()
        .map_err(|error| {
            AppError::Storage(format!(
                "active equalizer profile could not be saved: {error}"
            ))
        })?;
    connection
        .execute(
            "INSERT INTO app_settings(
                 singleton, proxy_enabled, proxy_kind, proxy_address,
                 proxy_username, proxy_password, audio_quality, cache_root,
                 audio_cache_bytes, theme, updated_at_ms, auto_radio,
                 youtube_history_sync, audio_normalization, loudness_level,
                 equalizer_enabled, equalizer_gains_json, playback_varispeed,
                 playback_tempo_milli, playback_transpose_semitones,
                 lastfm_scrobbling, lastfm_now_playing, lastfm_sync_likes,
                 lastfm_min_track_seconds, lastfm_delay_percent_milli,
                 lastfm_max_delay_seconds, discord_rich_presence,
                 listen_together_server_url, listen_together_username,
                 listen_together_auto_approve_joins,
                 listen_together_auto_approve_suggestions,
                 listen_together_sync_host_volume,
                 equalizer_active_profile_json
             ) VALUES (1, ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23, ?24, ?25, ?26, ?27, ?28, ?29, ?30, ?31, ?32)
             ON CONFLICT(singleton) DO UPDATE SET
                 proxy_enabled = excluded.proxy_enabled,
                 proxy_kind = excluded.proxy_kind,
                 proxy_address = excluded.proxy_address,
                 proxy_username = excluded.proxy_username,
                 proxy_password = excluded.proxy_password,
                 audio_quality = excluded.audio_quality,
                 cache_root = excluded.cache_root,
                 audio_cache_bytes = excluded.audio_cache_bytes,
                 theme = excluded.theme,
                 updated_at_ms = excluded.updated_at_ms,
                 auto_radio = excluded.auto_radio,
                 youtube_history_sync = excluded.youtube_history_sync,
                 audio_normalization = excluded.audio_normalization,
                 loudness_level = excluded.loudness_level,
                 equalizer_enabled = excluded.equalizer_enabled,
                 equalizer_gains_json = excluded.equalizer_gains_json,
                 playback_varispeed = excluded.playback_varispeed,
                 playback_tempo_milli = excluded.playback_tempo_milli,
                 playback_transpose_semitones = excluded.playback_transpose_semitones,
                 lastfm_scrobbling = excluded.lastfm_scrobbling,
                 lastfm_now_playing = excluded.lastfm_now_playing,
                 lastfm_sync_likes = excluded.lastfm_sync_likes,
                 lastfm_min_track_seconds = excluded.lastfm_min_track_seconds,
                 lastfm_delay_percent_milli = excluded.lastfm_delay_percent_milli,
                 lastfm_max_delay_seconds = excluded.lastfm_max_delay_seconds,
                 discord_rich_presence = excluded.discord_rich_presence,
                 listen_together_server_url = excluded.listen_together_server_url,
                 listen_together_username = excluded.listen_together_username,
                 listen_together_auto_approve_joins = excluded.listen_together_auto_approve_joins,
                 listen_together_auto_approve_suggestions = excluded.listen_together_auto_approve_suggestions,
                 listen_together_sync_host_volume = excluded.listen_together_sync_host_volume,
                 equalizer_active_profile_json = excluded.equalizer_active_profile_json",
            params![
                settings.proxy.enabled,
                settings.proxy.kind.storage_value(),
                settings.proxy.address.trim(),
                settings.proxy.username,
                settings.proxy.password,
                settings.audio_quality.storage_value(),
                cache_root,
                audio_cache_bytes,
                settings.theme.storage_value(),
                now_ms(),
                settings.auto_radio,
                settings.youtube_history_sync,
                settings.audio_normalization,
                settings.loudness_level.storage_value(),
                settings.equalizer.enabled,
                equalizer_gains_json,
                settings.playback_parameters.varispeed,
                settings.playback_parameters.tempo_milli,
                settings.playback_parameters.transpose_semitones,
                settings.lastfm_scrobbling,
                settings.lastfm_now_playing,
                settings.lastfm_sync_likes,
                settings.lastfm_scrobble_policy.min_track_seconds,
                settings.lastfm_scrobble_policy.delay_percent_milli,
                settings.lastfm_scrobble_policy.max_delay_seconds,
                settings.discord_rich_presence,
                settings.listen_together.server_url,
                settings.listen_together.username,
                settings.listen_together.auto_approve_joins,
                settings.listen_together.auto_approve_suggestions,
                settings.listen_together.sync_host_volume,
                equalizer_active_profile_json,
            ],
        )
        .map_err(storage_error)?;
    Ok(())
}

fn load_settings(connection: &Connection) -> Result<Option<AppSettings>> {
    let stored = connection
        .query_row(
            "SELECT proxy_enabled, proxy_kind, proxy_address, proxy_username,
                    proxy_password, audio_quality, cache_root,
                    audio_cache_bytes, theme, auto_radio, youtube_history_sync,
                    audio_normalization, loudness_level,
                    equalizer_enabled, equalizer_gains_json, playback_varispeed,
                    playback_tempo_milli, playback_transpose_semitones,
                    lastfm_scrobbling, lastfm_now_playing, lastfm_sync_likes,
                    lastfm_min_track_seconds, lastfm_delay_percent_milli,
                    lastfm_max_delay_seconds, discord_rich_presence,
                    listen_together_server_url, listen_together_username,
                    listen_together_auto_approve_joins,
                    listen_together_auto_approve_suggestions,
                    listen_together_sync_host_volume,
                    equalizer_active_profile_json
             FROM app_settings WHERE singleton = 1",
            [],
            |row| {
                Ok((
                    row.get::<_, bool>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, String>(6)?,
                    row.get::<_, i64>(7)?,
                    row.get::<_, String>(8)?,
                    row.get::<_, bool>(9)?,
                    row.get::<_, bool>(10)?,
                    row.get::<_, bool>(11)?,
                    row.get::<_, String>(12)?,
                    row.get::<_, bool>(13)?,
                    row.get::<_, String>(14)?,
                    row.get::<_, bool>(15)?,
                    row.get::<_, u16>(16)?,
                    row.get::<_, i8>(17)?,
                    row.get::<_, bool>(18)?,
                    row.get::<_, bool>(19)?,
                    row.get::<_, bool>(20)?,
                    row.get::<_, u16>(21)?,
                    row.get::<_, u16>(22)?,
                    row.get::<_, u16>(23)?,
                    row.get::<_, bool>(24)?,
                    row.get::<_, String>(25)?,
                    row.get::<_, String>(26)?,
                    row.get::<_, bool>(27)?,
                    row.get::<_, bool>(28)?,
                    row.get::<_, bool>(29)?,
                    row.get::<_, Option<String>>(30)?,
                ))
            },
        )
        .optional()
        .map_err(storage_error)?;
    let Some((
        proxy_enabled,
        proxy_kind,
        proxy_address,
        proxy_username,
        proxy_password,
        audio_quality,
        cache_root,
        audio_cache_bytes,
        theme,
        auto_radio,
        youtube_history_sync,
        audio_normalization,
        loudness_level,
        equalizer_enabled,
        equalizer_gains_json,
        playback_varispeed,
        playback_tempo_milli,
        playback_transpose_semitones,
        lastfm_scrobbling,
        lastfm_now_playing,
        lastfm_sync_likes,
        lastfm_min_track_seconds,
        lastfm_delay_percent_milli,
        lastfm_max_delay_seconds,
        discord_rich_presence,
        listen_together_server_url,
        listen_together_username,
        listen_together_auto_approve_joins,
        listen_together_auto_approve_suggestions,
        listen_together_sync_host_volume,
        equalizer_active_profile_json,
    )) = stored
    else {
        return Ok(None);
    };
    let audio_cache_bytes = u64::try_from(audio_cache_bytes)
        .map_err(|_| AppError::Storage("stored audio cache capacity is invalid".into()))?;
    let equalizer_gains = serde_json::from_str(&equalizer_gains_json).map_err(|error| {
        AppError::Storage(format!("stored equalizer settings are invalid: {error}"))
    })?;
    let active_profile = equalizer_active_profile_json
        .map(|profile| serde_json::from_str::<EqualizerProfile>(&profile))
        .transpose()
        .map_err(|error| {
            AppError::Storage(format!(
                "stored active equalizer profile is invalid: {error}"
            ))
        })?;
    let settings = AppSettings {
        proxy: ProxySettings {
            enabled: proxy_enabled,
            kind: ProxyKind::from_storage(&proxy_kind)?,
            address: proxy_address,
            username: proxy_username,
            password: proxy_password,
        },
        audio_quality: AudioQuality::from_storage(&audio_quality)?,
        audio_normalization,
        loudness_level: LoudnessLevel::from_storage(&loudness_level)?,
        equalizer: EqualizerSettings {
            enabled: equalizer_enabled,
            gains_mb: equalizer_gains,
            active_profile,
        },
        playback_parameters: PlaybackParameters {
            varispeed: playback_varispeed,
            tempo_milli: playback_tempo_milli,
            transpose_semitones: playback_transpose_semitones,
        },
        cache_root: PathBuf::from(cache_root),
        audio_cache_bytes,
        auto_radio,
        youtube_history_sync,
        lastfm_scrobbling,
        lastfm_now_playing,
        lastfm_sync_likes,
        lastfm_scrobble_policy: crate::services::LastFmScrobblePolicy {
            min_track_seconds: lastfm_min_track_seconds,
            delay_percent_milli: lastfm_delay_percent_milli,
            max_delay_seconds: lastfm_max_delay_seconds,
        },
        discord_rich_presence,
        listen_together: ListenTogetherSettings {
            server_url: listen_together_server_url,
            username: listen_together_username,
            auto_approve_joins: listen_together_auto_approve_joins,
            auto_approve_suggestions: listen_together_auto_approve_suggestions,
            sync_host_volume: listen_together_sync_host_volume,
        },
        theme: AppTheme::from_storage(&theme)?,
    };
    settings.validate().map(Some).map_err(|error| {
        AppError::Storage(format!("stored application settings are invalid: {error}"))
    })
}

fn save_equalizer_profile(connection: &mut Connection, profile: &EqualizerProfile) -> Result<()> {
    profile.validate()?;
    upsert_equalizer_profile(connection, profile)
}

fn save_equalizer_profiles(
    connection: &mut Connection,
    profiles: &[EqualizerProfile],
) -> Result<()> {
    if profiles.is_empty() {
        return Err(AppError::InvalidConfig(
            "at least one equalizer profile is required".into(),
        ));
    }
    for profile in profiles {
        profile.validate()?;
    }
    let transaction = connection.transaction().map_err(storage_error)?;
    for profile in profiles {
        upsert_equalizer_profile(&transaction, profile)?;
    }
    transaction.commit().map_err(storage_error)
}

fn upsert_equalizer_profile(connection: &Connection, profile: &EqualizerProfile) -> Result<()> {
    let equalizer_json = serde_json::to_string(&profile.equalizer).map_err(|error| {
        AppError::Storage(format!("equalizer profile could not be saved: {error}"))
    })?;
    connection
        .execute(
            "INSERT INTO equalizer_profile(
                 id, name, device_model, equalizer_json, source, rig,
                 is_custom, added_at_ms
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
             ON CONFLICT(id) DO UPDATE SET
                 name = excluded.name,
                 device_model = excluded.device_model,
                 equalizer_json = excluded.equalizer_json,
                 source = excluded.source,
                 rig = excluded.rig,
                 is_custom = excluded.is_custom,
                 added_at_ms = excluded.added_at_ms",
            params![
                profile.id,
                profile.name,
                profile.device_model,
                equalizer_json,
                profile.source,
                profile.rig,
                profile.is_custom,
                profile.added_at_ms,
            ],
        )
        .map_err(storage_error)?;
    Ok(())
}

fn equalizer_profiles(connection: &Connection) -> Result<Vec<EqualizerProfile>> {
    let mut statement = connection
        .prepare(
            "SELECT id, name, device_model, equalizer_json, source, rig,
                    is_custom, added_at_ms
             FROM equalizer_profile
             ORDER BY added_at_ms DESC, id ASC",
        )
        .map_err(storage_error)?;
    let rows = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, String>(5)?,
                row.get::<_, bool>(6)?,
                row.get::<_, i64>(7)?,
            ))
        })
        .map_err(storage_error)?;
    let mut profiles = Vec::new();
    for row in rows {
        let (id, name, device_model, equalizer_json, source, rig, is_custom, added_at_ms) =
            row.map_err(storage_error)?;
        let equalizer =
            serde_json::from_str::<ParametricEqualizer>(&equalizer_json).map_err(|error| {
                AppError::Storage(format!(
                    "stored equalizer profile '{id}' is invalid: {error}"
                ))
            })?;
        let profile = EqualizerProfile {
            id,
            name,
            device_model,
            equalizer,
            source,
            rig,
            is_custom,
            added_at_ms,
        };
        profile.validate().map_err(|error| {
            AppError::Storage(format!("stored equalizer profile is invalid: {error}"))
        })?;
        profiles.push(profile);
    }
    Ok(profiles)
}

fn delete_equalizer_profile(connection: &Connection, profile_id: &str) -> Result<()> {
    let active_profile = connection
        .query_row(
            "SELECT equalizer_enabled, equalizer_active_profile_json
             FROM app_settings WHERE singleton = 1",
            [],
            |row| Ok((row.get::<_, bool>(0)?, row.get::<_, Option<String>>(1)?)),
        )
        .optional()
        .map_err(storage_error)?;
    if let Some((true, Some(active_profile_json))) = active_profile {
        let active_profile = serde_json::from_str::<EqualizerProfile>(&active_profile_json)
            .map_err(|error| {
                AppError::Storage(format!(
                    "stored active equalizer profile is invalid: {error}"
                ))
            })?;
        if active_profile.id == profile_id {
            return Err(AppError::Storage(
                "disable the active equalizer profile before deleting it".into(),
            ));
        }
    }
    connection
        .execute("DELETE FROM equalizer_profile WHERE id = ?1", [profile_id])
        .map_err(storage_error)?;
    Ok(())
}

fn queue_download(
    connection: &mut Connection,
    song: &Song,
    audio_quality: AudioQuality,
) -> Result<AudioDownload> {
    let transaction = connection.transaction().map_err(storage_error)?;
    upsert_song(&transaction, song)?;
    let existing = transaction
        .query_row(
            "SELECT audio_quality, state FROM audio_download WHERE video_id = ?1",
            [&song.video_id],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
        )
        .optional()
        .map_err(storage_error)?;
    let timestamp = now_ms();
    match existing {
        Some((quality, state))
            if quality == audio_quality.storage_value() && state == "completed" => {}
        Some((quality, _)) if quality == audio_quality.storage_value() => {
            transaction
                .execute(
                    "UPDATE audio_download
                     SET state = 'queued', updated_at_ms = ?2,
                         completed_at_ms = NULL, last_error = NULL
                     WHERE video_id = ?1",
                    params![song.video_id, timestamp],
                )
                .map_err(storage_error)?;
        }
        Some(_) => {
            transaction
                .execute(
                    "UPDATE audio_download
                     SET audio_quality = ?2, mime_type = NULL, content_length = NULL,
                         loudness_lufs_mb = NULL, downloaded_bytes = 0,
                         state = 'queued', updated_at_ms = ?3,
                         completed_at_ms = NULL, last_error = NULL
                     WHERE video_id = ?1",
                    params![song.video_id, audio_quality.storage_value(), timestamp],
                )
                .map_err(storage_error)?;
        }
        None => {
            transaction
                .execute(
                    "INSERT INTO audio_download(
                         video_id, audio_quality, downloaded_bytes, state,
                         created_at_ms, updated_at_ms
                     ) VALUES (?1, ?2, 0, 'queued', ?3, ?3)",
                    params![song.video_id, audio_quality.storage_value(), timestamp],
                )
                .map_err(storage_error)?;
        }
    }
    transaction.commit().map_err(storage_error)?;
    download_by_id(connection, &song.video_id)?.ok_or_else(|| {
        AppError::Storage("queued download disappeared before it could be loaded".into())
    })
}

fn mark_download_started(
    connection: &Connection,
    video_id: &str,
    mime_type: &str,
    content_length: u64,
    downloaded_bytes: u64,
    loudness_lufs_mb: Option<i32>,
) -> Result<()> {
    if !mime_type.starts_with("audio/")
        || content_length == 0
        || downloaded_bytes > content_length
        || !valid_loudness_lufs_mb(loudness_lufs_mb)
    {
        return Err(AppError::Storage(
            "download start metadata is invalid".into(),
        ));
    }
    let changed = connection
        .execute(
            "UPDATE audio_download
             SET mime_type = ?2, content_length = ?3, downloaded_bytes = ?4,
                 loudness_lufs_mb = ?5, state = 'downloading', updated_at_ms = ?6,
                 completed_at_ms = NULL, last_error = NULL
             WHERE video_id = ?1 AND state != 'completed'",
            params![
                video_id,
                mime_type,
                u64_to_i64(content_length, "download content length")?,
                u64_to_i64(downloaded_bytes, "download progress")?,
                loudness_lufs_mb,
                now_ms(),
            ],
        )
        .map_err(storage_error)?;
    require_download_change(changed)
}

fn update_download_progress(
    connection: &Connection,
    video_id: &str,
    downloaded_bytes: u64,
) -> Result<()> {
    let changed = connection
        .execute(
            "UPDATE audio_download
             SET downloaded_bytes = ?2, updated_at_ms = ?3
             WHERE video_id = ?1 AND state = 'downloading'
               AND content_length IS NOT NULL
               AND ?2 >= downloaded_bytes AND ?2 <= content_length",
            params![
                video_id,
                u64_to_i64(downloaded_bytes, "download progress")?,
                now_ms(),
            ],
        )
        .map_err(storage_error)?;
    require_download_change(changed)
}

fn finish_download(connection: &Connection, video_id: &str) -> Result<()> {
    let timestamp = now_ms();
    let changed = connection
        .execute(
            "UPDATE audio_download
             SET downloaded_bytes = content_length, state = 'completed',
                 updated_at_ms = ?2, completed_at_ms = ?2, last_error = NULL
             WHERE video_id = ?1 AND state = 'downloading'
               AND mime_type IS NOT NULL AND content_length IS NOT NULL
               AND downloaded_bytes = content_length",
            params![video_id, timestamp],
        )
        .map_err(storage_error)?;
    require_download_change(changed)
}

fn stop_download(
    connection: &Connection,
    video_id: &str,
    state: DownloadState,
    error: Option<&str>,
) -> Result<()> {
    if !matches!(state, DownloadState::Paused | DownloadState::Failed) {
        return Err(AppError::Storage(
            "a stopped download must be paused or failed".into(),
        ));
    }
    let error = error.map(|error| error.chars().take(2_048).collect::<String>());
    let changed = connection
        .execute(
            "UPDATE audio_download
             SET state = ?2, updated_at_ms = ?3,
                 completed_at_ms = NULL, last_error = ?4
             WHERE video_id = ?1 AND (?2 = 'failed' OR state != 'completed')",
            params![video_id, state.storage_value(), now_ms(), error],
        )
        .map_err(storage_error)?;
    require_download_change(changed)
}

fn downloads(connection: &Connection) -> Result<Vec<AudioDownload>> {
    let mut statement = connection
        .prepare(
            "SELECT d.audio_quality, d.mime_type, d.content_length,
                    d.downloaded_bytes, d.state, d.created_at_ms,
                    d.updated_at_ms, d.completed_at_ms, d.last_error,
                    d.loudness_lufs_mb,
                    s.video_id, s.title, s.artists_json, s.duration_ms, s.thumbnail_url,
                    s.is_episode
             FROM audio_download d
             JOIN song s ON s.video_id = d.video_id
             ORDER BY COALESCE(d.completed_at_ms, d.updated_at_ms) DESC, d.video_id ASC",
        )
        .map_err(storage_error)?;
    let rows = statement
        .query_map([], download_from_row)
        .map_err(storage_error)?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(storage_error)
}

fn download_by_id(connection: &Connection, video_id: &str) -> Result<Option<AudioDownload>> {
    connection
        .query_row(
            "SELECT d.audio_quality, d.mime_type, d.content_length,
                    d.downloaded_bytes, d.state, d.created_at_ms,
                    d.updated_at_ms, d.completed_at_ms, d.last_error,
                    d.loudness_lufs_mb,
                    s.video_id, s.title, s.artists_json, s.duration_ms, s.thumbnail_url,
                    s.is_episode
             FROM audio_download d
             JOIN song s ON s.video_id = d.video_id
             WHERE d.video_id = ?1",
            [video_id],
            download_from_row,
        )
        .optional()
        .map_err(storage_error)
}

fn delete_download(connection: &Connection, video_id: &str) -> Result<()> {
    connection
        .execute("DELETE FROM audio_download WHERE video_id = ?1", [video_id])
        .map_err(storage_error)?;
    Ok(())
}

fn download_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<AudioDownload> {
    let quality: String = row.get(0)?;
    let state: String = row.get(4)?;
    let content_length: Option<i64> = row.get(2)?;
    let downloaded_bytes: i64 = row.get(3)?;
    Ok(AudioDownload {
        audio_quality: AudioQuality::from_storage(&quality)
            .map_err(|error| row_conversion_error(0, error))?,
        mime_type: row.get(1)?,
        content_length: content_length
            .map(|value| {
                u64::try_from(value).map_err(|_| rusqlite::Error::IntegralValueOutOfRange(2, value))
            })
            .transpose()?,
        downloaded_bytes: u64::try_from(downloaded_bytes)
            .map_err(|_| rusqlite::Error::IntegralValueOutOfRange(3, downloaded_bytes))?,
        state: DownloadState::from_storage(&state)
            .map_err(|error| row_conversion_error(4, error))?,
        created_at_ms: row.get(5)?,
        updated_at_ms: row.get(6)?,
        completed_at_ms: row.get(7)?,
        last_error: row.get(8)?,
        loudness_lufs_mb: row.get(9)?,
        song: song_from_row(row, 10)?,
    })
}

fn row_conversion_error(index: usize, error: AppError) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(index, rusqlite::types::Type::Text, Box::new(error))
}

fn u64_to_i64(value: u64, label: &str) -> Result<i64> {
    i64::try_from(value).map_err(|_| AppError::Storage(format!("{label} is too large for SQLite")))
}

fn valid_loudness_lufs_mb(value: Option<i32>) -> bool {
    value.is_none_or(|value| (-10_000..=2_000).contains(&value))
}

fn require_download_change(changed: usize) -> Result<()> {
    if changed == 1 {
        Ok(())
    } else {
        Err(AppError::Storage(
            "download state changed before the operation completed".into(),
        ))
    }
}

fn song_from_row(row: &rusqlite::Row<'_>, offset: usize) -> rusqlite::Result<Song> {
    let artists_json: String = row.get(offset + 2)?;
    let artists = serde_json::from_str(&artists_json).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(
            offset + 2,
            rusqlite::types::Type::Text,
            Box::new(error),
        )
    })?;
    let duration_ms: Option<i64> = row.get(offset + 3)?;
    Ok(Song {
        video_id: row.get(offset)?,
        title: row.get(offset + 1)?,
        artists,
        duration: duration_ms
            .and_then(|value| u64::try_from(value).ok())
            .map(Duration::from_millis),
        thumbnail_url: row.get(offset + 4)?,
        album: None,
        is_episode: row.get(offset + 5)?,
    })
}

fn upsert_song(transaction: &Transaction<'_>, song: &Song) -> Result<()> {
    let artists = serde_json::to_string(&song.artists)
        .map_err(|error| AppError::Storage(error.to_string()))?;
    let timestamp = now_ms();
    transaction
        .execute(
            "INSERT INTO song(
                 video_id, title, artists_json, duration_ms, thumbnail_url, is_episode,
                 created_at_ms, updated_at_ms
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7)
             ON CONFLICT(video_id) DO UPDATE SET
                 title = excluded.title,
                 artists_json = excluded.artists_json,
                 duration_ms = excluded.duration_ms,
                 thumbnail_url = excluded.thumbnail_url,
                 is_episode = excluded.is_episode,
                 updated_at_ms = excluded.updated_at_ms",
            params![
                song.video_id,
                song.title,
                artists,
                song.duration.map(duration_to_i64_ms).transpose()?,
                song.thumbnail_url,
                song.is_episode,
                timestamp,
            ],
        )
        .map_err(storage_error)?;
    if let Some(album) = &song.album {
        transaction
            .execute(
                "INSERT INTO song_album(video_id, browse_id, title, thumbnail_url)
                 VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT(video_id) DO UPDATE SET
                     browse_id = excluded.browse_id,
                     title = excluded.title,
                     thumbnail_url = COALESCE(excluded.thumbnail_url, song_album.thumbnail_url)",
                params![
                    song.video_id,
                    album.browse_id,
                    album.title,
                    album.thumbnail_url,
                ],
            )
            .map_err(storage_error)?;
    }
    Ok(())
}

fn duration_to_i64_ms(duration: Duration) -> Result<i64> {
    i64::try_from(duration.as_millis())
        .map_err(|_| AppError::Storage("duration is too large for SQLite".into()))
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|duration| i64::try_from(duration.as_millis()).ok())
        .unwrap_or_default()
}

fn storage_error(error: rusqlite::Error) -> AppError {
    AppError::Storage(error.to_string())
}

fn playlist_name_error(error: rusqlite::Error) -> AppError {
    if matches!(
        &error,
        rusqlite::Error::SqliteFailure(inner, _)
            if inner.code == rusqlite::ErrorCode::ConstraintViolation
    ) {
        AppError::Storage("a playlist with that name already exists".into())
    } else {
        storage_error(error)
    }
}

fn storage_stopped() -> AppError {
    AppError::Storage("storage worker stopped unexpectedly".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn song(id: &str) -> Song {
        Song {
            video_id: id.into(),
            title: format!("Song {id}"),
            artists: vec![ArtistCredit {
                id: Some(format!("artist-{id}")),
                name: format!("Artist {id}"),
            }],
            duration: Some(Duration::from_secs(180)),
            thumbnail_url: Some(format!("https://example.invalid/{id}.jpg")),
            album: None,
            is_episode: false,
        }
    }

    fn episode(id: &str) -> Song {
        Song {
            is_episode: true,
            ..song(id)
        }
    }

    fn podcast(id: &str, subscribed_at_ms: i64) -> PodcastSubscription {
        PodcastSubscription {
            podcast_id: id.into(),
            title: format!("Podcast {id}"),
            author: Some(format!("Host {id}")),
            thumbnail_url: Some(format!("https://example.invalid/{id}.jpg")),
            channel_id: Some(format!("UC-{id}")),
            subscribed_at_ms,
        }
    }

    fn equalizer_profile(id: &str) -> EqualizerProfile {
        EqualizerProfile {
            id: id.into(),
            name: format!("Profile {id}"),
            device_model: format!("Headphones {id}"),
            equalizer: ParametricEqualizer {
                preamp_mb: -520,
                bands: vec![crate::ParametricEqualizerBand {
                    filter_type: crate::ParametricFilterType::Peaking,
                    frequency_millihz: 1_000_000,
                    gain_mb: 350,
                    q_milli: 1_410,
                    enabled: true,
                }],
            },
            source: "fixture".into(),
            rig: "fixture".into(),
            is_custom: true,
            added_at_ms: 1_700_000_000_000,
        }
    }

    fn insert_song_before_v15(transaction: &Transaction<'_>, song: &Song) {
        let artists = serde_json::to_string(&song.artists).unwrap();
        transaction
            .execute(
                "INSERT INTO song(
                     video_id, title, artists_json, duration_ms, thumbnail_url,
                     created_at_ms, updated_at_ms
                 ) VALUES (?1, ?2, ?3, ?4, ?5, 0, 0)",
                params![
                    song.video_id,
                    song.title,
                    artists,
                    song.duration.map(duration_to_i64_ms).transpose().unwrap(),
                    song.thumbnail_url,
                ],
            )
            .unwrap();
    }

    fn temporary_database(name: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "metrolist-storage-test-{name}-{}-{:?}.sqlite3",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_file(&path);
        path
    }

    fn downgrade_playback_session_to_v18(connection: &Connection) {
        connection
            .execute_batch(
                "ALTER TABLE playback_session DROP COLUMN repeat_mode;
                 ALTER TABLE playback_session DROP COLUMN shuffle_enabled;
                 DELETE FROM schema_migration WHERE version = 19;",
            )
            .unwrap();
    }

    #[test]
    fn migrations_create_versioned_desktop_schema() {
        let path = temporary_database("migration");
        let store = DesktopStore::open(&path).unwrap();
        drop(store);

        let connection = Connection::open(&path).unwrap();
        let version: i64 = connection
            .query_row("SELECT MAX(version) FROM schema_migration", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(version, SCHEMA_VERSION);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn podcast_library_and_episode_resume_state_are_local_first_and_independent() {
        let path = temporary_database("podcast-state");
        let store = DesktopStore::open(&path).unwrap();
        let podcast = PodcastSubscription {
            podcast_id: "MPSPfixture".into(),
            title: "Fixture Podcast".into(),
            author: Some("Fixture Host".into()),
            thumbnail_url: Some("https://example.invalid/podcast.jpg".into()),
            channel_id: Some("UCfixture".into()),
            subscribed_at_ms: 1_700_000_000_000,
        };

        futures::executor::block_on(store.set_podcast_subscription(podcast.clone(), true)).unwrap();
        assert_eq!(
            futures::executor::block_on(store.podcast_subscriptions()).unwrap(),
            vec![podcast.clone()]
        );

        let saved_episode = episode("episode-one");
        futures::executor::block_on(store.set_episode_for_later(saved_episode.clone(), true))
            .unwrap();
        futures::executor::block_on(
            store.save_episode_playback_position(saved_episode.clone(), Duration::from_secs(2)),
        )
        .unwrap();
        assert_eq!(
            futures::executor::block_on(
                store.episode_playback_position(saved_episode.video_id.clone())
            )
            .unwrap(),
            None
        );
        futures::executor::block_on(
            store.save_episode_playback_position(saved_episode.clone(), Duration::from_secs(42)),
        )
        .unwrap();
        let episodes = futures::executor::block_on(store.episodes_for_later()).unwrap();
        assert_eq!(episodes.len(), 1);
        assert_eq!(episodes[0].song, saved_episode);
        assert_eq!(episodes[0].playback_position, Some(Duration::from_secs(42)));

        futures::executor::block_on(store.set_episode_for_later(episode("episode-one"), false))
            .unwrap();
        assert!(
            futures::executor::block_on(store.episodes_for_later())
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            futures::executor::block_on(store.episode_playback_position("episode-one".into()))
                .unwrap(),
            Some(Duration::from_secs(42))
        );
        assert!(
            futures::executor::block_on(store.set_episode_for_later(song("not-episode"), true))
                .is_err()
        );

        futures::executor::block_on(store.set_podcast_subscription(podcast, false)).unwrap();
        assert!(
            futures::executor::block_on(store.podcast_subscriptions())
                .unwrap()
                .is_empty()
        );
        drop(store);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn remote_podcast_reconciliation_is_atomic_server_first_and_respects_tombstones() {
        let path = temporary_database("podcast-reconcile");
        let store = DesktopStore::open(&path).unwrap();
        let kept = podcast("MPSP-kept", 10);
        let removed = podcast("MPSP-removed", 20);
        let tombstoned = podcast("MPSP-tombstoned", 30);
        for item in [&kept, &removed, &tombstoned] {
            futures::executor::block_on(store.set_podcast_subscription(item.clone(), true))
                .unwrap();
        }
        futures::executor::block_on(store.set_podcast_subscription(tombstoned.clone(), false))
            .unwrap();

        let kept_episode = episode("episode-kept");
        let removed_episode = episode("episode-removed");
        futures::executor::block_on(store.set_episode_for_later(kept_episode.clone(), true))
            .unwrap();
        futures::executor::block_on(store.set_episode_for_later(removed_episode, true)).unwrap();
        futures::executor::block_on(
            store.save_episode_playback_position(kept_episode.clone(), Duration::from_secs(75)),
        )
        .unwrap();

        let mut remote_kept = kept.clone();
        remote_kept.title = "Updated remote title".into();
        remote_kept.subscribed_at_ms = 999;
        let remote_new = podcast("MPSP-new", 40);
        let mut updated_episode = kept_episode.clone();
        updated_episode.title = "Updated remote episode".into();
        let remote_episode = episode("episode-new");
        let summary = futures::executor::block_on(store.reconcile_podcast_library(
            vec![remote_kept, remote_new.clone(), tombstoned.clone()],
            vec![updated_episode, remote_episode],
        ))
        .unwrap();

        assert_eq!(
            summary,
            PodcastLibraryReconcileSummary {
                podcast_count: 2,
                episode_count: 2,
                removed_podcast_count: 1,
                removed_episode_count: 1,
                skipped_podcast_tombstones: 1,
            }
        );
        let podcasts = futures::executor::block_on(store.podcast_subscriptions()).unwrap();
        assert_eq!(podcasts.len(), 2);
        let kept = podcasts
            .iter()
            .find(|podcast| podcast.podcast_id == "MPSP-kept")
            .unwrap();
        assert_eq!(kept.title, "Updated remote title");
        assert_eq!(kept.subscribed_at_ms, 10);
        assert!(
            podcasts
                .iter()
                .any(|podcast| podcast.podcast_id == remote_new.podcast_id)
        );
        assert!(
            podcasts
                .iter()
                .all(|podcast| podcast.podcast_id != tombstoned.podcast_id)
        );
        let episodes = futures::executor::block_on(store.episodes_for_later()).unwrap();
        assert_eq!(episodes.len(), 2);
        let kept = episodes
            .iter()
            .find(|episode| episode.song.video_id == "episode-kept")
            .unwrap();
        assert_eq!(kept.song.title, "Updated remote episode");
        assert_eq!(kept.playback_position, Some(Duration::from_secs(75)));

        let before = podcasts;
        assert!(
            futures::executor::block_on(
                store.reconcile_podcast_library(vec![], vec![song("not-an-episode")])
            )
            .is_err()
        );
        assert_eq!(
            futures::executor::block_on(store.podcast_subscriptions()).unwrap(),
            before
        );

        futures::executor::block_on(store.set_podcast_subscription(tombstoned.clone(), true))
            .unwrap();
        futures::executor::block_on(store.reconcile_podcast_library(vec![tombstoned], Vec::new()))
            .unwrap();
        assert_eq!(
            futures::executor::block_on(store.podcast_subscriptions())
                .unwrap()
                .len(),
            1
        );
        drop(store);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn v16_database_upgrades_to_podcast_state_without_losing_library_data() {
        let path = temporary_database("v16-podcast-upgrade");
        let store = DesktopStore::open(&path).unwrap();
        futures::executor::block_on(store.set_favorite(song("kept-v16"), true)).unwrap();
        drop(store);

        let connection = Connection::open(&path).unwrap();
        downgrade_playback_session_to_v18(&connection);
        connection
            .execute_batch(
                "DROP TABLE podcast_subscription_tombstone;
                 DELETE FROM schema_migration WHERE version = 18;
                 DROP TABLE episode_playback_position;
                 DROP TABLE episode_for_later;
                 DROP TABLE podcast_subscription;
                 DELETE FROM schema_migration WHERE version = 17;",
            )
            .unwrap();
        drop(connection);

        let store = DesktopStore::open(&path).unwrap();
        let favorites = futures::executor::block_on(store.favorites(10)).unwrap();
        assert_eq!(favorites.len(), 1);
        assert_eq!(favorites[0].song.video_id, "kept-v16");
        futures::executor::block_on(store.set_episode_for_later(episode("new-episode"), true))
            .unwrap();
        assert_eq!(
            futures::executor::block_on(store.episodes_for_later())
                .unwrap()
                .len(),
            1
        );
        drop(store);

        let connection = Connection::open(&path).unwrap();
        let version: i64 = connection
            .query_row("SELECT MAX(version) FROM schema_migration", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(version, SCHEMA_VERSION);
        drop(connection);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn v17_database_upgrades_to_podcast_tombstones_without_losing_state() {
        let path = temporary_database("v17-podcast-tombstone-upgrade");
        let store = DesktopStore::open(&path).unwrap();
        let saved = podcast("MPSP-kept-v17", 17);
        futures::executor::block_on(store.set_podcast_subscription(saved.clone(), true)).unwrap();
        drop(store);

        let connection = Connection::open(&path).unwrap();
        downgrade_playback_session_to_v18(&connection);
        connection
            .execute_batch(
                "DROP TABLE podcast_subscription_tombstone;
                 DELETE FROM schema_migration WHERE version = 18;",
            )
            .unwrap();
        drop(connection);

        let store = DesktopStore::open(&path).unwrap();
        assert_eq!(
            futures::executor::block_on(store.podcast_subscriptions()).unwrap(),
            vec![saved.clone()]
        );
        futures::executor::block_on(store.set_podcast_subscription(saved, false)).unwrap();
        assert!(
            futures::executor::block_on(store.podcast_subscriptions())
                .unwrap()
                .is_empty()
        );
        drop(store);

        let connection = Connection::open(&path).unwrap();
        let version: i64 = connection
            .query_row("SELECT MAX(version) FROM schema_migration", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(version, SCHEMA_VERSION);
        let tombstone_count: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM podcast_subscription_tombstone",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(tombstone_count, 1);
        drop(connection);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn v18_database_upgrades_playback_modes_with_safe_defaults() {
        let path = temporary_database("v18-playback-modes-upgrade");
        let store = DesktopStore::open(&path).unwrap();
        let legacy_session = PersistedSession {
            queue: vec![song("one"), song("two")],
            current_index: Some(1),
            position: Duration::from_secs(42),
            volume: 0.7,
            repeat_mode: RepeatMode::One,
            shuffle_enabled: true,
            playback_source: None,
        };
        futures::executor::block_on(store.save_session(legacy_session)).unwrap();
        drop(store);

        let connection = Connection::open(&path).unwrap();
        downgrade_playback_session_to_v18(&connection);
        drop(connection);

        let store = DesktopStore::open(&path).unwrap();
        let restored = futures::executor::block_on(store.load_session())
            .unwrap()
            .unwrap();
        assert_eq!(restored.queue, vec![song("one"), song("two")]);
        assert_eq!(restored.current_index, Some(1));
        assert_eq!(restored.position, Duration::from_secs(42));
        assert_eq!(restored.repeat_mode, RepeatMode::Off);
        assert!(!restored.shuffle_enabled);
        drop(store);

        let connection = Connection::open(&path).unwrap();
        let version: i64 = connection
            .query_row("SELECT MAX(version) FROM schema_migration", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(version, SCHEMA_VERSION);
        let mut statement = connection
            .prepare("PRAGMA table_info(playback_session)")
            .unwrap();
        let columns = statement
            .query_map([], |row| row.get::<_, String>(1))
            .unwrap()
            .collect::<std::result::Result<HashSet<_>, _>>()
            .unwrap();
        assert!(columns.contains("repeat_mode"));
        assert!(columns.contains("shuffle_enabled"));
        drop(statement);
        drop(connection);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn download_lifecycle_persists_only_valid_completed_metadata() {
        let path = temporary_database("download-lifecycle");
        let store = DesktopStore::open(&path).unwrap();

        let queued =
            futures::executor::block_on(store.queue_download(song("offline"), AudioQuality::High))
                .unwrap();
        assert_eq!(queued.state, DownloadState::Queued);
        assert_eq!(queued.downloaded_bytes, 0);
        assert_eq!(queued.content_length, None);

        futures::executor::block_on(store.mark_download_started(
            "offline".into(),
            "audio/mp4; codecs=\"mp4a.40.2\"".into(),
            1_024,
            512,
            Some(-1_340),
        ))
        .unwrap();
        futures::executor::block_on(store.update_download_progress("offline".into(), 768)).unwrap();
        futures::executor::block_on(store.stop_download(
            "offline".into(),
            DownloadState::Paused,
            None,
        ))
        .unwrap();

        let paused = futures::executor::block_on(store.downloads()).unwrap();
        assert_eq!(paused.len(), 1);
        assert_eq!(paused[0].state, DownloadState::Paused);
        assert_eq!(paused[0].downloaded_bytes, 768);
        assert_eq!(paused[0].loudness_lufs_mb, Some(-1_340));

        futures::executor::block_on(store.queue_download(song("offline"), AudioQuality::High))
            .unwrap();
        futures::executor::block_on(store.mark_download_started(
            "offline".into(),
            "audio/mp4; codecs=\"mp4a.40.2\"".into(),
            1_024,
            768,
            Some(-1_340),
        ))
        .unwrap();
        assert!(
            futures::executor::block_on(store.update_download_progress("offline".into(), 700))
                .is_err()
        );
        assert!(futures::executor::block_on(store.finish_download("offline".into())).is_err());
        futures::executor::block_on(store.update_download_progress("offline".into(), 1_024))
            .unwrap();
        futures::executor::block_on(store.finish_download("offline".into())).unwrap();

        let completed = futures::executor::block_on(store.downloads()).unwrap();
        assert_eq!(completed[0].state, DownloadState::Completed);
        assert_eq!(completed[0].content_length, Some(1_024));
        assert_eq!(completed[0].downloaded_bytes, 1_024);
        assert!(completed[0].completed_at_ms.is_some());
        assert!(completed[0].last_error.is_none());
        assert_eq!(completed[0].loudness_lufs_mb, Some(-1_340));

        assert!(
            futures::executor::block_on(store.update_download_progress("offline".into(), 2_048))
                .is_err()
        );
        futures::executor::block_on(store.delete_download("offline".into())).unwrap();
        assert!(
            futures::executor::block_on(store.downloads())
                .unwrap()
                .is_empty()
        );
        drop(store);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn migration_rejects_a_database_from_a_newer_application() {
        let path = temporary_database("future-migration");
        let connection = Connection::open(&path).unwrap();
        connection
            .execute_batch(
                "CREATE TABLE schema_migration (
                     version INTEGER PRIMARY KEY,
                     applied_at_ms INTEGER NOT NULL
                 );
                 INSERT INTO schema_migration(version, applied_at_ms) VALUES (99, 0);",
            )
            .unwrap();
        drop(connection);

        let error = DesktopStore::open(&path).err().unwrap();
        assert!(matches!(error, AppError::Storage(_)));
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn v1_database_is_upgraded_without_losing_existing_data() {
        let path = temporary_database("v1-upgrade");
        let mut connection = Connection::open(&path).unwrap();
        connection
            .execute_batch(
                "PRAGMA foreign_keys = ON;
                 CREATE TABLE schema_migration (
                     version INTEGER PRIMARY KEY,
                     applied_at_ms INTEGER NOT NULL
                 );",
            )
            .unwrap();
        migrate_to_v1(&mut connection).unwrap();
        let transaction = connection.transaction().unwrap();
        insert_song_before_v15(&transaction, &song("kept"));
        transaction.commit().unwrap();
        drop(connection);

        let store = DesktopStore::open(&path).unwrap();
        futures::executor::block_on(store.set_favorite(song("kept"), true)).unwrap();
        let favorites = futures::executor::block_on(store.favorites(10)).unwrap();
        assert_eq!(favorites[0].song, song("kept"));
        drop(store);

        let connection = Connection::open(&path).unwrap();
        let version: i64 = connection
            .query_row("SELECT MAX(version) FROM schema_migration", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(version, SCHEMA_VERSION);
        let song_count: i64 = connection
            .query_row("SELECT COUNT(*) FROM song", [], |row| row.get(0))
            .unwrap();
        assert_eq!(song_count, 1);
        drop(connection);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn v5_settings_upgrade_enables_radio_and_youtube_history_by_default() {
        let path = temporary_database("v5-settings-upgrade");
        let mut connection = Connection::open(&path).unwrap();
        connection
            .execute_batch(
                "PRAGMA foreign_keys = ON;
                 CREATE TABLE schema_migration (
                     version INTEGER PRIMARY KEY,
                     applied_at_ms INTEGER NOT NULL
                 );",
            )
            .unwrap();
        migrate_to_v1(&mut connection).unwrap();
        migrate_to_v2(&mut connection).unwrap();
        migrate_to_v3(&mut connection).unwrap();
        migrate_to_v4(&mut connection).unwrap();
        migrate_to_v5(&mut connection).unwrap();
        connection
            .execute(
                "INSERT INTO app_settings(
                     singleton, proxy_enabled, proxy_kind, proxy_address,
                     proxy_username, proxy_password, audio_quality, cache_root,
                     audio_cache_bytes, theme, updated_at_ms
                 ) VALUES (1, 0, 'http', '', '', '', 'low', ?1, ?2, 'light', 0)",
                params![
                    std::env::temp_dir()
                        .join("metrolist-v5-upgrade-cache")
                        .to_string_lossy(),
                    128_i64 * 1024 * 1024,
                ],
            )
            .unwrap();
        drop(connection);

        let store = DesktopStore::open(&path).unwrap();
        let settings = futures::executor::block_on(store.load_settings())
            .unwrap()
            .unwrap();
        assert!(settings.auto_radio);
        assert!(settings.youtube_history_sync);
        assert_eq!(settings.audio_quality, AudioQuality::Low);
        assert_eq!(settings.theme, AppTheme::Light);
        drop(store);

        let connection = Connection::open(&path).unwrap();
        let version: i64 = connection
            .query_row("SELECT MAX(version) FROM schema_migration", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(version, SCHEMA_VERSION);
        drop(connection);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn v7_database_upgrades_to_download_schema_without_losing_library_data() {
        let path = temporary_database("v7-download-upgrade");
        let mut connection = Connection::open(&path).unwrap();
        connection
            .execute_batch(
                "PRAGMA foreign_keys = ON;
                 CREATE TABLE schema_migration (
                     version INTEGER PRIMARY KEY,
                     applied_at_ms INTEGER NOT NULL
                 );",
            )
            .unwrap();
        migrate_to_v1(&mut connection).unwrap();
        migrate_to_v2(&mut connection).unwrap();
        migrate_to_v3(&mut connection).unwrap();
        migrate_to_v4(&mut connection).unwrap();
        migrate_to_v5(&mut connection).unwrap();
        migrate_to_v6(&mut connection).unwrap();
        migrate_to_v7(&mut connection).unwrap();
        let transaction = connection.transaction().unwrap();
        insert_song_before_v15(&transaction, &song("kept-v7"));
        transaction
            .execute(
                "INSERT INTO favorite_song(video_id, liked_at_ms) VALUES ('kept-v7', 1)",
                [],
            )
            .unwrap();
        transaction.commit().unwrap();
        drop(connection);

        let store = DesktopStore::open(&path).unwrap();
        let favorites = futures::executor::block_on(store.favorites(10)).unwrap();
        assert_eq!(favorites[0].song, song("kept-v7"));
        let queued =
            futures::executor::block_on(store.queue_download(song("kept-v7"), AudioQuality::Auto))
                .unwrap();
        assert_eq!(queued.state, DownloadState::Queued);
        drop(store);

        let connection = Connection::open(&path).unwrap();
        let version: i64 = connection
            .query_row("SELECT MAX(version) FROM schema_migration", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(version, SCHEMA_VERSION);
        let mut statement = connection
            .prepare("PRAGMA table_info(audio_download)")
            .unwrap();
        let columns = statement
            .query_map([], |row| row.get::<_, String>(1))
            .unwrap()
            .collect::<std::result::Result<Vec<_>, _>>()
            .unwrap();
        assert!(columns.contains(&"video_id".into()));
        assert!(columns.contains(&"downloaded_bytes".into()));
        assert!(!columns.iter().any(|column| {
            column.contains("url") || column.contains("header") || column.contains("token")
        }));
        drop(statement);
        drop(connection);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn v8_database_upgrades_to_normalization_schema_with_safe_defaults() {
        let path = temporary_database("v8-normalization-upgrade");
        let mut connection = Connection::open(&path).unwrap();
        connection
            .execute_batch(
                "PRAGMA foreign_keys = ON;
                 CREATE TABLE schema_migration (
                     version INTEGER PRIMARY KEY,
                     applied_at_ms INTEGER NOT NULL
                 );",
            )
            .unwrap();
        migrate_to_v1(&mut connection).unwrap();
        migrate_to_v2(&mut connection).unwrap();
        migrate_to_v3(&mut connection).unwrap();
        migrate_to_v4(&mut connection).unwrap();
        migrate_to_v5(&mut connection).unwrap();
        migrate_to_v6(&mut connection).unwrap();
        migrate_to_v7(&mut connection).unwrap();
        migrate_to_v8(&mut connection).unwrap();
        connection
            .execute(
                "INSERT INTO app_settings(
                     singleton, proxy_enabled, proxy_kind, proxy_address,
                     proxy_username, proxy_password, audio_quality, cache_root,
                     audio_cache_bytes, auto_radio, youtube_history_sync, theme,
                     updated_at_ms
                 ) VALUES (1, 0, 'http', '', '', '', 'high', ?1, ?2, 1, 0, 'dark', 0)",
                params![
                    std::env::temp_dir()
                        .join("metrolist-v8-upgrade-cache")
                        .to_string_lossy(),
                    128_i64 * 1024 * 1024,
                ],
            )
            .unwrap();
        let transaction = connection.transaction().unwrap();
        insert_song_before_v15(&transaction, &song("kept-v8"));
        transaction
            .execute_batch(
                "INSERT INTO playback_session(
                     singleton, current_index, position_ms, volume, updated_at_ms
                 ) VALUES (1, 0, 1234, 0.7, 0);
                 INSERT INTO queue_item(position, video_id) VALUES (0, 'kept-v8');
                 INSERT INTO playback_source_session(
                     singleton, video_id, mime_type, content_length,
                     resolved_at_ms, expires_at_ms
                 ) VALUES (1, 'kept-v8', 'audio/mp4', 42, 100, 200);
                 INSERT INTO audio_download(
                     video_id, audio_quality, downloaded_bytes, state,
                     created_at_ms, updated_at_ms
                 ) VALUES ('kept-v8', 'high', 0, 'queued', 0, 0);",
            )
            .unwrap();
        transaction.commit().unwrap();
        drop(connection);

        let store = DesktopStore::open(&path).unwrap();
        let settings = futures::executor::block_on(store.load_settings())
            .unwrap()
            .unwrap();
        assert!(settings.audio_normalization);
        assert_eq!(settings.loudness_level, LoudnessLevel::Balanced);
        assert_eq!(settings.equalizer, EqualizerSettings::default());
        assert_eq!(settings.audio_quality, AudioQuality::High);
        assert!(!settings.youtube_history_sync);
        let session = futures::executor::block_on(store.load_session())
            .unwrap()
            .unwrap();
        assert_eq!(
            session
                .playback_source
                .as_ref()
                .and_then(|source| source.loudness_lufs_mb),
            None
        );
        let downloads = futures::executor::block_on(store.downloads()).unwrap();
        assert_eq!(downloads.len(), 1);
        assert_eq!(downloads[0].loudness_lufs_mb, None);
        drop(store);

        let connection = Connection::open(&path).unwrap();
        let version: i64 = connection
            .query_row("SELECT MAX(version) FROM schema_migration", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(version, SCHEMA_VERSION);
        drop(connection);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn v9_database_upgrades_to_equalizer_schema_with_flat_disabled_defaults() {
        let path = temporary_database("v9-equalizer-upgrade");
        let mut connection = Connection::open(&path).unwrap();
        connection
            .execute_batch(
                "PRAGMA foreign_keys = ON;
                 CREATE TABLE schema_migration (
                     version INTEGER PRIMARY KEY,
                     applied_at_ms INTEGER NOT NULL
                 );",
            )
            .unwrap();
        migrate_to_v1(&mut connection).unwrap();
        migrate_to_v2(&mut connection).unwrap();
        migrate_to_v3(&mut connection).unwrap();
        migrate_to_v4(&mut connection).unwrap();
        migrate_to_v5(&mut connection).unwrap();
        migrate_to_v6(&mut connection).unwrap();
        migrate_to_v7(&mut connection).unwrap();
        migrate_to_v8(&mut connection).unwrap();
        migrate_to_v9(&mut connection).unwrap();
        connection
            .execute(
                "INSERT INTO app_settings(
                     singleton, proxy_enabled, proxy_kind, proxy_address,
                     proxy_username, proxy_password, audio_quality, cache_root,
                     audio_cache_bytes, auto_radio, youtube_history_sync, theme,
                     updated_at_ms, audio_normalization, loudness_level
                 ) VALUES (1, 0, 'http', '', '', '', 'low', ?1, ?2, 0, 1,
                           'light', 0, 0, 'quiet')",
                params![
                    std::env::temp_dir()
                        .join("metrolist-v9-upgrade-cache")
                        .to_string_lossy(),
                    128_i64 * 1024 * 1024,
                ],
            )
            .unwrap();
        drop(connection);

        let store = DesktopStore::open(&path).unwrap();
        let settings = futures::executor::block_on(store.load_settings())
            .unwrap()
            .unwrap();
        assert_eq!(settings.equalizer, EqualizerSettings::default());
        assert_eq!(settings.playback_parameters, PlaybackParameters::default());
        assert!(!settings.audio_normalization);
        assert_eq!(settings.loudness_level, LoudnessLevel::Quiet);
        assert_eq!(settings.audio_quality, AudioQuality::Low);
        assert!(!settings.auto_radio);
        drop(store);

        let connection = Connection::open(&path).unwrap();
        let version: i64 = connection
            .query_row("SELECT MAX(version) FROM schema_migration", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(version, SCHEMA_VERSION);
        let columns = connection
            .prepare("PRAGMA table_info(app_settings)")
            .unwrap()
            .query_map([], |row| row.get::<_, String>(1))
            .unwrap()
            .collect::<std::result::Result<Vec<_>, _>>()
            .unwrap();
        assert!(columns.contains(&"equalizer_enabled".into()));
        assert!(columns.contains(&"equalizer_gains_json".into()));
        assert!(columns.contains(&"playback_varispeed".into()));
        assert!(columns.contains(&"playback_tempo_milli".into()));
        assert!(columns.contains(&"playback_transpose_semitones".into()));
        drop(connection);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn v10_database_upgrades_to_playback_parameters_with_android_defaults() {
        let path = temporary_database("v10-playback-parameters-upgrade");
        let mut connection = Connection::open(&path).unwrap();
        connection
            .execute_batch(
                "PRAGMA foreign_keys = ON;
                 CREATE TABLE schema_migration (
                     version INTEGER PRIMARY KEY,
                     applied_at_ms INTEGER NOT NULL
                 );",
            )
            .unwrap();
        migrate_to_v1(&mut connection).unwrap();
        migrate_to_v2(&mut connection).unwrap();
        migrate_to_v3(&mut connection).unwrap();
        migrate_to_v4(&mut connection).unwrap();
        migrate_to_v5(&mut connection).unwrap();
        migrate_to_v6(&mut connection).unwrap();
        migrate_to_v7(&mut connection).unwrap();
        migrate_to_v8(&mut connection).unwrap();
        migrate_to_v9(&mut connection).unwrap();
        migrate_to_v10(&mut connection).unwrap();
        connection
            .execute(
                "INSERT INTO app_settings(
                     singleton, proxy_enabled, proxy_kind, proxy_address,
                     proxy_username, proxy_password, audio_quality, cache_root,
                     audio_cache_bytes, theme, updated_at_ms, auto_radio,
                     youtube_history_sync, audio_normalization, loudness_level,
                     equalizer_enabled, equalizer_gains_json
                 ) VALUES (1, 0, 'http', '', '', '', 'high', ?1, ?2, 'dark',
                           0, 1, 0, 1, 'balanced', 1,
                           '[600,450,300,100,0,-100,-150,-150,-100,0]')",
                params![
                    std::env::temp_dir()
                        .join("metrolist-v10-upgrade-cache")
                        .to_string_lossy(),
                    128_i64 * 1024 * 1024,
                ],
            )
            .unwrap();
        drop(connection);

        let store = DesktopStore::open(&path).unwrap();
        let settings = futures::executor::block_on(store.load_settings())
            .unwrap()
            .unwrap();
        assert_eq!(settings.playback_parameters, PlaybackParameters::default());
        assert!(settings.equalizer.enabled);
        assert_eq!(settings.equalizer.gains_mb[0], 600);
        drop(store);

        let connection = Connection::open(&path).unwrap();
        let version: i64 = connection
            .query_row("SELECT MAX(version) FROM schema_migration", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(version, SCHEMA_VERSION);
        drop(connection);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn v11_database_upgrades_to_lastfm_settings_with_android_defaults() {
        let path = temporary_database("v11-lastfm-upgrade");
        let mut connection = Connection::open(&path).unwrap();
        connection
            .execute_batch(
                "PRAGMA foreign_keys = ON;
                 CREATE TABLE schema_migration (
                     version INTEGER PRIMARY KEY,
                     applied_at_ms INTEGER NOT NULL
                 );",
            )
            .unwrap();
        migrate_to_v1(&mut connection).unwrap();
        migrate_to_v2(&mut connection).unwrap();
        migrate_to_v3(&mut connection).unwrap();
        migrate_to_v4(&mut connection).unwrap();
        migrate_to_v5(&mut connection).unwrap();
        migrate_to_v6(&mut connection).unwrap();
        migrate_to_v7(&mut connection).unwrap();
        migrate_to_v8(&mut connection).unwrap();
        migrate_to_v9(&mut connection).unwrap();
        migrate_to_v10(&mut connection).unwrap();
        migrate_to_v11(&mut connection).unwrap();
        connection
            .execute(
                "INSERT INTO app_settings(
                     singleton, proxy_enabled, proxy_kind, proxy_address,
                     proxy_username, proxy_password, audio_quality, cache_root,
                     audio_cache_bytes, theme, updated_at_ms, auto_radio,
                     youtube_history_sync, audio_normalization, loudness_level,
                     equalizer_enabled, equalizer_gains_json, playback_varispeed,
                     playback_tempo_milli, playback_transpose_semitones
                 ) VALUES (1, 0, 'http', '', '', '', 'auto', ?1, ?2, 'dark',
                           0, 1, 1, 1, 'balanced', 0,
                           '[0,0,0,0,0,0,0,0,0,0]', 0, 1000, 0)",
                params![
                    std::env::temp_dir()
                        .join("metrolist-v11-upgrade-cache")
                        .to_string_lossy(),
                    128_i64 * 1024 * 1024,
                ],
            )
            .unwrap();
        drop(connection);

        let store = DesktopStore::open(&path).unwrap();
        let settings = futures::executor::block_on(store.load_settings())
            .unwrap()
            .unwrap();
        assert!(!settings.lastfm_scrobbling);
        assert!(!settings.lastfm_now_playing);
        assert!(!settings.lastfm_sync_likes);
        assert_eq!(
            settings.lastfm_scrobble_policy,
            crate::services::LastFmScrobblePolicy::default()
        );
        assert!(!settings.discord_rich_presence);
        drop(store);

        let connection = Connection::open(&path).unwrap();
        let version: i64 = connection
            .query_row("SELECT MAX(version) FROM schema_migration", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(version, SCHEMA_VERSION);
        let columns = connection
            .prepare("PRAGMA table_info(app_settings)")
            .unwrap()
            .query_map([], |row| row.get::<_, String>(1))
            .unwrap()
            .collect::<std::result::Result<Vec<_>, _>>()
            .unwrap();
        for column in [
            "lastfm_scrobbling",
            "lastfm_now_playing",
            "lastfm_sync_likes",
            "lastfm_min_track_seconds",
            "lastfm_delay_percent_milli",
            "lastfm_max_delay_seconds",
            "discord_rich_presence",
        ] {
            assert!(columns.contains(&column.into()));
        }
        drop(connection);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn v12_database_upgrades_to_opt_in_discord_rich_presence() {
        let path = temporary_database("v12-discord-upgrade");
        let mut connection = Connection::open(&path).unwrap();
        connection
            .execute_batch(
                "PRAGMA foreign_keys = ON;
                 CREATE TABLE schema_migration (
                     version INTEGER PRIMARY KEY,
                     applied_at_ms INTEGER NOT NULL
                 );",
            )
            .unwrap();
        migrate_to_v1(&mut connection).unwrap();
        migrate_to_v2(&mut connection).unwrap();
        migrate_to_v3(&mut connection).unwrap();
        migrate_to_v4(&mut connection).unwrap();
        migrate_to_v5(&mut connection).unwrap();
        migrate_to_v6(&mut connection).unwrap();
        migrate_to_v7(&mut connection).unwrap();
        migrate_to_v8(&mut connection).unwrap();
        migrate_to_v9(&mut connection).unwrap();
        migrate_to_v10(&mut connection).unwrap();
        migrate_to_v11(&mut connection).unwrap();
        migrate_to_v12(&mut connection).unwrap();
        connection
            .execute(
                "INSERT INTO app_settings(
                     singleton, proxy_enabled, proxy_kind, proxy_address,
                     proxy_username, proxy_password, audio_quality, cache_root,
                     audio_cache_bytes, theme, updated_at_ms
                 ) VALUES (1, 0, 'http', '', '', '', 'auto', ?1, ?2, 'dark', 0)",
                params![
                    std::env::temp_dir()
                        .join("metrolist-v12-upgrade-cache")
                        .to_string_lossy(),
                    128_i64 * 1024 * 1024,
                ],
            )
            .unwrap();
        drop(connection);

        let store = DesktopStore::open(&path).unwrap();
        let settings = futures::executor::block_on(store.load_settings())
            .unwrap()
            .unwrap();
        assert!(!settings.discord_rich_presence);
        drop(store);

        let connection = Connection::open(&path).unwrap();
        let version: i64 = connection
            .query_row("SELECT MAX(version) FROM schema_migration", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(version, SCHEMA_VERSION);
        let default: bool = connection
            .query_row(
                "SELECT discord_rich_presence FROM app_settings WHERE singleton = 1",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(!default);
        drop(connection);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn v13_database_upgrades_to_listen_together_defaults_without_session_secrets() {
        let path = temporary_database("v13-listen-together-upgrade");
        let mut connection = Connection::open(&path).unwrap();
        connection
            .execute_batch(
                "PRAGMA foreign_keys = ON;
                 CREATE TABLE schema_migration (
                     version INTEGER PRIMARY KEY,
                     applied_at_ms INTEGER NOT NULL
                 );",
            )
            .unwrap();
        migrate_to_v1(&mut connection).unwrap();
        migrate_to_v2(&mut connection).unwrap();
        migrate_to_v3(&mut connection).unwrap();
        migrate_to_v4(&mut connection).unwrap();
        migrate_to_v5(&mut connection).unwrap();
        migrate_to_v6(&mut connection).unwrap();
        migrate_to_v7(&mut connection).unwrap();
        migrate_to_v8(&mut connection).unwrap();
        migrate_to_v9(&mut connection).unwrap();
        migrate_to_v10(&mut connection).unwrap();
        migrate_to_v11(&mut connection).unwrap();
        migrate_to_v12(&mut connection).unwrap();
        migrate_to_v13(&mut connection).unwrap();
        connection
            .execute(
                "INSERT INTO app_settings(
                     singleton, proxy_enabled, proxy_kind, proxy_address,
                     proxy_username, proxy_password, audio_quality, cache_root,
                     audio_cache_bytes, theme, updated_at_ms
                 ) VALUES (1, 0, 'http', '', '', '', 'auto', ?1, ?2, 'dark', 0)",
                params![
                    std::env::temp_dir()
                        .join("metrolist-v13-upgrade-cache")
                        .to_string_lossy(),
                    128_i64 * 1024 * 1024,
                ],
            )
            .unwrap();
        drop(connection);

        let store = DesktopStore::open(&path).unwrap();
        let settings = futures::executor::block_on(store.load_settings())
            .unwrap()
            .unwrap();
        assert_eq!(settings.listen_together, ListenTogetherSettings::default());
        drop(store);

        let connection = Connection::open(&path).unwrap();
        let version: i64 = connection
            .query_row("SELECT MAX(version) FROM schema_migration", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(version, SCHEMA_VERSION);
        let columns = connection
            .prepare("PRAGMA table_info(app_settings)")
            .unwrap()
            .query_map([], |row| row.get::<_, String>(1))
            .unwrap()
            .collect::<std::result::Result<Vec<_>, _>>()
            .unwrap();
        for column in [
            "listen_together_server_url",
            "listen_together_username",
            "listen_together_auto_approve_joins",
            "listen_together_auto_approve_suggestions",
            "listen_together_sync_host_volume",
        ] {
            assert!(columns.contains(&column.into()));
        }
        assert!(
            columns
                .iter()
                .filter(|column| column.starts_with("listen_together_"))
                .all(|column| !column.contains("session") && !column.contains("token"))
        );
        drop(connection);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn v14_database_upgrades_songs_with_persisted_episode_identity() {
        let path = temporary_database("v14-episode-identity-upgrade");
        let mut connection = Connection::open(&path).unwrap();
        connection
            .execute_batch(
                "PRAGMA foreign_keys = ON;
                 CREATE TABLE schema_migration (
                     version INTEGER PRIMARY KEY,
                     applied_at_ms INTEGER NOT NULL
                 );",
            )
            .unwrap();
        migrate_to_v1(&mut connection).unwrap();
        migrate_to_v2(&mut connection).unwrap();
        migrate_to_v3(&mut connection).unwrap();
        migrate_to_v4(&mut connection).unwrap();
        migrate_to_v5(&mut connection).unwrap();
        migrate_to_v6(&mut connection).unwrap();
        migrate_to_v7(&mut connection).unwrap();
        migrate_to_v8(&mut connection).unwrap();
        migrate_to_v9(&mut connection).unwrap();
        migrate_to_v10(&mut connection).unwrap();
        migrate_to_v11(&mut connection).unwrap();
        migrate_to_v12(&mut connection).unwrap();
        migrate_to_v13(&mut connection).unwrap();
        migrate_to_v14(&mut connection).unwrap();
        connection
            .execute(
                "INSERT INTO song(
                     video_id, title, artists_json, duration_ms, thumbnail_url,
                     created_at_ms, updated_at_ms
                 ) VALUES ('legacy-song', 'Legacy song', '[]', NULL, NULL, 0, 0)",
                [],
            )
            .unwrap();
        drop(connection);

        let store = DesktopStore::open(&path).unwrap();
        let mut episode = song("new-episode");
        episode.is_episode = true;
        futures::executor::block_on(store.set_favorite(episode, true)).unwrap();
        let favorites = futures::executor::block_on(store.favorites(10)).unwrap();
        assert_eq!(favorites.len(), 1);
        assert!(favorites[0].song.is_episode);
        drop(store);

        let connection = Connection::open(&path).unwrap();
        let version: i64 = connection
            .query_row("SELECT MAX(version) FROM schema_migration", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(version, SCHEMA_VERSION);
        let legacy_is_episode: bool = connection
            .query_row(
                "SELECT is_episode FROM song WHERE video_id = 'legacy-song'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(!legacy_is_episode);
        drop(connection);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn v15_database_upgrades_to_parametric_equalizer_profiles_without_activation() {
        let path = temporary_database("v15-parametric-equalizer-upgrade");
        let mut connection = Connection::open(&path).unwrap();
        connection
            .execute_batch(
                "PRAGMA foreign_keys = ON;
                 CREATE TABLE schema_migration (
                     version INTEGER PRIMARY KEY,
                     applied_at_ms INTEGER NOT NULL
                 );",
            )
            .unwrap();
        migrate_to_v1(&mut connection).unwrap();
        migrate_to_v2(&mut connection).unwrap();
        migrate_to_v3(&mut connection).unwrap();
        migrate_to_v4(&mut connection).unwrap();
        migrate_to_v5(&mut connection).unwrap();
        migrate_to_v6(&mut connection).unwrap();
        migrate_to_v7(&mut connection).unwrap();
        migrate_to_v8(&mut connection).unwrap();
        migrate_to_v9(&mut connection).unwrap();
        migrate_to_v10(&mut connection).unwrap();
        migrate_to_v11(&mut connection).unwrap();
        migrate_to_v12(&mut connection).unwrap();
        migrate_to_v13(&mut connection).unwrap();
        migrate_to_v14(&mut connection).unwrap();
        migrate_to_v15(&mut connection).unwrap();
        drop(connection);

        let store = DesktopStore::open(&path).unwrap();
        assert!(
            futures::executor::block_on(store.equalizer_profiles())
                .unwrap()
                .is_empty()
        );
        drop(store);

        let connection = Connection::open(&path).unwrap();
        let version: i64 = connection
            .query_row("SELECT MAX(version) FROM schema_migration", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(version, SCHEMA_VERSION);
        let active_column: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_info('app_settings')
                 WHERE name = 'equalizer_active_profile_json'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(active_column, 1);
        drop(connection);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn history_round_trips_song_metadata() {
        let path = temporary_database("history");
        let store = DesktopStore::open(&path).unwrap();
        futures::executor::block_on(store.record_history(song("one"), Duration::from_secs(42)))
            .unwrap();

        let history = futures::executor::block_on(store.recent_history(10)).unwrap();
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].song, song("one"));
        assert_eq!(history[0].play_time, Duration::from_secs(42));
        drop(store);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn lyrics_cache_round_trips_offline_and_replaces_timeline_shape() {
        let path = temporary_database("lyrics-cache");
        let track = song("lyrics");
        let synced = LyricsDocument::synced(
            "Fixture provider",
            vec![
                LyricsLine {
                    start: Some(Duration::from_millis(1_000)),
                    text: "First fixture line".into(),
                },
                LyricsLine {
                    start: Some(Duration::from_millis(2_500)),
                    text: "Second fixture line".into(),
                },
            ],
        )
        .unwrap();
        let store = DesktopStore::open(&path).unwrap();
        futures::executor::block_on(store.save_lyrics(track.clone(), synced.clone())).unwrap();
        drop(store);

        let reopened = DesktopStore::open(&path).unwrap();
        assert_eq!(
            futures::executor::block_on(reopened.load_lyrics(track.video_id.clone())).unwrap(),
            Some(synced)
        );
        let plain = LyricsDocument::plain("Fallback provider", "One\nTwo").unwrap();
        futures::executor::block_on(reopened.save_lyrics(track.clone(), plain.clone())).unwrap();
        assert_eq!(
            futures::executor::block_on(reopened.load_lyrics(track.video_id)).unwrap(),
            Some(plain)
        );
        assert!(
            futures::executor::block_on(reopened.load_lyrics("missing".into()))
                .unwrap()
                .is_none()
        );
        drop(reopened);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn lyrics_cache_rejects_mixed_or_empty_documents() {
        let path = temporary_database("invalid-lyrics-cache");
        let store = DesktopStore::open(&path).unwrap();
        let mixed = LyricsDocument {
            provider: "Fixture".into(),
            lines: vec![
                LyricsLine {
                    start: Some(Duration::from_secs(1)),
                    text: "Timed".into(),
                },
                LyricsLine {
                    start: None,
                    text: "Untimed".into(),
                },
            ],
        };

        assert!(futures::executor::block_on(store.save_lyrics(song("mixed"), mixed)).is_err());
        assert!(
            futures::executor::block_on(store.save_lyrics(
                song("empty"),
                LyricsDocument {
                    provider: "Fixture".into(),
                    lines: Vec::new(),
                },
            ))
            .is_err()
        );
        drop(store);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn settings_round_trip_across_reopen_and_replace_the_singleton() {
        let path = temporary_database("settings");
        let first = AppSettings {
            proxy: ProxySettings {
                enabled: true,
                kind: ProxyKind::Socks5,
                address: "127.0.0.1:1080".into(),
                username: "listener".into(),
                password: "fixture-secret".into(),
            },
            audio_quality: AudioQuality::Low,
            audio_normalization: false,
            loudness_level: LoudnessLevel::Quiet,
            equalizer: EqualizerSettings {
                enabled: true,
                gains_mb: [600, 450, 300, 100, 0, -100, -150, -150, -100, 0],
                active_profile: None,
            },
            playback_parameters: PlaybackParameters {
                varispeed: true,
                tempo_milli: 1_250,
                transpose_semitones: 0,
            },
            cache_root: std::env::temp_dir().join("metrolist-settings-cache-one"),
            audio_cache_bytes: 256 * 1024 * 1024,
            auto_radio: false,
            youtube_history_sync: false,
            lastfm_scrobbling: true,
            lastfm_now_playing: true,
            lastfm_sync_likes: true,
            lastfm_scrobble_policy: crate::services::LastFmScrobblePolicy {
                min_track_seconds: 45,
                delay_percent_milli: 650,
                max_delay_seconds: 240,
            },
            discord_rich_presence: true,
            listen_together: ListenTogetherSettings {
                server_url: "wss://example.test/ws".into(),
                username: "listener".into(),
                auto_approve_joins: true,
                auto_approve_suggestions: true,
                sync_host_volume: false,
            },
            theme: AppTheme::Light,
        };
        let store = DesktopStore::open(&path).unwrap();
        assert!(
            futures::executor::block_on(store.load_settings())
                .unwrap()
                .is_none()
        );
        futures::executor::block_on(store.save_settings(first.clone())).unwrap();
        drop(store);

        let reopened = DesktopStore::open(&path).unwrap();
        assert_eq!(
            futures::executor::block_on(reopened.load_settings()).unwrap(),
            Some(first)
        );
        let replacement = AppSettings {
            proxy: ProxySettings::default(),
            audio_quality: AudioQuality::High,
            audio_normalization: true,
            loudness_level: LoudnessLevel::Loud,
            equalizer: EqualizerSettings {
                enabled: true,
                gains_mb: [-150, -150, -100, 0, 100, 200, 350, 500, 600, 500],
                active_profile: None,
            },
            playback_parameters: PlaybackParameters {
                varispeed: false,
                tempo_milli: 850,
                transpose_semitones: -3,
            },
            cache_root: std::env::temp_dir().join("metrolist-settings-cache-two"),
            audio_cache_bytes: 1024 * 1024 * 1024,
            auto_radio: true,
            youtube_history_sync: true,
            lastfm_scrobbling: false,
            lastfm_now_playing: false,
            lastfm_sync_likes: false,
            lastfm_scrobble_policy: crate::services::LastFmScrobblePolicy::default(),
            discord_rich_presence: false,
            listen_together: ListenTogetherSettings::default(),
            theme: AppTheme::Dark,
        };
        futures::executor::block_on(reopened.save_settings(replacement.clone())).unwrap();
        assert_eq!(
            futures::executor::block_on(reopened.load_settings()).unwrap(),
            Some(replacement)
        );
        drop(reopened);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn equalizer_profiles_and_active_snapshot_round_trip_and_guard_deletion() {
        let path = temporary_database("equalizer-profiles");
        let store = DesktopStore::open(&path).unwrap();
        let profile = equalizer_profile("custom-fixture");
        futures::executor::block_on(store.save_equalizer_profile(profile.clone())).unwrap();
        assert_eq!(
            futures::executor::block_on(store.equalizer_profiles()).unwrap(),
            vec![profile.clone()]
        );

        let mut settings = AppSettings::for_current_user(512 * 1024 * 1024).unwrap();
        settings.equalizer.enabled = true;
        settings.equalizer.active_profile = Some(profile.clone());
        futures::executor::block_on(store.save_settings(settings.clone())).unwrap();
        assert!(
            futures::executor::block_on(store.delete_equalizer_profile(profile.id.clone()))
                .is_err()
        );
        drop(store);

        let reopened = DesktopStore::open(&path).unwrap();
        assert_eq!(
            futures::executor::block_on(reopened.load_settings()).unwrap(),
            Some(settings.clone())
        );
        settings.equalizer.enabled = false;
        settings.equalizer.active_profile = None;
        futures::executor::block_on(reopened.save_settings(settings)).unwrap();
        futures::executor::block_on(reopened.delete_equalizer_profile(profile.id)).unwrap();
        assert!(
            futures::executor::block_on(reopened.equalizer_profiles())
                .unwrap()
                .is_empty()
        );
        drop(reopened);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn equalizer_profile_batch_is_atomic_and_persists_every_valid_profile() {
        let path = temporary_database("equalizer-profile-batch");
        let store = DesktopStore::open(&path).unwrap();
        let first = equalizer_profile("autoeq-first");
        let mut second = equalizer_profile("autoeq-second");
        second.added_at_ms += 1;
        futures::executor::block_on(
            store.save_equalizer_profiles(vec![first.clone(), second.clone()]),
        )
        .unwrap();
        assert_eq!(
            futures::executor::block_on(store.equalizer_profiles()).unwrap(),
            vec![second, first]
        );

        let third = equalizer_profile("autoeq-third");
        let mut invalid = equalizer_profile("autoeq-invalid");
        invalid.id.clear();
        assert!(
            futures::executor::block_on(
                store.save_equalizer_profiles(vec![third.clone(), invalid])
            )
            .is_err()
        );
        assert!(
            futures::executor::block_on(store.equalizer_profiles())
                .unwrap()
                .iter()
                .all(|profile| profile.id != third.id)
        );
        drop(store);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn settings_reject_relative_cache_locations_without_replacing_valid_data() {
        let path = temporary_database("invalid-settings");
        let store = DesktopStore::open(&path).unwrap();
        let valid = AppSettings {
            proxy: ProxySettings::default(),
            audio_quality: AudioQuality::Auto,
            audio_normalization: true,
            loudness_level: LoudnessLevel::Balanced,
            equalizer: EqualizerSettings::default(),
            playback_parameters: PlaybackParameters::default(),
            cache_root: std::env::temp_dir().join("metrolist-valid-cache"),
            audio_cache_bytes: 512 * 1024 * 1024,
            auto_radio: true,
            youtube_history_sync: true,
            lastfm_scrobbling: false,
            lastfm_now_playing: false,
            lastfm_sync_likes: false,
            lastfm_scrobble_policy: crate::services::LastFmScrobblePolicy::default(),
            discord_rich_presence: false,
            listen_together: ListenTogetherSettings::default(),
            theme: AppTheme::Dark,
        };
        futures::executor::block_on(store.save_settings(valid.clone())).unwrap();
        let mut invalid = valid.clone();
        invalid.cache_root = "relative/cache".into();
        assert!(futures::executor::block_on(store.save_settings(invalid)).is_err());
        let mut invalid_equalizer = valid.clone();
        invalid_equalizer.equalizer.gains_mb[0] = 1_201;
        assert!(futures::executor::block_on(store.save_settings(invalid_equalizer)).is_err());
        let mut invalid_playback = valid.clone();
        invalid_playback.playback_parameters.tempo_milli = 1_025;
        assert!(futures::executor::block_on(store.save_settings(invalid_playback)).is_err());
        let mut invalid_lastfm = valid.clone();
        invalid_lastfm.lastfm_scrobble_policy.delay_percent_milli = 299;
        assert!(futures::executor::block_on(store.save_settings(invalid_lastfm)).is_err());
        assert_eq!(
            futures::executor::block_on(store.load_settings()).unwrap(),
            Some(valid)
        );
        drop(store);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn clearing_history_preserves_other_library_data() {
        let path = temporary_database("clear-history");
        let store = DesktopStore::open(&path).unwrap();
        let kept_song = song("kept");
        futures::executor::block_on(
            store.record_history(kept_song.clone(), Duration::from_secs(42)),
        )
        .unwrap();
        futures::executor::block_on(store.set_favorite(kept_song.clone(), true)).unwrap();
        let playlist =
            futures::executor::block_on(store.create_playlist("Kept playlist".into())).unwrap();
        futures::executor::block_on(store.add_to_playlist(playlist.id, kept_song.clone())).unwrap();

        futures::executor::block_on(store.clear_history()).unwrap();

        assert!(
            futures::executor::block_on(store.recent_history(10))
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            futures::executor::block_on(store.favorites(10)).unwrap()[0].song,
            kept_song
        );
        assert_eq!(
            futures::executor::block_on(store.playlist_songs(playlist.id)).unwrap(),
            vec![song("kept")]
        );
        drop(store);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn playback_session_round_trips_queue_order_and_position() {
        let path = temporary_database("session");
        let store = DesktopStore::open(&path).unwrap();
        let session = PersistedSession {
            queue: vec![song("one"), song("two")],
            current_index: Some(1),
            position: Duration::from_millis(12_345),
            volume: 0.65,
            repeat_mode: RepeatMode::One,
            shuffle_enabled: true,
            playback_source: Some(PersistedPlaybackSource {
                video_id: "two".into(),
                mime_type: "audio/mp4; codecs=mp4a.40.2".into(),
                content_length: 4_200_000,
                loudness_lufs_mb: Some(-1_432),
                resolved_at_ms: 1_700_000_000_000,
                expires_at_ms: 1_700_021_540_000,
            }),
        };
        futures::executor::block_on(store.save_session(session.clone())).unwrap();

        let loaded = futures::executor::block_on(store.load_session())
            .unwrap()
            .unwrap();
        assert_eq!(loaded, session);
        drop(store);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn empty_playback_session_replaces_the_previous_queue_and_source() {
        let path = temporary_database("empty-session");
        let store = DesktopStore::open(&path).unwrap();
        let populated = PersistedSession {
            queue: vec![song("one")],
            current_index: Some(0),
            position: Duration::from_secs(12),
            volume: 0.6,
            repeat_mode: RepeatMode::One,
            shuffle_enabled: true,
            playback_source: Some(PersistedPlaybackSource {
                video_id: "one".into(),
                mime_type: "audio/mp4".into(),
                content_length: 42,
                loudness_lufs_mb: None,
                resolved_at_ms: 100,
                expires_at_ms: 200,
            }),
        };
        futures::executor::block_on(store.save_session(populated)).unwrap();
        let empty = PersistedSession {
            queue: Vec::new(),
            current_index: None,
            position: Duration::ZERO,
            volume: 0.6,
            repeat_mode: RepeatMode::All,
            shuffle_enabled: false,
            playback_source: None,
        };

        futures::executor::block_on(store.save_session(empty.clone())).unwrap();

        assert_eq!(
            futures::executor::block_on(store.load_session()).unwrap(),
            Some(empty)
        );
        drop(store);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn playback_session_rejects_an_invalid_current_index() {
        let path = temporary_database("invalid-session");
        let store = DesktopStore::open(&path).unwrap();
        let session = PersistedSession {
            queue: vec![song("one")],
            current_index: Some(2),
            position: Duration::ZERO,
            volume: 0.8,
            repeat_mode: RepeatMode::Off,
            shuffle_enabled: false,
            playback_source: None,
        };

        let error = futures::executor::block_on(store.save_session(session)).unwrap_err();
        assert!(matches!(error, AppError::Storage(_)));
        drop(store);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn playback_source_metadata_rejects_a_different_current_song() {
        let path = temporary_database("source-mismatch");
        let store = DesktopStore::open(&path).unwrap();
        let session = PersistedSession {
            queue: vec![song("one"), song("two")],
            current_index: Some(0),
            position: Duration::ZERO,
            volume: 0.8,
            repeat_mode: RepeatMode::Off,
            shuffle_enabled: false,
            playback_source: Some(PersistedPlaybackSource {
                video_id: "two".into(),
                mime_type: "audio/mp4".into(),
                content_length: 42,
                loudness_lufs_mb: None,
                resolved_at_ms: 100,
                expires_at_ms: 200,
            }),
        };

        let error = futures::executor::block_on(store.save_session(session)).unwrap_err();
        assert!(matches!(error, AppError::Storage(_)));
        drop(store);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn playback_source_schema_cannot_store_urls_or_request_headers() {
        let path = temporary_database("safe-source-schema");
        let store = DesktopStore::open(&path).unwrap();
        drop(store);

        let connection = Connection::open(&path).unwrap();
        let mut statement = connection
            .prepare("PRAGMA table_info(playback_source_session)")
            .unwrap();
        let columns = statement
            .query_map([], |row| row.get::<_, String>(1))
            .unwrap()
            .collect::<std::result::Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(
            columns,
            [
                "singleton",
                "video_id",
                "mime_type",
                "content_length",
                "resolved_at_ms",
                "expires_at_ms",
                "loudness_lufs_mb",
            ]
        );
        assert!(
            columns
                .iter()
                .all(|column| !column.contains("url") && !column.contains("header"))
        );
        drop(statement);
        drop(connection);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn favorites_are_idempotent_and_can_be_removed() {
        let path = temporary_database("favorites");
        let store = DesktopStore::open(&path).unwrap();
        futures::executor::block_on(store.set_favorite(song("one"), true)).unwrap();
        futures::executor::block_on(store.set_favorite(song("one"), true)).unwrap();

        let favorites = futures::executor::block_on(store.favorites(10)).unwrap();
        assert_eq!(favorites.len(), 1);
        assert_eq!(favorites[0].song, song("one"));

        futures::executor::block_on(store.set_favorite(song("one"), false)).unwrap();
        assert!(
            futures::executor::block_on(store.favorites(10))
                .unwrap()
                .is_empty()
        );
        drop(store);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn local_playlist_round_trips_order_and_mutations() {
        let path = temporary_database("playlists");
        let store = DesktopStore::open(&path).unwrap();
        let playlist =
            futures::executor::block_on(store.create_playlist("  Road trip  ".into())).unwrap();
        assert_eq!(playlist.name, "Road trip");

        futures::executor::block_on(store.add_to_playlist(playlist.id, song("one"))).unwrap();
        futures::executor::block_on(store.add_to_playlist(playlist.id, song("two"))).unwrap();
        futures::executor::block_on(store.add_to_playlist(playlist.id, song("one"))).unwrap();
        assert_eq!(
            futures::executor::block_on(store.playlist_songs(playlist.id)).unwrap(),
            vec![song("one"), song("two")]
        );

        let playlists = futures::executor::block_on(store.playlists()).unwrap();
        assert_eq!(playlists.len(), 1);
        assert_eq!(playlists[0].song_count, 2);

        futures::executor::block_on(store.remove_from_playlist(playlist.id, "one".into())).unwrap();
        assert_eq!(
            futures::executor::block_on(store.playlist_songs(playlist.id)).unwrap(),
            vec![song("two")]
        );

        futures::executor::block_on(store.delete_playlist(playlist.id)).unwrap();
        assert!(
            futures::executor::block_on(store.playlists())
                .unwrap()
                .is_empty()
        );
        drop(store);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn local_playlist_names_are_nonempty_and_case_insensitively_unique() {
        let path = temporary_database("playlist-names");
        let store = DesktopStore::open(&path).unwrap();
        assert!(futures::executor::block_on(store.create_playlist("  ".into())).is_err());
        futures::executor::block_on(store.create_playlist("Mix".into())).unwrap();
        assert!(futures::executor::block_on(store.create_playlist("mix".into())).is_err());
        drop(store);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn local_playlist_can_be_renamed_without_losing_songs() {
        let path = temporary_database("playlist-rename");
        let store = DesktopStore::open(&path).unwrap();
        let playlist =
            futures::executor::block_on(store.create_playlist("Old name".into())).unwrap();
        futures::executor::block_on(store.add_to_playlist(playlist.id, song("one"))).unwrap();
        futures::executor::block_on(store.create_playlist("Already used".into())).unwrap();

        let renamed =
            futures::executor::block_on(store.rename_playlist(playlist.id, "  New name  ".into()))
                .unwrap();
        assert_eq!(renamed.name, "New name");
        assert_eq!(renamed.song_count, 1);
        assert_eq!(
            futures::executor::block_on(store.playlist_songs(playlist.id)).unwrap(),
            vec![song("one")]
        );
        assert!(
            futures::executor::block_on(store.rename_playlist(playlist.id, "  ".into())).is_err()
        );
        let duplicate_error =
            futures::executor::block_on(store.rename_playlist(playlist.id, "already USED".into()))
                .unwrap_err()
                .to_string();
        assert!(duplicate_error.contains("already exists"));
        assert!(
            futures::executor::block_on(store.rename_playlist(99_999, "Missing".into())).is_err()
        );
        drop(store);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn local_playlists_support_all_sort_modes_and_directions() {
        let path = temporary_database("playlist-sorting");
        let mut connection = open_and_migrate(&path).unwrap();
        let zulu = create_playlist(&mut connection, "Zulu").unwrap();
        let alpha = create_playlist(&mut connection, "alpha").unwrap();
        let _middle = create_playlist(&mut connection, "Middle").unwrap();
        add_to_playlist(&mut connection, zulu.id, &song("zulu-one")).unwrap();
        add_to_playlist(&mut connection, alpha.id, &song("alpha-one")).unwrap();
        add_to_playlist(&mut connection, alpha.id, &song("alpha-two")).unwrap();
        connection
            .execute(
                "UPDATE local_playlist
                 SET created_at_ms = CASE id WHEN ?1 THEN 100 WHEN ?2 THEN 300 ELSE 200 END,
                     updated_at_ms = CASE id WHEN ?1 THEN 300 WHEN ?2 THEN 100 ELSE 200 END",
                params![zulu.id, alpha.id],
            )
            .unwrap();

        let names = |sort, direction| {
            playlists(&connection, sort, direction)
                .unwrap()
                .into_iter()
                .map(|playlist| playlist.name)
                .collect::<Vec<_>>()
        };
        assert_eq!(
            names(PlaylistSort::CreatedAt, SortDirection::Ascending),
            ["Zulu", "Middle", "alpha"]
        );
        assert_eq!(
            names(PlaylistSort::CreatedAt, SortDirection::Descending),
            ["alpha", "Middle", "Zulu"]
        );
        assert_eq!(
            names(PlaylistSort::Name, SortDirection::Ascending),
            ["alpha", "Middle", "Zulu"]
        );
        assert_eq!(
            names(PlaylistSort::Name, SortDirection::Descending),
            ["Zulu", "Middle", "alpha"]
        );
        assert_eq!(
            names(PlaylistSort::SongCount, SortDirection::Ascending),
            ["Middle", "Zulu", "alpha"]
        );
        assert_eq!(
            names(PlaylistSort::SongCount, SortDirection::Descending),
            ["alpha", "Zulu", "Middle"]
        );
        assert_eq!(
            names(PlaylistSort::UpdatedAt, SortDirection::Ascending),
            ["alpha", "Middle", "Zulu"]
        );
        assert_eq!(
            names(PlaylistSort::UpdatedAt, SortDirection::Descending),
            ["Zulu", "Middle", "alpha"]
        );
        drop(connection);
        let _ = std::fs::remove_file(path);
    }
}
