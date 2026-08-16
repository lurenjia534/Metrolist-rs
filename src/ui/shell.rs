use gpui::prelude::FluentBuilder as _;
use gpui::*;
use gpui_component::{
    ActiveTheme, Disableable, Icon, IconName, Selectable, StyledExt, Theme, ThemeMode, WindowExt,
    button::{Button, ButtonVariant, ButtonVariants},
    dialog::DialogButtonProps,
    h_flex,
    input::{Input, InputContentType, InputEvent, InputState},
    scroll::ScrollableElement,
    slider::{Slider, SliderEvent, SliderState},
    tab::{Tab, TabBar},
    v_flex,
};
use http_client::Url;

use std::{
    collections::{HashMap, HashSet, VecDeque},
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use zeroize::Zeroizing;

use crate::domain::{
    BrowseItem, BrowseKind, BrowsePage, ExplorePage, HomeChip, HomeItem, HomePage, HomeSection,
    LyricsDocument, PlaylistEntry, RemoteHistoryPage, Song,
};
use crate::services::innertube::{
    AccountProfile, InnerTubeClient, InnerTubeSession, PlaybackTrackingUrl, RadioEndpoint,
    RadioPage, ResolvedPlayback, SearchFilter, SearchResult, SearchSuggestions,
};
use crate::services::{
    AudioCache, AudioDeviceOperation, AudioPlayer, AuthSession, AutoEqClient, AutoEqEntry,
    AutoEqIndex, AutoEqIndexOrigin, AutoEqModel, CredentialStore, DesktopAudioPlayer,
    DesktopMediaCommand, DesktopMediaSession, DesktopSeekDirection, DesktopServices,
    DiscordPlaybackObservation, DiscordPlaybackState, DiscordPresenceService, DiscordPresenceState,
    DiscordPresenceTracker, DownloadOutcome, DownloadUpdate, DownloadedAudioStore,
    EQUALIZER_RESPONSE_MAX_FREQUENCY_HZ, EQUALIZER_RESPONSE_MIN_FREQUENCY_HZ,
    EqualizerFrequencyResponse, LastFmApiCredentials, LastFmClient, LastFmCredentialStore,
    LastFmPlaybackAction, LastFmPlaybackObservation, LastFmPlaybackTracker, LastFmSession,
    LastFmTrack, ListenTogetherClient, ListenTogetherConnectionState, ListenTogetherEvent,
    ListenTogetherLocalPlaybackState, ListenTogetherPlaybackAction,
    ListenTogetherPlaybackActionPayload, ListenTogetherPlaybackObservation,
    ListenTogetherPlaybackTracker, ListenTogetherRoomRole, ListenTogetherRoomState,
    ListenTogetherSnapshot, ListenTogetherTrack, LyricsClient, MAX_AUTO_EQ_SEARCH_RESULTS,
    MAX_EQUALIZER_APO_FILE_BYTES, MicrophoneCancellation, MicrophoneRecorder, PlaybackSource,
    PlaybackSourceAccess, PlaybackState, Queue, QueueItem, RECOGNITION_CAPTURE_DURATION,
    RECOGNITION_SAMPLE_RATE, RecognitionClient, RecognitionResult, RepeatMode, ThumbnailCache,
    download_song, equalizer_frequency_response, generate_shazam_signature, launch_account_login,
    linear_resample_mono_i16, parse_equalizer_apo, search_auto_eq_models,
};
use crate::storage::{
    AudioDownload, DesktopStore, DownloadState, FavoriteEntry, HistoryEntry, ListeningStats,
    LocalPlaylist, PersistedPlaybackSource, PersistedSession, PlaylistSort, PodcastSubscription,
    RecognitionHistoryEntry, SavedEpisode, SearchHistoryEntry, SongListeningStats, SortDirection,
};
use crate::{
    AppError, AppModel, AppSettings, AppTheme, AudioQuality, EQUALIZER_FREQUENCIES_HZ,
    EqualizerPreset, EqualizerProfile, EqualizerSettings, LoudnessLevel, MAX_EQUALIZER_GAIN_MB,
    MAX_PLAYBACK_RATE_MILLI, MAX_TRANSPOSE_SEMITONES, MIN_EQUALIZER_GAIN_MB,
    MIN_PLAYBACK_RATE_MILLI, MIN_TRANSPOSE_SEMITONES, PLAYBACK_RATE_STEP_MILLI, PlaybackParameters,
    ProxyKind, Route,
};

const HISTORY_THRESHOLD: Duration = Duration::from_secs(30);
const SESSION_SAVE_INTERVAL: Duration = Duration::from_secs(5);
const EPISODE_POSITION_THRESHOLD: Duration = Duration::from_secs(3);
const EPISODE_POSITION_SAVE_INTERVAL: Duration = Duration::from_secs(15);
const MAX_MEMORY_THUMBNAILS: usize = 256;
const RADIO_PREFETCH_THRESHOLD: usize = 5;
const MAX_PARALLEL_DOWNLOADS: usize = 3;

enum SearchViewState {
    Idle,
    Loading,
    Loaded(SearchResult),
    Empty,
    Failed(String),
}

enum SearchSuggestionViewState {
    Hidden,
    Loading,
    Loaded(SearchSuggestions),
    Failed(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum SearchSource {
    #[default]
    Online,
    Local,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum StatsPeriod {
    #[default]
    Week,
    Month,
    ThreeMonths,
    SixMonths,
    Year,
    AllTime,
}

impl StatsPeriod {
    const ALL: [Self; 6] = [
        Self::Week,
        Self::Month,
        Self::ThreeMonths,
        Self::SixMonths,
        Self::Year,
        Self::AllTime,
    ];

    const fn label(self) -> &'static str {
        match self {
            Self::Week => "7 days",
            Self::Month => "30 days",
            Self::ThreeMonths => "3 months",
            Self::SixMonths => "6 months",
            Self::Year => "1 year",
            Self::AllTime => "All time",
        }
    }

    fn start_ms(self) -> i64 {
        let days = match self {
            Self::Week => 7,
            Self::Month => 30,
            Self::ThreeMonths => 90,
            Self::SixMonths => 180,
            Self::Year => 365,
            Self::AllTime => return 0,
        };
        unix_time_ms().saturating_sub(days * 24 * 60 * 60 * 1_000)
    }
}

enum ParsedYouTubeUrl {
    Video(String),
    Playlist(String),
    Album(String),
    Artist(String),
}

fn parse_youtube_url(value: &str) -> Option<ParsedYouTubeUrl> {
    let value = value.trim();
    if value.is_empty() {
        return None;
    }
    let candidate = if value.starts_with("http://") || value.starts_with("https://") {
        value.to_owned()
    } else {
        format!("https://{value}")
    };
    let url = Url::parse(&candidate).ok()?;
    let host = url.host_str()?.to_ascii_lowercase();
    let music_host = matches!(host.as_str(), "music.youtube.com" | "www.music.youtube.com");
    let youtube_host = music_host || matches!(host.as_str(), "youtube.com" | "www.youtube.com");
    let short_host = host == "youtu.be";
    if !youtube_host && !short_host {
        return None;
    }

    let segments = url
        .path_segments()
        .map(|segments| {
            segments
                .filter(|segment| !segment.is_empty())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let query_value = |name: &str| {
        url.query_pairs()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.into_owned())
    };

    if short_host {
        return segments
            .first()
            .filter(|id| valid_youtube_video_id(id))
            .map(|id| ParsedYouTubeUrl::Video((*id).to_owned()));
    }
    if segments.first() == Some(&"watch")
        && let Some(video_id) = query_value("v").filter(|id| valid_youtube_video_id(id))
    {
        return Some(ParsedYouTubeUrl::Video(video_id));
    }
    if segments.first() == Some(&"shorts")
        && let Some(video_id) = segments.get(1).filter(|id| valid_youtube_video_id(id))
    {
        return Some(ParsedYouTubeUrl::Video((*video_id).to_owned()));
    }
    if segments.first() == Some(&"playlist")
        && let Some(playlist_id) = query_value("list").filter(|id| valid_youtube_id(id))
    {
        return Some(if music_host {
            ParsedYouTubeUrl::Album(playlist_id)
        } else {
            ParsedYouTubeUrl::Playlist(playlist_id)
        });
    }
    if music_host
        && segments.first() == Some(&"channel")
        && let Some(artist_id) = segments.get(1).filter(|id| valid_youtube_id(id))
    {
        return Some(ParsedYouTubeUrl::Artist((*artist_id).to_owned()));
    }
    if music_host
        && segments.first() == Some(&"browse")
        && let Some(artist_id) = segments
            .get(1)
            .filter(|id| id.starts_with("MPRE") && valid_youtube_id(id))
    {
        return Some(ParsedYouTubeUrl::Artist((*artist_id).to_owned()));
    }
    None
}

fn valid_youtube_video_id(value: &str) -> bool {
    value.len() == 11 && valid_youtube_id(value)
}

fn valid_youtube_id(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
}

fn home_catalog_items(page: &HomePage) -> Vec<BrowseItem> {
    let mut items = Vec::new();
    for section in &page.sections {
        if let Some(item) = &section.more {
            items.push(item.clone());
        }
        for item in &section.items {
            if let HomeItem::Browse(item) = item {
                items.push(item.clone());
            }
        }
    }
    items
}

fn explore_catalog_items(page: &ExplorePage) -> Vec<BrowseItem> {
    let mut items = page.new_release_albums.clone();
    if let Some(item) = &page.new_releases_more {
        items.push(item.clone());
    }
    for section in &page.chart_sections {
        if let Some(item) = &section.more {
            items.push(item.clone());
        }
        for item in &section.items {
            if let HomeItem::Browse(item) = item {
                items.push(item.clone());
            }
        }
    }
    items
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum LocalSearchFilter {
    #[default]
    All,
    Songs,
    Albums,
    Artists,
    Playlists,
}

impl LocalSearchFilter {
    const ALL: [Self; 5] = [
        Self::All,
        Self::Songs,
        Self::Albums,
        Self::Artists,
        Self::Playlists,
    ];

    const fn label(self) -> &'static str {
        match self {
            Self::All => "All",
            Self::Songs => "Songs",
            Self::Albums => "Albums",
            Self::Artists => "Artists",
            Self::Playlists => "Playlists",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum QueueEndAction {
    Stop,
    Advance,
    Wrap,
    Replay,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SleepTimer {
    Deadline(Instant),
    EndOfSong,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum NowPlayingTab {
    #[default]
    UpNext,
    Lyrics,
    Related,
}

impl NowPlayingTab {
    fn index(self) -> usize {
        match self {
            Self::UpNext => 0,
            Self::Lyrics => 1,
            Self::Related => 2,
        }
    }

    fn from_index(index: usize) -> Self {
        match index {
            1 => Self::Lyrics,
            2 => Self::Related,
            _ => Self::UpNext,
        }
    }
}

impl SleepTimer {
    fn deadline_reached(self, now: Instant) -> bool {
        matches!(self, Self::Deadline(deadline) if now >= deadline)
    }

    fn stops_after_song(self) -> bool {
        self == Self::EndOfSong
    }

    fn summary(self, now: Instant) -> String {
        match self {
            Self::Deadline(deadline) => format!(
                "Stops in {}",
                format_duration(deadline.saturating_duration_since(now))
            ),
            Self::EndOfSong => "Stops when this song ends".into(),
        }
    }
}

enum BrowseViewState {
    Loading(BrowseItem),
    Loaded(BrowsePage),
    Failed(BrowseItem, String),
}

enum HistoryViewState {
    Loading,
    Loaded(Vec<HistoryEntry>),
    Failed(String),
}

enum RemoteHistoryViewState {
    SignedOut,
    Loading,
    Loaded(RemoteHistoryPage),
    Failed(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum HistorySource {
    #[default]
    Local,
    YouTubeMusic,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum LibraryTab {
    #[default]
    Overview,
    Playlists,
    Songs,
    Albums,
    Artists,
    Podcasts,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum LibrarySongSource {
    #[default]
    Liked,
    Library,
    Uploaded,
    Downloaded,
}

impl LibrarySongSource {
    const ALL: [Self; 4] = [Self::Liked, Self::Library, Self::Uploaded, Self::Downloaded];

    const fn label(self) -> &'static str {
        match self {
            Self::Liked => "Liked",
            Self::Library => "Library",
            Self::Uploaded => "Uploaded",
            Self::Downloaded => "Downloaded",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum LibrarySongSort {
    #[default]
    Recent,
    Title,
    Artist,
    PlayTime,
}

impl LibrarySongSort {
    const ALL: [Self; 4] = [Self::Recent, Self::Title, Self::Artist, Self::PlayTime];

    const fn label(self) -> &'static str {
        match self {
            Self::Recent => "Recent",
            Self::Title => "Title",
            Self::Artist => "Artist",
            Self::PlayTime => "Play time",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum LibraryAlbumSource {
    #[default]
    Liked,
    Library,
    Uploaded,
}

impl LibraryAlbumSource {
    const ALL: [Self; 3] = [Self::Liked, Self::Library, Self::Uploaded];

    const fn label(self) -> &'static str {
        match self {
            Self::Liked => "Liked",
            Self::Library => "Library",
            Self::Uploaded => "Uploaded",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum LibraryArtistSource {
    #[default]
    Liked,
    Library,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum LibraryPodcastSource {
    #[default]
    Episodes,
    Channels,
    Downloaded,
}

impl LibraryPodcastSource {
    const ALL: [Self; 3] = [Self::Episodes, Self::Channels, Self::Downloaded];

    const fn label(self) -> &'static str {
        match self {
            Self::Episodes => "Episodes",
            Self::Channels => "Channels",
            Self::Downloaded => "Downloaded",
        }
    }
}

impl LibraryArtistSource {
    const ALL: [Self; 2] = [Self::Liked, Self::Library];

    const fn label(self) -> &'static str {
        match self {
            Self::Liked => "Liked",
            Self::Library => "Library",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum LibraryCatalogSort {
    #[default]
    Recent,
    Title,
    Subtitle,
}

impl LibraryCatalogSort {
    const ALL: [Self; 3] = [Self::Recent, Self::Title, Self::Subtitle];

    const fn label(self) -> &'static str {
        match self {
            Self::Recent => "Recent",
            Self::Title => "Title",
            Self::Subtitle => "Artist / details",
        }
    }
}

impl LibraryTab {
    const ALL: [Self; 6] = [
        Self::Overview,
        Self::Playlists,
        Self::Songs,
        Self::Albums,
        Self::Artists,
        Self::Podcasts,
    ];

    const fn label(self) -> &'static str {
        match self {
            Self::Overview => "Overview",
            Self::Playlists => "Playlists",
            Self::Songs => "Songs",
            Self::Albums => "Albums",
            Self::Artists => "Artists",
            Self::Podcasts => "Podcasts",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum RemoteHistoryOperation {
    #[default]
    Idle,
    Removing,
}

enum StoredViewState<T> {
    Loading,
    Loaded(T),
    Failed(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
struct LocalLibraryRetryTargets {
    history: bool,
    favorites: bool,
    podcasts: bool,
    episodes: bool,
    playlists: bool,
    downloads: bool,
}

impl LocalLibraryRetryTargets {
    fn from_states(
        history: &HistoryViewState,
        favorites: &StoredViewState<Vec<FavoriteEntry>>,
        podcasts: &StoredViewState<Vec<PodcastSubscription>>,
        episodes: &StoredViewState<Vec<SavedEpisode>>,
        playlists: &StoredViewState<Vec<LocalPlaylist>>,
        downloads: &StoredViewState<Vec<AudioDownload>>,
    ) -> Self {
        Self {
            history: matches!(history, HistoryViewState::Failed(_)),
            favorites: matches!(favorites, StoredViewState::Failed(_)),
            podcasts: matches!(podcasts, StoredViewState::Failed(_)),
            episodes: matches!(episodes, StoredViewState::Failed(_)),
            playlists: matches!(playlists, StoredViewState::Failed(_)),
            downloads: matches!(downloads, StoredViewState::Failed(_)),
        }
    }

    fn any(self) -> bool {
        self.history
            || self.favorites
            || self.podcasts
            || self.episodes
            || self.playlists
            || self.downloads
    }
}

enum HomeFeedState {
    Loading,
    Loaded(HomePage),
    Failed(String),
}

enum ExploreFeedState {
    Loading,
    Loaded(ExplorePage),
    Failed(String),
}

enum RecognitionViewState {
    Ready,
    Listening,
    Processing,
    Matched(RecognitionResult),
    NoMatch,
    Cancelled,
    Failed(String),
}

impl RecognitionViewState {
    fn is_busy(&self) -> bool {
        matches!(self, Self::Listening | Self::Processing)
    }
}

enum LyricsViewState {
    Idle,
    Loading(String),
    Loaded(String, LyricsDocument),
    Unavailable(String),
    Failed(String, String),
}

enum PlaylistDetailState {
    Loading(LocalPlaylist),
    Loaded(LocalPlaylist, Vec<Song>),
    Failed(LocalPlaylist, String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum SettingsOperation {
    #[default]
    Idle,
    Applying,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum EqualizerOperation {
    #[default]
    Idle,
    Importing,
    LoadingDatabase,
    LoadingVariants,
    SavingAutoEq,
    Applying,
    Deleting,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum AutoEqWizardStep {
    #[default]
    ModelSelection,
    VariantSelection,
}

enum AutoEqDatabaseState {
    NotLoaded { cached: bool },
    Loading,
    Ready(Arc<AutoEqIndex>),
    Failed { message: String, cached: bool },
}

#[derive(Clone)]
enum AccountViewState {
    SignedOut,
    Checking,
    SignedIn(AccountProfile),
    Expired(String),
    Failed(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum LastFmOperation {
    #[default]
    Idle,
    SigningIn,
    SigningOut,
}

impl AccountViewState {
    fn is_verified(&self) -> bool {
        matches!(self, Self::SignedIn(_))
    }
}

fn sidebar_account_summary(
    state: &AccountViewState,
    operation: AccountOperation,
) -> (String, String) {
    match state {
        AccountViewState::SignedOut => (
            "YouTube Music".into(),
            "Anonymous search and playback".into(),
        ),
        AccountViewState::Checking => (
            "YouTube Music".into(),
            if operation == AccountOperation::SigningIn {
                "Complete sign-in in the browser window…".into()
            } else {
                "Checking saved account…".into()
            },
        ),
        AccountViewState::SignedIn(profile) => {
            let detail = [profile.email.as_deref(), profile.channel_handle.as_deref()]
                .into_iter()
                .flatten()
                .collect::<Vec<_>>()
                .join(" · ");
            (
                profile.name.clone(),
                if detail.is_empty() {
                    "Account sync is active".into()
                } else {
                    detail
                },
            )
        }
        AccountViewState::Expired(_) => (
            "Account needs attention".into(),
            "Open Settings to reconnect".into(),
        ),
        AccountViewState::Failed(_) => (
            "Account unavailable".into(),
            "Anonymous playback remains available".into(),
        ),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct CloudLibraryData {
    liked_songs: Vec<Song>,
    library_songs: Vec<Song>,
    uploaded_songs: Vec<Song>,
    playlists: Vec<BrowseItem>,
    albums: Vec<BrowseItem>,
    uploaded_albums: Vec<BrowseItem>,
    artists: Vec<BrowseItem>,
}

struct ActiveDownload {
    cancelled: Arc<AtomicBool>,
    latest_update: Arc<Mutex<Option<DownloadUpdate>>>,
}

impl CloudLibraryData {
    fn video_liked(&self, video_id: &str) -> bool {
        self.liked_songs
            .iter()
            .any(|song| song.video_id == video_id)
    }

    fn playlist_liked(&self, playlist_id: &str) -> bool {
        let playlist_id = ui_playlist_id(playlist_id);
        self.playlists
            .iter()
            .any(|item| ui_playlist_id(&item.browse_id) == playlist_id)
    }

    fn album_liked(&self, browse_id: &str) -> bool {
        self.albums.iter().any(|item| item.browse_id == browse_id)
    }

    fn artist_subscribed(&self, channel_id: &str) -> bool {
        self.artists.iter().any(|item| item.browse_id == channel_id)
    }

    fn set_video_liked(&mut self, song: Song, liked: bool) {
        self.liked_songs
            .retain(|existing| existing.video_id != song.video_id);
        if liked {
            self.liked_songs.insert(0, song);
        }
    }

    fn set_playlist_liked(&mut self, item: BrowseItem, liked: bool) {
        let playlist_id = ui_playlist_id(&item.browse_id).to_owned();
        self.playlists
            .retain(|existing| ui_playlist_id(&existing.browse_id) != playlist_id);
        if liked {
            self.playlists.insert(0, item);
        }
    }

    fn set_album_liked(&mut self, item: BrowseItem, liked: bool) {
        self.albums
            .retain(|existing| existing.browse_id != item.browse_id);
        if liked {
            self.albums.insert(0, item);
        }
    }

    fn set_artist_subscribed(&mut self, item: BrowseItem, subscribed: bool) {
        self.artists
            .retain(|existing| existing.browse_id != item.browse_id);
        if subscribed {
            self.artists.insert(0, item);
        }
    }
}

fn ui_playlist_id(value: &str) -> &str {
    value.strip_prefix("VL").unwrap_or(value)
}

enum CloudLibraryViewState {
    SignedOut,
    Loading,
    Loaded(CloudLibraryData),
    Failed(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum CloudLibraryOperation {
    #[default]
    Idle,
    SettingVideoLike,
    SettingPlaylistLike,
    SettingAlbumLike,
    SettingSubscription,
    CreatingPlaylist,
    AddingToPlaylist,
    RemovingFromPlaylist,
    RenamingPlaylist,
    DeletingPlaylist,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum AccountOperation {
    #[default]
    Idle,
    SigningIn,
    Importing,
    SigningOut,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum LibraryOperation {
    #[default]
    Idle,
    LoadingLibrary,
    RetryingLibrary,
    CreatingPlaylist,
    SortingPlaylists,
    AddingToPlaylist,
    RemovingFromPlaylist,
    RenamingPlaylist,
    DeletingPlaylist,
    ClearingHistory,
    RemovingHistory,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum PodcastOperation {
    #[default]
    Idle,
    Syncing,
    SavingPodcast,
    SavingEpisode,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum PlaybackSourceAttempt {
    #[default]
    None,
    CacheOnly,
    Network,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PlaybackParameterChange {
    NormalMode,
    VarispeedMode,
    SpeedDown,
    SpeedUp,
    TransposeDown,
    TransposeUp,
    Reset,
}

fn adjusted_playback_parameters(
    mut parameters: PlaybackParameters,
    change: PlaybackParameterChange,
) -> PlaybackParameters {
    match change {
        PlaybackParameterChange::NormalMode => {
            parameters.varispeed = false;
            parameters.transpose_semitones = 0;
        }
        PlaybackParameterChange::VarispeedMode => {
            parameters.varispeed = true;
            parameters.transpose_semitones = 0;
        }
        PlaybackParameterChange::SpeedDown => {
            parameters.tempo_milli = parameters
                .tempo_milli
                .saturating_sub(PLAYBACK_RATE_STEP_MILLI)
                .max(MIN_PLAYBACK_RATE_MILLI);
        }
        PlaybackParameterChange::SpeedUp => {
            parameters.tempo_milli = parameters
                .tempo_milli
                .saturating_add(PLAYBACK_RATE_STEP_MILLI)
                .min(MAX_PLAYBACK_RATE_MILLI);
        }
        PlaybackParameterChange::TransposeDown if !parameters.varispeed => {
            parameters.transpose_semitones = parameters
                .transpose_semitones
                .saturating_sub(1)
                .max(MIN_TRANSPOSE_SEMITONES);
        }
        PlaybackParameterChange::TransposeUp if !parameters.varispeed => {
            parameters.transpose_semitones = parameters
                .transpose_semitones
                .saturating_add(1)
                .min(MAX_TRANSPOSE_SEMITONES);
        }
        PlaybackParameterChange::TransposeDown | PlaybackParameterChange::TransposeUp => {}
        PlaybackParameterChange::Reset => parameters = PlaybackParameters::default(),
    }
    parameters
}

fn format_playback_parameters(parameters: PlaybackParameters) -> String {
    if parameters.varispeed {
        format!("{:.2}× varispeed", parameters.tempo_ratio())
    } else if parameters.transpose_semitones == 0 {
        format!("{:.2}×", parameters.tempo_ratio())
    } else {
        format!(
            "{:.2}× · {:+} st",
            parameters.tempo_ratio(),
            parameters.transpose_semitones
        )
    }
}

#[derive(Clone)]
struct ActivePlaybackSource {
    video_id: String,
    source: PlaybackSource,
    expires_at_ms: i64,
    playback_tracking: Option<PlaybackTrackingUrl>,
}

#[derive(Debug, Clone)]
struct PendingGuestSync {
    track_id: String,
    is_playing: bool,
    position_ms: i64,
    effective_at_server_time_ms: Option<i64>,
    ready_sent: bool,
    buffer_complete: bool,
}

#[derive(Debug, Clone)]
struct GuestTrackStart {
    track: ListenTogetherTrack,
    upcoming: Vec<ListenTogetherTrack>,
    is_playing: bool,
    position_ms: i64,
    effective_at_server_time_ms: Option<i64>,
    bypass_buffer: bool,
}

#[derive(Clone)]
struct RadioSession {
    seed_video_id: String,
    title: Option<String>,
    recommendations: Vec<Song>,
    endpoint: RadioEndpoint,
    continuation: Option<String>,
    seen_continuations: HashSet<String>,
}

#[derive(Clone)]
struct DailyDiscoverItem {
    seed: Song,
    recommendation: Song,
}

#[derive(Clone)]
enum RadioRequest {
    Initial {
        seed_video_id: String,
        replace_future: bool,
    },
    Continuation(Box<RadioSession>),
}

impl RadioRequest {
    fn seed_video_id(&self) -> &str {
        match self {
            Self::Initial { seed_video_id, .. } => seed_video_id,
            Self::Continuation(session) => &session.seed_video_id,
        }
    }
}

#[derive(Default)]
enum RadioQueueState {
    #[default]
    Idle,
    Loading(RadioRequest),
    Active(RadioSession),
    Exhausted(RadioSession),
    Failed(RadioRequest, String),
}

pub struct AccountBootstrap {
    credential_store: Arc<dyn CredentialStore>,
    auth_session: Option<AuthSession>,
    credential_warning: Option<String>,
}

pub struct LastFmBootstrap {
    credential_store: Arc<dyn LastFmCredentialStore>,
    api_credentials: Option<LastFmApiCredentials>,
    client: Option<LastFmClient>,
    session: Option<LastFmSession>,
    warning: Option<String>,
}

impl LastFmBootstrap {
    pub fn new(
        credential_store: Arc<dyn LastFmCredentialStore>,
        api_credentials: Option<LastFmApiCredentials>,
        client: Option<LastFmClient>,
        session: Option<LastFmSession>,
        warning: Option<String>,
    ) -> Self {
        Self {
            credential_store,
            api_credentials,
            client,
            session,
            warning,
        }
    }
}

pub struct IntegrationBootstrap {
    account: AccountBootstrap,
    lastfm: LastFmBootstrap,
    discord_presence: Option<DiscordPresenceService>,
    discord_warning: Option<String>,
    listen_together: Option<ListenTogetherClient>,
    listen_together_warning: Option<String>,
}

impl IntegrationBootstrap {
    pub fn new(
        account: AccountBootstrap,
        lastfm: LastFmBootstrap,
        discord_presence: Option<DiscordPresenceService>,
        discord_warning: Option<String>,
        listen_together: Option<ListenTogetherClient>,
        listen_together_warning: Option<String>,
    ) -> Self {
        Self {
            account,
            lastfm,
            discord_presence,
            discord_warning,
            listen_together,
            listen_together_warning,
        }
    }
}

impl AccountBootstrap {
    pub fn new(
        credential_store: Arc<dyn CredentialStore>,
        auth_session: Option<AuthSession>,
        credential_warning: Option<String>,
    ) -> Self {
        Self {
            credential_store,
            auth_session,
            credential_warning,
        }
    }
}

pub struct MetrolistShell {
    model: AppModel,
    search_input: Entity<InputState>,
    history_search_input: Entity<InputState>,
    library_search_input: Entity<InputState>,
    library_catalog_search_input: Entity<InputState>,
    library_playlist_search_input: Entity<InputState>,
    autoeq_search_input: Entity<InputState>,
    playlist_name_input: Entity<InputState>,
    playlist_rename_input: Entity<InputState>,
    proxy_address_input: Entity<InputState>,
    proxy_username_input: Entity<InputState>,
    proxy_password_input: Entity<InputState>,
    cache_root_input: Entity<InputState>,
    account_cookie_input: Entity<InputState>,
    lastfm_username_input: Entity<InputState>,
    lastfm_password_input: Entity<InputState>,
    listen_together_server_input: Entity<InputState>,
    listen_together_username_input: Entity<InputState>,
    listen_together_room_code_input: Entity<InputState>,
    search_client: Arc<InnerTubeClient>,
    lyrics_client: Arc<LyricsClient>,
    microphone_recorder: Arc<dyn MicrophoneRecorder>,
    recognition_client: Arc<RecognitionClient>,
    home_state: HomeFeedState,
    home_default_page: Option<HomePage>,
    selected_home_chip: Option<HomeChip>,
    home_loading_more: bool,
    home_load_more_error: Option<String>,
    home_seen_continuations: HashSet<String>,
    daily_discover_state: StoredViewState<Vec<DailyDiscoverItem>>,
    explore_state: ExploreFeedState,
    search_source: SearchSource,
    local_search_filter: LocalSearchFilter,
    local_catalog_state: StoredViewState<Vec<BrowseItem>>,
    local_catalog_error: Option<String>,
    search_filter: SearchFilter,
    search_state: SearchViewState,
    search_suggestion_state: SearchSuggestionViewState,
    search_suggestion_generation: u64,
    search_history_state: StoredViewState<Vec<SearchHistoryEntry>>,
    search_history_task: Option<Task<()>>,
    search_history_error: Option<String>,
    stats_period: StatsPeriod,
    stats_state: StoredViewState<ListeningStats>,
    stats_task: Option<Task<()>>,
    history_query: String,
    browse_state: Option<BrowseViewState>,
    browse_return_route: Route,
    search_loading_more: bool,
    search_load_more_error: Option<String>,
    search_seen_continuations: HashSet<String>,
    browse_loading_more: bool,
    browse_load_more_error: Option<String>,
    browse_seen_continuations: HashSet<String>,
    thumbnail_cache: Option<ThumbnailCache>,
    thumbnail_images: HashMap<String, Arc<Image>>,
    thumbnail_order: VecDeque<String>,
    thumbnail_failures: HashSet<String>,
    thumbnail_tasks: HashMap<String, Task<()>>,
    search_task: Option<Task<()>>,
    search_suggestion_task: Option<Task<()>>,
    browse_task: Option<Task<()>>,
    home_task: Option<Task<()>>,
    daily_discover_task: Option<Task<()>>,
    explore_task: Option<Task<()>>,
    recognition_state: RecognitionViewState,
    recognition_cancellation: Option<MicrophoneCancellation>,
    recognition_generation: u64,
    recognition_task: Option<Task<()>>,
    recognition_history_visible: bool,
    recognition_history_state: StoredViewState<Vec<RecognitionHistoryEntry>>,
    recognition_history_task: Option<Task<()>>,
    recognition_history_error: Option<String>,
    playback_task: Option<Task<()>>,
    lyrics_task: Option<Task<()>>,
    progress_slider: Entity<SliderState>,
    volume_slider: Entity<SliderState>,
    audio_player: DesktopAudioPlayer,
    audio_cache: Arc<AudioCache>,
    downloaded_audio: Arc<DownloadedAudioStore>,
    downloads_state: StoredViewState<Vec<AudioDownload>>,
    download_queue: VecDeque<String>,
    active_downloads: HashMap<String, ActiveDownload>,
    download_tasks: HashMap<String, Task<()>>,
    download_removals: HashSet<String>,
    download_error: Option<String>,
    desktop_media: DesktopMediaSession,
    store: DesktopStore,
    queue: Queue,
    repeat_mode: RepeatMode,
    shuffle_enabled: bool,
    sleep_timer: Option<SleepTimer>,
    radio_state: RadioQueueState,
    radio_task: Option<Task<()>>,
    queue_generation: u64,
    queue_visible: bool,
    now_playing_visible: bool,
    now_playing_tab: NowPlayingTab,
    lyrics_visible: bool,
    playback_parameters_visible: bool,
    lyrics_state: LyricsViewState,
    lyrics_active_line: Option<usize>,
    lyrics_scroll: ScrollHandle,
    playlist_picker_song: Option<Song>,
    cloud_playlist_picker_song: Option<Song>,
    settings: AppSettings,
    settings_draft: AppSettings,
    settings_operation: SettingsOperation,
    settings_error: Option<String>,
    settings_notice: Option<String>,
    settings_task: Option<Task<()>>,
    equalizer_profiles: StoredViewState<Vec<EqualizerProfile>>,
    equalizer_operation: EqualizerOperation,
    equalizer_error: Option<String>,
    equalizer_notice: Option<String>,
    equalizer_delete_confirmation: Option<String>,
    equalizer_task: Option<Task<()>>,
    autoeq_database_state: AutoEqDatabaseState,
    autoeq_wizard_step: AutoEqWizardStep,
    autoeq_models: Vec<AutoEqModel>,
    autoeq_selected_model: Option<String>,
    autoeq_variants: Vec<AutoEqEntry>,
    autoeq_selected_variant_paths: HashSet<String>,
    autoeq_search_generation: u64,
    autoeq_search_task: Option<Task<()>>,
    playback_parameters_pending: Option<PlaybackParameters>,
    playback_parameters_error: Option<String>,
    playback_parameters_notice: Option<String>,
    playback_parameters_task: Option<Task<()>>,
    credential_store: Arc<dyn CredentialStore>,
    auth_session: Option<AuthSession>,
    account_state: AccountViewState,
    account_operation: AccountOperation,
    account_error: Option<String>,
    credential_warning: Option<String>,
    account_task: Option<Task<()>>,
    lastfm_credential_store: Arc<dyn LastFmCredentialStore>,
    lastfm_api_credentials: Option<LastFmApiCredentials>,
    lastfm_client: Option<LastFmClient>,
    lastfm_session: Option<LastFmSession>,
    lastfm_operation: LastFmOperation,
    lastfm_warning: Option<String>,
    lastfm_error: Option<String>,
    lastfm_notice: Option<String>,
    lastfm_task: Option<Task<()>>,
    lastfm_playback_task: Option<Task<()>>,
    lastfm_playback_tracker: LastFmPlaybackTracker,
    discord_presence: Option<DiscordPresenceService>,
    discord_presence_tracker: DiscordPresenceTracker,
    discord_warning: Option<String>,
    listen_together: Option<ListenTogetherClient>,
    listen_together_snapshot: ListenTogetherSnapshot,
    listen_together_tracker: ListenTogetherPlaybackTracker,
    listen_together_warning: Option<String>,
    listen_together_error: Option<String>,
    listen_together_notice: Option<String>,
    listen_together_pending_sync: Option<PendingGuestSync>,
    cloud_library_state: CloudLibraryViewState,
    cloud_library_task: Option<Task<()>>,
    cloud_library_operation: CloudLibraryOperation,
    cloud_library_error: Option<String>,
    cloud_mutation_task: Option<Task<()>>,
    remote_history_state: RemoteHistoryViewState,
    remote_history_source: HistorySource,
    remote_history_operation: RemoteHistoryOperation,
    remote_history_error: Option<String>,
    remote_history_task: Option<Task<()>>,
    current_song: Option<Song>,
    resolving_playback: bool,
    seeking: bool,
    seek_preview: Option<Duration>,
    last_playback_state: PlaybackState,
    playback_retry_count: u8,
    playback_source_attempt: PlaybackSourceAttempt,
    play_after_resolution: Option<bool>,
    active_playback_source: Option<ActivePlaybackSource>,
    persisted_playback_source: Option<PersistedPlaybackSource>,
    pending_resume_position: Option<Duration>,
    played_this_track: Duration,
    history_recorded_for_current: bool,
    last_playback_poll: Instant,
    last_session_save: Instant,
    playback_error: Option<String>,
    history_state: HistoryViewState,
    keep_listening_state: StoredViewState<Vec<Song>>,
    forgotten_favorites_state: StoredViewState<Vec<Song>>,
    favorites_state: StoredViewState<Vec<FavoriteEntry>>,
    podcast_subscriptions: StoredViewState<Vec<PodcastSubscription>>,
    episodes_for_later: StoredViewState<Vec<SavedEpisode>>,
    podcast_operation: PodcastOperation,
    podcast_error: Option<String>,
    podcast_notice: Option<String>,
    podcast_task: Option<Task<()>>,
    podcast_state_revision: u64,
    episode_resume_generation: u64,
    last_episode_progress_save: Instant,
    playlists_state: StoredViewState<Vec<LocalPlaylist>>,
    playlist_detail: Option<PlaylistDetailState>,
    playlist_sort: PlaylistSort,
    playlist_sort_direction: SortDirection,
    library_tab: LibraryTab,
    library_song_source: LibrarySongSource,
    library_song_sort: LibrarySongSort,
    library_song_sort_direction: SortDirection,
    library_song_query: String,
    library_album_source: LibraryAlbumSource,
    library_artist_source: LibraryArtistSource,
    library_podcast_source: LibraryPodcastSource,
    library_podcast_sort: LibrarySongSort,
    library_podcast_sort_direction: SortDirection,
    library_catalog_sort: LibraryCatalogSort,
    library_catalog_sort_direction: SortDirection,
    library_catalog_query: String,
    library_playlist_query: String,
    library_operation: LibraryOperation,
    library_error: Option<String>,
    theme_mode: ThemeMode,
    _initial_storage_task: Task<()>,
    _playback_refresh_task: Task<()>,
    _subscriptions: Vec<Subscription>,
}

impl MetrolistShell {
    pub fn new(
        route: Route,
        settings: AppSettings,
        services: DesktopServices,
        store: DesktopStore,
        integrations: IntegrationBootstrap,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let IntegrationBootstrap {
            account,
            lastfm,
            discord_presence,
            discord_warning,
            listen_together,
            listen_together_warning,
        } = integrations;
        let AccountBootstrap {
            credential_store,
            auth_session,
            credential_warning,
        } = account;
        let LastFmBootstrap {
            credential_store: lastfm_credential_store,
            api_credentials: lastfm_api_credentials,
            client: lastfm_client,
            session: lastfm_session,
            warning: lastfm_warning,
        } = lastfm;
        let search_input = cx.new(|cx| {
            InputState::new(window, cx).placeholder("Search songs, albums, artists, or playlists")
        });
        let history_search_input =
            cx.new(|cx| InputState::new(window, cx).placeholder("Filter title or artist"));
        let library_search_input =
            cx.new(|cx| InputState::new(window, cx).placeholder("Filter title or artist"));
        let library_catalog_search_input =
            cx.new(|cx| InputState::new(window, cx).placeholder("Filter title or details"));
        let library_playlist_search_input =
            cx.new(|cx| InputState::new(window, cx).placeholder("Search playlists"));
        let autoeq_search_input = cx.new(|cx| {
            InputState::new(window, cx).placeholder("Search headphone or earphone model")
        });
        let playlist_name_input =
            cx.new(|cx| InputState::new(window, cx).placeholder("New playlist name"));
        let playlist_rename_input =
            cx.new(|cx| InputState::new(window, cx).placeholder("Playlist name"));
        let proxy_address_input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("127.0.0.1:8080")
                .default_value(settings.proxy.address.clone())
        });
        let proxy_username_input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("Optional username")
                .default_value(settings.proxy.username.clone())
        });
        let proxy_password_input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("Optional password")
                .default_value(settings.proxy.password.clone())
                .masked(true)
        });
        let cache_root_input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("Absolute cache directory")
                .default_value(settings.cache_root.to_string_lossy().into_owned())
        });
        let account_cookie_input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("Paste a Cookie header containing SAPISID")
                .masked(true)
        });
        let lastfm_username_input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("Last.fm username")
                .default_value(
                    lastfm_session
                        .as_ref()
                        .map(LastFmSession::username)
                        .unwrap_or_default(),
                )
        });
        let lastfm_password_input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("Last.fm password")
                .masked(true)
        });
        let listen_together_server_input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("wss://example.org/ws")
                .default_value(settings.listen_together.server_url.clone())
        });
        let listen_together_username_input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("Display name")
                .default_value(settings.listen_together.username.clone())
        });
        let listen_together_room_code_input =
            cx.new(|cx| InputState::new(window, cx).placeholder("8-character room code"));
        let progress_slider = cx.new(|_| {
            SliderState::new()
                .min(0.0)
                .max(1.0)
                .step(0.001)
                .default_value(0.0)
        });
        let DesktopServices {
            innertube: search_client,
            lyrics: lyrics_client,
            thumbnails: thumbnail_cache,
            audio_cache,
            downloaded_audio,
            audio: audio_player,
            microphone: microphone_recorder,
            recognition: recognition_client,
        } = services;
        let volume_slider = cx.new(|_| {
            SliderState::new()
                .min(0.0)
                .max(1.0)
                .step(0.01)
                .default_value(audio_player.snapshot().volume)
        });

        let mut subscriptions = vec![cx.subscribe_in(&search_input, window, {
            let search_input = search_input.clone();
            move |this, _, event: &InputEvent, window, cx| match event {
                InputEvent::Change => {
                    this.model.set_search_query(search_input.read(cx).value());
                    this.browse_state = None;
                    this.browse_task = None;
                    this.search_task = None;
                    this.search_state = SearchViewState::Idle;
                    if this.search_source == SearchSource::Online {
                        this.schedule_search_suggestions(window, cx);
                    } else {
                        this.dismiss_search_suggestions();
                        this.refresh_visible_thumbnails(cx);
                        cx.notify();
                    }
                }
                InputEvent::PressEnter { .. } => this.start_search(window, cx),
                _ => {}
            }
        })];
        subscriptions.push(cx.subscribe(
            &history_search_input,
            |this, input, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) {
                    this.history_query = input.read(cx).value().to_string();
                    cx.notify();
                }
            },
        ));
        subscriptions.push(cx.subscribe(
            &library_search_input,
            |this, input, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) {
                    this.library_song_query = input.read(cx).value().to_string();
                    this.refresh_visible_thumbnails(cx);
                    cx.notify();
                }
            },
        ));
        subscriptions.push(cx.subscribe(
            &library_catalog_search_input,
            |this, input, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) {
                    this.library_catalog_query = input.read(cx).value().to_string();
                    this.refresh_visible_thumbnails(cx);
                    cx.notify();
                }
            },
        ));
        subscriptions.push(cx.subscribe(
            &library_playlist_search_input,
            |this, input, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) {
                    this.library_playlist_query = input.read(cx).value().to_string();
                    this.refresh_visible_thumbnails(cx);
                    cx.notify();
                }
            },
        ));
        subscriptions.push(cx.subscribe_in(
            &playlist_name_input,
            window,
            |this, _, event: &InputEvent, window, cx| {
                if matches!(event, InputEvent::PressEnter { .. }) {
                    this.create_playlist(window, cx);
                }
            },
        ));
        subscriptions.push(cx.subscribe(
            &autoeq_search_input,
            |this, _, event: &InputEvent, cx| match event {
                InputEvent::Change => this.schedule_auto_eq_search(cx),
                InputEvent::PressEnter { .. } => this.refresh_auto_eq_models(cx),
                _ => {}
            },
        ));
        for input in [
            &proxy_address_input,
            &proxy_username_input,
            &proxy_password_input,
            &cache_root_input,
        ] {
            subscriptions.push(cx.subscribe(input, |this, _, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) {
                    this.settings_error = None;
                    this.settings_notice = None;
                    cx.notify();
                }
            }));
        }
        subscriptions.push(cx.subscribe(
            &account_cookie_input,
            |this, _, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) {
                    this.account_error = None;
                    cx.notify();
                }
            },
        ));
        for input in [&lastfm_username_input, &lastfm_password_input] {
            subscriptions.push(cx.subscribe(input, |this, _, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) {
                    this.lastfm_error = None;
                    this.lastfm_notice = None;
                    cx.notify();
                }
            }));
        }
        for input in [
            &listen_together_server_input,
            &listen_together_username_input,
            &listen_together_room_code_input,
        ] {
            subscriptions.push(cx.subscribe(input, |this, _, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) {
                    this.listen_together_error = None;
                    this.listen_together_notice = None;
                    cx.notify();
                }
            }));
        }
        subscriptions.push(
            cx.subscribe(
                &progress_slider,
                |this, _, event: &SliderEvent, cx| match event {
                    SliderEvent::Change(value) => {
                        this.seeking = true;
                        this.seek_preview = this
                            .playback_duration()
                            .map(|duration| duration.mul_f32(value.end().clamp(0.0, 1.0)));
                        cx.notify();
                    }
                    SliderEvent::Release(value) => {
                        let target = this
                            .playback_duration()
                            .map(|duration| duration.mul_f32(value.end().clamp(0.0, 1.0)));
                        this.seeking = false;
                        this.seek_preview = None;
                        if this.reject_guest_playback_control(cx) {
                            return;
                        }
                        if let Some(target) = target
                            && let Err(error) = this.audio_player.seek(target)
                        {
                            this.playback_error = Some(error.to_string());
                        }
                        cx.notify();
                    }
                },
            ),
        );
        subscriptions.push(
            cx.subscribe(&volume_slider, |this, _, event: &SliderEvent, cx| {
                if this.listen_together_is_guest() {
                    return;
                }
                let volume = match event {
                    SliderEvent::Change(value) | SliderEvent::Release(value) => {
                        value.end().clamp(0.0, 1.0)
                    }
                };
                this.audio_player.set_volume(volume);
                if matches!(event, SliderEvent::Release(_)) {
                    this.save_session(cx);
                }
                cx.notify();
            }),
        );
        let playback_refresh_task = cx.spawn_in(window, async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(250))
                    .await;
                if this
                    .update_in(cx, |this, window, cx| {
                        this.poll_playback(window, cx);
                    })
                    .is_err()
                {
                    break;
                }
            }
        });
        let initial_store = store.clone();
        let initial_downloaded_audio = downloaded_audio.clone();
        let playlist_sort = PlaylistSort::default();
        let playlist_sort_direction = SortDirection::default();
        let initial_storage_operation = cx.background_executor().spawn(async move {
            let (
                session,
                history,
                search_history,
                keep_listening,
                forgotten_favorites,
                favorites,
                podcast_subscriptions,
                episodes_for_later,
                catalog_items,
                playlists,
                downloads,
                equalizer_profiles,
            ) = futures::join!(
                initial_store.load_session(),
                initial_store.recent_history(100),
                initial_store.search_history(500),
                initial_store.keep_listening(15, 5),
                initial_store.forgotten_favorites(20),
                initial_store.favorites(500),
                initial_store.podcast_subscriptions(),
                initial_store.episodes_for_later(),
                initial_store.catalog_items(2_000),
                initial_store.playlists_sorted(playlist_sort, playlist_sort_direction),
                initial_store.downloads(),
                initial_store.equalizer_profiles()
            );
            let downloads = match downloads {
                Ok(mut downloads) => {
                    for download in &mut downloads {
                        if download.is_complete()
                            && let (Some(resource_key), Some(content_length)) =
                                (download.resource_key(), download.content_length)
                            && !initial_downloaded_audio
                                .contains_complete_resource(
                                    &resource_key,
                                    content_length,
                                    crate::services::RANGE_CHUNK_SIZE,
                                )
                                .unwrap_or(false)
                        {
                            let message =
                                "downloaded audio is missing or incomplete; retry to repair it";
                            if initial_store
                                .stop_download(
                                    download.song.video_id.clone(),
                                    DownloadState::Failed,
                                    Some(message.into()),
                                )
                                .await
                                .is_ok()
                            {
                                download.state = DownloadState::Failed;
                                download.completed_at_ms = None;
                                download.last_error = Some(message.into());
                            }
                        }
                    }
                    Ok(downloads)
                }
                Err(error) => Err(error),
            };
            (
                session,
                history,
                search_history,
                keep_listening,
                forgotten_favorites,
                favorites,
                podcast_subscriptions,
                episodes_for_later,
                catalog_items,
                playlists,
                downloads,
                equalizer_profiles,
            )
        });
        let initial_storage_task = cx.spawn(async move |this, cx| {
            let (
                session,
                history,
                search_history,
                keep_listening,
                forgotten_favorites,
                favorites,
                podcast_subscriptions,
                episodes_for_later,
                catalog_items,
                playlists,
                downloads,
                equalizer_profiles,
            ) = initial_storage_operation.await;
            this.update(cx, |this, cx| {
                match session {
                    Ok(Some(session)) => this.restore_session(session, cx),
                    Ok(None) => {}
                    Err(error) => tracing::warn!(%error, "playback session restore failed"),
                }
                this.history_state = match history {
                    Ok(history) => HistoryViewState::Loaded(history),
                    Err(error) => HistoryViewState::Failed(error.to_string()),
                };
                if matches!(&this.search_history_state, StoredViewState::Loading) {
                    this.search_history_state = match search_history {
                        Ok(history) => StoredViewState::Loaded(history),
                        Err(error) => StoredViewState::Failed(error.to_string()),
                    };
                }
                this.keep_listening_state = match keep_listening {
                    Ok(songs) => StoredViewState::Loaded(songs),
                    Err(error) => StoredViewState::Failed(error.to_string()),
                };
                this.forgotten_favorites_state = match forgotten_favorites {
                    Ok(songs) => StoredViewState::Loaded(songs),
                    Err(error) => StoredViewState::Failed(error.to_string()),
                };
                this.favorites_state = match favorites {
                    Ok(favorites) => StoredViewState::Loaded(favorites),
                    Err(error) => StoredViewState::Failed(error.to_string()),
                };
                this.reload_daily_discover(cx);
                if this.podcast_state_revision == 0 {
                    this.podcast_subscriptions = match podcast_subscriptions {
                        Ok(podcasts) => StoredViewState::Loaded(podcasts),
                        Err(error) => StoredViewState::Failed(error.to_string()),
                    };
                    this.episodes_for_later = match episodes_for_later {
                        Ok(episodes) => StoredViewState::Loaded(episodes),
                        Err(error) => StoredViewState::Failed(error.to_string()),
                    };
                }
                match catalog_items {
                    Ok(items) => this.merge_local_catalog_items(items),
                    Err(error) if matches!(this.local_catalog_state, StoredViewState::Loading) => {
                        this.local_catalog_state = StoredViewState::Failed(error.to_string());
                    }
                    Err(error) => this.local_catalog_error = Some(error.to_string()),
                }
                this.playlists_state = match playlists {
                    Ok(playlists) => StoredViewState::Loaded(playlists),
                    Err(error) => StoredViewState::Failed(error.to_string()),
                };
                this.equalizer_profiles = match equalizer_profiles {
                    Ok(profiles) => StoredViewState::Loaded(profiles),
                    Err(error) => StoredViewState::Failed(error.to_string()),
                };
                match downloads {
                    Ok(downloads) => {
                        this.download_queue.extend(
                            downloads
                                .iter()
                                .filter(|download| {
                                    matches!(
                                        download.state,
                                        DownloadState::Queued | DownloadState::Downloading
                                    )
                                })
                                .map(|download| download.song.video_id.clone()),
                        );
                        this.downloads_state = StoredViewState::Loaded(downloads);
                        this.start_queued_downloads(cx);
                    }
                    Err(error) => {
                        this.downloads_state = StoredViewState::Failed(error.to_string());
                    }
                }
                this.library_operation = LibraryOperation::Idle;
                this.refresh_visible_thumbnails(cx);
                cx.notify();
            })
            .ok();
        });
        let initial_home_client = search_client.clone();
        let initial_home_task = cx.spawn(async move |this, cx| {
            let result = initial_home_client.home(None).await;
            this.update(cx, |this, cx| {
                match result {
                    Ok(page) => {
                        this.remember_local_catalog_items(home_catalog_items(&page), cx);
                        this.home_default_page = Some(page.clone());
                        this.home_state = HomeFeedState::Loaded(page);
                    }
                    Err(error) => this.home_state = HomeFeedState::Failed(error.to_string()),
                }
                this.refresh_visible_thumbnails(cx);
                cx.notify();
            })
            .ok();
        });
        let initial_explore_client = search_client.clone();
        let initial_explore_task = cx.spawn(async move |this, cx| {
            let result = initial_explore_client.explore().await;
            this.update(cx, |this, cx| {
                this.explore_state = match result {
                    Ok(page) => {
                        this.remember_local_catalog_items(explore_catalog_items(&page), cx);
                        ExploreFeedState::Loaded(page)
                    }
                    Err(error) => ExploreFeedState::Failed(error.to_string()),
                };
                this.refresh_visible_thumbnails(cx);
                cx.notify();
            })
            .ok();
        });
        let (autoeq_database_state, initial_autoeq_operation, initial_autoeq_task) =
            match AutoEqClient::with_settings(&settings) {
                Ok(client) if client.is_database_cached() => {
                    let operation = cx
                        .background_executor()
                        .spawn(async move { client.build_index().await });
                    let task = cx.spawn(async move |this, cx| {
                        let result = operation.await;
                        this.update(cx, |this, cx| {
                            this.equalizer_operation = EqualizerOperation::Idle;
                            this.equalizer_task = None;
                            match result {
                                Ok(index) => this.install_auto_eq_index(index, cx),
                                Err(error) => {
                                    this.autoeq_database_state = AutoEqDatabaseState::Failed {
                                        message: error.to_string(),
                                        cached: true,
                                    };
                                }
                            }
                            cx.notify();
                        })
                        .ok();
                    });
                    (
                        AutoEqDatabaseState::Loading,
                        EqualizerOperation::LoadingDatabase,
                        Some(task),
                    )
                }
                Ok(_) => (
                    AutoEqDatabaseState::NotLoaded { cached: false },
                    EqualizerOperation::Idle,
                    None,
                ),
                Err(error) => (
                    AutoEqDatabaseState::Failed {
                        message: error.to_string(),
                        cached: false,
                    },
                    EqualizerOperation::Idle,
                    None,
                ),
            };
        let has_initial_auth = auth_session.is_some();
        let initial_account_task = auth_session.as_ref().map(|_| {
            let client = search_client.clone();
            cx.spawn(async move |this, cx| {
                let result = client.account_info().await;
                this.update(cx, |this, cx| {
                    this.account_operation = AccountOperation::Idle;
                    match result {
                        Ok(profile) => {
                            this.account_state = AccountViewState::SignedIn(profile);
                            this.reload_cloud_library(cx);
                            this.reload_remote_history(cx);
                            this.sync_podcast_library(cx);
                        }
                        Err(error) => {
                            let message = this.record_account_failure(&error);
                            this.cloud_library_state =
                                CloudLibraryViewState::Failed(message.clone());
                            this.remote_history_state = RemoteHistoryViewState::Failed(message);
                        }
                    }
                    this.refresh_visible_thumbnails(cx);
                    cx.notify();
                })
                .ok();
            })
        });
        let now = Instant::now();
        if let Err(error) = audio_player.refresh_output_devices() {
            tracing::warn!(%error, "initial audio output refresh failed");
        }
        let desktop_media = DesktopMediaSession::new(window);
        let theme_mode = match settings.theme {
            AppTheme::Light => ThemeMode::Light,
            AppTheme::Dark => ThemeMode::Dark,
        };

        Self {
            model: AppModel::new(route),
            search_input,
            history_search_input,
            library_search_input,
            library_catalog_search_input,
            library_playlist_search_input,
            autoeq_search_input,
            playlist_name_input,
            playlist_rename_input,
            proxy_address_input,
            proxy_username_input,
            proxy_password_input,
            cache_root_input,
            account_cookie_input,
            lastfm_username_input,
            lastfm_password_input,
            listen_together_server_input,
            listen_together_username_input,
            listen_together_room_code_input,
            search_client,
            lyrics_client,
            microphone_recorder,
            recognition_client,
            home_state: HomeFeedState::Loading,
            home_default_page: None,
            selected_home_chip: None,
            home_loading_more: false,
            home_load_more_error: None,
            home_seen_continuations: HashSet::new(),
            daily_discover_state: StoredViewState::Loading,
            explore_state: ExploreFeedState::Loading,
            search_source: SearchSource::Online,
            local_search_filter: LocalSearchFilter::All,
            local_catalog_state: StoredViewState::Loading,
            local_catalog_error: None,
            search_filter: SearchFilter::All,
            search_state: SearchViewState::Idle,
            search_suggestion_state: SearchSuggestionViewState::Hidden,
            search_suggestion_generation: 0,
            search_history_state: StoredViewState::Loading,
            search_history_task: None,
            search_history_error: None,
            stats_period: StatsPeriod::Week,
            stats_state: StoredViewState::Loading,
            stats_task: None,
            history_query: String::new(),
            browse_state: None,
            browse_return_route: Route::Search,
            search_loading_more: false,
            search_load_more_error: None,
            search_seen_continuations: HashSet::new(),
            browse_loading_more: false,
            browse_load_more_error: None,
            browse_seen_continuations: HashSet::new(),
            thumbnail_cache: Some(thumbnail_cache),
            thumbnail_images: HashMap::new(),
            thumbnail_order: VecDeque::new(),
            thumbnail_failures: HashSet::new(),
            thumbnail_tasks: HashMap::new(),
            search_task: None,
            search_suggestion_task: None,
            browse_task: None,
            home_task: Some(initial_home_task),
            daily_discover_task: None,
            explore_task: Some(initial_explore_task),
            recognition_state: RecognitionViewState::Ready,
            recognition_cancellation: None,
            recognition_generation: 0,
            recognition_task: None,
            recognition_history_visible: false,
            recognition_history_state: StoredViewState::Loading,
            recognition_history_task: None,
            recognition_history_error: None,
            playback_task: None,
            lyrics_task: None,
            progress_slider,
            volume_slider,
            audio_player,
            audio_cache,
            downloaded_audio,
            downloads_state: StoredViewState::Loading,
            download_queue: VecDeque::new(),
            active_downloads: HashMap::new(),
            download_tasks: HashMap::new(),
            download_removals: HashSet::new(),
            download_error: None,
            desktop_media,
            store,
            queue: Queue::default(),
            repeat_mode: RepeatMode::Off,
            shuffle_enabled: false,
            sleep_timer: None,
            radio_state: RadioQueueState::Idle,
            radio_task: None,
            queue_generation: 0,
            queue_visible: false,
            now_playing_visible: false,
            now_playing_tab: NowPlayingTab::UpNext,
            lyrics_visible: false,
            playback_parameters_visible: false,
            lyrics_state: LyricsViewState::Idle,
            lyrics_active_line: None,
            lyrics_scroll: ScrollHandle::new(),
            playlist_picker_song: None,
            cloud_playlist_picker_song: None,
            settings: settings.clone(),
            settings_draft: settings,
            settings_operation: SettingsOperation::Idle,
            settings_error: None,
            settings_notice: None,
            settings_task: None,
            equalizer_profiles: StoredViewState::Loading,
            equalizer_operation: initial_autoeq_operation,
            equalizer_error: None,
            equalizer_notice: None,
            equalizer_delete_confirmation: None,
            equalizer_task: initial_autoeq_task,
            autoeq_database_state,
            autoeq_wizard_step: AutoEqWizardStep::ModelSelection,
            autoeq_models: Vec::new(),
            autoeq_selected_model: None,
            autoeq_variants: Vec::new(),
            autoeq_selected_variant_paths: HashSet::new(),
            autoeq_search_generation: 0,
            autoeq_search_task: None,
            playback_parameters_pending: None,
            playback_parameters_error: None,
            playback_parameters_notice: None,
            playback_parameters_task: None,
            credential_store,
            auth_session,
            account_state: if initial_account_task.is_some() {
                AccountViewState::Checking
            } else {
                AccountViewState::SignedOut
            },
            account_operation: AccountOperation::Idle,
            account_error: None,
            credential_warning,
            account_task: initial_account_task,
            lastfm_credential_store,
            lastfm_api_credentials,
            lastfm_client,
            lastfm_session,
            lastfm_operation: LastFmOperation::Idle,
            lastfm_warning,
            lastfm_error: None,
            lastfm_notice: None,
            lastfm_task: None,
            lastfm_playback_task: None,
            lastfm_playback_tracker: LastFmPlaybackTracker::default(),
            discord_presence,
            discord_presence_tracker: DiscordPresenceTracker::default(),
            discord_warning,
            listen_together_snapshot: listen_together
                .as_ref()
                .map(ListenTogetherClient::snapshot)
                .unwrap_or_default(),
            listen_together,
            listen_together_tracker: ListenTogetherPlaybackTracker::default(),
            listen_together_warning,
            listen_together_error: None,
            listen_together_notice: None,
            listen_together_pending_sync: None,
            cloud_library_state: if has_initial_auth {
                CloudLibraryViewState::Loading
            } else {
                CloudLibraryViewState::SignedOut
            },
            cloud_library_task: None,
            cloud_library_operation: CloudLibraryOperation::Idle,
            cloud_library_error: None,
            cloud_mutation_task: None,
            remote_history_state: if has_initial_auth {
                RemoteHistoryViewState::Loading
            } else {
                RemoteHistoryViewState::SignedOut
            },
            remote_history_source: HistorySource::Local,
            remote_history_operation: RemoteHistoryOperation::Idle,
            remote_history_error: None,
            remote_history_task: None,
            current_song: None,
            resolving_playback: false,
            seeking: false,
            seek_preview: None,
            last_playback_state: PlaybackState::Idle,
            playback_retry_count: 0,
            playback_source_attempt: PlaybackSourceAttempt::None,
            play_after_resolution: None,
            active_playback_source: None,
            persisted_playback_source: None,
            pending_resume_position: None,
            played_this_track: Duration::ZERO,
            history_recorded_for_current: false,
            last_playback_poll: now,
            last_session_save: now,
            playback_error: None,
            history_state: HistoryViewState::Loading,
            keep_listening_state: StoredViewState::Loading,
            forgotten_favorites_state: StoredViewState::Loading,
            favorites_state: StoredViewState::Loading,
            podcast_subscriptions: StoredViewState::Loading,
            episodes_for_later: StoredViewState::Loading,
            podcast_operation: PodcastOperation::Idle,
            podcast_error: None,
            podcast_notice: None,
            podcast_task: None,
            podcast_state_revision: 0,
            episode_resume_generation: 0,
            last_episode_progress_save: now,
            playlists_state: StoredViewState::Loading,
            playlist_detail: None,
            playlist_sort,
            playlist_sort_direction,
            library_tab: LibraryTab::Overview,
            library_song_source: LibrarySongSource::Liked,
            library_song_sort: LibrarySongSort::Recent,
            library_song_sort_direction: SortDirection::Descending,
            library_song_query: String::new(),
            library_album_source: LibraryAlbumSource::Liked,
            library_artist_source: LibraryArtistSource::Liked,
            library_podcast_source: LibraryPodcastSource::Episodes,
            library_podcast_sort: LibrarySongSort::Recent,
            library_podcast_sort_direction: SortDirection::Descending,
            library_catalog_sort: LibraryCatalogSort::Recent,
            library_catalog_sort_direction: SortDirection::Descending,
            library_catalog_query: String::new(),
            library_playlist_query: String::new(),
            library_operation: LibraryOperation::LoadingLibrary,
            library_error: None,
            theme_mode,
            _initial_storage_task: initial_storage_task,
            _playback_refresh_task: playback_refresh_task,
            _subscriptions: subscriptions,
        }
    }

    fn reload_home(
        &mut self,
        params: Option<String>,
        selected_chip: Option<HomeChip>,
        cx: &mut Context<Self>,
    ) {
        self.home_state = HomeFeedState::Loading;
        self.selected_home_chip = selected_chip;
        self.home_loading_more = false;
        self.home_load_more_error = None;
        self.home_seen_continuations.clear();
        self.refresh_visible_thumbnails(cx);
        let client = self.search_client.clone();
        self.home_task = Some(cx.spawn(async move |this, cx| {
            let result = client.home(params.as_deref()).await;
            this.update(cx, |this, cx| {
                match result {
                    Ok(mut page) => {
                        this.remember_local_catalog_items(home_catalog_items(&page), cx);
                        if this.selected_home_chip.is_some() {
                            if let Some(default_page) = &this.home_default_page {
                                page.chips.clone_from(&default_page.chips);
                            }
                        } else {
                            this.home_default_page = Some(page.clone());
                        }
                        this.home_state = HomeFeedState::Loaded(page);
                    }
                    Err(error) => this.home_state = HomeFeedState::Failed(error.to_string()),
                }
                this.refresh_visible_thumbnails(cx);
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    fn toggle_home_chip(&mut self, chip: HomeChip, cx: &mut Context<Self>) {
        if self.selected_home_chip.as_ref() == Some(&chip) {
            self.home_task = None;
            self.selected_home_chip = None;
            self.home_loading_more = false;
            self.home_load_more_error = None;
            self.home_seen_continuations.clear();
            if let Some(page) = self.home_default_page.clone() {
                self.home_state = HomeFeedState::Loaded(page);
                self.refresh_visible_thumbnails(cx);
                cx.notify();
            } else {
                self.reload_home(None, None, cx);
            }
            return;
        }
        self.reload_home(chip.params.clone(), Some(chip), cx);
    }

    fn load_more_home(&mut self, cx: &mut Context<Self>) {
        if self.home_loading_more {
            return;
        }
        let continuation = match &self.home_state {
            HomeFeedState::Loaded(page) => page.continuation.clone(),
            HomeFeedState::Loading | HomeFeedState::Failed(_) => None,
        };
        let Some(continuation) = continuation else {
            return;
        };
        if self.home_seen_continuations.contains(&continuation) {
            if let HomeFeedState::Loaded(page) = &mut self.home_state {
                page.continuation = None;
            }
            self.home_load_more_error =
                Some("YouTube Music repeated a continuation token; loading stopped safely.".into());
            cx.notify();
            return;
        }

        self.home_loading_more = true;
        self.home_load_more_error = None;
        let client = self.search_client.clone();
        self.home_task = Some(cx.spawn(async move |this, cx| {
            let result = client.home_continuation(&continuation).await;
            this.update(cx, |this, cx| {
                this.home_loading_more = false;
                match result {
                    Ok(mut next) => {
                        this.remember_local_catalog_items(home_catalog_items(&next), cx);
                        this.home_seen_continuations.insert(continuation);
                        if next
                            .continuation
                            .as_ref()
                            .is_some_and(|token| this.home_seen_continuations.contains(token))
                        {
                            next.continuation = None;
                        }
                        if let HomeFeedState::Loaded(page) = &mut this.home_state {
                            page.append_continuation(next);
                        }
                    }
                    Err(error) => this.home_load_more_error = Some(error.to_string()),
                }
                this.refresh_visible_thumbnails(cx);
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    fn reload_daily_discover(&mut self, cx: &mut Context<Self>) {
        self.daily_discover_task = None;
        let seeds = match &self.favorites_state {
            StoredViewState::Loaded(favorites) => favorites
                .iter()
                .rev()
                .filter(|entry| !entry.song.is_episode)
                .map(|entry| entry.song.clone())
                .take(5)
                .collect::<Vec<_>>(),
            StoredViewState::Loading => {
                self.daily_discover_state = StoredViewState::Loading;
                return;
            }
            StoredViewState::Failed(message) => {
                self.daily_discover_state = StoredViewState::Failed(message.clone());
                return;
            }
        };
        if seeds.is_empty() {
            self.daily_discover_state = StoredViewState::Loaded(Vec::new());
            self.refresh_visible_thumbnails(cx);
            cx.notify();
            return;
        }

        self.daily_discover_state = StoredViewState::Loading;
        let client = self.search_client.clone();
        self.daily_discover_task = Some(cx.spawn(async move |this, cx| {
            let requests = seeds.into_iter().map(|seed| {
                let client = client.clone();
                async move {
                    let page = client.radio(&seed.video_id).await?;
                    let recommendation = page
                        .recommendations_after_current(&seed.video_id)
                        .into_iter()
                        .find(|song| !song.is_episode);
                    Ok::<_, AppError>(recommendation.map(|recommendation| DailyDiscoverItem {
                        seed,
                        recommendation,
                    }))
                }
            });
            let results = futures::future::join_all(requests).await;
            this.update(cx, |this, cx| {
                this.daily_discover_task = None;
                let mut discoveries = Vec::new();
                let mut last_error = None;
                for result in results {
                    match result {
                        Ok(Some(item))
                            if !discoveries.iter().any(|existing: &DailyDiscoverItem| {
                                existing.recommendation.video_id == item.recommendation.video_id
                            }) =>
                        {
                            discoveries.push(item);
                        }
                        Ok(_) => {}
                        Err(error) => last_error = Some(error.to_string()),
                    }
                }
                this.daily_discover_state = if discoveries.is_empty() {
                    last_error.map_or_else(
                        || StoredViewState::Loaded(Vec::new()),
                        StoredViewState::Failed,
                    )
                } else {
                    StoredViewState::Loaded(discoveries)
                };
                this.refresh_visible_thumbnails(cx);
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    fn reload_explore(&mut self, cx: &mut Context<Self>) {
        self.explore_state = ExploreFeedState::Loading;
        self.refresh_visible_thumbnails(cx);
        let client = self.search_client.clone();
        self.explore_task = Some(cx.spawn(async move |this, cx| {
            let result = client.explore().await;
            this.update(cx, |this, cx| {
                this.explore_state = match result {
                    Ok(page) => {
                        this.remember_local_catalog_items(explore_catalog_items(&page), cx);
                        ExploreFeedState::Loaded(page)
                    }
                    Err(error) => ExploreFeedState::Failed(error.to_string()),
                };
                this.refresh_visible_thumbnails(cx);
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    fn settings_from_editor(&self, cx: &App) -> crate::Result<AppSettings> {
        let mut settings = self.settings_draft.clone();
        settings.proxy.address = self.proxy_address_input.read(cx).value().trim().to_owned();
        settings.proxy.username = self.proxy_username_input.read(cx).value().trim().to_owned();
        settings.proxy.password = self.proxy_password_input.read(cx).value().to_string();
        settings.cache_root = PathBuf::from(self.cache_root_input.read(cx).value().trim());
        settings.listen_together.server_url = self
            .listen_together_server_input
            .read(cx)
            .value()
            .trim()
            .to_owned();
        settings.listen_together.username = self
            .listen_together_username_input
            .read(cx)
            .value()
            .trim()
            .to_owned();
        settings.validate()
    }

    fn downloads(&self) -> Option<&[AudioDownload]> {
        match &self.downloads_state {
            StoredViewState::Loaded(downloads) => Some(downloads),
            StoredViewState::Loading | StoredViewState::Failed(_) => None,
        }
    }

    fn download_for(&self, video_id: &str) -> Option<&AudioDownload> {
        self.downloads()?
            .iter()
            .find(|download| download.song.video_id == video_id)
    }

    fn downloaded_playback_source(&self, video_id: &str) -> Option<PlaybackSource> {
        let download = self
            .download_for(video_id)
            .filter(|download| download.is_complete())?;
        Some(
            PlaybackSource::cache_only(
                download.cache_key(),
                download.mime_type.clone()?,
                download.content_length?,
            )
            .with_loudness_lufs_mb(download.loudness_lufs_mb),
        )
    }

    fn queue_song_download(&mut self, song: Song, cx: &mut Context<Self>) {
        self.queue_song_download_with_quality(song, self.settings.audio_quality, cx);
    }

    fn queue_song_download_with_quality(
        &mut self,
        song: Song,
        audio_quality: AudioQuality,
        cx: &mut Context<Self>,
    ) {
        if self.active_downloads.contains_key(&song.video_id)
            || self
                .download_queue
                .iter()
                .any(|video_id| video_id == &song.video_id)
            || self.download_for(&song.video_id).is_some_and(|download| {
                download.is_complete() && download.audio_quality == audio_quality
            })
        {
            return;
        }
        let previous_resource = self.download_for(&song.video_id).and_then(|download| {
            (download.audio_quality != audio_quality)
                .then(|| download.resource_key())
                .flatten()
        });
        let video_id = song.video_id.clone();
        let store = self.store.clone();
        let downloaded_audio = self.downloaded_audio.clone();
        self.download_error = None;
        let operation = cx.background_executor().spawn(async move {
            if let Some(resource_key) = previous_resource {
                downloaded_audio
                    .remove_resource(&resource_key)
                    .map_err(|error| {
                        AppError::Download(format!(
                            "old quality download could not be removed: {error}"
                        ))
                    })?;
            }
            store.queue_download(song, audio_quality).await
        });
        cx.spawn(async move |this, cx| {
            let result = operation.await;
            this.update(cx, |this, cx| {
                match result {
                    Ok(download) => {
                        if let StoredViewState::Loaded(downloads) = &mut this.downloads_state {
                            if let Some(existing) = downloads
                                .iter_mut()
                                .find(|existing| existing.song.video_id == download.song.video_id)
                            {
                                *existing = download;
                            } else {
                                downloads.push(download);
                            }
                        } else {
                            this.downloads_state = StoredViewState::Loaded(vec![download]);
                        }
                        if !this.download_queue.iter().any(|queued| queued == &video_id) {
                            this.download_queue.push_back(video_id);
                        }
                        this.start_queued_downloads(cx);
                    }
                    Err(error) => this.download_error = Some(error.to_string()),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    fn start_queued_downloads(&mut self, cx: &mut Context<Self>) {
        while self.active_downloads.len() < MAX_PARALLEL_DOWNLOADS {
            let Some(video_id) = self.download_queue.pop_front() else {
                break;
            };
            if self.active_downloads.contains_key(&video_id) {
                continue;
            }
            let Some(download) = self.download_for(&video_id).cloned() else {
                continue;
            };
            if download.is_complete() {
                continue;
            }
            self.start_download(download, cx);
        }
    }

    fn start_download(&mut self, download: AudioDownload, cx: &mut Context<Self>) {
        let video_id = download.song.video_id.clone();
        let audio_quality = download.audio_quality;
        let client = self.search_client.clone();
        let player_cache = self.audio_cache.clone();
        let downloaded_audio = self.downloaded_audio.clone();
        let store = self.store.clone();
        let completion_store = self.store.clone();
        let cancelled = Arc::new(AtomicBool::new(false));
        let worker_cancelled = cancelled.clone();
        let latest_update = Arc::new(Mutex::new(None));
        let worker_update = latest_update.clone();
        let worker_video_id = video_id.clone();
        let update_video_id = video_id.clone();
        let operation =
            cx.background_executor().spawn(async move {
                download_song(
                    client,
                    player_cache,
                    downloaded_audio,
                    &worker_video_id,
                    audio_quality,
                    worker_cancelled,
                    move |update| {
                        match &update {
                            DownloadUpdate::Prepared {
                                mime_type,
                                content_length,
                                downloaded_bytes,
                                loudness_lufs_mb,
                            } => futures::executor::block_on(store.mark_download_started(
                                update_video_id.clone(),
                                mime_type.clone(),
                                *content_length,
                                *downloaded_bytes,
                                *loudness_lufs_mb,
                            ))?,
                            DownloadUpdate::Progress {
                                downloaded_bytes, ..
                            } => futures::executor::block_on(store.update_download_progress(
                                update_video_id.clone(),
                                *downloaded_bytes,
                            ))?,
                        }
                        if let Ok(mut latest) = worker_update.lock() {
                            *latest = Some(update);
                        }
                        Ok(())
                    },
                )
                .await
            });
        let completion_video_id = video_id.clone();
        let task = cx.spawn(async move |this, cx| {
            let outcome = operation.await;
            let terminal_error = match outcome {
                Ok(DownloadOutcome::Completed(_)) => {
                    match completion_store
                        .finish_download(completion_video_id.clone())
                        .await
                    {
                        Ok(()) => None,
                        Err(error) => {
                            let message = error.to_string();
                            match completion_store
                                .stop_download(
                                    completion_video_id.clone(),
                                    DownloadState::Failed,
                                    Some(message),
                                )
                                .await
                            {
                                Ok(()) => Some(error),
                                Err(mark_error) => Some(AppError::Download(format!(
                                    "download completed but its state could not be committed: {error}; marking it failed also failed: {mark_error}"
                                ))),
                            }
                        }
                    }
                }
                Ok(DownloadOutcome::Cancelled) => completion_store
                    .stop_download(completion_video_id.clone(), DownloadState::Paused, None)
                    .await
                    .err(),
                Err(error) => {
                    let message = error.to_string();
                    completion_store
                        .stop_download(
                            completion_video_id.clone(),
                            DownloadState::Failed,
                            Some(message.clone()),
                        )
                        .await
                        .err()
                        .or(Some(error))
                }
            };
            let records = completion_store.downloads().await;
            this.update(cx, |this, cx| {
                this.active_downloads.remove(&completion_video_id);
                this.download_tasks.remove(&completion_video_id);
                if let Some(error) = terminal_error {
                    this.download_error = Some(error.to_string());
                }
                match records {
                    Ok(records) => this.downloads_state = StoredViewState::Loaded(records),
                    Err(error) => this.downloads_state = StoredViewState::Failed(error.to_string()),
                }
                if this.download_removals.remove(&completion_video_id) {
                    this.remove_download_files(completion_video_id.clone(), cx);
                } else {
                    this.start_queued_downloads(cx);
                }
                cx.notify();
            })
            .ok();
        });
        self.active_downloads.insert(
            video_id.clone(),
            ActiveDownload {
                cancelled,
                latest_update,
            },
        );
        self.download_tasks.insert(video_id, task);
    }

    fn pause_download(&mut self, video_id: &str, cx: &mut Context<Self>) {
        if let Some(active) = self.active_downloads.get(video_id) {
            active.cancelled.store(true, Ordering::Release);
            cx.notify();
            return;
        }
        self.download_queue.retain(|queued| queued != video_id);
        let store = self.store.clone();
        let video_id = video_id.to_owned();
        cx.spawn(async move |this, cx| {
            let result = store
                .stop_download(video_id, DownloadState::Paused, None)
                .await;
            let records = store.downloads().await;
            this.update(cx, |this, cx| {
                if let Err(error) = result {
                    this.download_error = Some(error.to_string());
                }
                if let Ok(records) = records {
                    this.downloads_state = StoredViewState::Loaded(records);
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn retry_download(&mut self, video_id: &str, cx: &mut Context<Self>) {
        let Some(download) = self.download_for(video_id).cloned() else {
            return;
        };
        self.queue_song_download_with_quality(download.song, download.audio_quality, cx);
    }

    fn remove_download(&mut self, video_id: &str, cx: &mut Context<Self>) {
        if let Some(active) = self.active_downloads.get(video_id) {
            self.download_removals.insert(video_id.to_owned());
            active.cancelled.store(true, Ordering::Release);
            cx.notify();
            return;
        }
        self.download_queue.retain(|queued| queued != video_id);
        self.remove_download_files(video_id.to_owned(), cx);
    }

    fn confirm_remove_download(
        &mut self,
        video_id: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(download) = self.download_for(&video_id).cloned() else {
            return;
        };
        if self.download_removals.contains(&video_id) {
            return;
        }
        let description = if download.is_complete() {
            format!(
                "The offline audio for \"{}\" will be removed from this device. Favourites, playlists, history, and YouTube Music data will be kept.",
                download.song.title
            )
        } else {
            format!(
                "The saved download progress for \"{}\" will be discarded. Favourites, playlists, history, and YouTube Music data will be kept.",
                download.song.title
            )
        };
        let weak = cx.weak_entity();
        window.open_alert_dialog(cx, move |dialog, _, _| {
            let weak = weak.clone();
            let video_id = video_id.clone();
            dialog
                .title("Remove offline download?")
                .description(description.clone())
                .button_props(
                    DialogButtonProps::default()
                        .ok_text("Remove")
                        .ok_variant(ButtonVariant::Danger)
                        .cancel_text("Cancel")
                        .show_cancel(true),
                )
                .on_ok(move |_, _, cx| {
                    weak.update(cx, |this, cx| {
                        this.remove_download(&video_id, cx);
                    })
                    .ok();
                    true
                })
        });
    }

    fn remove_download_files(&mut self, video_id: String, cx: &mut Context<Self>) {
        let resource_key = self
            .download_for(&video_id)
            .and_then(AudioDownload::resource_key);
        let downloaded_audio = self.downloaded_audio.clone();
        let store = self.store.clone();
        let removal_video_id = video_id.clone();
        self.download_removals.insert(video_id.clone());
        let operation = cx.background_executor().spawn(async move {
            if let Some(resource_key) = resource_key {
                downloaded_audio
                    .remove_resource(&resource_key)
                    .map_err(|error| {
                        AppError::Download(format!("download could not be removed: {error}"))
                    })?;
            }
            futures::executor::block_on(store.delete_download(removal_video_id))?;
            futures::executor::block_on(store.downloads())
        });
        cx.spawn(async move |this, cx| {
            let result = operation.await;
            this.update(cx, |this, cx| {
                this.download_removals.remove(&video_id);
                match result {
                    Ok(records) => this.downloads_state = StoredViewState::Loaded(records),
                    Err(error) => this.download_error = Some(error.to_string()),
                }
                this.start_queued_downloads(cx);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn poll_download_progress(&mut self) {
        let updates = self
            .active_downloads
            .iter()
            .filter_map(|(video_id, active)| {
                active
                    .latest_update
                    .lock()
                    .ok()
                    .and_then(|mut update| update.take())
                    .map(|update| (video_id.clone(), update))
            })
            .collect::<Vec<_>>();
        let StoredViewState::Loaded(downloads) = &mut self.downloads_state else {
            return;
        };
        for (video_id, update) in updates {
            let Some(download) = downloads
                .iter_mut()
                .find(|download| download.song.video_id == video_id)
            else {
                continue;
            };
            match update {
                DownloadUpdate::Prepared {
                    mime_type,
                    content_length,
                    downloaded_bytes,
                    loudness_lufs_mb,
                } => {
                    download.mime_type = Some(mime_type);
                    download.content_length = Some(content_length);
                    download.downloaded_bytes = downloaded_bytes;
                    download.loudness_lufs_mb = loudness_lufs_mb;
                    download.state = DownloadState::Downloading;
                    download.last_error = None;
                }
                DownloadUpdate::Progress {
                    downloaded_bytes, ..
                } => {
                    download.downloaded_bytes = downloaded_bytes;
                    download.state = DownloadState::Downloading;
                }
            }
        }
    }

    fn repair_failed_offline_download(&mut self, song: Song, cx: &mut Context<Self>) {
        let Some(resource_key) = self
            .download_for(&song.video_id)
            .filter(|download| download.is_complete())
            .and_then(AudioDownload::resource_key)
        else {
            return;
        };
        let video_id = song.video_id.clone();
        let failure =
            "offline audio could not be read or decoded; resume the download to repair it"
                .to_owned();
        self.download_removals.insert(video_id.clone());
        if let StoredViewState::Loaded(downloads) = &mut self.downloads_state
            && let Some(download) = downloads
                .iter_mut()
                .find(|download| download.song.video_id == video_id)
        {
            download.state = DownloadState::Failed;
            download.downloaded_bytes = 0;
            download.completed_at_ms = None;
            download.last_error = Some(failure.clone());
        }

        let downloaded_audio = self.downloaded_audio.clone();
        let store = self.store.clone();
        let repair_video_id = video_id.clone();
        let operation = cx.background_executor().spawn(async move {
            downloaded_audio
                .remove_resource(&resource_key)
                .map_err(|error| {
                    AppError::Download(format!(
                        "unreadable offline audio could not be discarded: {error}"
                    ))
                })?;
            futures::executor::block_on(store.stop_download(
                repair_video_id,
                DownloadState::Failed,
                Some(failure),
            ))?;
            futures::executor::block_on(store.downloads())
        });
        cx.spawn(async move |this, cx| {
            let result = operation.await;
            this.update_in(cx, |this, window, cx| {
                this.download_removals.remove(&video_id);
                match result {
                    Ok(records) => {
                        this.downloads_state = StoredViewState::Loaded(records);
                        this.resolve_and_play(song, window, cx);
                    }
                    Err(error) => {
                        let message = error.to_string();
                        this.download_error = Some(message.clone());
                        this.playback_error = Some(message);
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    fn account_busy(&self) -> bool {
        self.settings_operation == SettingsOperation::Applying
            || self.account_operation != AccountOperation::Idle
            || self.cloud_busy()
            || self.podcast_busy()
            || matches!(self.account_state, AccountViewState::Checking)
    }

    fn cloud_busy(&self) -> bool {
        self.cloud_library_operation != CloudLibraryOperation::Idle
            || self.remote_history_operation != RemoteHistoryOperation::Idle
    }

    fn account_ready(&self) -> bool {
        self.auth_session.is_some() && self.account_state.is_verified()
    }

    fn record_account_failure(&mut self, error: &AppError) -> String {
        let message = error.to_string();
        if matches!(error, AppError::SessionExpired(_)) {
            self.account_state = AccountViewState::Expired(message.clone());
            self.cloud_playlist_picker_song = None;
            self.remote_history_source = HistorySource::Local;
            match InnerTubeClient::with_settings(InnerTubeSession::default(), &self.settings) {
                Ok(client) => self.search_client = Arc::new(client),
                Err(client_error) => {
                    tracing::warn!(%client_error, "anonymous client recovery failed")
                }
            }
        } else {
            self.account_state = AccountViewState::Failed(message.clone());
        }
        message
    }

    fn record_cloud_request_failure(&mut self, error: &AppError) -> String {
        if matches!(error, AppError::SessionExpired(_) | AppError::Credential(_)) {
            self.record_account_failure(error)
        } else {
            error.to_string()
        }
    }

    fn record_cloud_error(&mut self, error: AppError) {
        let message = self.record_cloud_request_failure(&error);
        self.cloud_library_error = Some(message);
    }

    fn verify_account(&mut self, cx: &mut Context<Self>) {
        self.account_error = None;
        if self.auth_session.is_none() {
            self.account_task = None;
            self.cloud_library_task = None;
            self.remote_history_task = None;
            self.account_state = AccountViewState::SignedOut;
            self.cloud_library_state = CloudLibraryViewState::SignedOut;
            self.remote_history_state = RemoteHistoryViewState::SignedOut;
            self.remote_history_source = HistorySource::Local;
            return;
        }
        self.cloud_library_task = None;
        self.cloud_library_state = CloudLibraryViewState::Loading;
        self.remote_history_task = None;
        self.remote_history_state = RemoteHistoryViewState::Loading;
        self.account_state = AccountViewState::Checking;
        let Some(auth) = self.auth_session.clone() else {
            return;
        };
        let client = match InnerTubeClient::with_settings(
            InnerTubeSession::default().with_auth(Some(auth)),
            &self.settings,
        ) {
            Ok(client) => Arc::new(client),
            Err(error) => {
                self.record_account_failure(&error);
                cx.notify();
                return;
            }
        };
        let verified_client = client.clone();
        self.account_task = Some(cx.spawn(async move |this, cx| {
            let result = verified_client.account_info().await;
            this.update(cx, |this, cx| {
                match result {
                    Ok(profile) => {
                        this.search_client = client;
                        this.account_state = AccountViewState::SignedIn(profile);
                        this.reload_cloud_library(cx);
                        this.reload_remote_history(cx);
                        this.sync_podcast_library(cx);
                    }
                    Err(error) => {
                        let message = this.record_account_failure(&error);
                        this.cloud_library_state = CloudLibraryViewState::Failed(message.clone());
                        this.remote_history_state = RemoteHistoryViewState::Failed(message);
                    }
                }
                this.refresh_visible_thumbnails(cx);
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    fn reload_cloud_library(&mut self, cx: &mut Context<Self>) {
        if self.cloud_busy() {
            return;
        }
        if !self.account_ready() {
            self.cloud_library_task = None;
            if self.auth_session.is_none() {
                self.cloud_library_state = CloudLibraryViewState::SignedOut;
            }
            cx.notify();
            return;
        }
        self.cloud_library_error = None;
        self.cloud_library_state = CloudLibraryViewState::Loading;
        let client = self.search_client.clone();
        self.cloud_library_task = Some(cx.spawn(async move |this, cx| {
            let (
                liked_songs,
                library_songs,
                uploaded_songs,
                playlists,
                albums,
                uploaded_albums,
                artists,
            ) = futures::join!(
                client.completed_playlist_page("LM", "Liked songs"),
                client.completed_library_page("FEmusic_liked_videos", "Library songs"),
                client.completed_library_page_at_tab(
                    "FEmusic_library_privately_owned_tracks",
                    "Uploaded songs",
                    1,
                ),
                client.completed_library_page("FEmusic_liked_playlists", "Liked playlists"),
                client.completed_library_page("FEmusic_liked_albums", "Liked albums"),
                client.completed_library_page(
                    "FEmusic_library_privately_owned_releases",
                    "Uploaded albums",
                ),
                client.completed_library_page("FEmusic_library_corpus_artists", "Library artists")
            );
            let result = match (
                liked_songs,
                library_songs,
                uploaded_songs,
                playlists,
                albums,
                uploaded_albums,
                artists,
            ) {
                (
                    Ok(liked_songs),
                    Ok(library_songs),
                    Ok(uploaded_songs),
                    Ok(playlists),
                    Ok(albums),
                    Ok(uploaded_albums),
                    Ok(artists),
                ) => Ok(CloudLibraryData {
                    liked_songs: liked_songs.songs,
                    library_songs: library_songs.songs,
                    uploaded_songs: uploaded_songs.songs,
                    playlists: playlists
                        .related
                        .into_iter()
                        .filter(|item| item.kind == BrowseKind::Playlist)
                        .collect(),
                    albums: albums
                        .related
                        .into_iter()
                        .filter(|item| item.kind == BrowseKind::Album)
                        .collect(),
                    uploaded_albums: uploaded_albums
                        .related
                        .into_iter()
                        .filter(|item| item.kind == BrowseKind::Album)
                        .collect(),
                    artists: artists
                        .related
                        .into_iter()
                        .filter(|item| item.kind == BrowseKind::Artist)
                        .collect(),
                }),
                (Err(error), _, _, _, _, _, _)
                | (_, Err(error), _, _, _, _, _)
                | (_, _, Err(error), _, _, _, _)
                | (_, _, _, Err(error), _, _, _)
                | (_, _, _, _, Err(error), _, _)
                | (_, _, _, _, _, Err(error), _)
                | (_, _, _, _, _, _, Err(error)) => Err(error),
            };
            this.update(cx, |this, cx| {
                match result {
                    Ok(library) => {
                        this.remember_local_catalog_items(
                            library
                                .albums
                                .iter()
                                .chain(&library.uploaded_albums)
                                .chain(&library.artists)
                                .cloned(),
                            cx,
                        );
                        this.cloud_library_state = CloudLibraryViewState::Loaded(library);
                    }
                    Err(error) => {
                        let message = this.record_cloud_request_failure(&error);
                        this.cloud_library_state = CloudLibraryViewState::Failed(message);
                    }
                }
                this.refresh_visible_thumbnails(cx);
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    fn reload_remote_history(&mut self, cx: &mut Context<Self>) {
        if self.cloud_busy() {
            return;
        }
        if matches!(self.remote_history_state, RemoteHistoryViewState::Loading)
            && self.remote_history_task.is_some()
        {
            return;
        }
        if !self.account_ready() {
            self.remote_history_task = None;
            if self.auth_session.is_none() {
                self.remote_history_state = RemoteHistoryViewState::SignedOut;
                self.remote_history_source = HistorySource::Local;
            }
            cx.notify();
            return;
        }
        self.remote_history_error = None;
        self.remote_history_state = RemoteHistoryViewState::Loading;
        let client = self.search_client.clone();
        self.remote_history_task = Some(cx.spawn(async move |this, cx| {
            let result = client.completed_history_page().await;
            this.update(cx, |this, cx| {
                match result {
                    Ok(page) => {
                        this.remote_history_state = RemoteHistoryViewState::Loaded(page);
                    }
                    Err(error) => {
                        let message = this.record_cloud_request_failure(&error);
                        this.remote_history_state = RemoteHistoryViewState::Failed(message);
                    }
                }
                this.refresh_visible_thumbnails(cx);
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    fn remove_remote_history_item(&mut self, feedback_token: String, cx: &mut Context<Self>) {
        if self.cloud_busy() || !self.account_ready() {
            return;
        }
        let previous = match &self.remote_history_state {
            RemoteHistoryViewState::Loaded(page) => page.clone(),
            _ => return,
        };
        if let RemoteHistoryViewState::Loaded(page) = &mut self.remote_history_state
            && !page.remove_feedback_token(&feedback_token)
        {
            return;
        }
        self.remote_history_operation = RemoteHistoryOperation::Removing;
        self.remote_history_error = None;
        let client = self.search_client.clone();
        self.remote_history_task = Some(cx.spawn(async move |this, cx| {
            let result = client.remove_history_item(&feedback_token).await;
            this.update(cx, |this, cx| {
                this.remote_history_operation = RemoteHistoryOperation::Idle;
                match result {
                    Ok(()) => this.remote_history_error = None,
                    Err(error) => {
                        let message = this.record_cloud_request_failure(&error);
                        this.remote_history_state = RemoteHistoryViewState::Loaded(previous);
                        this.remote_history_error = Some(message);
                    }
                }
                this.refresh_visible_thumbnails(cx);
                cx.notify();
            })
            .ok();
        }));
        self.refresh_visible_thumbnails(cx);
        cx.notify();
    }

    fn cloud_library(&self) -> Option<&CloudLibraryData> {
        match &self.cloud_library_state {
            CloudLibraryViewState::Loaded(library) => Some(library),
            _ => None,
        }
    }

    fn cloud_video_liked(&self, video_id: &str) -> bool {
        self.cloud_library()
            .is_some_and(|library| library.video_liked(video_id))
    }

    fn cloud_playlist_liked(&self, playlist_id: &str) -> bool {
        self.cloud_library()
            .is_some_and(|library| library.playlist_liked(playlist_id))
    }

    fn cloud_album_liked(&self, browse_id: &str) -> bool {
        self.cloud_library()
            .is_some_and(|library| library.album_liked(browse_id))
    }

    fn cloud_artist_subscribed(&self, channel_id: &str) -> bool {
        self.cloud_library()
            .is_some_and(|library| library.artist_subscribed(channel_id))
    }

    fn set_cloud_video_liked(&mut self, song: Song, liked: bool, cx: &mut Context<Self>) {
        if self.cloud_busy() || !self.account_ready() {
            return;
        }
        let previous = match &self.cloud_library_state {
            CloudLibraryViewState::Loaded(library) => Some(library.clone()),
            _ => None,
        };
        if let CloudLibraryViewState::Loaded(library) = &mut self.cloud_library_state {
            library.set_video_liked(song.clone(), liked);
        }
        self.cloud_library_error = None;
        self.cloud_library_operation = CloudLibraryOperation::SettingVideoLike;
        let client = self.search_client.clone();
        self.cloud_mutation_task = Some(cx.spawn(async move |this, cx| {
            let result = client.set_video_liked(&song.video_id, liked).await;
            this.update(cx, |this, cx| {
                this.cloud_library_operation = CloudLibraryOperation::Idle;
                match result {
                    Ok(()) => {
                        this.cloud_library_error = None;
                        if previous.is_none() {
                            this.reload_cloud_library(cx);
                        }
                    }
                    Err(error) => {
                        if let Some(previous) = previous {
                            this.cloud_library_state = CloudLibraryViewState::Loaded(previous);
                        }
                        this.record_cloud_error(error);
                    }
                }
                this.refresh_visible_thumbnails(cx);
                cx.notify();
            })
            .ok();
        }));
        self.refresh_visible_thumbnails(cx);
        cx.notify();
    }

    fn set_cloud_playlist_liked(&mut self, item: BrowseItem, liked: bool, cx: &mut Context<Self>) {
        if self.cloud_busy() || !self.account_ready() || item.editable {
            return;
        }
        let previous = match &self.cloud_library_state {
            CloudLibraryViewState::Loaded(library) => Some(library.clone()),
            _ => None,
        };
        if let CloudLibraryViewState::Loaded(library) = &mut self.cloud_library_state {
            library.set_playlist_liked(item.clone(), liked);
        }
        self.cloud_library_error = None;
        self.cloud_library_operation = CloudLibraryOperation::SettingPlaylistLike;
        let client = self.search_client.clone();
        let playlist_id = item.browse_id.clone();
        self.cloud_mutation_task = Some(cx.spawn(async move |this, cx| {
            let result = client.set_playlist_liked(&playlist_id, liked).await;
            this.update(cx, |this, cx| {
                this.cloud_library_operation = CloudLibraryOperation::Idle;
                match result {
                    Ok(()) => {
                        this.cloud_library_error = None;
                        if previous.is_none() {
                            this.reload_cloud_library(cx);
                        }
                    }
                    Err(error) => {
                        if let Some(previous) = previous {
                            this.cloud_library_state = CloudLibraryViewState::Loaded(previous);
                        }
                        this.record_cloud_error(error);
                    }
                }
                this.refresh_visible_thumbnails(cx);
                cx.notify();
            })
            .ok();
        }));
        self.refresh_visible_thumbnails(cx);
        cx.notify();
    }

    fn set_cloud_album_liked(
        &mut self,
        item: BrowseItem,
        playlist_id: String,
        liked: bool,
        cx: &mut Context<Self>,
    ) {
        if self.cloud_busy() || !self.account_ready() || item.kind != BrowseKind::Album {
            return;
        }
        let previous = match &self.cloud_library_state {
            CloudLibraryViewState::Loaded(library) => Some(library.clone()),
            _ => None,
        };
        if let CloudLibraryViewState::Loaded(library) = &mut self.cloud_library_state {
            library.set_album_liked(item.clone(), liked);
        }
        self.cloud_library_error = None;
        self.cloud_library_operation = CloudLibraryOperation::SettingAlbumLike;
        let client = self.search_client.clone();
        self.cloud_mutation_task = Some(cx.spawn(async move |this, cx| {
            let result = client.set_playlist_liked(&playlist_id, liked).await;
            this.update(cx, |this, cx| {
                this.cloud_library_operation = CloudLibraryOperation::Idle;
                match result {
                    Ok(()) => {
                        this.cloud_library_error = None;
                        if previous.is_none() {
                            this.reload_cloud_library(cx);
                        }
                    }
                    Err(error) => {
                        if let Some(previous) = previous {
                            this.cloud_library_state = CloudLibraryViewState::Loaded(previous);
                        }
                        this.record_cloud_error(error);
                    }
                }
                this.refresh_visible_thumbnails(cx);
                cx.notify();
            })
            .ok();
        }));
        self.refresh_visible_thumbnails(cx);
        cx.notify();
    }

    fn set_cloud_subscription(
        &mut self,
        channel_id: String,
        subscribed: bool,
        cx: &mut Context<Self>,
    ) {
        if self.cloud_busy() || !self.account_ready() {
            return;
        }
        let artist_item = self
            .browse_state
            .as_ref()
            .and_then(|state| match state {
                BrowseViewState::Loaded(page) if page.item.kind == BrowseKind::Artist => {
                    Some(BrowseItem {
                        browse_id: channel_id.clone(),
                        kind: BrowseKind::Artist,
                        title: page.item.title.clone(),
                        subtitle: page.item.subtitle.clone(),
                        thumbnail_url: page.item.thumbnail_url.clone(),
                        params: page.item.params.clone(),
                        editable: false,
                    })
                }
                BrowseViewState::Loading(_)
                | BrowseViewState::Loaded(_)
                | BrowseViewState::Failed(_, _) => None,
            })
            .or_else(|| {
                self.current_song.as_ref().and_then(|song| {
                    song.artists
                        .iter()
                        .find(|artist| artist.id.as_deref() == Some(channel_id.as_str()))
                        .map(|artist| BrowseItem {
                            browse_id: channel_id.clone(),
                            kind: BrowseKind::Artist,
                            title: artist.name.clone(),
                            subtitle: "Subscribed artist".into(),
                            thumbnail_url: song.thumbnail_url.clone(),
                            params: None,
                            editable: false,
                        })
                })
            });
        let previous_page = match &self.browse_state {
            Some(BrowseViewState::Loaded(page)) => Some(page.clone()),
            _ => None,
        };
        let previous_library = match &self.cloud_library_state {
            CloudLibraryViewState::Loaded(library) => Some(library.clone()),
            _ => None,
        };
        if let Some(BrowseViewState::Loaded(page)) = &mut self.browse_state
            && let Some(subscription) = &mut page.channel_subscription
            && subscription.channel_id == channel_id
        {
            subscription.subscribed = subscribed;
        }
        if let (CloudLibraryViewState::Loaded(library), Some(item)) =
            (&mut self.cloud_library_state, artist_item)
        {
            library.set_artist_subscribed(item, subscribed);
        }
        self.cloud_library_error = None;
        self.cloud_library_operation = CloudLibraryOperation::SettingSubscription;
        let client = self.search_client.clone();
        self.cloud_mutation_task = Some(cx.spawn(async move |this, cx| {
            let result = client
                .set_channel_subscribed(&channel_id, subscribed, None)
                .await;
            this.update(cx, |this, cx| {
                this.cloud_library_operation = CloudLibraryOperation::Idle;
                match result {
                    Ok(()) => this.cloud_library_error = None,
                    Err(error) => {
                        if let Some(previous) = previous_page {
                            this.browse_state = Some(BrowseViewState::Loaded(previous));
                        }
                        if let Some(previous) = previous_library {
                            this.cloud_library_state = CloudLibraryViewState::Loaded(previous);
                        }
                        this.record_cloud_error(error);
                    }
                }
                this.refresh_visible_thumbnails(cx);
                cx.notify();
            })
            .ok();
        }));
        self.refresh_visible_thumbnails(cx);
        cx.notify();
    }

    fn open_cloud_playlist_picker(&mut self, song: Song, cx: &mut Context<Self>) {
        if !self.account_ready() {
            self.navigate(Route::Settings, cx);
            return;
        }
        self.playlist_picker_song = None;
        self.queue_visible = false;
        self.lyrics_visible = false;
        self.playback_parameters_visible = false;
        if matches!(self.lyrics_state, LyricsViewState::Loading(_)) {
            self.lyrics_state = LyricsViewState::Idle;
        }
        self.lyrics_task = None;
        self.cloud_playlist_picker_song = Some(song);
        cx.notify();
    }

    fn add_song_to_cloud_playlist(
        &mut self,
        playlist: BrowseItem,
        song: Song,
        cx: &mut Context<Self>,
    ) {
        if self.cloud_busy() || !self.account_ready() || !playlist.editable {
            return;
        }
        self.cloud_playlist_picker_song = None;
        self.cloud_library_error = None;
        self.cloud_library_operation = CloudLibraryOperation::AddingToPlaylist;
        let client = self.search_client.clone();
        let playlist_id = playlist.browse_id.clone();
        let rollback_song = song.clone();
        self.cloud_mutation_task = Some(cx.spawn(async move |this, cx| {
            let result = client
                .add_video_to_playlist(&playlist_id, &song.video_id)
                .await;
            this.update(cx, |this, cx| {
                this.cloud_library_operation = CloudLibraryOperation::Idle;
                match result {
                    Ok(()) => {
                        this.cloud_library_error = None;
                        this.reload_cloud_library(cx);
                    }
                    Err(error) => {
                        this.cloud_playlist_picker_song = Some(rollback_song);
                        this.record_cloud_error(error);
                    }
                }
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    fn remove_song_from_cloud_playlist(
        &mut self,
        playlist_id: String,
        entry: PlaylistEntry,
        cx: &mut Context<Self>,
    ) {
        if self.cloud_busy() || !self.account_ready() {
            return;
        }
        let previous = match &self.browse_state {
            Some(BrowseViewState::Loaded(page)) if page.item.editable => Some(page.clone()),
            _ => None,
        };
        let Some(previous) = previous else {
            self.cloud_library_error = Some(
                "This playlist response did not include an editable track identity. Refresh and try again."
                    .into(),
            );
            cx.notify();
            return;
        };
        if let Some(BrowseViewState::Loaded(page)) = &mut self.browse_state {
            page.playlist_entries
                .retain(|current| current.set_video_id != entry.set_video_id);
            page.songs = page
                .playlist_entries
                .iter()
                .map(|current| current.song.clone())
                .collect();
        }
        self.cloud_library_error = None;
        self.cloud_library_operation = CloudLibraryOperation::RemovingFromPlaylist;
        let client = self.search_client.clone();
        self.cloud_mutation_task = Some(cx.spawn(async move |this, cx| {
            let result = client
                .remove_video_from_playlist(&playlist_id, &entry.song.video_id, &entry.set_video_id)
                .await;
            this.update(cx, |this, cx| {
                this.cloud_library_operation = CloudLibraryOperation::Idle;
                match result {
                    Ok(()) => this.cloud_library_error = None,
                    Err(error) => {
                        this.browse_state = Some(BrowseViewState::Loaded(previous));
                        this.record_cloud_error(error);
                    }
                }
                this.refresh_visible_thumbnails(cx);
                cx.notify();
            })
            .ok();
        }));
        self.refresh_visible_thumbnails(cx);
        cx.notify();
    }

    fn open_create_cloud_playlist_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.cloud_busy() || !self.account_ready() {
            return;
        }
        self.playlist_rename_input
            .update(cx, |input, cx| input.set_value("", window, cx));
        let input = self.playlist_rename_input.clone();
        let weak = cx.weak_entity();
        window.open_alert_dialog(cx, move |dialog, window, cx| {
            input.update(cx, |input, cx| input.focus(window, cx));
            let input_for_submit = input.clone();
            let weak = weak.clone();
            dialog
                .title("Create YouTube Music playlist")
                .description("The playlist will be private. It is created remotely and is not copied into local playlists.")
                .child(Input::new(&input))
                .button_props(
                    DialogButtonProps::default()
                        .ok_text("Create")
                        .cancel_text("Cancel")
                        .show_cancel(true),
                )
                .on_ok(move |_, window, cx| {
                    let title = input_for_submit.read(cx).value().trim().to_owned();
                    if title.is_empty() {
                        window.push_notification("Playlist name cannot be empty.", cx);
                        return false;
                    }
                    weak.update(cx, |this, cx| {
                        this.create_cloud_playlist(title.clone(), cx);
                    })
                    .ok();
                    true
                })
        });
    }

    fn create_cloud_playlist(&mut self, title: String, cx: &mut Context<Self>) {
        if self.cloud_busy() || !self.account_ready() {
            return;
        }
        self.cloud_library_error = None;
        self.cloud_library_operation = CloudLibraryOperation::CreatingPlaylist;
        let client = self.search_client.clone();
        self.cloud_mutation_task = Some(cx.spawn(async move |this, cx| {
            let result = client.create_playlist(&title).await;
            this.update(cx, |this, cx| {
                this.cloud_library_operation = CloudLibraryOperation::Idle;
                match result {
                    Ok(_) => {
                        this.cloud_library_error = None;
                        this.reload_cloud_library(cx);
                    }
                    Err(error) => this.record_cloud_error(error),
                }
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    fn open_rename_cloud_playlist_dialog(
        &mut self,
        playlist: BrowseItem,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.cloud_busy() || !self.account_ready() || !playlist.editable {
            return;
        }
        self.playlist_rename_input.update(cx, |input, cx| {
            input.set_value(&playlist.title, window, cx);
        });
        let input = self.playlist_rename_input.clone();
        let weak = cx.weak_entity();
        window.open_alert_dialog(cx, move |dialog, window, cx| {
            input.update(cx, |input, cx| input.focus(window, cx));
            let input_for_submit = input.clone();
            let weak = weak.clone();
            let playlist = playlist.clone();
            dialog
                .title("Rename YouTube Music playlist")
                .description("This changes the remote playlist name.")
                .child(Input::new(&input))
                .button_props(
                    DialogButtonProps::default()
                        .ok_text("Rename")
                        .cancel_text("Cancel")
                        .show_cancel(true),
                )
                .on_ok(move |_, window, cx| {
                    let title = input_for_submit.read(cx).value().trim().to_owned();
                    if title.is_empty() {
                        window.push_notification("Playlist name cannot be empty.", cx);
                        return false;
                    }
                    weak.update(cx, |this, cx| {
                        this.rename_cloud_playlist(playlist.clone(), title.clone(), cx);
                    })
                    .ok();
                    true
                })
        });
    }

    fn rename_cloud_playlist(
        &mut self,
        playlist: BrowseItem,
        title: String,
        cx: &mut Context<Self>,
    ) {
        if self.cloud_busy() || !self.account_ready() || !playlist.editable {
            return;
        }
        let previous_library = match &self.cloud_library_state {
            CloudLibraryViewState::Loaded(library) => Some(library.clone()),
            _ => None,
        };
        let previous_browse = match &self.browse_state {
            Some(BrowseViewState::Loaded(page))
                if ui_playlist_id(&page.item.browse_id) == ui_playlist_id(&playlist.browse_id) =>
            {
                Some(page.clone())
            }
            _ => None,
        };
        if let CloudLibraryViewState::Loaded(library) = &mut self.cloud_library_state
            && let Some(item) = library
                .playlists
                .iter_mut()
                .find(|item| ui_playlist_id(&item.browse_id) == ui_playlist_id(&playlist.browse_id))
        {
            item.title.clone_from(&title);
        }
        if let Some(BrowseViewState::Loaded(page)) = &mut self.browse_state
            && ui_playlist_id(&page.item.browse_id) == ui_playlist_id(&playlist.browse_id)
        {
            page.item.title.clone_from(&title);
        }
        self.cloud_library_error = None;
        self.cloud_library_operation = CloudLibraryOperation::RenamingPlaylist;
        let client = self.search_client.clone();
        let playlist_id = playlist.browse_id.clone();
        self.cloud_mutation_task = Some(cx.spawn(async move |this, cx| {
            let result = client.rename_playlist(&playlist_id, &title).await;
            this.update(cx, |this, cx| {
                this.cloud_library_operation = CloudLibraryOperation::Idle;
                match result {
                    Ok(()) => {
                        this.cloud_library_error = None;
                        this.reload_cloud_library(cx);
                    }
                    Err(error) => {
                        if let Some(previous) = previous_library {
                            this.cloud_library_state = CloudLibraryViewState::Loaded(previous);
                        }
                        if let Some(previous) = previous_browse {
                            this.browse_state = Some(BrowseViewState::Loaded(previous));
                        }
                        this.record_cloud_error(error);
                    }
                }
                this.refresh_visible_thumbnails(cx);
                cx.notify();
            })
            .ok();
        }));
        self.refresh_visible_thumbnails(cx);
        cx.notify();
    }

    fn confirm_delete_cloud_playlist(
        &mut self,
        playlist: BrowseItem,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.cloud_busy() || !self.account_ready() || !playlist.editable {
            return;
        }
        let weak = cx.weak_entity();
        window.open_alert_dialog(cx, move |dialog, _, _| {
            let weak = weak.clone();
            let playlist = playlist.clone();
            dialog
                .title("Delete YouTube Music playlist?")
                .description(format!(
                    "\"{}\" will be deleted remotely. Local favourites, playlists, history, and cache are not affected.",
                    playlist.title
                ))
                .button_props(
                    DialogButtonProps::default()
                        .ok_text("Delete remotely")
                        .ok_variant(ButtonVariant::Danger)
                        .cancel_text("Cancel")
                        .show_cancel(true),
                )
                .on_ok(move |_, _, cx| {
                    weak.update(cx, |this, cx| {
                        this.delete_cloud_playlist(playlist.clone(), cx);
                    })
                    .ok();
                    true
                })
        });
    }

    fn delete_cloud_playlist(&mut self, playlist: BrowseItem, cx: &mut Context<Self>) {
        if self.cloud_busy() || !self.account_ready() || !playlist.editable {
            return;
        }
        let previous_library = match &self.cloud_library_state {
            CloudLibraryViewState::Loaded(library) => Some(library.clone()),
            _ => None,
        };
        let previous_browse = match &self.browse_state {
            Some(BrowseViewState::Loaded(page))
                if ui_playlist_id(&page.item.browse_id) == ui_playlist_id(&playlist.browse_id) =>
            {
                Some(page.clone())
            }
            _ => None,
        };
        if let CloudLibraryViewState::Loaded(library) = &mut self.cloud_library_state {
            library.playlists.retain(|item| {
                ui_playlist_id(&item.browse_id) != ui_playlist_id(&playlist.browse_id)
            });
        }
        if previous_browse.is_some() {
            self.browse_state = None;
        }
        self.cloud_library_error = None;
        self.cloud_library_operation = CloudLibraryOperation::DeletingPlaylist;
        let client = self.search_client.clone();
        let playlist_id = playlist.browse_id.clone();
        self.cloud_mutation_task = Some(cx.spawn(async move |this, cx| {
            let result = client.delete_playlist(&playlist_id).await;
            this.update(cx, |this, cx| {
                this.cloud_library_operation = CloudLibraryOperation::Idle;
                match result {
                    Ok(()) => {
                        this.cloud_library_error = None;
                        this.reload_cloud_library(cx);
                    }
                    Err(error) => {
                        if let Some(previous) = previous_library {
                            this.cloud_library_state = CloudLibraryViewState::Loaded(previous);
                        }
                        if let Some(previous) = previous_browse {
                            this.browse_state = Some(BrowseViewState::Loaded(previous));
                        }
                        this.record_cloud_error(error);
                    }
                }
                this.refresh_visible_thumbnails(cx);
                cx.notify();
            })
            .ok();
        }));
        self.refresh_visible_thumbnails(cx);
        cx.notify();
    }

    fn activate_account_session(
        &mut self,
        profile: AccountProfile,
        auth: AuthSession,
        client: Arc<InnerTubeClient>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.reset_radio_queue();
        self.search_task = None;
        self.browse_task = None;
        self.home_task = None;
        self.explore_task = None;
        self.search_client = client;
        self.auth_session = Some(auth);
        self.account_state = AccountViewState::SignedIn(profile);
        self.account_error = None;
        self.credential_warning = None;
        self.cloud_mutation_task = None;
        self.cloud_library_operation = CloudLibraryOperation::Idle;
        self.cloud_library_error = None;
        self.cloud_playlist_picker_song = None;
        self.remote_history_operation = RemoteHistoryOperation::Idle;
        self.remote_history_error = None;
        self.remote_history_task = None;
        self.browse_state = None;
        self.home_default_page = None;
        self.reload_home(None, None, cx);
        self.reload_explore(cx);
        if !self.model.search_query().trim().is_empty() {
            self.run_search(window, cx);
        }
        self.reload_cloud_library(cx);
        self.reload_remote_history(cx);
        self.sync_podcast_library(cx);
    }

    fn sign_in_account(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.account_busy() {
            return;
        }
        let previous_state = self.account_state.clone();
        self.account_operation = AccountOperation::SigningIn;
        self.account_state = AccountViewState::Checking;
        self.account_error = None;

        let settings = self.settings.clone();
        let proxy = settings.proxy.clone();
        let credential_store = self.credential_store.clone();
        let executor = cx.background_executor().clone();
        let operation = executor.clone().spawn(async move {
            let Some(auth) = launch_account_login(proxy, executor).await? else {
                return Ok::<_, crate::AppError>(None);
            };
            let client = Arc::new(InnerTubeClient::with_settings(
                InnerTubeSession::default().with_auth(Some(auth.clone())),
                &settings,
            )?);
            let profile = client.account_info().await?;
            credential_store.save(&auth)?;
            Ok(Some((profile, auth, client)))
        });
        self.account_task = Some(cx.spawn_in(window, async move |this, cx| {
            let result = operation.await;
            this.update_in(cx, |this, window, cx| {
                this.account_operation = AccountOperation::Idle;
                match result {
                    Ok(Some((profile, auth, client))) => {
                        this.activate_account_session(profile, auth, client, window, cx);
                    }
                    Ok(None) => {
                        this.account_state = previous_state;
                    }
                    Err(error) => {
                        this.account_state = previous_state;
                        this.account_error = Some(error.to_string());
                    }
                }
                this.refresh_visible_thumbnails(cx);
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    fn import_account(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.account_busy() {
            return;
        }
        let previous_state = self.account_state.clone();
        let imported = Zeroizing::new(self.account_cookie_input.read(cx).value().to_string());
        self.account_cookie_input
            .update(cx, |input, cx| input.set_value("", window, cx));
        let auth = match AuthSession::from_import(&imported) {
            Ok(auth) => auth,
            Err(error) => {
                self.account_error = Some(error.to_string());
                cx.notify();
                return;
            }
        };
        let client = match InnerTubeClient::with_settings(
            InnerTubeSession::default().with_auth(Some(auth.clone())),
            &self.settings,
        ) {
            Ok(client) => Arc::new(client),
            Err(error) => {
                self.account_error = Some(error.to_string());
                cx.notify();
                return;
            }
        };

        self.account_operation = AccountOperation::Importing;
        self.account_state = AccountViewState::Checking;
        self.account_error = None;
        let credential_store = self.credential_store.clone();
        let verified_client = client.clone();
        let saved_auth = auth.clone();
        let operation = cx.background_executor().spawn(async move {
            let profile = verified_client.account_info().await?;
            credential_store.save(&saved_auth)?;
            Ok::<_, crate::AppError>((profile, saved_auth))
        });
        self.account_task = Some(cx.spawn_in(window, async move |this, cx| {
            let result = operation.await;
            this.update_in(cx, |this, window, cx| {
                this.account_operation = AccountOperation::Idle;
                match result {
                    Ok((profile, auth)) => {
                        this.activate_account_session(profile, auth, client, window, cx);
                    }
                    Err(error) => {
                        this.account_state = previous_state;
                        this.account_error = Some(error.to_string());
                    }
                }
                this.refresh_visible_thumbnails(cx);
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    fn sign_out_account(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.account_busy() || self.auth_session.is_none() {
            return;
        }
        let anonymous =
            match InnerTubeClient::with_settings(InnerTubeSession::default(), &self.settings) {
                Ok(client) => Arc::new(client),
                Err(error) => {
                    self.account_state = AccountViewState::Failed(error.to_string());
                    cx.notify();
                    return;
                }
            };
        self.account_operation = AccountOperation::SigningOut;
        self.account_error = None;
        let credential_store = self.credential_store.clone();
        let operation = cx
            .background_executor()
            .spawn(async move { credential_store.delete() });
        self.account_task = Some(cx.spawn_in(window, async move |this, cx| {
            let result = operation.await;
            this.update_in(cx, |this, window, cx| {
                this.account_operation = AccountOperation::Idle;
                match result {
                    Ok(()) => {
                        this.reset_radio_queue();
                        this.search_task = None;
                        this.browse_task = None;
                        this.home_task = None;
                        this.explore_task = None;
                        this.search_client = anonymous;
                        this.auth_session = None;
                        this.account_state = AccountViewState::SignedOut;
                        this.account_error = None;
                        this.cloud_library_task = None;
                        this.cloud_library_state = CloudLibraryViewState::SignedOut;
                        this.cloud_mutation_task = None;
                        this.cloud_library_operation = CloudLibraryOperation::Idle;
                        this.cloud_library_error = None;
                        this.cloud_playlist_picker_song = None;
                        this.remote_history_task = None;
                        this.remote_history_state = RemoteHistoryViewState::SignedOut;
                        this.remote_history_source = HistorySource::Local;
                        this.remote_history_operation = RemoteHistoryOperation::Idle;
                        this.remote_history_error = None;
                        this.credential_warning = None;
                        this.browse_state = None;
                        this.home_default_page = None;
                        this.reload_home(None, None, cx);
                        this.reload_explore(cx);
                        if !this.model.search_query().trim().is_empty() {
                            this.run_search(window, cx);
                        }
                    }
                    Err(error) => {
                        this.account_state = AccountViewState::Failed(error.to_string());
                    }
                }
                this.refresh_visible_thumbnails(cx);
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    fn confirm_sign_out_account(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.account_busy() || self.auth_session.is_none() {
            return;
        }
        let weak = cx.weak_entity();
        window.open_alert_dialog(cx, move |dialog, _, _| {
            let weak = weak.clone();
            dialog
                .title("Sign out of YouTube Music?")
                .description(
                    "The imported session will be removed from the system credential store. Local favourites, playlists, history, and cached audio will be kept.",
                )
                .button_props(
                    DialogButtonProps::default()
                        .ok_text("Sign out")
                        .ok_variant(ButtonVariant::Danger)
                        .cancel_text("Cancel")
                        .show_cancel(true),
                )
                .on_ok(move |_, window, cx| {
                    weak.update(cx, |this, cx| {
                        this.sign_out_account(window, cx);
                    })
                    .ok();
                    true
                })
        });
    }

    fn lastfm_busy(&self) -> bool {
        self.lastfm_operation != LastFmOperation::Idle
            || self.settings_operation == SettingsOperation::Applying
    }

    fn sign_in_lastfm(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.lastfm_busy() {
            return;
        }
        let Some(client) = self.lastfm_client.clone() else {
            self.lastfm_error = Some(
                "Configure LASTFM_API_KEY and LASTFM_SHARED_SECRET (or LASTFM_SECRET) before signing in."
                    .into(),
            );
            cx.notify();
            return;
        };
        let username = self
            .lastfm_username_input
            .read(cx)
            .value()
            .trim()
            .to_owned();
        let password = Zeroizing::new(self.lastfm_password_input.read(cx).value().to_string());
        self.lastfm_password_input
            .update(cx, |input, cx| input.set_value("", window, cx));
        if username.is_empty() || password.is_empty() {
            self.lastfm_error = Some("Enter both a Last.fm username and password.".into());
            cx.notify();
            return;
        }

        self.lastfm_operation = LastFmOperation::SigningIn;
        self.lastfm_error = None;
        self.lastfm_notice = None;
        let credential_store = self.lastfm_credential_store.clone();
        let login_client = client.clone();
        let operation = cx.background_executor().spawn(async move {
            let session = login_client.login(&username, &password).await?;
            credential_store.save(&session)?;
            Ok::<_, AppError>(session)
        });
        self.lastfm_task = Some(cx.spawn_in(window, async move |this, cx| {
            let result = operation.await;
            this.update_in(cx, |this, window, cx| {
                this.lastfm_operation = LastFmOperation::Idle;
                this.lastfm_task = None;
                match result {
                    Ok(session) => {
                        this.lastfm_username_input.update(cx, |input, cx| {
                            input.set_value(session.username(), window, cx)
                        });
                        this.lastfm_client = Some(client.with_session(Some(session.clone())));
                        this.lastfm_session = Some(session);
                        this.lastfm_warning = None;
                        this.lastfm_error = None;
                        this.lastfm_notice = Some("Last.fm account signed in securely.".into());
                        this.lastfm_playback_tracker.reset();
                    }
                    Err(error) => {
                        this.lastfm_error = Some(error.to_string());
                    }
                }
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    fn sign_out_lastfm(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.lastfm_busy() || self.lastfm_session.is_none() {
            return;
        }
        self.lastfm_operation = LastFmOperation::SigningOut;
        self.lastfm_error = None;
        self.lastfm_notice = None;
        let credential_store = self.lastfm_credential_store.clone();
        let operation = cx
            .background_executor()
            .spawn(async move { credential_store.delete() });
        self.lastfm_task = Some(cx.spawn_in(window, async move |this, cx| {
            let result = operation.await;
            this.update_in(cx, |this, window, cx| {
                this.lastfm_operation = LastFmOperation::Idle;
                this.lastfm_task = None;
                match result {
                    Ok(()) => {
                        this.lastfm_session = None;
                        this.lastfm_client = this
                            .lastfm_client
                            .as_ref()
                            .map(|client| client.with_session(None));
                        this.lastfm_username_input
                            .update(cx, |input, cx| input.set_value("", window, cx));
                        this.lastfm_playback_tracker.reset();
                        this.lastfm_warning = None;
                        this.lastfm_notice = Some(
                            "Last.fm session removed; local history and favourites were kept."
                                .into(),
                        );
                    }
                    Err(error) => this.lastfm_error = Some(error.to_string()),
                }
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    fn toggle_playback_parameters_panel(&mut self, cx: &mut Context<Self>) {
        self.playback_parameters_visible = !self.playback_parameters_visible;
        if self.playback_parameters_visible {
            self.queue_visible = false;
            self.lyrics_visible = false;
            self.playlist_picker_song = None;
            self.cloud_playlist_picker_song = None;
            if matches!(self.lyrics_state, LyricsViewState::Loading(_)) {
                self.lyrics_state = LyricsViewState::Idle;
            }
            self.lyrics_task = None;
        }
        cx.notify();
    }

    fn request_playback_parameter_change(
        &mut self,
        change: PlaybackParameterChange,
        cx: &mut Context<Self>,
    ) {
        if self.playback_parameters_pending.is_some()
            || self.settings_operation == SettingsOperation::Applying
            || self.equalizer_operation != EqualizerOperation::Idle
        {
            return;
        }
        if self.listen_together_snapshot.room.is_some() {
            self.playback_parameters_error = Some(
                "Leave the Listen Together room before changing speed or pitch; the room protocol does not synchronize these parameters."
                    .into(),
            );
            self.playback_parameters_notice = None;
            cx.notify();
            return;
        }
        if self.current_song.is_none() {
            self.playback_parameters_error =
                Some("Choose a song before changing live playback parameters.".into());
            self.playback_parameters_notice = None;
            cx.notify();
            return;
        }

        let previous = self.settings.playback_parameters;
        let parameters = adjusted_playback_parameters(previous, change);
        if parameters == previous {
            return;
        }
        if let Err(error) = parameters.validate() {
            self.playback_parameters_error = Some(error.to_string());
            self.playback_parameters_notice = None;
            cx.notify();
            return;
        }

        let mut next_settings = self.settings.clone();
        next_settings.playback_parameters = parameters;
        let store = self.store.clone();
        let audio_control = self.audio_player.parameter_control();
        self.playback_parameters_pending = Some(parameters);
        self.playback_parameters_error = None;
        self.playback_parameters_notice = None;
        let operation = cx.background_executor().spawn(async move {
            audio_control.set_playback_parameters(parameters)?;
            if let Err(storage_error) = store.save_settings(next_settings).await {
                let storage_message = storage_error.to_string();
                return match audio_control.set_playback_parameters(previous) {
                    Ok(()) => Err(AppError::Storage(format!(
                        "live playback settings were not saved; the previous audio parameters were restored: {storage_message}"
                    ))),
                    Err(rollback_error) => Err(AppError::Storage(format!(
                        "live playback settings were not saved and audio rollback also failed; restart to restore persisted parameters: {storage_message}; {rollback_error}"
                    ))),
                };
            }
            Ok::<_, AppError>(parameters)
        });
        self.playback_parameters_task = Some(cx.spawn(async move |this, cx| {
            let result = operation.await;
            this.update(cx, |this, cx| {
                this.playback_parameters_pending = None;
                this.playback_parameters_task = None;
                match result {
                    Ok(parameters) => {
                        this.settings.playback_parameters = parameters;
                        this.settings_draft.playback_parameters = parameters;
                        this.playback_parameters_error = None;
                        this.playback_parameters_notice = Some(format!(
                            "Applied and saved {}.",
                            format_playback_parameters(parameters)
                        ));
                    }
                    Err(error) => {
                        this.playback_parameters_error = Some(error.to_string());
                        this.playback_parameters_notice = None;
                    }
                }
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    fn install_auto_eq_index(&mut self, index: AutoEqIndex, cx: &mut Context<Self>) {
        self.autoeq_database_state = AutoEqDatabaseState::Ready(Arc::new(index));
        self.autoeq_wizard_step = AutoEqWizardStep::ModelSelection;
        self.autoeq_selected_model = None;
        self.autoeq_variants.clear();
        self.autoeq_selected_variant_paths.clear();
        self.refresh_auto_eq_models(cx);
    }

    fn download_auto_eq_database(&mut self, cx: &mut Context<Self>) {
        if self.equalizer_operation != EqualizerOperation::Idle
            || self.settings_operation == SettingsOperation::Applying
            || self.playback_parameters_pending.is_some()
        {
            return;
        }
        let client = match AutoEqClient::with_settings(&self.settings) {
            Ok(client) => client,
            Err(error) => {
                self.autoeq_database_state = AutoEqDatabaseState::Failed {
                    message: error.to_string(),
                    cached: false,
                };
                cx.notify();
                return;
            }
        };
        let cached = client.is_database_cached();
        self.autoeq_database_state = AutoEqDatabaseState::Loading;
        self.autoeq_wizard_step = AutoEqWizardStep::ModelSelection;
        self.autoeq_models.clear();
        self.autoeq_selected_model = None;
        self.autoeq_variants.clear();
        self.autoeq_selected_variant_paths.clear();
        self.equalizer_operation = EqualizerOperation::LoadingDatabase;
        self.equalizer_error = None;
        self.equalizer_notice = None;
        let operation = cx
            .background_executor()
            .spawn(async move { client.build_index().await });
        self.equalizer_task = Some(cx.spawn(async move |this, cx| {
            let result = operation.await;
            this.update(cx, |this, cx| {
                this.equalizer_operation = EqualizerOperation::Idle;
                this.equalizer_task = None;
                match result {
                    Ok(index) => this.install_auto_eq_index(index, cx),
                    Err(error) => {
                        this.autoeq_database_state = AutoEqDatabaseState::Failed {
                            message: error.to_string(),
                            cached,
                        };
                    }
                }
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    fn refresh_auto_eq_models(&mut self, cx: &mut Context<Self>) {
        let AutoEqDatabaseState::Ready(index) = &self.autoeq_database_state else {
            return;
        };
        let query = self.autoeq_search_input.read(cx).value();
        self.autoeq_models = search_auto_eq_models(index, &query, MAX_AUTO_EQ_SEARCH_RESULTS);
        self.autoeq_search_task = None;
        cx.notify();
    }

    fn schedule_auto_eq_search(&mut self, cx: &mut Context<Self>) {
        if !matches!(self.autoeq_database_state, AutoEqDatabaseState::Ready(_))
            || self.autoeq_wizard_step != AutoEqWizardStep::ModelSelection
        {
            return;
        }
        self.autoeq_search_generation = self.autoeq_search_generation.wrapping_add(1);
        let generation = self.autoeq_search_generation;
        self.autoeq_search_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(300))
                .await;
            this.update(cx, |this, cx| {
                if this.autoeq_search_generation == generation {
                    this.refresh_auto_eq_models(cx);
                }
            })
            .ok();
        }));
    }

    fn select_auto_eq_model(&mut self, model: AutoEqModel, cx: &mut Context<Self>) {
        if self.equalizer_operation != EqualizerOperation::Idle
            || !matches!(self.autoeq_database_state, AutoEqDatabaseState::Ready(_))
        {
            return;
        }
        let client = match AutoEqClient::with_settings(&self.settings) {
            Ok(client) => client,
            Err(error) => {
                self.equalizer_error = Some(error.to_string());
                cx.notify();
                return;
            }
        };
        self.equalizer_operation = EqualizerOperation::LoadingVariants;
        self.equalizer_error = None;
        self.equalizer_notice = None;
        self.autoeq_selected_model = Some(model.name.clone());
        self.autoeq_selected_variant_paths.clear();
        let operation = cx
            .background_executor()
            .spawn(async move { client.resolve_variant_rigs(model.variants).await });
        self.equalizer_task = Some(cx.spawn(async move |this, cx| {
            let variants = operation.await;
            this.update(cx, |this, cx| {
                this.equalizer_operation = EqualizerOperation::Idle;
                this.equalizer_task = None;
                this.autoeq_variants = variants;
                this.autoeq_wizard_step = AutoEqWizardStep::VariantSelection;
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    fn back_to_auto_eq_models(&mut self, cx: &mut Context<Self>) {
        if self.equalizer_operation != EqualizerOperation::Idle {
            return;
        }
        self.autoeq_wizard_step = AutoEqWizardStep::ModelSelection;
        self.autoeq_selected_model = None;
        self.autoeq_variants.clear();
        self.autoeq_selected_variant_paths.clear();
        self.refresh_auto_eq_models(cx);
    }

    fn toggle_auto_eq_variant(&mut self, repo_path: String, cx: &mut Context<Self>) {
        if self.equalizer_operation != EqualizerOperation::Idle
            || !self
                .autoeq_variants
                .iter()
                .any(|variant| variant.repo_path == repo_path)
        {
            return;
        }
        if !self.autoeq_selected_variant_paths.remove(&repo_path) {
            self.autoeq_selected_variant_paths.insert(repo_path);
        }
        cx.notify();
    }

    fn save_selected_auto_eq_profiles(&mut self, cx: &mut Context<Self>) {
        if self.equalizer_operation != EqualizerOperation::Idle {
            return;
        }
        let selected = self
            .autoeq_variants
            .iter()
            .filter(|variant| {
                self.autoeq_selected_variant_paths
                    .contains(&variant.repo_path)
            })
            .cloned()
            .collect::<Vec<_>>();
        if selected.is_empty() {
            return;
        }
        let active_profile_id = self
            .settings
            .equalizer
            .active_profile
            .as_ref()
            .map(|profile| profile.id.as_str());
        if selected
            .iter()
            .any(|entry| active_profile_id == Some(entry.profile_id().as_str()))
        {
            self.equalizer_error = Some(
                "Disable the active AutoEQ profile before refreshing it from the online database."
                    .into(),
            );
            self.equalizer_notice = None;
            cx.notify();
            return;
        }
        let client = match AutoEqClient::with_settings(&self.settings) {
            Ok(client) => client,
            Err(error) => {
                self.equalizer_error = Some(error.to_string());
                cx.notify();
                return;
            }
        };
        let store = self.store.clone();
        self.equalizer_operation = EqualizerOperation::SavingAutoEq;
        self.equalizer_error = None;
        self.equalizer_notice = None;
        let operation = cx.background_executor().spawn(async move {
            let mut profiles = Vec::new();
            let mut failures = Vec::new();
            let added_at_ms = unix_time_ms();
            for (index, entry) in selected.into_iter().enumerate() {
                let result = async {
                    let equalizer = client.load_equalizer(&entry).await?;
                    entry
                        .clone()
                        .into_profile(equalizer, added_at_ms.saturating_add(index as i64))
                }
                .await;
                match result {
                    Ok(profile) => profiles.push(profile),
                    Err(error) => failures.push(format!("{}: {error}", entry.label)),
                }
            }
            if profiles.is_empty() {
                return Err(AppError::Network(format!(
                    "none of the selected AutoEQ profiles could be downloaded{}",
                    failures
                        .first()
                        .map(|failure| format!(": {failure}"))
                        .unwrap_or_default()
                )));
            }
            store.save_equalizer_profiles(profiles.clone()).await?;
            let saved_profiles = store.equalizer_profiles().await?;
            Ok::<_, AppError>((profiles.len(), failures.len(), saved_profiles))
        });
        self.equalizer_task = Some(cx.spawn(async move |this, cx| {
            let result = operation.await;
            this.update(cx, |this, cx| {
                this.equalizer_operation = EqualizerOperation::Idle;
                this.equalizer_task = None;
                match result {
                    Ok((saved, failed, profiles)) => {
                        this.equalizer_profiles = StoredViewState::Loaded(profiles);
                        this.autoeq_selected_variant_paths.clear();
                        this.equalizer_error = None;
                        this.equalizer_notice = Some(if failed == 0 {
                            format!("Saved {saved} AutoEQ profile(s). Select one below to apply it.")
                        } else {
                            format!("Saved {saved} AutoEQ profile(s); {failed} selected download(s) failed.")
                        });
                    }
                    Err(error) => {
                        this.equalizer_error = Some(error.to_string());
                        this.equalizer_notice = None;
                    }
                }
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    fn import_equalizer_profile(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.equalizer_operation != EqualizerOperation::Idle
            || self.settings_operation == SettingsOperation::Applying
            || self.playback_parameters_pending.is_some()
        {
            return;
        }

        let picker = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("Select an AutoEQ / Equalizer APO text profile".into()),
        });
        self.equalizer_operation = EqualizerOperation::Importing;
        self.equalizer_error = None;
        self.equalizer_notice = None;
        self.equalizer_delete_confirmation = None;
        let read_task = cx.background_executor().spawn(async move {
            let selected = picker
                .await
                .map_err(|error| {
                    AppError::InvalidConfig(format!("equalizer file picker closed: {error}"))
                })?
                .map_err(|error| {
                    AppError::InvalidConfig(format!(
                        "equalizer file picker could not be opened: {error}"
                    ))
                })?;
            let Some(path) = selected.and_then(|paths| paths.into_iter().next()) else {
                return Ok::<_, AppError>(None);
            };
            let metadata = std::fs::metadata(&path).map_err(|error| {
                AppError::InvalidConfig(format!(
                    "could not inspect equalizer profile '{}': {error}",
                    path.display()
                ))
            })?;
            if !metadata.is_file() {
                return Err(AppError::InvalidConfig(
                    "equalizer import requires a regular text file".into(),
                ));
            }
            if metadata.len() > MAX_EQUALIZER_APO_FILE_BYTES as u64 {
                return Err(AppError::InvalidConfig(format!(
                    "equalizer profile exceeds the {} KiB import limit",
                    MAX_EQUALIZER_APO_FILE_BYTES / 1024
                )));
            }
            let content = std::fs::read_to_string(&path).map_err(|error| {
                AppError::InvalidConfig(format!(
                    "could not read equalizer profile '{}': {error}",
                    path.display()
                ))
            })?;
            let equalizer = parse_equalizer_apo(&content)?;
            let name = path
                .file_stem()
                .and_then(|name| name.to_str())
                .map(str::trim)
                .filter(|name| !name.is_empty())
                .unwrap_or("Imported EQ")
                .to_owned();
            let added_at_ms = unix_time_ms();
            let profile = EqualizerProfile {
                id: format!("custom-{added_at_ms}-{:016x}", fastrand::u64(..u64::MAX)),
                name: name.clone(),
                device_model: name,
                equalizer,
                source: "Imported Equalizer APO".into(),
                rig: "unknown".into(),
                is_custom: true,
                added_at_ms,
            };
            profile.validate()?;
            Ok(Some(profile))
        });
        let store = self.store.clone();
        self.equalizer_task = Some(cx.spawn_in(window, async move |this, cx| {
            let result = match read_task.await {
                Ok(Some(profile)) => {
                    async {
                        store.save_equalizer_profile(profile.clone()).await?;
                        let profiles = store.equalizer_profiles().await?;
                        Ok::<_, AppError>(Some((profile, profiles)))
                    }
                    .await
                }
                Ok(None) => Ok(None),
                Err(error) => Err(error),
            };
            this.update_in(cx, |this, _, cx| {
                this.equalizer_operation = EqualizerOperation::Idle;
                this.equalizer_task = None;
                match result {
                    Ok(Some((profile, profiles))) => {
                        this.equalizer_profiles = StoredViewState::Loaded(profiles);
                        this.equalizer_notice = Some(format!(
                            "Imported {} with {} enabled bands. Select it to apply immediately.",
                            profile.name,
                            profile.equalizer.bands.len()
                        ));
                        this.equalizer_error = None;
                    }
                    Ok(None) => {}
                    Err(error) => {
                        this.equalizer_error = Some(error.to_string());
                        this.equalizer_notice = None;
                    }
                }
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    fn reload_equalizer_profiles(&mut self, cx: &mut Context<Self>) {
        if self.equalizer_operation != EqualizerOperation::Idle {
            return;
        }
        self.equalizer_operation = EqualizerOperation::Importing;
        self.equalizer_profiles = StoredViewState::Loading;
        self.equalizer_error = None;
        let store = self.store.clone();
        self.equalizer_task = Some(cx.spawn(async move |this, cx| {
            let result = store.equalizer_profiles().await;
            this.update(cx, |this, cx| {
                this.equalizer_operation = EqualizerOperation::Idle;
                this.equalizer_task = None;
                this.equalizer_profiles = match result {
                    Ok(profiles) => StoredViewState::Loaded(profiles),
                    Err(error) => StoredViewState::Failed(error.to_string()),
                };
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    fn request_equalizer_profile_change(
        &mut self,
        profile: Option<EqualizerProfile>,
        cx: &mut Context<Self>,
    ) {
        if self.equalizer_operation != EqualizerOperation::Idle
            || self.settings_operation == SettingsOperation::Applying
            || self.playback_parameters_pending.is_some()
        {
            return;
        }
        let previous = self.settings.equalizer.clone();
        let mut equalizer = previous.clone();
        equalizer.enabled = profile.is_some();
        equalizer.active_profile = profile.clone();
        if equalizer == previous {
            return;
        }
        if let Err(error) = equalizer.validate() {
            self.equalizer_error = Some(error.to_string());
            self.equalizer_notice = None;
            cx.notify();
            return;
        }

        let mut settings = self.settings.clone();
        settings.equalizer = equalizer.clone();
        let store = self.store.clone();
        let audio_control = self.audio_player.parameter_control();
        self.equalizer_operation = EqualizerOperation::Applying;
        self.equalizer_error = None;
        self.equalizer_notice = None;
        self.equalizer_delete_confirmation = None;
        let operation = cx.background_executor().spawn(async move {
            audio_control.set_equalizer(equalizer.clone())?;
            if let Err(storage_error) = store.save_settings(settings).await {
                let storage_message = storage_error.to_string();
                return match audio_control.set_equalizer(previous) {
                    Ok(()) => Err(AppError::Storage(format!(
                        "equalizer selection was not saved; the previous audio profile was restored: {storage_message}"
                    ))),
                    Err(rollback_error) => Err(AppError::Storage(format!(
                        "equalizer selection was not saved and audio rollback also failed; restart to restore persisted settings: {storage_message}; {rollback_error}"
                    ))),
                };
            }
            Ok::<_, AppError>(equalizer)
        });
        let selected_name = profile.map(|profile| profile.name);
        self.equalizer_task = Some(cx.spawn(async move |this, cx| {
            let result = operation.await;
            this.update(cx, |this, cx| {
                this.equalizer_operation = EqualizerOperation::Idle;
                this.equalizer_task = None;
                match result {
                    Ok(equalizer) => {
                        this.settings.equalizer = equalizer.clone();
                        this.settings_draft.equalizer = equalizer;
                        this.equalizer_notice = Some(selected_name.map_or_else(
                            || "Equalizer disabled and saved.".into(),
                            |name| format!("Applied and saved {name}."),
                        ));
                        this.equalizer_error = None;
                    }
                    Err(error) => {
                        this.equalizer_error = Some(error.to_string());
                        this.equalizer_notice = None;
                    }
                }
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    fn begin_equalizer_profile_delete(&mut self, profile_id: String, cx: &mut Context<Self>) {
        let active = self
            .settings
            .equalizer
            .active_profile
            .as_ref()
            .is_some_and(|profile| profile.id == profile_id);
        if active {
            self.equalizer_error = Some(
                "Disable the active profile before deleting it so playback and persisted state remain consistent."
                    .into(),
            );
            self.equalizer_notice = None;
        } else {
            self.equalizer_delete_confirmation = Some(profile_id);
            self.equalizer_error = None;
        }
        cx.notify();
    }

    fn delete_equalizer_profile(&mut self, profile_id: String, cx: &mut Context<Self>) {
        if self.equalizer_operation != EqualizerOperation::Idle
            || self.equalizer_delete_confirmation.as_deref() != Some(&profile_id)
        {
            return;
        }
        self.equalizer_operation = EqualizerOperation::Deleting;
        self.equalizer_error = None;
        self.equalizer_notice = None;
        let store = self.store.clone();
        self.equalizer_task = Some(cx.spawn(async move |this, cx| {
            let result = async {
                store.delete_equalizer_profile(profile_id).await?;
                store.equalizer_profiles().await
            }
            .await;
            this.update(cx, |this, cx| {
                this.equalizer_operation = EqualizerOperation::Idle;
                this.equalizer_task = None;
                this.equalizer_delete_confirmation = None;
                match result {
                    Ok(profiles) => {
                        this.equalizer_profiles = StoredViewState::Loaded(profiles);
                        this.equalizer_notice = Some("Equalizer profile deleted.".into());
                    }
                    Err(error) => this.equalizer_error = Some(error.to_string()),
                }
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    fn apply_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.settings_operation == SettingsOperation::Applying
            || self.playback_parameters_pending.is_some()
            || self.equalizer_operation != EqualizerOperation::Idle
            || self.account_operation != AccountOperation::Idle
            || self.lastfm_operation != LastFmOperation::Idle
            || self.cloud_busy()
            || matches!(self.account_state, AccountViewState::Checking)
        {
            return;
        }
        if !self.active_downloads.is_empty() || !self.download_queue.is_empty() {
            self.settings_error = Some(
                "Pause active and queued downloads before applying network or cache settings."
                    .into(),
            );
            self.settings_notice = None;
            cx.notify();
            return;
        }
        let settings = match self.settings_from_editor(cx) {
            Ok(settings) => settings,
            Err(error) => {
                self.settings_error = Some(error.to_string());
                self.settings_notice = None;
                cx.notify();
                return;
            }
        };
        let listen_together_server_changed =
            self.settings.listen_together.server_url != settings.listen_together.server_url;
        if listen_together_server_changed && self.listen_together_snapshot.room.is_some() {
            self.settings_error =
                Some("Leave the current Listen Together room before changing its server.".into());
            self.settings_notice = None;
            cx.notify();
            return;
        }

        self.settings_operation = SettingsOperation::Applying;
        self.settings_error = None;
        self.settings_notice = None;
        let build_settings = settings.clone();
        let build_auth = self
            .account_ready()
            .then(|| self.auth_session.clone())
            .flatten();
        let build_lastfm_credentials = self.lastfm_api_credentials.clone();
        let build_lastfm_session = self.lastfm_session.clone();
        let build_task = cx.background_executor().spawn(async move {
            let services = DesktopServices::with_settings_and_auth(&build_settings, build_auth)?;
            let lastfm_client = build_lastfm_credentials
                .map(|credentials| {
                    LastFmClient::with_proxy(
                        credentials,
                        build_lastfm_session,
                        &build_settings.proxy,
                    )
                })
                .transpose()?;
            let listen_together = listen_together_server_changed
                .then(|| {
                    ListenTogetherClient::new(build_settings.listen_together.server_url.clone())
                })
                .transpose()?;
            Ok::<_, AppError>((services, lastfm_client, listen_together))
        });
        let store = self.store.clone();
        self.settings_task = Some(cx.spawn_in(window, async move |this, cx| {
            let result = match build_task.await {
                Ok((services, lastfm_client, listen_together)) => {
                    match store.save_settings(settings.clone()).await {
                        Ok(()) => Ok((settings, services, lastfm_client, listen_together)),
                        Err(error) => Err(error),
                    }
                }
                Err(error) => Err(error),
            };
            this.update_in(cx, |this, window, cx| {
                this.settings_operation = SettingsOperation::Idle;
                match result {
                    Ok((settings, services, lastfm_client, listen_together)) => {
                        this.install_services(
                            settings,
                            services,
                            lastfm_client,
                            listen_together,
                            window,
                            cx,
                        );
                        this.settings_notice = Some(
                            "Settings saved and applied to network, playback, and caches.".into(),
                        );
                        this.settings_error = None;
                    }
                    Err(error) => {
                        this.settings_error = Some(error.to_string());
                        this.settings_notice = None;
                    }
                }
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    fn install_services(
        &mut self,
        settings: AppSettings,
        services: DesktopServices,
        lastfm_client: Option<LastFmClient>,
        listen_together: Option<ListenTogetherClient>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let quality_changed = self.settings.audio_quality != settings.audio_quality;
        let autoeq_cache_changed = self.settings.cache_root != settings.cache_root;
        let before = self.audio_player.snapshot();
        let selected_output = self.audio_player.device_snapshot().selected_id;
        let active_source = self.active_playback_source.clone();
        let offline_source = self
            .current_song
            .as_ref()
            .and_then(|song| self.downloaded_playback_source(&song.video_id));
        let restorable_source = offline_source.map(|source| (source, true)).or_else(|| {
            active_source
                .filter(|_| !quality_changed)
                .map(|active| (active.source, false))
        });

        self.reset_radio_queue();
        self.search_task = None;
        self.browse_task = None;
        self.home_task = None;
        self.explore_task = None;
        self.lyrics_task = None;
        self.thumbnail_tasks.clear();
        self.thumbnail_failures.clear();

        let DesktopServices {
            innertube,
            lyrics,
            thumbnails,
            audio_cache,
            downloaded_audio,
            mut audio,
            microphone,
            recognition,
        } = services;
        if let Some(device_id) = selected_output {
            let _ = audio.select_output_device(device_id);
        }
        audio.set_volume(before.volume);
        let mut restored_offline = false;
        if let Some((source, is_offline)) = restorable_source {
            match audio.load(source) {
                Ok(()) => {
                    restored_offline = is_offline;
                    if !before.position.is_zero() {
                        let _ = audio.seek(before.position);
                    }
                    if before.state == PlaybackState::Playing {
                        let _ = audio.play();
                    }
                }
                Err(error) => self.playback_error = Some(error.to_string()),
            }
        }
        let previous_audio = std::mem::replace(&mut self.audio_player, audio);
        cx.background_executor()
            .spawn(async move { drop(previous_audio) })
            .detach();
        let _ = self.audio_player.refresh_output_devices();

        self.search_client = innertube;
        self.lyrics_client = lyrics;
        self.thumbnail_cache = Some(thumbnails);
        self.audio_cache = audio_cache;
        self.downloaded_audio = downloaded_audio;
        self.microphone_recorder = microphone;
        self.recognition_client = recognition;
        self.lastfm_client = lastfm_client;
        if let Some(listen_together) = listen_together {
            self.listen_together = Some(listen_together);
            self.listen_together_snapshot = ListenTogetherSnapshot::default();
            self.listen_together_tracker.reset();
            self.listen_together_pending_sync = None;
            self.listen_together_warning = None;
        }
        self.settings = settings.clone();
        self.settings_draft = settings.clone();
        if autoeq_cache_changed {
            self.autoeq_search_generation = self.autoeq_search_generation.wrapping_add(1);
            self.autoeq_search_task = None;
            self.autoeq_models.clear();
            self.autoeq_selected_model = None;
            self.autoeq_variants.clear();
            self.autoeq_selected_variant_paths.clear();
            self.autoeq_wizard_step = AutoEqWizardStep::ModelSelection;
            let cached = AutoEqClient::with_settings(&settings)
                .is_ok_and(|client| client.is_database_cached());
            self.autoeq_database_state = AutoEqDatabaseState::NotLoaded { cached };
            if cached {
                self.download_auto_eq_database(cx);
            }
        }
        self.theme_mode = match settings.theme {
            AppTheme::Light => ThemeMode::Light,
            AppTheme::Dark => ThemeMode::Dark,
        };
        Theme::change(self.theme_mode, Some(window), cx);
        self.proxy_address_input.update(cx, |input, cx| {
            input.set_value(settings.proxy.address, window, cx)
        });
        self.proxy_username_input.update(cx, |input, cx| {
            input.set_value(settings.proxy.username, window, cx)
        });
        self.proxy_password_input.update(cx, |input, cx| {
            input.set_value(settings.proxy.password, window, cx)
        });
        self.cache_root_input.update(cx, |input, cx| {
            input.set_value(settings.cache_root.to_string_lossy(), window, cx)
        });
        self.listen_together_server_input.update(cx, |input, cx| {
            input.set_value(settings.listen_together.server_url, window, cx)
        });
        self.listen_together_username_input.update(cx, |input, cx| {
            input.set_value(settings.listen_together.username, window, cx)
        });

        if quality_changed && before.state != PlaybackState::Idle && !restored_offline {
            self.active_playback_source = None;
            self.persisted_playback_source = None;
            self.pending_resume_position = (!before.position.is_zero()).then_some(before.position);
            self.play_after_resolution = Some(before.state == PlaybackState::Playing);
            self.save_session(cx);
            if let Some(song) = self.current_song.clone() {
                self.resolve_and_play(song, window, cx);
            }
        }

        self.home_default_page = None;
        self.reload_home(None, None, cx);
        self.reload_explore(cx);
        if !self.model.search_query().trim().is_empty() {
            self.run_search(window, cx);
        }
        if self.lyrics_visible {
            self.reload_lyrics(cx);
        }
        self.verify_account(cx);
        self.maybe_refill_radio(window, cx);
        self.refresh_visible_thumbnails(cx);
    }

    fn reset_settings_editor(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.settings_draft = self.settings.clone();
        self.settings_error = None;
        self.settings_notice = None;
        self.theme_mode = match self.settings.theme {
            AppTheme::Light => ThemeMode::Light,
            AppTheme::Dark => ThemeMode::Dark,
        };
        Theme::change(self.theme_mode, Some(window), cx);
        self.proxy_address_input.update(cx, |input, cx| {
            input.set_value(&self.settings.proxy.address, window, cx)
        });
        self.proxy_username_input.update(cx, |input, cx| {
            input.set_value(&self.settings.proxy.username, window, cx)
        });
        self.proxy_password_input.update(cx, |input, cx| {
            input.set_value(&self.settings.proxy.password, window, cx)
        });
        self.cache_root_input.update(cx, |input, cx| {
            input.set_value(self.settings.cache_root.to_string_lossy(), window, cx)
        });
        self.listen_together_server_input.update(cx, |input, cx| {
            input.set_value(&self.settings.listen_together.server_url, window, cx)
        });
        self.listen_together_username_input.update(cx, |input, cx| {
            input.set_value(&self.settings.listen_together.username, window, cx)
        });
        cx.notify();
    }

    fn set_current_song(&mut self, song: Song, cx: &mut Context<Self>) {
        let changed = self
            .current_song
            .as_ref()
            .is_none_or(|current| current.video_id != song.video_id);
        self.current_song = Some(song.clone());
        if !changed {
            return;
        }

        self.lastfm_playback_tracker.reset();
        self.lyrics_task = None;
        self.lyrics_state = LyricsViewState::Idle;
        self.lyrics_active_line = None;
        self.lyrics_scroll = ScrollHandle::new();
        if song.is_episode {
            self.lyrics_visible = false;
            if self.now_playing_tab == NowPlayingTab::Lyrics {
                self.now_playing_tab = NowPlayingTab::UpNext;
            }
            return;
        }
        if self.lyrics_surface_visible() {
            self.load_lyrics(song, false, cx);
        }
    }

    fn lyrics_surface_visible(&self) -> bool {
        self.lyrics_visible
            || (self.now_playing_visible && self.now_playing_tab == NowPlayingTab::Lyrics)
    }

    fn open_now_playing(&mut self, cx: &mut Context<Self>) {
        if self.current_song.is_none() {
            return;
        }
        self.now_playing_visible = true;
        self.queue_visible = false;
        self.lyrics_visible = false;
        self.playback_parameters_visible = false;
        self.playlist_picker_song = None;
        self.cloud_playlist_picker_song = None;
        cx.notify();
    }

    fn close_now_playing(&mut self, cx: &mut Context<Self>) {
        self.now_playing_visible = false;
        if matches!(self.lyrics_state, LyricsViewState::Loading(_)) {
            self.lyrics_task = None;
            self.lyrics_state = LyricsViewState::Idle;
        }
        cx.notify();
    }

    fn select_now_playing_tab(&mut self, index: usize, cx: &mut Context<Self>) {
        let next_tab = NowPlayingTab::from_index(index);
        if self.now_playing_tab == NowPlayingTab::Lyrics
            && next_tab != NowPlayingTab::Lyrics
            && matches!(self.lyrics_state, LyricsViewState::Loading(_))
        {
            self.lyrics_task = None;
            self.lyrics_state = LyricsViewState::Idle;
        }
        self.now_playing_tab = next_tab;
        if self.now_playing_tab == NowPlayingTab::Lyrics
            && let Some(song) = self.current_song.clone()
            && !song.is_episode
            && !lyrics_state_matches_song(&self.lyrics_state, &song.video_id)
        {
            self.load_lyrics(song, false, cx);
        }
        self.update_lyrics_timeline();
        cx.notify();
    }

    fn toggle_lyrics(&mut self, cx: &mut Context<Self>) {
        self.lyrics_visible = !self.lyrics_visible;
        if self.lyrics_visible {
            self.queue_visible = false;
            self.playback_parameters_visible = false;
            self.playlist_picker_song = None;
            self.cloud_playlist_picker_song = None;
            if let Some(song) = self.current_song.clone()
                && !lyrics_state_matches_song(&self.lyrics_state, &song.video_id)
            {
                self.load_lyrics(song, false, cx);
            }
            self.update_lyrics_timeline();
        } else {
            if matches!(self.lyrics_state, LyricsViewState::Loading(_)) {
                self.lyrics_state = LyricsViewState::Idle;
            }
            self.lyrics_task = None;
        }
        cx.notify();
    }

    fn reload_lyrics(&mut self, cx: &mut Context<Self>) {
        if let Some(song) = self.current_song.clone() {
            self.load_lyrics(song, true, cx);
        }
    }

    fn load_lyrics(&mut self, song: Song, force_network: bool, cx: &mut Context<Self>) {
        if song.is_episode {
            self.lyrics_task = None;
            self.lyrics_state = LyricsViewState::Idle;
            return;
        }
        let video_id = song.video_id.clone();
        self.lyrics_state = LyricsViewState::Loading(video_id.clone());
        self.lyrics_active_line = None;
        let client = self.lyrics_client.clone();
        let store = self.store.clone();
        self.lyrics_task = Some(cx.spawn(async move |this, cx| {
            let cached = if force_network {
                None
            } else {
                match store.load_lyrics(video_id.clone()).await {
                    Ok(document) => document,
                    Err(error) => {
                        tracing::warn!(%error, "lyrics cache read failed");
                        None
                    }
                }
            };
            let result = if let Some(document) = cached {
                Ok(Some(document))
            } else {
                let result = client.lyrics_for_song(&song).await;
                if let Ok(Some(document)) = &result
                    && let Err(error) = store.save_lyrics(song, document.clone()).await
                {
                    tracing::warn!(%error, "lyrics cache write failed");
                }
                result
            };
            this.update(cx, |this, cx| {
                if !this.lyrics_surface_visible()
                    || !lyrics_request_matches_current(this.current_song.as_ref(), &video_id)
                {
                    return;
                }
                this.lyrics_state = match result {
                    Ok(Some(document)) => LyricsViewState::Loaded(video_id, document),
                    Ok(None) => LyricsViewState::Unavailable(video_id),
                    Err(error) => LyricsViewState::Failed(video_id, error.to_string()),
                };
                this.update_lyrics_timeline();
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    fn update_lyrics_timeline(&mut self) {
        let position = self
            .seek_preview
            .or(self.pending_resume_position)
            .unwrap_or_else(|| self.audio_player.snapshot().position);
        let active = match (&self.lyrics_state, self.current_song.as_ref()) {
            (LyricsViewState::Loaded(video_id, document), Some(song))
                if video_id == &song.video_id =>
            {
                document.active_line_index(position)
            }
            _ => None,
        };
        if active == self.lyrics_active_line {
            return;
        }
        self.lyrics_active_line = active;
        if self.lyrics_surface_visible()
            && let Some(index) = active
        {
            self.lyrics_scroll
                .scroll_to_top_of_item(index.saturating_sub(3));
        }
    }

    fn reload_stats(&mut self, cx: &mut Context<Self>) {
        if self.stats_task.is_some() {
            return;
        }
        self.stats_state = StoredViewState::Loading;
        let store = self.store.clone();
        let start_ms = self.stats_period.start_ms();
        self.stats_task = Some(cx.spawn(async move |this, cx| {
            let result = store.listening_stats(start_ms, 20).await;
            this.update(cx, |this, cx| {
                this.stats_task = None;
                this.stats_state = match result {
                    Ok(stats) => StoredViewState::Loaded(stats),
                    Err(error) => StoredViewState::Failed(error.to_string()),
                };
                this.refresh_visible_thumbnails(cx);
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    fn select_stats_period(&mut self, period: StatsPeriod, cx: &mut Context<Self>) {
        if self.stats_period == period {
            return;
        }
        self.stats_period = period;
        self.reload_stats(cx);
    }

    fn merge_local_catalog_items(&mut self, items: Vec<BrowseItem>) {
        if !matches!(self.local_catalog_state, StoredViewState::Loaded(_)) {
            self.local_catalog_state = StoredViewState::Loaded(Vec::new());
        }
        let StoredViewState::Loaded(catalog) = &mut self.local_catalog_state else {
            return;
        };
        for item in items.into_iter().rev() {
            if !matches!(item.kind, BrowseKind::Album | BrowseKind::Artist)
                || item.browse_id.trim().is_empty()
                || item.title.trim().is_empty()
            {
                continue;
            }
            catalog.retain(|existing| {
                existing.kind != item.kind || existing.browse_id != item.browse_id
            });
            catalog.insert(0, item);
        }
        catalog.truncate(2_000);
    }

    fn remember_local_catalog_items(
        &mut self,
        items: impl IntoIterator<Item = BrowseItem>,
        cx: &mut Context<Self>,
    ) {
        let mut seen = HashSet::new();
        let items = items
            .into_iter()
            .filter(|item| matches!(item.kind, BrowseKind::Album | BrowseKind::Artist))
            .filter(|item| {
                seen.insert((item.kind, item.browse_id.clone()))
                    && !item.browse_id.trim().is_empty()
                    && !item.title.trim().is_empty()
            })
            .collect::<Vec<_>>();
        if items.is_empty() {
            return;
        }
        self.local_catalog_error = None;
        self.merge_local_catalog_items(items.clone());
        let store = self.store.clone();
        cx.spawn(async move |this, cx| {
            let result = store.remember_catalog_items(items).await;
            this.update(cx, |this, cx| {
                if let Err(error) = result {
                    this.local_catalog_error = Some(error.to_string());
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    fn save_search_query(&mut self, query: String, cx: &mut Context<Self>) {
        let query = query.trim().to_owned();
        if query.is_empty() {
            return;
        }
        let store = self.store.clone();
        cx.spawn(async move |this, cx| {
            let result = store.record_search_query(query).await;
            this.update(cx, |this, cx| {
                match result {
                    Ok(entry) => {
                        this.search_history_error = None;
                        if let StoredViewState::Loaded(history) = &mut this.search_history_state {
                            history.retain(|item| item.query != entry.query);
                            history.insert(0, entry);
                            history.truncate(500);
                        } else {
                            this.search_history_state = StoredViewState::Loaded(vec![entry]);
                        }
                    }
                    Err(error) => this.search_history_error = Some(error.to_string()),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn reload_search_history(&mut self, cx: &mut Context<Self>) {
        if self.search_history_task.is_some() {
            return;
        }
        self.search_history_error = None;
        self.search_history_state = StoredViewState::Loading;
        let store = self.store.clone();
        self.search_history_task = Some(cx.spawn(async move |this, cx| {
            let result = store.search_history(500).await;
            this.update(cx, |this, cx| {
                this.search_history_task = None;
                this.search_history_state = match result {
                    Ok(history) => StoredViewState::Loaded(history),
                    Err(error) => StoredViewState::Failed(error.to_string()),
                };
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    fn delete_search_history(&mut self, id: i64, cx: &mut Context<Self>) {
        if self.search_history_task.is_some() {
            return;
        }
        self.search_history_error = None;
        let store = self.store.clone();
        self.search_history_task = Some(cx.spawn(async move |this, cx| {
            let result = store.delete_search_history(id).await;
            this.update(cx, |this, cx| {
                this.search_history_task = None;
                match result {
                    Ok(()) => {
                        if let StoredViewState::Loaded(history) = &mut this.search_history_state {
                            history.retain(|entry| entry.id != id);
                        }
                    }
                    Err(error) => this.search_history_error = Some(error.to_string()),
                }
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    fn clear_search_history(&mut self, cx: &mut Context<Self>) {
        if self.search_history_task.is_some() {
            return;
        }
        self.search_history_error = None;
        let store = self.store.clone();
        self.search_history_task = Some(cx.spawn(async move |this, cx| {
            let result = store.clear_search_history().await;
            this.update(cx, |this, cx| {
                this.search_history_task = None;
                match result {
                    Ok(()) => this.search_history_state = StoredViewState::Loaded(Vec::new()),
                    Err(error) => this.search_history_error = Some(error.to_string()),
                }
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    fn confirm_clear_search_history(
        &mut self,
        entry_count: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.search_history_task.is_some() || entry_count == 0 {
            return;
        }
        let weak = cx.weak_entity();
        window.open_alert_dialog(cx, move |dialog, _, _| {
            let weak = weak.clone();
            dialog
                .title("Clear search history?")
                .description(format!(
                    "All {entry_count} saved search queries will be removed from this device."
                ))
                .button_props(
                    DialogButtonProps::default()
                        .ok_text("Clear history")
                        .ok_variant(ButtonVariant::Danger)
                        .cancel_text("Cancel")
                        .show_cancel(true),
                )
                .on_ok(move |_, _, cx| {
                    weak.update(cx, |this, cx| this.clear_search_history(cx))
                        .ok();
                    true
                })
        });
    }

    fn start_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let query = self.model.search_query().trim().to_owned();
        if !query.is_empty() {
            self.save_search_query(query, cx);
        }
        self.run_search(window, cx);
    }

    fn run_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.search_source == SearchSource::Local {
            self.dismiss_search_suggestions();
            self.search_task = None;
            self.browse_task = None;
            self.browse_state = None;
            self.search_state = SearchViewState::Idle;
            self.refresh_visible_thumbnails(cx);
            cx.notify();
            return;
        }
        let query = self.model.search_query().trim().to_owned();
        if query.is_empty() {
            self.search_state = SearchViewState::Idle;
            self.dismiss_search_suggestions();
            self.refresh_visible_thumbnails(cx);
            cx.notify();
            return;
        }
        if let Some(parsed) = parse_youtube_url(&query) {
            self.open_youtube_url(parsed, window, cx);
            return;
        }

        self.dismiss_search_suggestions();
        self.search_state = SearchViewState::Loading;
        self.browse_task = None;
        self.search_loading_more = false;
        self.search_load_more_error = None;
        self.search_seen_continuations.clear();
        self.refresh_visible_thumbnails(cx);
        cx.notify();

        self.browse_state = None;
        let client = self.search_client.clone();
        let filter = self.search_filter;
        self.search_task = Some(cx.spawn_in(window, async move |this, cx| {
            let result = client.search(&query, filter).await;
            this.update(cx, |this, cx| {
                this.search_state = match result {
                    Ok(result) => {
                        this.remember_local_catalog_items(result.items.clone(), cx);
                        if (filter.returns_songs() && result.songs.is_empty())
                            || (!filter.returns_songs() && result.items.is_empty())
                        {
                            SearchViewState::Empty
                        } else {
                            SearchViewState::Loaded(result)
                        }
                    }
                    Err(error) => SearchViewState::Failed(error.to_string()),
                };
                this.refresh_visible_thumbnails(cx);
                cx.notify();
            })
            .ok();
        }));
    }

    fn open_youtube_url(
        &mut self,
        parsed: ParsedYouTubeUrl,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.dismiss_search_suggestions();
        self.search_task = None;
        self.browse_task = None;
        self.browse_state = None;
        match parsed {
            ParsedYouTubeUrl::Video(video_id) => {
                self.search_state = SearchViewState::Loading;
                let client = self.search_client.clone();
                self.search_task = Some(cx.spawn_in(window, async move |this, cx| {
                    let result = async {
                        let page = client.radio(&video_id).await?;
                        page.songs
                            .into_iter()
                            .find(|song| song.video_id == video_id)
                            .ok_or_else(|| {
                                AppError::Protocol(
                                    "YouTube returned no playable song for this video URL".into(),
                                )
                            })
                    }
                    .await;
                    this.update_in(cx, |this, window, cx| {
                        this.search_task = None;
                        match result {
                            Ok(song) => {
                                this.search_state = SearchViewState::Idle;
                                this.play_song_collection(vec![song], 0, window, cx);
                            }
                            Err(error) => {
                                this.search_state = SearchViewState::Failed(error.to_string());
                            }
                        }
                        this.refresh_visible_thumbnails(cx);
                        cx.notify();
                    })
                    .ok();
                }));
                cx.notify();
            }
            ParsedYouTubeUrl::Playlist(playlist_id) => {
                self.search_state = SearchViewState::Idle;
                self.open_online_browse(
                    BrowseItem {
                        browse_id: playlist_id,
                        kind: BrowseKind::Playlist,
                        title: "YouTube playlist".into(),
                        subtitle: "Shared link".into(),
                        thumbnail_url: None,
                        params: None,
                        editable: false,
                    },
                    cx,
                );
            }
            ParsedYouTubeUrl::Album(playlist_id) => {
                self.search_state = SearchViewState::Idle;
                self.open_online_browse(
                    BrowseItem {
                        browse_id: format!("MPREb_{playlist_id}"),
                        kind: BrowseKind::Album,
                        title: "YouTube Music album".into(),
                        subtitle: "Shared link".into(),
                        thumbnail_url: None,
                        params: None,
                        editable: false,
                    },
                    cx,
                );
            }
            ParsedYouTubeUrl::Artist(artist_id) => {
                self.search_state = SearchViewState::Idle;
                self.open_online_browse(
                    BrowseItem {
                        browse_id: artist_id,
                        kind: BrowseKind::Artist,
                        title: "YouTube Music artist".into(),
                        subtitle: "Shared link".into(),
                        thumbnail_url: None,
                        params: None,
                        editable: false,
                    },
                    cx,
                );
            }
        }
    }

    fn dismiss_search_suggestions(&mut self) {
        self.search_suggestion_generation = self.search_suggestion_generation.wrapping_add(1);
        self.search_suggestion_task = None;
        self.search_suggestion_state = SearchSuggestionViewState::Hidden;
    }

    fn schedule_search_suggestions(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.search_source != SearchSource::Online {
            self.dismiss_search_suggestions();
            return;
        }
        self.search_suggestion_generation = self.search_suggestion_generation.wrapping_add(1);
        let generation = self.search_suggestion_generation;
        self.search_suggestion_task = None;
        let query = self.model.search_query().trim().to_owned();
        if query.is_empty() {
            self.search_suggestion_state = SearchSuggestionViewState::Hidden;
            self.refresh_visible_thumbnails(cx);
            cx.notify();
            return;
        }
        if parse_youtube_url(&query).is_some() {
            self.search_suggestion_state = SearchSuggestionViewState::Hidden;
            self.refresh_visible_thumbnails(cx);
            cx.notify();
            return;
        }

        self.search_suggestion_state = SearchSuggestionViewState::Loading;
        self.refresh_visible_thumbnails(cx);
        cx.notify();
        let client = self.search_client.clone();
        self.search_suggestion_task = Some(cx.spawn_in(window, async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(250))
                .await;
            let result = client.search_suggestions(&query).await;
            this.update(cx, |this, cx| {
                if !search_suggestion_request_is_current(
                    this.search_suggestion_generation,
                    generation,
                    this.model.search_query(),
                    &query,
                ) {
                    return;
                }
                this.search_suggestion_state = match result {
                    Ok(suggestions)
                        if suggestions.queries.is_empty()
                            && suggestions.songs.is_empty()
                            && suggestions.items.is_empty() =>
                    {
                        SearchSuggestionViewState::Hidden
                    }
                    Ok(suggestions) => SearchSuggestionViewState::Loaded(suggestions),
                    Err(error) => SearchSuggestionViewState::Failed(error.to_string()),
                };
                this.refresh_visible_thumbnails(cx);
                cx.notify();
            })
            .ok();
        }));
    }

    fn apply_search_suggestion(
        &mut self,
        query: String,
        submit: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.search_input.update(cx, |input, cx| {
            input.set_value(&query, window, cx);
        });
        self.model.set_search_query(query);
        self.browse_state = None;
        self.browse_task = None;
        self.search_task = None;
        self.search_state = SearchViewState::Idle;
        if submit {
            self.start_search(window, cx);
        } else {
            self.schedule_search_suggestions(window, cx);
        }
    }

    fn play_search_suggestion(&mut self, song: Song, window: &mut Window, cx: &mut Context<Self>) {
        self.dismiss_search_suggestions();
        self.play_song_collection(vec![song], 0, window, cx);
        self.refresh_visible_thumbnails(cx);
        cx.notify();
    }

    fn open_search_suggestion(&mut self, item: BrowseItem, cx: &mut Context<Self>) {
        self.dismiss_search_suggestions();
        self.open_online_browse(item, cx);
    }

    fn select_search_filter(
        &mut self,
        filter: SearchFilter,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.search_filter == filter {
            return;
        }
        self.search_filter = filter;
        self.browse_state = None;
        self.browse_task = None;
        if self.model.search_query().trim().is_empty() {
            self.search_state = SearchViewState::Idle;
            self.refresh_visible_thumbnails(cx);
            cx.notify();
        } else {
            self.run_search(window, cx);
        }
    }

    fn select_search_source(
        &mut self,
        source: SearchSource,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.search_source == source {
            return;
        }
        self.search_source = source;
        self.search_task = None;
        self.browse_task = None;
        self.browse_state = None;
        self.search_state = SearchViewState::Idle;
        self.dismiss_search_suggestions();
        if source == SearchSource::Online && !self.model.search_query().trim().is_empty() {
            self.schedule_search_suggestions(window, cx);
        }
        self.refresh_visible_thumbnails(cx);
        cx.notify();
    }

    fn select_local_search_filter(&mut self, filter: LocalSearchFilter, cx: &mut Context<Self>) {
        if self.local_search_filter != filter {
            self.local_search_filter = filter;
            cx.notify();
        }
    }

    fn open_online_browse(&mut self, item: BrowseItem, cx: &mut Context<Self>) {
        if self.model.route() != Route::Search {
            self.browse_return_route = self.model.route();
            self.model.navigate(Route::Search);
        } else if self.browse_state.is_none() {
            self.browse_return_route = Route::Search;
        }
        self.browse_state = Some(BrowseViewState::Loading(item.clone()));
        self.browse_loading_more = false;
        self.browse_load_more_error = None;
        self.browse_seen_continuations.clear();
        self.refresh_visible_thumbnails(cx);
        let client = self.search_client.clone();
        self.browse_task = Some(cx.spawn(async move |this, cx| {
            let result = client.browse(&item).await;
            this.update(cx, |this, cx| {
                this.browse_state = Some(match result {
                    Ok(page) => {
                        this.remember_local_catalog_items(
                            std::iter::once(page.item.clone()).chain(page.related.iter().cloned()),
                            cx,
                        );
                        BrowseViewState::Loaded(page)
                    }
                    Err(error) => BrowseViewState::Failed(item, error.to_string()),
                });
                this.refresh_visible_thumbnails(cx);
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    fn close_online_browse(&mut self, cx: &mut Context<Self>) {
        self.browse_task = None;
        self.browse_state = None;
        self.browse_loading_more = false;
        self.browse_load_more_error = None;
        self.browse_seen_continuations.clear();
        self.model.navigate(self.browse_return_route);
        self.refresh_visible_thumbnails(cx);
        cx.notify();
    }

    fn load_more_search_results(&mut self, cx: &mut Context<Self>) {
        if self.search_loading_more {
            return;
        }
        let continuation = match &self.search_state {
            SearchViewState::Loaded(result) => result.continuation.clone(),
            _ => None,
        };
        let Some(continuation) = continuation else {
            return;
        };
        if self.search_seen_continuations.contains(&continuation) {
            if let SearchViewState::Loaded(result) = &mut self.search_state {
                result.continuation = None;
            }
            self.search_load_more_error =
                Some("YouTube Music repeated a continuation token; loading stopped safely.".into());
            cx.notify();
            return;
        }

        self.search_loading_more = true;
        self.search_load_more_error = None;
        let client = self.search_client.clone();
        self.search_task = Some(cx.spawn(async move |this, cx| {
            let result = client.search_continuation(&continuation).await;
            this.update(cx, |this, cx| {
                this.search_loading_more = false;
                match result {
                    Ok(mut next) => {
                        this.remember_local_catalog_items(next.items.clone(), cx);
                        this.search_seen_continuations.insert(continuation);
                        if next
                            .continuation
                            .as_ref()
                            .is_some_and(|token| this.search_seen_continuations.contains(token))
                        {
                            next.continuation = None;
                        }
                        if let SearchViewState::Loaded(current) = &mut this.search_state
                            && current.append_continuation(next) == 0
                        {
                            current.continuation = None;
                        }
                    }
                    Err(error) => this.search_load_more_error = Some(error.to_string()),
                }
                this.refresh_visible_thumbnails(cx);
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    fn load_more_online_browse(&mut self, cx: &mut Context<Self>) {
        if self.browse_loading_more {
            return;
        }
        let continuation = match &self.browse_state {
            Some(BrowseViewState::Loaded(page)) => page.continuation.clone(),
            _ => None,
        };
        let Some(continuation) = continuation else {
            return;
        };
        if self.browse_seen_continuations.contains(&continuation) {
            if let Some(BrowseViewState::Loaded(page)) = &mut self.browse_state {
                page.continuation = None;
            }
            self.browse_load_more_error =
                Some("YouTube Music repeated a continuation token; loading stopped safely.".into());
            cx.notify();
            return;
        }

        self.browse_loading_more = true;
        self.browse_load_more_error = None;
        let client = self.search_client.clone();
        self.browse_task = Some(cx.spawn(async move |this, cx| {
            let result = client.browse_continuation(&continuation).await;
            this.update(cx, |this, cx| {
                this.browse_loading_more = false;
                match result {
                    Ok(mut next) => {
                        this.remember_local_catalog_items(next.items.clone(), cx);
                        this.browse_seen_continuations.insert(continuation);
                        if next
                            .continuation
                            .as_ref()
                            .is_some_and(|token| this.browse_seen_continuations.contains(token))
                        {
                            next.continuation = None;
                        }
                        if let Some(BrowseViewState::Loaded(page)) = &mut this.browse_state
                            && page.append_continuation(next) == 0
                        {
                            page.continuation = None;
                        }
                    }
                    Err(error) => this.browse_load_more_error = Some(error.to_string()),
                }
                this.refresh_visible_thumbnails(cx);
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    fn start_recognition(&mut self, cx: &mut Context<Self>) {
        if self.recognition_state.is_busy() {
            return;
        }
        self.recognition_generation = self.recognition_generation.wrapping_add(1);
        let generation = self.recognition_generation;
        let capture = match self.microphone_recorder.start(RECOGNITION_CAPTURE_DURATION) {
            Ok(capture) => capture,
            Err(error) => {
                self.recognition_state = RecognitionViewState::Failed(error.to_string());
                cx.notify();
                return;
            }
        };
        self.recognition_cancellation = Some(capture.cancellation());
        self.recognition_state = RecognitionViewState::Listening;
        let recognition_client = self.recognition_client.clone();
        self.recognition_task = Some(cx.spawn(async move |this, cx| {
            let recording = match capture.finish().await {
                Ok(recording) => recording,
                Err(error) => {
                    this.update(cx, |this, cx| {
                        if this.recognition_generation == generation {
                            this.recognition_cancellation = None;
                            this.recognition_task = None;
                            this.recognition_state =
                                RecognitionViewState::Failed(error.to_string());
                            cx.notify();
                        }
                    })
                    .ok();
                    return;
                }
            };
            let should_process = this
                .update(cx, |this, cx| {
                    if this.recognition_generation != generation {
                        return false;
                    }
                    this.recognition_cancellation = None;
                    this.recognition_state = RecognitionViewState::Processing;
                    cx.notify();
                    true
                })
                .unwrap_or(false);
            if !should_process {
                return;
            }

            let processing = cx.background_executor().spawn(async move {
                let (samples, source_sample_rate, _) = recording.into_parts();
                let samples = linear_resample_mono_i16(
                    &samples,
                    source_sample_rate,
                    RECOGNITION_SAMPLE_RATE,
                )?;
                let signature = generate_shazam_signature(&samples)?;
                recognition_client.recognize(&signature).await
            });
            let result = processing.await;
            this.update(cx, |this, cx| {
                if this.recognition_generation == generation {
                    this.recognition_task = None;
                    this.recognition_state = match result {
                        Ok(Some(result)) => {
                            this.save_recognition_result(result.clone(), cx);
                            RecognitionViewState::Matched(result)
                        }
                        Ok(None) => RecognitionViewState::NoMatch,
                        Err(error) => RecognitionViewState::Failed(error.to_string()),
                    };
                    this.refresh_visible_thumbnails(cx);
                    cx.notify();
                }
            })
            .ok();
        }));
        cx.notify();
    }

    fn cancel_recognition(&mut self, show_cancelled: bool, cx: &mut Context<Self>) {
        let was_busy = self.recognition_state.is_busy();
        if let Some(cancellation) = self.recognition_cancellation.take() {
            cancellation.cancel();
        }
        self.recognition_task = None;
        if was_busy {
            self.recognition_generation = self.recognition_generation.wrapping_add(1);
            self.recognition_state = if show_cancelled {
                RecognitionViewState::Cancelled
            } else {
                RecognitionViewState::Ready
            };
            cx.notify();
        }
    }

    fn save_recognition_result(&mut self, result: RecognitionResult, cx: &mut Context<Self>) {
        let store = self.store.clone();
        cx.spawn(async move |this, cx| {
            let saved = store.record_recognition(result).await;
            this.update(cx, |this, cx| {
                match saved {
                    Ok(entry) => {
                        this.recognition_history_error = None;
                        if let StoredViewState::Loaded(history) =
                            &mut this.recognition_history_state
                        {
                            history.insert(0, entry);
                        }
                    }
                    Err(error) => this.recognition_history_error = Some(error.to_string()),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn open_recognition_history(&mut self, cx: &mut Context<Self>) {
        self.recognition_history_visible = true;
        if !matches!(&self.recognition_history_state, StoredViewState::Loaded(_)) {
            self.reload_recognition_history(cx);
        } else {
            self.refresh_visible_thumbnails(cx);
            cx.notify();
        }
    }

    fn reload_recognition_history(&mut self, cx: &mut Context<Self>) {
        if self.recognition_history_task.is_some() {
            return;
        }
        self.recognition_history_error = None;
        self.recognition_history_state = StoredViewState::Loading;
        let store = self.store.clone();
        self.recognition_history_task = Some(cx.spawn(async move |this, cx| {
            let history = store.recognition_history(500).await;
            this.update(cx, |this, cx| {
                this.recognition_history_task = None;
                this.recognition_history_state = match history {
                    Ok(history) => StoredViewState::Loaded(history),
                    Err(error) => StoredViewState::Failed(error.to_string()),
                };
                this.refresh_visible_thumbnails(cx);
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    fn delete_recognition_history(&mut self, id: i64, cx: &mut Context<Self>) {
        if self.recognition_history_task.is_some() {
            return;
        }
        self.recognition_history_error = None;
        let store = self.store.clone();
        self.recognition_history_task = Some(cx.spawn(async move |this, cx| {
            let result = store.delete_recognition_history(id).await;
            this.update(cx, |this, cx| {
                this.recognition_history_task = None;
                match result {
                    Ok(()) => {
                        if let StoredViewState::Loaded(history) =
                            &mut this.recognition_history_state
                        {
                            history.retain(|entry| entry.id != id);
                        }
                    }
                    Err(error) => this.recognition_history_error = Some(error.to_string()),
                }
                this.refresh_visible_thumbnails(cx);
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    fn clear_recognition_history(&mut self, cx: &mut Context<Self>) {
        if self.recognition_history_task.is_some() {
            return;
        }
        self.recognition_history_error = None;
        let store = self.store.clone();
        self.recognition_history_task = Some(cx.spawn(async move |this, cx| {
            let result = store.clear_recognition_history().await;
            this.update(cx, |this, cx| {
                this.recognition_history_task = None;
                match result {
                    Ok(()) => this.recognition_history_state = StoredViewState::Loaded(Vec::new()),
                    Err(error) => this.recognition_history_error = Some(error.to_string()),
                }
                this.refresh_visible_thumbnails(cx);
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    fn confirm_delete_recognition_history(
        &mut self,
        id: i64,
        title: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.recognition_history_task.is_some() {
            return;
        }
        let weak = cx.weak_entity();
        window.open_alert_dialog(cx, move |dialog, _, _| {
            let weak = weak.clone();
            dialog
                .title("Delete recognition history item?")
                .description(format!(
                    "“{title}” will be removed from recognition history."
                ))
                .button_props(
                    DialogButtonProps::default()
                        .ok_text("Delete")
                        .ok_variant(ButtonVariant::Danger)
                        .cancel_text("Cancel")
                        .show_cancel(true),
                )
                .on_ok(move |_, _, cx| {
                    weak.update(cx, |this, cx| this.delete_recognition_history(id, cx))
                        .ok();
                    true
                })
        });
    }

    fn confirm_clear_recognition_history(
        &mut self,
        entry_count: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.recognition_history_task.is_some() || entry_count == 0 {
            return;
        }
        let weak = cx.weak_entity();
        window.open_alert_dialog(cx, move |dialog, _, _| {
            let weak = weak.clone();
            dialog
                .title("Clear recognition history?")
                .description(format!(
                    "All {entry_count} recognized song entries will be removed from this device."
                ))
                .button_props(
                    DialogButtonProps::default()
                        .ok_text("Clear history")
                        .ok_variant(ButtonVariant::Danger)
                        .cancel_text("Cancel")
                        .show_cancel(true),
                )
                .on_ok(move |_, _, cx| {
                    weak.update(cx, |this, cx| this.clear_recognition_history(cx))
                        .ok();
                    true
                })
        });
    }

    fn navigate(&mut self, route: Route, cx: &mut Context<Self>) {
        if self.model.route() == Route::Recognition && route != Route::Recognition {
            self.cancel_recognition(false, cx);
            self.recognition_history_visible = false;
        }
        if route != Route::Search {
            self.browse_task = None;
            self.search_task = None;
            self.dismiss_search_suggestions();
            self.browse_state = None;
            self.search_loading_more = false;
            self.browse_loading_more = false;
            if matches!(self.search_state, SearchViewState::Loading) {
                self.search_state = SearchViewState::Idle;
            }
        }
        self.model.navigate(route);
        if route == Route::Stats {
            self.reload_stats(cx);
        }
        self.refresh_visible_thumbnails(cx);
        cx.notify();
    }

    fn refresh_visible_thumbnails(&mut self, cx: &mut Context<Self>) {
        let mut urls = HashSet::new();
        if let Some(song) = &self.current_song {
            collect_thumbnail_url(song.thumbnail_url.as_deref(), &mut urls);
        }
        if self.model.route() == Route::Home
            && let HomeFeedState::Loaded(page) = &self.home_state
        {
            for section in &page.sections {
                collect_thumbnail_url(section.thumbnail_url.as_deref(), &mut urls);
                for item in &section.items {
                    collect_thumbnail_url(item.thumbnail_url(), &mut urls);
                }
            }
        }
        if self.model.route() == Route::Home {
            for song in self.home_quick_picks() {
                collect_thumbnail_url(song.thumbnail_url.as_deref(), &mut urls);
            }
            if let StoredViewState::Loaded(discoveries) = &self.daily_discover_state {
                for item in discoveries {
                    collect_thumbnail_url(item.recommendation.thumbnail_url.as_deref(), &mut urls);
                }
            }
            if let StoredViewState::Loaded(songs) = &self.keep_listening_state {
                for song in songs {
                    collect_thumbnail_url(song.thumbnail_url.as_deref(), &mut urls);
                }
            }
            if let StoredViewState::Loaded(songs) = &self.forgotten_favorites_state {
                for song in songs {
                    collect_thumbnail_url(song.thumbnail_url.as_deref(), &mut urls);
                }
            }
            if let AccountViewState::SignedIn(profile) = &self.account_state {
                collect_thumbnail_url(profile.thumbnail_url.as_deref(), &mut urls);
            }
            if let CloudLibraryViewState::Loaded(library) = &self.cloud_library_state {
                for item in &library.playlists {
                    collect_thumbnail_url(item.thumbnail_url.as_deref(), &mut urls);
                }
            }
        }
        if self.model.route() == Route::Explore
            && let ExploreFeedState::Loaded(page) = &self.explore_state
        {
            for section in &page.chart_sections {
                collect_thumbnail_url(section.thumbnail_url.as_deref(), &mut urls);
                for item in &section.items {
                    collect_thumbnail_url(item.thumbnail_url(), &mut urls);
                }
            }
            for item in &page.new_release_albums {
                collect_thumbnail_url(item.thumbnail_url.as_deref(), &mut urls);
            }
        }
        if self.model.route() == Route::Recognition {
            if self.recognition_history_visible {
                if let StoredViewState::Loaded(history) = &self.recognition_history_state {
                    for entry in history {
                        collect_thumbnail_url(entry.result.cover_art_url.as_deref(), &mut urls);
                    }
                }
            } else if let RecognitionViewState::Matched(result) = &self.recognition_state {
                collect_thumbnail_url(result.cover_art_url.as_deref(), &mut urls);
            }
        }
        if self.model.route() == Route::Settings
            && let AccountViewState::SignedIn(profile) = &self.account_state
        {
            collect_thumbnail_url(profile.thumbnail_url.as_deref(), &mut urls);
        }
        if self.model.route() == Route::Library
            && let CloudLibraryViewState::Loaded(library) = &self.cloud_library_state
        {
            for song in library
                .liked_songs
                .iter()
                .chain(&library.library_songs)
                .chain(&library.uploaded_songs)
            {
                collect_thumbnail_url(song.thumbnail_url.as_deref(), &mut urls);
            }
            for item in &library.playlists {
                collect_thumbnail_url(item.thumbnail_url.as_deref(), &mut urls);
            }
            for item in library
                .albums
                .iter()
                .chain(&library.uploaded_albums)
                .chain(&library.artists)
            {
                collect_thumbnail_url(item.thumbnail_url.as_deref(), &mut urls);
            }
        }
        if self.model.route() == Route::Library {
            if let StoredViewState::Loaded(podcasts) = &self.podcast_subscriptions {
                for podcast in podcasts {
                    collect_thumbnail_url(podcast.thumbnail_url.as_deref(), &mut urls);
                }
            }
            if let StoredViewState::Loaded(episodes) = &self.episodes_for_later {
                for episode in episodes {
                    collect_thumbnail_url(episode.song.thumbnail_url.as_deref(), &mut urls);
                }
            }
            if let StoredViewState::Loaded(downloads) = &self.downloads_state {
                for download in downloads {
                    collect_thumbnail_url(download.song.thumbnail_url.as_deref(), &mut urls);
                }
            }
        }
        if self.model.route() == Route::Stats
            && let StoredViewState::Loaded(stats) = &self.stats_state
        {
            for entry in &stats.top_songs {
                collect_thumbnail_url(entry.song.thumbnail_url.as_deref(), &mut urls);
            }
            for album in &stats.top_albums {
                collect_thumbnail_url(album.thumbnail_url.as_deref(), &mut urls);
            }
        }
        if self.model.route() == Route::Search {
            if self.search_source == SearchSource::Local {
                for song in self.local_search_songs() {
                    collect_thumbnail_url(song.thumbnail_url.as_deref(), &mut urls);
                }
            } else {
                match &self.browse_state {
                    Some(BrowseViewState::Loaded(page)) => {
                        collect_thumbnail_url(page.item.thumbnail_url.as_deref(), &mut urls);
                        for song in &page.songs {
                            collect_thumbnail_url(song.thumbnail_url.as_deref(), &mut urls);
                        }
                        for item in &page.related {
                            collect_thumbnail_url(item.thumbnail_url.as_deref(), &mut urls);
                        }
                    }
                    None => {
                        if let SearchViewState::Loaded(result) = &self.search_state {
                            for song in &result.songs {
                                collect_thumbnail_url(song.thumbnail_url.as_deref(), &mut urls);
                            }
                            for item in &result.items {
                                collect_thumbnail_url(item.thumbnail_url.as_deref(), &mut urls);
                            }
                        }
                        if let SearchSuggestionViewState::Loaded(suggestions) =
                            &self.search_suggestion_state
                        {
                            for song in &suggestions.songs {
                                collect_thumbnail_url(song.thumbnail_url.as_deref(), &mut urls);
                            }
                            for item in &suggestions.items {
                                collect_thumbnail_url(item.thumbnail_url.as_deref(), &mut urls);
                            }
                        }
                    }
                    Some(BrowseViewState::Loading(_) | BrowseViewState::Failed(_, _)) => {}
                }
            }
        }

        self.thumbnail_tasks.retain(|url, _| urls.contains(url));
        self.thumbnail_failures.retain(|url| urls.contains(url));
        let Some(cache) = self.thumbnail_cache.clone() else {
            return;
        };
        for url in urls {
            if self.thumbnail_images.contains_key(&url)
                || self.thumbnail_failures.contains(&url)
                || self.thumbnail_tasks.contains_key(&url)
            {
                continue;
            }
            let cache = cache.clone();
            let task_url = url.clone();
            let task = cx.spawn(async move |this, cx| {
                let result = cache.load(&task_url).await;
                this.update(cx, |this, cx| {
                    if let Some(completed_task) = this.thumbnail_tasks.remove(&task_url) {
                        completed_task.detach();
                    }
                    match result {
                        Ok(image) => {
                            this.thumbnail_failures.remove(&task_url);
                            let image = Arc::new(Image::from_bytes(image.format, image.bytes));
                            if this
                                .thumbnail_images
                                .insert(task_url.clone(), image)
                                .is_none()
                            {
                                this.thumbnail_order.push_back(task_url.clone());
                            }
                            while this.thumbnail_images.len() > MAX_MEMORY_THUMBNAILS {
                                let Some(evicted_url) = this.thumbnail_order.pop_front() else {
                                    break;
                                };
                                if let Some(evicted) = this.thumbnail_images.remove(&evicted_url) {
                                    evicted.remove_asset(cx);
                                }
                            }
                        }
                        Err(error) => {
                            tracing::warn!(%error, "thumbnail load failed");
                            this.thumbnail_failures.insert(task_url);
                        }
                    }
                    cx.notify();
                })
                .ok();
            });
            self.thumbnail_tasks.insert(url, task);
        }
    }

    fn refresh_audio_outputs(&mut self, cx: &mut Context<Self>) {
        if let Err(error) = self.audio_player.refresh_output_devices() {
            tracing::warn!(%error, "audio output refresh could not be queued");
        }
        cx.notify();
    }

    fn select_audio_output(&mut self, device_id: String, cx: &mut Context<Self>) {
        if let Err(error) = self.audio_player.select_output_device(device_id) {
            tracing::warn!(%error, "audio output switch could not be queued");
        }
        cx.notify();
    }

    fn is_favorite(&self, video_id: &str) -> bool {
        matches!(
            &self.favorites_state,
            StoredViewState::Loaded(favorites)
                if favorites.iter().any(|entry| entry.song.video_id == video_id)
        )
    }

    fn podcast_busy(&self) -> bool {
        self.podcast_operation != PodcastOperation::Idle
    }

    fn is_podcast_saved(&self, podcast_id: &str) -> bool {
        matches!(
            &self.podcast_subscriptions,
            StoredViewState::Loaded(podcasts)
                if podcasts.iter().any(|podcast| podcast.podcast_id == podcast_id)
        )
    }

    fn is_episode_saved(&self, video_id: &str) -> bool {
        matches!(
            &self.episodes_for_later,
            StoredViewState::Loaded(episodes)
                if episodes.iter().any(|episode| episode.song.video_id == video_id)
        )
    }

    fn saved_episode_position(&self, video_id: &str) -> Option<Duration> {
        match &self.episodes_for_later {
            StoredViewState::Loaded(episodes) => episodes
                .iter()
                .find(|episode| episode.song.video_id == video_id)
                .and_then(|episode| episode.playback_position),
            StoredViewState::Loading | StoredViewState::Failed(_) => None,
        }
    }

    fn sync_podcast_library(&mut self, cx: &mut Context<Self>) {
        if self.podcast_busy() || !self.account_ready() {
            return;
        }
        self.podcast_state_revision = self.podcast_state_revision.wrapping_add(1);
        self.podcast_operation = PodcastOperation::Syncing;
        self.podcast_error = None;
        self.podcast_notice = None;
        let client = self.search_client.clone();
        let store = self.store.clone();
        self.podcast_task = Some(cx.spawn(async move |this, cx| {
            let result = async {
                let remote = client.podcast_library_snapshot().await?;
                let timestamp = unix_time_ms();
                let podcasts = remote
                    .podcasts
                    .into_iter()
                    .map(|item| PodcastSubscription {
                        channel_id: item
                            .browse_id
                            .starts_with("UC")
                            .then(|| item.browse_id.clone()),
                        podcast_id: item.browse_id,
                        title: item.title,
                        author: (!item.subtitle.trim().is_empty()
                            && item.subtitle != BrowseKind::Podcast.label())
                        .then_some(item.subtitle),
                        thumbnail_url: item.thumbnail_url,
                        subscribed_at_ms: timestamp,
                    })
                    .collect::<Vec<_>>();
                let episodes = remote
                    .episodes
                    .into_iter()
                    .map(|episode| episode.song)
                    .collect::<Vec<_>>();
                let summary = store.reconcile_podcast_library(podcasts, episodes).await?;
                let (podcasts, episodes) =
                    futures::join!(store.podcast_subscriptions(), store.episodes_for_later());
                Ok::<_, AppError>((summary, podcasts?, episodes?))
            }
            .await;
            this.update(cx, |this, cx| {
                this.podcast_task = None;
                this.podcast_operation = PodcastOperation::Idle;
                match result {
                    Ok((summary, podcasts, episodes)) => {
                        this.podcast_subscriptions = StoredViewState::Loaded(podcasts);
                        this.episodes_for_later = StoredViewState::Loaded(episodes);
                        this.podcast_notice = Some(format!(
                            "Podcast library synced: {} shows and {} episodes{}{}.",
                            summary.podcast_count,
                            summary.episode_count,
                            if summary.removed_podcast_count + summary.removed_episode_count > 0 {
                                format!(
                                    "; removed {} stale local items",
                                    summary.removed_podcast_count + summary.removed_episode_count
                                )
                            } else {
                                String::new()
                            },
                            if summary.skipped_podcast_tombstones > 0 {
                                format!(
                                    "; kept {} locally removed shows hidden",
                                    summary.skipped_podcast_tombstones
                                )
                            } else {
                                String::new()
                            }
                        ));
                    }
                    Err(error) => {
                        let message = this.record_cloud_request_failure(&error);
                        this.podcast_error = Some(format!(
                            "Podcast library sync failed; existing local data was kept: {message}"
                        ));
                    }
                }
                this.refresh_visible_thumbnails(cx);
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    fn toggle_podcast_subscription(
        &mut self,
        item: BrowseItem,
        channel_id: Option<String>,
        cx: &mut Context<Self>,
    ) {
        if self.podcast_busy() || item.kind != BrowseKind::Podcast {
            return;
        }
        let saved = !self.is_podcast_saved(&item.browse_id);
        let podcast = PodcastSubscription {
            podcast_id: item.browse_id.clone(),
            title: item.title,
            author: (!item.subtitle.trim().is_empty()).then_some(item.subtitle),
            thumbnail_url: item.thumbnail_url,
            channel_id,
            subscribed_at_ms: unix_time_ms(),
        };
        let store = self.store.clone();
        let client = self.search_client.clone();
        let sync_remote = self.account_ready();
        self.podcast_state_revision = self.podcast_state_revision.wrapping_add(1);
        self.podcast_operation = PodcastOperation::SavingPodcast;
        self.podcast_error = None;
        self.podcast_notice = None;
        self.podcast_task = Some(cx.spawn(async move |this, cx| {
            let result = async {
                store
                    .set_podcast_subscription(podcast.clone(), saved)
                    .await?;
                let remote = if sync_remote {
                    Some(client.set_podcast_saved(&podcast.podcast_id, saved).await)
                } else {
                    None
                };
                let podcasts = store.podcast_subscriptions().await?;
                Ok::<_, AppError>((podcasts, remote))
            }
            .await;
            this.update(cx, |this, cx| {
                this.podcast_task = None;
                this.podcast_operation = PodcastOperation::Idle;
                match result {
                    Ok((podcasts, remote)) => {
                        this.podcast_subscriptions = StoredViewState::Loaded(podcasts);
                        match remote {
                            Some(Ok(())) => {
                                this.podcast_notice = Some(if saved {
                                    "Podcast saved locally and synced to YouTube Music.".into()
                                } else {
                                    "Podcast removed locally and from YouTube Music.".into()
                                });
                            }
                            Some(Err(error)) => {
                                let message = this.record_cloud_request_failure(&error);
                                this.podcast_error = Some(format!(
                                    "The local podcast library was updated, but YouTube Music sync failed: {message}"
                                ));
                            }
                            None => {
                                this.podcast_notice = Some(if saved {
                                    "Podcast saved on this device. Sign in to sync it to YouTube Music."
                                        .into()
                                } else {
                                    "Podcast removed from this device.".into()
                                });
                            }
                        }
                    }
                    Err(error) => this.podcast_error = Some(error.to_string()),
                }
                this.refresh_visible_thumbnails(cx);
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    fn toggle_episode_for_later(&mut self, song: Song, cx: &mut Context<Self>) {
        if self.podcast_busy() || !song.is_episode {
            return;
        }
        let saved = !self.is_episode_saved(&song.video_id);
        let store = self.store.clone();
        let client = self.search_client.clone();
        let sync_remote = self.account_ready();
        self.podcast_state_revision = self.podcast_state_revision.wrapping_add(1);
        self.podcast_operation = PodcastOperation::SavingEpisode;
        self.podcast_error = None;
        self.podcast_notice = None;
        self.podcast_task = Some(cx.spawn(async move |this, cx| {
            let result = async {
                store.set_episode_for_later(song.clone(), saved).await?;
                let remote = if sync_remote {
                    Some(client.set_episode_saved(&song.video_id, saved).await)
                } else {
                    None
                };
                let episodes = store.episodes_for_later().await?;
                Ok::<_, AppError>((episodes, remote))
            }
            .await;
            this.update(cx, |this, cx| {
                this.podcast_task = None;
                this.podcast_operation = PodcastOperation::Idle;
                match result {
                    Ok((episodes, remote)) => {
                        this.episodes_for_later = StoredViewState::Loaded(episodes);
                        match remote {
                            Some(Ok(())) => {
                                this.podcast_notice = Some(if saved {
                                    "Episode saved for later locally and on YouTube Music.".into()
                                } else {
                                    "Episode removed from both later lists.".into()
                                });
                            }
                            Some(Err(error)) => {
                                let message = this.record_cloud_request_failure(&error);
                                this.podcast_error = Some(format!(
                                    "The local Episodes for Later list was updated, but YouTube Music sync failed: {message}"
                                ));
                            }
                            None => {
                                this.podcast_notice = Some(if saved {
                                    "Episode saved for later on this device. Sign in to sync it to YouTube Music."
                                        .into()
                                } else {
                                    "Episode removed from this device's later list.".into()
                                });
                            }
                        }
                    }
                    Err(error) => this.podcast_error = Some(error.to_string()),
                }
                this.refresh_visible_thumbnails(cx);
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    fn toggle_favorite(&mut self, song: Song, cx: &mut Context<Self>) {
        if self.library_busy() {
            return;
        }
        let favorite = !self.is_favorite(&song.video_id);
        let lastfm_song = song.clone();
        let store = self.store.clone();
        self.library_error = None;
        cx.spawn(async move |this, cx| {
            let result = store.set_favorite(song, favorite).await;
            let favorites = match result {
                Ok(()) => store.favorites(500).await,
                Err(error) => Err(error),
            };
            this.update(cx, |this, cx| {
                match favorites {
                    Ok(favorites) => {
                        this.favorites_state = StoredViewState::Loaded(favorites);
                        this.sync_lastfm_love(lastfm_song, favorite, cx);
                        this.reload_daily_discover(cx);
                    }
                    Err(error) => {
                        this.library_error = Some(error.to_string());
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn sync_lastfm_love(&mut self, song: Song, loved: bool, cx: &mut Context<Self>) {
        if song.is_episode || !self.settings.lastfm_sync_likes || self.lastfm_session.is_none() {
            return;
        }
        let Some(client) = self.lastfm_client.clone() else {
            return;
        };
        let track = match LastFmTrack::from_song(&song) {
            Ok(track) => track,
            Err(error) => {
                self.lastfm_error = Some(error.to_string());
                cx.notify();
                return;
            }
        };
        cx.spawn(async move |this, cx| {
            let result = client.set_love_status(&track, loved).await;
            this.update(cx, |this, cx| {
                if let Err(error) = result {
                    this.lastfm_error = Some(error.to_string());
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn library_busy(&self) -> bool {
        self.library_operation != LibraryOperation::Idle
    }

    fn local_library_retry_targets(&self) -> LocalLibraryRetryTargets {
        LocalLibraryRetryTargets::from_states(
            &self.history_state,
            &self.favorites_state,
            &self.podcast_subscriptions,
            &self.episodes_for_later,
            &self.playlists_state,
            &self.downloads_state,
        )
    }

    fn retry_failed_local_library(&mut self, cx: &mut Context<Self>) {
        if self.library_busy() || self.podcast_busy() {
            return;
        }
        let targets = self.local_library_retry_targets();
        if !targets.any() {
            return;
        }

        if targets.history {
            self.history_state = HistoryViewState::Loading;
        }
        if targets.favorites {
            self.favorites_state = StoredViewState::Loading;
        }
        if targets.podcasts {
            self.podcast_subscriptions = StoredViewState::Loading;
        }
        if targets.episodes {
            self.episodes_for_later = StoredViewState::Loading;
        }
        if targets.playlists {
            self.playlists_state = StoredViewState::Loading;
        }
        if targets.downloads {
            self.downloads_state = StoredViewState::Loading;
        }

        self.library_operation = LibraryOperation::RetryingLibrary;
        self.library_error = None;
        let store = self.store.clone();
        let sort = self.playlist_sort;
        let direction = self.playlist_sort_direction;
        let podcast_revision = self.podcast_state_revision;
        let operation = cx.background_executor().spawn(async move {
            futures::join!(
                async {
                    if targets.history {
                        Some(store.recent_history(100).await)
                    } else {
                        None
                    }
                },
                async {
                    if targets.favorites {
                        Some(store.favorites(500).await)
                    } else {
                        None
                    }
                },
                async {
                    if targets.podcasts {
                        Some(store.podcast_subscriptions().await)
                    } else {
                        None
                    }
                },
                async {
                    if targets.episodes {
                        Some(store.episodes_for_later().await)
                    } else {
                        None
                    }
                },
                async {
                    if targets.playlists {
                        Some(store.playlists_sorted(sort, direction).await)
                    } else {
                        None
                    }
                },
                async {
                    if targets.downloads {
                        Some(store.downloads().await)
                    } else {
                        None
                    }
                },
            )
        });
        cx.spawn(async move |this, cx| {
            let (history, favorites, podcasts, episodes, playlists, downloads) = operation.await;
            this.update(cx, |this, cx| {
                let mut errors = Vec::new();
                if let Some(result) = history {
                    match result {
                        Ok(history) => this.history_state = HistoryViewState::Loaded(history),
                        Err(error) => {
                            let message = error.to_string();
                            errors.push(format!("history: {message}"));
                            this.history_state = HistoryViewState::Failed(message);
                        }
                    }
                }
                if let Some(result) = favorites {
                    match result {
                        Ok(favorites) => this.favorites_state = StoredViewState::Loaded(favorites),
                        Err(error) => {
                            let message = error.to_string();
                            errors.push(format!("favourites: {message}"));
                            this.favorites_state = StoredViewState::Failed(message);
                        }
                    }
                }
                if let Some(result) = podcasts
                    && this.podcast_state_revision == podcast_revision
                {
                    match result {
                        Ok(podcasts) => {
                            this.podcast_subscriptions = StoredViewState::Loaded(podcasts)
                        }
                        Err(error) => {
                            let message = error.to_string();
                            errors.push(format!("podcasts: {message}"));
                            this.podcast_subscriptions = StoredViewState::Failed(message);
                        }
                    }
                }
                if let Some(result) = episodes
                    && this.podcast_state_revision == podcast_revision
                {
                    match result {
                        Ok(episodes) => this.episodes_for_later = StoredViewState::Loaded(episodes),
                        Err(error) => {
                            let message = error.to_string();
                            errors.push(format!("episodes: {message}"));
                            this.episodes_for_later = StoredViewState::Failed(message);
                        }
                    }
                }
                if let Some(result) = playlists {
                    match result {
                        Ok(playlists) => this.playlists_state = StoredViewState::Loaded(playlists),
                        Err(error) => {
                            let message = error.to_string();
                            errors.push(format!("playlists: {message}"));
                            this.playlists_state = StoredViewState::Failed(message);
                        }
                    }
                }
                if let Some(result) = downloads {
                    match result {
                        Ok(downloads) => this.downloads_state = StoredViewState::Loaded(downloads),
                        Err(error) => {
                            let message = error.to_string();
                            errors.push(format!("downloads: {message}"));
                            this.downloads_state = StoredViewState::Failed(message);
                        }
                    }
                }
                this.library_operation = LibraryOperation::Idle;
                this.library_error = (!errors.is_empty()).then(|| {
                    format!(
                        "Some local library sections are still unavailable: {}",
                        errors.join("; ")
                    )
                });
                this.refresh_visible_thumbnails(cx);
                cx.notify();
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    fn create_playlist(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.library_busy() {
            return;
        }
        let name = self.playlist_name_input.read(cx).value().trim().to_owned();
        if name.is_empty() {
            self.library_error = Some("Enter a playlist name first.".into());
            cx.notify();
            return;
        }
        self.playlist_name_input
            .update(cx, |input, cx| input.set_value("", window, cx));
        self.library_error = None;
        self.library_operation = LibraryOperation::CreatingPlaylist;
        let store = self.store.clone();
        let sort = self.playlist_sort;
        let direction = self.playlist_sort_direction;
        cx.spawn(async move |this, cx| {
            let result = store.create_playlist(name).await;
            let playlists = match result {
                Ok(_) => store.playlists_sorted(sort, direction).await,
                Err(error) => Err(error),
            };
            this.update(cx, |this, cx| {
                this.library_operation = LibraryOperation::Idle;
                match playlists {
                    Ok(playlists) => {
                        this.playlists_state = StoredViewState::Loaded(playlists);
                    }
                    Err(error) => {
                        this.library_error = Some(error.to_string());
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn set_playlist_sort(&mut self, sort: PlaylistSort, cx: &mut Context<Self>) {
        if self.library_busy() || self.playlist_sort == sort {
            return;
        }
        let previous_sort = self.playlist_sort;
        let previous_direction = self.playlist_sort_direction;
        self.playlist_sort = sort;
        self.reload_sorted_playlists(previous_sort, previous_direction, cx);
    }

    fn toggle_playlist_sort_direction(&mut self, cx: &mut Context<Self>) {
        if self.library_busy() {
            return;
        }
        let previous_sort = self.playlist_sort;
        let previous_direction = self.playlist_sort_direction;
        self.playlist_sort_direction = match self.playlist_sort_direction {
            SortDirection::Ascending => SortDirection::Descending,
            SortDirection::Descending => SortDirection::Ascending,
        };
        self.reload_sorted_playlists(previous_sort, previous_direction, cx);
    }

    fn reload_sorted_playlists(
        &mut self,
        previous_sort: PlaylistSort,
        previous_direction: SortDirection,
        cx: &mut Context<Self>,
    ) {
        self.library_error = None;
        self.library_operation = LibraryOperation::SortingPlaylists;
        let store = self.store.clone();
        let sort = self.playlist_sort;
        let direction = self.playlist_sort_direction;
        cx.spawn(async move |this, cx| {
            let playlists = store.playlists_sorted(sort, direction).await;
            this.update(cx, |this, cx| {
                this.library_operation = LibraryOperation::Idle;
                match playlists {
                    Ok(playlists) => {
                        this.playlists_state = StoredViewState::Loaded(playlists);
                    }
                    Err(error) => {
                        this.playlist_sort = previous_sort;
                        this.playlist_sort_direction = previous_direction;
                        this.library_error = Some(error.to_string());
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    fn open_playlist(&mut self, playlist: LocalPlaylist, cx: &mut Context<Self>) {
        self.library_error = None;
        self.playlist_detail = Some(PlaylistDetailState::Loading(playlist.clone()));
        let store = self.store.clone();
        cx.spawn(async move |this, cx| {
            let result = store.playlist_songs(playlist.id).await;
            this.update(cx, |this, cx| {
                this.playlist_detail = Some(match result {
                    Ok(songs) => PlaylistDetailState::Loaded(playlist, songs),
                    Err(error) => PlaylistDetailState::Failed(playlist, error.to_string()),
                });
                cx.notify();
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    fn open_playlist_picker(&mut self, song: Song, cx: &mut Context<Self>) {
        if matches!(&self.playlists_state, StoredViewState::Loaded(playlists) if playlists.is_empty())
        {
            self.model.navigate(Route::Library);
            self.library_error = Some("Create a playlist before adding songs.".into());
        } else {
            self.queue_visible = false;
            self.lyrics_visible = false;
            self.playback_parameters_visible = false;
            self.cloud_playlist_picker_song = None;
            if matches!(self.lyrics_state, LyricsViewState::Loading(_)) {
                self.lyrics_state = LyricsViewState::Idle;
            }
            self.lyrics_task = None;
            self.playlist_picker_song = Some(song);
        }
        cx.notify();
    }

    fn add_song_to_playlist(&mut self, playlist_id: i64, song: Song, cx: &mut Context<Self>) {
        if self.library_busy() {
            return;
        }
        self.playlist_picker_song = None;
        self.library_error = None;
        self.library_operation = LibraryOperation::AddingToPlaylist;
        let store = self.store.clone();
        let sort = self.playlist_sort;
        let direction = self.playlist_sort_direction;
        cx.spawn(async move |this, cx| {
            let result = store.add_to_playlist(playlist_id, song).await;
            let playlists = match result {
                Ok(()) => store.playlists_sorted(sort, direction).await,
                Err(error) => Err(error),
            };
            this.update(cx, |this, cx| {
                this.library_operation = LibraryOperation::Idle;
                match playlists {
                    Ok(playlists) => {
                        this.playlists_state = StoredViewState::Loaded(playlists);
                    }
                    Err(error) => this.library_error = Some(error.to_string()),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn remove_from_playlist(
        &mut self,
        playlist: LocalPlaylist,
        song: Song,
        cx: &mut Context<Self>,
    ) {
        if self.library_busy() {
            return;
        }
        self.library_error = None;
        self.library_operation = LibraryOperation::RemovingFromPlaylist;
        let store = self.store.clone();
        let sort = self.playlist_sort;
        let direction = self.playlist_sort_direction;
        cx.spawn(async move |this, cx| {
            let result = store.remove_from_playlist(playlist.id, song.video_id).await;
            let (songs, playlists) = match result {
                Ok(()) => futures::join!(
                    store.playlist_songs(playlist.id),
                    store.playlists_sorted(sort, direction)
                ),
                Err(error) => {
                    this.update(cx, |this, cx| {
                        this.library_operation = LibraryOperation::Idle;
                        this.library_error = Some(error.to_string());
                        cx.notify();
                    })
                    .ok();
                    return;
                }
            };
            this.update(cx, |this, cx| {
                this.library_operation = LibraryOperation::Idle;
                match songs {
                    Ok(songs) => {
                        let mut playlist = playlist;
                        playlist.song_count = songs.len();
                        this.playlist_detail = Some(PlaylistDetailState::Loaded(playlist, songs));
                    }
                    Err(error) => this.library_error = Some(error.to_string()),
                }
                if let Ok(playlists) = playlists {
                    this.playlists_state = StoredViewState::Loaded(playlists);
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn open_rename_playlist_dialog(
        &mut self,
        playlist: LocalPlaylist,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.library_busy() {
            return;
        }
        self.playlist_rename_input.update(cx, |input, cx| {
            input.set_value(&playlist.name, window, cx);
        });
        let input = self.playlist_rename_input.clone();
        let weak = cx.weak_entity();
        window.open_alert_dialog(cx, move |dialog, window, cx| {
            input.update(cx, |input, cx| input.focus(window, cx));
            let input_for_submit = input.clone();
            let weak = weak.clone();
            let playlist = playlist.clone();
            dialog
                .title("Rename playlist")
                .description("Choose a unique name for this local playlist.")
                .child(Input::new(&input))
                .button_props(
                    DialogButtonProps::default()
                        .ok_text("Rename")
                        .cancel_text("Cancel")
                        .show_cancel(true),
                )
                .on_ok(move |_, window, cx| {
                    let name = input_for_submit.read(cx).value().trim().to_owned();
                    if name.is_empty() {
                        window.push_notification("Playlist name cannot be empty.", cx);
                        return false;
                    }
                    weak.update(cx, |this, cx| {
                        this.rename_playlist(playlist.clone(), name.clone(), cx);
                    })
                    .ok();
                    true
                })
        });
    }

    fn rename_playlist(&mut self, playlist: LocalPlaylist, name: String, cx: &mut Context<Self>) {
        if self.library_busy() {
            return;
        }
        self.library_error = None;
        self.library_operation = LibraryOperation::RenamingPlaylist;
        let store = self.store.clone();
        let sort = self.playlist_sort;
        let direction = self.playlist_sort_direction;
        cx.spawn(async move |this, cx| {
            let renamed = store.rename_playlist(playlist.id, name).await;
            let result = match renamed {
                Ok(renamed) => store
                    .playlists_sorted(sort, direction)
                    .await
                    .map(|playlists| (renamed, playlists)),
                Err(error) => Err(error),
            };
            this.update(cx, |this, cx| {
                this.library_operation = LibraryOperation::Idle;
                match result {
                    Ok((renamed, playlists)) => {
                        this.playlists_state = StoredViewState::Loaded(playlists);
                        this.playlist_detail =
                            this.playlist_detail.take().map(|detail| match detail {
                                PlaylistDetailState::Loading(_) => {
                                    PlaylistDetailState::Loading(renamed.clone())
                                }
                                PlaylistDetailState::Loaded(_, songs) => {
                                    PlaylistDetailState::Loaded(renamed.clone(), songs)
                                }
                                PlaylistDetailState::Failed(_, error) => {
                                    PlaylistDetailState::Failed(renamed, error)
                                }
                            });
                    }
                    Err(error) => this.library_error = Some(error.to_string()),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    fn confirm_delete_playlist(
        &mut self,
        playlist: LocalPlaylist,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.library_busy() {
            return;
        }
        let weak = cx.weak_entity();
        window.open_alert_dialog(cx, move |dialog, _, _| {
            let weak = weak.clone();
            let playlist = playlist.clone();
            dialog
                .title("Delete playlist?")
                .description(format!(
                    "\"{}\" and its local song list will be removed. This cannot be undone.",
                    playlist.name
                ))
                .button_props(
                    DialogButtonProps::default()
                        .ok_text("Delete")
                        .ok_variant(ButtonVariant::Danger)
                        .cancel_text("Cancel")
                        .show_cancel(true),
                )
                .on_ok(move |_, _, cx| {
                    weak.update(cx, |this, cx| {
                        this.delete_playlist(playlist.id, cx);
                    })
                    .ok();
                    true
                })
        });
    }

    fn delete_playlist(&mut self, playlist_id: i64, cx: &mut Context<Self>) {
        if self.library_busy() {
            return;
        }
        self.library_error = None;
        self.library_operation = LibraryOperation::DeletingPlaylist;
        let store = self.store.clone();
        let sort = self.playlist_sort;
        let direction = self.playlist_sort_direction;
        cx.spawn(async move |this, cx| {
            let result = store.delete_playlist(playlist_id).await;
            let playlists = match result {
                Ok(()) => store.playlists_sorted(sort, direction).await,
                Err(error) => Err(error),
            };
            this.update(cx, |this, cx| {
                this.library_operation = LibraryOperation::Idle;
                match playlists {
                    Ok(playlists) => {
                        this.playlist_detail = None;
                        this.playlists_state = StoredViewState::Loaded(playlists);
                    }
                    Err(error) => this.library_error = Some(error.to_string()),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    fn confirm_clear_history(
        &mut self,
        entry_count: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.library_busy() || entry_count == 0 {
            return;
        }
        let weak = cx.weak_entity();
        window.open_alert_dialog(cx, move |dialog, _, _| {
            let weak = weak.clone();
            dialog
                .title("Clear listening history?")
                .description(format!(
                    "All {entry_count} listening history entries will be removed from this device. Favourites and playlists will be kept."
                ))
                .button_props(
                    DialogButtonProps::default()
                        .ok_text("Clear history")
                        .ok_variant(ButtonVariant::Danger)
                        .cancel_text("Cancel")
                        .show_cancel(true),
                )
                .on_ok(move |_, _, cx| {
                    weak.update(cx, |this, cx| this.clear_history(cx)).ok();
                    true
                })
        });
    }

    fn clear_history(&mut self, cx: &mut Context<Self>) {
        if self.library_busy() {
            return;
        }
        self.library_error = None;
        self.library_operation = LibraryOperation::ClearingHistory;
        let store = self.store.clone();
        cx.spawn(async move |this, cx| {
            let result = store.clear_history().await;
            this.update(cx, |this, cx| {
                this.library_operation = LibraryOperation::Idle;
                match result {
                    Ok(()) => {
                        this.history_state = HistoryViewState::Loaded(Vec::new());
                        this.keep_listening_state = StoredViewState::Loaded(Vec::new());
                        this.forgotten_favorites_state = StoredViewState::Loaded(Vec::new());
                        this.stats_state = StoredViewState::Loading;
                    }
                    Err(error) => this.library_error = Some(error.to_string()),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    fn remove_local_history_entry(&mut self, id: i64, cx: &mut Context<Self>) {
        if self.library_busy() {
            return;
        }
        self.library_error = None;
        self.library_operation = LibraryOperation::RemovingHistory;
        let store = self.store.clone();
        cx.spawn(async move |this, cx| {
            let deleted = store.delete_history_entry(id).await;
            let snapshots = if deleted.is_ok() {
                Some(futures::join!(
                    store.recent_history(100),
                    store.keep_listening(15, 5),
                    store.forgotten_favorites(20),
                ))
            } else {
                None
            };
            this.update(cx, |this, cx| {
                this.library_operation = LibraryOperation::Idle;
                match (deleted, snapshots) {
                    (Ok(()), Some((history, keep_listening, forgotten_favorites))) => {
                        this.history_state = match history {
                            Ok(history) => HistoryViewState::Loaded(history),
                            Err(error) => HistoryViewState::Failed(error.to_string()),
                        };
                        this.keep_listening_state = match keep_listening {
                            Ok(songs) => StoredViewState::Loaded(songs),
                            Err(error) => StoredViewState::Failed(error.to_string()),
                        };
                        this.forgotten_favorites_state = match forgotten_favorites {
                            Ok(songs) => StoredViewState::Loaded(songs),
                            Err(error) => StoredViewState::Failed(error.to_string()),
                        };
                        this.stats_state = StoredViewState::Loading;
                    }
                    (Err(error), _) => this.library_error = Some(error.to_string()),
                    (Ok(()), None) => {}
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    fn playback_duration(&self) -> Option<Duration> {
        self.audio_player
            .snapshot()
            .duration
            .or_else(|| self.current_song.as_ref().and_then(|song| song.duration))
    }

    fn restore_session(&mut self, session: PersistedSession, cx: &mut Context<Self>) {
        let PersistedSession {
            queue,
            current_index,
            position,
            volume,
            repeat_mode,
            shuffle_enabled,
            playback_source,
        } = session;
        self.repeat_mode = repeat_mode;
        self.shuffle_enabled = shuffle_enabled;
        self.audio_player.set_volume(volume);
        let items = queue.into_iter().map(|song| QueueItem { song }).collect();
        let current = self
            .queue
            .replace(items, current_index)
            .map(|item| item.song.clone());
        if let Some(song) = current {
            self.set_current_song(song, cx);
        } else {
            self.current_song = None;
            self.lyrics_task = None;
            self.lyrics_state = LyricsViewState::Idle;
            self.lyrics_active_line = None;
        }
        self.pending_resume_position = self
            .current_song
            .as_ref()
            .map(|_| position)
            .filter(|position| !position.is_zero());
        self.persisted_playback_source = playback_source.filter(|source| {
            self.current_song
                .as_ref()
                .is_some_and(|song| song.video_id == source.video_id)
        });
    }

    fn persisted_session(&self) -> PersistedSession {
        let snapshot = self.audio_player.snapshot();
        let position = if self.resolving_playback {
            self.pending_resume_position.unwrap_or(Duration::ZERO)
        } else {
            self.pending_resume_position.unwrap_or(snapshot.position)
        };
        PersistedSession {
            queue: self
                .queue
                .items()
                .iter()
                .map(|item| item.song.clone())
                .collect(),
            current_index: self.queue.current_index(),
            position,
            volume: snapshot.volume,
            repeat_mode: self.repeat_mode,
            shuffle_enabled: self.shuffle_enabled,
            playback_source: self.persisted_playback_source.clone().filter(|source| {
                self.current_song
                    .as_ref()
                    .is_some_and(|song| song.video_id == source.video_id)
            }),
        }
    }

    fn save_session(&mut self, cx: &mut Context<Self>) {
        self.last_session_save = Instant::now();
        let store = self.store.clone();
        let session = self.persisted_session();
        cx.spawn(async move |_, _| {
            if let Err(error) = store.save_session(session).await {
                tracing::warn!(%error, "playback session save failed");
            }
        })
        .detach();
    }

    fn record_current_history(&mut self, cx: &mut Context<Self>) {
        let Some(song) = self.current_song.clone() else {
            return;
        };
        self.history_recorded_for_current = true;
        let play_time = self.played_this_track;
        let store = self.store.clone();
        let client = self.search_client.clone();
        let sync_remote = self.settings.youtube_history_sync && self.account_ready();
        let playback_tracking = self
            .active_playback_source
            .as_ref()
            .filter(|source| source.video_id == song.video_id)
            .and_then(|source| source.playback_tracking.clone());
        let video_id = song.video_id.clone();
        cx.spawn(async move |this, cx| {
            let local_snapshot = async {
                store.record_history(song, play_time).await?;
                Ok::<_, AppError>(futures::join!(
                    store.recent_history(100),
                    store.keep_listening(15, 5),
                    store.forgotten_favorites(20),
                ))
            };
            let remote_registration = async {
                if !sync_remote {
                    return Ok(());
                }
                let tracking = match playback_tracking {
                    Some(tracking) => tracking,
                    None => client
                        .resolve_playback_tracking(&video_id)
                        .await?
                        .ok_or_else(|| {
                            AppError::Protocol(
                                "player response contained no playback history endpoint".into(),
                            )
                        })?,
                };
                client.register_playback(&tracking).await
            };
            let (snapshot, remote_result) = futures::join!(local_snapshot, remote_registration);
            this.update(cx, |this, cx| {
                match snapshot {
                    Ok((history, keep_listening, forgotten_favorites)) => {
                        this.history_state = match history {
                            Ok(history) => HistoryViewState::Loaded(history),
                            Err(error) => HistoryViewState::Failed(error.to_string()),
                        };
                        this.keep_listening_state = match keep_listening {
                            Ok(songs) => StoredViewState::Loaded(songs),
                            Err(error) => StoredViewState::Failed(error.to_string()),
                        };
                        this.forgotten_favorites_state = match forgotten_favorites {
                            Ok(songs) => StoredViewState::Loaded(songs),
                            Err(error) => StoredViewState::Failed(error.to_string()),
                        };
                    }
                    Err(error) => {
                        let message = error.to_string();
                        this.history_state = HistoryViewState::Failed(message.clone());
                        this.keep_listening_state = StoredViewState::Failed(message.clone());
                        this.forgotten_favorites_state = StoredViewState::Failed(message);
                    }
                }
                if sync_remote {
                    match remote_result {
                        Ok(()) => this.remote_history_error = None,
                        Err(error) => {
                            let message = this.record_cloud_request_failure(&error);
                            this.remote_history_error = Some(message);
                        }
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn play_search_result(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let SearchViewState::Loaded(result) = &self.search_state else {
            return;
        };
        self.play_song_collection(result.songs.clone(), index, window, cx);
    }

    fn reset_radio_queue(&mut self) {
        self.radio_task = None;
        self.radio_state = RadioQueueState::Idle;
        self.queue_generation = self.queue_generation.wrapping_add(1);
    }

    fn start_radio_from_current(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.reject_guest_playback_control(cx) {
            return;
        }
        if self
            .current_song
            .as_ref()
            .is_some_and(|song| song.is_episode)
        {
            return;
        }
        let Some(seed_video_id) = self.current_song.as_ref().map(|song| song.video_id.clone())
        else {
            return;
        };
        self.reset_radio_queue();
        self.request_radio(
            RadioRequest::Initial {
                seed_video_id,
                replace_future: true,
            },
            window,
            cx,
        );
    }

    fn retry_radio(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.reject_guest_playback_control(cx) {
            return;
        }
        let RadioQueueState::Failed(request, _) = &self.radio_state else {
            return;
        };
        self.request_radio(request.clone(), window, cx);
    }

    fn maybe_refill_radio(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.listen_together_is_guest()
            || !self.settings.auto_radio
            || self.current_song.is_none()
            || self
                .current_song
                .as_ref()
                .is_some_and(|song| song.is_episode)
            || self.queue.remaining_after_current() > RADIO_PREFETCH_THRESHOLD
        {
            return;
        }
        let playback_state = if self.resolving_playback {
            PlaybackState::Loading
        } else {
            self.audio_player.snapshot().state
        };
        if matches!(playback_state, PlaybackState::Idle | PlaybackState::Failed) {
            return;
        }

        let request = match &self.radio_state {
            RadioQueueState::Idle => RadioRequest::Initial {
                seed_video_id: self
                    .current_song
                    .as_ref()
                    .expect("the current song was checked above")
                    .video_id
                    .clone(),
                replace_future: false,
            },
            RadioQueueState::Active(session)
                if session.continuation.as_ref().is_some_and(|token| {
                    !token.is_empty() && !session.seen_continuations.contains(token)
                }) =>
            {
                RadioRequest::Continuation(Box::new(session.clone()))
            }
            RadioQueueState::Loading(_)
            | RadioQueueState::Active(_)
            | RadioQueueState::Exhausted(_)
            | RadioQueueState::Failed(_, _) => return,
        };
        self.request_radio(request, window, cx);
    }

    fn request_radio(
        &mut self,
        request: RadioRequest,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if matches!(self.radio_state, RadioQueueState::Loading(_)) {
            return;
        }
        let generation = self.queue_generation;
        let client = self.search_client.clone();
        self.radio_state = RadioQueueState::Loading(request.clone());
        self.radio_task = Some(cx.spawn_in(window, async move |this, cx| {
            let result = match &request {
                RadioRequest::Initial { seed_video_id, .. } => client.radio(seed_video_id).await,
                RadioRequest::Continuation(session) => {
                    let continuation = session.continuation.as_deref().ok_or_else(|| {
                        crate::AppError::Protocol("radio session has no continuation".into())
                    });
                    match continuation {
                        Ok(continuation) => {
                            client
                                .radio_continuation(session.endpoint.clone(), continuation)
                                .await
                        }
                        Err(error) => Err(error),
                    }
                }
            };
            this.update_in(cx, |this, window, cx| {
                if this.queue_generation != generation {
                    return;
                }
                match result {
                    Ok(page) => this.apply_radio_page(request, page, window, cx),
                    Err(error) => {
                        this.radio_state = RadioQueueState::Failed(request, error.to_string());
                    }
                }
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    fn apply_radio_page(
        &mut self,
        request: RadioRequest,
        page: RadioPage,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let (session, songs, stop_without_progress) = match request {
            RadioRequest::Initial {
                seed_video_id,
                replace_future,
            } => {
                let songs = page.recommendations_after_current(&seed_video_id);
                let mut recommendations = Vec::new();
                extend_radio_recommendations(&mut recommendations, &seed_video_id, &songs);
                let mut seen_continuations = HashSet::new();
                let continuation =
                    accept_radio_continuation(&mut seen_continuations, None, page.continuation);
                if replace_future && !songs.is_empty() {
                    self.queue.truncate_after_current();
                }
                (
                    RadioSession {
                        seed_video_id,
                        title: page.title,
                        recommendations,
                        endpoint: page.endpoint,
                        continuation,
                        seen_continuations,
                    },
                    songs,
                    false,
                )
            }
            RadioRequest::Continuation(session) => {
                let mut session = *session;
                let songs = page.songs;
                session.continuation = accept_radio_continuation(
                    &mut session.seen_continuations,
                    session.continuation.take(),
                    page.continuation,
                );
                if page.title.is_some() {
                    session.title = page.title;
                }
                session.endpoint = page.endpoint;
                extend_radio_recommendations(
                    &mut session.recommendations,
                    &session.seed_video_id,
                    &songs,
                );
                (session, songs, true)
            }
        };
        let added = self.queue.append_unique(
            songs
                .into_iter()
                .filter(|song| song.video_id != session.seed_video_id)
                .map(|song| QueueItem { song }),
        );
        if added > 0 && self.shuffle_enabled {
            self.queue.shuffle_upcoming();
        }
        let exhausted = session.continuation.is_none() || (stop_without_progress && added == 0);
        self.radio_state = if exhausted {
            RadioQueueState::Exhausted(session)
        } else {
            RadioQueueState::Active(session)
        };
        if added > 0 {
            self.save_session(cx);
            self.refresh_visible_thumbnails(cx);
            if self.audio_player.snapshot().state == PlaybackState::Ended {
                self.advance_after_end(window, cx);
            }
        }
    }

    fn play_song_collection(
        &mut self,
        songs: Vec<Song>,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.reject_guest_playback_control(cx) {
            return;
        }
        self.reset_radio_queue();
        self.shuffle_enabled = false;
        let items = songs.into_iter().map(|song| QueueItem { song }).collect();
        let song = self
            .queue
            .replace_and_select(items, index)
            .map(|item| item.song.clone());
        if let Some(song) = song {
            self.start_playback(song, window, cx);
        }
    }

    fn play_shuffled_collection(
        &mut self,
        mut songs: Vec<Song>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.reject_guest_playback_control(cx) || songs.is_empty() {
            return;
        }
        fastrand::shuffle(&mut songs);
        self.play_song_collection(songs, 0, window, cx);
        self.shuffle_enabled = true;
        self.save_session(cx);
        cx.notify();
    }

    fn play_queue_item(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        if self.reject_guest_playback_control(cx) {
            return;
        }
        let song = self.queue.select(index).map(|item| item.song.clone());
        if let Some(song) = song {
            self.start_playback(song, window, cx);
        }
    }

    fn play_song_next(&mut self, song: Song, window: &mut Window, cx: &mut Context<Self>) {
        if self.reject_guest_playback_control(cx) {
            return;
        }
        if self.queue.current().is_none() {
            self.play_song_collection(vec![song], 0, window, cx);
            return;
        }

        self.queue.insert_after_current(QueueItem { song });
        self.save_session(cx);
        self.refresh_visible_thumbnails(cx);
        cx.notify();
    }

    fn add_song_to_queue(&mut self, song: Song, window: &mut Window, cx: &mut Context<Self>) {
        if self.reject_guest_playback_control(cx) {
            return;
        }
        if self.queue.current().is_none() {
            self.play_song_collection(vec![song], 0, window, cx);
            return;
        }

        self.queue.push(QueueItem { song });
        if self.shuffle_enabled {
            self.queue.shuffle_upcoming();
        }
        self.save_session(cx);
        self.refresh_visible_thumbnails(cx);
        cx.notify();
    }

    fn move_queue_item(&mut self, from: usize, to: usize, cx: &mut Context<Self>) {
        if self.reject_guest_playback_control(cx) || !self.queue.move_item(from, to) {
            return;
        }
        self.save_session(cx);
        cx.notify();
    }

    fn remove_queue_item(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        if self.reject_guest_playback_control(cx) {
            return;
        }
        let removed_current = self.queue.current_index() == Some(index);
        if self.queue.remove(index).is_none() {
            return;
        }

        if !removed_current {
            self.save_session(cx);
            self.refresh_visible_thumbnails(cx);
            cx.notify();
            return;
        }

        self.reset_radio_queue();
        if let Some(song) = self.queue.current().map(|item| item.song.clone()) {
            self.start_playback(song, window, cx);
            return;
        }

        let snapshot = self.audio_player.snapshot();
        self.save_current_episode_progress(snapshot.position, cx);
        self.playback_task = None;
        self.episode_resume_generation = self.episode_resume_generation.wrapping_add(1);
        self.resolving_playback = false;
        self.playback_source_attempt = PlaybackSourceAttempt::None;
        self.pending_resume_position = None;
        self.play_after_resolution = None;
        self.persisted_playback_source = None;
        self.active_playback_source = None;
        self.current_song = None;
        self.now_playing_visible = false;
        self.played_this_track = Duration::ZERO;
        self.history_recorded_for_current = false;
        self.lastfm_playback_tracker.reset();
        self.lyrics_task = None;
        self.lyrics_state = LyricsViewState::Idle;
        self.lyrics_active_line = None;
        self.last_playback_state = PlaybackState::Idle;
        self.playback_error = self
            .audio_player
            .stop()
            .err()
            .map(|error| error.to_string());
        self.save_session(cx);
        self.refresh_visible_thumbnails(cx);
        cx.notify();
    }

    fn play_next(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.reject_guest_playback_control(cx) {
            return;
        }
        let song = if let Some(item) = self.queue.next_item() {
            Some(item.song.clone())
        } else if self.repeat_mode == RepeatMode::All && !self.queue.is_empty() {
            if self.shuffle_enabled {
                self.queue.shuffle_around_current();
            }
            self.queue.select(0).map(|item| item.song.clone())
        } else {
            None
        };
        if let Some(song) = song {
            self.start_playback(song, window, cx);
        }
    }

    fn play_previous(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.reject_guest_playback_control(cx) {
            return;
        }
        let snapshot = self.audio_player.snapshot();
        if snapshot.position >= Duration::from_secs(5) {
            if let Err(error) = self.audio_player.seek(Duration::ZERO) {
                self.playback_error = Some(error.to_string());
            }
            cx.notify();
            return;
        }

        let song = if let Some(item) = self.queue.previous_item() {
            Some(item.song.clone())
        } else if self.repeat_mode == RepeatMode::All && !self.queue.is_empty() {
            let last = self.queue.len().saturating_sub(1);
            self.queue.select(last).map(|item| item.song.clone())
        } else {
            None
        };
        if let Some(song) = song {
            self.start_playback(song, window, cx);
        }
    }

    fn toggle_shuffle(&mut self, cx: &mut Context<Self>) {
        if self.reject_guest_playback_control(cx) || self.queue.is_empty() {
            return;
        }
        self.shuffle_enabled = !self.shuffle_enabled;
        if self.shuffle_enabled {
            self.queue.shuffle_upcoming();
        }
        self.save_session(cx);
        cx.notify();
    }

    fn cycle_repeat_mode(&mut self, cx: &mut Context<Self>) {
        if self.reject_guest_playback_control(cx) || self.queue.is_empty() {
            return;
        }
        self.repeat_mode = self.repeat_mode.next();
        self.save_session(cx);
        cx.notify();
    }

    fn set_sleep_timer(&mut self, timer: SleepTimer, cx: &mut Context<Self>) {
        if self.reject_guest_playback_control(cx) || self.current_song.is_none() {
            return;
        }
        self.sleep_timer = Some(timer);
        cx.notify();
    }

    fn cancel_sleep_timer(&mut self, cx: &mut Context<Self>) {
        self.sleep_timer = None;
        cx.notify();
    }

    fn clear_upcoming_queue(&mut self, cx: &mut Context<Self>) {
        if self.reject_guest_playback_control(cx) || self.queue.remaining_after_current() == 0 {
            return;
        }
        self.queue.truncate_after_current();
        self.reset_radio_queue();
        self.save_session(cx);
        self.refresh_visible_thumbnails(cx);
        cx.notify();
    }

    fn advance_after_end(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if self.listen_together_is_guest() {
            return false;
        }
        let action = queue_end_action(
            self.repeat_mode,
            self.queue.current().is_some(),
            self.queue.has_next(),
        );
        let (song, resume_episode) = match action {
            QueueEndAction::Stop => (None, true),
            QueueEndAction::Advance => (self.queue.next_item().map(|item| item.song.clone()), true),
            QueueEndAction::Wrap => {
                if self.shuffle_enabled {
                    self.queue.shuffle_around_current();
                }
                (self.queue.select(0).map(|item| item.song.clone()), false)
            }
            QueueEndAction::Replay => (self.queue.current().map(|item| item.song.clone()), false),
        };
        let Some(song) = song else {
            return false;
        };
        self.start_playback_with_episode_resume(song, resume_episode, window, cx);
        true
    }

    fn play_from_desktop(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.reject_guest_playback_control(cx) {
            return;
        }
        if self.resolving_playback {
            return;
        }
        let result = match self.audio_player.snapshot().state {
            PlaybackState::Paused => self.audio_player.play(),
            PlaybackState::Playing | PlaybackState::Loading => return,
            PlaybackState::Idle | PlaybackState::Ended | PlaybackState::Failed => {
                if let Some(song) = self.current_song.clone() {
                    self.start_playback(song, window, cx);
                }
                return;
            }
        };
        self.playback_error = result.err().map(|error| error.to_string());
        cx.notify();
    }

    fn pause_from_desktop(&mut self, cx: &mut Context<Self>) {
        if self.reject_guest_playback_control(cx) {
            return;
        }
        if self.audio_player.snapshot().state == PlaybackState::Playing {
            self.playback_error = self
                .audio_player
                .pause()
                .err()
                .map(|error| error.to_string());
            cx.notify();
        }
    }

    fn stop_from_desktop(&mut self, cx: &mut Context<Self>) {
        if self.reject_guest_playback_control(cx) {
            return;
        }
        self.playback_task = None;
        self.resolving_playback = false;
        self.playback_source_attempt = PlaybackSourceAttempt::None;
        self.pending_resume_position = None;
        self.playback_error = self
            .audio_player
            .stop()
            .err()
            .map(|error| error.to_string());
        self.last_playback_state = PlaybackState::Idle;
        self.save_session(cx);
        cx.notify();
    }

    fn seek_from_desktop(&mut self, target: Duration, cx: &mut Context<Self>) {
        if self.reject_guest_playback_control(cx) {
            return;
        }
        let target = self
            .playback_duration()
            .map_or(target, |duration| target.min(duration));
        let snapshot = self.audio_player.snapshot();
        if self.resolving_playback || snapshot.state == PlaybackState::Idle {
            if self.current_song.is_some() {
                self.pending_resume_position = Some(target);
                self.save_session(cx);
                cx.notify();
            }
            return;
        }
        self.playback_error = self
            .audio_player
            .seek(target)
            .err()
            .map(|error| error.to_string());
        self.save_session(cx);
        cx.notify();
    }

    fn handle_desktop_media_commands(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        for command in self.desktop_media.drain_commands() {
            if self.listen_together_is_guest()
                && !matches!(
                    command,
                    DesktopMediaCommand::Raise | DesktopMediaCommand::Quit
                )
            {
                self.listen_together_error =
                    Some("Playback is controlled by the Listen Together room host.".into());
                continue;
            }
            match command {
                DesktopMediaCommand::Play => self.play_from_desktop(window, cx),
                DesktopMediaCommand::Pause => self.pause_from_desktop(cx),
                DesktopMediaCommand::Toggle => self.toggle_playback(window, cx),
                DesktopMediaCommand::Next => self.play_next(window, cx),
                DesktopMediaCommand::Previous => self.play_previous(window, cx),
                DesktopMediaCommand::Stop => self.stop_from_desktop(cx),
                DesktopMediaCommand::SeekRelative { direction, amount } => {
                    let position = self
                        .pending_resume_position
                        .unwrap_or_else(|| self.audio_player.snapshot().position);
                    let target = match direction {
                        DesktopSeekDirection::Forward => position.saturating_add(amount),
                        DesktopSeekDirection::Backward => position.saturating_sub(amount),
                    };
                    self.seek_from_desktop(target, cx);
                }
                DesktopMediaCommand::SetPosition(position) => {
                    self.seek_from_desktop(position, cx);
                }
                DesktopMediaCommand::SetVolume(volume) => {
                    self.audio_player.set_volume(volume);
                    self.save_session(cx);
                    cx.notify();
                }
                DesktopMediaCommand::Raise => window.activate_window(),
                DesktopMediaCommand::Quit => cx.quit(),
            }
        }
    }

    fn poll_lastfm(
        &mut self,
        playback_state: PlaybackState,
        duration: Option<Duration>,
        cx: &mut Context<Self>,
    ) {
        if self.lastfm_playback_task.is_some()
            || self.lastfm_session.is_none()
            || self.lastfm_client.is_none()
        {
            return;
        }
        let Some(song) = self.current_song.as_ref() else {
            return;
        };
        if song.is_episode {
            self.lastfm_playback_tracker.reset();
            return;
        }
        let actions = match self
            .lastfm_playback_tracker
            .observe(LastFmPlaybackObservation {
                song,
                duration,
                is_playing: playback_state == PlaybackState::Playing,
                played: self.played_this_track,
                unix_seconds: unix_time_seconds(),
                now_playing_enabled: self.settings.lastfm_now_playing,
                scrobbling_enabled: self.settings.lastfm_scrobbling,
                policy: self.settings.lastfm_scrobble_policy,
            }) {
            Ok(actions) => actions,
            Err(error) => {
                self.lastfm_error = Some(error.to_string());
                return;
            }
        };
        if actions.is_empty() {
            return;
        }
        let Some(client) = self.lastfm_client.clone() else {
            return;
        };
        self.lastfm_playback_task = Some(cx.spawn(async move |this, cx| {
            let result = async {
                for action in actions {
                    match action {
                        LastFmPlaybackAction::UpdateNowPlaying(track) => {
                            client.update_now_playing(&track).await?;
                        }
                        LastFmPlaybackAction::Scrobble { track, started_at } => {
                            client.scrobble(&track, started_at).await?;
                        }
                    }
                }
                Ok::<_, AppError>(())
            }
            .await;
            this.update(cx, |this, cx| {
                this.lastfm_playback_task = None;
                match result {
                    Ok(()) => this.lastfm_error = None,
                    Err(error) => this.lastfm_error = Some(error.to_string()),
                }
                cx.notify();
            })
            .ok();
        }));
    }

    fn poll_discord_presence(
        &mut self,
        playback_state: PlaybackState,
        position: Duration,
        duration: Option<Duration>,
        tempo_milli: u16,
    ) {
        let state = match playback_state {
            PlaybackState::Loading => DiscordPlaybackState::Loading,
            PlaybackState::Playing => DiscordPlaybackState::Playing,
            PlaybackState::Paused => DiscordPlaybackState::Paused,
            PlaybackState::Idle | PlaybackState::Ended | PlaybackState::Failed => {
                DiscordPlaybackState::Inactive
            }
        };
        let observation = DiscordPlaybackObservation {
            enabled: self.settings.discord_rich_presence,
            song: self.current_song.as_ref(),
            state,
            position,
            duration: duration
                .or_else(|| self.current_song.as_ref().and_then(|song| song.duration)),
            tempo_milli,
            unix_seconds: unix_time_seconds(),
        };
        let Some(action) = self.discord_presence_tracker.observe(observation) else {
            return;
        };
        let Some(service) = self.discord_presence.as_ref() else {
            return;
        };
        match service.apply(action, observation) {
            Ok(()) => self.discord_warning = None,
            Err(error) => self.discord_warning = Some(error.to_string()),
        }
    }

    fn listen_together_is_guest(&self) -> bool {
        self.listen_together_snapshot.room.is_some()
            && self.listen_together_snapshot.role == ListenTogetherRoomRole::Guest
    }

    fn reject_guest_playback_control(&mut self, cx: &mut Context<Self>) -> bool {
        if !self.listen_together_is_guest() {
            return false;
        }
        self.listen_together_error =
            Some("Playback is controlled by the Listen Together room host.".into());
        cx.notify();
        true
    }

    fn create_listen_together_room(&mut self, cx: &mut Context<Self>) {
        let Some(client) = self.listen_together.as_ref() else {
            self.listen_together_error = Some("Listen Together is unavailable.".into());
            cx.notify();
            return;
        };
        if self.listen_together_server_input.read(cx).value().trim()
            != self.settings.listen_together.server_url
        {
            self.listen_together_error =
                Some("Save the changed room server before creating a room.".into());
            cx.notify();
            return;
        }
        let username = self
            .listen_together_username_input
            .read(cx)
            .value()
            .trim()
            .to_owned();
        match client.create_room(username) {
            Ok(()) => {
                self.listen_together_error = None;
                self.listen_together_notice = Some("Creating room…".into());
            }
            Err(error) => self.listen_together_error = Some(error.to_string()),
        }
        cx.notify();
    }

    fn join_listen_together_room(&mut self, cx: &mut Context<Self>) {
        let Some(client) = self.listen_together.as_ref() else {
            self.listen_together_error = Some("Listen Together is unavailable.".into());
            cx.notify();
            return;
        };
        if self.listen_together_server_input.read(cx).value().trim()
            != self.settings.listen_together.server_url
        {
            self.listen_together_error =
                Some("Save the changed room server before joining a room.".into());
            cx.notify();
            return;
        }
        let username = self
            .listen_together_username_input
            .read(cx)
            .value()
            .trim()
            .to_owned();
        let room_code = self
            .listen_together_room_code_input
            .read(cx)
            .value()
            .trim()
            .to_owned();
        match client.join_room(room_code, username) {
            Ok(()) => {
                self.listen_together_error = None;
                self.listen_together_notice = Some("Requesting to join room…".into());
            }
            Err(error) => self.listen_together_error = Some(error.to_string()),
        }
        cx.notify();
    }

    fn connect_listen_together(&mut self, cx: &mut Context<Self>) {
        if self.listen_together_server_input.read(cx).value().trim()
            != self.settings.listen_together.server_url
        {
            self.listen_together_error =
                Some("Save the changed room server before testing it.".into());
            cx.notify();
            return;
        }
        let result = self
            .listen_together
            .as_ref()
            .ok_or_else(|| AppError::ListenTogether("connection worker is unavailable".into()))
            .and_then(ListenTogetherClient::connect);
        match result {
            Ok(()) => {
                self.listen_together_error = None;
                self.listen_together_notice = Some("Connecting to room server…".into());
            }
            Err(error) => self.listen_together_error = Some(error.to_string()),
        }
        cx.notify();
    }

    fn leave_listen_together_room(&mut self, cx: &mut Context<Self>) {
        let result = self
            .listen_together
            .as_ref()
            .ok_or_else(|| AppError::ListenTogether("connection worker is unavailable".into()))
            .and_then(ListenTogetherClient::leave_room);
        match result {
            Ok(()) => {
                self.listen_together_tracker.reset();
                self.listen_together_pending_sync = None;
                self.listen_together_error = None;
                self.listen_together_notice = Some("Left the room.".into());
            }
            Err(error) => self.listen_together_error = Some(error.to_string()),
        }
        cx.notify();
    }

    fn disconnect_listen_together(&mut self, cx: &mut Context<Self>) {
        let result = self
            .listen_together
            .as_ref()
            .ok_or_else(|| AppError::ListenTogether("connection worker is unavailable".into()))
            .and_then(ListenTogetherClient::disconnect);
        match result {
            Ok(()) => {
                self.listen_together_tracker.reset();
                self.listen_together_pending_sync = None;
                self.listen_together_error = None;
                self.listen_together_notice = Some("Disconnected from the room server.".into());
            }
            Err(error) => self.listen_together_error = Some(error.to_string()),
        }
        cx.notify();
    }

    fn canonical_guest_songs(
        current: Option<&ListenTogetherTrack>,
        upcoming: &[ListenTogetherTrack],
    ) -> Vec<Song> {
        let mut seen = HashSet::new();
        current
            .into_iter()
            .chain(upcoming)
            .filter(|track| seen.insert(track.id.clone()))
            .map(ListenTogetherTrack::to_song)
            .collect()
    }

    fn replace_guest_queue(
        &mut self,
        current: Option<&ListenTogetherTrack>,
        upcoming: &[ListenTogetherTrack],
        cx: &mut Context<Self>,
    ) {
        let items = Self::canonical_guest_songs(current, upcoming)
            .into_iter()
            .map(|song| QueueItem { song })
            .collect();
        let current_index = current.map(|_| 0);
        let song = self
            .queue
            .replace(items, current_index)
            .map(|item| item.song.clone());
        if let Some(song) = song {
            self.set_current_song(song, cx);
        } else {
            self.current_song = None;
        }
        self.reset_radio_queue();
        self.save_session(cx);
        self.refresh_visible_thumbnails(cx);
    }

    fn start_guest_track(
        &mut self,
        start: GuestTrackStart,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let GuestTrackStart {
            track,
            upcoming,
            is_playing,
            position_ms,
            effective_at_server_time_ms,
            bypass_buffer,
        } = start;
        let adjusted_position = self.listen_together.as_ref().map_or(position_ms, |client| {
            client.position_at_server_time(position_ms, effective_at_server_time_ms, is_playing)
        });
        self.replace_guest_queue(Some(&track), &upcoming, cx);
        let song = track.to_song();
        self.playback_retry_count = 0;
        self.played_this_track = Duration::ZERO;
        self.history_recorded_for_current = false;
        self.lastfm_playback_tracker.reset();
        self.last_playback_poll = Instant::now();
        self.pending_resume_position = Some(Duration::from_millis(adjusted_position.max(0) as u64));
        self.play_after_resolution = Some(bypass_buffer && is_playing);
        if self
            .persisted_playback_source
            .as_ref()
            .is_some_and(|source| source.video_id != song.video_id)
        {
            self.persisted_playback_source = None;
        }
        if self
            .active_playback_source
            .as_ref()
            .is_some_and(|source| source.video_id != song.video_id)
        {
            self.active_playback_source = None;
        }
        self.listen_together_pending_sync = (!bypass_buffer).then_some(PendingGuestSync {
            track_id: track.id,
            is_playing,
            position_ms,
            effective_at_server_time_ms,
            ready_sent: false,
            buffer_complete: false,
        });
        self.begin_playback(song, window, cx);
        self.save_session(cx);
    }

    fn apply_guest_room_state(
        &mut self,
        state: ListenTogetherRoomState,
        bypass_buffer: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.settings.listen_together.sync_host_volume {
            self.audio_player.set_volume(state.volume.clamp(0.0, 1.0));
        }
        let Some(track) = state.current_track else {
            self.replace_guest_queue(None, &state.queue, cx);
            let _ = self.audio_player.pause();
            self.listen_together_pending_sync = None;
            return;
        };
        if self
            .current_song
            .as_ref()
            .map(|song| song.video_id.as_str())
            == Some(track.id.as_str())
            && !self.resolving_playback
        {
            let position_ms = self
                .listen_together
                .as_ref()
                .map_or(state.position_ms, |client| {
                    client.position_at_server_time(
                        state.position_ms,
                        Some(state.last_update_ms),
                        state.is_playing,
                    )
                });
            self.replace_guest_queue(Some(&track), &state.queue, cx);
            if bypass_buffer {
                let _ = self
                    .audio_player
                    .seek(Duration::from_millis(position_ms.max(0) as u64));
                let result = if state.is_playing {
                    self.audio_player.play()
                } else {
                    self.audio_player.pause()
                };
                if let Err(error) = result {
                    self.listen_together_error = Some(error.to_string());
                }
                self.listen_together_pending_sync = None;
            } else {
                let _ = self.audio_player.pause();
                self.listen_together_pending_sync = Some(PendingGuestSync {
                    track_id: track.id,
                    is_playing: state.is_playing,
                    position_ms: state.position_ms,
                    effective_at_server_time_ms: Some(state.last_update_ms),
                    ready_sent: false,
                    buffer_complete: false,
                });
            }
        } else {
            self.start_guest_track(
                GuestTrackStart {
                    track,
                    upcoming: state.queue,
                    is_playing: state.is_playing,
                    position_ms: state.position_ms,
                    effective_at_server_time_ms: Some(state.last_update_ms),
                    bypass_buffer,
                },
                window,
                cx,
            );
        }
    }

    fn apply_guest_playback_action(
        &mut self,
        action: ListenTogetherPlaybackActionPayload,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match action.action {
            ListenTogetherPlaybackAction::ChangeTrack => {
                if let Some(track) = action.track {
                    self.start_guest_track(
                        GuestTrackStart {
                            track,
                            upcoming: action.queue.unwrap_or_default(),
                            is_playing: false,
                            position_ms: 0,
                            effective_at_server_time_ms: action
                                .server_time_ms
                                .or(action.captured_at_server_time_ms),
                            bypass_buffer: false,
                        },
                        window,
                        cx,
                    );
                }
            }
            ListenTogetherPlaybackAction::Play
            | ListenTogetherPlaybackAction::Pause
            | ListenTogetherPlaybackAction::Seek => {
                let target_id = action.track_id.as_deref();
                if target_id.is_some()
                    && self
                        .current_song
                        .as_ref()
                        .map(|song| song.video_id.as_str())
                        != target_id
                {
                    if let Some(client) = self.listen_together.as_ref() {
                        let _ = client.request_sync();
                    }
                    return;
                }
                let position_ms = action.position_ms.unwrap_or_default().max(0);
                let is_playing = match action.action {
                    ListenTogetherPlaybackAction::Play => true,
                    ListenTogetherPlaybackAction::Pause => false,
                    ListenTogetherPlaybackAction::Seek => self
                        .listen_together_snapshot
                        .room
                        .as_ref()
                        .is_some_and(|room| room.is_playing),
                    _ => unreachable!(),
                };
                if let Some(pending) = self.listen_together_pending_sync.as_mut() {
                    pending.is_playing = is_playing;
                    pending.position_ms = position_ms;
                    pending.effective_at_server_time_ms =
                        action.server_time_ms.or(action.captured_at_server_time_ms);
                    return;
                }
                let adjusted = self.listen_together.as_ref().map_or(position_ms, |client| {
                    client.position_at_server_time(
                        position_ms,
                        action.server_time_ms.or(action.captured_at_server_time_ms),
                        is_playing,
                    )
                });
                let _ = self
                    .audio_player
                    .seek(Duration::from_millis(adjusted.max(0) as u64));
                let result = match action.action {
                    ListenTogetherPlaybackAction::Play => self.audio_player.play(),
                    ListenTogetherPlaybackAction::Pause => self.audio_player.pause(),
                    ListenTogetherPlaybackAction::Seek => Ok(()),
                    _ => unreachable!(),
                };
                if let Err(error) = result {
                    self.listen_together_error = Some(error.to_string());
                }
            }
            ListenTogetherPlaybackAction::SetVolume => {
                if self.settings.listen_together.sync_host_volume
                    && let Some(volume) = action.volume
                {
                    self.audio_player.set_volume(volume.clamp(0.0, 1.0));
                }
            }
            ListenTogetherPlaybackAction::SyncQueue
            | ListenTogetherPlaybackAction::QueueAdd
            | ListenTogetherPlaybackAction::QueueRemove
            | ListenTogetherPlaybackAction::QueueClear => {
                if let Some(queue) = action.queue {
                    let current = self
                        .current_song
                        .as_ref()
                        .map(ListenTogetherTrack::from_song);
                    self.replace_guest_queue(current.as_ref(), &queue, cx);
                }
            }
            ListenTogetherPlaybackAction::SkipNext | ListenTogetherPlaybackAction::SkipPrevious => {
                if let Some(client) = self.listen_together.as_ref() {
                    let _ = client.request_sync();
                }
            }
        }
    }

    fn poll_guest_buffer_barrier(&mut self) {
        let Some(pending) = self.listen_together_pending_sync.as_mut() else {
            return;
        };
        let snapshot = self.audio_player.snapshot();
        let ready = !self.resolving_playback
            && self
                .current_song
                .as_ref()
                .map(|song| song.video_id.as_str())
                == Some(pending.track_id.as_str())
            && matches!(
                snapshot.state,
                PlaybackState::Paused | PlaybackState::Playing
            );
        if ready && !pending.ready_sent {
            let result = self
                .listen_together
                .as_ref()
                .ok_or_else(|| AppError::ListenTogether("connection worker is unavailable".into()))
                .and_then(|client| client.send_buffer_ready(pending.track_id.clone()));
            match result {
                Ok(()) => pending.ready_sent = true,
                Err(error) => self.listen_together_error = Some(error.to_string()),
            }
        }
        if !pending.ready_sent || !pending.buffer_complete {
            return;
        }
        let pending = self
            .listen_together_pending_sync
            .take()
            .expect("pending sync was checked");
        let adjusted = self
            .listen_together
            .as_ref()
            .map_or(pending.position_ms, |client| {
                client.position_at_server_time(
                    pending.position_ms,
                    pending.effective_at_server_time_ms,
                    pending.is_playing,
                )
            });
        if let Err(error) = self
            .audio_player
            .seek(Duration::from_millis(adjusted.max(0) as u64))
            .and_then(|_| {
                if pending.is_playing {
                    self.audio_player.play()
                } else {
                    self.audio_player.pause()
                }
            })
        {
            self.listen_together_error = Some(error.to_string());
        }
    }

    fn poll_listen_together(
        &mut self,
        playback_state: PlaybackState,
        position: Duration,
        volume: f32,
        tempo_milli: u16,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(client) = self.listen_together.as_ref() else {
            return;
        };
        let events = client.drain_events();
        for event in events {
            match event {
                ListenTogetherEvent::RoomCreated { room_code } => {
                    self.listen_together_tracker.reset();
                    self.listen_together_notice = Some(format!(
                        "Room {room_code} created. Share the code; guests require approval."
                    ));
                    self.listen_together_error = None;
                }
                ListenTogetherEvent::JoinRequest(request) => {
                    if self.settings.listen_together.auto_approve_joins {
                        if let Some(client) = self.listen_together.as_ref()
                            && let Err(error) = client.approve_join(request.user_id)
                        {
                            self.listen_together_error = Some(error.to_string());
                        }
                    } else {
                        self.listen_together_notice =
                            Some(format!("{} wants to join the room.", request.username));
                    }
                }
                ListenTogetherEvent::JoinApproved(room) => {
                    self.listen_together_notice =
                        Some(format!("Joined room {} as a guest.", room.room_code));
                    self.listen_together_error = None;
                    self.apply_guest_room_state(room, false, window, cx);
                }
                ListenTogetherEvent::JoinRejected { reason } => {
                    self.listen_together_error = Some(format!("Join request rejected: {reason}"));
                }
                ListenTogetherEvent::PlaybackSync(action) => {
                    if self.listen_together_snapshot.role == ListenTogetherRoomRole::Guest
                        || self.listen_together.as_ref().is_some_and(|client| {
                            client.snapshot().role == ListenTogetherRoomRole::Guest
                        })
                    {
                        self.apply_guest_playback_action(action, window, cx);
                    }
                }
                ListenTogetherEvent::BufferComplete { track_id } => {
                    if let Some(pending) = self.listen_together_pending_sync.as_mut()
                        && pending.track_id == track_id
                    {
                        pending.buffer_complete = true;
                    }
                }
                ListenTogetherEvent::SyncState(state) => {
                    if self.listen_together.as_ref().is_some_and(|client| {
                        client.snapshot().role == ListenTogetherRoomRole::Guest
                    }) {
                        self.apply_guest_room_state(
                            ListenTogetherRoomState {
                                room_code: self
                                    .listen_together_snapshot
                                    .room
                                    .as_ref()
                                    .map(|room| room.room_code.clone())
                                    .unwrap_or_default(),
                                host_id: self
                                    .listen_together_snapshot
                                    .room
                                    .as_ref()
                                    .map(|room| room.host_id.clone())
                                    .unwrap_or_default(),
                                users: self
                                    .listen_together_snapshot
                                    .room
                                    .as_ref()
                                    .map(|room| room.users.clone())
                                    .unwrap_or_default(),
                                current_track: state.current_track,
                                is_playing: state.is_playing,
                                position_ms: state.position_ms,
                                last_update_ms: state.last_update_ms,
                                volume: state.volume,
                                queue: state.queue,
                                revision: state.revision,
                            },
                            true,
                            window,
                            cx,
                        );
                    }
                }
                ListenTogetherEvent::Reconnected(room) => {
                    let role = self
                        .listen_together
                        .as_ref()
                        .map(ListenTogetherClient::snapshot)
                        .map(|snapshot| snapshot.role)
                        .unwrap_or_default();
                    if role == ListenTogetherRoomRole::Guest {
                        self.apply_guest_room_state(room, true, window, cx);
                    } else {
                        self.listen_together_tracker.reset();
                    }
                    self.listen_together_notice = Some("Room session reconnected.".into());
                }
                ListenTogetherEvent::HostChanged {
                    new_host_id,
                    new_host_name,
                } => {
                    self.listen_together_tracker.reset();
                    self.listen_together_notice =
                        Some(format!("{new_host_name} is now the room host."));
                    if self.listen_together.as_ref().is_some_and(|client| {
                        client.snapshot().user_id.as_deref() != Some(new_host_id.as_str())
                    }) && let Some(client) = self.listen_together.as_ref()
                    {
                        let _ = client.request_sync();
                    }
                }
                ListenTogetherEvent::Kicked { reason } => {
                    self.listen_together_pending_sync = None;
                    self.listen_together_error = Some(format!("Removed from room: {reason}"));
                }
                ListenTogetherEvent::SuggestionReceived(suggestion) => {
                    if self.settings.listen_together.auto_approve_suggestions {
                        if let Some(client) = self.listen_together.as_ref()
                            && let Err(error) = client.approve_suggestion(suggestion.suggestion_id)
                        {
                            self.listen_together_error = Some(error.to_string());
                        }
                    } else {
                        self.listen_together_notice = Some(format!(
                            "{} suggested {}.",
                            suggestion.from_username, suggestion.track.title
                        ));
                    }
                }
                ListenTogetherEvent::SuggestionApproved { track, .. } => {
                    self.listen_together_notice =
                        Some(format!("Track suggestion approved: {}.", track.title));
                }
                ListenTogetherEvent::SuggestionRejected { reason, .. } => {
                    self.listen_together_error = Some(reason.map_or_else(
                        || "Track suggestion was rejected.".into(),
                        |reason| format!("Track suggestion rejected: {reason}"),
                    ));
                }
                ListenTogetherEvent::ServerError { code, message } => {
                    self.listen_together_error = Some(format!("{code}: {message}"));
                }
                ListenTogetherEvent::ConnectionError { message } => {
                    self.listen_together_error = Some(message);
                }
                ListenTogetherEvent::Disconnected => {
                    self.listen_together_tracker.reset();
                    self.listen_together_pending_sync = None;
                }
                ListenTogetherEvent::UserJoined(user) => {
                    self.listen_together_notice =
                        Some(format!("{} joined the room.", user.username));
                }
                ListenTogetherEvent::UserLeft { username, .. } => {
                    self.listen_together_notice = Some(format!("{username} left the room."));
                }
                ListenTogetherEvent::BufferWait { .. }
                | ListenTogetherEvent::UserReconnected { .. }
                | ListenTogetherEvent::UserDisconnected { .. } => {}
            }
        }

        self.listen_together_snapshot = self
            .listen_together
            .as_ref()
            .map(ListenTogetherClient::snapshot)
            .unwrap_or_default();
        if self.listen_together_snapshot.role == ListenTogetherRoomRole::Guest {
            self.poll_guest_buffer_barrier();
            return;
        }

        let upcoming_queue = self
            .queue
            .current_index()
            .map(|index| {
                self.queue.items()[index.saturating_add(1)..]
                    .iter()
                    .map(|item| ListenTogetherTrack::from_song(&item.song))
                    .collect()
            })
            .unwrap_or_default();
        let state = match playback_state {
            PlaybackState::Playing => ListenTogetherLocalPlaybackState::Playing,
            PlaybackState::Paused => ListenTogetherLocalPlaybackState::Paused,
            PlaybackState::Loading => ListenTogetherLocalPlaybackState::Loading,
            PlaybackState::Idle | PlaybackState::Ended | PlaybackState::Failed => {
                ListenTogetherLocalPlaybackState::Inactive
            }
        };
        let actions = self.listen_together_tracker.observe(
            ListenTogetherPlaybackObservation {
                is_host: self.listen_together_snapshot.role == ListenTogetherRoomRole::Host,
                current_track: self
                    .current_song
                    .as_ref()
                    .map(ListenTogetherTrack::from_song),
                upcoming_queue,
                state,
                position_ms: position.as_millis().min(i64::MAX as u128) as i64,
                volume,
                sync_volume: self.settings.listen_together.sync_host_volume,
                tempo_milli,
            },
            Instant::now(),
        );
        if let Some(client) = self.listen_together.as_ref() {
            for action in actions {
                if let Err(error) = client.send_playback_action(action) {
                    self.listen_together_error = Some(error.to_string());
                    break;
                }
            }
        }
    }

    fn save_current_episode_progress(&mut self, position: Duration, cx: &mut Context<Self>) {
        if position < EPISODE_POSITION_THRESHOLD {
            return;
        }
        let Some(song) = self.current_song.clone().filter(|song| song.is_episode) else {
            return;
        };
        self.last_episode_progress_save = Instant::now();
        if let StoredViewState::Loaded(episodes) = &mut self.episodes_for_later
            && let Some(episode) = episodes
                .iter_mut()
                .find(|episode| episode.song.video_id == song.video_id)
        {
            episode.playback_position = Some(position);
        }
        let store = self.store.clone();
        cx.spawn(async move |this, cx| {
            let result = store.save_episode_playback_position(song, position).await;
            this.update(cx, |this, cx| {
                if let Err(error) = result {
                    this.podcast_error = Some(format!(
                        "Episode playback progress could not be saved: {error}"
                    ));
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    fn poll_playback(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.handle_desktop_media_commands(window, cx);
        self.poll_download_progress();
        let now = Instant::now();
        let poll_elapsed = now.saturating_duration_since(self.last_playback_poll);
        self.last_playback_poll = now;
        if self.last_playback_state == PlaybackState::Playing {
            self.played_this_track += poll_elapsed.min(Duration::from_secs(1));
        }
        if self
            .sleep_timer
            .is_some_and(|timer| timer.deadline_reached(now))
        {
            self.sleep_timer = None;
            self.play_after_resolution = Some(false);
            self.playback_error = self
                .audio_player
                .pause()
                .err()
                .map(|error| error.to_string());
        }
        let snapshot = self.audio_player.snapshot();
        let observed_state = if self.resolving_playback {
            PlaybackState::Loading
        } else if self.playback_error.is_some() || snapshot.error.is_some() {
            PlaybackState::Failed
        } else {
            snapshot.state
        };

        if !self.seeking {
            let position = self.pending_resume_position.unwrap_or(snapshot.position);
            let progress = self
                .playback_duration()
                .filter(|duration| !duration.is_zero())
                .map_or(0.0, |duration| {
                    (position.as_secs_f32() / duration.as_secs_f32()).clamp(0.0, 1.0)
                });
            let current = self.progress_slider.read(cx).value().end();
            if (current - progress).abs() >= 0.0005 {
                self.progress_slider.update(cx, |slider, cx| {
                    slider.set_value(progress, window, cx);
                });
            }
        }
        let displayed_volume = self.volume_slider.read(cx).value().end();
        if (displayed_volume - snapshot.volume).abs() >= 0.005 {
            self.volume_slider.update(cx, |slider, cx| {
                slider.set_value(snapshot.volume.clamp(0.0, 1.0), window, cx);
            });
        }

        let just_ended = observed_state == PlaybackState::Ended
            && self.last_playback_state != PlaybackState::Ended;
        let just_failed = observed_state == PlaybackState::Failed
            && self.last_playback_state != PlaybackState::Failed;
        let just_paused = observed_state == PlaybackState::Paused
            && self.last_playback_state == PlaybackState::Playing;
        let episode_save_due = observed_state == PlaybackState::Playing
            && now.saturating_duration_since(self.last_episode_progress_save)
                >= EPISODE_POSITION_SAVE_INTERVAL;
        if just_paused || just_ended || episode_save_due {
            self.save_current_episode_progress(snapshot.position, cx);
        }
        self.maybe_refill_radio(window, cx);
        self.last_playback_state = observed_state;
        if matches!(
            observed_state,
            PlaybackState::Playing | PlaybackState::Paused
        ) {
            self.play_after_resolution = None;
        }
        if observed_state == PlaybackState::Playing {
            self.pending_resume_position = None;
        }
        if !self.history_recorded_for_current && self.played_this_track >= HISTORY_THRESHOLD {
            self.record_current_history(cx);
        }
        self.poll_lastfm(observed_state, snapshot.duration, cx);
        self.poll_discord_presence(
            observed_state,
            snapshot.position,
            snapshot.duration,
            snapshot.playback_parameters.tempo_milli,
        );
        self.poll_listen_together(
            observed_state,
            snapshot.position,
            snapshot.volume,
            snapshot.playback_parameters.tempo_milli,
            window,
            cx,
        );
        if now.saturating_duration_since(self.last_session_save) >= SESSION_SAVE_INTERVAL {
            self.save_session(cx);
        }
        if just_failed {
            if !snapshot.position.is_zero() {
                self.pending_resume_position = Some(snapshot.position);
            }
            match self.playback_source_attempt {
                PlaybackSourceAttempt::CacheOnly => {
                    self.playback_source_attempt = PlaybackSourceAttempt::None;
                    if let Some(song) = self.current_song.clone() {
                        if self
                            .download_for(&song.video_id)
                            .is_some_and(|download| download.is_complete())
                        {
                            self.repair_failed_offline_download(song, cx);
                            return;
                        }
                        self.resolve_and_play(song, window, cx);
                        return;
                    }
                }
                PlaybackSourceAttempt::Network if self.playback_retry_count < 1 => {
                    self.active_playback_source = None;
                    self.playback_retry_count += 1;
                    if let Some(song) = self.current_song.clone() {
                        self.resolve_and_play(song, window, cx);
                        return;
                    }
                }
                PlaybackSourceAttempt::Network => self.active_playback_source = None,
                PlaybackSourceAttempt::None => {}
            }
        }
        let sleep_after_song =
            just_ended && self.sleep_timer.is_some_and(SleepTimer::stops_after_song);
        if sleep_after_song {
            self.sleep_timer = None;
        }
        if just_ended && !sleep_after_song && self.advance_after_end(window, cx) {
            return;
        }
        let media_song = if self.resolving_playback || snapshot.state != PlaybackState::Idle {
            self.current_song.as_ref()
        } else {
            None
        };
        self.desktop_media.sync(
            media_song,
            observed_state,
            snapshot.position,
            snapshot.volume,
        );
        self.update_lyrics_timeline();
        cx.notify();
    }

    fn start_playback(&mut self, song: Song, window: &mut Window, cx: &mut Context<Self>) {
        self.start_playback_with_episode_resume(song, true, window, cx);
    }

    fn start_playback_with_episode_resume(
        &mut self,
        song: Song,
        resume_episode: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.reject_guest_playback_control(cx) {
            return;
        }
        let previous = self.audio_player.snapshot();
        self.save_current_episode_progress(previous.position, cx);
        self.play_after_resolution = None;
        self.playback_retry_count = 0;
        self.pending_resume_position = None;
        self.played_this_track = Duration::ZERO;
        self.history_recorded_for_current = false;
        self.lastfm_playback_tracker.reset();
        self.last_playback_poll = Instant::now();
        if self
            .persisted_playback_source
            .as_ref()
            .is_some_and(|source| source.video_id != song.video_id)
        {
            self.persisted_playback_source = None;
        }
        if self
            .active_playback_source
            .as_ref()
            .is_some_and(|source| source.video_id != song.video_id)
        {
            self.active_playback_source = None;
        }
        self.episode_resume_generation = self.episode_resume_generation.wrapping_add(1);
        let resume_generation = self.episode_resume_generation;
        if song.is_episode && resume_episode {
            let video_id = song.video_id.clone();
            let store = self.store.clone();
            self.set_current_song(song.clone(), cx);
            self.resolving_playback = true;
            self.last_playback_state = PlaybackState::Loading;
            self.playback_error = None;
            self.playback_task = Some(cx.spawn_in(window, async move |this, cx| {
                let result = store.episode_playback_position(video_id.clone()).await;
                this.update_in(cx, |this, window, cx| {
                    let still_selected = this.episode_resume_generation == resume_generation
                        && this
                            .queue
                            .current()
                            .is_some_and(|item| item.song.video_id == video_id);
                    if !still_selected {
                        return;
                    }
                    this.resolving_playback = false;
                    match result {
                        Ok(position) => this.pending_resume_position = position,
                        Err(error) => {
                            this.pending_resume_position = None;
                            this.podcast_error = Some(format!(
                                "Episode resume position could not be loaded: {error}"
                            ));
                        }
                    }
                    this.begin_playback(song, window, cx);
                })
                .ok();
            }));
        } else {
            self.begin_playback(song, window, cx);
        }
        self.save_session(cx);
        self.maybe_refill_radio(window, cx);
    }

    fn begin_playback(&mut self, song: Song, window: &mut Window, cx: &mut Context<Self>) {
        let should_play = self.play_after_resolution.unwrap_or(true);
        if let Some(source) = self.downloaded_playback_source(&song.video_id) {
            self.load_and_play_source(
                song,
                source,
                PlaybackSourceAttempt::CacheOnly,
                should_play,
                cx,
            );
            return;
        }
        if let Some((source, attempt)) = choose_playback_source(
            &song.video_id,
            self.active_playback_source.as_ref(),
            self.persisted_playback_source.as_ref(),
            self.settings.audio_quality,
            unix_time_ms(),
        ) {
            self.load_and_play_source(song, source, attempt, should_play, cx);
        } else {
            self.resolve_and_play(song, window, cx);
        }
    }

    fn load_and_play_source(
        &mut self,
        song: Song,
        source: PlaybackSource,
        attempt: PlaybackSourceAttempt,
        should_play: bool,
        cx: &mut Context<Self>,
    ) {
        self.set_current_song(song, cx);
        self.resolving_playback = false;
        self.last_playback_state = PlaybackState::Loading;
        self.playback_source_attempt = attempt;
        self.playback_error = None;
        let resume_position = self.pending_resume_position;
        self.playback_error = self
            .audio_player
            .load(source)
            .and_then(|_| {
                if let Some(position) = resume_position {
                    self.audio_player.seek(position)
                } else {
                    Ok(())
                }
            })
            .and_then(|_| {
                if should_play {
                    self.audio_player.play()
                } else {
                    Ok(())
                }
            })
            .err()
            .map(|error| error.to_string());
        self.refresh_visible_thumbnails(cx);
        cx.notify();
    }

    fn resolve_and_play(&mut self, song: Song, window: &mut Window, cx: &mut Context<Self>) {
        self.set_current_song(song.clone(), cx);
        self.resolving_playback = true;
        self.last_playback_state = PlaybackState::Loading;
        self.playback_source_attempt = PlaybackSourceAttempt::Network;
        self.playback_error = None;
        self.refresh_visible_thumbnails(cx);
        cx.notify();

        let client = self.search_client.clone();
        self.playback_task = Some(cx.spawn_in(window, async move |this, cx| {
            let result = client.resolve_playback_source(&song.video_id).await;
            this.update(cx, |this, cx| {
                this.resolving_playback = false;
                match result {
                    Ok(resolved) => {
                        let resolved_at_ms = unix_time_ms();
                        let expires_at_ms =
                            playback_source_expiration(resolved_at_ms, resolved.expires_in);
                        this.persisted_playback_source = persisted_playback_source(
                            &song.video_id,
                            &resolved,
                            this.settings.audio_quality,
                            resolved_at_ms,
                            expires_at_ms,
                        );
                        this.active_playback_source = Some(ActivePlaybackSource {
                            video_id: song.video_id.clone(),
                            source: resolved.source.clone(),
                            expires_at_ms,
                            playback_tracking: resolved.playback_tracking().cloned(),
                        });
                        let should_play = this.play_after_resolution.unwrap_or(true);
                        this.load_and_play_source(
                            song,
                            resolved.source,
                            PlaybackSourceAttempt::Network,
                            should_play,
                            cx,
                        );
                        this.save_session(cx);
                    }
                    Err(error) => {
                        this.playback_error = Some(error.to_string());
                    }
                }
                cx.notify();
            })
            .ok();
        }));
    }

    fn toggle_playback(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.reject_guest_playback_control(cx) {
            return;
        }
        let snapshot = self.audio_player.snapshot();
        let result = match snapshot.state {
            PlaybackState::Playing => self.audio_player.pause(),
            PlaybackState::Ended | PlaybackState::Failed => {
                if let Some(song) = self.current_song.clone() {
                    self.start_playback(song, window, cx);
                }
                return;
            }
            PlaybackState::Paused => self.audio_player.play(),
            PlaybackState::Idle => {
                if let Some(song) = self.current_song.clone() {
                    self.playback_retry_count = 0;
                    self.played_this_track = Duration::ZERO;
                    self.history_recorded_for_current = false;
                    self.lastfm_playback_tracker.reset();
                    self.last_playback_poll = Instant::now();
                    self.begin_playback(song, window, cx);
                }
                return;
            }
            PlaybackState::Loading => return,
        };
        self.playback_error = result.err().map(|error| error.to_string());
        cx.notify();
    }

    fn nav_button(&self, route: Route, cx: &mut Context<Self>) -> Button {
        let (id, icon) = match route {
            Route::Home => ("nav-home", IconName::LayoutDashboard),
            Route::Explore => ("nav-explore", IconName::BookOpen),
            Route::Search => ("nav-search", IconName::Search),
            Route::Recognition => ("nav-recognition", IconName::Asterisk),
            Route::History => ("nav-history", IconName::Undo2),
            Route::Stats => ("nav-stats", IconName::ChartPie),
            Route::Library => ("nav-library", IconName::BookOpen),
            Route::Settings => ("nav-settings", IconName::Settings),
        };

        Button::new(id)
            .ghost()
            .w_full()
            .justify_start()
            .icon(icon)
            .label(route.title())
            .selected(self.model.route() == route)
            .on_click(cx.listener(move |this, _, _, cx| this.navigate(route, cx)))
    }

    fn render_sidebar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let (account_title, account_detail) =
            sidebar_account_summary(&self.account_state, self.account_operation);
        let account_icon = match self.account_state {
            AccountViewState::Checking => IconName::LoaderCircle,
            AccountViewState::Expired(_) | AccountViewState::Failed(_) => IconName::TriangleAlert,
            AccountViewState::SignedOut | AccountViewState::SignedIn(_) => IconName::User,
        };
        v_flex()
            .h_full()
            .w(px(220.))
            .flex_shrink_0()
            .border_r_1()
            .border_color(cx.theme().sidebar_border)
            .bg(cx.theme().sidebar)
            .text_color(cx.theme().sidebar_foreground)
            .p_4()
            .child(
                h_flex()
                    .gap_3()
                    .items_center()
                    .h(px(56.))
                    .mb_5()
                    .child(
                        div()
                            .size_9()
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded(cx.theme().radius)
                            .bg(cx.theme().sidebar_primary)
                            .text_color(cx.theme().sidebar_primary_foreground)
                            .child(Icon::new(IconName::Play)),
                    )
                    .child(
                        v_flex()
                            .child(div().font_semibold().child("Metrolist"))
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .child("Music for desktop"),
                            ),
                    ),
            )
            .child(
                v_flex()
                    .gap_1()
                    .children(Route::ALL.map(|route| self.nav_button(route, cx))),
            )
            .child(div().flex_1())
            .child(
                div()
                    .id("sidebar-account-settings")
                    .cursor_pointer()
                    .border_1()
                    .border_color(cx.theme().sidebar_border)
                    .rounded(cx.theme().radius)
                    .p_3()
                    .hover(|style| style.bg(cx.theme().sidebar_accent.opacity(0.8)))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.navigate(Route::Settings, cx);
                    }))
                    .child(
                        h_flex()
                            .items_center()
                            .gap_2()
                            .child(Icon::new(account_icon).size_4())
                            .child(
                                v_flex()
                                    .min_w_0()
                                    .child(
                                        div()
                                            .overflow_hidden()
                                            .text_sm()
                                            .font_medium()
                                            .child(account_title),
                                    )
                                    .child(
                                        div()
                                            .mt_1()
                                            .overflow_hidden()
                                            .text_xs()
                                            .text_color(cx.theme().muted_foreground)
                                            .child(account_detail),
                                    ),
                            ),
                    ),
            )
    }

    fn page_heading(
        &self,
        title: &'static str,
        description: &'static str,
        cx: &App,
    ) -> impl IntoElement {
        v_flex()
            .gap_1()
            .child(div().text_2xl().font_semibold().child(title))
            .child(
                div()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child(description),
            )
    }

    fn feature_card(
        &self,
        icon: IconName,
        title: &'static str,
        description: &'static str,
        cx: &App,
    ) -> impl IntoElement {
        v_flex()
            .flex_1()
            .min_w(px(180.))
            .gap_3()
            .rounded(cx.theme().radius_lg)
            .border_1()
            .border_color(cx.theme().border)
            .bg(cx.theme().secondary)
            .p_5()
            .child(
                div()
                    .size_9()
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(cx.theme().radius)
                    .bg(cx.theme().primary)
                    .text_color(cx.theme().primary_foreground)
                    .child(Icon::new(icon)),
            )
            .child(div().font_semibold().child(title))
            .child(
                div()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child(description),
            )
    }

    fn recent_unique_songs(&self, limit: usize) -> Vec<Song> {
        let HistoryViewState::Loaded(history) = &self.history_state else {
            return Vec::new();
        };
        let mut seen = HashSet::new();
        history
            .iter()
            .filter(|entry| seen.insert(entry.song.video_id.clone()))
            .take(limit)
            .map(|entry| entry.song.clone())
            .collect()
    }

    fn home_quick_picks(&self) -> Vec<Song> {
        let mut picks = Vec::new();
        let mut seen = HashSet::new();
        let mut add_song = |song: &Song| {
            if !song.is_episode && seen.insert(song.video_id.clone()) && picks.len() < 20 {
                picks.push(song.clone());
            }
        };

        if let StoredViewState::Loaded(discoveries) = &self.daily_discover_state {
            for item in discoveries {
                add_song(&item.recommendation);
            }
        }
        if let StoredViewState::Loaded(songs) = &self.forgotten_favorites_state {
            for song in songs {
                add_song(song);
            }
        }
        if let StoredViewState::Loaded(songs) = &self.keep_listening_state {
            for song in songs {
                add_song(song);
            }
        }
        for song in self.recent_unique_songs(20) {
            add_song(&song);
        }
        picks
    }

    fn render_home_section(
        &self,
        section_index: usize,
        section: &HomeSection,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        self.render_home_section_options(section_index, section, false, cx)
    }

    fn render_home_section_with_play_all(
        &self,
        section_index: usize,
        section: &HomeSection,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        self.render_home_section_options(section_index, section, true, cx)
    }

    fn render_home_section_options(
        &self,
        section_index: usize,
        section: &HomeSection,
        play_all: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let songs = Arc::new(
            section
                .items
                .iter()
                .filter_map(|item| match item {
                    HomeItem::Song(song) => Some(song.clone()),
                    HomeItem::Browse(_) => None,
                })
                .collect::<Vec<_>>(),
        );
        let play_all_songs = songs.clone();
        let more = section.more.clone();
        let header = h_flex()
            .w_full()
            .items_end()
            .justify_between()
            .gap_3()
            .child(
                v_flex()
                    .gap_1()
                    .child(div().text_lg().font_semibold().child(section.title.clone()))
                    .when_some(section.label.clone(), |column, label| {
                        column.child(
                            div()
                                .text_sm()
                                .text_color(cx.theme().muted_foreground)
                                .child(label),
                        )
                    }),
            )
            .when_some(more, |row, item| {
                row.child(
                    Button::new(format!("home-section-more-{section_index}"))
                        .ghost()
                        .label("More")
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.open_online_browse(item.clone(), cx);
                        })),
                )
            })
            .when(play_all && !songs.is_empty(), |row| {
                row.child(
                    Button::new(format!("home-section-play-all-{section_index}"))
                        .primary()
                        .icon(IconName::Play)
                        .label("Play all")
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.play_song_collection(
                                play_all_songs.as_ref().clone(),
                                0,
                                window,
                                cx,
                            );
                        })),
                )
            });

        v_flex()
            .gap_3()
            .child(header)
            .child(
                h_flex()
                    .flex_wrap()
                    .gap_3()
                    .children(section.items.iter().enumerate().map(
                        |(item_index, item)| match item {
                            HomeItem::Song(song) => {
                                let queue = songs.clone();
                                let queue_index = queue
                                    .iter()
                                    .position(|candidate| candidate.video_id == song.video_id)
                                    .unwrap_or_default();
                                v_flex()
                                    .w(px(300.))
                                    .min_w_0()
                                    .gap_3()
                                    .rounded(cx.theme().radius)
                                    .border_1()
                                    .border_color(cx.theme().border)
                                    .bg(cx.theme().secondary)
                                    .p_3()
                                    .child(
                                        h_flex()
                                            .w_full()
                                            .min_w_0()
                                            .gap_3()
                                            .items_center()
                                            .child(self.render_thumbnail(
                                                song.thumbnail_url.as_deref(),
                                                px(48.),
                                                IconName::Play,
                                                cx,
                                            ))
                                            .child(
                                                v_flex()
                                                    .flex_1()
                                                    .min_w_0()
                                                    .child(
                                                        div()
                                                            .font_medium()
                                                            .overflow_hidden()
                                                            .child(song.title.clone()),
                                                    )
                                                    .child(
                                                        div()
                                                            .text_sm()
                                                            .text_color(
                                                                cx.theme().muted_foreground,
                                                            )
                                                            .overflow_hidden()
                                                            .child(song.artist_line()),
                                                    ),
                                            ),
                                    )
                                    .child(
                                        h_flex()
                                            .w_full()
                                            .flex_wrap()
                                            .gap_1()
                                            .justify_end()
                                            .child(self.download_button(
                                                format!(
                                                    "download-home-{section_index}-{item_index}"
                                                ),
                                                song,
                                                cx,
                                            ))
                                            .child(self.queue_insert_buttons(
                                                format!(
                                                    "home-section-{section_index}-song-{item_index}"
                                                ),
                                                song,
                                                cx,
                                            ))
                                            .child(
                                                Button::new(format!(
                                                    "home-section-{section_index}-song-{item_index}-play"
                                                ))
                                                .ghost()
                                                .icon(IconName::Play)
                                                .label("Play")
                                                .tooltip("Play from this shelf")
                                                .on_click(cx.listener(
                                                    move |this, _, window, cx| {
                                                        this.play_song_collection(
                                                            queue.as_ref().clone(),
                                                            queue_index,
                                                            window,
                                                            cx,
                                                        );
                                                    },
                                                )),
                                            ),
                                    )
                                    .into_any_element()
                            }
                            HomeItem::Browse(item) => {
                                let selected = item.clone();
                                let icon = if item.kind == BrowseKind::Artist {
                                    IconName::User
                                } else {
                                    IconName::BookOpen
                                };
                                v_flex()
                                    .w(px(300.))
                                    .min_w_0()
                                    .gap_3()
                                    .rounded(cx.theme().radius)
                                    .border_1()
                                    .border_color(cx.theme().border)
                                    .bg(cx.theme().secondary)
                                    .p_3()
                                    .child(
                                        h_flex()
                                            .w_full()
                                            .min_w_0()
                                            .gap_3()
                                            .items_center()
                                            .child(self.render_thumbnail(
                                                item.thumbnail_url.as_deref(),
                                                px(48.),
                                                icon,
                                                cx,
                                            ))
                                            .child(
                                                v_flex()
                                                    .flex_1()
                                                    .min_w_0()
                                                    .child(
                                                        div()
                                                            .font_medium()
                                                            .overflow_hidden()
                                                            .child(item.title.clone()),
                                                    )
                                                    .child(
                                                        div()
                                                            .text_sm()
                                                            .text_color(
                                                                cx.theme().muted_foreground,
                                                            )
                                                            .overflow_hidden()
                                                            .child(item.subtitle.clone()),
                                                    ),
                                            ),
                                    )
                                    .child(
                                        h_flex().w_full().justify_end().child(
                                            Button::new(format!(
                                                "home-section-{section_index}-browse-{item_index}"
                                            ))
                                            .ghost()
                                            .label("Open")
                                            .on_click(cx.listener(move |this, _, _, cx| {
                                                this.open_online_browse(selected.clone(), cx);
                                            })),
                                        ),
                                    )
                                    .into_any_element()
                            }
                        },
                    )),
            )
            .into_any_element()
    }

    fn render_home_feed(&self, cx: &mut Context<Self>) -> AnyElement {
        match &self.home_state {
            HomeFeedState::Loading => v_flex()
                .min_h(px(180.))
                .items_center()
                .justify_center()
                .gap_3()
                .rounded(cx.theme().radius_lg)
                .border_1()
                .border_color(cx.theme().border)
                .child(Icon::new(IconName::LoaderCircle).size_8())
                .child("Loading recommendations…")
                .into_any_element(),
            HomeFeedState::Failed(message) => v_flex()
                .min_h(px(180.))
                .items_center()
                .justify_center()
                .gap_3()
                .rounded(cx.theme().radius_lg)
                .border_1()
                .border_color(cx.theme().border)
                .child(Icon::new(IconName::TriangleAlert).size_8())
                .child(div().font_semibold().child("Recommendations unavailable"))
                .child(
                    div()
                        .max_w(px(620.))
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .child(message.clone()),
                )
                .child(
                    Button::new("retry-home-feed")
                        .label("Try again")
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.reload_home(None, None, cx);
                        })),
                )
                .into_any_element(),
            HomeFeedState::Loaded(page) => {
                let chip_row = (!page.chips.is_empty()).then(|| {
                    h_flex()
                        .flex_wrap()
                        .gap_2()
                        .children(page.chips.iter().enumerate().map(|(index, chip)| {
                            let selected_chip = chip.clone();
                            Button::new(format!("home-chip-{index}"))
                                .label(chip.title.clone())
                                .selected(self.selected_home_chip.as_ref() == Some(chip))
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.toggle_home_chip(selected_chip.clone(), cx);
                                }))
                        }))
                        .into_any_element()
                });
                let sections =
                    if page.sections.is_empty() {
                        div()
                            .rounded(cx.theme().radius)
                            .border_1()
                            .border_color(cx.theme().border)
                            .p_5()
                            .text_sm()
                            .text_color(cx.theme().muted_foreground)
                            .child("YouTube Music returned no recommendations for this selection.")
                            .into_any_element()
                    } else {
                        v_flex()
                            .gap_7()
                            .children(page.sections.iter().enumerate().map(|(index, section)| {
                                self.render_home_section(index, section, cx)
                            }))
                            .into_any_element()
                    };

                v_flex()
                    .gap_5()
                    .child(
                        v_flex()
                            .gap_1()
                            .child(div().text_xl().font_semibold().child("Made for you"))
                            .child(
                                div()
                                    .text_sm()
                                    .text_color(cx.theme().muted_foreground)
                                    .child("Anonymous recommendations from YouTube Music."),
                            ),
                    )
                    .when_some(chip_row, |layout, chips| layout.child(chips))
                    .child(sections)
                    .when_some(self.home_load_more_error.clone(), |layout, error| {
                        layout.child(div().text_sm().text_color(cx.theme().danger).child(error))
                    })
                    .when(page.continuation.is_some(), |layout| {
                        layout.child(
                            h_flex().justify_center().child(
                                Button::new("load-more-home")
                                    .label(if self.home_load_more_error.is_some() {
                                        "Try loading more again"
                                    } else {
                                        "Load more recommendations"
                                    })
                                    .loading(self.home_loading_more)
                                    .disabled(self.home_loading_more)
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.load_more_home(cx);
                                    })),
                            ),
                        )
                    })
                    .into_any_element()
            }
        }
    }

    fn render_home_account_playlists(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let CloudLibraryViewState::Loaded(library) = &self.cloud_library_state else {
            return None;
        };
        if library.playlists.is_empty() {
            return None;
        }
        let (account_name, account_thumbnail) = match &self.account_state {
            AccountViewState::SignedIn(profile) => {
                (profile.name.clone(), profile.thumbnail_url.as_deref())
            }
            _ => ("YouTube Music".into(), None),
        };
        let has_more = library.playlists.len() > 12;

        Some(
            v_flex()
                .gap_3()
                .child(
                    h_flex()
                        .w_full()
                        .items_center()
                        .justify_between()
                        .gap_3()
                        .child(
                            h_flex()
                                .items_center()
                                .gap_3()
                                .child(self.render_thumbnail(
                                    account_thumbnail,
                                    px(36.),
                                    IconName::User,
                                    cx,
                                ))
                                .child(
                                    v_flex()
                                        .child(div().text_lg().font_semibold().child(account_name))
                                        .child(
                                            div()
                                                .text_sm()
                                                .text_color(cx.theme().muted_foreground)
                                                .child("Your YouTube Music playlists"),
                                        ),
                                ),
                        )
                        .when(has_more, |header| {
                            header.child(
                                Button::new("home-account-playlists-view-all")
                                    .ghost()
                                    .label("View all in Library")
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.navigate(Route::Library, cx);
                                    })),
                            )
                        }),
                )
                .child(
                    h_flex().flex_wrap().gap_3().children(
                        library
                            .playlists
                            .iter()
                            .take(12)
                            .enumerate()
                            .map(|(index, item)| {
                                let selected = item.clone();
                                v_flex()
                                    .w(px(260.))
                                    .min_w_0()
                                    .gap_3()
                                    .rounded(cx.theme().radius)
                                    .border_1()
                                    .border_color(cx.theme().border)
                                    .bg(cx.theme().secondary)
                                    .p_3()
                                    .child(
                                        h_flex()
                                            .w_full()
                                            .min_w_0()
                                            .items_center()
                                            .gap_3()
                                            .child(self.render_thumbnail(
                                                item.thumbnail_url.as_deref(),
                                                px(48.),
                                                IconName::BookOpen,
                                                cx,
                                            ))
                                            .child(
                                                v_flex()
                                                    .flex_1()
                                                    .min_w_0()
                                                    .child(
                                                        div()
                                                            .font_medium()
                                                            .overflow_hidden()
                                                            .child(item.title.clone()),
                                                    )
                                                    .child(
                                                        div()
                                                            .text_sm()
                                                            .text_color(cx.theme().muted_foreground)
                                                            .overflow_hidden()
                                                            .child(item.subtitle.clone()),
                                                    ),
                                            ),
                                    )
                                    .child(
                                        h_flex().w_full().justify_end().child(
                                            Button::new(format!("home-account-playlist-{index}"))
                                                .ghost()
                                                .label("Open")
                                                .on_click(cx.listener(move |this, _, _, cx| {
                                                    this.open_online_browse(selected.clone(), cx);
                                                })),
                                        ),
                                    )
                                    .into_any_element()
                            }),
                    ),
                )
                .into_any_element(),
        )
    }

    fn render_home_quick_picks(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let songs = self.home_quick_picks();
        if songs.is_empty() {
            return None;
        }
        let section = HomeSection {
            title: "Quick picks".into(),
            label: Some("A mix of recent listening and recommendations from your library.".into()),
            thumbnail_url: None,
            more: None,
            items: songs.into_iter().map(HomeItem::Song).collect(),
        };
        Some(self.render_home_section_with_play_all(10_001, &section, cx))
    }

    fn render_home_daily_discover(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let StoredViewState::Loaded(discoveries) = &self.daily_discover_state else {
            return None;
        };
        if discoveries.is_empty() {
            return None;
        }
        let queue = Arc::new(
            discoveries
                .iter()
                .map(|item| item.recommendation.clone())
                .collect::<Vec<_>>(),
        );
        let play_all = queue.clone();

        Some(
            v_flex()
                .gap_3()
                .child(
                    h_flex()
                        .w_full()
                        .items_end()
                        .justify_between()
                        .gap_3()
                        .child(
                            v_flex()
                                .gap_1()
                                .child(div().text_lg().font_semibold().child("Your Daily Discover"))
                                .child(
                                    div()
                                        .text_sm()
                                        .text_color(cx.theme().muted_foreground)
                                        .child("Recommendations based on your local favorites."),
                                ),
                        )
                        .child(
                            Button::new("home-daily-discover-play-all")
                                .primary()
                                .icon(IconName::Play)
                                .label("Play all")
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    this.play_song_collection(
                                        play_all.as_ref().clone(),
                                        0,
                                        window,
                                        cx,
                                    );
                                })),
                        ),
                )
                .child(
                    h_flex()
                        .flex_wrap()
                        .gap_3()
                        .children(discoveries.iter().enumerate().map(|(index, item)| {
                            let play_queue = queue.clone();
                            let song = &item.recommendation;
                            v_flex()
                                .w(px(320.))
                                .min_w_0()
                                .gap_3()
                                .rounded(cx.theme().radius)
                                .border_1()
                                .border_color(cx.theme().border)
                                .bg(cx.theme().secondary)
                                .p_3()
                                .child(
                                    h_flex()
                                        .w_full()
                                        .min_w_0()
                                        .items_center()
                                        .gap_3()
                                        .child(self.render_thumbnail(
                                            song.thumbnail_url.as_deref(),
                                            px(56.),
                                            IconName::Play,
                                            cx,
                                        ))
                                        .child(
                                            v_flex()
                                                .flex_1()
                                                .min_w_0()
                                                .child(
                                                    div()
                                                        .font_medium()
                                                        .overflow_hidden()
                                                        .child(song.title.clone()),
                                                )
                                                .child(
                                                    div()
                                                        .text_sm()
                                                        .text_color(cx.theme().muted_foreground)
                                                        .overflow_hidden()
                                                        .child(song.artist_line()),
                                                )
                                                .child(
                                                    div()
                                                        .mt_1()
                                                        .text_xs()
                                                        .text_color(cx.theme().muted_foreground)
                                                        .overflow_hidden()
                                                        .child(format!(
                                                            "Because you listen to {}",
                                                            item.seed.title
                                                        )),
                                                ),
                                        ),
                                )
                                .child(
                                    h_flex()
                                        .w_full()
                                        .flex_wrap()
                                        .gap_1()
                                        .justify_end()
                                        .child(self.download_button(
                                            format!("download-home-daily-{index}"),
                                            song,
                                            cx,
                                        ))
                                        .child(self.queue_insert_buttons(
                                            format!("home-daily-{index}"),
                                            song,
                                            cx,
                                        ))
                                        .child(
                                            Button::new(format!("home-daily-{index}-play"))
                                                .ghost()
                                                .icon(IconName::Play)
                                                .label("Play")
                                                .on_click(cx.listener(
                                                    move |this, _, window, cx| {
                                                        this.play_song_collection(
                                                            play_queue.as_ref().clone(),
                                                            index,
                                                            window,
                                                            cx,
                                                        );
                                                    },
                                                )),
                                        ),
                                )
                                .into_any_element()
                        })),
                )
                .into_any_element(),
        )
    }

    fn render_home_keep_listening(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let StoredViewState::Loaded(songs) = &self.keep_listening_state else {
            return None;
        };
        if songs.is_empty() {
            return None;
        }
        let section = HomeSection {
            title: "Keep listening".into(),
            label: Some("More from your most-played songs in the last two weeks.".into()),
            thumbnail_url: None,
            more: None,
            items: songs.iter().cloned().map(HomeItem::Song).collect(),
        };
        Some(self.render_home_section(10_000, &section, cx))
    }

    fn render_home_forgotten_favorites(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let StoredViewState::Loaded(songs) = &self.forgotten_favorites_state else {
            return None;
        };
        if songs.is_empty() {
            return None;
        }
        let queue = Arc::new(songs.clone());
        let play_all = queue.clone();

        Some(
            v_flex()
                .gap_3()
                .child(
                    h_flex()
                        .w_full()
                        .items_end()
                        .justify_between()
                        .gap_3()
                        .child(
                            v_flex()
                                .gap_1()
                                .child(div().text_lg().font_semibold().child("Forgotten favorites"))
                                .child(
                                    div()
                                        .text_sm()
                                        .text_color(cx.theme().muted_foreground)
                                        .child(
                                            "Songs you played much more before the last 30 days.",
                                        ),
                                ),
                        )
                        .child(
                            Button::new("home-forgotten-play-all")
                                .primary()
                                .icon(IconName::Play)
                                .label("Play all")
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    this.play_song_collection(
                                        play_all.as_ref().clone(),
                                        0,
                                        window,
                                        cx,
                                    );
                                })),
                        ),
                )
                .child(
                    h_flex()
                        .flex_wrap()
                        .gap_3()
                        .children(songs.iter().enumerate().map(|(index, song)| {
                            let play_queue = queue.clone();
                            v_flex()
                                .w(px(300.))
                                .min_w_0()
                                .gap_3()
                                .rounded(cx.theme().radius)
                                .border_1()
                                .border_color(cx.theme().border)
                                .bg(cx.theme().secondary)
                                .p_3()
                                .child(
                                    h_flex()
                                        .w_full()
                                        .min_w_0()
                                        .items_center()
                                        .gap_3()
                                        .child(self.render_thumbnail(
                                            song.thumbnail_url.as_deref(),
                                            px(48.),
                                            IconName::Play,
                                            cx,
                                        ))
                                        .child(
                                            v_flex()
                                                .flex_1()
                                                .min_w_0()
                                                .child(
                                                    div()
                                                        .font_medium()
                                                        .overflow_hidden()
                                                        .child(song.title.clone()),
                                                )
                                                .child(
                                                    div()
                                                        .text_sm()
                                                        .text_color(cx.theme().muted_foreground)
                                                        .overflow_hidden()
                                                        .child(song.artist_line()),
                                                ),
                                        ),
                                )
                                .child(
                                    h_flex()
                                        .w_full()
                                        .flex_wrap()
                                        .gap_1()
                                        .justify_end()
                                        .child(self.download_button(
                                            format!("download-home-forgotten-{index}"),
                                            song,
                                            cx,
                                        ))
                                        .child(self.queue_insert_buttons(
                                            format!("home-forgotten-{index}"),
                                            song,
                                            cx,
                                        ))
                                        .child(
                                            Button::new(format!("home-forgotten-{index}-play"))
                                                .ghost()
                                                .icon(IconName::Play)
                                                .label("Play")
                                                .on_click(cx.listener(
                                                    move |this, _, window, cx| {
                                                        this.play_song_collection(
                                                            play_queue.as_ref().clone(),
                                                            index,
                                                            window,
                                                            cx,
                                                        );
                                                    },
                                                )),
                                        ),
                                )
                                .into_any_element()
                        })),
                )
                .into_any_element(),
        )
    }

    fn render_home(&self, cx: &mut Context<Self>) -> AnyElement {
        let recent_songs = Arc::new(self.recent_unique_songs(6));
        let recent_content = if recent_songs.is_empty() {
            div()
                .text_sm()
                .text_color(cx.theme().muted_foreground)
                .child("Play a song for 30 seconds and it will appear here.")
                .into_any_element()
        } else {
            h_flex()
                .flex_wrap()
                .gap_3()
                .children(recent_songs.iter().enumerate().map(|(index, song)| {
                    let songs = recent_songs.clone();
                    v_flex()
                        .w(px(300.))
                        .min_w_0()
                        .gap_3()
                        .rounded(cx.theme().radius)
                        .border_1()
                        .border_color(cx.theme().border)
                        .bg(cx.theme().secondary)
                        .p_3()
                        .child(
                            h_flex()
                                .w_full()
                                .min_w_0()
                                .gap_3()
                                .items_center()
                                .child(self.render_thumbnail(
                                    song.thumbnail_url.as_deref(),
                                    px(48.),
                                    IconName::Play,
                                    cx,
                                ))
                                .child(
                                    v_flex()
                                        .flex_1()
                                        .min_w_0()
                                        .child(
                                            div()
                                                .font_medium()
                                                .overflow_hidden()
                                                .child(song.title.clone()),
                                        )
                                        .child(
                                            div()
                                                .text_sm()
                                                .text_color(cx.theme().muted_foreground)
                                                .overflow_hidden()
                                                .child(song.artist_line()),
                                        ),
                                ),
                        )
                        .child(
                            h_flex()
                                .w_full()
                                .flex_wrap()
                                .gap_1()
                                .justify_end()
                                .child(self.download_button(
                                    format!("download-home-recent-{index}"),
                                    song,
                                    cx,
                                ))
                                .child(self.queue_insert_buttons(
                                    format!("home-recent-{index}"),
                                    song,
                                    cx,
                                ))
                                .child(
                                    Button::new(format!("home-recent-{index}-play"))
                                        .ghost()
                                        .icon(IconName::Play)
                                        .label("Play")
                                        .tooltip("Play from recent history")
                                        .on_click(cx.listener(move |this, _, window, cx| {
                                            this.play_song_collection(
                                                songs.as_ref().clone(),
                                                index,
                                                window,
                                                cx,
                                            );
                                        })),
                                ),
                        )
                        .into_any_element()
                }))
                .into_any_element()
        };

        v_flex()
            .gap_7()
            .child(self.page_heading(
                "Good evening",
                "Continue where you left off or revisit something recent.",
                cx,
            ))
            .when_some(self.current_song.clone(), |layout, song| {
                layout.child(
                    v_flex()
                        .gap_3()
                        .child(div().text_lg().font_semibold().child("Continue listening"))
                        .child(
                            h_flex()
                                .max_w(px(620.))
                                .gap_4()
                                .items_center()
                                .rounded(cx.theme().radius_lg)
                                .border_1()
                                .border_color(cx.theme().border)
                                .bg(cx.theme().secondary)
                                .p_5()
                                .child(
                                    div()
                                        .size_12()
                                        .flex()
                                        .items_center()
                                        .justify_center()
                                        .rounded(cx.theme().radius)
                                        .bg(cx.theme().primary)
                                        .text_color(cx.theme().primary_foreground)
                                        .child(Icon::new(IconName::Play)),
                                )
                                .child(
                                    v_flex()
                                        .flex_1()
                                        .min_w_0()
                                        .child(div().font_semibold().child(song.title.clone()))
                                        .child(
                                            div()
                                                .text_sm()
                                                .text_color(cx.theme().muted_foreground)
                                                .child(song.artist_line()),
                                        ),
                                )
                                .child(
                                    Button::new("home-continue")
                                        .primary()
                                        .icon(IconName::Play)
                                        .label("Resume")
                                        .on_click(cx.listener(|this, _, window, cx| {
                                            this.toggle_playback(window, cx);
                                        })),
                                ),
                        ),
                )
            })
            .child(
                v_flex()
                    .gap_3()
                    .child(div().text_lg().font_semibold().child("Recently played"))
                    .child(recent_content),
            )
            .when_some(self.render_home_quick_picks(cx), |layout, songs| {
                layout.child(songs)
            })
            .when_some(self.render_home_keep_listening(cx), |layout, songs| {
                layout.child(songs)
            })
            .when_some(
                self.render_home_daily_discover(cx),
                |layout, discoveries| layout.child(discoveries),
            )
            .when_some(
                self.render_home_account_playlists(cx),
                |layout, playlists| layout.child(playlists),
            )
            .when_some(self.render_home_forgotten_favorites(cx), |layout, songs| {
                layout.child(songs)
            })
            .child(self.render_home_feed(cx))
            .child(
                v_flex()
                    .gap_3()
                    .child(div().text_lg().font_semibold().child("Start here"))
                    .child(
                        h_flex()
                            .flex_wrap()
                            .gap_4()
                            .child(self.feature_card(
                                IconName::Search,
                                "Search music",
                                "Find songs through an anonymous YouTube Music session.",
                                cx,
                            ))
                            .child(self.feature_card(
                                IconName::Play,
                                "Ready to play",
                                "Resolve and play AAC audio through the desktop output device.",
                                cx,
                            ))
                            .child(self.feature_card(
                                IconName::BookOpen,
                                "Your library",
                                "Keep favourites and build playlists that stay on this device.",
                                cx,
                            )),
                    ),
            )
            .into_any_element()
    }

    fn render_explore(&self, cx: &mut Context<Self>) -> AnyElement {
        let content = match &self.explore_state {
            ExploreFeedState::Loading => v_flex()
                .min_h(px(300.))
                .items_center()
                .justify_center()
                .gap_3()
                .child(Icon::new(IconName::LoaderCircle).size_8())
                .child("Loading Explore…")
                .into_any_element(),
            ExploreFeedState::Failed(message) => v_flex()
                .min_h(px(300.))
                .items_center()
                .justify_center()
                .gap_3()
                .child(Icon::new(IconName::TriangleAlert).size_8())
                .child(div().font_semibold().child("Explore unavailable"))
                .child(
                    div()
                        .max_w(px(620.))
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .child(message.clone()),
                )
                .child(
                    Button::new("retry-explore")
                        .label("Try again")
                        .on_click(cx.listener(|this, _, _, cx| this.reload_explore(cx))),
                )
                .into_any_element(),
            ExploreFeedState::Loaded(page)
                if page.chart_sections.is_empty()
                    && page.new_release_albums.is_empty()
                    && page.new_releases_more.is_none()
                    && page.categories.is_empty() =>
            {
                v_flex()
                    .min_h(px(300.))
                    .items_center()
                    .justify_center()
                    .gap_2()
                    .child(Icon::new(IconName::BookOpen).size_8())
                    .child(div().font_semibold().child("Nothing to explore yet"))
                    .child(
                        div()
                            .text_sm()
                            .text_color(cx.theme().muted_foreground)
                            .child("YouTube Music returned no supported shelves."),
                    )
                    .into_any_element()
            }
            ExploreFeedState::Loaded(page) => v_flex()
                .gap_7()
                .when(!page.chart_sections.is_empty(), |layout| {
                    layout.child(
                        v_flex().gap_7().children(
                            page.chart_sections
                                .iter()
                                .enumerate()
                                .map(|(index, section)| {
                                    self.render_home_section(index, section, cx)
                                }),
                        ),
                    )
                })
                .when(
                    !page.new_release_albums.is_empty() || page.new_releases_more.is_some(),
                    |layout| {
                        layout.child(
                            v_flex()
                                .gap_3()
                                .child(
                                    h_flex()
                                        .items_center()
                                        .justify_between()
                                        .child(
                                            div().text_lg().font_semibold().child("New releases"),
                                        )
                                        .when_some(
                                            page.new_releases_more.clone(),
                                            |header, selected| {
                                                header.child(
                                                    Button::new("explore-new-releases-all")
                                                        .ghost()
                                                        .label("View all")
                                                        .on_click(cx.listener(
                                                            move |this, _, _, cx| {
                                                                this.open_online_browse(
                                                                    selected.clone(),
                                                                    cx,
                                                                );
                                                            },
                                                        )),
                                                )
                                            },
                                        ),
                                )
                                .child(h_flex().flex_wrap().gap_3().children(
                                    page.new_release_albums.iter().enumerate().map(
                                        |(index, album)| {
                                            let selected = album.clone();
                                            h_flex()
                                                .w(px(300.))
                                                .min_w_0()
                                                .gap_3()
                                                .items_center()
                                                .rounded(cx.theme().radius)
                                                .border_1()
                                                .border_color(cx.theme().border)
                                                .bg(cx.theme().secondary)
                                                .p_3()
                                                .child(self.render_thumbnail(
                                                    album.thumbnail_url.as_deref(),
                                                    px(52.),
                                                    IconName::BookOpen,
                                                    cx,
                                                ))
                                                .child(
                                                    v_flex()
                                                        .flex_1()
                                                        .min_w_0()
                                                        .child(
                                                            div()
                                                                .font_medium()
                                                                .overflow_hidden()
                                                                .child(album.title.clone()),
                                                        )
                                                        .child(
                                                            div()
                                                                .text_sm()
                                                                .text_color(
                                                                    cx.theme().muted_foreground,
                                                                )
                                                                .overflow_hidden()
                                                                .child(album.subtitle.clone()),
                                                        ),
                                                )
                                                .child(
                                                    Button::new(format!("explore-album-{index}"))
                                                        .ghost()
                                                        .label("Open")
                                                        .on_click(cx.listener(
                                                            move |this, _, _, cx| {
                                                                this.open_online_browse(
                                                                    selected.clone(),
                                                                    cx,
                                                                );
                                                            },
                                                        )),
                                                )
                                                .into_any_element()
                                        },
                                    ),
                                )),
                        )
                    },
                )
                .when(!page.categories.is_empty(), |layout| {
                    layout.child(
                        v_flex()
                            .gap_3()
                            .child(div().text_lg().font_semibold().child("Moods & genres"))
                            .child(h_flex().flex_wrap().gap_3().children(
                                page.categories.iter().enumerate().map(|(index, category)| {
                                    let item = category.browse_item();
                                    Button::new(format!("explore-category-{index}"))
                                        .w(px(220.))
                                        .justify_start()
                                        .icon(IconName::BookOpen)
                                        .label(category.title.clone())
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            this.open_online_browse(item.clone(), cx);
                                        }))
                                }),
                            )),
                    )
                })
                .into_any_element(),
        };

        v_flex()
            .gap_6()
            .child(self.page_heading("Explore", "Discover new releases, moods, and genres.", cx))
            .child(content)
            .into_any_element()
    }

    fn visible_search_history(&self) -> Vec<SearchHistoryEntry> {
        let query = self.model.search_query().trim().to_lowercase();
        let limit = if query.is_empty() { usize::MAX } else { 3 };
        match &self.search_history_state {
            StoredViewState::Loaded(history) => history
                .iter()
                .filter(|entry| entry.query.to_lowercase().starts_with(&query))
                .take(limit)
                .cloned()
                .collect(),
            StoredViewState::Loading | StoredViewState::Failed(_) => Vec::new(),
        }
    }

    fn render_search_history(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let query_is_empty = self.model.search_query().trim().is_empty();
        match &self.search_history_state {
            StoredViewState::Loading if query_is_empty => {
                return Some(
                    h_flex()
                        .max_w(px(720.))
                        .w_full()
                        .gap_2()
                        .items_center()
                        .rounded(cx.theme().radius)
                        .border_1()
                        .border_color(cx.theme().border)
                        .bg(cx.theme().secondary)
                        .p_3()
                        .child(Icon::new(IconName::LoaderCircle))
                        .child("Loading recent searches…")
                        .into_any_element(),
                );
            }
            StoredViewState::Failed(error) if query_is_empty => {
                return Some(
                    h_flex()
                        .max_w(px(720.))
                        .w_full()
                        .gap_3()
                        .items_center()
                        .rounded(cx.theme().radius)
                        .border_1()
                        .border_color(cx.theme().border)
                        .bg(cx.theme().secondary)
                        .p_3()
                        .child(Icon::new(IconName::TriangleAlert))
                        .child(
                            v_flex()
                                .flex_1()
                                .child(
                                    div()
                                        .text_sm()
                                        .font_medium()
                                        .child("Search history unavailable"),
                                )
                                .child(
                                    div()
                                        .text_xs()
                                        .text_color(cx.theme().muted_foreground)
                                        .child(error.clone()),
                                ),
                        )
                        .child(
                            Button::new("retry-search-history").label("Retry").on_click(
                                cx.listener(|this, _, _, cx| this.reload_search_history(cx)),
                            ),
                        )
                        .into_any_element(),
                );
            }
            StoredViewState::Loading | StoredViewState::Failed(_) | StoredViewState::Loaded(_) => {}
        }

        let entries = self.visible_search_history();
        if entries.is_empty() && self.search_history_error.is_none() {
            return None;
        }
        let total_count = match &self.search_history_state {
            StoredViewState::Loaded(history) => history.len(),
            StoredViewState::Loading | StoredViewState::Failed(_) => 0,
        };
        let busy = self.search_history_task.is_some();
        Some(
            v_flex()
                .max_w(px(720.))
                .w_full()
                .gap_2()
                .rounded(cx.theme().radius)
                .border_1()
                .border_color(cx.theme().border)
                .bg(cx.theme().secondary)
                .p_3()
                .when(!entries.is_empty(), |layout| {
                    layout.child(
                        h_flex()
                            .items_center()
                            .justify_between()
                            .gap_3()
                            .child(
                                div()
                                    .text_xs()
                                    .font_medium()
                                    .text_color(cx.theme().muted_foreground)
                                    .child("Recent searches"),
                            )
                            .when(query_is_empty && total_count > 0, |heading| {
                                heading.child(
                                    Button::new("clear-search-history")
                                        .ghost()
                                        .label("Clear all")
                                        .disabled(busy)
                                        .on_click(cx.listener(move |this, _, window, cx| {
                                            this.confirm_clear_search_history(
                                                total_count,
                                                window,
                                                cx,
                                            );
                                        })),
                                )
                            }),
                    )
                })
                .when_some(self.search_history_error.clone(), |layout, error| {
                    layout.child(
                        div()
                            .rounded(cx.theme().radius)
                            .bg(cx.theme().danger.opacity(0.12))
                            .text_color(cx.theme().danger)
                            .p_2()
                            .text_sm()
                            .child(error),
                    )
                })
                .children(entries.into_iter().enumerate().map(|(index, entry)| {
                    let submit_query = entry.query.clone();
                    let fill_query = entry.query.clone();
                    let id = entry.id;
                    h_flex()
                        .w_full()
                        .gap_2()
                        .items_center()
                        .child(
                            Button::new(format!("search-history-{index}"))
                                .ghost()
                                .flex_1()
                                .justify_start()
                                .icon(IconName::Undo2)
                                .label(entry.query)
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    this.apply_search_suggestion(
                                        submit_query.clone(),
                                        true,
                                        window,
                                        cx,
                                    );
                                })),
                        )
                        .child(
                            Button::new(format!("fill-search-history-{index}"))
                                .ghost()
                                .label("Fill")
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    this.apply_search_suggestion(
                                        fill_query.clone(),
                                        false,
                                        window,
                                        cx,
                                    );
                                })),
                        )
                        .child(
                            Button::new(format!("delete-search-history-{index}"))
                                .ghost()
                                .label("Delete")
                                .disabled(busy)
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.delete_search_history(id, cx);
                                })),
                        )
                }))
                .into_any_element(),
        )
    }

    fn render_search_suggestions(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        match &self.search_suggestion_state {
            SearchSuggestionViewState::Hidden => None,
            SearchSuggestionViewState::Loading => Some(
                h_flex()
                    .max_w(px(720.))
                    .w_full()
                    .gap_2()
                    .items_center()
                    .rounded(cx.theme().radius)
                    .border_1()
                    .border_color(cx.theme().border)
                    .bg(cx.theme().secondary)
                    .p_3()
                    .child(Icon::new(IconName::LoaderCircle))
                    .child(
                        div()
                            .text_sm()
                            .text_color(cx.theme().muted_foreground)
                            .child("Loading suggestions…"),
                    )
                    .into_any_element(),
            ),
            SearchSuggestionViewState::Failed(message) => Some(
                h_flex()
                    .max_w(px(720.))
                    .w_full()
                    .gap_3()
                    .items_center()
                    .rounded(cx.theme().radius)
                    .border_1()
                    .border_color(cx.theme().border)
                    .bg(cx.theme().secondary)
                    .p_3()
                    .child(Icon::new(IconName::TriangleAlert))
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .child(
                                div()
                                    .text_sm()
                                    .font_medium()
                                    .child("Suggestions unavailable"),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .overflow_hidden()
                                    .child(message.clone()),
                            ),
                    )
                    .child(
                        Button::new("retry-search-suggestions")
                            .ghost()
                            .label("Retry")
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.schedule_search_suggestions(window, cx);
                            })),
                    )
                    .into_any_element(),
            ),
            SearchSuggestionViewState::Loaded(suggestions) => Some(
                v_flex()
                    .max_w(px(720.))
                    .w_full()
                    .gap_2()
                    .rounded(cx.theme().radius)
                    .border_1()
                    .border_color(cx.theme().border)
                    .bg(cx.theme().secondary)
                    .p_3()
                    .child(
                        div()
                            .text_xs()
                            .font_medium()
                            .text_color(cx.theme().muted_foreground)
                            .child("Suggestions"),
                    )
                    .children(
                        suggestions
                            .queries
                            .iter()
                            .filter(|query| {
                                !matches!(
                                    &self.search_history_state,
                                    StoredViewState::Loaded(history)
                                        if history.iter().any(|entry| entry.query.as_str() == query.as_str())
                                )
                            })
                            .enumerate()
                            .map(|(index, query)| {
                                let submit_query = query.clone();
                                let fill_query = query.clone();
                                h_flex()
                                    .w_full()
                                    .gap_2()
                                    .items_center()
                                    .child(
                                        Button::new(format!("search-suggestion-{index}"))
                                            .ghost()
                                            .flex_1()
                                            .justify_start()
                                            .icon(IconName::Search)
                                            .label(query.clone())
                                            .on_click(cx.listener(move |this, _, window, cx| {
                                                this.apply_search_suggestion(
                                                    submit_query.clone(),
                                                    true,
                                                    window,
                                                    cx,
                                                );
                                            })),
                                    )
                                    .child(
                                        Button::new(format!("fill-search-suggestion-{index}"))
                                            .ghost()
                                            .label("Fill")
                                            .tooltip("Put this suggestion in the search box")
                                            .on_click(cx.listener(move |this, _, window, cx| {
                                                this.apply_search_suggestion(
                                                    fill_query.clone(),
                                                    false,
                                                    window,
                                                    cx,
                                                );
                                            })),
                                    )
                            }),
                    )
                    .when(!suggestions.songs.is_empty(), |layout| {
                        layout.child(
                            div()
                                .mt_2()
                                .text_xs()
                                .font_medium()
                                .text_color(cx.theme().muted_foreground)
                                .child("Recommended tracks"),
                        )
                    })
                    .children(suggestions.songs.iter().enumerate().map(|(index, song)| {
                        let selected = song.clone();
                        h_flex()
                            .w_full()
                            .gap_3()
                            .items_center()
                            .rounded(cx.theme().radius)
                            .p_2()
                            .child(self.render_thumbnail(
                                song.thumbnail_url.as_deref(),
                                px(40.),
                                IconName::Play,
                                cx,
                            ))
                            .child(
                                v_flex()
                                    .flex_1()
                                    .min_w_0()
                                    .child(div().text_sm().font_medium().child(song.title.clone()))
                                    .child(
                                        div()
                                            .text_xs()
                                            .text_color(cx.theme().muted_foreground)
                                            .overflow_hidden()
                                            .child(song.artist_line()),
                                    ),
                            )
                            .child(self.queue_insert_buttons(
                                format!("search-recommendation-{index}"),
                                song,
                                cx,
                            ))
                            .child(
                                Button::new(format!("play-search-recommendation-{index}"))
                                    .ghost()
                                    .icon(IconName::Play)
                                    .label("Play")
                                    .on_click(cx.listener(move |this, _, window, cx| {
                                        this.play_search_suggestion(selected.clone(), window, cx);
                                    })),
                            )
                    }))
                    .when(!suggestions.items.is_empty(), |layout| {
                        layout.child(
                            div()
                                .mt_2()
                                .text_xs()
                                .font_medium()
                                .text_color(cx.theme().muted_foreground)
                                .child("Recommended pages"),
                        )
                    })
                    .children(suggestions.items.iter().enumerate().map(|(index, item)| {
                        let selected = item.clone();
                        let icon = if item.kind == BrowseKind::Artist {
                            IconName::User
                        } else {
                            IconName::BookOpen
                        };
                        h_flex()
                            .w_full()
                            .gap_3()
                            .items_center()
                            .rounded(cx.theme().radius)
                            .p_2()
                            .child(self.render_thumbnail(
                                item.thumbnail_url.as_deref(),
                                px(40.),
                                icon,
                                cx,
                            ))
                            .child(
                                v_flex()
                                    .flex_1()
                                    .min_w_0()
                                    .child(div().text_sm().font_medium().child(item.title.clone()))
                                    .child(
                                        div()
                                            .text_xs()
                                            .text_color(cx.theme().muted_foreground)
                                            .overflow_hidden()
                                            .child(item.subtitle.clone()),
                                    ),
                            )
                            .child(
                                Button::new(format!("open-search-recommendation-{index}"))
                                    .ghost()
                                    .label("Open")
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.open_search_suggestion(selected.clone(), cx);
                                    })),
                            )
                    }))
                    .into_any_element(),
            ),
        }
    }

    fn render_search_results(&self, cx: &mut Context<Self>) -> AnyElement {
        match &self.search_state {
            SearchViewState::Idle => v_flex()
                .flex_1()
                .min_h(px(260.))
                .items_center()
                .justify_center()
                .rounded(cx.theme().radius_lg)
                .border_1()
                .border_color(cx.theme().border)
                .child(
                    div()
                        .size_12()
                        .mb_4()
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded_full()
                        .bg(cx.theme().muted)
                        .child(Icon::new(IconName::Search)),
                )
                .child(
                    div()
                        .font_semibold()
                        .child(if self.model.search_query().is_empty() {
                            "Search for something you love".to_owned()
                        } else {
                            format!("Press Enter or Search for “{}”", self.model.search_query())
                        }),
                )
                .child(
                    div()
                        .mt_2()
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .child("Network work runs asynchronously and never blocks the UI thread."),
                )
                .into_any_element(),
            SearchViewState::Loading => v_flex()
                .flex_1()
                .min_h(px(260.))
                .items_center()
                .justify_center()
                .gap_3()
                .child(Icon::new(IconName::LoaderCircle).size_8())
                .child(div().font_medium().child("Searching YouTube Music…"))
                .child(
                    div()
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .child("Using an anonymous InnerTube session"),
                )
                .into_any_element(),
            SearchViewState::Empty => v_flex()
                .flex_1()
                .min_h(px(260.))
                .items_center()
                .justify_center()
                .gap_2()
                .child(Icon::new(IconName::Search).size_8())
                .child(div().font_semibold().child(format!(
                    "No {} found",
                    self.search_filter.label().to_lowercase()
                )))
                .child(
                    div()
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .child("Try a different title or artist."),
                )
                .into_any_element(),
            SearchViewState::Failed(message) => v_flex()
                .flex_1()
                .min_h(px(260.))
                .items_center()
                .justify_center()
                .gap_3()
                .child(Icon::new(IconName::TriangleAlert).size_8())
                .child(div().font_semibold().child("Search failed"))
                .child(
                    div()
                        .max_w(px(560.))
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .child(message.clone()),
                )
                .child(
                    Button::new("retry-search")
                        .label("Try again")
                        .on_click(cx.listener(|this, _, window, cx| this.run_search(window, cx))),
                )
                .into_any_element(),
            SearchViewState::Loaded(result) if self.search_filter.returns_songs() => {
                let pagination = self.render_search_pagination(result.continuation.is_some(), cx);
                v_flex()
                    .gap_2()
                    .child(
                        div()
                            .mb_2()
                            .text_sm()
                            .text_color(cx.theme().muted_foreground)
                            .child(format!(
                                "{} {}",
                                result.songs.len(),
                                self.search_filter.label().to_lowercase()
                            )),
                    )
                    .children(
                        result
                            .songs
                            .iter()
                            .enumerate()
                            .map(|(index, song)| self.render_song_row(index, song, cx)),
                    )
                    .when_some(pagination, |layout, pagination| layout.child(pagination))
                    .into_any_element()
            }
            SearchViewState::Loaded(result) => {
                let pagination = self.render_search_pagination(result.continuation.is_some(), cx);
                v_flex()
                    .gap_2()
                    .child(
                        div()
                            .mb_2()
                            .text_sm()
                            .text_color(cx.theme().muted_foreground)
                            .child(format!(
                                "{} {}",
                                result.items.len(),
                                self.search_filter.label().to_lowercase()
                            )),
                    )
                    .children(
                        result
                            .items
                            .iter()
                            .enumerate()
                            .map(|(index, item)| self.render_browse_item_row(index, item, cx)),
                    )
                    .when_some(pagination, |layout, pagination| layout.child(pagination))
                    .into_any_element()
            }
        }
    }

    fn render_search_pagination(
        &self,
        has_continuation: bool,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        if !has_continuation && !self.search_loading_more && self.search_load_more_error.is_none() {
            return None;
        }
        Some(
            v_flex()
                .mt_3()
                .items_center()
                .gap_2()
                .when_some(self.search_load_more_error.clone(), |layout, error| {
                    layout.child(
                        div()
                            .max_w(px(620.))
                            .text_sm()
                            .text_color(cx.theme().danger)
                            .child(error),
                    )
                })
                .when(has_continuation, |layout| {
                    layout.child(
                        Button::new("load-more-search-results")
                            .label(if self.search_load_more_error.is_some() {
                                "Try loading more again"
                            } else {
                                "Load more"
                            })
                            .loading(self.search_loading_more)
                            .disabled(self.search_loading_more)
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.load_more_search_results(cx);
                            })),
                    )
                })
                .into_any_element(),
        )
    }

    fn render_thumbnail(
        &self,
        url: Option<&str>,
        size: Pixels,
        fallback_icon: IconName,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let url = url.map(str::trim).filter(|url| !url.is_empty());
        let frame = div()
            .size(size)
            .flex_shrink_0()
            .overflow_hidden()
            .rounded(cx.theme().radius)
            .bg(cx.theme().muted);
        if let Some(image) = url.and_then(|url| self.thumbnail_images.get(url)) {
            return frame
                .child(
                    img(image.clone())
                        .size_full()
                        .object_fit(ObjectFit::Cover)
                        .with_loading(|| {
                            div()
                                .size_full()
                                .flex()
                                .items_center()
                                .justify_center()
                                .child(Icon::new(IconName::LoaderCircle))
                                .into_any_element()
                        })
                        .with_fallback(|| {
                            div()
                                .size_full()
                                .flex()
                                .items_center()
                                .justify_center()
                                .child(Icon::new(IconName::TriangleAlert))
                                .into_any_element()
                        }),
                )
                .into_any_element();
        }

        let placeholder_icon = match url {
            Some(url) if self.thumbnail_failures.contains(url) => IconName::TriangleAlert,
            Some(url) if self.thumbnail_tasks.contains_key(url) => IconName::LoaderCircle,
            _ => fallback_icon,
        };
        frame
            .flex()
            .items_center()
            .justify_center()
            .child(Icon::new(placeholder_icon))
            .into_any_element()
    }

    fn render_browse_item_row(
        &self,
        index: usize,
        item: &BrowseItem,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let selected = item.clone();
        let icon = if item.kind == BrowseKind::Artist {
            IconName::User
        } else {
            IconName::BookOpen
        };
        h_flex()
            .w_full()
            .gap_4()
            .items_center()
            .rounded(cx.theme().radius)
            .border_1()
            .border_color(cx.theme().border)
            .p_3()
            .child(self.render_thumbnail(item.thumbnail_url.as_deref(), px(44.), icon, cx))
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .child(div().font_medium().child(item.title.clone()))
                    .child(
                        div()
                            .text_sm()
                            .text_color(cx.theme().muted_foreground)
                            .child(item.subtitle.clone()),
                    ),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(item.kind.label()),
            )
            .child(
                Button::new(format!("open-browse-result-{index}"))
                    .ghost()
                    .label("Open")
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.open_online_browse(selected.clone(), cx);
                    })),
            )
            .into_any_element()
    }

    fn render_cloud_playlist_row(
        &self,
        index: usize,
        item: &BrowseItem,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let open_item = item.clone();
        let unlike_item = item.clone();
        let rename_item = item.clone();
        let delete_item = item.clone();
        h_flex()
            .w_full()
            .gap_3()
            .items_center()
            .rounded(cx.theme().radius)
            .border_1()
            .border_color(cx.theme().border)
            .p_3()
            .child(self.render_thumbnail(
                item.thumbnail_url.as_deref(),
                px(44.),
                IconName::BookOpen,
                cx,
            ))
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .child(div().font_medium().child(item.title.clone()))
                    .child(
                        div()
                            .text_sm()
                            .text_color(cx.theme().muted_foreground)
                            .child(item.subtitle.clone()),
                    ),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(if item.editable { "Owned" } else { "Saved" }),
            )
            .child(
                Button::new(format!("open-cloud-playlist-{index}"))
                    .ghost()
                    .label("Open")
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.open_online_browse(open_item.clone(), cx);
                    })),
            )
            .when(item.editable, |row| {
                row.child(
                    Button::new(format!("rename-cloud-library-playlist-{index}"))
                        .ghost()
                        .label("Rename")
                        .disabled(self.cloud_busy())
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.open_rename_cloud_playlist_dialog(rename_item.clone(), window, cx);
                        })),
                )
                .child(
                    Button::new(format!("delete-cloud-library-playlist-{index}"))
                        .danger()
                        .label("Delete")
                        .disabled(self.cloud_busy())
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.confirm_delete_cloud_playlist(delete_item.clone(), window, cx);
                        })),
                )
            })
            .when(!item.editable, |row| {
                row.child(
                    Button::new(format!("unlike-cloud-playlist-{index}"))
                        .ghost()
                        .label("Remove from library")
                        .disabled(self.cloud_busy())
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.set_cloud_playlist_liked(unlike_item.clone(), false, cx);
                        })),
                )
            })
            .into_any_element()
    }

    fn download_button(&self, id: String, song: &Song, cx: &mut Context<Self>) -> Button {
        let download_song = song.clone();
        let download_video_id = song.video_id.clone();
        let removing = self.download_removals.contains(&song.video_id);
        let downloads_ready = matches!(self.downloads_state, StoredViewState::Loaded(_));
        let state = self.download_for(&song.video_id).map(|item| item.state);
        let retry = matches!(state, Some(DownloadState::Paused | DownloadState::Failed));
        let label = if removing {
            "Removing…"
        } else {
            match state {
                Some(DownloadState::Completed) => "Offline ✓",
                Some(DownloadState::Queued | DownloadState::Downloading) => "Downloading…",
                Some(DownloadState::Paused | DownloadState::Failed) => "Resume",
                None if downloads_ready => "Download",
                None => "Loading…",
            }
        };
        let disabled = removing
            || !downloads_ready
            || matches!(
                state,
                Some(DownloadState::Completed | DownloadState::Queued | DownloadState::Downloading)
            );
        Button::new(id)
            .ghost()
            .label(label)
            .tooltip(if song.is_episode {
                "Keep this episode for offline playback"
            } else {
                "Keep this song for offline playback"
            })
            .disabled(disabled)
            .on_click(cx.listener(move |this, _, _, cx| {
                if retry {
                    this.retry_download(&download_video_id, cx);
                } else {
                    this.queue_song_download(download_song.clone(), cx);
                }
            }))
    }

    fn queue_insert_buttons(&self, id: String, song: &Song, cx: &mut Context<Self>) -> AnyElement {
        let next_song = song.clone();
        let queued_song = song.clone();
        let guest = self.listen_together_is_guest();
        let queue_tooltip = if self.shuffle_enabled {
            "Add this to the queue; Shuffle may reorder upcoming items"
        } else {
            "Add this to the end of the queue"
        };
        h_flex()
            .flex_wrap()
            .gap_1()
            .child(
                Button::new(format!("{id}-play-next"))
                    .ghost()
                    .icon(IconName::ArrowDown)
                    .label("Next")
                    .tooltip("Play this immediately after the current item")
                    .disabled(guest)
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.play_song_next(next_song.clone(), window, cx);
                    })),
            )
            .child(
                Button::new(format!("{id}-add-queue"))
                    .ghost()
                    .icon(IconName::Plus)
                    .label("Queue")
                    .tooltip(queue_tooltip)
                    .disabled(guest)
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.add_song_to_queue(queued_song.clone(), window, cx);
                    })),
            )
            .into_any_element()
    }

    fn render_song_row(&self, index: usize, song: &Song, cx: &mut Context<Self>) -> AnyElement {
        let duration = song.duration.map(format_duration).unwrap_or_default();
        let favorite = self.is_favorite(&song.video_id);
        let favorite_song = song.clone();
        let playlist_song = song.clone();
        let cloud_liked = self.cloud_video_liked(&song.video_id);
        let cloud_like_song = song.clone();
        let cloud_playlist_song = song.clone();
        let episode_saved = self.is_episode_saved(&song.video_id);
        let later_song = song.clone();
        let saved_position = self.saved_episode_position(&song.video_id);

        h_flex()
            .flex_wrap()
            .w_full()
            .gap_4()
            .items_center()
            .rounded(cx.theme().radius)
            .border_1()
            .border_color(cx.theme().border)
            .p_3()
            .child(
                div()
                    .w(px(24.))
                    .text_center()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child((index + 1).to_string()),
            )
            .child(self.render_thumbnail(
                song.thumbnail_url.as_deref(),
                px(44.),
                IconName::Play,
                cx,
            ))
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .child(div().font_medium().child(song.title.clone()))
                    .child(
                        div()
                            .text_sm()
                            .text_color(cx.theme().muted_foreground)
                            .child(song.artist_line()),
                    )
                    .when_some(saved_position, |details, position| {
                        details.child(
                            div()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child(format!("Resume at {}", format_duration(position))),
                        )
                    }),
            )
            .child(
                div()
                    .w(px(64.))
                    .text_right()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child(duration),
            )
            .child(
                Button::new(format!("favorite-result-{index}"))
                    .ghost()
                    .icon(IconName::Heart)
                    .selected(favorite)
                    .disabled(self.library_busy())
                    .tooltip(if favorite {
                        "Remove from favourites"
                    } else {
                        "Add to favourites"
                    })
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.toggle_favorite(favorite_song.clone(), cx);
                    })),
            )
            .child(
                Button::new(format!("playlist-result-{index}"))
                    .ghost()
                    .icon(IconName::Plus)
                    .tooltip("Add to playlist")
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.open_playlist_picker(playlist_song.clone(), cx);
                    })),
            )
            .when(song.is_episode, |row| {
                row.child(
                    Button::new(format!("later-result-{index}"))
                        .ghost()
                        .label(if episode_saved { "Saved" } else { "Later" })
                        .selected(episode_saved)
                        .tooltip(if episode_saved {
                            "Remove from Episodes for Later"
                        } else {
                            "Save to Episodes for Later"
                        })
                        .disabled(self.podcast_busy())
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.toggle_episode_for_later(later_song.clone(), cx);
                        })),
                )
            })
            .child(self.download_button(format!("download-result-{index}"), song, cx))
            .when(self.account_ready() && !song.is_episode, |row| {
                row.child(
                    Button::new(format!("cloud-like-result-{index}"))
                        .ghost()
                        .label(if cloud_liked { "YT ♥" } else { "YT ♡" })
                        .tooltip(if cloud_liked {
                            "Remove from YouTube Music liked songs"
                        } else {
                            "Add to YouTube Music liked songs"
                        })
                        .disabled(self.cloud_busy())
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.set_cloud_video_liked(cloud_like_song.clone(), !cloud_liked, cx);
                        })),
                )
                .child(
                    Button::new(format!("cloud-playlist-result-{index}"))
                        .ghost()
                        .label("YT +")
                        .tooltip("Add to an editable YouTube Music playlist")
                        .disabled(self.cloud_busy())
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.open_cloud_playlist_picker(cloud_playlist_song.clone(), cx);
                        })),
                )
            })
            .child(self.queue_insert_buttons(format!("search-result-{index}"), song, cx))
            .child(
                Button::new(format!("play-result-{index}"))
                    .ghost()
                    .icon(IconName::Play)
                    .tooltip("Play")
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.play_search_result(index, window, cx);
                    })),
            )
            .into_any_element()
    }

    fn render_online_song_row(
        &self,
        index: usize,
        song: &Song,
        songs: Arc<Vec<Song>>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let duration = song.duration.map(format_duration).unwrap_or_default();
        let favorite = self.is_favorite(&song.video_id);
        let favorite_song = song.clone();
        let playlist_song = song.clone();
        let cloud_liked = self.cloud_video_liked(&song.video_id);
        let cloud_like_song = song.clone();
        let cloud_playlist_song = song.clone();
        let episode_saved = self.is_episode_saved(&song.video_id);
        let later_song = song.clone();
        let saved_position = self.saved_episode_position(&song.video_id);
        h_flex()
            .flex_wrap()
            .w_full()
            .gap_3()
            .items_center()
            .rounded(cx.theme().radius)
            .border_1()
            .border_color(cx.theme().border)
            .p_3()
            .child(
                div()
                    .w(px(24.))
                    .text_center()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child((index + 1).to_string()),
            )
            .child(self.render_thumbnail(
                song.thumbnail_url.as_deref(),
                px(44.),
                IconName::Play,
                cx,
            ))
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .child(div().font_medium().child(song.title.clone()))
                    .child(
                        div()
                            .text_sm()
                            .text_color(cx.theme().muted_foreground)
                            .child(song.artist_line()),
                    )
                    .when_some(saved_position, |details, position| {
                        details.child(
                            div()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child(format!("Resume at {}", format_duration(position))),
                        )
                    }),
            )
            .child(
                div()
                    .w(px(64.))
                    .text_right()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child(duration),
            )
            .child(
                Button::new(format!("favorite-online-{index}"))
                    .ghost()
                    .icon(IconName::Heart)
                    .selected(favorite)
                    .disabled(self.library_busy())
                    .tooltip(if favorite {
                        "Remove from favourites"
                    } else {
                        "Add to favourites"
                    })
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.toggle_favorite(favorite_song.clone(), cx);
                    })),
            )
            .child(
                Button::new(format!("playlist-online-{index}"))
                    .ghost()
                    .icon(IconName::Plus)
                    .tooltip("Add to local playlist")
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.open_playlist_picker(playlist_song.clone(), cx);
                    })),
            )
            .when(song.is_episode, |row| {
                row.child(
                    Button::new(format!("later-online-{index}"))
                        .ghost()
                        .label(if episode_saved { "Saved" } else { "Later" })
                        .selected(episode_saved)
                        .tooltip(if episode_saved {
                            "Remove from Episodes for Later"
                        } else {
                            "Save to Episodes for Later"
                        })
                        .disabled(self.podcast_busy())
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.toggle_episode_for_later(later_song.clone(), cx);
                        })),
                )
            })
            .child(self.download_button(format!("download-online-{index}"), song, cx))
            .when(self.account_ready() && !song.is_episode, |row| {
                row.child(
                    Button::new(format!("cloud-like-online-{index}"))
                        .ghost()
                        .label(if cloud_liked { "YT ♥" } else { "YT ♡" })
                        .tooltip(if cloud_liked {
                            "Remove from YouTube Music liked songs"
                        } else {
                            "Add to YouTube Music liked songs"
                        })
                        .disabled(self.cloud_busy())
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.set_cloud_video_liked(cloud_like_song.clone(), !cloud_liked, cx);
                        })),
                )
                .child(
                    Button::new(format!("cloud-playlist-online-{index}"))
                        .ghost()
                        .label("YT +")
                        .tooltip("Add to an editable YouTube Music playlist")
                        .disabled(self.cloud_busy())
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.open_cloud_playlist_picker(cloud_playlist_song.clone(), cx);
                        })),
                )
            })
            .child(self.queue_insert_buttons(format!("online-song-{index}"), song, cx))
            .child(
                Button::new(format!("play-online-{index}"))
                    .ghost()
                    .icon(IconName::Play)
                    .tooltip("Play")
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.play_song_collection(songs.as_ref().clone(), index, window, cx);
                    })),
            )
            .into_any_element()
    }

    fn render_editable_cloud_song_row(
        &self,
        index: usize,
        entry: &PlaylistEntry,
        songs: Arc<Vec<Song>>,
        playlist_id: String,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let song = &entry.song;
        let duration = song.duration.map(format_duration).unwrap_or_default();
        let favorite = self.is_favorite(&song.video_id);
        let favorite_song = song.clone();
        let playlist_song = song.clone();
        let cloud_liked = self.cloud_video_liked(&song.video_id);
        let cloud_like_song = song.clone();
        let remove_entry = entry.clone();
        h_flex()
            .flex_wrap()
            .w_full()
            .gap_3()
            .items_center()
            .rounded(cx.theme().radius)
            .border_1()
            .border_color(cx.theme().border)
            .p_3()
            .child(
                div()
                    .w(px(24.))
                    .text_center()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child((index + 1).to_string()),
            )
            .child(self.render_thumbnail(
                song.thumbnail_url.as_deref(),
                px(44.),
                IconName::Play,
                cx,
            ))
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .child(div().font_medium().child(song.title.clone()))
                    .child(
                        div()
                            .text_sm()
                            .text_color(cx.theme().muted_foreground)
                            .child(song.artist_line()),
                    ),
            )
            .child(
                div()
                    .w(px(64.))
                    .text_right()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child(duration),
            )
            .child(
                Button::new(format!("favorite-cloud-editable-{index}"))
                    .ghost()
                    .icon(IconName::Heart)
                    .selected(favorite)
                    .disabled(self.library_busy())
                    .tooltip(if favorite {
                        "Remove from local favourites"
                    } else {
                        "Add to local favourites"
                    })
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.toggle_favorite(favorite_song.clone(), cx);
                    })),
            )
            .child(
                Button::new(format!("local-playlist-cloud-editable-{index}"))
                    .ghost()
                    .icon(IconName::Plus)
                    .tooltip("Add to local playlist")
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.open_playlist_picker(playlist_song.clone(), cx);
                    })),
            )
            .child(self.download_button(format!("download-cloud-editable-{index}"), song, cx))
            .child(
                Button::new(format!("cloud-like-editable-{index}"))
                    .ghost()
                    .label(if cloud_liked { "YT ♥" } else { "YT ♡" })
                    .tooltip(if cloud_liked {
                        "Remove from YouTube Music liked songs"
                    } else {
                        "Add to YouTube Music liked songs"
                    })
                    .disabled(self.cloud_busy())
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.set_cloud_video_liked(cloud_like_song.clone(), !cloud_liked, cx);
                    })),
            )
            .child(
                Button::new(format!("remove-cloud-playlist-song-{index}"))
                    .danger()
                    .label(
                        if self.cloud_library_operation
                            == CloudLibraryOperation::RemovingFromPlaylist
                        {
                            "Removing…"
                        } else {
                            "Remove"
                        },
                    )
                    .disabled(self.cloud_busy())
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.remove_song_from_cloud_playlist(
                            playlist_id.clone(),
                            remove_entry.clone(),
                            cx,
                        );
                    })),
            )
            .child(self.queue_insert_buttons(format!("cloud-editable-{index}"), song, cx))
            .child(
                Button::new(format!("play-cloud-editable-{index}"))
                    .ghost()
                    .icon(IconName::Play)
                    .tooltip("Play")
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.play_song_collection(songs.as_ref().clone(), index, window, cx);
                    })),
            )
            .into_any_element()
    }

    fn render_online_browse(&self, cx: &mut Context<Self>) -> AnyElement {
        let Some(state) = &self.browse_state else {
            return div().into_any_element();
        };
        match state {
            BrowseViewState::Loading(item) => v_flex()
                .gap_6()
                .child(
                    Button::new("browse-loading-back")
                        .ghost()
                        .icon(IconName::ArrowLeft)
                        .label("Back")
                        .on_click(cx.listener(|this, _, _, cx| this.close_online_browse(cx))),
                )
                .child(
                    v_flex()
                        .min_h(px(320.))
                        .items_center()
                        .justify_center()
                        .gap_3()
                        .child(Icon::new(IconName::LoaderCircle).size_8())
                        .child(
                            div()
                                .font_semibold()
                                .child(format!("Loading {}", item.title)),
                        )
                        .child(
                            div()
                                .text_sm()
                                .text_color(cx.theme().muted_foreground)
                                .child(format!("Fetching {} details…", item.kind.label())),
                        ),
                )
                .into_any_element(),
            BrowseViewState::Failed(item, message) => {
                let retry_item = item.clone();
                v_flex()
                    .gap_6()
                    .child(
                        Button::new("browse-failed-back")
                            .ghost()
                            .icon(IconName::ArrowLeft)
                            .label("Back")
                            .on_click(cx.listener(|this, _, _, cx| this.close_online_browse(cx))),
                    )
                    .child(
                        v_flex()
                            .min_h(px(320.))
                            .items_center()
                            .justify_center()
                            .gap_3()
                            .child(Icon::new(IconName::TriangleAlert).size_8())
                            .child(div().font_semibold().child("Details unavailable"))
                            .child(
                                div()
                                    .max_w(px(560.))
                                    .text_sm()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(message.clone()),
                            )
                            .child(Button::new("retry-browse").label("Try again").on_click(
                                cx.listener(move |this, _, _, cx| {
                                    this.open_online_browse(retry_item.clone(), cx);
                                }),
                            )),
                    )
                    .into_any_element()
            }
            BrowseViewState::Loaded(page) => {
                let is_podcast = page.item.kind == BrowseKind::Podcast;
                let is_collection =
                    matches!(page.item.kind, BrowseKind::Album | BrowseKind::Playlist);
                let editable_entries = page.item.editable && !page.playlist_entries.is_empty();
                let songs = Arc::new(if editable_entries {
                    page.playlist_entries
                        .iter()
                        .map(|entry| entry.song.clone())
                        .collect()
                } else {
                    page.songs.clone()
                });
                let play_all_songs = songs.clone();
                let shuffle_songs = songs.clone();
                let download_songs = songs.clone();
                let pagination = self.render_browse_pagination(page.continuation.is_some(), cx);
                let tracks =
                    if songs.is_empty()
                        && page.item.kind == BrowseKind::Category
                        && !page.related.is_empty()
                    {
                        div().into_any_element()
                    } else if songs.is_empty() {
                        div()
                            .rounded(cx.theme().radius)
                            .border_1()
                            .border_color(cx.theme().border)
                            .p_5()
                            .text_sm()
                            .text_color(cx.theme().muted_foreground)
                            .child(if is_podcast {
                                "This podcast has no playable episodes yet."
                            } else {
                                "This page has no playable tracks yet."
                            })
                            .into_any_element()
                    } else if editable_entries {
                        let playlist_id = page.item.browse_id.clone();
                        v_flex()
                            .gap_2()
                            .children(page.playlist_entries.iter().enumerate().map(
                                |(index, entry)| {
                                    self.render_editable_cloud_song_row(
                                        index,
                                        entry,
                                        songs.clone(),
                                        playlist_id.clone(),
                                        cx,
                                    )
                                },
                            ))
                            .into_any_element()
                    } else {
                        v_flex()
                            .gap_2()
                            .children(songs.iter().enumerate().map(|(index, song)| {
                                self.render_online_song_row(index, song, songs.clone(), cx)
                            }))
                            .into_any_element()
                    };
                let related = (!page.related.is_empty()).then(|| {
                    v_flex()
                        .gap_3()
                        .child(div().text_lg().font_semibold().child(
                            if page.item.kind == BrowseKind::Category {
                                "Albums"
                            } else {
                                "More to explore"
                            },
                        ))
                        .children(
                            page.related
                                .iter()
                                .enumerate()
                                .map(|(index, item)| self.render_browse_item_row(index, item, cx)),
                        )
                        .into_any_element()
                });
                let playlist_liked = self.cloud_playlist_liked(&page.item.browse_id);
                let like_item = page.item.clone();
                let album_liked = self.cloud_album_liked(&page.item.browse_id);
                let album_like_item = page.item.clone();
                let album_playlist_id = page.playlist_id.clone();
                let rename_item = page.item.clone();
                let delete_item = page.item.clone();
                let subscription = page.channel_subscription.clone();
                let podcast_item = page.item.clone();
                let podcast_saved = self.is_podcast_saved(&page.item.browse_id);
                let podcast_channel_id = subscription
                    .as_ref()
                    .map(|subscription| subscription.channel_id.clone());

                v_flex()
                    .gap_6()
                    .child(
                        Button::new("browse-loaded-back")
                            .ghost()
                            .icon(IconName::ArrowLeft)
                            .label("Back")
                            .on_click(cx.listener(|this, _, _, cx| this.close_online_browse(cx))),
                    )
                    .when_some(self.cloud_library_error.clone(), |layout, message| {
                        layout.child(
                            div()
                                .rounded(cx.theme().radius)
                                .bg(cx.theme().danger.opacity(0.12))
                                .text_color(cx.theme().danger)
                                .p_3()
                                .child(message),
                        )
                    })
                    .when_some(self.podcast_error.clone(), |layout, message| {
                        layout.child(
                            div()
                                .rounded(cx.theme().radius)
                                .bg(cx.theme().danger.opacity(0.12))
                                .text_color(cx.theme().danger)
                                .p_3()
                                .child(message),
                        )
                    })
                    .when_some(self.podcast_notice.clone(), |layout, message| {
                        layout.child(
                            div()
                                .rounded(cx.theme().radius)
                                .bg(cx.theme().success.opacity(0.12))
                                .text_color(cx.theme().success)
                                .p_3()
                                .child(message),
                        )
                    })
                    .child(
                        h_flex()
                            .gap_5()
                            .items_center()
                            .child(self.render_thumbnail(
                                page.item.thumbnail_url.as_deref(),
                                px(96.),
                                if page.item.kind == BrowseKind::Artist {
                                    IconName::User
                                } else {
                                    IconName::BookOpen
                                },
                                cx,
                            ))
                            .child(
                                v_flex()
                                    .flex_1()
                                    .gap_1()
                                    .child(
                                        div()
                                            .text_xs()
                                            .text_color(cx.theme().muted_foreground)
                                            .child(page.item.kind.label()),
                                    )
                                    .child(
                                        div()
                                            .text_2xl()
                                            .font_semibold()
                                            .child(page.item.title.clone()),
                                    )
                                    .child(
                                        div()
                                            .text_sm()
                                            .text_color(cx.theme().muted_foreground)
                                            .child(page.item.subtitle.clone()),
                                    ),
                            )
                            .child(
                                h_flex()
                                    .gap_2()
                                    .flex_wrap()
                                    .child(
                                        Button::new("play-online-page")
                                            .primary()
                                            .icon(IconName::Play)
                                            .label("Play all")
                                            .disabled(songs.is_empty())
                                            .on_click(cx.listener(move |this, _, window, cx| {
                                                this.play_song_collection(
                                                    play_all_songs.as_ref().clone(),
                                                    0,
                                                    window,
                                                    cx,
                                                );
                                            })),
                                    )
                                    .when(is_collection, |actions| {
                                        actions
                                            .child(
                                                Button::new("shuffle-online-page")
                                                    .label("Shuffle")
                                                    .disabled(shuffle_songs.len() < 2)
                                                    .on_click(cx.listener(
                                                        move |this, _, window, cx| {
                                                            this.play_shuffled_collection(
                                                                shuffle_songs.as_ref().clone(),
                                                                window,
                                                                cx,
                                                            );
                                                        },
                                                    )),
                                            )
                                            .child(
                                                Button::new("download-online-page")
                                                    .label("Download all")
                                                    .disabled(download_songs.is_empty())
                                                    .on_click(cx.listener(
                                                        move |this, _, _, cx| {
                                                            for song in
                                                                download_songs.iter().cloned()
                                                            {
                                                                this.queue_song_download(song, cx);
                                                            }
                                                        },
                                                    )),
                                            )
                                    })
                                    .when(is_podcast, |actions| {
                                        actions.child(
                                            Button::new("save-online-podcast")
                                                .label(if podcast_saved {
                                                    "Remove podcast"
                                                } else {
                                                    "Save podcast"
                                                })
                                                .selected(podcast_saved)
                                                .disabled(self.podcast_busy())
                                                .on_click(cx.listener(move |this, _, _, cx| {
                                                    this.toggle_podcast_subscription(
                                                        podcast_item.clone(),
                                                        podcast_channel_id.clone(),
                                                        cx,
                                                    );
                                                })),
                                        )
                                    })
                                    .when(
                                        self.account_ready()
                                            && page.item.kind == BrowseKind::Playlist
                                            && !page.item.editable,
                                        |actions| {
                                            actions.child(
                                                Button::new("cloud-like-online-playlist")
                                                    .label(if playlist_liked {
                                                        "Remove from library"
                                                    } else {
                                                        "Save to library"
                                                    })
                                                    .disabled(self.cloud_busy())
                                                    .on_click(cx.listener(
                                                        move |this, _, _, cx| {
                                                            this.set_cloud_playlist_liked(
                                                                like_item.clone(),
                                                                !playlist_liked,
                                                                cx,
                                                            );
                                                        },
                                                    )),
                                            )
                                        },
                                    )
                                    .when_some(
                                        (self.account_ready()
                                            && page.item.kind == BrowseKind::Album)
                                            .then_some(album_playlist_id)
                                            .flatten(),
                                        |actions, playlist_id| {
                                            actions.child(
                                                Button::new("cloud-like-online-album")
                                                    .label(if album_liked {
                                                        "Remove from library"
                                                    } else {
                                                        "Save to library"
                                                    })
                                                    .selected(album_liked)
                                                    .disabled(self.cloud_busy())
                                                    .on_click(cx.listener(
                                                        move |this, _, _, cx| {
                                                            this.set_cloud_album_liked(
                                                                album_like_item.clone(),
                                                                playlist_id.clone(),
                                                                !album_liked,
                                                                cx,
                                                            );
                                                        },
                                                    )),
                                            )
                                        },
                                    )
                                    .when(self.account_ready() && page.item.editable, |actions| {
                                        actions
                                            .child(
                                                Button::new("rename-cloud-playlist")
                                                    .label("Rename remotely")
                                                    .disabled(self.cloud_busy())
                                                    .on_click(cx.listener(
                                                        move |this, _, window, cx| {
                                                            this.open_rename_cloud_playlist_dialog(
                                                                rename_item.clone(),
                                                                window,
                                                                cx,
                                                            );
                                                        },
                                                    )),
                                            )
                                            .child(
                                                Button::new("delete-cloud-playlist")
                                                    .danger()
                                                    .label("Delete remotely")
                                                    .disabled(self.cloud_busy())
                                                    .on_click(cx.listener(
                                                        move |this, _, window, cx| {
                                                            this.confirm_delete_cloud_playlist(
                                                                delete_item.clone(),
                                                                window,
                                                                cx,
                                                            );
                                                        },
                                                    )),
                                            )
                                    })
                                    .when_some(
                                        (self.account_ready()
                                            && page.item.kind == BrowseKind::Artist)
                                            .then_some(subscription)
                                            .flatten(),
                                        |actions, subscription| {
                                            let subscribed = subscription.subscribed;
                                            let channel_id = subscription.channel_id;
                                            actions.child(
                                                Button::new("cloud-subscribe-artist")
                                                    .label(if subscribed {
                                                        "Unsubscribe"
                                                    } else {
                                                        "Subscribe"
                                                    })
                                                    .selected(subscribed)
                                                    .disabled(self.cloud_busy())
                                                    .on_click(cx.listener(
                                                        move |this, _, _, cx| {
                                                            this.set_cloud_subscription(
                                                                channel_id.clone(),
                                                                !subscribed,
                                                                cx,
                                                            );
                                                        },
                                                    )),
                                            )
                                        },
                                    ),
                            ),
                    )
                    .when_some(page.description.clone(), |layout, description| {
                        layout.child(
                            div()
                                .max_w(px(820.))
                                .text_sm()
                                .text_color(cx.theme().muted_foreground)
                                .child(description),
                        )
                    })
                    .child(
                        v_flex()
                            .gap_3()
                            .child(div().text_lg().font_semibold().child(format!(
                                "{} ({})",
                                if is_podcast { "Episodes" } else { "Tracks" },
                                songs.len()
                            )))
                            .child(tracks),
                    )
                    .when_some(pagination, |layout, pagination| layout.child(pagination))
                    .when_some(related, |layout, related| layout.child(related))
                    .into_any_element()
            }
        }
    }

    fn render_browse_pagination(
        &self,
        has_continuation: bool,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        if !has_continuation && !self.browse_loading_more && self.browse_load_more_error.is_none() {
            return None;
        }
        Some(
            v_flex()
                .items_center()
                .gap_2()
                .when_some(self.browse_load_more_error.clone(), |layout, error| {
                    layout.child(
                        div()
                            .max_w(px(620.))
                            .text_sm()
                            .text_color(cx.theme().danger)
                            .child(error),
                    )
                })
                .when(has_continuation, |layout| {
                    layout.child(
                        Button::new("load-more-online-details")
                            .label(if self.browse_load_more_error.is_some() {
                                "Try loading more again"
                            } else {
                                "Load more tracks"
                            })
                            .loading(self.browse_loading_more)
                            .disabled(self.browse_loading_more)
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.load_more_online_browse(cx);
                            })),
                    )
                })
                .into_any_element(),
        )
    }

    fn local_known_songs(&self) -> Vec<Song> {
        let mut songs = Vec::new();
        let mut seen = HashSet::new();
        let mut add_song = |song: &Song| {
            if seen.insert(song.video_id.clone()) {
                songs.push(song.clone());
            }
        };

        if let StoredViewState::Loaded(favorites) = &self.favorites_state {
            for entry in favorites {
                add_song(&entry.song);
            }
        }
        if let HistoryViewState::Loaded(history) = &self.history_state {
            for entry in history {
                add_song(&entry.song);
            }
        }
        if let StoredViewState::Loaded(downloads) = &self.downloads_state {
            for download in downloads {
                add_song(&download.song);
            }
        }
        if let StoredViewState::Loaded(episodes) = &self.episodes_for_later {
            for episode in episodes {
                add_song(&episode.song);
            }
        }
        songs
    }

    fn local_search_songs(&self) -> Vec<Song> {
        let query = self.model.search_query().trim().to_lowercase();
        if query.is_empty() {
            return Vec::new();
        }
        self.local_known_songs()
            .into_iter()
            .filter(|song| {
                song.title.to_lowercase().contains(&query)
                    || song
                        .artists
                        .iter()
                        .any(|artist| artist.name.to_lowercase().contains(&query))
            })
            .collect()
    }

    fn local_search_catalog_items(&self, kind: BrowseKind) -> Vec<BrowseItem> {
        let query = self.model.search_query().trim().to_lowercase();
        if query.is_empty() {
            return Vec::new();
        }
        let mut items = match &self.local_catalog_state {
            StoredViewState::Loaded(items) => items
                .iter()
                .filter(|item| {
                    item.kind == kind
                        && (item.title.to_lowercase().contains(&query)
                            || item.subtitle.to_lowercase().contains(&query))
                })
                .cloned()
                .collect::<Vec<_>>(),
            StoredViewState::Loading | StoredViewState::Failed(_) => Vec::new(),
        };
        if kind == BrowseKind::Artist {
            let mut seen = items
                .iter()
                .map(|item| item.browse_id.clone())
                .collect::<HashSet<_>>();
            for song in self.local_known_songs() {
                for artist in &song.artists {
                    let Some(id) = artist.id.as_ref().filter(|id| !id.trim().is_empty()) else {
                        continue;
                    };
                    if artist.name.to_lowercase().contains(&query) && seen.insert(id.clone()) {
                        items.push(BrowseItem {
                            browse_id: id.clone(),
                            kind: BrowseKind::Artist,
                            title: artist.name.clone(),
                            subtitle: "Artist · On this device".into(),
                            thumbnail_url: song.thumbnail_url.clone(),
                            params: None,
                            editable: false,
                        });
                    }
                }
            }
        }
        items
    }

    fn local_search_playlists(&self) -> Vec<LocalPlaylist> {
        let query = self.model.search_query().trim().to_lowercase();
        if query.is_empty() {
            return Vec::new();
        }
        match &self.playlists_state {
            StoredViewState::Loaded(playlists) => playlists
                .iter()
                .filter(|playlist| playlist.name.to_lowercase().contains(&query))
                .cloned()
                .collect(),
            StoredViewState::Loading | StoredViewState::Failed(_) => Vec::new(),
        }
    }

    fn render_local_search_section_heading(
        &self,
        label: &'static str,
        count: usize,
        filter: LocalSearchFilter,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        h_flex()
            .items_center()
            .gap_2()
            .child(
                div()
                    .flex_1()
                    .text_lg()
                    .font_semibold()
                    .child(format!("{label} ({count})")),
            )
            .when(self.local_search_filter == LocalSearchFilter::All, |row| {
                row.child(
                    Button::new(format!("local-search-view-all-{}", label.to_lowercase()))
                        .ghost()
                        .label("View all")
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.select_local_search_filter(filter, cx);
                        })),
                )
            })
            .into_any_element()
    }

    fn render_local_search_results(&self, cx: &mut Context<Self>) -> AnyElement {
        if self.model.search_query().trim().is_empty() {
            return v_flex()
                .min_h(px(240.))
                .items_center()
                .justify_center()
                .gap_2()
                .child(Icon::new(IconName::Search).size_8())
                .child(div().font_semibold().child("Search this device"))
                .child(
                    div()
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .child("Type a song, album, artist, or playlist name."),
                )
                .into_any_element();
        }

        let all = self.local_search_filter == LocalSearchFilter::All;
        let songs = self.local_search_songs();
        let albums = self.local_search_catalog_items(BrowseKind::Album);
        let artists = self.local_search_catalog_items(BrowseKind::Artist);
        let playlists = self.local_search_playlists();
        let show_songs = matches!(
            self.local_search_filter,
            LocalSearchFilter::All | LocalSearchFilter::Songs
        );
        let show_albums = matches!(
            self.local_search_filter,
            LocalSearchFilter::All | LocalSearchFilter::Albums
        );
        let show_artists = matches!(
            self.local_search_filter,
            LocalSearchFilter::All | LocalSearchFilter::Artists
        );
        let show_playlists = matches!(
            self.local_search_filter,
            LocalSearchFilter::All | LocalSearchFilter::Playlists
        );
        let songs_loading = matches!(self.history_state, HistoryViewState::Loading)
            || matches!(self.favorites_state, StoredViewState::Loading)
            || matches!(self.downloads_state, StoredViewState::Loading)
            || matches!(self.episodes_for_later, StoredViewState::Loading);
        let loading = ((show_songs || show_artists) && songs_loading)
            || ((show_albums || show_artists)
                && matches!(self.local_catalog_state, StoredViewState::Loading))
            || (show_playlists && matches!(self.playlists_state, StoredViewState::Loading));
        let mut errors = Vec::new();
        if let HistoryViewState::Failed(message) = &self.history_state {
            errors.push(format!("History: {message}"));
        }
        if let StoredViewState::Failed(message) = &self.favorites_state {
            errors.push(format!("Favorites: {message}"));
        }
        if let StoredViewState::Failed(message) = &self.downloads_state {
            errors.push(format!("Downloads: {message}"));
        }
        if let StoredViewState::Failed(message) = &self.episodes_for_later {
            errors.push(format!("Episodes for Later: {message}"));
        }
        if let StoredViewState::Failed(message) = &self.playlists_state {
            errors.push(format!("Playlists: {message}"));
        }
        if let StoredViewState::Failed(message) = &self.local_catalog_state {
            errors.push(format!("Albums and artists: {message}"));
        }
        if let Some(message) = &self.local_catalog_error {
            errors.push(format!("Albums and artists: {message}"));
        }
        let empty = (!show_songs || songs.is_empty())
            && (!show_albums || albums.is_empty())
            && (!show_artists || artists.is_empty())
            && (!show_playlists || playlists.is_empty());
        let shown_songs = Arc::new(if all {
            songs.iter().take(3).cloned().collect::<Vec<_>>()
        } else {
            songs.clone()
        });

        v_flex()
            .gap_4()
            .when(loading, |layout| {
                layout.child(
                    h_flex()
                        .gap_2()
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .child(Icon::new(IconName::LoaderCircle))
                        .child("Loading local library…"),
                )
            })
            .children(errors.into_iter().map(|message| {
                div()
                    .rounded(cx.theme().radius)
                    .bg(cx.theme().danger.opacity(0.12))
                    .text_color(cx.theme().danger)
                    .p_3()
                    .text_sm()
                    .child(message)
                    .into_any_element()
            }))
            .when(show_songs && !songs.is_empty(), |layout| {
                layout.child(
                    v_flex()
                        .gap_2()
                        .child(self.render_local_search_section_heading(
                            "Songs",
                            songs.len(),
                            LocalSearchFilter::Songs,
                            cx,
                        ))
                        .children(shown_songs.iter().enumerate().map(|(index, song)| {
                            self.render_online_song_row(index, song, shown_songs.clone(), cx)
                        })),
                )
            })
            .when(show_albums && !albums.is_empty(), |layout| {
                layout.child(
                    v_flex()
                        .gap_2()
                        .child(self.render_local_search_section_heading(
                            "Albums",
                            albums.len(),
                            LocalSearchFilter::Albums,
                            cx,
                        ))
                        .children(
                            albums
                                .iter()
                                .take(if all { 3 } else { usize::MAX })
                                .enumerate()
                                .map(|(index, item)| {
                                    self.render_browse_item_row(index + 30_000, item, cx)
                                }),
                        ),
                )
            })
            .when(show_artists && !artists.is_empty(), |layout| {
                layout.child(
                    v_flex()
                        .gap_2()
                        .child(self.render_local_search_section_heading(
                            "Artists",
                            artists.len(),
                            LocalSearchFilter::Artists,
                            cx,
                        ))
                        .children(
                            artists
                                .iter()
                                .take(if all { 3 } else { usize::MAX })
                                .enumerate()
                                .map(|(index, item)| {
                                    self.render_browse_item_row(index + 40_000, item, cx)
                                }),
                        ),
                )
            })
            .when(show_playlists && !playlists.is_empty(), |layout| {
                layout.child(
                    v_flex()
                        .gap_2()
                        .child(self.render_local_search_section_heading(
                            "Playlists",
                            playlists.len(),
                            LocalSearchFilter::Playlists,
                            cx,
                        ))
                        .children(playlists.iter().take(if all { 3 } else { usize::MAX }).map(
                            |playlist| {
                                let selected = playlist.clone();
                                h_flex()
                                    .w_full()
                                    .items_center()
                                    .gap_3()
                                    .rounded(cx.theme().radius)
                                    .border_1()
                                    .border_color(cx.theme().border)
                                    .p_3()
                                    .child(Icon::new(IconName::BookOpen))
                                    .child(
                                        v_flex()
                                            .flex_1()
                                            .child(div().font_medium().child(playlist.name.clone()))
                                            .child(
                                                div()
                                                    .text_sm()
                                                    .text_color(cx.theme().muted_foreground)
                                                    .child(format!(
                                                        "{} song{}",
                                                        playlist.song_count,
                                                        if playlist.song_count == 1 {
                                                            ""
                                                        } else {
                                                            "s"
                                                        }
                                                    )),
                                            ),
                                    )
                                    .child(
                                        Button::new(format!(
                                            "open-local-search-playlist-{}",
                                            playlist.id
                                        ))
                                        .ghost()
                                        .label("Open")
                                        .on_click(
                                            cx.listener(move |this, _, _, cx| {
                                                this.open_playlist(selected.clone(), cx);
                                            }),
                                        ),
                                    )
                                    .into_any_element()
                            },
                        )),
                )
            })
            .when(empty && !loading, |layout| {
                layout.child(
                    v_flex()
                        .min_h(px(240.))
                        .items_center()
                        .justify_center()
                        .gap_2()
                        .child(Icon::new(IconName::Search).size_8())
                        .child(div().font_semibold().child("No local results found"))
                        .child(
                            div()
                                .text_sm()
                                .text_color(cx.theme().muted_foreground)
                                .child("Try a different song, album, artist, or playlist name."),
                        ),
                )
            })
            .into_any_element()
    }

    fn render_search(&self, cx: &mut Context<Self>) -> AnyElement {
        if self.search_source == SearchSource::Online && self.browse_state.is_some() {
            return self.render_online_browse(cx);
        }
        let is_loading = self.search_source == SearchSource::Online
            && matches!(self.search_state, SearchViewState::Loading);
        let query_is_empty = self.model.search_query().trim().is_empty();
        let filters = match self.search_source {
            SearchSource::Online => h_flex()
                .gap_2()
                .flex_wrap()
                .children(SearchFilter::ALL.into_iter().map(|filter| {
                    Button::new(format!("search-filter-{}", filter.label().to_lowercase()))
                        .label(filter.label())
                        .selected(self.search_filter == filter)
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.select_search_filter(filter, window, cx);
                        }))
                }))
                .into_any_element(),
            SearchSource::Local => h_flex()
                .gap_2()
                .flex_wrap()
                .children(LocalSearchFilter::ALL.into_iter().map(|filter| {
                    Button::new(format!(
                        "local-search-filter-{}",
                        filter.label().to_lowercase()
                    ))
                    .label(filter.label())
                    .selected(self.local_search_filter == filter)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.select_local_search_filter(filter, cx);
                    }))
                }))
                .into_any_element(),
        };

        v_flex()
            .gap_6()
            .child(self.page_heading(
                "Search",
                if self.search_source == SearchSource::Online {
                    "Find songs, albums, artists, playlists, podcasts, and episodes on YouTube Music."
                } else {
                    "Search songs, albums, artists, and playlists already known to this device."
                },
                cx,
            ))
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        Button::new("search-source-online")
                            .icon(IconName::Search)
                            .label("YouTube Music")
                            .selected(self.search_source == SearchSource::Online)
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.select_search_source(SearchSource::Online, window, cx);
                            })),
                    )
                    .child(
                        Button::new("search-source-local")
                            .icon(IconName::BookOpen)
                            .label("On this device")
                            .selected(self.search_source == SearchSource::Local)
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.select_search_source(SearchSource::Local, window, cx);
                            })),
                    ),
            )
            .child(filters)
            .child(
                h_flex()
                    .gap_3()
                    .max_w(px(720.))
                    .child(
                        div()
                            .flex_1()
                            .child(Input::new(&self.search_input).prefix(IconName::Search)),
                    )
                    .child(
                        Button::new("submit-search")
                            .primary()
                            .label(if self.search_source == SearchSource::Online {
                                "Search online"
                            } else {
                                "Search device"
                            })
                            .loading(is_loading)
                            .disabled(
                                self.search_source == SearchSource::Online && query_is_empty,
                            )
                            .on_click(
                                cx.listener(|this, _, window, cx| this.start_search(window, cx)),
                            ),
                    ),
            )
            .when(self.search_source == SearchSource::Online, |layout| {
                layout
                    .when_some(self.render_search_history(cx), |layout, history| {
                        layout.child(history)
                    })
                    .when_some(
                        self.render_search_suggestions(cx),
                        |layout, suggestions| layout.child(suggestions),
                    )
            })
            .child(if self.search_source == SearchSource::Online {
                self.render_search_results(cx)
            } else {
                self.render_local_search_results(cx)
            })
            .into_any_element()
    }

    fn render_stats_song_row(
        &self,
        index: usize,
        entry: &SongListeningStats,
        songs: Arc<Vec<Song>>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let play_songs = songs;
        let song = entry.song.clone();
        h_flex()
            .flex_wrap()
            .w_full()
            .gap_3()
            .items_center()
            .rounded(cx.theme().radius)
            .border_1()
            .border_color(cx.theme().border)
            .p_3()
            .child(
                div()
                    .w(px(28.))
                    .text_center()
                    .font_semibold()
                    .text_color(cx.theme().muted_foreground)
                    .child((index + 1).to_string()),
            )
            .child(self.render_thumbnail(
                entry.song.thumbnail_url.as_deref(),
                px(48.),
                IconName::Play,
                cx,
            ))
            .child(
                v_flex()
                    .flex_1()
                    .min_w(px(200.))
                    .child(div().font_medium().child(entry.song.title.clone()))
                    .child(
                        div()
                            .text_sm()
                            .text_color(cx.theme().muted_foreground)
                            .child(entry.song.artist_line()),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(format!(
                                "{} play{} · {} listened",
                                entry.play_count,
                                if entry.play_count == 1 { "" } else { "s" },
                                format_duration(entry.play_time)
                            )),
                    ),
            )
            .child(self.queue_insert_buttons(format!("stats-song-{index}"), &entry.song, cx))
            .child(self.download_button(format!("download-stats-song-{index}"), &entry.song, cx))
            .child(
                Button::new(format!("play-stats-song-{index}"))
                    .primary()
                    .icon(IconName::Play)
                    .label("Play")
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.play_song_collection(play_songs.as_ref().clone(), index, window, cx);
                    })),
            )
            .when(entry.song.is_episode, |row| {
                row.child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child("Episode"),
                )
            })
            .when(
                self.current_song
                    .as_ref()
                    .is_some_and(|current| current.video_id == song.video_id),
                |row| row.bg(cx.theme().secondary),
            )
            .into_any_element()
    }

    fn render_stats(&self, cx: &mut Context<Self>) -> AnyElement {
        let content = match &self.stats_state {
            StoredViewState::Loading => v_flex()
                .min_h(px(320.))
                .items_center()
                .justify_center()
                .gap_3()
                .child(Icon::new(IconName::LoaderCircle).size_8())
                .child("Loading listening stats…")
                .into_any_element(),
            StoredViewState::Failed(error) => v_flex()
                .min_h(px(320.))
                .items_center()
                .justify_center()
                .gap_3()
                .child(Icon::new(IconName::TriangleAlert).size_8())
                .child(div().font_semibold().child("Listening stats unavailable"))
                .child(
                    div()
                        .max_w(px(560.))
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .child(error.clone()),
                )
                .child(
                    Button::new("retry-listening-stats")
                        .label("Try again")
                        .on_click(cx.listener(|this, _, _, cx| this.reload_stats(cx))),
                )
                .into_any_element(),
            StoredViewState::Loaded(stats) if stats.top_songs.is_empty() => v_flex()
                .min_h(px(320.))
                .items_center()
                .justify_center()
                .gap_3()
                .child(Icon::new(IconName::ChartPie).size_8())
                .child(
                    div()
                        .font_semibold()
                        .child("No listening stats for this period"),
                )
                .child(
                    div()
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .child("Songs appear here after at least 30 seconds of playback."),
                )
                .into_any_element(),
            StoredViewState::Loaded(stats) => {
                let songs = Arc::new(
                    stats
                        .top_songs
                        .iter()
                        .map(|entry| entry.song.clone())
                        .collect::<Vec<_>>(),
                );
                let play_all = songs.as_ref().clone();
                let shuffle_all = play_all.clone();
                v_flex()
                    .gap_6()
                    .child(
                        h_flex()
                            .flex_wrap()
                            .gap_3()
                            .child(
                                v_flex()
                                    .flex_1()
                                    .min_w(px(150.))
                                    .rounded(cx.theme().radius_lg)
                                    .border_1()
                                    .border_color(cx.theme().border)
                                    .bg(cx.theme().secondary)
                                    .p_4()
                                    .child(
                                        div()
                                            .text_xs()
                                            .text_color(cx.theme().muted_foreground)
                                            .child("Listening time"),
                                    )
                                    .child(
                                        div()
                                            .text_2xl()
                                            .font_semibold()
                                            .child(format_duration(stats.play_time)),
                                    ),
                            )
                            .child(
                                v_flex()
                                    .flex_1()
                                    .min_w(px(150.))
                                    .rounded(cx.theme().radius_lg)
                                    .border_1()
                                    .border_color(cx.theme().border)
                                    .bg(cx.theme().secondary)
                                    .p_4()
                                    .child(
                                        div()
                                            .text_xs()
                                            .text_color(cx.theme().muted_foreground)
                                            .child("Recorded plays"),
                                    )
                                    .child(
                                        div()
                                            .text_2xl()
                                            .font_semibold()
                                            .child(stats.play_count.to_string()),
                                    ),
                            )
                            .child(
                                v_flex()
                                    .flex_1()
                                    .min_w(px(150.))
                                    .rounded(cx.theme().radius_lg)
                                    .border_1()
                                    .border_color(cx.theme().border)
                                    .bg(cx.theme().secondary)
                                    .p_4()
                                    .child(
                                        div()
                                            .text_xs()
                                            .text_color(cx.theme().muted_foreground)
                                            .child("Unique songs / artists / albums"),
                                    )
                                    .child(div().text_2xl().font_semibold().child(format!(
                                        "{} / {} / {}",
                                        stats.unique_songs,
                                        stats.unique_artists,
                                        stats.unique_albums
                                    ))),
                            ),
                    )
                    .child(
                        h_flex()
                            .flex_wrap()
                            .items_center()
                            .justify_between()
                            .gap_3()
                            .child(
                                v_flex()
                                    .child(div().text_xl().font_semibold().child("Top songs"))
                                    .child(
                                        div()
                                            .text_sm()
                                            .text_color(cx.theme().muted_foreground)
                                            .child(format!(
                                                "Ranked by listening time · {} shown",
                                                stats.top_songs.len()
                                            )),
                                    ),
                            )
                            .child(
                                h_flex()
                                    .gap_2()
                                    .child(
                                        Button::new("play-all-stats-songs")
                                            .primary()
                                            .icon(IconName::Play)
                                            .label("Play all")
                                            .disabled(self.listen_together_is_guest())
                                            .on_click(cx.listener(move |this, _, window, cx| {
                                                this.play_song_collection(
                                                    play_all.clone(),
                                                    0,
                                                    window,
                                                    cx,
                                                );
                                            })),
                                    )
                                    .child(
                                        Button::new("shuffle-stats-songs")
                                            .label("Shuffle")
                                            .disabled(self.listen_together_is_guest())
                                            .on_click(cx.listener(move |this, _, window, cx| {
                                                this.play_shuffled_collection(
                                                    shuffle_all.clone(),
                                                    window,
                                                    cx,
                                                );
                                            })),
                                    ),
                            ),
                    )
                    .children(stats.top_songs.iter().enumerate().map(|(index, entry)| {
                        self.render_stats_song_row(index, entry, songs.clone(), cx)
                    }))
                    .when(!stats.top_artists.is_empty(), |layout| {
                        layout
                            .child(
                                v_flex()
                                    .mt_2()
                                    .child(div().text_xl().font_semibold().child("Top artists"))
                                    .child(
                                        div()
                                            .text_sm()
                                            .text_color(cx.theme().muted_foreground)
                                            .child("Aggregated from each song's artist credits."),
                                    ),
                            )
                            .children(stats.top_artists.iter().enumerate().map(
                                |(index, artist)| {
                                    let browse_item = artist.id.as_ref().map(|id| BrowseItem {
                                        browse_id: id.clone(),
                                        kind: BrowseKind::Artist,
                                        title: artist.name.clone(),
                                        subtitle: "Top artist".into(),
                                        thumbnail_url: None,
                                        params: None,
                                        editable: false,
                                    });
                                    h_flex()
                                        .w_full()
                                        .gap_3()
                                        .items_center()
                                        .rounded(cx.theme().radius)
                                        .border_1()
                                        .border_color(cx.theme().border)
                                        .p_3()
                                        .child(
                                            div()
                                                .w(px(28.))
                                                .text_center()
                                                .font_semibold()
                                                .text_color(cx.theme().muted_foreground)
                                                .child((index + 1).to_string()),
                                        )
                                        .child(Icon::new(IconName::User))
                                        .child(
                                            v_flex()
                                                .flex_1()
                                                .child(
                                                    div().font_medium().child(artist.name.clone()),
                                                )
                                                .child(
                                                    div()
                                                        .text_sm()
                                                        .text_color(cx.theme().muted_foreground)
                                                        .child(format!(
                                                            "{} play{} · {} listened",
                                                            artist.play_count,
                                                            if artist.play_count == 1 {
                                                                ""
                                                            } else {
                                                                "s"
                                                            },
                                                            format_duration(artist.play_time)
                                                        )),
                                                ),
                                        )
                                        .when_some(browse_item, |row, item| {
                                            row.child(
                                                Button::new(format!("open-stats-artist-{index}"))
                                                    .label("Open")
                                                    .on_click(cx.listener(
                                                        move |this, _, _, cx| {
                                                            this.open_online_browse(
                                                                item.clone(),
                                                                cx,
                                                            );
                                                        },
                                                    )),
                                            )
                                        })
                                },
                            ))
                    })
                    .when(!stats.top_albums.is_empty(), |layout| {
                        layout
                            .child(
                                v_flex()
                                    .mt_2()
                                    .child(div().text_xl().font_semibold().child("Top albums"))
                                    .child(
                                        div()
                                            .text_sm()
                                            .text_color(cx.theme().muted_foreground)
                                            .child("Aggregated from real album identities in played songs."),
                                    ),
                            )
                            .children(stats.top_albums.iter().enumerate().map(
                                |(index, album)| {
                                    let browse_item = BrowseItem {
                                        browse_id: album.browse_id.clone(),
                                        kind: BrowseKind::Album,
                                        title: album.title.clone(),
                                        subtitle: "Top album".into(),
                                        thumbnail_url: album.thumbnail_url.clone(),
                                        params: None,
                                        editable: false,
                                    };
                                    h_flex()
                                        .w_full()
                                        .gap_3()
                                        .items_center()
                                        .rounded(cx.theme().radius)
                                        .border_1()
                                        .border_color(cx.theme().border)
                                        .p_3()
                                        .child(
                                            div()
                                                .w(px(28.))
                                                .text_center()
                                                .font_semibold()
                                                .text_color(cx.theme().muted_foreground)
                                                .child((index + 1).to_string()),
                                        )
                                        .child(self.render_thumbnail(
                                            album.thumbnail_url.as_deref(),
                                            px(48.),
                                            IconName::BookOpen,
                                            cx,
                                        ))
                                        .child(
                                            v_flex()
                                                .flex_1()
                                                .child(
                                                    div().font_medium().child(album.title.clone()),
                                                )
                                                .child(
                                                    div()
                                                        .text_sm()
                                                        .text_color(cx.theme().muted_foreground)
                                                        .child(format!(
                                                            "{} play{} · {} listened",
                                                            album.play_count,
                                                            if album.play_count == 1 {
                                                                ""
                                                            } else {
                                                                "s"
                                                            },
                                                            format_duration(album.play_time)
                                                        )),
                                                ),
                                        )
                                        .child(
                                            Button::new(format!("open-stats-album-{index}"))
                                                .label("Open")
                                                .on_click(cx.listener(move |this, _, _, cx| {
                                                    this.open_online_browse(
                                                        browse_item.clone(),
                                                        cx,
                                                    );
                                                })),
                                        )
                                },
                            ))
                    })
                    .into_any_element()
            }
        };

        v_flex()
            .gap_6()
            .child(self.page_heading(
                "Listening stats",
                "Your playback history on this device, ranked by real listening time.",
                cx,
            ))
            .child(
                h_flex()
                    .gap_2()
                    .flex_wrap()
                    .children(StatsPeriod::ALL.into_iter().map(|period| {
                        Button::new(format!(
                            "stats-period-{}",
                            period.label().to_lowercase().replace(' ', "-")
                        ))
                        .label(period.label())
                        .selected(self.stats_period == period)
                        .disabled(self.stats_task.is_some())
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.select_stats_period(period, cx);
                        }))
                    })),
            )
            .child(content)
            .into_any_element()
    }

    fn render_favorites_section(&self, cx: &mut Context<Self>) -> AnyElement {
        let content = match &self.favorites_state {
            StoredViewState::Loading => div()
                .text_sm()
                .text_color(cx.theme().muted_foreground)
                .child("Loading favourites…")
                .into_any_element(),
            StoredViewState::Failed(message) => div()
                .text_sm()
                .text_color(cx.theme().danger)
                .child(format!("Favourites unavailable: {message}"))
                .into_any_element(),
            StoredViewState::Loaded(favorites) if favorites.is_empty() => div()
                .text_sm()
                .text_color(cx.theme().muted_foreground)
                .child("Use the heart button in search results to keep songs here.")
                .into_any_element(),
            StoredViewState::Loaded(favorites) => {
                let songs = Arc::new(
                    favorites
                        .iter()
                        .map(|entry| entry.song.clone())
                        .collect::<Vec<_>>(),
                );
                v_flex()
                    .gap_2()
                    .children(favorites.iter().enumerate().map(|(index, entry)| {
                        let play_songs = songs.clone();
                        let favorite_song = entry.song.clone();
                        h_flex()
                            .flex_wrap()
                            .w_full()
                            .gap_3()
                            .items_center()
                            .rounded(cx.theme().radius)
                            .border_1()
                            .border_color(cx.theme().border)
                            .p_3()
                            .child(Icon::new(IconName::Heart))
                            .child(
                                v_flex()
                                    .flex_1()
                                    .min_w_0()
                                    .child(div().font_medium().child(entry.song.title.clone()))
                                    .child(
                                        div()
                                            .text_sm()
                                            .text_color(cx.theme().muted_foreground)
                                            .child(entry.song.artist_line()),
                                    ),
                            )
                            .child(
                                Button::new(format!("unfavorite-library-{index}"))
                                    .ghost()
                                    .icon(IconName::HeartOff)
                                    .disabled(self.library_busy())
                                    .tooltip("Remove from favourites")
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.toggle_favorite(favorite_song.clone(), cx);
                                    })),
                            )
                            .child(self.download_button(
                                format!("download-favorite-{index}"),
                                &entry.song,
                                cx,
                            ))
                            .child(self.queue_insert_buttons(
                                format!("favorite-{index}"),
                                &entry.song,
                                cx,
                            ))
                            .child(
                                Button::new(format!("play-favorite-{index}"))
                                    .ghost()
                                    .icon(IconName::Play)
                                    .tooltip("Play")
                                    .on_click(cx.listener(move |this, _, window, cx| {
                                        this.play_song_collection(
                                            play_songs.as_ref().clone(),
                                            index,
                                            window,
                                            cx,
                                        );
                                    })),
                            )
                            .into_any_element()
                    }))
                    .into_any_element()
            }
        };

        v_flex()
            .gap_3()
            .child(div().text_lg().font_semibold().child("Favourites"))
            .child(content)
            .into_any_element()
    }

    fn render_library_playlist_controls(&self, cx: &mut Context<Self>) -> AnyElement {
        v_flex()
            .gap_3()
            .child(
                div().w_full().child(
                    Input::new(&self.library_playlist_search_input).prefix(IconName::Search),
                ),
            )
            .child(div().font_semibold().child("Auto playlists"))
            .child(
                h_flex()
                    .flex_wrap()
                    .gap_2()
                    .child(
                        Button::new("library-auto-liked")
                            .label("Liked")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.library_tab = LibraryTab::Songs;
                                this.library_song_source = LibrarySongSource::Liked;
                                this.refresh_visible_thumbnails(cx);
                                cx.notify();
                            })),
                    )
                    .child(
                        Button::new("library-auto-offline")
                            .label("Offline")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.library_tab = LibraryTab::Songs;
                                this.library_song_source = LibrarySongSource::Downloaded;
                                this.refresh_visible_thumbnails(cx);
                                cx.notify();
                            })),
                    )
                    .child(
                        Button::new("library-auto-top")
                            .label("My Top")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.navigate(Route::Stats, cx);
                            })),
                    )
                    .child(
                        Button::new("library-auto-uploaded")
                            .label("Uploaded")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.library_tab = LibraryTab::Songs;
                                this.library_song_source = LibrarySongSource::Uploaded;
                                this.refresh_visible_thumbnails(cx);
                                cx.notify();
                            })),
                    ),
            )
            .into_any_element()
    }

    fn render_playlists_section(&self, cx: &mut Context<Self>) -> AnyElement {
        let busy = self.library_busy();
        let sorting = self.library_operation == LibraryOperation::SortingPlaylists;
        let query = if self.library_tab == LibraryTab::Playlists {
            self.library_playlist_query.trim().to_lowercase()
        } else {
            String::new()
        };
        let (direction_icon, direction_label) = match self.playlist_sort_direction {
            SortDirection::Ascending => (IconName::SortAscending, "Ascending"),
            SortDirection::Descending => (IconName::SortDescending, "Descending"),
        };
        let content = match &self.playlists_state {
            StoredViewState::Loading => div()
                .text_sm()
                .text_color(cx.theme().muted_foreground)
                .child("Loading playlists…")
                .into_any_element(),
            StoredViewState::Failed(message) => div()
                .text_sm()
                .text_color(cx.theme().danger)
                .child(format!("Playlists unavailable: {message}"))
                .into_any_element(),
            StoredViewState::Loaded(playlists) => {
                let visible = playlists
                    .iter()
                    .filter(|playlist| {
                        query.is_empty() || playlist.name.to_lowercase().contains(&query)
                    })
                    .collect::<Vec<_>>();
                if visible.is_empty() {
                    div()
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .child(if query.is_empty() {
                            "Create a local playlist, then add songs from search results."
                        } else {
                            "No local playlists match this search."
                        })
                        .into_any_element()
                } else {
                    h_flex()
                        .flex_wrap()
                        .gap_3()
                        .children(visible.into_iter().map(|playlist| {
                            let selected = playlist.clone();
                            Button::new(format!("open-playlist-{}", playlist.id))
                                .w(px(210.))
                                .justify_start()
                                .icon(IconName::BookOpen)
                                .label(format!(
                                    "{} · {} song{}",
                                    playlist.name,
                                    playlist.song_count,
                                    if playlist.song_count == 1 { "" } else { "s" }
                                ))
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.open_playlist(selected.clone(), cx);
                                }))
                                .into_any_element()
                        }))
                        .into_any_element()
                }
            }
        };

        v_flex()
            .gap_3()
            .child(div().text_lg().font_semibold().child("Local playlists"))
            .child(
                h_flex()
                    .max_w(px(560.))
                    .gap_2()
                    .child(
                        div()
                            .flex_1()
                            .child(Input::new(&self.playlist_name_input).disabled(busy)),
                    )
                    .child(
                        Button::new("create-playlist")
                            .primary()
                            .icon(IconName::Plus)
                            .label(
                                if self.library_operation == LibraryOperation::CreatingPlaylist {
                                    "Creating…"
                                } else {
                                    "Create"
                                },
                            )
                            .disabled(busy)
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.create_playlist(window, cx);
                            })),
                    ),
            )
            .child(
                h_flex()
                    .flex_wrap()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child("Sort by"),
                    )
                    .child(
                        Button::new("playlist-sort-created")
                            .label("Created")
                            .selected(self.playlist_sort == PlaylistSort::CreatedAt)
                            .disabled(busy)
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.set_playlist_sort(PlaylistSort::CreatedAt, cx);
                            })),
                    )
                    .child(
                        Button::new("playlist-sort-name")
                            .label("Name")
                            .selected(self.playlist_sort == PlaylistSort::Name)
                            .disabled(busy)
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.set_playlist_sort(PlaylistSort::Name, cx);
                            })),
                    )
                    .child(
                        Button::new("playlist-sort-song-count")
                            .label("Songs")
                            .selected(self.playlist_sort == PlaylistSort::SongCount)
                            .disabled(busy)
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.set_playlist_sort(PlaylistSort::SongCount, cx);
                            })),
                    )
                    .child(
                        Button::new("playlist-sort-updated")
                            .label("Updated")
                            .selected(self.playlist_sort == PlaylistSort::UpdatedAt)
                            .disabled(busy)
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.set_playlist_sort(PlaylistSort::UpdatedAt, cx);
                            })),
                    )
                    .child(
                        Button::new("playlist-sort-direction")
                            .icon(direction_icon)
                            .label(if sorting {
                                "Sorting…"
                            } else {
                                direction_label
                            })
                            .disabled(busy)
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.toggle_playlist_sort_direction(cx);
                            })),
                    ),
            )
            .child(content)
            .into_any_element()
    }

    fn history_song_matches(&self, song: &Song) -> bool {
        if self.model.route() != Route::History {
            return true;
        }
        let query = self.history_query.trim().to_lowercase();
        query.is_empty()
            || song.title.to_lowercase().contains(&query)
            || song
                .artists
                .iter()
                .any(|artist| artist.name.to_lowercase().contains(&query))
    }

    fn render_local_history_content(&self, cx: &mut Context<Self>) -> AnyElement {
        match &self.history_state {
            HistoryViewState::Loading => div()
                .text_sm()
                .text_color(cx.theme().muted_foreground)
                .child("Loading local listening history…")
                .into_any_element(),
            HistoryViewState::Failed(message) => v_flex()
                .gap_2()
                .child(
                    div()
                        .text_sm()
                        .text_color(cx.theme().danger)
                        .child(format!("Local history unavailable: {message}")),
                )
                .child(
                    Button::new("retry-local-history")
                        .label("Try again")
                        .disabled(self.library_busy())
                        .on_click(
                            cx.listener(|this, _, _, cx| this.retry_failed_local_library(cx)),
                        ),
                )
                .into_any_element(),
            HistoryViewState::Loaded(history) if history.is_empty() => div()
                .text_sm()
                .text_color(cx.theme().muted_foreground)
                .child("A local entry appears after 30 seconds of actual playback.")
                .into_any_element(),
            HistoryViewState::Loaded(history) => {
                let filtered = history
                    .iter()
                    .filter(|entry| self.history_song_matches(&entry.song))
                    .collect::<Vec<_>>();
                if filtered.is_empty() {
                    return div()
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .child("No local history matches this title or artist.")
                        .into_any_element();
                }
                let songs = Arc::new(
                    filtered
                        .iter()
                        .map(|entry| entry.song.clone())
                        .collect::<Vec<_>>(),
                );
                v_flex()
                    .gap_2()
                    .children(filtered.into_iter().enumerate().map(|(index, entry)| {
                        let play_songs = songs.clone();
                        let entry_id = entry.id;
                        h_flex()
                            .flex_wrap()
                            .w_full()
                            .gap_4()
                            .items_center()
                            .rounded(cx.theme().radius)
                            .border_1()
                            .border_color(cx.theme().border)
                            .p_3()
                            .child(
                                v_flex()
                                    .flex_1()
                                    .min_w_0()
                                    .child(div().font_medium().child(entry.song.title.clone()))
                                    .child(
                                        div()
                                            .text_sm()
                                            .text_color(cx.theme().muted_foreground)
                                            .child(entry.song.artist_line()),
                                    ),
                            )
                            .child(
                                v_flex()
                                    .items_end()
                                    .child(div().text_sm().child(format_duration(entry.play_time)))
                                    .child(
                                        div()
                                            .text_xs()
                                            .text_color(cx.theme().muted_foreground)
                                            .child(format_history_age(entry.played_at_ms)),
                                    ),
                            )
                            .child(self.download_button(
                                format!("download-history-{index}"),
                                &entry.song,
                                cx,
                            ))
                            .child(self.queue_insert_buttons(
                                format!("local-history-{index}"),
                                &entry.song,
                                cx,
                            ))
                            .child(
                                Button::new(format!("remove-local-history-{index}"))
                                    .ghost()
                                    .label("Remove")
                                    .disabled(self.library_busy())
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.remove_local_history_entry(entry_id, cx);
                                    })),
                            )
                            .child(
                                Button::new(format!("play-history-{index}"))
                                    .ghost()
                                    .icon(IconName::Play)
                                    .tooltip("Play from history")
                                    .on_click(cx.listener(move |this, _, window, cx| {
                                        this.play_song_collection(
                                            play_songs.as_ref().clone(),
                                            index,
                                            window,
                                            cx,
                                        );
                                    })),
                            )
                            .into_any_element()
                    }))
                    .into_any_element()
            }
        }
    }

    fn render_remote_history_content(&self, cx: &mut Context<Self>) -> AnyElement {
        let removing = self.remote_history_operation == RemoteHistoryOperation::Removing;
        match &self.remote_history_state {
            RemoteHistoryViewState::SignedOut => div()
                .text_sm()
                .text_color(cx.theme().muted_foreground)
                .child("Import a YouTube Music session in Settings to view remote history.")
                .into_any_element(),
            RemoteHistoryViewState::Loading => div()
                .text_sm()
                .text_color(cx.theme().muted_foreground)
                .child("Loading YouTube Music history…")
                .into_any_element(),
            RemoteHistoryViewState::Failed(message) => div()
                .text_sm()
                .text_color(cx.theme().danger)
                .child(format!("YouTube Music history unavailable: {message}"))
                .into_any_element(),
            RemoteHistoryViewState::Loaded(page) if page.entry_count() == 0 => div()
                .text_sm()
                .text_color(cx.theme().muted_foreground)
                .child("YouTube Music returned no listening history.")
                .into_any_element(),
            RemoteHistoryViewState::Loaded(page) => {
                let sections = page
                    .sections
                    .iter()
                    .filter_map(|section| {
                        let entries = section
                            .entries
                            .iter()
                            .filter(|entry| self.history_song_matches(&entry.song))
                            .cloned()
                            .collect::<Vec<_>>();
                        (!entries.is_empty()).then(|| (section.title.clone(), entries))
                    })
                    .collect::<Vec<_>>();
                if sections.is_empty() {
                    return div()
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .child("No YouTube Music history matches this title or artist.")
                        .into_any_element();
                }
                let songs = Arc::new(
                    sections
                        .iter()
                        .flat_map(|(_, entries)| entries.iter())
                        .map(|entry| entry.song.clone())
                        .collect::<Vec<_>>(),
                );
                v_flex()
                    .gap_4()
                    .children(sections.iter().enumerate().map(
                        |(section_index, (section_title, entries))| {
                            let section_start = sections
                                .iter()
                                .take(section_index)
                                .map(|(_, entries)| entries.len())
                                .sum::<usize>();
                            let play_songs = songs.clone();
                            v_flex()
                                .gap_2()
                                .child(
                                    div()
                                        .text_sm()
                                        .font_semibold()
                                        .child(section_title.clone()),
                                )
                                .children(entries.iter().enumerate().map(
                                    |(entry_index, entry)| {
                                        let play_songs = play_songs.clone();
                                        let play_index = section_start + entry_index;
                                        let duration = entry
                                            .song
                                            .duration
                                            .map(format_duration)
                                            .unwrap_or_else(|| "—".into());
                                        h_flex()
                                            .flex_wrap()
                                            .w_full()
                                            .gap_3()
                                            .items_center()
                                            .rounded(cx.theme().radius)
                                            .border_1()
                                            .border_color(cx.theme().border)
                                            .p_3()
                                            .child(
                                                v_flex()
                                                    .flex_1()
                                                    .min_w_0()
                                                    .child(
                                                        div()
                                                            .font_medium()
                                                            .child(entry.song.title.clone()),
                                                    )
                                                    .child(
                                                        div()
                                                            .text_sm()
                                                            .text_color(
                                                                cx.theme().muted_foreground,
                                                            )
                                                            .child(entry.song.artist_line()),
                                                    ),
                                            )
                                            .child(div().text_sm().child(duration))
                                            .child(self.download_button(
                                                format!(
                                                    "download-remote-history-{section_index}-{entry_index}"
                                                ),
                                                &entry.song,
                                                cx,
                                            ))
                                            .when_some(
                                                entry.feedback_token.clone(),
                                                |row, feedback_token| {
                                                    row.child(
                                                        Button::new(format!(
                                                            "remove-remote-history-{section_index}-{entry_index}"
                                                        ))
                                                        .ghost()
                                                        .label(if removing {
                                                            "Removing…"
                                                        } else {
                                                            "Remove remote"
                                                        })
                                                        .tooltip(
                                                            "Remove only this YouTube Music history entry",
                                                        )
                                                        .disabled(removing)
                                                        .on_click(cx.listener(
                                                            move |this, _, _, cx| {
                                                                this.remove_remote_history_item(
                                                                    feedback_token.clone(),
                                                                    cx,
                                                                );
                                                            },
                                                        )),
                                                    )
                                                },
                                            )
                                            .child(self.queue_insert_buttons(
                                                format!(
                                                    "remote-history-{section_index}-{entry_index}"
                                                ),
                                                &entry.song,
                                                cx,
                                            ))
                                            .child(
                                                Button::new(format!(
                                                    "play-remote-history-{section_index}-{entry_index}"
                                                ))
                                                .ghost()
                                                .icon(IconName::Play)
                                                .tooltip("Play from YouTube Music history")
                                                .on_click(cx.listener(
                                                    move |this, _, window, cx| {
                                                        this.play_song_collection(
                                                            play_songs.as_ref().clone(),
                                                            play_index,
                                                            window,
                                                            cx,
                                                        );
                                                    },
                                                )),
                                            )
                                            .into_any_element()
                                    },
                                ))
                                .into_any_element()
                        },
                    ))
                    .into_any_element()
            }
        }
    }

    fn render_history_section(&self, cx: &mut Context<Self>) -> AnyElement {
        let local_entry_count = match &self.history_state {
            HistoryViewState::Loaded(history) => history.len(),
            _ => 0,
        };
        let clearing = self.library_operation == LibraryOperation::ClearingHistory;
        let remote_selected = self.remote_history_source == HistorySource::YouTubeMusic;
        let signed_in = self.account_ready();
        let content = if remote_selected {
            self.render_remote_history_content(cx)
        } else {
            self.render_local_history_content(cx)
        };

        v_flex()
            .gap_3()
            .child(
                h_flex()
                    .w_full()
                    .flex_wrap()
                    .items_center()
                    .justify_between()
                    .gap_3()
                    .child(div().text_lg().font_semibold().child("Listening history"))
                    .child(
                        h_flex()
                            .gap_2()
                            .flex_wrap()
                            .child(
                                Button::new("history-source-local")
                                    .label("Local")
                                    .selected(!remote_selected)
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.remote_history_source = HistorySource::Local;
                                        cx.notify();
                                    })),
                            )
                            .child(
                                Button::new("history-source-youtube")
                                    .label("YouTube Music")
                                    .selected(remote_selected)
                                    .disabled(!signed_in)
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.remote_history_source =
                                            HistorySource::YouTubeMusic;
                                        if matches!(
                                            this.remote_history_state,
                                            RemoteHistoryViewState::SignedOut
                                        ) {
                                            this.reload_remote_history(cx);
                                        }
                                        cx.notify();
                                    })),
                            )
                            .when(!remote_selected, |buttons| {
                                buttons.child(
                                    Button::new("clear-listening-history")
                                        .danger()
                                        .label(if clearing {
                                            "Clearing…"
                                        } else {
                                            "Clear local"
                                        })
                                        .disabled(
                                            self.library_busy() || local_entry_count == 0,
                                        )
                                        .on_click(cx.listener(
                                            move |this, _, window, cx| {
                                                this.confirm_clear_history(
                                                    local_entry_count,
                                                    window,
                                                    cx,
                                                );
                                            },
                                        )),
                                )
                            })
                            .when(remote_selected, |buttons| {
                                buttons.child(
                                    Button::new("refresh-remote-history")
                                        .label("Refresh remote")
                                        .disabled(
                                            self.cloud_busy()
                                                || matches!(
                                                    self.remote_history_state,
                                                    RemoteHistoryViewState::Loading
                                                ),
                                        )
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            this.reload_remote_history(cx);
                                        })),
                                )
                            }),
                    ),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child("Local history stays in this device's SQLite database. YouTube Music history is fetched on demand and is never copied into it."),
            )
            .when_some(
                remote_selected
                    .then(|| self.remote_history_error.clone())
                    .flatten(),
                |section, error| {
                    section.child(
                        div()
                            .rounded(cx.theme().radius)
                            .bg(cx.theme().danger.opacity(0.12))
                            .text_color(cx.theme().danger)
                            .p_3()
                            .text_sm()
                            .child(error),
                    )
                },
            )
            .child(content)
            .into_any_element()
    }

    fn render_history(&self, cx: &mut Context<Self>) -> AnyElement {
        v_flex()
            .gap_6()
            .child(self.page_heading(
                "History",
                "Search, replay, queue, download, or remove songs from local and YouTube Music listening history.",
                cx,
            ))
            .child(
                h_flex()
                    .max_w(px(720.))
                    .gap_2()
                    .child(
                        div()
                            .flex_1()
                            .child(Input::new(&self.history_search_input).prefix(IconName::Search)),
                    )
                    .when(!self.history_query.is_empty(), |row| {
                        row.child(
                            Button::new("clear-history-filter")
                                .label("Clear filter")
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.history_search_input.update(cx, |input, cx| {
                                        input.set_value("", window, cx);
                                    });
                                })),
                        )
                    }),
            )
            .when_some(self.library_error.clone(), |page, error| {
                page.child(
                    div()
                        .rounded(cx.theme().radius)
                        .bg(cx.theme().danger.opacity(0.12))
                        .text_color(cx.theme().danger)
                        .p_3()
                        .text_sm()
                        .child(error),
                )
            })
            .child(self.render_history_section(cx))
            .into_any_element()
    }

    fn render_playlist_detail(&self, cx: &mut Context<Self>) -> AnyElement {
        let Some(detail) = &self.playlist_detail else {
            return div().into_any_element();
        };
        let playlist = match detail {
            PlaylistDetailState::Loading(playlist)
            | PlaylistDetailState::Loaded(playlist, _)
            | PlaylistDetailState::Failed(playlist, _) => playlist,
        };
        let header = h_flex()
            .items_center()
            .gap_3()
            .child(
                Button::new("close-playlist-detail")
                    .ghost()
                    .icon(IconName::ArrowLeft)
                    .label("Library")
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.playlist_detail = None;
                        cx.notify();
                    })),
            )
            .child(
                v_flex()
                    .flex_1()
                    .child(
                        div()
                            .text_2xl()
                            .font_semibold()
                            .child(playlist.name.clone()),
                    )
                    .child(
                        div()
                            .text_sm()
                            .text_color(cx.theme().muted_foreground)
                            .child(format!("{} songs · stored locally", playlist.song_count)),
                    ),
            );

        let content = match detail {
            PlaylistDetailState::Loading(_) => v_flex()
                .min_h(px(240.))
                .items_center()
                .justify_center()
                .gap_3()
                .child(Icon::new(IconName::LoaderCircle).size_8())
                .child("Loading playlist…")
                .into_any_element(),
            PlaylistDetailState::Failed(_, message) => v_flex()
                .min_h(px(240.))
                .items_center()
                .justify_center()
                .gap_2()
                .child(Icon::new(IconName::TriangleAlert).size_8())
                .child(message.clone())
                .into_any_element(),
            PlaylistDetailState::Loaded(_, songs) if songs.is_empty() => v_flex()
                .min_h(px(240.))
                .items_center()
                .justify_center()
                .gap_2()
                .child(Icon::new(IconName::BookOpen).size_8())
                .child("This playlist is empty.")
                .child(
                    div()
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .child("Add songs from search results."),
                )
                .into_any_element(),
            PlaylistDetailState::Loaded(playlist, songs) => {
                let song_collection = Arc::new(songs.clone());
                v_flex()
                    .gap_2()
                    .children(songs.iter().enumerate().map(|(index, song)| {
                        let play_songs = song_collection.clone();
                        let remove_song = song.clone();
                        let remove_playlist = playlist.clone();
                        h_flex()
                            .flex_wrap()
                            .w_full()
                            .gap_3()
                            .items_center()
                            .rounded(cx.theme().radius)
                            .border_1()
                            .border_color(cx.theme().border)
                            .p_3()
                            .child(
                                div()
                                    .w(px(24.))
                                    .text_sm()
                                    .text_color(cx.theme().muted_foreground)
                                    .child((index + 1).to_string()),
                            )
                            .child(
                                v_flex()
                                    .flex_1()
                                    .min_w_0()
                                    .child(div().font_medium().child(song.title.clone()))
                                    .child(
                                        div()
                                            .text_sm()
                                            .text_color(cx.theme().muted_foreground)
                                            .child(song.artist_line()),
                                    ),
                            )
                            .child(
                                Button::new(format!("remove-playlist-song-{index}"))
                                    .ghost()
                                    .label("Remove")
                                    .disabled(self.library_busy())
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.remove_from_playlist(
                                            remove_playlist.clone(),
                                            remove_song.clone(),
                                            cx,
                                        );
                                    })),
                            )
                            .child(self.download_button(
                                format!("download-playlist-song-{index}"),
                                song,
                                cx,
                            ))
                            .child(self.queue_insert_buttons(
                                format!("local-playlist-{index}"),
                                song,
                                cx,
                            ))
                            .child(
                                Button::new(format!("play-playlist-song-{index}"))
                                    .ghost()
                                    .icon(IconName::Play)
                                    .tooltip("Play")
                                    .on_click(cx.listener(move |this, _, window, cx| {
                                        this.play_song_collection(
                                            play_songs.as_ref().clone(),
                                            index,
                                            window,
                                            cx,
                                        );
                                    })),
                            )
                            .into_any_element()
                    }))
                    .into_any_element()
            }
        };
        let rename_playlist = playlist.clone();
        let delete_playlist = playlist.clone();
        let play_all = match detail {
            PlaylistDetailState::Loaded(_, songs) if !songs.is_empty() => {
                let songs = songs.clone();
                Some(
                    Button::new("play-playlist")
                        .primary()
                        .icon(IconName::Play)
                        .label("Play")
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.play_song_collection(songs.clone(), 0, window, cx);
                        })),
                )
            }
            _ => None,
        };

        v_flex()
            .gap_6()
            .child(header)
            .when_some(self.library_error.clone(), |layout, message| {
                layout.child(
                    div()
                        .rounded(cx.theme().radius)
                        .bg(cx.theme().danger.opacity(0.12))
                        .text_color(cx.theme().danger)
                        .p_3()
                        .child(message),
                )
            })
            .child(
                h_flex()
                    .gap_2()
                    .when_some(play_all, |row, button| row.child(button))
                    .child(
                        Button::new("rename-playlist")
                            .label(
                                if self.library_operation == LibraryOperation::RenamingPlaylist {
                                    "Renaming…"
                                } else {
                                    "Rename"
                                },
                            )
                            .disabled(self.library_busy())
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.open_rename_playlist_dialog(
                                    rename_playlist.clone(),
                                    window,
                                    cx,
                                );
                            })),
                    )
                    .child(
                        Button::new("delete-playlist")
                            .danger()
                            .label(
                                if self.library_operation == LibraryOperation::DeletingPlaylist {
                                    "Deleting…"
                                } else {
                                    "Delete playlist"
                                },
                            )
                            .disabled(self.library_busy())
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.confirm_delete_playlist(delete_playlist.clone(), window, cx);
                            })),
                    ),
            )
            .child(content)
            .into_any_element()
    }

    fn render_cloud_library_section(
        &self,
        selected_tab: LibraryTab,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let busy = self.cloud_busy();
        let show_songs = matches!(selected_tab, LibraryTab::Overview | LibraryTab::Songs);
        let show_playlists = matches!(selected_tab, LibraryTab::Overview | LibraryTab::Playlists);
        let show_albums = matches!(selected_tab, LibraryTab::Overview | LibraryTab::Albums);
        let show_artists = matches!(selected_tab, LibraryTab::Overview | LibraryTab::Artists);
        let operation_label = match self.cloud_library_operation {
            CloudLibraryOperation::Idle => None,
            CloudLibraryOperation::SettingVideoLike => Some("Updating liked songs…"),
            CloudLibraryOperation::SettingPlaylistLike => Some("Updating saved playlists…"),
            CloudLibraryOperation::SettingAlbumLike => Some("Updating saved albums…"),
            CloudLibraryOperation::SettingSubscription => Some("Updating subscription…"),
            CloudLibraryOperation::CreatingPlaylist => Some("Creating online playlist…"),
            CloudLibraryOperation::AddingToPlaylist => Some("Adding song online…"),
            CloudLibraryOperation::RemovingFromPlaylist => Some("Removing song online…"),
            CloudLibraryOperation::RenamingPlaylist => Some("Renaming online playlist…"),
            CloudLibraryOperation::DeletingPlaylist => Some("Deleting online playlist…"),
        };
        let content = match &self.cloud_library_state {
            CloudLibraryViewState::SignedOut => v_flex()
                .gap_2()
                .child(
                    div()
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .child("Sign in to YouTube Music in Settings to view your cloud library."),
                )
                .child(
                    Button::new("cloud-library-open-settings")
                        .label("Open account settings")
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.navigate(Route::Settings, cx);
                        })),
                )
                .into_any_element(),
            CloudLibraryViewState::Loading => h_flex()
                .gap_2()
                .text_sm()
                .text_color(cx.theme().muted_foreground)
                .child(Icon::new(IconName::LoaderCircle))
                .child("Loading songs, playlists, albums, and artists from YouTube Music…")
                .into_any_element(),
            CloudLibraryViewState::Failed(error) => v_flex()
                .gap_2()
                .child(
                    div()
                        .text_sm()
                        .text_color(cx.theme().danger)
                        .child(format!("Cloud library unavailable: {error}")),
                )
                .when(self.account_ready(), |content| {
                    content.child(
                        Button::new("cloud-library-retry")
                            .label("Try again")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.reload_cloud_library(cx);
                            })),
                    )
                })
                .into_any_element(),
            CloudLibraryViewState::Loaded(library) => {
                let songs = Arc::new(library.liked_songs.clone());
                let play_all = songs.clone();
                let playlist_query = if selected_tab == LibraryTab::Playlists {
                    self.library_playlist_query.trim().to_lowercase()
                } else {
                    String::new()
                };
                let visible_playlists = library
                    .playlists
                    .iter()
                    .filter(|playlist| {
                        playlist_query.is_empty()
                            || playlist.title.to_lowercase().contains(&playlist_query)
                            || playlist.subtitle.to_lowercase().contains(&playlist_query)
                    })
                    .cloned()
                    .collect::<Vec<_>>();
                v_flex()
                    .gap_5()
                    .child(
                        h_flex()
                            .items_center()
                            .justify_between()
                            .child(
                                v_flex()
                                    .gap_1()
                                    .child(
                                        div().text_sm().text_color(cx.theme().muted_foreground).child(
                                            format!(
                                                "{} liked song{} · {} library song{} · {} uploaded song{} · {} playlist{} · {} liked album{} · {} uploaded album{} · {} artist{} · synchronized snapshot",
                                                songs.len(),
                                                if songs.len() == 1 { "" } else { "s" },
                                                library.library_songs.len(),
                                                if library.library_songs.len() == 1 { "" } else { "s" },
                                                library.uploaded_songs.len(),
                                                if library.uploaded_songs.len() == 1 { "" } else { "s" },
                                                library.playlists.len(),
                                                if library.playlists.len() == 1 { "" } else { "s" },
                                                library.albums.len(),
                                                if library.albums.len() == 1 { "" } else { "s" },
                                                library.uploaded_albums.len(),
                                                if library.uploaded_albums.len() == 1 { "" } else { "s" },
                                                library.artists.len(),
                                                if library.artists.len() == 1 { "" } else { "s" }
                                            ),
                                        ),
                                    )
                                    .when_some(operation_label, |status, label| {
                                        status.child(
                                            div()
                                                .text_xs()
                                                .text_color(cx.theme().muted_foreground)
                                                .child(label),
                                        )
                                    }),
                            )
                            .child(
                                h_flex()
                                    .gap_2()
                                    .when(show_playlists, |actions| {
                                        actions.child(
                                            Button::new("create-cloud-playlist")
                                                .primary()
                                                .icon(IconName::Plus)
                                                .label(if self.cloud_library_operation
                                                    == CloudLibraryOperation::CreatingPlaylist
                                                {
                                                    "Creating…"
                                                } else {
                                                    "New online playlist"
                                                })
                                                .disabled(busy)
                                                .on_click(cx.listener(|this, _, window, cx| {
                                                    this.open_create_cloud_playlist_dialog(
                                                        window, cx,
                                                    );
                                                })),
                                        )
                                    })
                                    .child(
                                        Button::new("cloud-library-refresh")
                                            .ghost()
                                            .label("Refresh")
                                            .disabled(busy)
                                            .on_click(cx.listener(|this, _, _, cx| {
                                                this.reload_cloud_library(cx);
                                            })),
                                    ),
                            ),
                    )
                    .when(show_songs && !songs.is_empty(), |layout| {
                        layout.child(
                            v_flex()
                                .gap_3()
                                .child(
                                    h_flex()
                                        .items_center()
                                        .justify_between()
                                        .child(div().font_semibold().child("Liked songs"))
                                        .child(
                                            Button::new("play-cloud-liked-songs")
                                                .primary()
                                                .icon(IconName::Play)
                                                .label("Play all")
                                                .on_click(cx.listener(move |this, _, window, cx| {
                                                    this.play_song_collection(
                                                        play_all.as_ref().clone(),
                                                        0,
                                                        window,
                                                        cx,
                                                    );
                                                })),
                                        ),
                                )
                                .children(songs.iter().enumerate().map(|(index, song)| {
                                    self.render_online_song_row(index, song, songs.clone(), cx)
                                })),
                        )
                    })
                    .when(show_songs && songs.is_empty(), |layout| {
                        layout.child(
                            div()
                                .text_sm()
                                .text_color(cx.theme().muted_foreground)
                                .child("No liked songs were returned."),
                        )
                    })
                    .when(show_playlists && !visible_playlists.is_empty(), |layout| {
                        layout.child(
                            v_flex()
                                .gap_3()
                                .child(div().font_semibold().child("Liked playlists"))
                                .children(
                                    visible_playlists
                                        .iter()
                                        .enumerate()
                                        .map(|(index, item)| {
                                            self.render_cloud_playlist_row(index, item, cx)
                                        }),
                                ),
                        )
                    })
                    .when(show_playlists && visible_playlists.is_empty(), |layout| {
                        layout.child(
                            div()
                                .text_sm()
                                .text_color(cx.theme().muted_foreground)
                                .child(if playlist_query.is_empty() {
                                    "No liked playlists were returned."
                                } else {
                                    "No online playlists match this search."
                                }),
                        )
                    })
                    .when(show_albums, |layout| {
                        layout.child(
                            v_flex()
                                .gap_3()
                                .child(div().font_semibold().child("Liked albums"))
                                .when(library.albums.is_empty(), |section| {
                                    section.child(
                                        div()
                                            .text_sm()
                                            .text_color(cx.theme().muted_foreground)
                                            .child("No liked albums were returned."),
                                    )
                                })
                                .when(!library.albums.is_empty(), |section| {
                                    section.children(library.albums.iter().enumerate().map(
                                        |(index, item)| {
                                            self.render_browse_item_row(index + 10_000, item, cx)
                                        },
                                    ))
                                }),
                        )
                    })
                    .when(show_artists, |layout| {
                        layout.child(
                            v_flex()
                                .gap_3()
                                .child(div().font_semibold().child("Library artists"))
                                .when(library.artists.is_empty(), |section| {
                                    section.child(
                                        div()
                                            .text_sm()
                                            .text_color(cx.theme().muted_foreground)
                                            .child("No library artists were returned."),
                                    )
                                })
                                .when(!library.artists.is_empty(), |section| {
                                    section.children(library.artists.iter().enumerate().map(
                                        |(index, item)| {
                                            self.render_browse_item_row(index + 20_000, item, cx)
                                        },
                                    ))
                                }),
                        )
                    })
                    .into_any_element()
            }
        };

        v_flex()
            .gap_3()
            .child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .child(Icon::new(IconName::BookOpen))
                    .child(
                        div()
                            .text_lg()
                            .font_semibold()
                            .child("YouTube Music library"),
                    ),
            )
            .when_some(self.cloud_library_error.clone(), |layout, message| {
                layout.child(
                    div()
                        .rounded(cx.theme().radius)
                        .bg(cx.theme().danger.opacity(0.12))
                        .text_color(cx.theme().danger)
                        .p_3()
                        .text_sm()
                        .child(message),
                )
            })
            .child(content)
            .into_any_element()
    }

    fn render_downloads_section(&self, cx: &mut Context<Self>) -> AnyElement {
        let content = match &self.downloads_state {
            StoredViewState::Loading => h_flex()
                .gap_2()
                .text_sm()
                .text_color(cx.theme().muted_foreground)
                .child(Icon::new(IconName::LoaderCircle))
                .child("Loading offline downloads…")
                .into_any_element(),
            StoredViewState::Failed(error) => div()
                .text_sm()
                .text_color(cx.theme().danger)
                .child(format!("Offline downloads unavailable: {error}"))
                .into_any_element(),
            StoredViewState::Loaded(downloads) if downloads.is_empty() => div()
                .text_sm()
                .text_color(cx.theme().muted_foreground)
                .child("Choose Download on an online song to keep it for offline playback.")
                .into_any_element(),
            StoredViewState::Loaded(downloads) => {
                let completed_songs = downloads
                    .iter()
                    .filter(|download| download.is_complete())
                    .map(|download| download.song.clone())
                    .collect::<Vec<_>>();
                let play_all = completed_songs.clone();
                v_flex()
                    .gap_3()
                    .when(!completed_songs.is_empty(), |list| {
                        list.child(
                            Button::new("play-all-downloads")
                                .primary()
                                .icon(IconName::Play)
                                .label(format!("Play {} offline", completed_songs.len()))
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    this.play_song_collection(play_all.clone(), 0, window, cx);
                                })),
                        )
                    })
                    .children(downloads.iter().enumerate().map(|(index, download)| {
                        let video_id = download.song.video_id.clone();
                        let retry_video_id = video_id.clone();
                        let pause_video_id = video_id.clone();
                        let remove_video_id = video_id.clone();
                        let play_song = download.song.clone();
                        let removing = self.download_removals.contains(&video_id);
                        let cancelling = self
                            .active_downloads
                            .get(&video_id)
                            .is_some_and(|active| active.cancelled.load(Ordering::Acquire));
                        let progress = download.content_length.map_or_else(
                            || format_download_bytes(download.downloaded_bytes),
                            |content_length| {
                                let percent = download
                                    .downloaded_bytes
                                    .saturating_mul(100)
                                    .checked_div(content_length)
                                    .unwrap_or(0);
                                format!(
                                    "{} / {} · {percent}%",
                                    format_download_bytes(download.downloaded_bytes),
                                    format_download_bytes(content_length)
                                )
                            },
                        );
                        h_flex()
                            .flex_wrap()
                            .w_full()
                            .gap_3()
                            .items_center()
                            .rounded(cx.theme().radius)
                            .border_1()
                            .border_color(cx.theme().border)
                            .p_3()
                            .child(self.render_thumbnail(
                                download.song.thumbnail_url.as_deref(),
                                px(44.),
                                IconName::Play,
                                cx,
                            ))
                            .child(
                                v_flex()
                                    .flex_1()
                                    .min_w_0()
                                    .child(div().font_medium().child(download.song.title.clone()))
                                    .child(
                                        div()
                                            .text_sm()
                                            .text_color(cx.theme().muted_foreground)
                                            .child(download.song.artist_line()),
                                    )
                                    .child(
                                        div()
                                            .text_xs()
                                            .text_color(cx.theme().muted_foreground)
                                            .child(format!(
                                                "{} · {} · {progress}",
                                                download.state.label(),
                                                download.audio_quality.label()
                                            )),
                                    )
                                    .when_some(download.last_error.clone(), |details, error| {
                                        details.child(
                                            div()
                                                .text_xs()
                                                .text_color(cx.theme().danger)
                                                .child(error),
                                        )
                                    }),
                            )
                            .when(download.is_complete(), |row| {
                                row.child(
                                    Button::new(format!("play-download-{index}"))
                                        .ghost()
                                        .icon(IconName::Play)
                                        .label("Play offline")
                                        .disabled(removing)
                                        .on_click(cx.listener(move |this, _, window, cx| {
                                            this.play_song_collection(
                                                vec![play_song.clone()],
                                                0,
                                                window,
                                                cx,
                                            );
                                        })),
                                )
                            })
                            .child(self.queue_insert_buttons(
                                format!("download-{index}"),
                                &download.song,
                                cx,
                            ))
                            .when(
                                matches!(
                                    download.state,
                                    DownloadState::Queued | DownloadState::Downloading
                                ),
                                |row| {
                                    row.child(
                                        Button::new(format!("pause-download-{index}"))
                                            .label(if cancelling { "Pausing…" } else { "Pause" })
                                            .disabled(cancelling || removing)
                                            .on_click(cx.listener(move |this, _, _, cx| {
                                                this.pause_download(&pause_video_id, cx);
                                            })),
                                    )
                                },
                            )
                            .when(
                                matches!(
                                    download.state,
                                    DownloadState::Paused | DownloadState::Failed
                                ),
                                |row| {
                                    row.child(
                                        Button::new(format!("retry-download-{index}"))
                                            .label("Resume")
                                            .disabled(removing)
                                            .on_click(cx.listener(move |this, _, _, cx| {
                                                this.retry_download(&retry_video_id, cx);
                                            })),
                                    )
                                },
                            )
                            .child(
                                Button::new(format!("remove-download-{index}"))
                                    .danger()
                                    .label(if removing { "Removing…" } else { "Remove" })
                                    .disabled(removing)
                                    .on_click(cx.listener(move |this, _, window, cx| {
                                        this.confirm_remove_download(
                                            remove_video_id.clone(),
                                            window,
                                            cx,
                                        );
                                    })),
                            )
                            .into_any_element()
                    }))
                    .into_any_element()
            }
        };

        v_flex()
            .gap_3()
            .child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .child(Icon::new(IconName::BookOpen))
                    .child(div().text_lg().font_semibold().child("Offline downloads")),
            )
            .child(
                div()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child("Explicit downloads use a separate persistent store and are never evicted by the playback cache."),
            )
            .when_some(self.download_error.clone(), |layout, error| {
                layout.child(
                    div()
                        .rounded(cx.theme().radius)
                        .bg(cx.theme().danger.opacity(0.12))
                        .text_color(cx.theme().danger)
                        .p_3()
                        .text_sm()
                        .child(error),
                )
            })
            .child(content)
            .into_any_element()
    }

    fn render_recognition_history(&self, cx: &mut Context<Self>) -> AnyElement {
        let busy = self.recognition_history_task.is_some();
        let entry_count = match &self.recognition_history_state {
            StoredViewState::Loaded(history) => history.len(),
            StoredViewState::Loading | StoredViewState::Failed(_) => 0,
        };
        let content = match &self.recognition_history_state {
            StoredViewState::Loading => v_flex()
                .min_h(px(280.))
                .items_center()
                .justify_center()
                .gap_3()
                .child(Icon::new(IconName::LoaderCircle).size_8())
                .child("Loading recognition history…")
                .into_any_element(),
            StoredViewState::Failed(error) => v_flex()
                .min_h(px(280.))
                .items_center()
                .justify_center()
                .gap_3()
                .child(Icon::new(IconName::TriangleAlert).size_8())
                .child(
                    div()
                        .font_semibold()
                        .child("Recognition history unavailable"),
                )
                .child(
                    div()
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .child(error.clone()),
                )
                .child(
                    Button::new("retry-recognition-history")
                        .label("Try again")
                        .on_click(
                            cx.listener(|this, _, _, cx| this.reload_recognition_history(cx)),
                        ),
                )
                .into_any_element(),
            StoredViewState::Loaded(history) if history.is_empty() => v_flex()
                .min_h(px(280.))
                .items_center()
                .justify_center()
                .gap_2()
                .child(Icon::new(IconName::Asterisk).size_8())
                .child(div().font_semibold().child("No recognition history"))
                .child(
                    div()
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .child("Songs identified by Recognize will appear here."),
                )
                .into_any_element(),
            StoredViewState::Loaded(history) => v_flex()
                .gap_3()
                .children(history.iter().enumerate().map(|(index, entry)| {
                    let query = format!("{} {}", entry.result.title, entry.result.artist);
                    let delete_title = entry.result.title.clone();
                    let delete_id = entry.id;
                    h_flex()
                        .w_full()
                        .flex_wrap()
                        .gap_3()
                        .items_center()
                        .rounded(cx.theme().radius)
                        .border_1()
                        .border_color(cx.theme().border)
                        .p_3()
                        .child(self.render_thumbnail(
                            entry.result.cover_art_url.as_deref(),
                            px(60.),
                            IconName::Asterisk,
                            cx,
                        ))
                        .child(
                            v_flex()
                                .flex_1()
                                .min_w(px(220.))
                                .child(
                                    div()
                                        .font_medium()
                                        .overflow_hidden()
                                        .child(entry.result.title.clone()),
                                )
                                .child(
                                    div()
                                        .text_sm()
                                        .text_color(cx.theme().muted_foreground)
                                        .overflow_hidden()
                                        .child(entry.result.artist.clone()),
                                )
                                .child(
                                    div()
                                        .text_xs()
                                        .text_color(cx.theme().muted_foreground)
                                        .child(format_history_age(entry.recognized_at_ms)),
                                ),
                        )
                        .child(
                            Button::new(format!("search-recognition-history-{index}"))
                                .icon(IconName::Search)
                                .label("Search")
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    this.search_source = SearchSource::Online;
                                    this.search_filter = SearchFilter::All;
                                    this.navigate(Route::Search, cx);
                                    this.apply_search_suggestion(query.clone(), true, window, cx);
                                })),
                        )
                        .child(
                            Button::new(format!("delete-recognition-history-{index}"))
                                .danger()
                                .label("Delete")
                                .disabled(busy)
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    this.confirm_delete_recognition_history(
                                        delete_id,
                                        delete_title.clone(),
                                        window,
                                        cx,
                                    );
                                })),
                        )
                        .into_any_element()
                }))
                .into_any_element(),
        };

        v_flex()
            .gap_6()
            .child(
                h_flex()
                    .w_full()
                    .items_center()
                    .justify_between()
                    .gap_3()
                    .child(
                        h_flex()
                            .gap_3()
                            .items_center()
                            .child(
                                Button::new("recognition-history-back")
                                    .ghost()
                                    .icon(IconName::ArrowLeft)
                                    .label("Back")
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.recognition_history_visible = false;
                                        this.refresh_visible_thumbnails(cx);
                                        cx.notify();
                                    })),
                            )
                            .child(
                                v_flex()
                                    .child(
                                        div()
                                            .text_2xl()
                                            .font_semibold()
                                            .child("Recognition history"),
                                    )
                                    .child(
                                        div()
                                            .text_sm()
                                            .text_color(cx.theme().muted_foreground)
                                            .child(format!(
                                                "{entry_count} recognized song{}",
                                                if entry_count == 1 { "" } else { "s" }
                                            )),
                                    ),
                            ),
                    )
                    .when(entry_count > 0, |heading| {
                        heading.child(
                            Button::new("clear-recognition-history")
                                .danger()
                                .label("Clear all")
                                .disabled(busy)
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    this.confirm_clear_recognition_history(entry_count, window, cx);
                                })),
                        )
                    }),
            )
            .when_some(self.recognition_history_error.clone(), |layout, error| {
                layout.child(
                    div()
                        .rounded(cx.theme().radius)
                        .bg(cx.theme().danger.opacity(0.12))
                        .text_color(cx.theme().danger)
                        .p_3()
                        .child(error),
                )
            })
            .child(content)
            .into_any_element()
    }

    fn render_recognition(&self, cx: &mut Context<Self>) -> AnyElement {
        if self.recognition_history_visible {
            return self.render_recognition_history(cx);
        }
        let busy = self.recognition_state.is_busy();
        let (status_title, status_description) = match &self.recognition_state {
            RecognitionViewState::Ready => (
                "Ready".to_string(),
                "Start when music is audible near your default microphone.".to_string(),
            ),
            RecognitionViewState::Listening => (
                "Listening…".to_string(),
                "Capturing the default microphone for at most 12 seconds. You can stop at any time."
                    .to_string(),
            ),
            RecognitionViewState::Processing => (
                "Matching…".to_string(),
                "The microphone is closed. Matching the captured signature with Shazam."
                    .to_string(),
            ),
            RecognitionViewState::Matched(result) => (
                "Song identified".to_string(),
                format!("{} by {}", result.title, result.artist),
            ),
            RecognitionViewState::NoMatch => (
                "No match found".to_string(),
                "Try again with the music louder or move closer to the source.".to_string(),
            ),
            RecognitionViewState::Cancelled => (
                "Stopped".to_string(),
                "Recognition was cancelled.".to_string(),
            ),
            RecognitionViewState::Failed(error) => (
                "Recognition failed".to_string(),
                error.clone(),
            ),
        };
        let action = if busy {
            Button::new("recognition-cancel")
                .danger()
                .label("Stop")
                .on_click(cx.listener(|this, _, _, cx| {
                    this.cancel_recognition(true, cx);
                }))
                .into_any_element()
        } else {
            Button::new("recognition-start")
                .primary()
                .icon(IconName::Asterisk)
                .label(
                    if matches!(&self.recognition_state, RecognitionViewState::Ready) {
                        "Recognize music"
                    } else {
                        "Try again"
                    },
                )
                .on_click(cx.listener(|this, _, _, cx| {
                    this.start_recognition(cx);
                }))
                .into_any_element()
        };
        let match_result = match &self.recognition_state {
            RecognitionViewState::Matched(result) => {
                let query = format!("{} {}", result.title, result.artist);
                let youtube_video_id = result.youtube_video_id.clone();
                let details = [
                    result.album.as_deref(),
                    result.genre.as_deref(),
                    result.release_date.as_deref(),
                ]
                .into_iter()
                .flatten()
                .collect::<Vec<_>>()
                .join(" · ");
                Some(
                    h_flex()
                        .max_w(px(720.))
                        .w_full()
                        .flex_wrap()
                        .gap_4()
                        .items_center()
                        .rounded(cx.theme().radius_lg)
                        .border_1()
                        .border_color(cx.theme().border)
                        .p_5()
                        .child(self.render_thumbnail(
                            result.cover_art_url.as_deref(),
                            px(96.),
                            IconName::Play,
                            cx,
                        ))
                        .child(
                            v_flex()
                                .flex_1()
                                .min_w(px(220.))
                                .gap_1()
                                .child(div().text_xl().font_semibold().child(result.title.clone()))
                                .child(
                                    div()
                                        .text_sm()
                                        .text_color(cx.theme().muted_foreground)
                                        .child(result.artist.clone()),
                                )
                                .when(!details.is_empty(), |metadata| {
                                    metadata.child(
                                        div()
                                            .text_xs()
                                            .text_color(cx.theme().muted_foreground)
                                            .child(details),
                                    )
                                }),
                        )
                        .child(
                            h_flex()
                                .gap_2()
                                .flex_wrap()
                                .when_some(youtube_video_id, |actions, video_id| {
                                    actions.child(
                                        Button::new("play-recognition-match")
                                            .primary()
                                            .icon(IconName::Play)
                                            .label("Play")
                                            .on_click(cx.listener(move |this, _, window, cx| {
                                                this.search_source = SearchSource::Online;
                                                this.navigate(Route::Search, cx);
                                                this.open_youtube_url(
                                                    ParsedYouTubeUrl::Video(video_id.clone()),
                                                    window,
                                                    cx,
                                                );
                                            })),
                                    )
                                })
                                .child(
                                    Button::new("search-recognition-match")
                                        .icon(IconName::Search)
                                        .label("Find on YouTube Music")
                                        .on_click(cx.listener(move |this, _, window, cx| {
                                            this.search_source = SearchSource::Online;
                                            this.search_filter = SearchFilter::All;
                                            this.navigate(Route::Search, cx);
                                            this.apply_search_suggestion(
                                                query.clone(),
                                                true,
                                                window,
                                                cx,
                                            );
                                        })),
                                ),
                        )
                        .into_any_element(),
                )
            }
            _ => None,
        };

        v_flex()
            .gap_6()
            .child(self.page_heading(
                "Recognize music",
                "Listen to nearby music, identify it, then play or search it on YouTube Music.",
                cx,
            ))
            .child(
                Button::new("open-recognition-history")
                    .icon(IconName::BookOpen)
                    .label("History")
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.open_recognition_history(cx);
                    })),
            )
            .child(
                v_flex()
                    .max_w(px(720.))
                    .gap_4()
                    .rounded(cx.theme().radius_lg)
                    .border_1()
                    .border_color(cx.theme().border)
                    .bg(cx.theme().secondary)
                    .p_5()
                    .child(
                        h_flex()
                            .gap_3()
                            .items_center()
                            .child(
                                div()
                                    .size_10()
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .rounded(cx.theme().radius)
                                    .bg(cx.theme().primary)
                                    .text_color(cx.theme().primary_foreground)
                                    .child(Icon::new(if busy {
                                        IconName::LoaderCircle
                                    } else {
                                        IconName::Asterisk
                                    })),
                            )
                            .child(
                                v_flex()
                                    .gap_1()
                                    .child(div().font_semibold().child(status_title))
                                    .child(
                                        div()
                                            .text_sm()
                                            .text_color(cx.theme().muted_foreground)
                                            .child(status_description),
                                    ),
                            ),
                    )
                    .child(action),
            )
            .when_some(match_result, |layout, result| layout.child(result))
            .child(
                v_flex()
                    .max_w(px(720.))
                    .gap_3()
                    .rounded(cx.theme().radius_lg)
                    .border_1()
                    .border_color(cx.theme().border)
                    .p_5()
                    .child(
                        h_flex()
                            .gap_2()
                            .items_center()
                            .child(Icon::new(IconName::Info))
                            .child(div().font_semibold().child("How recognition works")),
                    )
                    .child(
                        div()
                            .text_sm()
                            .text_color(cx.theme().muted_foreground)
                            .child("Recording starts only after you click the button, stops after 12 seconds or when you cancel, and is cancelled automatically when you leave this page."),
                    )
                    .child(
                        div()
                            .text_sm()
                            .text_color(cx.theme().muted_foreground)
                            .child("Metrolist creates a Shazam-compatible signature in memory and sends that signature to Shazam for matching. The recorded PCM is not written to disk."),
                    ),
            )
            .into_any_element()
    }

    fn downloaded_podcast_episodes(&self) -> Vec<Song> {
        let mut songs = match &self.downloads_state {
            StoredViewState::Loaded(downloads) => downloads
                .iter()
                .filter(|download| download.is_complete() && download.song.is_episode)
                .map(|download| download.song.clone())
                .collect::<Vec<_>>(),
            StoredViewState::Loading | StoredViewState::Failed(_) => Vec::new(),
        };
        let play_times = if self.library_podcast_sort == LibrarySongSort::PlayTime {
            let mut totals = HashMap::new();
            if let HistoryViewState::Loaded(history) = &self.history_state {
                for entry in history {
                    let total = totals.entry(entry.song.video_id.clone()).or_insert(0_u128);
                    *total = total.saturating_add(entry.play_time.as_millis());
                }
            }
            totals
        } else {
            HashMap::new()
        };
        match self.library_podcast_sort {
            LibrarySongSort::Recent => {
                if self.library_podcast_sort_direction == SortDirection::Ascending {
                    songs.reverse();
                }
            }
            LibrarySongSort::Title | LibrarySongSort::Artist | LibrarySongSort::PlayTime => {
                songs.sort_by(|left, right| {
                    let ordering = match self.library_podcast_sort {
                        LibrarySongSort::Title => {
                            left.title.to_lowercase().cmp(&right.title.to_lowercase())
                        }
                        LibrarySongSort::Artist => left
                            .artist_line()
                            .to_lowercase()
                            .cmp(&right.artist_line().to_lowercase()),
                        LibrarySongSort::PlayTime => play_times
                            .get(&left.video_id)
                            .copied()
                            .unwrap_or_default()
                            .cmp(&play_times.get(&right.video_id).copied().unwrap_or_default()),
                        LibrarySongSort::Recent => std::cmp::Ordering::Equal,
                    };
                    if self.library_podcast_sort_direction == SortDirection::Descending {
                        ordering.reverse()
                    } else {
                        ordering
                    }
                });
            }
        }
        songs
    }

    fn render_podcasts_section(
        &self,
        selected_tab: LibraryTab,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let podcasts = match &self.podcast_subscriptions {
            StoredViewState::Loading => v_flex()
                .min_h(px(100.))
                .items_center()
                .justify_center()
                .child("Loading saved podcasts…")
                .into_any_element(),
            StoredViewState::Failed(message) => div()
                .text_sm()
                .text_color(cx.theme().danger)
                .child(format!("Saved podcasts unavailable: {message}"))
                .into_any_element(),
            StoredViewState::Loaded(podcasts) if podcasts.is_empty() => div()
                .text_sm()
                .text_color(cx.theme().muted_foreground)
                .child("No podcasts saved on this device yet.")
                .into_any_element(),
            StoredViewState::Loaded(podcasts) => v_flex()
                .gap_2()
                .children(podcasts.iter().enumerate().map(|(index, podcast)| {
                    let item = BrowseItem {
                        browse_id: podcast.podcast_id.clone(),
                        kind: BrowseKind::Podcast,
                        title: podcast.title.clone(),
                        subtitle: podcast.author.clone().unwrap_or_default(),
                        thumbnail_url: podcast.thumbnail_url.clone(),
                        params: None,
                        editable: false,
                    };
                    let open_item = item.clone();
                    let remove_item = item;
                    let channel_id = podcast.channel_id.clone();
                    h_flex()
                        .w_full()
                        .items_center()
                        .gap_3()
                        .rounded(cx.theme().radius)
                        .border_1()
                        .border_color(cx.theme().border)
                        .p_3()
                        .child(self.render_thumbnail(
                            podcast.thumbnail_url.as_deref(),
                            px(48.),
                            IconName::BookOpen,
                            cx,
                        ))
                        .child(
                            v_flex()
                                .flex_1()
                                .min_w_0()
                                .child(div().font_medium().child(podcast.title.clone()))
                                .when_some(podcast.author.clone(), |details, author| {
                                    details.child(
                                        div()
                                            .text_sm()
                                            .text_color(cx.theme().muted_foreground)
                                            .child(author),
                                    )
                                }),
                        )
                        .child(
                            Button::new(format!("remove-saved-podcast-{index}"))
                                .ghost()
                                .label("Remove")
                                .disabled(self.podcast_busy())
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.toggle_podcast_subscription(
                                        remove_item.clone(),
                                        channel_id.clone(),
                                        cx,
                                    );
                                })),
                        )
                        .child(
                            Button::new(format!("open-saved-podcast-{index}"))
                                .ghost()
                                .label("Open")
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.open_online_browse(open_item.clone(), cx);
                                })),
                        )
                        .into_any_element()
                }))
                .into_any_element(),
        };

        let episodes = match &self.episodes_for_later {
            StoredViewState::Loading => v_flex()
                .min_h(px(100.))
                .items_center()
                .justify_center()
                .child("Loading episodes for later…")
                .into_any_element(),
            StoredViewState::Failed(message) => div()
                .text_sm()
                .text_color(cx.theme().danger)
                .child(format!("Episodes for Later unavailable: {message}"))
                .into_any_element(),
            StoredViewState::Loaded(episodes) if episodes.is_empty() => div()
                .text_sm()
                .text_color(cx.theme().muted_foreground)
                .child("No episodes saved for later on this device yet.")
                .into_any_element(),
            StoredViewState::Loaded(episodes) => {
                let songs = Arc::new(
                    episodes
                        .iter()
                        .map(|episode| episode.song.clone())
                        .collect::<Vec<_>>(),
                );
                v_flex()
                    .gap_2()
                    .children(episodes.iter().enumerate().map(|(index, episode)| {
                        self.render_online_song_row(index, &episode.song, songs.clone(), cx)
                    }))
                    .into_any_element()
            }
        };

        let channels = match &self.podcast_subscriptions {
            StoredViewState::Loading => v_flex()
                .min_h(px(120.))
                .items_center()
                .justify_center()
                .child("Loading podcast channels…")
                .into_any_element(),
            StoredViewState::Failed(message) => div()
                .text_sm()
                .text_color(cx.theme().danger)
                .child(format!("Podcast channels unavailable: {message}"))
                .into_any_element(),
            StoredViewState::Loaded(podcasts) => {
                let mut seen = HashSet::new();
                let channels = podcasts
                    .iter()
                    .filter_map(|podcast| {
                        let channel_id = podcast
                            .channel_id
                            .as_ref()
                            .filter(|id| !id.trim().is_empty())?;
                        seen.insert(channel_id.clone()).then(|| BrowseItem {
                            browse_id: channel_id.clone(),
                            kind: BrowseKind::Artist,
                            title: podcast
                                .author
                                .clone()
                                .filter(|author| !author.trim().is_empty())
                                .unwrap_or_else(|| podcast.title.clone()),
                            subtitle: "Podcast channel".into(),
                            thumbnail_url: podcast.thumbnail_url.clone(),
                            params: None,
                            editable: false,
                        })
                    })
                    .collect::<Vec<_>>();
                if channels.is_empty() {
                    div()
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .child("No subscribed podcast channels were found.")
                        .into_any_element()
                } else {
                    v_flex()
                        .gap_2()
                        .child(
                            div()
                                .font_medium()
                                .child(format!("{} channels", channels.len())),
                        )
                        .children(channels.iter().enumerate().map(|(index, item)| {
                            self.render_browse_item_row(index + 70_000, item, cx)
                        }))
                        .into_any_element()
                }
            }
        };

        let downloaded = match &self.downloads_state {
            StoredViewState::Loading => v_flex()
                .min_h(px(120.))
                .items_center()
                .justify_center()
                .child("Loading downloaded episodes…")
                .into_any_element(),
            StoredViewState::Failed(message) => div()
                .text_sm()
                .text_color(cx.theme().danger)
                .child(format!("Downloaded episodes unavailable: {message}"))
                .into_any_element(),
            StoredViewState::Loaded(_) => {
                let songs = Arc::new(self.downloaded_podcast_episodes());
                if songs.is_empty() {
                    div()
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .child("No podcast episodes are fully downloaded yet.")
                        .into_any_element()
                } else {
                    let shuffle = songs.clone();
                    let (direction_icon, direction_label) =
                        match self.library_podcast_sort_direction {
                            SortDirection::Ascending => (IconName::ArrowUp, "Ascending"),
                            SortDirection::Descending => (IconName::ArrowDown, "Descending"),
                        };
                    v_flex()
                        .gap_3()
                        .child(
                            h_flex()
                                .w_full()
                                .flex_wrap()
                                .gap_2()
                                .child(
                                    div()
                                        .flex_1()
                                        .font_medium()
                                        .child(format!("{} downloaded episodes", songs.len())),
                                )
                                .children(LibrarySongSort::ALL.into_iter().map(|sort| {
                                    Button::new(format!(
                                        "podcast-download-sort-{}",
                                        sort.label().to_lowercase().replace(' ', "-")
                                    ))
                                    .label(sort.label())
                                    .selected(self.library_podcast_sort == sort)
                                    .on_click(cx.listener(
                                        move |this, _, _, cx| {
                                            this.library_podcast_sort = sort;
                                            cx.notify();
                                        },
                                    ))
                                }))
                                .child(
                                    Button::new("podcast-download-sort-direction")
                                        .icon(direction_icon)
                                        .label(direction_label)
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            this.library_podcast_sort_direction =
                                                match this.library_podcast_sort_direction {
                                                    SortDirection::Ascending => {
                                                        SortDirection::Descending
                                                    }
                                                    SortDirection::Descending => {
                                                        SortDirection::Ascending
                                                    }
                                                };
                                            cx.notify();
                                        })),
                                )
                                .child(
                                    Button::new("shuffle-downloaded-podcasts")
                                        .label("Shuffle")
                                        .on_click(cx.listener(move |this, _, window, cx| {
                                            this.play_shuffled_collection(
                                                shuffle.as_ref().clone(),
                                                window,
                                                cx,
                                            );
                                        })),
                                ),
                        )
                        .children(songs.iter().enumerate().map(|(index, song)| {
                            self.render_online_song_row(index, song, songs.clone(), cx)
                        }))
                        .into_any_element()
                }
            }
        };

        let selected_source = if selected_tab == LibraryTab::Podcasts {
            self.library_podcast_source
        } else {
            LibraryPodcastSource::Episodes
        };
        let content = match selected_source {
            LibraryPodcastSource::Episodes => {
                let new_episodes = BrowseItem {
                    browse_id: "RDPN".into(),
                    kind: BrowseKind::Playlist,
                    title: "New episodes".into(),
                    subtitle: "YouTube Music".into(),
                    thumbnail_url: None,
                    params: None,
                    editable: false,
                };
                let remote_saved = BrowseItem {
                    browse_id: "SE".into(),
                    kind: BrowseKind::Playlist,
                    title: "Episodes for Later".into(),
                    subtitle: "YouTube Music".into(),
                    thumbnail_url: None,
                    params: None,
                    editable: false,
                };
                v_flex()
                    .gap_3()
                    .when(self.account_ready(), |episodes_section| {
                        episodes_section.child(
                            h_flex()
                                .flex_wrap()
                                .gap_2()
                                .child(
                                    Button::new("open-new-podcast-episodes")
                                        .label("New episodes")
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            this.open_online_browse(new_episodes.clone(), cx);
                                        })),
                                )
                                .child(
                                    Button::new("open-remote-episodes-for-later")
                                        .label("Open synced Episodes for Later")
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            this.open_online_browse(remote_saved.clone(), cx);
                                        })),
                                ),
                        )
                    })
                    .child(div().font_medium().child("Saved podcasts"))
                    .child(podcasts)
                    .child(div().mt_2().font_medium().child("Episodes for Later"))
                    .child(episodes)
                    .into_any_element()
            }
            LibraryPodcastSource::Channels => channels,
            LibraryPodcastSource::Downloaded => downloaded,
        };

        v_flex()
            .gap_4()
            .rounded(cx.theme().radius_lg)
            .border_1()
            .border_color(cx.theme().border)
            .p_5()
            .child(
                h_flex()
                    .w_full()
                    .items_center()
                    .justify_between()
                    .gap_3()
                    .child(div().text_lg().font_semibold().child("Podcasts"))
                    .when(self.account_ready(), |header| {
                        header.child(
                            Button::new("sync-podcast-library")
                                .label(
                                    if self.podcast_operation == PodcastOperation::Syncing {
                                        "Syncing…"
                                    } else {
                                        "Sync YouTube Music"
                                    },
                                )
                                .disabled(self.podcast_busy())
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.sync_podcast_library(cx);
                                })),
                        )
                    }),
            )
            .child(
                div()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child("Saved shows and episodes work without an account. When signed in, changes are also synced to YouTube Music."),
            )
            .when(selected_tab == LibraryTab::Podcasts, |section| {
                section.child(
                    h_flex()
                        .flex_wrap()
                        .gap_2()
                        .children(LibraryPodcastSource::ALL.into_iter().map(|source| {
                            Button::new(format!(
                                "library-podcast-source-{}",
                                source.label().to_lowercase()
                            ))
                            .label(source.label())
                            .selected(self.library_podcast_source == source)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.library_podcast_source = source;
                                this.refresh_visible_thumbnails(cx);
                                cx.notify();
                            }))
                        })),
                )
            })
            .when_some(self.podcast_error.clone(), |section, message| {
                section.child(
                    div()
                        .rounded(cx.theme().radius)
                        .bg(cx.theme().danger.opacity(0.12))
                        .text_color(cx.theme().danger)
                        .p_3()
                        .child(message),
                )
            })
            .when_some(self.podcast_notice.clone(), |section, message| {
                section.child(
                    div()
                        .rounded(cx.theme().radius)
                        .bg(cx.theme().success.opacity(0.12))
                        .text_color(cx.theme().success)
                        .p_3()
                        .child(message),
                )
            })
            .child(content)
            .into_any_element()
    }

    fn library_song_results(&self) -> Vec<Song> {
        let mut songs = Vec::new();
        let mut seen = HashSet::new();
        let mut add_song = |song: &Song| {
            if seen.insert(song.video_id.clone()) {
                songs.push(song.clone());
            }
        };
        match self.library_song_source {
            LibrarySongSource::Liked => {
                if let CloudLibraryViewState::Loaded(library) = &self.cloud_library_state {
                    for song in &library.liked_songs {
                        add_song(song);
                    }
                }
                if let StoredViewState::Loaded(favorites) = &self.favorites_state {
                    for favorite in favorites {
                        add_song(&favorite.song);
                    }
                }
            }
            LibrarySongSource::Library => {
                if let CloudLibraryViewState::Loaded(library) = &self.cloud_library_state {
                    for song in &library.library_songs {
                        add_song(song);
                    }
                }
            }
            LibrarySongSource::Uploaded => {
                if let CloudLibraryViewState::Loaded(library) = &self.cloud_library_state {
                    for song in &library.uploaded_songs {
                        add_song(song);
                    }
                }
            }
            LibrarySongSource::Downloaded => {
                if let StoredViewState::Loaded(downloads) = &self.downloads_state {
                    for download in downloads.iter().filter(|download| download.is_complete()) {
                        add_song(&download.song);
                    }
                }
            }
        }

        let query = self.library_song_query.trim().to_lowercase();
        if !query.is_empty() {
            songs.retain(|song| {
                song.title.to_lowercase().contains(&query)
                    || song
                        .artists
                        .iter()
                        .any(|artist| artist.name.to_lowercase().contains(&query))
            });
        }

        let play_times = if self.library_song_sort == LibrarySongSort::PlayTime {
            let mut totals = HashMap::new();
            if let HistoryViewState::Loaded(history) = &self.history_state {
                for entry in history {
                    let total = totals.entry(entry.song.video_id.clone()).or_insert(0_u128);
                    *total = total.saturating_add(entry.play_time.as_millis());
                }
            }
            totals
        } else {
            HashMap::new()
        };
        match self.library_song_sort {
            LibrarySongSort::Recent => {
                if self.library_song_sort_direction == SortDirection::Ascending {
                    songs.reverse();
                }
            }
            LibrarySongSort::Title | LibrarySongSort::Artist | LibrarySongSort::PlayTime => {
                songs.sort_by(|left, right| {
                    let ordering = match self.library_song_sort {
                        LibrarySongSort::Title => {
                            left.title.to_lowercase().cmp(&right.title.to_lowercase())
                        }
                        LibrarySongSort::Artist => left
                            .artists
                            .iter()
                            .map(|artist| artist.name.as_str())
                            .collect::<Vec<_>>()
                            .join(", ")
                            .to_lowercase()
                            .cmp(
                                &right
                                    .artists
                                    .iter()
                                    .map(|artist| artist.name.as_str())
                                    .collect::<Vec<_>>()
                                    .join(", ")
                                    .to_lowercase(),
                            ),
                        LibrarySongSort::PlayTime => play_times
                            .get(&left.video_id)
                            .copied()
                            .unwrap_or_default()
                            .cmp(&play_times.get(&right.video_id).copied().unwrap_or_default()),
                        LibrarySongSort::Recent => std::cmp::Ordering::Equal,
                    };
                    if self.library_song_sort_direction == SortDirection::Descending {
                        ordering.reverse()
                    } else {
                        ordering
                    }
                });
            }
        }
        songs
    }

    fn library_catalog_results(&self, kind: BrowseKind) -> Vec<BrowseItem> {
        let mut items = match (kind, self.library_album_source, self.library_artist_source) {
            (BrowseKind::Album, LibraryAlbumSource::Liked, _) => self
                .cloud_library()
                .map(|library| library.albums.clone())
                .unwrap_or_default(),
            (BrowseKind::Album, LibraryAlbumSource::Uploaded, _) => self
                .cloud_library()
                .map(|library| library.uploaded_albums.clone())
                .unwrap_or_default(),
            (BrowseKind::Artist, _, LibraryArtistSource::Liked) => self
                .cloud_library()
                .map(|library| library.artists.clone())
                .unwrap_or_default(),
            (BrowseKind::Album, LibraryAlbumSource::Library, _)
            | (BrowseKind::Artist, _, LibraryArtistSource::Library) => {
                match &self.local_catalog_state {
                    StoredViewState::Loaded(items) => items
                        .iter()
                        .filter(|item| item.kind == kind)
                        .cloned()
                        .collect(),
                    StoredViewState::Loading | StoredViewState::Failed(_) => Vec::new(),
                }
            }
            _ => Vec::new(),
        };

        if kind == BrowseKind::Artist && self.library_artist_source == LibraryArtistSource::Library
        {
            let mut seen = items
                .iter()
                .map(|item| item.browse_id.clone())
                .collect::<HashSet<_>>();
            for song in self.local_known_songs() {
                for artist in &song.artists {
                    let Some(id) = artist.id.as_ref().filter(|id| !id.trim().is_empty()) else {
                        continue;
                    };
                    if seen.insert(id.clone()) {
                        items.push(BrowseItem {
                            browse_id: id.clone(),
                            kind: BrowseKind::Artist,
                            title: artist.name.clone(),
                            subtitle: "Artist · On this device".into(),
                            thumbnail_url: song.thumbnail_url.clone(),
                            params: None,
                            editable: false,
                        });
                    }
                }
            }
        }

        let query = self.library_catalog_query.trim().to_lowercase();
        if !query.is_empty() {
            items.retain(|item| {
                item.title.to_lowercase().contains(&query)
                    || item.subtitle.to_lowercase().contains(&query)
            });
        }
        match self.library_catalog_sort {
            LibraryCatalogSort::Recent => {
                if self.library_catalog_sort_direction == SortDirection::Ascending {
                    items.reverse();
                }
            }
            LibraryCatalogSort::Title | LibraryCatalogSort::Subtitle => {
                items.sort_by(|left, right| {
                    let ordering = match self.library_catalog_sort {
                        LibraryCatalogSort::Title => {
                            left.title.to_lowercase().cmp(&right.title.to_lowercase())
                        }
                        LibraryCatalogSort::Subtitle => left
                            .subtitle
                            .to_lowercase()
                            .cmp(&right.subtitle.to_lowercase()),
                        LibraryCatalogSort::Recent => std::cmp::Ordering::Equal,
                    };
                    if self.library_catalog_sort_direction == SortDirection::Descending {
                        ordering.reverse()
                    } else {
                        ordering
                    }
                });
            }
        }
        items
    }

    fn render_library_catalog(&self, kind: BrowseKind, cx: &mut Context<Self>) -> AnyElement {
        let is_album = kind == BrowseKind::Album;
        let label = if is_album { "Albums" } else { "Artists" };
        let items = self.library_catalog_results(kind);
        let cloud_source = if is_album {
            self.library_album_source != LibraryAlbumSource::Library
        } else {
            self.library_artist_source != LibraryArtistSource::Library
        };
        let loading = if cloud_source {
            matches!(self.cloud_library_state, CloudLibraryViewState::Loading)
        } else {
            matches!(self.local_catalog_state, StoredViewState::Loading)
        };
        let error = if cloud_source {
            if let CloudLibraryViewState::Failed(error) = &self.cloud_library_state {
                Some(error.clone())
            } else {
                None
            }
        } else {
            match &self.local_catalog_state {
                StoredViewState::Failed(error) => Some(error.clone()),
                StoredViewState::Loading | StoredViewState::Loaded(_) => {
                    self.local_catalog_error.clone()
                }
            }
        };
        let has_error = error.is_some();
        let signed_out =
            cloud_source && matches!(self.cloud_library_state, CloudLibraryViewState::SignedOut);
        let source_filters = if is_album {
            h_flex()
                .flex_wrap()
                .gap_2()
                .children(LibraryAlbumSource::ALL.into_iter().map(|source| {
                    Button::new(format!(
                        "library-album-source-{}",
                        source.label().to_lowercase()
                    ))
                    .label(source.label())
                    .selected(self.library_album_source == source)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.library_album_source = source;
                        this.refresh_visible_thumbnails(cx);
                        cx.notify();
                    }))
                }))
                .into_any_element()
        } else {
            h_flex()
                .flex_wrap()
                .gap_2()
                .children(LibraryArtistSource::ALL.into_iter().map(|source| {
                    Button::new(format!(
                        "library-artist-source-{}",
                        source.label().to_lowercase()
                    ))
                    .label(source.label())
                    .selected(self.library_artist_source == source)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.library_artist_source = source;
                        this.refresh_visible_thumbnails(cx);
                        cx.notify();
                    }))
                }))
                .into_any_element()
        };
        let (direction_icon, direction_label) = match self.library_catalog_sort_direction {
            SortDirection::Ascending => (IconName::ArrowUp, "Ascending"),
            SortDirection::Descending => (IconName::ArrowDown, "Descending"),
        };

        v_flex()
            .gap_4()
            .child(source_filters)
            .child(
                h_flex()
                    .w_full()
                    .flex_wrap()
                    .gap_2()
                    .child(div().min_w(px(240.)).flex_1().child(
                        Input::new(&self.library_catalog_search_input).prefix(IconName::Search),
                    ))
                    .children(LibraryCatalogSort::ALL.into_iter().map(|sort| {
                        Button::new(format!(
                            "library-catalog-sort-{}",
                            sort.label()
                                .to_lowercase()
                                .replace(' ', "-")
                                .replace('/', "-")
                        ))
                        .label(sort.label())
                        .selected(self.library_catalog_sort == sort)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.library_catalog_sort = sort;
                            cx.notify();
                        }))
                    }))
                    .child(
                        Button::new("library-catalog-sort-direction")
                            .icon(direction_icon)
                            .label(direction_label)
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.library_catalog_sort_direction =
                                    match this.library_catalog_sort_direction {
                                        SortDirection::Ascending => SortDirection::Descending,
                                        SortDirection::Descending => SortDirection::Ascending,
                                    };
                                cx.notify();
                            })),
                    ),
            )
            .when(loading, |layout| {
                layout.child(
                    h_flex()
                        .gap_2()
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .child(Icon::new(IconName::LoaderCircle))
                        .child(format!("Loading {}…", label.to_lowercase())),
                )
            })
            .when(signed_out, |layout| {
                layout.child(
                    h_flex()
                        .flex_wrap()
                        .gap_3()
                        .items_center()
                        .child(
                            div()
                                .text_sm()
                                .text_color(cx.theme().muted_foreground)
                                .child(format!(
                                    "Sign in to load these YouTube Music {}.",
                                    label.to_lowercase()
                                )),
                        )
                        .child(
                            Button::new("library-catalog-open-account-settings")
                                .label("Open account settings")
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.navigate(Route::Settings, cx);
                                })),
                        ),
                )
            })
            .when_some(error, |layout, error| {
                layout.child(
                    v_flex()
                        .gap_2()
                        .rounded(cx.theme().radius)
                        .bg(cx.theme().danger.opacity(0.12))
                        .text_color(cx.theme().danger)
                        .p_3()
                        .child(format!("{label} unavailable: {error}"))
                        .when(cloud_source && self.account_ready(), |message| {
                            message.child(
                                Button::new("retry-library-catalog-source")
                                    .label("Try again")
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.reload_cloud_library(cx);
                                    })),
                            )
                        }),
                )
            })
            .when(!items.is_empty(), |layout| {
                layout
                    .child(div().font_semibold().child(format!(
                        "{} {}",
                        items.len(),
                        label.to_lowercase()
                    )))
                    .children(items.iter().enumerate().map(|(index, item)| {
                        self.render_browse_item_row(
                            index + if is_album { 50_000 } else { 60_000 },
                            item,
                            cx,
                        )
                    }))
            })
            .when(items.is_empty() && !loading && !has_error, |layout| {
                layout.child(
                    v_flex()
                        .min_h(px(180.))
                        .items_center()
                        .justify_center()
                        .gap_2()
                        .child(Icon::new(IconName::Search).size_8())
                        .child(
                            div()
                                .font_semibold()
                                .child(format!("No {} found", label.to_lowercase())),
                        )
                        .when(!self.library_catalog_query.trim().is_empty(), |empty| {
                            empty.child(
                                div()
                                    .text_sm()
                                    .text_color(cx.theme().muted_foreground)
                                    .child("Try a different title or detail."),
                            )
                        }),
                )
            })
            .into_any_element()
    }

    fn render_library_songs(&self, cx: &mut Context<Self>) -> AnyElement {
        let songs = Arc::new(self.library_song_results());
        let play_all = songs.clone();
        let shuffle_all = songs.clone();
        let source = self.library_song_source;
        let cloud_source = source != LibrarySongSource::Downloaded;
        let loading = match source {
            LibrarySongSource::Liked => {
                matches!(self.cloud_library_state, CloudLibraryViewState::Loading)
                    || matches!(self.favorites_state, StoredViewState::Loading)
            }
            LibrarySongSource::Library | LibrarySongSource::Uploaded => {
                matches!(self.cloud_library_state, CloudLibraryViewState::Loading)
            }
            LibrarySongSource::Downloaded => {
                matches!(self.downloads_state, StoredViewState::Loading)
            }
        };
        let error = match source {
            LibrarySongSource::Liked => match (&self.cloud_library_state, &self.favorites_state) {
                (CloudLibraryViewState::Failed(error), _) => Some(error.clone()),
                (_, StoredViewState::Failed(error)) => Some(error.clone()),
                _ => None,
            },
            LibrarySongSource::Library | LibrarySongSource::Uploaded => {
                if let CloudLibraryViewState::Failed(error) = &self.cloud_library_state {
                    Some(error.clone())
                } else {
                    None
                }
            }
            LibrarySongSource::Downloaded => {
                if let StoredViewState::Failed(error) = &self.downloads_state {
                    Some(error.clone())
                } else {
                    None
                }
            }
        };
        let signed_out =
            cloud_source && matches!(self.cloud_library_state, CloudLibraryViewState::SignedOut);
        let has_error = error.is_some();
        let (direction_icon, direction_label) = match self.library_song_sort_direction {
            SortDirection::Ascending => (IconName::ArrowUp, "Ascending"),
            SortDirection::Descending => (IconName::ArrowDown, "Descending"),
        };

        v_flex()
            .gap_4()
            .child(
                h_flex()
                    .flex_wrap()
                    .gap_2()
                    .children(LibrarySongSource::ALL.into_iter().map(|candidate| {
                        Button::new(format!(
                            "library-song-source-{}",
                            candidate.label().to_lowercase()
                        ))
                        .label(candidate.label())
                        .selected(source == candidate)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.library_song_source = candidate;
                            this.refresh_visible_thumbnails(cx);
                            cx.notify();
                        }))
                    })),
            )
            .child(
                h_flex()
                    .w_full()
                    .flex_wrap()
                    .gap_2()
                    .child(
                        div()
                            .min_w(px(240.))
                            .flex_1()
                            .child(Input::new(&self.library_search_input).prefix(IconName::Search)),
                    )
                    .children(LibrarySongSort::ALL.into_iter().map(|sort| {
                        Button::new(format!(
                            "library-song-sort-{}",
                            sort.label().to_lowercase().replace(' ', "-")
                        ))
                        .label(sort.label())
                        .selected(self.library_song_sort == sort)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.library_song_sort = sort;
                            cx.notify();
                        }))
                    }))
                    .child(
                        Button::new("library-song-sort-direction")
                            .icon(direction_icon)
                            .label(direction_label)
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.library_song_sort_direction =
                                    match this.library_song_sort_direction {
                                        SortDirection::Ascending => SortDirection::Descending,
                                        SortDirection::Descending => SortDirection::Ascending,
                                    };
                                cx.notify();
                            })),
                    ),
            )
            .when(loading, |layout| {
                layout.child(
                    h_flex()
                        .gap_2()
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .child(Icon::new(IconName::LoaderCircle))
                        .child(format!("Loading {} songs…", source.label().to_lowercase())),
                )
            })
            .when(signed_out, |layout| {
                layout.child(
                    h_flex()
                        .flex_wrap()
                        .gap_3()
                        .items_center()
                        .child(
                            div()
                                .text_sm()
                                .text_color(cx.theme().muted_foreground)
                                .child(if source == LibrarySongSource::Liked {
                                    "Sign in to include YouTube Music likes; local favorites remain available."
                                } else {
                                    "Sign in to load this YouTube Music song collection."
                                }),
                        )
                        .child(
                            Button::new("library-songs-open-account-settings")
                                .label("Open account settings")
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.navigate(Route::Settings, cx);
                                })),
                        ),
                )
            })
            .when_some(error, |layout, error| {
                layout.child(
                    v_flex()
                        .gap_2()
                        .rounded(cx.theme().radius)
                        .bg(cx.theme().danger.opacity(0.12))
                        .text_color(cx.theme().danger)
                        .p_3()
                        .child(format!("{} songs unavailable: {error}", source.label()))
                        .when(cloud_source && self.account_ready(), |message| {
                            message.child(
                                Button::new("retry-library-song-source")
                                    .label("Try again")
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.reload_cloud_library(cx);
                                    })),
                            )
                        }),
                )
            })
            .when(!songs.is_empty(), |layout| {
                layout
                    .child(
                        h_flex()
                            .flex_wrap()
                            .items_center()
                            .gap_2()
                            .child(
                                div()
                                    .flex_1()
                                    .font_semibold()
                                    .child(format!("{} songs", songs.len())),
                            )
                            .child(
                                Button::new("play-library-song-source")
                                    .primary()
                                    .icon(IconName::Play)
                                    .label("Play all")
                                    .on_click(cx.listener(move |this, _, window, cx| {
                                        this.play_song_collection(
                                            play_all.as_ref().clone(),
                                            0,
                                            window,
                                            cx,
                                        );
                                    })),
                            )
                            .child(
                                Button::new("shuffle-library-song-source")
                                    .label("Shuffle")
                                    .on_click(cx.listener(move |this, _, window, cx| {
                                        this.play_shuffled_collection(
                                            shuffle_all.as_ref().clone(),
                                            window,
                                            cx,
                                        );
                                    })),
                            ),
                    )
                    .children(songs.iter().enumerate().map(|(index, song)| {
                        self.render_online_song_row(index, song, songs.clone(), cx)
                    }))
            })
            .when(songs.is_empty() && !loading && !has_error, |layout| {
                layout.child(
                    v_flex()
                        .min_h(px(180.))
                        .items_center()
                        .justify_center()
                        .gap_2()
                        .child(Icon::new(IconName::Search).size_8())
                        .child(div().font_semibold().child(format!(
                            "No {} songs found",
                            source.label().to_lowercase()
                        )))
                        .when(!self.library_song_query.trim().is_empty(), |empty| {
                            empty.child(
                                div()
                                    .text_sm()
                                    .text_color(cx.theme().muted_foreground)
                                    .child("Try a different title or artist."),
                            )
                        }),
                )
            })
            .into_any_element()
    }

    fn render_library(&self, cx: &mut Context<Self>) -> AnyElement {
        if self.playlist_detail.is_some() {
            return self.render_playlist_detail(cx);
        }

        let retry_targets = self.local_library_retry_targets();
        let local_library_loading = matches!(
            self.library_operation,
            LibraryOperation::LoadingLibrary | LibraryOperation::RetryingLibrary
        );
        let local_library_retrying = self.library_operation == LibraryOperation::RetryingLibrary;
        let selected_tab = self.library_tab;

        v_flex()
            .gap_7()
            .child(
                h_flex()
                    .w_full()
                    .items_center()
                    .justify_between()
                    .gap_3()
                    .child(self.page_heading(
                        "Library",
                        "Browse cloud and local collections, including persistent offline downloads stored on this device.",
                        cx,
                    ))
                    .when(retry_targets.any() || local_library_loading, |heading| {
                        heading.child(
                            Button::new("retry-local-library")
                                .label(if local_library_loading {
                                    if local_library_retrying {
                                        "Retrying local data…"
                                    } else {
                                        "Loading local data…"
                                    }
                                } else {
                                    "Retry local data"
                                })
                                .loading(local_library_loading)
                                .disabled(self.library_busy() || self.podcast_busy())
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.retry_failed_local_library(cx);
                                })),
                        )
                    }),
            )
            .child(
                h_flex()
                    .w_full()
                    .flex_wrap()
                    .gap_2()
                    .children(LibraryTab::ALL.into_iter().map(|tab| {
                        Button::new(format!("library-tab-{}", tab.label().to_lowercase()))
                            .label(tab.label())
                            .selected(selected_tab == tab)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.library_tab = tab;
                                cx.notify();
                            }))
                    })),
            )
            .when_some(self.library_error.clone(), |layout, message| {
                layout.child(
                    div()
                        .rounded(cx.theme().radius)
                        .bg(cx.theme().danger.opacity(0.12))
                        .text_color(cx.theme().danger)
                        .p_3()
                    .child(message),
                )
            })
            .when(selected_tab == LibraryTab::Playlists, |layout| {
                layout.child(self.render_library_playlist_controls(cx))
            })
            .when(
                matches!(selected_tab, LibraryTab::Overview | LibraryTab::Playlists),
                |layout| {
                    layout.child(self.render_cloud_library_section(selected_tab, cx))
                },
            )
            .when(selected_tab == LibraryTab::Songs, |layout| {
                layout.child(self.render_library_songs(cx))
            })
            .when(selected_tab == LibraryTab::Albums, |layout| {
                layout.child(self.render_library_catalog(BrowseKind::Album, cx))
            })
            .when(selected_tab == LibraryTab::Artists, |layout| {
                layout.child(self.render_library_catalog(BrowseKind::Artist, cx))
            })
            .when(
                matches!(selected_tab, LibraryTab::Overview | LibraryTab::Podcasts),
                |layout| layout.child(self.render_podcasts_section(selected_tab, cx)),
            )
            .when(
                selected_tab == LibraryTab::Overview,
                |layout| {
                    layout
                        .child(self.render_downloads_section(cx))
                        .child(self.render_favorites_section(cx))
                },
            )
            .when(
                matches!(selected_tab, LibraryTab::Overview | LibraryTab::Playlists),
                |layout| layout.child(self.render_playlists_section(cx)),
            )
            .when(selected_tab == LibraryTab::Overview, |layout| {
                layout.child(self.render_history_section(cx))
            })
            .into_any_element()
    }

    fn render_audio_outputs(&self, cx: &mut Context<Self>) -> AnyElement {
        let snapshot = self.audio_player.device_snapshot();
        let refreshing = snapshot.operation == AudioDeviceOperation::Refreshing;
        let switching = snapshot.operation == AudioDeviceOperation::Switching;
        let busy = snapshot.operation != AudioDeviceOperation::Idle;
        let selected_id = snapshot.selected_id.clone();

        v_flex()
            .gap_4()
            .max_w(px(680.))
            .rounded(cx.theme().radius_lg)
            .border_1()
            .border_color(cx.theme().border)
            .p_5()
            .child(
                h_flex()
                    .w_full()
                    .items_center()
                    .gap_3()
                    .child(
                        v_flex()
                            .flex_1()
                            .gap_1()
                            .child(div().font_semibold().child("Audio output"))
                            .child(
                                div()
                                    .text_sm()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(
                                        "Choose where music plays. Switching preserves the current track and position.",
                                    ),
                            ),
                    )
                    .child(
                        Button::new("refresh-audio-outputs")
                            .label("Refresh")
                            .loading(refreshing)
                            .disabled(switching)
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.refresh_audio_outputs(cx);
                            })),
                    ),
            )
            .when_some(snapshot.error, |card, message| {
                card.child(
                    div()
                        .rounded(cx.theme().radius)
                        .bg(cx.theme().danger.opacity(0.12))
                        .text_sm()
                        .text_color(cx.theme().danger)
                        .p_3()
                        .child(message),
                )
            })
            .when(snapshot.devices.is_empty(), |card| {
                card.child(
                    div()
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .child(if refreshing {
                            "Detecting output devices…"
                        } else {
                            "No usable output devices found."
                        }),
                )
            })
            .when(!snapshot.devices.is_empty(), |card| {
                card.child(v_flex().max_h(px(320.)).overflow_y_scrollbar().gap_2().children(
                    snapshot
                        .devices
                        .into_iter()
                        .enumerate()
                        .map(|(index, device)| {
                            let is_selected = selected_id.as_deref() == Some(device.id.as_str());
                            let device_id = device.id.clone();
                            let label = if device.is_default {
                                format!("{} · System default", device.name)
                            } else {
                                device.name
                            };
                            Button::new(format!("audio-output-{index}"))
                                .w_full()
                                .justify_start()
                                .label(label)
                                .tooltip(device.id)
                                .selected(is_selected)
                                .disabled(busy || is_selected)
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.select_audio_output(device_id.clone(), cx);
                                }))
                        }),
                ))
            })
            .when(switching, |card| {
                card.child(
                    div()
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .child("Switching output and restoring playback…"),
                )
            })
            .into_any_element()
    }

    fn render_account_settings(&self, cx: &mut Context<Self>) -> AnyElement {
        let busy = self.account_busy();
        let has_session = self.auth_session.is_some();
        let input_empty = self.account_cookie_input.read(cx).value().trim().is_empty();
        let status = match &self.account_state {
            AccountViewState::SignedOut => div()
                .text_sm()
                .text_color(cx.theme().muted_foreground)
                .child("Not signed in. Anonymous search and playback remain available.")
                .into_any_element(),
            AccountViewState::Checking => h_flex()
                .gap_2()
                .text_sm()
                .text_color(cx.theme().muted_foreground)
                .child(Icon::new(IconName::LoaderCircle))
                .child(match self.account_operation {
                    AccountOperation::SigningIn => {
                        "Finish signing in in the secure browser window…"
                    }
                    AccountOperation::Importing => {
                        "Verifying the imported session before saving it…"
                    }
                    AccountOperation::Idle | AccountOperation::SigningOut => {
                        "Checking the saved YouTube Music session…"
                    }
                })
                .into_any_element(),
            AccountViewState::SignedIn(profile) => {
                let subtitle = [profile.email.as_deref(), profile.channel_handle.as_deref()]
                    .into_iter()
                    .flatten()
                    .collect::<Vec<_>>()
                    .join(" · ");
                h_flex()
                    .gap_3()
                    .items_center()
                    .child(self.render_thumbnail(
                        profile.thumbnail_url.as_deref(),
                        px(48.),
                        IconName::User,
                        cx,
                    ))
                    .child(
                        v_flex()
                            .gap_1()
                            .child(div().font_medium().child(profile.name.clone()))
                            .when(!subtitle.is_empty(), |details| {
                                details.child(
                                    div()
                                        .text_sm()
                                        .text_color(cx.theme().muted_foreground)
                                        .child(subtitle),
                                )
                            }),
                    )
                    .into_any_element()
            }
            AccountViewState::Expired(error) => v_flex()
                .gap_1()
                .rounded(cx.theme().radius)
                .bg(cx.theme().warning.opacity(0.12))
                .text_color(cx.theme().warning)
                .p_3()
                .text_sm()
                .child("The saved session is no longer accepted. Cloud likes, playlists, subscriptions, and YouTube Music history sync are paused; anonymous search and playback remain available.")
                .child(error.clone())
                .into_any_element(),
            AccountViewState::Failed(error) => div()
                .rounded(cx.theme().radius)
                .bg(cx.theme().danger.opacity(0.12))
                .text_color(cx.theme().danger)
                .p_3()
                .text_sm()
                .child(error.clone())
                .into_any_element(),
        };
        let import_label = match self.account_operation {
            AccountOperation::Importing => "Verifying…",
            AccountOperation::Idle | AccountOperation::SigningIn | AccountOperation::SigningOut => {
                if has_session {
                    "Verify and replace"
                } else {
                    "Verify and save"
                }
            }
        };
        let sign_in_label = if self.account_operation == AccountOperation::SigningIn {
            "Signing in…"
        } else if has_session {
            "Sign in and replace"
        } else {
            "Sign in"
        };

        v_flex()
            .gap_4()
            .max_w(px(680.))
            .rounded(cx.theme().radius_lg)
            .border_1()
            .border_color(cx.theme().border)
            .p_5()
            .child(
                v_flex()
                    .gap_1()
                    .child(div().font_semibold().child("YouTube Music account"))
                    .child(
                        div()
                            .text_sm()
                            .text_color(cx.theme().muted_foreground)
                            .child(format!(
                                "Account sessions are stored only in {}—never in Metrolist's SQLite database or logs.",
                                self.credential_store.backend_label()
                            )),
                    ),
            )
            .child(status)
            .when_some(self.account_error.clone(), |card, error| {
                card.child(
                    div()
                        .rounded(cx.theme().radius)
                        .bg(cx.theme().danger.opacity(0.12))
                        .text_color(cx.theme().danger)
                        .p_3()
                        .text_sm()
                        .child(error),
                )
            })
            .when_some(self.credential_warning.clone(), |card, warning| {
                card.child(
                    div()
                        .rounded(cx.theme().radius)
                        .bg(cx.theme().warning.opacity(0.12))
                        .text_color(cx.theme().warning)
                        .p_3()
                        .text_sm()
                        .child(warning),
                )
            })
            .child(
                v_flex()
                    .gap_2()
                    .child(
                        Button::new("account-sign-in")
                            .primary()
                            .label(sign_in_label)
                            .disabled(busy)
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.sign_in_account(window, cx);
                            })),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child("Opens an isolated system WebView. Metrolist reads the resulting YouTube Music session only after Google redirects back to music.youtube.com, then verifies the account before saving it."),
                    ),
            )
            .child(
                v_flex()
                    .gap_1()
                    .child(div().text_sm().font_medium().child("Advanced session import"))
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child("Cookie header or Android session template"),
                    )
                    .child(
                        Input::new(&self.account_cookie_input)
                            .content_type(InputContentType::Password)
                            .mask_toggle()
                            .disabled(busy),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child("Paste a music.youtube.com Cookie header containing SAPISID. The field is cleared immediately when submitted."),
                    ),
            )
            .child(
                h_flex()
                    .gap_2()
                    .flex_wrap()
                    .child(
                        Button::new("account-import")
                            .label(import_label)
                            .disabled(busy || input_empty)
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.import_account(window, cx);
                            })),
                    )
                    .when(
                        has_session
                            && matches!(
                                self.account_state,
                                AccountViewState::Failed(_) | AccountViewState::Expired(_)
                            ),
                        |buttons| {
                            buttons.child(
                                Button::new("account-retry")
                                    .label("Retry verification")
                                    .disabled(busy)
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.verify_account(cx);
                                    })),
                            )
                        },
                    )
                    .when(has_session, |buttons| {
                        buttons.child(
                            Button::new("account-sign-out")
                                .danger()
                                .label(if self.account_operation == AccountOperation::SigningOut {
                                    "Signing out…"
                                } else if matches!(
                                    self.account_state,
                                    AccountViewState::Expired(_)
                                ) {
                                    "Remove expired session"
                                } else {
                                    "Sign out"
                                })
                                .disabled(busy)
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.confirm_sign_out_account(window, cx);
                                })),
                        )
                    }),
            )
            .into_any_element()
    }

    fn render_lastfm_settings(&self, cx: &mut Context<Self>) -> AnyElement {
        let busy = self.lastfm_busy();
        let configured = self.lastfm_api_credentials.is_some();
        let signed_in = self.lastfm_session.is_some();
        let username_empty = self
            .lastfm_username_input
            .read(cx)
            .value()
            .trim()
            .is_empty();
        let password_empty = self.lastfm_password_input.read(cx).value().is_empty();
        let policy = self.settings_draft.lastfm_scrobble_policy;
        let operation_label = match self.lastfm_operation {
            LastFmOperation::Idle if signed_in => "Sign in and replace",
            LastFmOperation::Idle => "Sign in",
            LastFmOperation::SigningIn => "Signing in…",
            LastFmOperation::SigningOut => "Signing out…",
        };

        v_flex()
            .gap_4()
            .max_w(px(680.))
            .rounded(cx.theme().radius_lg)
            .border_1()
            .border_color(cx.theme().border)
            .p_5()
            .child(
                v_flex()
                    .gap_1()
                    .child(div().font_semibold().child("Last.fm"))
                    .child(
                        div()
                            .text_sm()
                            .text_color(cx.theme().muted_foreground)
                            .child(format!(
                                "The session key is stored only in {}. Your password is sent once over HTTPS for Last.fm mobile-session authentication, then immediately cleared and never saved.",
                                self.lastfm_credential_store.backend_label()
                            )),
                    ),
            )
            .child(
                div()
                    .text_sm()
                    .text_color(if signed_in {
                        cx.theme().success
                    } else {
                        cx.theme().muted_foreground
                    })
                    .child(
                        self.lastfm_session
                            .as_ref()
                            .map(|session| format!("Signed in as {}", session.username()))
                            .unwrap_or_else(|| "Not signed in".into()),
                    ),
            )
            .when(!configured, |card| {
                card.child(
                    div()
                        .rounded(cx.theme().radius)
                        .bg(cx.theme().warning.opacity(0.12))
                        .text_color(cx.theme().warning)
                        .p_3()
                        .text_sm()
                        .child("Last.fm is optional. To enable it, set LASTFM_API_KEY and LASTFM_SHARED_SECRET (or the Android-compatible LASTFM_SECRET) before starting Metrolist."),
                )
            })
            .when_some(self.lastfm_warning.clone(), |card, warning| {
                card.child(
                    div()
                        .rounded(cx.theme().radius)
                        .bg(cx.theme().warning.opacity(0.12))
                        .text_color(cx.theme().warning)
                        .p_3()
                        .text_sm()
                        .child(warning),
                )
            })
            .when_some(self.lastfm_error.clone(), |card, error| {
                card.child(
                    div()
                        .rounded(cx.theme().radius)
                        .bg(cx.theme().danger.opacity(0.12))
                        .text_color(cx.theme().danger)
                        .p_3()
                        .text_sm()
                        .child(error),
                )
            })
            .when_some(self.lastfm_notice.clone(), |card, notice| {
                card.child(
                    div()
                        .rounded(cx.theme().radius)
                        .bg(cx.theme().success.opacity(0.12))
                        .text_color(cx.theme().success)
                        .p_3()
                        .text_sm()
                        .child(notice),
                )
            })
            .child(
                v_flex()
                    .gap_2()
                    .child(Input::new(&self.lastfm_username_input).disabled(busy || !configured))
                    .child(
                        Input::new(&self.lastfm_password_input)
                            .content_type(InputContentType::Password)
                            .mask_toggle()
                            .disabled(busy || !configured),
                    ),
            )
            .child(
                h_flex()
                    .gap_2()
                    .flex_wrap()
                    .child(
                        Button::new("lastfm-sign-in")
                            .primary()
                            .label(operation_label)
                            .disabled(busy || !configured || username_empty || password_empty)
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.sign_in_lastfm(window, cx);
                            })),
                    )
                    .when(signed_in, |buttons| {
                        buttons.child(
                            Button::new("lastfm-sign-out")
                                .danger()
                                .label(if self.lastfm_operation == LastFmOperation::SigningOut {
                                    "Signing out…"
                                } else {
                                    "Sign out"
                                })
                                .disabled(busy)
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.sign_out_lastfm(window, cx);
                                })),
                        )
                    }),
            )
            .child(div().text_sm().font_medium().child("Activity sync"))
            .child(
                h_flex()
                    .gap_2()
                    .flex_wrap()
                    .child(
                        Button::new("lastfm-scrobbling")
                            .label("Scrobble")
                            .selected(self.settings_draft.lastfm_scrobbling)
                            .disabled(busy || !configured || !signed_in)
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.settings_draft.lastfm_scrobbling =
                                    !this.settings_draft.lastfm_scrobbling;
                                this.settings_error = None;
                                this.settings_notice = None;
                                cx.notify();
                            })),
                    )
                    .child(
                        Button::new("lastfm-now-playing")
                            .label("Now Playing")
                            .selected(self.settings_draft.lastfm_now_playing)
                            .disabled(busy || !configured || !signed_in)
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.settings_draft.lastfm_now_playing =
                                    !this.settings_draft.lastfm_now_playing;
                                this.settings_error = None;
                                this.settings_notice = None;
                                cx.notify();
                            })),
                    )
                    .child(
                        Button::new("lastfm-sync-likes")
                            .label("Sync likes")
                            .selected(self.settings_draft.lastfm_sync_likes)
                            .disabled(busy || !configured || !signed_in)
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.settings_draft.lastfm_sync_likes =
                                    !this.settings_draft.lastfm_sync_likes;
                                this.settings_error = None;
                                this.settings_notice = None;
                                cx.notify();
                            })),
                    ),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child("Scrobble after the earlier of the configured played percentage or maximum delay. Paused time is excluded; tracks at or below the minimum duration are ignored."),
            )
            .child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .child(div().w(px(180.)).text_sm().child(format!(
                        "Minimum track: {} s",
                        policy.min_track_seconds
                    )))
                    .child(
                        Button::new("lastfm-min-duration-down")
                            .ghost()
                            .label("−")
                            .disabled(busy || policy.min_track_seconds <= 10)
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.settings_draft.lastfm_scrobble_policy.min_track_seconds = this
                                    .settings_draft
                                    .lastfm_scrobble_policy
                                    .min_track_seconds
                                    .saturating_sub(5)
                                    .max(10);
                                cx.notify();
                            })),
                    )
                    .child(
                        Button::new("lastfm-min-duration-up")
                            .ghost()
                            .label("+")
                            .disabled(busy || policy.min_track_seconds >= 60)
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.settings_draft.lastfm_scrobble_policy.min_track_seconds = this
                                    .settings_draft
                                    .lastfm_scrobble_policy
                                    .min_track_seconds
                                    .saturating_add(5)
                                    .min(60);
                                cx.notify();
                            })),
                    ),
            )
            .child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .child(div().w(px(180.)).text_sm().child(format!(
                        "Played percentage: {}%",
                        policy.delay_percent_milli / 10
                    )))
                    .child(
                        Button::new("lastfm-percent-down")
                            .ghost()
                            .label("−")
                            .disabled(busy || policy.delay_percent_milli <= 300)
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.settings_draft.lastfm_scrobble_policy.delay_percent_milli = this
                                    .settings_draft
                                    .lastfm_scrobble_policy
                                    .delay_percent_milli
                                    .saturating_sub(50)
                                    .max(300);
                                cx.notify();
                            })),
                    )
                    .child(
                        Button::new("lastfm-percent-up")
                            .ghost()
                            .label("+")
                            .disabled(busy || policy.delay_percent_milli >= 950)
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.settings_draft.lastfm_scrobble_policy.delay_percent_milli = this
                                    .settings_draft
                                    .lastfm_scrobble_policy
                                    .delay_percent_milli
                                    .saturating_add(50)
                                    .min(950);
                                cx.notify();
                            })),
                    ),
            )
            .child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .child(div().w(px(180.)).text_sm().child(format!(
                        "Maximum delay: {} s",
                        policy.max_delay_seconds
                    )))
                    .child(
                        Button::new("lastfm-max-delay-down")
                            .ghost()
                            .label("−")
                            .disabled(busy || policy.max_delay_seconds <= 30)
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.settings_draft.lastfm_scrobble_policy.max_delay_seconds = this
                                    .settings_draft
                                    .lastfm_scrobble_policy
                                    .max_delay_seconds
                                    .saturating_sub(30)
                                    .max(30);
                                cx.notify();
                            })),
                    )
                    .child(
                        Button::new("lastfm-max-delay-up")
                            .ghost()
                            .label("+")
                            .disabled(busy || policy.max_delay_seconds >= 360)
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.settings_draft.lastfm_scrobble_policy.max_delay_seconds = this
                                    .settings_draft
                                    .lastfm_scrobble_policy
                                    .max_delay_seconds
                                    .saturating_add(30)
                                    .min(360);
                                cx.notify();
                            })),
                    )
                    .child(
                        Button::new("lastfm-policy-reset")
                            .label("Android defaults")
                            .disabled(busy)
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.settings_draft.lastfm_scrobble_policy = Default::default();
                                this.settings_error = None;
                                this.settings_notice = None;
                                cx.notify();
                            })),
                    ),
            )
            .into_any_element()
    }

    fn render_playback_parameters_settings(
        &self,
        busy: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let parameters = self.settings_draft.playback_parameters;
        v_flex()
            .gap_4()
            .max_w(px(680.))
            .rounded(cx.theme().radius_lg)
            .border_1()
            .border_color(cx.theme().border)
            .p_5()
            .child(
                v_flex()
                    .gap_1()
                    .child(div().font_semibold().child("Playback speed and pitch"))
                    .child(
                        div()
                            .text_sm()
                            .text_color(cx.theme().muted_foreground)
                            .child("Normal mode changes tempo without changing pitch and allows ±12 semitones. Varispeed links pitch to speed like tape playback. Save below to rebuild the audio chain at the current song position."),
                    ),
            )
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        Button::new("playback-mode-normal")
                            .label("Normal")
                            .selected(!parameters.varispeed)
                            .disabled(busy)
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.settings_draft.playback_parameters.varispeed = false;
                                this.settings_error = None;
                                this.settings_notice = None;
                                cx.notify();
                            })),
                    )
                    .child(
                        Button::new("playback-mode-varispeed")
                            .label("Varispeed")
                            .selected(parameters.varispeed)
                            .disabled(busy)
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.settings_draft.playback_parameters.varispeed = true;
                                this.settings_error = None;
                                this.settings_notice = None;
                                cx.notify();
                            })),
                    )
                    .child(
                        Button::new("playback-parameters-reset")
                            .ghost()
                            .label("Reset")
                            .disabled(busy || parameters == PlaybackParameters::default())
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.settings_draft.playback_parameters =
                                    PlaybackParameters::default();
                                this.settings_error = None;
                                this.settings_notice = None;
                                cx.notify();
                            })),
                    ),
            )
            .child(
                h_flex()
                    .gap_3()
                    .items_center()
                    .child(div().w(px(120.)).text_sm().child("Speed"))
                    .child(
                        Button::new("playback-speed-down")
                            .ghost()
                            .label("−")
                            .disabled(busy || parameters.tempo_milli <= MIN_PLAYBACK_RATE_MILLI)
                            .on_click(cx.listener(|this, _, _, cx| {
                                let parameters = &mut this.settings_draft.playback_parameters;
                                parameters.tempo_milli = parameters
                                    .tempo_milli
                                    .saturating_sub(PLAYBACK_RATE_STEP_MILLI)
                                    .max(MIN_PLAYBACK_RATE_MILLI);
                                this.settings_error = None;
                                this.settings_notice = None;
                                cx.notify();
                            })),
                    )
                    .child(
                        div()
                            .w(px(72.))
                            .text_center()
                            .font_medium()
                            .child(format!("{:.2}×", parameters.tempo_ratio())),
                    )
                    .child(
                        Button::new("playback-speed-up")
                            .ghost()
                            .label("+")
                            .disabled(busy || parameters.tempo_milli >= MAX_PLAYBACK_RATE_MILLI)
                            .on_click(cx.listener(|this, _, _, cx| {
                                let parameters = &mut this.settings_draft.playback_parameters;
                                parameters.tempo_milli = parameters
                                    .tempo_milli
                                    .saturating_add(PLAYBACK_RATE_STEP_MILLI)
                                    .min(MAX_PLAYBACK_RATE_MILLI);
                                this.settings_error = None;
                                this.settings_notice = None;
                                cx.notify();
                            })),
                    ),
            )
            .when(!parameters.varispeed, |card| {
                card.child(
                    h_flex()
                        .gap_3()
                        .items_center()
                        .child(div().w(px(120.)).text_sm().child("Transpose"))
                        .child(
                            Button::new("playback-transpose-down")
                                .ghost()
                                .label("−")
                                .disabled(
                                    busy
                                        || parameters.transpose_semitones
                                            <= MIN_TRANSPOSE_SEMITONES,
                                )
                                .on_click(cx.listener(|this, _, _, cx| {
                                    let parameters =
                                        &mut this.settings_draft.playback_parameters;
                                    parameters.transpose_semitones = parameters
                                        .transpose_semitones
                                        .saturating_sub(1)
                                        .max(MIN_TRANSPOSE_SEMITONES);
                                    this.settings_error = None;
                                    this.settings_notice = None;
                                    cx.notify();
                                })),
                        )
                        .child(
                            div()
                                .w(px(72.))
                                .text_center()
                                .font_medium()
                                .child(format!("{:+} st", parameters.transpose_semitones)),
                        )
                        .child(
                            Button::new("playback-transpose-up")
                                .ghost()
                                .label("+")
                                .disabled(
                                    busy
                                        || parameters.transpose_semitones
                                            >= MAX_TRANSPOSE_SEMITONES,
                                )
                                .on_click(cx.listener(|this, _, _, cx| {
                                    let parameters =
                                        &mut this.settings_draft.playback_parameters;
                                    parameters.transpose_semitones = parameters
                                        .transpose_semitones
                                        .saturating_add(1)
                                        .min(MAX_TRANSPOSE_SEMITONES);
                                    this.settings_error = None;
                                    this.settings_notice = None;
                                    cx.notify();
                                })),
                        ),
                )
            })
            .into_any_element()
    }

    fn render_discord_settings(&self, busy: bool, cx: &mut Context<Self>) -> AnyElement {
        let snapshot = self
            .discord_presence
            .as_ref()
            .map(DiscordPresenceService::snapshot);
        let status = if !self.settings.discord_rich_presence {
            "Off — no listening activity is shared.".to_owned()
        } else {
            match snapshot.as_ref().map(|snapshot| snapshot.state) {
                Some(DiscordPresenceState::Idle) => "Enabled — waiting for active playback.".into(),
                Some(DiscordPresenceState::Connecting) => {
                    "Connecting to the local Discord desktop client…".into()
                }
                Some(DiscordPresenceState::Active) => {
                    "Current listening activity was sent to the local Discord client.".into()
                }
                Some(DiscordPresenceState::Failed) => snapshot
                    .as_ref()
                    .and_then(|snapshot| snapshot.last_error.clone())
                    .unwrap_or_else(|| "Discord Rich Presence is unavailable.".into()),
                None => "Discord Rich Presence could not be initialized.".into(),
            }
        };

        v_flex()
            .gap_4()
            .max_w(px(680.))
            .rounded(cx.theme().radius_lg)
            .border_1()
            .border_color(cx.theme().border)
            .p_5()
            .child(
                v_flex()
                    .gap_1()
                    .child(div().font_semibold().child("Discord Rich Presence"))
                    .child(
                        div()
                            .text_sm()
                            .text_color(cx.theme().muted_foreground)
                            .child("Opt in to publish the current title, artists, artwork, playback timer, and two public links to the Discord desktop client on this computer. Metrolist uses local IPC without a Discord token, OAuth login, or Gateway connection."),
                    ),
            )
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        Button::new("discord-rich-presence-on")
                            .label("Share activity")
                            .selected(self.settings_draft.discord_rich_presence)
                            .disabled(busy || self.discord_presence.is_none())
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.settings_draft.discord_rich_presence = true;
                                this.settings_error = None;
                                this.settings_notice = None;
                                cx.notify();
                            })),
                    )
                    .child(
                        Button::new("discord-rich-presence-off")
                            .label("Keep private")
                            .selected(!self.settings_draft.discord_rich_presence)
                            .disabled(busy)
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.settings_draft.discord_rich_presence = false;
                                this.settings_error = None;
                                this.settings_notice = None;
                                cx.notify();
                            })),
                    ),
            )
            .child(
                div()
                    .text_sm()
                    .text_color(match snapshot.as_ref().map(|snapshot| snapshot.state) {
                        Some(DiscordPresenceState::Active) => cx.theme().success,
                        Some(DiscordPresenceState::Failed) => cx.theme().warning,
                        _ => cx.theme().muted_foreground,
                    })
                    .child(status),
            )
            .when_some(self.discord_warning.clone(), |card, warning| {
                card.child(
                    div()
                        .rounded(cx.theme().radius)
                        .bg(cx.theme().warning.opacity(0.12))
                        .text_color(cx.theme().warning)
                        .p_3()
                        .text_sm()
                        .child(warning),
                )
            })
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child("Pausing removes the live timer; after one minute paused, Metrolist clears the activity. If Discord is unavailable, playback continues normally and the connection is retried later."),
            )
            .into_any_element()
    }

    fn render_listen_together_settings(&self, busy: bool, cx: &mut Context<Self>) -> AnyElement {
        let snapshot = &self.listen_together_snapshot;
        let in_room = snapshot.room.is_some();
        let connecting = matches!(
            snapshot.connection,
            ListenTogetherConnectionState::Connecting
                | ListenTogetherConnectionState::Reconnecting { .. }
        );
        let connected = snapshot.connection == ListenTogetherConnectionState::Connected;
        let unavailable = self.listen_together.is_none();
        let username_empty = self
            .listen_together_username_input
            .read(cx)
            .value()
            .trim()
            .is_empty();
        let room_code_valid = {
            let value = self.listen_together_room_code_input.read(cx).value();
            value.len() == 8 && value.bytes().all(|byte| byte.is_ascii_alphanumeric())
        };
        let status = match snapshot.connection {
            ListenTogetherConnectionState::Disconnected => "Disconnected".to_owned(),
            ListenTogetherConnectionState::Connecting => "Connecting…".into(),
            ListenTogetherConnectionState::Connected => "Connected".into(),
            ListenTogetherConnectionState::Reconnecting {
                attempt,
                max_attempts,
            } => format!("Reconnecting ({attempt}/{max_attempts})…"),
            ListenTogetherConnectionState::Error => snapshot
                .last_error
                .clone()
                .unwrap_or_else(|| "Connection failed".into()),
        };
        let status_color = match snapshot.connection {
            ListenTogetherConnectionState::Connected => cx.theme().success,
            ListenTogetherConnectionState::Error => cx.theme().danger,
            ListenTogetherConnectionState::Connecting
            | ListenTogetherConnectionState::Reconnecting { .. } => cx.theme().warning,
            ListenTogetherConnectionState::Disconnected => cx.theme().muted_foreground,
        };

        v_flex()
            .gap_4()
            .max_w(px(680.))
            .rounded(cx.theme().radius_lg)
            .border_1()
            .border_color(cx.theme().border)
            .p_5()
            .child(
                v_flex()
                    .gap_1()
                    .child(div().font_semibold().child("Listen Together"))
                    .child(
                        div()
                            .text_sm()
                            .text_color(cx.theme().muted_foreground)
                            .child("Create or join an eight-character room and keep playback, queue, position, and optionally volume synchronized. The selected room server receives song metadata and timing, but never YouTube cookies, playback URLs, cache contents, or audio bytes."),
                    ),
            )
            .child(
                v_flex()
                    .gap_1()
                    .child(div().text_sm().child("Room server"))
                    .child(
                        Input::new(&self.listen_together_server_input)
                            .content_type(InputContentType::Url)
                            .disabled(busy || in_room),
                    ),
            )
            .child(
                h_flex()
                    .gap_3()
                    .child(
                        v_flex()
                            .flex_1()
                            .gap_1()
                            .child(div().text_sm().child("Display name"))
                            .child(
                                Input::new(&self.listen_together_username_input)
                                    .content_type(InputContentType::Username)
                                    .disabled(busy || in_room),
                            ),
                    )
                    .child(
                        v_flex()
                            .flex_1()
                            .gap_1()
                            .child(div().text_sm().child("Room code"))
                            .child(
                                Input::new(&self.listen_together_room_code_input)
                                    .disabled(busy || in_room),
                            ),
                    ),
            )
            .child(
                h_flex()
                    .gap_2()
                    .flex_wrap()
                    .when(!in_room, |row| {
                        row.child(
                            Button::new("listen-together-create")
                                .primary()
                                .label(if connecting { "Connecting…" } else { "Create room" })
                                .disabled(busy || unavailable || connecting || username_empty)
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.create_listen_together_room(cx)
                                })),
                        )
                        .child(
                            Button::new("listen-together-join")
                                .label(if connecting { "Connecting…" } else { "Join room" })
                                .disabled(
                                    busy
                                        || unavailable
                                        || connecting
                                        || username_empty
                                        || !room_code_valid,
                                )
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.join_listen_together_room(cx)
                                })),
                        )
                    })
                    .when(!in_room && !connected, |row| {
                        row.child(
                            Button::new("listen-together-connect")
                                .ghost()
                                .label("Test connection")
                                .disabled(busy || unavailable || connecting)
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.connect_listen_together(cx)
                                })),
                        )
                    })
                    .when(!in_room && connected, |row| {
                        row.child(
                            Button::new("listen-together-disconnect")
                                .ghost()
                                .label("Disconnect")
                                .disabled(busy)
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.disconnect_listen_together(cx)
                                })),
                        )
                    })
                    .when(in_room, |row| {
                        row.child(
                            Button::new("listen-together-leave")
                                .danger()
                                .label("Leave room")
                                .disabled(busy)
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.leave_listen_together_room(cx)
                                })),
                        )
                    }),
            )
            .child(div().text_sm().text_color(status_color).child(status))
            .when_some(snapshot.room.clone(), |card, room| {
                let local_user_id = snapshot.user_id.clone();
                let is_host = snapshot.role == ListenTogetherRoomRole::Host;
                card.child(
                    v_flex()
                        .gap_3()
                        .rounded(cx.theme().radius)
                        .bg(cx.theme().secondary.opacity(0.4))
                        .p_4()
                        .child(
                            h_flex()
                                .justify_between()
                                .child(
                                    v_flex()
                                        .child(
                                            div()
                                                .text_xs()
                                                .text_color(cx.theme().muted_foreground)
                                                .child("Room code"),
                                        )
                                        .child(
                                            div()
                                                .text_lg()
                                                .font_semibold()
                                                .child(room.room_code.clone()),
                                        ),
                                )
                                .child(
                                    div().text_sm().child(if is_host {
                                        "You are the host"
                                    } else {
                                        "Host controls playback"
                                    }),
                                ),
                        )
                        .child(
                            v_flex()
                                .gap_2()
                                .child(div().text_sm().font_medium().child(format!(
                                    "Participants ({})",
                                    room.users.len()
                                )))
                                .children(room.users.into_iter().enumerate().map(|(index, user)| {
                                    let kick_user_id = user.user_id.clone();
                                    let transfer_user_id = user.user_id.clone();
                                    let can_manage = is_host
                                        && local_user_id.as_deref() != Some(user.user_id.as_str());
                                    h_flex()
                                        .justify_between()
                                        .child(div().text_sm().child(format!(
                                            "{}{}{}",
                                            user.username,
                                            if user.is_host { " · host" } else { "" },
                                            if user.is_connected { "" } else { " · offline" },
                                        )))
                                        .when(can_manage, |row| {
                                            row.child(
                                                h_flex()
                                                    .gap_2()
                                                    .child(
                                                        Button::new(format!(
                                                            "listen-together-transfer-{index}"
                                                        ))
                                                        .ghost()
                                                        .label("Make host")
                                                        .on_click(cx.listener(
                                                            move |this, _, _, cx| {
                                                                let result = this
                                                                    .listen_together
                                                                    .as_ref()
                                                                    .ok_or_else(|| {
                                                                        AppError::ListenTogether(
                                                                            "connection worker is unavailable"
                                                                                .into(),
                                                                        )
                                                                    })
                                                                    .and_then(|client| {
                                                                        client.transfer_host(
                                                                            transfer_user_id
                                                                                .clone(),
                                                                        )
                                                                    });
                                                                if let Err(error) = result {
                                                                    this.listen_together_error =
                                                                        Some(error.to_string());
                                                                }
                                                                cx.notify();
                                                            },
                                                        )),
                                                    )
                                                    .child(
                                                        Button::new(format!(
                                                            "listen-together-kick-{index}"
                                                        ))
                                                        .ghost()
                                                        .label("Remove")
                                                        .on_click(cx.listener(
                                                            move |this, _, _, cx| {
                                                                let result = this
                                                                    .listen_together
                                                                    .as_ref()
                                                                    .ok_or_else(|| {
                                                                        AppError::ListenTogether(
                                                                            "connection worker is unavailable"
                                                                                .into(),
                                                                        )
                                                                    })
                                                                    .and_then(|client| {
                                                                        client.kick_user(
                                                                            kick_user_id.clone(),
                                                                            Some(
                                                                                "Removed by host"
                                                                                    .into(),
                                                                            ),
                                                                        )
                                                                    });
                                                                if let Err(error) = result {
                                                                    this.listen_together_error =
                                                                        Some(error.to_string());
                                                                }
                                                                cx.notify();
                                                            },
                                                        )),
                                                    ),
                                            )
                                        })
                                })),
                        ),
                )
            })
            .when(
                snapshot.role == ListenTogetherRoomRole::Guest && self.current_song.is_some(),
                |card| {
                    let song = self.current_song.clone().expect("song was checked");
                    card.child(
                        Button::new("listen-together-suggest-current")
                            .label("Suggest current track to host")
                            .disabled(busy)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                let track = ListenTogetherTrack::from_song(&song);
                                let result = this
                                    .listen_together
                                    .as_ref()
                                    .ok_or_else(|| {
                                        AppError::ListenTogether(
                                            "connection worker is unavailable".into(),
                                        )
                                    })
                                    .and_then(|client| client.suggest_track(track));
                                match result {
                                    Ok(()) => {
                                        this.listen_together_notice =
                                            Some("Track suggestion sent to the host.".into());
                                        this.listen_together_error = None;
                                    }
                                    Err(error) => {
                                        this.listen_together_error = Some(error.to_string());
                                    }
                                }
                                cx.notify();
                            })),
                    )
                },
            )
            .when(
                snapshot.role == ListenTogetherRoomRole::Host
                    && !snapshot.pending_join_requests.is_empty(),
                |card| {
                    card.child(
                        v_flex()
                            .gap_2()
                            .child(div().text_sm().font_medium().child("Join requests"))
                            .children(snapshot.pending_join_requests.clone().into_iter().map(
                                |request| {
                                    let approve_id = request.user_id.clone();
                                    let reject_id = request.user_id.clone();
                                    h_flex()
                                        .justify_between()
                                        .child(div().text_sm().child(request.username))
                                        .child(
                                            h_flex()
                                                .gap_2()
                                                .child(
                                                    Button::new(format!(
                                                        "listen-together-approve-{approve_id}"
                                                    ))
                                                    .label("Approve")
                                                    .on_click(cx.listener(
                                                        move |this, _, _, cx| {
                                                            let result = this
                                                                .listen_together
                                                                .as_ref()
                                                                .ok_or_else(|| {
                                                                    AppError::ListenTogether(
                                                                        "connection worker is unavailable"
                                                                            .into(),
                                                                    )
                                                                })
                                                                .and_then(|client| {
                                                                    client.approve_join(
                                                                        approve_id.clone(),
                                                                    )
                                                                });
                                                            if let Err(error) = result {
                                                                this.listen_together_error =
                                                                    Some(error.to_string());
                                                            }
                                                            cx.notify();
                                                        },
                                                    )),
                                                )
                                                .child(
                                                    Button::new(format!(
                                                        "listen-together-reject-{reject_id}"
                                                    ))
                                                    .ghost()
                                                    .label("Reject")
                                                    .on_click(cx.listener(
                                                        move |this, _, _, cx| {
                                                            let result = this
                                                                .listen_together
                                                                .as_ref()
                                                                .ok_or_else(|| {
                                                                    AppError::ListenTogether(
                                                                        "connection worker is unavailable"
                                                                            .into(),
                                                                    )
                                                                })
                                                                .and_then(|client| {
                                                                    client.reject_join(
                                                                        reject_id.clone(),
                                                                        None,
                                                                    )
                                                                });
                                                            if let Err(error) = result {
                                                                this.listen_together_error =
                                                                    Some(error.to_string());
                                                            }
                                                            cx.notify();
                                                        },
                                                    )),
                                                ),
                                        )
                                },
                            )),
                    )
                },
            )
            .when(
                snapshot.role == ListenTogetherRoomRole::Host
                    && !snapshot.pending_suggestions.is_empty(),
                |card| {
                    card.child(
                        v_flex()
                            .gap_2()
                            .child(div().text_sm().font_medium().child("Track suggestions"))
                            .children(
                                snapshot
                                    .pending_suggestions
                                    .clone()
                                    .into_iter()
                                    .enumerate()
                                    .map(|(index, suggestion)| {
                                        let approve_id = suggestion.suggestion_id.clone();
                                        let reject_id = suggestion.suggestion_id.clone();
                                        h_flex()
                                            .justify_between()
                                            .child(div().text_sm().child(format!(
                                                "{} — {} · {}",
                                                suggestion.track.title,
                                                suggestion.track.artist,
                                                suggestion.from_username,
                                            )))
                                            .child(
                                                h_flex()
                                                    .gap_2()
                                                    .child(
                                                        Button::new(format!(
                                                            "listen-together-approve-suggestion-{index}"
                                                        ))
                                                        .label("Approve")
                                                        .on_click(cx.listener(
                                                            move |this, _, _, cx| {
                                                                let result = this
                                                                    .listen_together
                                                                    .as_ref()
                                                                    .ok_or_else(|| {
                                                                        AppError::ListenTogether(
                                                                            "connection worker is unavailable"
                                                                                .into(),
                                                                        )
                                                                    })
                                                                    .and_then(|client| {
                                                                        client.approve_suggestion(
                                                                            approve_id.clone(),
                                                                        )
                                                                    });
                                                                if let Err(error) = result {
                                                                    this.listen_together_error =
                                                                        Some(error.to_string());
                                                                }
                                                                cx.notify();
                                                            },
                                                        )),
                                                    )
                                                    .child(
                                                        Button::new(format!(
                                                            "listen-together-reject-suggestion-{index}"
                                                        ))
                                                        .ghost()
                                                        .label("Reject")
                                                        .on_click(cx.listener(
                                                            move |this, _, _, cx| {
                                                                let result = this
                                                                    .listen_together
                                                                    .as_ref()
                                                                    .ok_or_else(|| {
                                                                        AppError::ListenTogether(
                                                                            "connection worker is unavailable"
                                                                                .into(),
                                                                        )
                                                                    })
                                                                    .and_then(|client| {
                                                                        client.reject_suggestion(
                                                                            reject_id.clone(),
                                                                            None,
                                                                        )
                                                                    });
                                                                if let Err(error) = result {
                                                                    this.listen_together_error =
                                                                        Some(error.to_string());
                                                                }
                                                                cx.notify();
                                                            },
                                                        )),
                                                    ),
                                            )
                                    }),
                            ),
                    )
                },
            )
            .child(
                h_flex()
                    .gap_2()
                    .flex_wrap()
                    .child(
                        Button::new("listen-together-auto-joins")
                            .label("Auto-approve joins")
                            .selected(self.settings_draft.listen_together.auto_approve_joins)
                            .disabled(busy)
                            .on_click(cx.listener(|this, _, _, cx| {
                                let value =
                                    &mut this.settings_draft.listen_together.auto_approve_joins;
                                *value = !*value;
                                this.settings_error = None;
                                cx.notify();
                            })),
                    )
                    .child(
                        Button::new("listen-together-auto-suggestions")
                            .label("Auto-approve suggestions")
                            .selected(
                                self.settings_draft
                                    .listen_together
                                    .auto_approve_suggestions,
                            )
                            .disabled(busy)
                            .on_click(cx.listener(|this, _, _, cx| {
                                let value = &mut this
                                    .settings_draft
                                    .listen_together
                                    .auto_approve_suggestions;
                                *value = !*value;
                                this.settings_error = None;
                                cx.notify();
                            })),
                    )
                    .child(
                        Button::new("listen-together-sync-volume")
                            .label("Sync host volume")
                            .selected(self.settings_draft.listen_together.sync_host_volume)
                            .disabled(busy)
                            .on_click(cx.listener(|this, _, _, cx| {
                                let value =
                                    &mut this.settings_draft.listen_together.sync_host_volume;
                                *value = !*value;
                                this.settings_error = None;
                                cx.notify();
                            })),
                    ),
            )
            .when_some(self.listen_together_notice.clone(), |card, notice| {
                card.child(
                    div()
                        .rounded(cx.theme().radius)
                        .bg(cx.theme().success.opacity(0.1))
                        .text_color(cx.theme().success)
                        .p_3()
                        .text_sm()
                        .child(notice),
                )
            })
            .when_some(self.listen_together_error.clone(), |card, error| {
                card.child(
                    div()
                        .rounded(cx.theme().radius)
                        .bg(cx.theme().danger.opacity(0.1))
                        .text_color(cx.theme().danger)
                        .p_3()
                        .text_sm()
                        .child(error),
                )
            })
            .when_some(self.listen_together_warning.clone(), |card, warning| {
                card.child(
                    div()
                        .rounded(cx.theme().radius)
                        .bg(cx.theme().warning.opacity(0.12))
                        .text_color(cx.theme().warning)
                        .p_3()
                        .text_sm()
                        .child(warning),
                )
            })
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child("Room session tokens remain only in memory and are used for bounded reconnects; they are not written to SQLite. Create/join connects automatically, so Test connection is optional."),
            )
            .into_any_element()
    }

    fn render_auto_eq_wizard(&self, busy: bool, cx: &mut Context<Self>) -> AnyElement {
        let content = match &self.autoeq_database_state {
            AutoEqDatabaseState::NotLoaded { cached } => v_flex()
                .gap_2()
                .child(
                    div()
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .child(if *cached {
                            "A local AutoEQ index is available. Load it to search headphone models."
                        } else {
                            "Download the AutoEQ GitHub index (about 17 MB) to search thousands of headphone measurements."
                        }),
                )
                .child(
                    Button::new("autoeq-database-download")
                        .icon(IconName::FolderOpen)
                        .label(if *cached {
                            "Load AutoEQ database"
                        } else {
                            "Download AutoEQ database"
                        })
                        .disabled(busy)
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.download_auto_eq_database(cx)
                        })),
                )
                .into_any_element(),
            AutoEqDatabaseState::Loading => h_flex()
                .gap_2()
                .text_sm()
                .text_color(cx.theme().muted_foreground)
                .child(Icon::new(IconName::LoaderCircle))
                .child("Loading and indexing the AutoEQ database…")
                .into_any_element(),
            AutoEqDatabaseState::Failed { message, cached } => v_flex()
                .gap_2()
                .child(
                    div()
                        .rounded(cx.theme().radius)
                        .bg(cx.theme().danger.opacity(0.1))
                        .text_color(cx.theme().danger)
                        .p_3()
                        .text_sm()
                        .child(message.clone()),
                )
                .when(*cached, |state| {
                    state.child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child("A cache file exists; Retry will validate it and use it when the network is unavailable."),
                    )
                })
                .child(
                    Button::new("autoeq-database-retry")
                        .label("Retry")
                        .disabled(busy)
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.download_auto_eq_database(cx)
                        })),
                )
                .into_any_element(),
            AutoEqDatabaseState::Ready(index) => {
                let origin = match index.origin {
                    AutoEqIndexOrigin::Downloaded => "downloaded now",
                    AutoEqIndexOrigin::FreshCache => "fresh local cache",
                    AutoEqIndexOrigin::StaleCache => "stale cache (offline fallback)",
                };
                let revision = index.revision.chars().take(7).collect::<String>();
                let status = format!(
                    "{} measurements · revision {} · {}",
                    index.entries.len(),
                    revision,
                    origin
                );
                let body = match self.autoeq_wizard_step {
                    AutoEqWizardStep::ModelSelection => {
                        let models = if self.autoeq_models.is_empty() {
                            div()
                                .rounded(cx.theme().radius)
                                .border_1()
                                .border_color(cx.theme().border)
                                .p_3()
                                .text_sm()
                                .text_color(cx.theme().muted_foreground)
                                .child("No matching models. Try a shorter model name.")
                                .into_any_element()
                        } else {
                            v_flex()
                                .max_h(px(300.))
                                .overflow_y_scrollbar()
                                .gap_1()
                                .children(self.autoeq_models.iter().cloned().enumerate().map(
                                    |(model_index, model)| {
                                        let selected_model = model.clone();
                                        Button::new(format!("autoeq-model-{model_index}"))
                                            .w_full()
                                            .justify_start()
                                            .label(format!(
                                                "{} · {} variant{}",
                                                model.name,
                                                model.variants.len(),
                                                if model.variants.len() == 1 { "" } else { "s" }
                                            ))
                                            .disabled(busy)
                                            .on_click(cx.listener(move |this, _, _, cx| {
                                                this.select_auto_eq_model(
                                                    selected_model.clone(),
                                                    cx,
                                                )
                                            }))
                                    },
                                ))
                                .into_any_element()
                        };
                        v_flex()
                            .gap_2()
                            .child(Input::new(&self.autoeq_search_input).disabled(busy))
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(format!(
                                        "Showing up to {MAX_AUTO_EQ_SEARCH_RESULTS} model names; exact and prefix matches appear first."
                                    )),
                            )
                            .child(models)
                            .into_any_element()
                    }
                    AutoEqWizardStep::VariantSelection => {
                        let selected_count = self.autoeq_selected_variant_paths.len();
                        v_flex()
                            .gap_3()
                            .child(
                                h_flex()
                                    .gap_2()
                                    .items_center()
                                    .child(
                                        Button::new("autoeq-model-back")
                                            .ghost()
                                            .label("Back to models")
                                            .disabled(busy)
                                            .on_click(cx.listener(|this, _, _, cx| {
                                                this.back_to_auto_eq_models(cx)
                                            })),
                                    )
                                    .child(
                                        div().font_medium().child(
                                            self.autoeq_selected_model
                                                .clone()
                                                .unwrap_or_else(|| "Selected model".into()),
                                        ),
                                    ),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .child("Choose one or more measurement variants. Unknown rigs are resolved from the source name index when available."),
                            )
                            .child(
                                v_flex()
                                    .max_h(px(320.))
                                    .overflow_y_scrollbar()
                                    .gap_1()
                                    .children(self.autoeq_variants.iter().cloned().enumerate().map(
                                        |(variant_index, variant)| {
                                            let selected = self
                                                .autoeq_selected_variant_paths
                                                .contains(&variant.repo_path);
                                            let path = variant.repo_path.clone();
                                            let flags = [
                                                variant
                                                    .label
                                                    .to_ascii_lowercase()
                                                    .contains("anc")
                                                    .then_some("ANC"),
                                                (variant
                                                    .label
                                                    .to_ascii_lowercase()
                                                    .contains("velour")
                                                    || (variant
                                                        .label
                                                        .to_ascii_lowercase()
                                                        .contains("pad")
                                                        && !variant
                                                            .label
                                                            .to_ascii_lowercase()
                                                            .contains("sample")))
                                                .then_some("pad variant"),
                                            ]
                                            .into_iter()
                                            .flatten()
                                            .collect::<Vec<_>>()
                                            .join(", ");
                                            let flag_suffix = if flags.is_empty() {
                                                String::new()
                                            } else {
                                                format!(" · {flags}")
                                            };
                                            Button::new(format!(
                                                "autoeq-variant-{variant_index}"
                                            ))
                                            .w_full()
                                            .justify_start()
                                            .selected(selected)
                                            .label(format!(
                                                "{} · {} · {} · {}{}",
                                                variant.label,
                                                variant.source,
                                                variant.rig,
                                                variant.form,
                                                flag_suffix
                                            ))
                                            .disabled(busy)
                                            .on_click(cx.listener(move |this, _, _, cx| {
                                                this.toggle_auto_eq_variant(path.clone(), cx)
                                            }))
                                        },
                                    )),
                            )
                            .child(
                                Button::new("autoeq-save-selected")
                                    .primary()
                                    .label(if self.equalizer_operation
                                        == EqualizerOperation::SavingAutoEq
                                    {
                                        "Downloading and saving…".into()
                                    } else {
                                        format!("Save selected profiles ({selected_count})")
                                    })
                                    .disabled(busy || selected_count == 0)
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.save_selected_auto_eq_profiles(cx)
                                    })),
                            )
                            .into_any_element()
                    }
                };
                v_flex()
                    .gap_2()
                    .child(
                        div()
                            .text_xs()
                            .text_color(if index.origin == AutoEqIndexOrigin::StaleCache {
                                cx.theme().warning
                            } else {
                                cx.theme().muted_foreground
                            })
                            .child(status),
                    )
                    .when(index.truncated, |state| {
                        state.child(
                            div()
                                .rounded(cx.theme().radius)
                                .bg(cx.theme().warning.opacity(0.1))
                                .text_color(cx.theme().warning)
                                .p_2()
                                .text_xs()
                                .child("GitHub marked this tree response as truncated, so some models may be absent."),
                        )
                    })
                    .child(body)
                    .into_any_element()
            }
        };

        v_flex()
            .gap_2()
            .rounded(cx.theme().radius)
            .border_1()
            .border_color(cx.theme().border)
            .p_3()
            .child(div().font_medium().child("Online AutoEQ database"))
            .child(content)
            .into_any_element()
    }

    fn render_equalizer_frequency_response(&self, cx: &mut Context<Self>) -> AnyElement {
        let active_profile = self
            .settings
            .equalizer
            .enabled
            .then_some(self.settings.equalizer.active_profile.as_ref())
            .flatten();
        let response = active_profile
            .and_then(|profile| equalizer_frequency_response(&profile.equalizer).ok());
        let title = active_profile
            .map(|profile| profile.name.clone())
            .unwrap_or_else(|| "No equalization".into());
        let (points, db_top, db_bottom, db_step) = response.map_or_else(
            || {
                (
                    vec![
                        (EQUALIZER_RESPONSE_MIN_FREQUENCY_HZ, 0.0),
                        (EQUALIZER_RESPONSE_MAX_FREQUENCY_HZ, 0.0),
                    ],
                    2.5,
                    -2.5,
                    2.5,
                )
            },
            |response: EqualizerFrequencyResponse| {
                (
                    response
                        .points
                        .into_iter()
                        .map(|point| (point.frequency_hz, point.gain_db))
                        .collect(),
                    response.db_top,
                    response.db_bottom,
                    response.db_step,
                )
            },
        );
        let grid_color = cx.theme().border.opacity(0.55);
        let zero_color = cx.theme().primary.opacity(0.5);
        let curve_color = cx.theme().primary;
        let fill_color = cx.theme().primary.opacity(0.12);
        let graph = canvas(
            move |_, _, _| (),
            move |bounds, _, window, _| {
                let left = bounds.left() + px(8.);
                let right = bounds.right() - px(8.);
                let top = bounds.top() + px(8.);
                let bottom = bounds.bottom() - px(8.);
                let width = (right - left).as_f32().max(1.0);
                let height = (bottom - top).as_f32().max(1.0);
                let db_range = (db_top - db_bottom).max(0.001);
                let frequency_x = |frequency_hz: f64| {
                    let fraction = ((frequency_hz.log10()
                        - EQUALIZER_RESPONSE_MIN_FREQUENCY_HZ.log10())
                        / (EQUALIZER_RESPONSE_MAX_FREQUENCY_HZ.log10()
                            - EQUALIZER_RESPONSE_MIN_FREQUENCY_HZ.log10()))
                    .clamp(0.0, 1.0);
                    left + px(width * fraction as f32)
                };
                let gain_y = |gain_db: f64| {
                    let fraction = ((db_top - gain_db) / db_range).clamp(0.0, 1.0);
                    top + px(height * fraction as f32)
                };

                let mut grid = PathBuilder::stroke(px(0.5));
                let mut db = db_bottom;
                while db <= db_top + 0.001 {
                    if db.abs() >= 0.001 {
                        let y = gain_y(db);
                        grid.move_to(point(left, y));
                        grid.line_to(point(right, y));
                    }
                    db += db_step;
                }
                for frequency in [100.0, 1_000.0, 10_000.0] {
                    let x = frequency_x(frequency);
                    grid.move_to(point(x, top));
                    grid.line_to(point(x, bottom));
                }
                if let Ok(path) = grid.build() {
                    window.paint_path(path, grid_color);
                }
                let zero_y = gain_y(0.0);
                let mut zero = PathBuilder::stroke(px(1.));
                zero.move_to(point(left, zero_y));
                zero.line_to(point(right, zero_y));
                if let Ok(path) = zero.build() {
                    window.paint_path(path, zero_color);
                }

                if let (Some(first), Some(last)) = (points.first(), points.last()) {
                    let mut fill = PathBuilder::fill();
                    fill.move_to(point(frequency_x(first.0), bottom));
                    for (frequency_hz, gain_db) in &points {
                        fill.line_to(point(frequency_x(*frequency_hz), gain_y(*gain_db)));
                    }
                    fill.line_to(point(frequency_x(last.0), bottom));
                    fill.close();
                    if let Ok(path) = fill.build() {
                        window.paint_path(path, fill_color);
                    }
                    let mut curve = PathBuilder::stroke(px(2.));
                    for (index, (frequency_hz, gain_db)) in points.iter().enumerate() {
                        let graph_point = point(frequency_x(*frequency_hz), gain_y(*gain_db));
                        if index == 0 {
                            curve.move_to(graph_point);
                        } else {
                            curve.line_to(graph_point);
                        }
                    }
                    if let Ok(path) = curve.build() {
                        window.paint_path(path, curve_color);
                    }
                }
            },
        )
        .w_full()
        .h(px(170.));

        v_flex()
            .gap_2()
            .rounded(cx.theme().radius)
            .bg(cx.theme().muted.opacity(0.25))
            .p_3()
            .child(
                h_flex()
                    .justify_between()
                    .child(
                        div()
                            .text_sm()
                            .font_medium()
                            .child(format!("Frequency response · {title}")),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(format!("{db_bottom:+.1} to {db_top:+.1} dB · 48 kHz")),
                    ),
            )
            .child(graph)
            .child(
                h_flex()
                    .justify_between()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child("20 Hz")
                    .child("100 Hz")
                    .child("1 kHz")
                    .child("10 kHz")
                    .child("20 kHz"),
            )
            .into_any_element()
    }

    fn render_equalizer_profiles(&self, busy: bool, cx: &mut Context<Self>) -> AnyElement {
        let active_profile_id = self
            .settings
            .equalizer
            .enabled
            .then(|| {
                self.settings
                    .equalizer
                    .active_profile
                    .as_ref()
                    .map(|profile| profile.id.as_str())
            })
            .flatten();
        let operation = match self.equalizer_operation {
            EqualizerOperation::Idle => None,
            EqualizerOperation::Importing => Some("Reading equalizer profiles…"),
            EqualizerOperation::LoadingDatabase => Some("Loading the AutoEQ database…"),
            EqualizerOperation::LoadingVariants => Some("Resolving AutoEQ measurement variants…"),
            EqualizerOperation::SavingAutoEq => {
                Some("Downloading and saving selected AutoEQ profiles…")
            }
            EqualizerOperation::Applying => Some("Rebuilding the audio chain…"),
            EqualizerOperation::Deleting => Some("Deleting equalizer profile…"),
        };
        let profiles = match &self.equalizer_profiles {
            StoredViewState::Loading => div()
                .text_sm()
                .text_color(cx.theme().muted_foreground)
                .child("Loading saved profiles…")
                .into_any_element(),
            StoredViewState::Failed(error) => v_flex()
                .gap_2()
                .child(
                    div()
                        .text_sm()
                        .text_color(cx.theme().danger)
                        .child(error.clone()),
                )
                .child(
                    Button::new("equalizer-profiles-retry")
                        .label("Retry")
                        .disabled(busy)
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.reload_equalizer_profiles(cx)
                        })),
                )
                .into_any_element(),
            StoredViewState::Loaded(profiles) if profiles.is_empty() => div()
                .rounded(cx.theme().radius)
                .border_1()
                .border_color(cx.theme().border)
                .p_3()
                .text_sm()
                .text_color(cx.theme().muted_foreground)
                .child("No imported profiles yet. AutoEQ ParametricEQ.txt and Equalizer APO text files are supported.")
                .into_any_element(),
            StoredViewState::Loaded(profiles) => v_flex()
                .gap_2()
                .children(profiles.iter().cloned().enumerate().map(|(index, profile)| {
                    let selected = active_profile_id == Some(profile.id.as_str());
                    let confirming = self.equalizer_delete_confirmation.as_deref()
                        == Some(profile.id.as_str());
                    let apply_profile = profile.clone();
                    let begin_delete_id = profile.id.clone();
                    let confirm_delete_id = profile.id.clone();
                    let type_counts = profile.equalizer.bands.iter().fold(
                        [0_usize; 3],
                        |mut counts, band| {
                            let index = match band.filter_type {
                                crate::ParametricFilterType::Peaking => 0,
                                crate::ParametricFilterType::LowShelf => 1,
                                crate::ParametricFilterType::HighShelf => 2,
                            };
                            counts[index] += 1;
                            counts
                        },
                    );
                    v_flex()
                        .gap_2()
                        .rounded(cx.theme().radius)
                        .border_1()
                        .border_color(if selected {
                            cx.theme().primary
                        } else {
                            cx.theme().border
                        })
                        .p_3()
                        .child(
                            h_flex()
                                .gap_3()
                                .justify_between()
                                .child(
                                    v_flex()
                                        .min_w_0()
                                        .gap_1()
                                        .child(div().font_medium().child(profile.device_model))
                                        .child(
                                            div()
                                                .text_xs()
                                                .text_color(cx.theme().muted_foreground)
                                                .child(format!(
                                                    "{} · {} · {} bands · preamp {:+.2} dB · PK {} / LSC {} / HSC {}",
                                                    profile.source,
                                                    profile.rig,
                                                    profile.equalizer.bands.len(),
                                                    f32::from(profile.equalizer.preamp_mb) / 100.0,
                                                    type_counts[0],
                                                    type_counts[1],
                                                    type_counts[2],
                                                )),
                                        ),
                                )
                                .child(
                                    h_flex()
                                        .gap_2()
                                        .child(
                                            Button::new(format!(
                                                "equalizer-profile-select-{index}"
                                            ))
                                            .label(if selected { "Active" } else { "Use" })
                                            .selected(selected)
                                            .disabled(busy || selected)
                                            .on_click(cx.listener(move |this, _, _, cx| {
                                                this.request_equalizer_profile_change(
                                                    Some(apply_profile.clone()),
                                                    cx,
                                                )
                                            })),
                                        )
                                        .child(
                                            Button::new(format!(
                                                "equalizer-profile-delete-{index}"
                                            ))
                                            .label("Delete")
                                            .danger()
                                            .disabled(busy || selected)
                                            .on_click(cx.listener(move |this, _, _, cx| {
                                                this.begin_equalizer_profile_delete(
                                                    begin_delete_id.clone(),
                                                    cx,
                                                )
                                            })),
                                        ),
                                ),
                        )
                        .when(confirming, |row| {
                            row.child(
                                h_flex()
                                    .gap_2()
                                    .child(
                                        div()
                                            .flex_1()
                                            .text_xs()
                                            .text_color(cx.theme().warning)
                                            .child("Delete this saved profile permanently?"),
                                    )
                                    .child(
                                        Button::new(format!(
                                            "equalizer-profile-delete-confirm-{index}"
                                        ))
                                        .label("Confirm delete")
                                        .danger()
                                        .disabled(busy)
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            this.delete_equalizer_profile(
                                                confirm_delete_id.clone(),
                                                cx,
                                            )
                                        })),
                                    )
                                    .child(
                                        Button::new(format!(
                                            "equalizer-profile-delete-cancel-{index}"
                                        ))
                                        .label("Cancel")
                                        .ghost()
                                        .disabled(busy)
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            this.equalizer_delete_confirmation = None;
                                            cx.notify();
                                        })),
                                    ),
                            )
                        })
                }))
                .into_any_element(),
        };

        v_flex()
            .gap_3()
            .rounded(cx.theme().radius)
            .bg(cx.theme().muted.opacity(0.2))
            .p_3()
            .child(
                h_flex()
                    .gap_3()
                    .justify_between()
                    .child(
                        v_flex()
                            .gap_1()
                            .child(div().font_medium().child("AutoEQ / Equalizer APO"))
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .child("Import up to 20 enabled PK, LSC, and HSC filters. Profile selection applies and saves immediately."),
                            ),
                    )
                    .child(
                        Button::new("equalizer-profile-import")
                            .icon(IconName::FolderOpen)
                            .label("Import file")
                            .disabled(busy)
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.import_equalizer_profile(window, cx)
                            })),
                    ),
            )
            .child(self.render_equalizer_frequency_response(cx))
            .child(self.render_auto_eq_wizard(busy, cx))
            .child(
                Button::new("equalizer-profile-disable")
                    .label("No equalization")
                    .selected(!self.settings.equalizer.enabled)
                    .disabled(busy || !self.settings.equalizer.enabled)
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.request_equalizer_profile_change(None, cx)
                    })),
            )
            .when_some(operation, |card, operation| {
                card.child(
                    div()
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .child(operation),
                )
            })
            .child(profiles)
            .when_some(self.equalizer_notice.clone(), |card, notice| {
                card.child(
                    div()
                        .rounded(cx.theme().radius)
                        .bg(cx.theme().success.opacity(0.1))
                        .text_color(cx.theme().success)
                        .p_3()
                        .text_sm()
                        .child(notice),
                )
            })
            .when_some(self.equalizer_error.clone(), |card, error| {
                card.child(
                    div()
                        .rounded(cx.theme().radius)
                        .bg(cx.theme().danger.opacity(0.1))
                        .text_color(cx.theme().danger)
                        .p_3()
                        .text_sm()
                        .child(error),
                )
            })
            .into_any_element()
    }

    fn render_settings(&self, cx: &mut Context<Self>) -> AnyElement {
        let busy = self.settings_operation == SettingsOperation::Applying
            || self.playback_parameters_pending.is_some()
            || self.equalizer_operation != EqualizerOperation::Idle
            || self.account_operation != AccountOperation::Idle
            || self.lastfm_operation != LastFmOperation::Idle
            || self.cloud_busy()
            || matches!(self.account_state, AccountViewState::Checking);
        v_flex()
            .gap_6()
            .child(self.page_heading(
                "Settings",
                "Configure appearance, network routing, stream quality, and cache storage.",
                cx,
            ))
            .child(self.render_account_settings(cx))
            .child(self.render_lastfm_settings(cx))
            .child(self.render_listen_together_settings(busy, cx))
            .child(self.render_discord_settings(busy, cx))
            .child(self.render_playback_parameters_settings(busy, cx))
            .child(
                v_flex()
                    .gap_4()
                    .max_w(px(680.))
                    .rounded(cx.theme().radius_lg)
                    .border_1()
                    .border_color(cx.theme().border)
                    .p_5()
                    .child(
                        v_flex()
                            .gap_1()
                            .child(div().font_semibold().child("Appearance"))
                            .child(
                                div()
                                    .text_sm()
                                    .text_color(cx.theme().muted_foreground)
                                    .child("Choose a light or dark theme. Save below to keep it after restart."),
                            ),
                    )
                    .child(
                        h_flex()
                            .gap_2()
                            .child(
                                Button::new("theme-light")
                                    .icon(IconName::Sun)
                                    .label("Light")
                                    .selected(self.theme_mode == ThemeMode::Light)
                                    .disabled(busy)
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.theme_mode = ThemeMode::Light;
                                        this.settings_draft.theme = AppTheme::Light;
                                        this.settings_error = None;
                                        this.settings_notice = None;
                                        Theme::change(ThemeMode::Light, Some(window), cx);
                                        cx.notify();
                                    })),
                            )
                            .child(
                                Button::new("theme-dark")
                                    .icon(IconName::Moon)
                                    .label("Dark")
                                    .selected(self.theme_mode == ThemeMode::Dark)
                                    .disabled(busy)
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.theme_mode = ThemeMode::Dark;
                                        this.settings_draft.theme = AppTheme::Dark;
                                        this.settings_error = None;
                                        this.settings_notice = None;
                                        Theme::change(ThemeMode::Dark, Some(window), cx);
                                        cx.notify();
                                    })),
                            ),
                    ),
            )
            .child(self.render_audio_outputs(cx))
            .child(
                v_flex()
                    .gap_4()
                    .max_w(px(680.))
                    .rounded(cx.theme().radius_lg)
                    .border_1()
                    .border_color(cx.theme().border)
                    .p_5()
                    .child(
                        v_flex()
                            .gap_1()
                            .child(div().font_semibold().child("Network proxy"))
                            .child(
                                div()
                                    .text_sm()
                                    .text_color(cx.theme().muted_foreground)
                                    .child("Applied consistently to YouTube Music, lyrics, artwork, and audio streams."),
                            ),
                    )
                    .child(
                        h_flex()
                            .gap_2()
                            .child(
                                Button::new("proxy-disabled")
                                    .label("Direct")
                                    .selected(!self.settings_draft.proxy.enabled)
                                    .disabled(busy)
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.settings_draft.proxy.enabled = false;
                                        this.settings_error = None;
                                        this.settings_notice = None;
                                        cx.notify();
                                    })),
                            )
                            .child(
                                Button::new("proxy-enabled")
                                    .label("Use proxy")
                                    .selected(self.settings_draft.proxy.enabled)
                                    .disabled(busy)
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.settings_draft.proxy.enabled = true;
                                        this.settings_error = None;
                                        this.settings_notice = None;
                                        cx.notify();
                                    })),
                            ),
                    )
                    .when(self.settings_draft.proxy.enabled, |card| {
                        card.child(
                            h_flex().gap_2().children(ProxyKind::ALL.into_iter().map(|kind| {
                                Button::new(format!("proxy-kind-{}", kind.label()))
                                    .label(kind.label())
                                    .selected(self.settings_draft.proxy.kind == kind)
                                    .disabled(busy)
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.settings_draft.proxy.kind = kind;
                                        this.settings_error = None;
                                        this.settings_notice = None;
                                        cx.notify();
                                    }))
                            })),
                        )
                        .child(
                            v_flex()
                                .gap_1()
                                .child(div().text_sm().child("Address"))
                                .child(
                                    Input::new(&self.proxy_address_input)
                                        .content_type(InputContentType::Url)
                                        .disabled(busy),
                                ),
                        )
                        .child(
                            h_flex()
                                .gap_3()
                                .child(
                                    v_flex()
                                        .flex_1()
                                        .gap_1()
                                        .child(div().text_sm().child("Username (optional)"))
                                        .child(
                                            Input::new(&self.proxy_username_input)
                                                .content_type(InputContentType::Username)
                                                .disabled(busy),
                                        ),
                                )
                                .child(
                                    v_flex()
                                        .flex_1()
                                        .gap_1()
                                        .child(div().text_sm().child("Password (optional)"))
                                        .child(
                                            Input::new(&self.proxy_password_input)
                                                .content_type(InputContentType::Password)
                                                .mask_toggle()
                                                .disabled(busy),
                                        ),
                                ),
                        )
                    }),
            )
            .child(
                v_flex()
                    .gap_4()
                    .max_w(px(680.))
                    .rounded(cx.theme().radius_lg)
                    .border_1()
                    .border_color(cx.theme().border)
                    .p_5()
                    .child(
                        v_flex()
                            .gap_1()
                            .child(div().font_semibold().child("Equalizer"))
                            .child(
                                div()
                                    .text_sm()
                                    .text_color(cx.theme().muted_foreground)
                                    .child("Use imported AutoEQ/APO parametric profiles or the compatible ten-band graphic equalizer below."),
                            ),
                    )
                    .child(self.render_equalizer_profiles(busy, cx))
                    .child(
                        h_flex()
                            .gap_2()
                            .child(
                                Button::new("equalizer-on")
                                    .label("On")
                                    .selected(self.settings_draft.equalizer.enabled)
                                    .disabled(busy)
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.settings_draft.equalizer.enabled = true;
                                        this.settings_error = None;
                                        this.settings_notice = None;
                                        cx.notify();
                                    })),
                            )
                            .child(
                                Button::new("equalizer-off")
                                    .label("Off")
                                    .selected(!self.settings_draft.equalizer.enabled)
                                    .disabled(busy)
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.settings_draft.equalizer.enabled = false;
                                        this.settings_draft.equalizer.active_profile = None;
                                        this.settings_error = None;
                                        this.settings_notice = None;
                                        cx.notify();
                                    })),
                            ),
                    )
                    .child(
                        h_flex().gap_2().flex_wrap().children(
                            EqualizerPreset::ALL.into_iter().map(|preset| {
                                Button::new(format!(
                                    "equalizer-preset-{}",
                                    preset.label().to_lowercase()
                                ))
                                .label(preset.label())
                                .selected(
                                    self.settings_draft.equalizer.preset() == Some(preset)
                                        && (preset != EqualizerPreset::Flat
                                            || !self.settings_draft.equalizer.enabled),
                                )
                                .disabled(busy)
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.settings_draft.equalizer =
                                        EqualizerSettings::from_preset(preset);
                                    this.settings_error = None;
                                    this.settings_notice = None;
                                    cx.notify();
                                }))
                            }),
                        ),
                    )
                    .child(
                        h_flex().gap_2().flex_wrap().children(
                            EQUALIZER_FREQUENCIES_HZ.into_iter().enumerate().map(
                                |(index, frequency)| {
                                    let gain_mb = self.settings_draft.equalizer.gains_mb[index];
                                    v_flex()
                                        .w(px(116.))
                                        .gap_2()
                                        .rounded(cx.theme().radius)
                                        .border_1()
                                        .border_color(cx.theme().border)
                                        .p_3()
                                        .items_center()
                                        .child(
                                            div()
                                                .text_xs()
                                                .text_color(cx.theme().muted_foreground)
                                                .child(format_equalizer_frequency(frequency)),
                                        )
                                        .child(
                                            div().font_medium().child(format!(
                                                "{:+.0} dB",
                                                f32::from(gain_mb) / 100.0
                                            )),
                                        )
                                        .child(
                                            h_flex()
                                                .gap_1()
                                                .child(
                                                    Button::new(format!(
                                                        "equalizer-band-{index}-down"
                                                    ))
                                                    .ghost()
                                                    .label("−")
                                                    .disabled(
                                                        busy
                                                            || gain_mb
                                                                <= MIN_EQUALIZER_GAIN_MB,
                                                    )
                                                    .on_click(cx.listener(
                                                        move |this, _, _, cx| {
                                                            this.settings_draft.equalizer
                                                                .gains_mb[index] = this
                                                                .settings_draft
                                                                .equalizer
                                                                .gains_mb[index]
                                                                .saturating_sub(100)
                                                                .max(MIN_EQUALIZER_GAIN_MB);
                                                            this.settings_draft.equalizer.enabled =
                                                                true;
                                                            this.settings_draft
                                                                .equalizer
                                                                .active_profile = None;
                                                            this.settings_error = None;
                                                            this.settings_notice = None;
                                                            cx.notify();
                                                        },
                                                    )),
                                                )
                                                .child(
                                                    Button::new(format!(
                                                        "equalizer-band-{index}-up"
                                                    ))
                                                    .ghost()
                                                    .label("+")
                                                    .disabled(
                                                        busy
                                                            || gain_mb
                                                                >= MAX_EQUALIZER_GAIN_MB,
                                                    )
                                                    .on_click(cx.listener(
                                                        move |this, _, _, cx| {
                                                            this.settings_draft.equalizer
                                                                .gains_mb[index] = this
                                                                .settings_draft
                                                                .equalizer
                                                                .gains_mb[index]
                                                                .saturating_add(100)
                                                                .min(MAX_EQUALIZER_GAIN_MB);
                                                            this.settings_draft.equalizer.enabled =
                                                                true;
                                                            this.settings_draft
                                                                .equalizer
                                                                .active_profile = None;
                                                            this.settings_error = None;
                                                            this.settings_notice = None;
                                                            cx.notify();
                                                        },
                                                    )),
                                                ),
                                        )
                                },
                            ),
                        ),
                    ),
            )
            .child(
                v_flex()
                    .gap_4()
                    .max_w(px(680.))
                    .rounded(cx.theme().radius_lg)
                    .border_1()
                    .border_color(cx.theme().border)
                    .p_5()
                    .child(
                        v_flex()
                            .gap_1()
                            .child(div().font_semibold().child("YouTube Music listening history"))
                            .child(
                                div()
                                    .text_sm()
                                    .text_color(cx.theme().muted_foreground)
                                    .child("When signed in, register one remote play after 30 seconds. Local history is always kept separately on this device."),
                            ),
                    )
                    .child(
                        h_flex()
                            .gap_2()
                            .child(
                                Button::new("youtube-history-sync-on")
                                    .label("Sync plays")
                                    .selected(self.settings_draft.youtube_history_sync)
                                    .disabled(busy)
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.settings_draft.youtube_history_sync = true;
                                        this.settings_error = None;
                                        this.settings_notice = None;
                                        cx.notify();
                                    })),
                            )
                            .child(
                                Button::new("youtube-history-sync-off")
                                    .label("Pause remote history")
                                    .selected(!self.settings_draft.youtube_history_sync)
                                    .disabled(busy)
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.settings_draft.youtube_history_sync = false;
                                        this.settings_error = None;
                                        this.settings_notice = None;
                                        cx.notify();
                                    })),
                            ),
                    ),
            )
            .child(
                v_flex()
                    .gap_4()
                    .max_w(px(680.))
                    .rounded(cx.theme().radius_lg)
                    .border_1()
                    .border_color(cx.theme().border)
                    .p_5()
                    .child(
                        v_flex()
                            .gap_1()
                            .child(div().font_semibold().child("Volume normalization"))
                            .child(
                                div()
                                    .text_sm()
                                    .text_color(cx.theme().muted_foreground)
                                    .child("Match track loudness using YouTube's measured metadata. Gain is limited to −15 dB through +3 dB and clipped safely before output."),
                            ),
                    )
                    .child(
                        h_flex()
                            .gap_2()
                            .child(
                                Button::new("audio-normalization-on")
                                    .label("On")
                                    .selected(self.settings_draft.audio_normalization)
                                    .disabled(busy)
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.settings_draft.audio_normalization = true;
                                        this.settings_error = None;
                                        this.settings_notice = None;
                                        cx.notify();
                                    })),
                            )
                            .child(
                                Button::new("audio-normalization-off")
                                    .label("Off")
                                    .selected(!self.settings_draft.audio_normalization)
                                    .disabled(busy)
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.settings_draft.audio_normalization = false;
                                        this.settings_error = None;
                                        this.settings_notice = None;
                                        cx.notify();
                                    })),
                            ),
                    )
                    .when(self.settings_draft.audio_normalization, |card| {
                        card.child(
                            h_flex().gap_2().flex_wrap().children(
                                LoudnessLevel::ALL.into_iter().map(|level| {
                                    Button::new(format!(
                                        "loudness-level-{}",
                                        level.label().to_lowercase()
                                    ))
                                    .label(format!(
                                        "{} ({} LUFS)",
                                        level.label(),
                                        level.target_lufs_mb() / 100
                                    ))
                                    .selected(self.settings_draft.loudness_level == level)
                                    .disabled(busy)
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.settings_draft.loudness_level = level;
                                        this.settings_error = None;
                                        this.settings_notice = None;
                                        cx.notify();
                                    }))
                                }),
                            ),
                        )
                    }),
            )
            .child(
                v_flex()
                    .gap_4()
                    .max_w(px(680.))
                    .rounded(cx.theme().radius_lg)
                    .border_1()
                    .border_color(cx.theme().border)
                    .p_5()
                    .child(
                        v_flex()
                            .gap_1()
                            .child(div().font_semibold().child("Audio quality"))
                            .child(
                                div()
                                    .text_sm()
                                    .text_color(cx.theme().muted_foreground)
                                    .child("Auto selects the best direct AAC stream; Low caps at 128 kbps; High prioritizes YouTube's high-quality marker."),
                            ),
                    )
                    .child(
                        h_flex().gap_2().children(AudioQuality::ALL.into_iter().map(|quality| {
                            Button::new(format!("audio-quality-{}", quality.label()))
                                .label(quality.label())
                                .selected(self.settings_draft.audio_quality == quality)
                                .disabled(busy)
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.settings_draft.audio_quality = quality;
                                    this.settings_error = None;
                                    this.settings_notice = None;
                                    cx.notify();
                                }))
                        })),
                    ),
            )
            .child(
                v_flex()
                    .gap_4()
                    .max_w(px(680.))
                    .rounded(cx.theme().radius_lg)
                    .border_1()
                    .border_color(cx.theme().border)
                    .p_5()
                    .child(
                        v_flex()
                            .gap_1()
                            .child(div().font_semibold().child("Automatic radio"))
                            .child(
                                div()
                                    .text_sm()
                                    .text_color(cx.theme().muted_foreground)
                                    .child("When five or fewer queued songs remain, fetch anonymous YouTube Music radio recommendations and append them without duplicates."),
                            ),
                    )
                    .child(
                        h_flex()
                            .gap_2()
                            .child(
                                Button::new("auto-radio-on")
                                    .label("On")
                                    .selected(self.settings_draft.auto_radio)
                                    .disabled(busy)
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.settings_draft.auto_radio = true;
                                        this.settings_error = None;
                                        this.settings_notice = None;
                                        cx.notify();
                                    })),
                            )
                            .child(
                                Button::new("auto-radio-off")
                                    .label("Off")
                                    .selected(!self.settings_draft.auto_radio)
                                    .disabled(busy)
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.settings_draft.auto_radio = false;
                                        this.settings_error = None;
                                        this.settings_notice = None;
                                        cx.notify();
                                    })),
                            ),
                    ),
            )
            .child(
                v_flex()
                    .gap_4()
                    .max_w(px(680.))
                    .rounded(cx.theme().radius_lg)
                    .border_1()
                    .border_color(cx.theme().border)
                    .p_5()
                    .child(
                        v_flex()
                            .gap_1()
                            .child(div().font_semibold().child("Cache storage"))
                            .child(
                                div()
                                    .text_sm()
                                    .text_color(cx.theme().muted_foreground)
                                    .child("Artwork and aligned 512 KiB audio blocks are stored below and reused across sessions."),
                            ),
                    )
                    .child(
                        v_flex()
                            .gap_1()
                            .child(div().text_sm().child("Cache directory"))
                            .child(Input::new(&self.cache_root_input).disabled(busy)),
                    )
                    .child(div().text_sm().child("Audio cache capacity"))
                    .child(
                        h_flex().gap_2().flex_wrap().children(
                            [128_u64, 512, 1024, 2048, 4096]
                                .into_iter()
                                .map(|mebibytes| {
                                    let bytes = mebibytes * 1024 * 1024;
                                    Button::new(format!("audio-cache-{mebibytes}"))
                                        .label(format!("{mebibytes} MiB"))
                                        .selected(self.settings_draft.audio_cache_bytes == bytes)
                                        .disabled(busy)
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            this.settings_draft.audio_cache_bytes = bytes;
                                            this.settings_error = None;
                                            this.settings_notice = None;
                                            cx.notify();
                                        }))
                                }),
                        ),
                    )
                    .child(
                        div()
                            .text_sm()
                            .text_color(cx.theme().muted_foreground)
                            .child(format!(
                                "Active: {} MiB at {}",
                                self.settings.audio_cache_bytes / 1024 / 1024,
                                self.settings.cache_root.display()
                            )),
                    ),
            )
            .when_some(self.settings_error.clone(), |page, error| {
                page.child(
                    div()
                        .max_w(px(680.))
                        .rounded(cx.theme().radius)
                        .bg(cx.theme().danger.opacity(0.12))
                        .text_color(cx.theme().danger)
                        .p_3()
                        .text_sm()
                        .child(error),
                )
            })
            .when_some(self.settings_notice.clone(), |page, notice| {
                page.child(
                    div()
                        .max_w(px(680.))
                        .rounded(cx.theme().radius)
                        .bg(cx.theme().success.opacity(0.12))
                        .text_color(cx.theme().success)
                        .p_3()
                        .text_sm()
                        .child(notice),
                )
            })
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        Button::new("settings-apply")
                            .primary()
                            .label(if busy { "Applying…" } else { "Save and apply" })
                            .disabled(busy)
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.apply_settings(window, cx)
                            })),
                    )
                    .child(
                        Button::new("settings-reset")
                            .label("Reset unsaved changes")
                            .disabled(busy)
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.reset_settings_editor(window, cx)
                            })),
                    ),
            )
            .into_any_element()
    }

    fn render_page(&self, cx: &mut Context<Self>) -> AnyElement {
        match self.model.route() {
            Route::Home => self.render_home(cx),
            Route::Explore => self.render_explore(cx),
            Route::Search => self.render_search(cx),
            Route::Recognition => self.render_recognition(cx),
            Route::History => self.render_history(cx),
            Route::Stats => self.render_stats(cx),
            Route::Library => self.render_library(cx),
            Route::Settings => self.render_settings(cx),
        }
    }

    fn render_queue_row(
        index: usize,
        item: QueueItem,
        current_index: Option<usize>,
        queue_len: usize,
        host_controlled: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        v_flex()
            .w_full()
            .gap_1()
            .pb_1()
            .child(
                Button::new(format!("queue-item-{index}"))
                    .ghost()
                    .w_full()
                    .justify_start()
                    .label(format!("{}. {}", index + 1, item.song.title))
                    .tooltip(item.song.artist_line())
                    .selected(current_index == Some(index))
                    .disabled(host_controlled)
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.play_queue_item(index, window, cx);
                    })),
            )
            .child(
                h_flex()
                    .w_full()
                    .gap_1()
                    .justify_end()
                    .child(
                        Button::new(format!("queue-move-up-{index}"))
                            .ghost()
                            .icon(IconName::ArrowUp)
                            .label("Up")
                            .tooltip("Move this item earlier in the queue")
                            .disabled(host_controlled || index == 0)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.move_queue_item(index, index.saturating_sub(1), cx);
                            })),
                    )
                    .child(
                        Button::new(format!("queue-move-down-{index}"))
                            .ghost()
                            .icon(IconName::ArrowDown)
                            .label("Down")
                            .tooltip("Move this item later in the queue")
                            .disabled(host_controlled || index.saturating_add(1) >= queue_len)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.move_queue_item(index, index.saturating_add(1), cx);
                            })),
                    )
                    .child(
                        Button::new(format!("queue-remove-{index}"))
                            .danger()
                            .icon(IconName::Delete)
                            .label("Remove")
                            .tooltip("Remove this item from the queue")
                            .disabled(host_controlled)
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.remove_queue_item(index, window, cx);
                            })),
                    ),
            )
            .into_any_element()
    }

    fn render_queue_panel(&self, cx: &mut Context<Self>) -> AnyElement {
        let host_controlled = self.listen_together_is_guest();
        let current_index = self.queue.current_index();
        let items = self.queue.items().to_vec();
        let queue_len = items.len();
        let seed_label = |video_id: &str| {
            self.queue
                .items()
                .iter()
                .find(|item| item.song.video_id == video_id)
                .map_or_else(
                    || "the current song".to_owned(),
                    |item| item.song.title.clone(),
                )
        };
        let (radio_title, radio_detail, radio_loading, radio_failed) = match &self.radio_state {
            RadioQueueState::Idle if self.settings.auto_radio => (
                "Automatic radio ready".to_owned(),
                "Recommendations load when five or fewer songs remain.".to_owned(),
                false,
                false,
            ),
            RadioQueueState::Idle => (
                "Radio is idle".to_owned(),
                "Automatic loading is off; you can still start a radio manually.".to_owned(),
                false,
                false,
            ),
            RadioQueueState::Loading(request) => (
                "Loading radio…".to_owned(),
                format!(
                    "Fetching anonymous recommendations for {}.",
                    seed_label(request.seed_video_id())
                ),
                true,
                false,
            ),
            RadioQueueState::Active(session) => (
                session
                    .title
                    .clone()
                    .unwrap_or_else(|| "Radio active".into()),
                "More recommendations will load near the end of the queue.".to_owned(),
                false,
                false,
            ),
            RadioQueueState::Exhausted(session) => (
                session
                    .title
                    .clone()
                    .unwrap_or_else(|| "Radio complete".into()),
                "No further recommendations are available. Restart to try again.".to_owned(),
                false,
                false,
            ),
            RadioQueueState::Failed(request, error) => (
                "Radio could not continue".to_owned(),
                format!("{}: {error}", seed_label(request.seed_video_id())),
                false,
                true,
            ),
        };
        let radio_detail_color = if radio_failed {
            cx.theme().danger
        } else {
            cx.theme().muted_foreground
        };

        v_flex()
            .h_full()
            .w(px(360.))
            .flex_shrink_0()
            .border_l_1()
            .border_color(cx.theme().border)
            .bg(cx.theme().background)
            .child(
                h_flex()
                    .h(px(64.))
                    .px_4()
                    .items_center()
                    .justify_between()
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .child(
                        v_flex().child(div().font_semibold().child("Queue")).child(
                            div()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child(format!("{queue_len} songs")),
                        ),
                    )
                    .child(Button::new("close-queue").ghost().label("Close").on_click(
                        cx.listener(|this, _, _, cx| {
                            this.queue_visible = false;
                            cx.notify();
                        }),
                    )),
            )
            .child(
                h_flex()
                    .flex_wrap()
                    .gap_2()
                    .m_3()
                    .mb_0()
                    .child(
                        Button::new("queue-shuffle")
                            .ghost()
                            .flex_1()
                            .label(if self.shuffle_enabled {
                                "Shuffle on"
                            } else {
                                "Shuffle"
                            })
                            .tooltip("Randomize the remaining queue")
                            .selected(self.shuffle_enabled)
                            .disabled(host_controlled || self.queue.is_empty())
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.toggle_shuffle(cx);
                            })),
                    )
                    .child(
                        Button::new("queue-repeat")
                            .ghost()
                            .flex_1()
                            .label(self.repeat_mode.label())
                            .tooltip("Cycle repeat off, repeat all, and repeat one")
                            .selected(self.repeat_mode != RepeatMode::Off)
                            .disabled(host_controlled || self.queue.is_empty())
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.cycle_repeat_mode(cx);
                            })),
                    )
                    .child(
                        Button::new("queue-clear-upcoming")
                            .danger()
                            .flex_1()
                            .label("Clear upcoming")
                            .tooltip("Keep the current song and remove everything after it")
                            .disabled(host_controlled || self.queue.remaining_after_current() == 0)
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.clear_upcoming_queue(cx);
                            })),
                    ),
            )
            .child(
                v_flex()
                    .gap_2()
                    .m_3()
                    .mb_0()
                    .rounded(cx.theme().radius)
                    .border_1()
                    .border_color(cx.theme().border)
                    .p_3()
                    .child(
                        h_flex()
                            .items_center()
                            .justify_between()
                            .child(div().text_sm().font_medium().child("Sleep timer"))
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(self.sleep_timer.map_or_else(
                                        || "Off".into(),
                                        |timer| timer.summary(Instant::now()),
                                    )),
                            ),
                    )
                    .child(
                        h_flex()
                            .flex_wrap()
                            .gap_2()
                            .child(
                                Button::new("sleep-timer-15")
                                    .ghost()
                                    .label("15 min")
                                    .disabled(host_controlled || self.current_song.is_none())
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.set_sleep_timer(
                                            SleepTimer::Deadline(
                                                Instant::now() + Duration::from_secs(15 * 60),
                                            ),
                                            cx,
                                        );
                                    })),
                            )
                            .child(
                                Button::new("sleep-timer-30")
                                    .ghost()
                                    .label("30 min")
                                    .disabled(host_controlled || self.current_song.is_none())
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.set_sleep_timer(
                                            SleepTimer::Deadline(
                                                Instant::now() + Duration::from_secs(30 * 60),
                                            ),
                                            cx,
                                        );
                                    })),
                            )
                            .child(
                                Button::new("sleep-timer-60")
                                    .ghost()
                                    .label("60 min")
                                    .disabled(host_controlled || self.current_song.is_none())
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.set_sleep_timer(
                                            SleepTimer::Deadline(
                                                Instant::now() + Duration::from_secs(60 * 60),
                                            ),
                                            cx,
                                        );
                                    })),
                            )
                            .child(
                                Button::new("sleep-timer-song-end")
                                    .ghost()
                                    .label("End of song")
                                    .disabled(host_controlled || self.current_song.is_none())
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.set_sleep_timer(SleepTimer::EndOfSong, cx);
                                    })),
                            )
                            .when(self.sleep_timer.is_some(), |actions| {
                                actions.child(
                                    Button::new("sleep-timer-cancel")
                                        .danger()
                                        .label("Cancel")
                                        .disabled(host_controlled)
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            this.cancel_sleep_timer(cx);
                                        })),
                                )
                            }),
                    ),
            )
            .child(
                v_flex()
                    .gap_2()
                    .m_3()
                    .mb_0()
                    .rounded(cx.theme().radius)
                    .border_1()
                    .border_color(cx.theme().border)
                    .p_3()
                    .child(
                        h_flex()
                            .gap_2()
                            .items_center()
                            .justify_between()
                            .child(div().min_w_0().text_sm().font_medium().child(radio_title))
                            .child(
                                Button::new("queue-start-radio")
                                    .ghost()
                                    .label(if radio_loading {
                                        "Loading…"
                                    } else {
                                        "Start radio"
                                    })
                                    .disabled(
                                        host_controlled
                                            || self.current_song.is_none()
                                            || radio_loading,
                                    )
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.start_radio_from_current(window, cx);
                                    })),
                            ),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(radio_detail_color)
                            .child(radio_detail),
                    )
                    .when(radio_failed, |card| {
                        card.child(
                            Button::new("queue-retry-radio")
                                .label("Retry")
                                .disabled(host_controlled)
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.retry_radio(window, cx);
                                })),
                        )
                    }),
            )
            .child(
                v_flex()
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scrollbar()
                    .gap_1()
                    .p_3()
                    .children(items.into_iter().enumerate().map(|(index, item)| {
                        Self::render_queue_row(
                            index,
                            item,
                            current_index,
                            queue_len,
                            host_controlled,
                            cx,
                        )
                    })),
            )
            .into_any_element()
    }

    fn render_playback_parameters_panel(&self, cx: &mut Context<Self>) -> AnyElement {
        let parameters = self
            .playback_parameters_pending
            .unwrap_or(self.settings.playback_parameters);
        let busy = self.playback_parameters_pending.is_some()
            || self.settings_operation == SettingsOperation::Applying
            || self.equalizer_operation != EqualizerOperation::Idle;
        let in_room = self.listen_together_snapshot.room.is_some();
        let unavailable = busy || in_room || self.current_song.is_none();

        v_flex()
            .h_full()
            .w(px(320.))
            .flex_shrink_0()
            .border_l_1()
            .border_color(cx.theme().border)
            .bg(cx.theme().background)
            .child(
                h_flex()
                    .h(px(72.))
                    .px_4()
                    .items_center()
                    .justify_between()
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .child(
                        v_flex()
                            .min_w_0()
                            .child(div().font_semibold().child("Speed & pitch"))
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(if busy {
                                        "Rebuilding the audio chain…"
                                    } else {
                                        "Changes apply and save immediately"
                                    }),
                            ),
                    )
                    .child(
                        Button::new("close-playback-parameters")
                            .ghost()
                            .label("Close")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.playback_parameters_visible = false;
                                cx.notify();
                            })),
                    ),
            )
            .child(
                v_flex()
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scrollbar()
                    .gap_5()
                    .p_4()
                    .when(in_room, |content| {
                        content.child(
                            div()
                                .rounded(cx.theme().radius)
                                .bg(cx.theme().warning.opacity(0.12))
                                .text_color(cx.theme().warning)
                                .p_3()
                                .text_sm()
                                .child("Speed and pitch are locked while connected to a Listen Together room because these parameters are not synchronized by the room protocol."),
                        )
                    })
                    .when_some(self.playback_parameters_error.clone(), |content, error| {
                        content.child(
                            div()
                                .rounded(cx.theme().radius)
                                .bg(cx.theme().danger.opacity(0.12))
                                .text_color(cx.theme().danger)
                                .p_3()
                                .text_sm()
                                .child(error),
                        )
                    })
                    .when_some(
                        self.playback_parameters_notice.clone(),
                        |content, notice| {
                            content.child(
                                div()
                                    .rounded(cx.theme().radius)
                                    .bg(cx.theme().success.opacity(0.12))
                                    .text_color(cx.theme().success)
                                    .p_3()
                                    .text_sm()
                                    .child(notice),
                            )
                        },
                    )
                    .child(
                        v_flex()
                            .gap_2()
                            .child(div().text_sm().font_medium().child("Mode"))
                            .child(
                                h_flex()
                                    .gap_2()
                                    .child(
                                        Button::new("live-playback-mode-normal")
                                            .label("Normal")
                                            .selected(!parameters.varispeed)
                                            .disabled(unavailable)
                                            .on_click(cx.listener(|this, _, _, cx| {
                                                this.request_playback_parameter_change(
                                                    PlaybackParameterChange::NormalMode,
                                                    cx,
                                                );
                                            })),
                                    )
                                    .child(
                                        Button::new("live-playback-mode-varispeed")
                                            .label("Varispeed")
                                            .selected(parameters.varispeed)
                                            .disabled(unavailable)
                                            .on_click(cx.listener(|this, _, _, cx| {
                                                this.request_playback_parameter_change(
                                                    PlaybackParameterChange::VarispeedMode,
                                                    cx,
                                                );
                                            })),
                                    ),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(if parameters.varispeed {
                                        "Varispeed links pitch to speed like tape playback."
                                    } else {
                                        "Normal mode changes tempo independently of pitch."
                                    }),
                            ),
                    )
                    .child(
                        v_flex()
                            .gap_2()
                            .child(div().text_sm().font_medium().child("Speed"))
                            .child(
                                h_flex()
                                    .gap_3()
                                    .items_center()
                                    .child(
                                        Button::new("live-playback-speed-down")
                                            .ghost()
                                            .label("−")
                                            .disabled(
                                                unavailable
                                                    || parameters.tempo_milli
                                                        <= MIN_PLAYBACK_RATE_MILLI,
                                            )
                                            .on_click(cx.listener(|this, _, _, cx| {
                                                this.request_playback_parameter_change(
                                                    PlaybackParameterChange::SpeedDown,
                                                    cx,
                                                );
                                            })),
                                    )
                                    .child(
                                        div()
                                            .w(px(88.))
                                            .text_center()
                                            .font_semibold()
                                            .child(format!(
                                                "{:.2}×",
                                                parameters.tempo_ratio()
                                            )),
                                    )
                                    .child(
                                        Button::new("live-playback-speed-up")
                                            .ghost()
                                            .label("+")
                                            .disabled(
                                                unavailable
                                                    || parameters.tempo_milli
                                                        >= MAX_PLAYBACK_RATE_MILLI,
                                            )
                                            .on_click(cx.listener(|this, _, _, cx| {
                                                this.request_playback_parameter_change(
                                                    PlaybackParameterChange::SpeedUp,
                                                    cx,
                                                );
                                            })),
                                    ),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .child("0.25×–2.00× in 0.05× steps"),
                            ),
                    )
                    .when(!parameters.varispeed, |content| {
                        content.child(
                            v_flex()
                                .gap_2()
                                .child(div().text_sm().font_medium().child("Transpose"))
                                .child(
                                    h_flex()
                                        .gap_3()
                                        .items_center()
                                        .child(
                                            Button::new("live-playback-transpose-down")
                                                .ghost()
                                                .label("−")
                                                .disabled(
                                                    unavailable
                                                        || parameters.transpose_semitones
                                                            <= MIN_TRANSPOSE_SEMITONES,
                                                )
                                                .on_click(cx.listener(|this, _, _, cx| {
                                                    this.request_playback_parameter_change(
                                                        PlaybackParameterChange::TransposeDown,
                                                        cx,
                                                    );
                                                })),
                                        )
                                        .child(
                                            div()
                                                .w(px(88.))
                                                .text_center()
                                                .font_semibold()
                                                .child(format!(
                                                    "{:+} st",
                                                    parameters.transpose_semitones
                                                )),
                                        )
                                        .child(
                                            Button::new("live-playback-transpose-up")
                                                .ghost()
                                                .label("+")
                                                .disabled(
                                                    unavailable
                                                        || parameters.transpose_semitones
                                                            >= MAX_TRANSPOSE_SEMITONES,
                                                )
                                                .on_click(cx.listener(|this, _, _, cx| {
                                                    this.request_playback_parameter_change(
                                                        PlaybackParameterChange::TransposeUp,
                                                        cx,
                                                    );
                                                })),
                                        ),
                                )
                                .child(
                                    div()
                                        .text_xs()
                                        .text_color(cx.theme().muted_foreground)
                                        .child("−12 to +12 semitones"),
                                ),
                        )
                    })
                    .child(
                        Button::new("live-playback-parameters-reset")
                            .label("Reset to 1.00× / 0 st")
                            .disabled(
                                unavailable || parameters == PlaybackParameters::default(),
                            )
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.request_playback_parameter_change(
                                    PlaybackParameterChange::Reset,
                                    cx,
                                );
                            })),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child("Each change rebuilds the DSP chain off the UI thread, restores the same media position and play/pause state, then saves the validated parameters. If rebuilding or saving fails, the previous parameters are restored."),
                    ),
            )
            .into_any_element()
    }

    fn render_lyrics_panel(&self, cx: &mut Context<Self>) -> AnyElement {
        let host_controlled = self.listen_together_is_guest();
        let song_title = self
            .current_song
            .as_ref()
            .map_or_else(|| "Nothing playing".to_owned(), |song| song.title.clone());
        let content = match &self.lyrics_state {
            LyricsViewState::Idle if self.current_song.is_none() => v_flex()
                .flex_1()
                .items_center()
                .justify_center()
                .gap_2()
                .child(div().font_medium().child("Choose a song"))
                .child(
                    div()
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .child("Timed lyrics will appear here."),
                )
                .into_any_element(),
            LyricsViewState::Idle => v_flex()
                .flex_1()
                .items_center()
                .justify_center()
                .child(
                    Button::new("load-current-lyrics")
                        .label("Load lyrics")
                        .on_click(cx.listener(|this, _, _, cx| this.reload_lyrics(cx))),
                )
                .into_any_element(),
            LyricsViewState::Loading(_) => v_flex()
                .flex_1()
                .items_center()
                .justify_center()
                .gap_3()
                .child(Icon::new(IconName::LoaderCircle).size_8())
                .child("Looking for timed lyrics…")
                .into_any_element(),
            LyricsViewState::Unavailable(_) => v_flex()
                .flex_1()
                .items_center()
                .justify_center()
                .gap_3()
                .child(Icon::new(IconName::BookOpen).size_8())
                .child(div().font_medium().child("Lyrics unavailable"))
                .child(
                    div()
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .child("No close metadata match was found."),
                )
                .child(
                    Button::new("retry-unavailable-lyrics")
                        .label("Try again")
                        .on_click(cx.listener(|this, _, _, cx| this.reload_lyrics(cx))),
                )
                .into_any_element(),
            LyricsViewState::Failed(_, message) => v_flex()
                .flex_1()
                .items_center()
                .justify_center()
                .gap_3()
                .child(Icon::new(IconName::TriangleAlert).size_8())
                .child(div().font_medium().child("Lyrics request failed"))
                .child(
                    div()
                        .max_w(px(300.))
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .child(message.clone()),
                )
                .child(
                    Button::new("retry-failed-lyrics")
                        .label("Try again")
                        .on_click(cx.listener(|this, _, _, cx| this.reload_lyrics(cx))),
                )
                .into_any_element(),
            LyricsViewState::Loaded(_, document) => {
                let synced = document.is_synced();
                v_flex()
                    .id("lyrics-scroll")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .track_scroll(&self.lyrics_scroll)
                    .vertical_scrollbar(&self.lyrics_scroll)
                    .gap_1()
                    .p_3()
                    .children(document.lines.iter().enumerate().map(|(index, line)| {
                        if let Some(start) = line.start {
                            Button::new(format!("lyrics-line-{index}"))
                                .ghost()
                                .w_full()
                                .justify_start()
                                .label(line.text.clone())
                                .tooltip(format!("Seek to {}", format_duration(start)))
                                .selected(self.lyrics_active_line == Some(index))
                                .disabled(host_controlled)
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.seek_from_desktop(start, cx);
                                }))
                                .into_any_element()
                        } else {
                            div()
                                .w_full()
                                .rounded(cx.theme().radius)
                                .px_3()
                                .py_2()
                                .text_sm()
                                .text_color(cx.theme().muted_foreground)
                                .child(line.text.clone())
                                .into_any_element()
                        }
                    }))
                    .when(!synced, |list| {
                        list.child(
                            div()
                                .mt_3()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child("This provider returned untimed lyrics."),
                        )
                    })
                    .into_any_element()
            }
        };
        let provider = match &self.lyrics_state {
            LyricsViewState::Loaded(_, document) => document.provider.as_str(),
            _ => "Lyrics",
        };

        v_flex()
            .h_full()
            .w(px(360.))
            .flex_shrink_0()
            .border_l_1()
            .border_color(cx.theme().border)
            .bg(cx.theme().background)
            .child(
                h_flex()
                    .h(px(72.))
                    .px_4()
                    .items_center()
                    .justify_between()
                    .gap_3()
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .child(div().font_semibold().child(provider.to_owned()))
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .overflow_hidden()
                                    .child(song_title),
                            ),
                    )
                    .child(
                        Button::new("refresh-lyrics")
                            .ghost()
                            .label("Refresh")
                            .disabled(self.current_song.is_none())
                            .on_click(cx.listener(|this, _, _, cx| this.reload_lyrics(cx))),
                    )
                    .child(
                        Button::new("close-lyrics")
                            .ghost()
                            .label("Close")
                            .on_click(cx.listener(|this, _, _, cx| this.toggle_lyrics(cx))),
                    ),
            )
            .child(content)
            .into_any_element()
    }

    fn render_playlist_picker(&self, cx: &mut Context<Self>) -> AnyElement {
        let Some(song) = self.playlist_picker_song.clone() else {
            return div().into_any_element();
        };
        let content = match &self.playlists_state {
            StoredViewState::Loading => v_flex()
                .flex_1()
                .items_center()
                .justify_center()
                .child("Loading playlists…")
                .into_any_element(),
            StoredViewState::Failed(message) => v_flex()
                .flex_1()
                .items_center()
                .justify_center()
                .gap_2()
                .child(Icon::new(IconName::TriangleAlert))
                .child(message.clone())
                .into_any_element(),
            StoredViewState::Loaded(playlists) if playlists.is_empty() => v_flex()
                .flex_1()
                .items_center()
                .justify_center()
                .gap_3()
                .child("No local playlists yet")
                .child(
                    Button::new("picker-create-playlist")
                        .label("Open Library")
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.playlist_picker_song = None;
                            this.navigate(Route::Library, cx);
                        })),
                )
                .into_any_element(),
            StoredViewState::Loaded(playlists) => v_flex()
                .flex_1()
                .min_h_0()
                .overflow_y_scrollbar()
                .gap_2()
                .p_3()
                .children(playlists.iter().map(|playlist| {
                    let playlist_song = song.clone();
                    let playlist_id = playlist.id;
                    Button::new(format!("picker-playlist-{playlist_id}"))
                        .w_full()
                        .justify_start()
                        .icon(IconName::Plus)
                        .label(playlist.name.clone())
                        .tooltip(format!("{} songs", playlist.song_count))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.add_song_to_playlist(playlist_id, playlist_song.clone(), cx);
                        }))
                        .into_any_element()
                }))
                .into_any_element(),
        };

        v_flex()
            .h_full()
            .w(px(300.))
            .flex_shrink_0()
            .border_l_1()
            .border_color(cx.theme().border)
            .bg(cx.theme().background)
            .child(
                h_flex()
                    .h(px(72.))
                    .px_4()
                    .items_center()
                    .justify_between()
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .child(
                        v_flex()
                            .min_w_0()
                            .child(div().font_semibold().child("Add to playlist"))
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(song.title),
                            ),
                    )
                    .child(
                        Button::new("close-playlist-picker")
                            .ghost()
                            .label("Close")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.playlist_picker_song = None;
                                cx.notify();
                            })),
                    ),
            )
            .child(content)
            .into_any_element()
    }

    fn render_cloud_playlist_picker(&self, cx: &mut Context<Self>) -> AnyElement {
        let Some(song) = self.cloud_playlist_picker_song.clone() else {
            return div().into_any_element();
        };
        let content = match &self.cloud_library_state {
            CloudLibraryViewState::SignedOut => v_flex()
                .flex_1()
                .items_center()
                .justify_center()
                .child("Sign in to add songs online.")
                .into_any_element(),
            CloudLibraryViewState::Loading => v_flex()
                .flex_1()
                .items_center()
                .justify_center()
                .child("Loading online playlists…")
                .into_any_element(),
            CloudLibraryViewState::Failed(message) => v_flex()
                .flex_1()
                .items_center()
                .justify_center()
                .gap_2()
                .child(Icon::new(IconName::TriangleAlert))
                .child(message.clone())
                .child(
                    Button::new("cloud-picker-retry")
                        .label("Try again")
                        .disabled(self.cloud_busy())
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.reload_cloud_library(cx);
                        })),
                )
                .into_any_element(),
            CloudLibraryViewState::Loaded(library) => {
                let editable = library
                    .playlists
                    .iter()
                    .filter(|playlist| playlist.editable)
                    .collect::<Vec<_>>();
                if editable.is_empty() {
                    v_flex()
                        .flex_1()
                        .items_center()
                        .justify_center()
                        .gap_3()
                        .child("No editable online playlists yet")
                        .child(
                            Button::new("cloud-picker-create")
                                .primary()
                                .label("Create online playlist")
                                .disabled(self.cloud_busy())
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.open_create_cloud_playlist_dialog(window, cx);
                                })),
                        )
                        .into_any_element()
                } else {
                    v_flex()
                        .flex_1()
                        .min_h_0()
                        .overflow_y_scrollbar()
                        .gap_2()
                        .p_3()
                        .children(editable.into_iter().enumerate().map(|(index, playlist)| {
                            let playlist = playlist.clone();
                            let playlist_song = song.clone();
                            Button::new(format!("cloud-picker-playlist-{index}"))
                                .w_full()
                                .justify_start()
                                .icon(IconName::Plus)
                                .label(playlist.title.clone())
                                .tooltip(playlist.subtitle.clone())
                                .disabled(self.cloud_busy())
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.add_song_to_cloud_playlist(
                                        playlist.clone(),
                                        playlist_song.clone(),
                                        cx,
                                    );
                                }))
                                .into_any_element()
                        }))
                        .into_any_element()
                }
            }
        };

        v_flex()
            .h_full()
            .w(px(320.))
            .flex_shrink_0()
            .border_l_1()
            .border_color(cx.theme().border)
            .bg(cx.theme().background)
            .child(
                h_flex()
                    .h(px(72.))
                    .px_4()
                    .items_center()
                    .justify_between()
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .child(
                        v_flex()
                            .min_w_0()
                            .child(div().font_semibold().child("Add online"))
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(song.title),
                            ),
                    )
                    .child(
                        Button::new("close-cloud-playlist-picker")
                            .ghost()
                            .label("Close")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.cloud_playlist_picker_song = None;
                                cx.notify();
                            })),
                    ),
            )
            .when_some(self.cloud_library_error.clone(), |panel, message| {
                panel.child(
                    div()
                        .m_3()
                        .rounded(cx.theme().radius)
                        .bg(cx.theme().danger.opacity(0.12))
                        .text_color(cx.theme().danger)
                        .p_3()
                        .text_sm()
                        .child(message),
                )
            })
            .child(content)
            .into_any_element()
    }

    fn render_now_playing_queue(&self, cx: &mut Context<Self>) -> AnyElement {
        let host_controlled = self.listen_together_is_guest();
        let current_index = self.queue.current_index();
        let queue_len = self.queue.len();
        let start = current_index.unwrap_or_default();
        let items = self
            .queue
            .items()
            .iter()
            .cloned()
            .enumerate()
            .skip(start)
            .collect::<Vec<_>>();

        v_flex()
            .size_full()
            .min_h_0()
            .child(
                h_flex()
                    .flex_wrap()
                    .gap_2()
                    .p_3()
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .child(
                        Button::new("now-playing-shuffle")
                            .ghost()
                            .label(if self.shuffle_enabled {
                                "Shuffle on"
                            } else {
                                "Shuffle"
                            })
                            .selected(self.shuffle_enabled)
                            .disabled(host_controlled || self.queue.is_empty())
                            .on_click(cx.listener(|this, _, _, cx| this.toggle_shuffle(cx))),
                    )
                    .child(
                        Button::new("now-playing-repeat")
                            .ghost()
                            .label(self.repeat_mode.label())
                            .selected(self.repeat_mode != RepeatMode::Off)
                            .disabled(host_controlled || self.queue.is_empty())
                            .on_click(cx.listener(|this, _, _, cx| this.cycle_repeat_mode(cx))),
                    )
                    .child(
                        Button::new("now-playing-clear-upcoming")
                            .danger()
                            .label("Clear upcoming")
                            .disabled(host_controlled || self.queue.remaining_after_current() == 0)
                            .on_click(cx.listener(|this, _, _, cx| this.clear_upcoming_queue(cx))),
                    ),
            )
            .child(
                v_flex()
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scrollbar()
                    .gap_1()
                    .p_3()
                    .when(items.is_empty(), |list| {
                        list.items_center()
                            .justify_center()
                            .child("The queue is empty")
                    })
                    .children(items.into_iter().map(|(index, item)| {
                        Self::render_queue_row(
                            index,
                            item,
                            current_index,
                            queue_len,
                            host_controlled,
                            cx,
                        )
                    })),
            )
            .into_any_element()
    }

    fn render_now_playing_lyrics(&self, cx: &mut Context<Self>) -> AnyElement {
        let host_controlled = self.listen_together_is_guest();
        match &self.lyrics_state {
            LyricsViewState::Idle => v_flex()
                .size_full()
                .items_center()
                .justify_center()
                .gap_3()
                .child(Icon::new(IconName::BookOpen).size_8())
                .child(
                    if self
                        .current_song
                        .as_ref()
                        .is_some_and(|song| song.is_episode)
                    {
                        "Lyrics are not available for podcast episodes"
                    } else {
                        "Lyrics have not been loaded"
                    },
                )
                .when(
                    self.current_song
                        .as_ref()
                        .is_some_and(|song| !song.is_episode),
                    |content| {
                        content.child(
                            Button::new("now-playing-load-lyrics")
                                .label("Load lyrics")
                                .on_click(cx.listener(|this, _, _, cx| this.reload_lyrics(cx))),
                        )
                    },
                )
                .into_any_element(),
            LyricsViewState::Loading(_) => v_flex()
                .size_full()
                .items_center()
                .justify_center()
                .gap_3()
                .child(Icon::new(IconName::LoaderCircle).size_8())
                .child("Looking for timed lyrics…")
                .into_any_element(),
            LyricsViewState::Unavailable(_) => v_flex()
                .size_full()
                .items_center()
                .justify_center()
                .gap_3()
                .child(Icon::new(IconName::BookOpen).size_8())
                .child(div().font_medium().child("Lyrics unavailable"))
                .child(
                    Button::new("now-playing-retry-unavailable-lyrics")
                        .label("Try again")
                        .on_click(cx.listener(|this, _, _, cx| this.reload_lyrics(cx))),
                )
                .into_any_element(),
            LyricsViewState::Failed(_, message) => v_flex()
                .size_full()
                .items_center()
                .justify_center()
                .gap_3()
                .child(Icon::new(IconName::TriangleAlert).size_8())
                .child(div().font_medium().child("Lyrics request failed"))
                .child(
                    div()
                        .max_w(px(360.))
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .child(message.clone()),
                )
                .child(
                    Button::new("now-playing-retry-failed-lyrics")
                        .label("Try again")
                        .on_click(cx.listener(|this, _, _, cx| this.reload_lyrics(cx))),
                )
                .into_any_element(),
            LyricsViewState::Loaded(_, document) => {
                v_flex()
                    .id("now-playing-lyrics-scroll")
                    .size_full()
                    .min_h_0()
                    .overflow_y_scroll()
                    .track_scroll(&self.lyrics_scroll)
                    .vertical_scrollbar(&self.lyrics_scroll)
                    .gap_1()
                    .p_4()
                    .children(document.lines.iter().enumerate().map(|(index, line)| {
                        if let Some(start) = line.start {
                            Button::new(format!("now-playing-lyrics-line-{index}"))
                                .ghost()
                                .w_full()
                                .justify_start()
                                .label(line.text.clone())
                                .tooltip(format!("Seek to {}", format_duration(start)))
                                .selected(self.lyrics_active_line == Some(index))
                                .disabled(host_controlled)
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.seek_from_desktop(start, cx)
                                }))
                                .into_any_element()
                        } else {
                            div()
                                .w_full()
                                .rounded(cx.theme().radius)
                                .px_3()
                                .py_2()
                                .text_sm()
                                .text_color(cx.theme().muted_foreground)
                                .child(line.text.clone())
                                .into_any_element()
                        }
                    }))
                    .into_any_element()
            }
        }
    }

    fn render_now_playing_related(&self, cx: &mut Context<Self>) -> AnyElement {
        let current_video_id = self
            .current_song
            .as_ref()
            .map(|song| song.video_id.as_str());
        let state_matches_current = current_video_id
            .is_some_and(|video_id| radio_state_matches_song(&self.radio_state, video_id));
        let (recommendations, loading, error) = match &self.radio_state {
            _ if !state_matches_current => (Vec::new(), false, None),
            RadioQueueState::Idle => (Vec::new(), false, None),
            RadioQueueState::Loading(RadioRequest::Initial { .. }) => (Vec::new(), true, None),
            RadioQueueState::Loading(RadioRequest::Continuation(session)) => {
                (session.recommendations.clone(), true, None)
            }
            RadioQueueState::Active(session) | RadioQueueState::Exhausted(session) => {
                (session.recommendations.clone(), false, None)
            }
            RadioQueueState::Failed(request, message) => {
                let songs = match request {
                    RadioRequest::Initial { .. } => Vec::new(),
                    RadioRequest::Continuation(session) => session.recommendations.clone(),
                };
                (songs, false, Some(message.clone()))
            }
        };
        let host_controlled = self.listen_together_is_guest();

        v_flex()
            .size_full()
            .min_h_0()
            .child(
                h_flex()
                    .gap_2()
                    .p_3()
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .child(
                        Button::new("now-playing-start-radio")
                            .primary()
                            .label(if loading { "Loading…" } else { "Start radio" })
                            .disabled(
                                host_controlled
                                    || self.current_song.is_none()
                                    || loading
                                    || self
                                        .current_song
                                        .as_ref()
                                        .is_some_and(|song| song.is_episode),
                            )
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.start_radio_from_current(window, cx)
                            })),
                    )
                    .when(error.is_some(), |header| {
                        header.child(
                            Button::new("now-playing-retry-radio")
                                .label("Retry")
                                .disabled(host_controlled)
                                .on_click(
                                    cx.listener(|this, _, window, cx| this.retry_radio(window, cx)),
                                ),
                        )
                    }),
            )
            .when_some(error, |layout, message| {
                layout.child(
                    div()
                        .mx_3()
                        .mt_3()
                        .rounded(cx.theme().radius)
                        .bg(cx.theme().danger.opacity(0.12))
                        .text_color(cx.theme().danger)
                        .p_3()
                        .text_sm()
                        .child(message),
                )
            })
            .child(
                v_flex()
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scrollbar()
                    .gap_2()
                    .p_3()
                    .when(recommendations.is_empty(), |list| {
                        list.items_center().justify_center().child(if loading {
                            "Loading related songs…"
                        } else {
                            "Start radio for the current song to load related songs"
                        })
                    })
                    .children(
                        recommendations
                            .into_iter()
                            .enumerate()
                            .map(|(index, song)| {
                                let play_song = song.clone();
                                h_flex()
                                    .flex_wrap()
                                    .w_full()
                                    .gap_3()
                                    .items_center()
                                    .rounded(cx.theme().radius)
                                    .border_1()
                                    .border_color(cx.theme().border)
                                    .p_3()
                                    .child(self.render_thumbnail(
                                        song.thumbnail_url.as_deref(),
                                        px(44.),
                                        IconName::Play,
                                        cx,
                                    ))
                                    .child(
                                        v_flex()
                                            .flex_1()
                                            .min_w_0()
                                            .child(div().font_medium().child(song.title.clone()))
                                            .child(
                                                div()
                                                    .text_sm()
                                                    .text_color(cx.theme().muted_foreground)
                                                    .child(song.artist_line()),
                                            ),
                                    )
                                    .child(self.queue_insert_buttons(
                                        format!("now-playing-related-{index}"),
                                        &song,
                                        cx,
                                    ))
                                    .child(
                                        Button::new(format!("now-playing-related-play-{index}"))
                                            .ghost()
                                            .icon(IconName::Play)
                                            .label("Play")
                                            .disabled(host_controlled)
                                            .on_click(cx.listener(move |this, _, window, cx| {
                                                this.play_song_collection(
                                                    vec![play_song.clone()],
                                                    0,
                                                    window,
                                                    cx,
                                                )
                                            })),
                                    )
                                    .into_any_element()
                            }),
                    ),
            )
            .into_any_element()
    }

    fn render_now_playing(&self, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let Some(song) = self.current_song.as_ref() else {
            return v_flex()
                .size_full()
                .items_center()
                .justify_center()
                .child("Nothing playing")
                .into_any_element();
        };
        let viewport = window.viewport_size();
        let art_size = if viewport.width < px(950.) || viewport.height < px(650.) {
            px(180.)
        } else if viewport.width >= px(1_500.) && viewport.height >= px(850.) {
            px(480.)
        } else {
            px(360.)
        };
        let pane = match self.now_playing_tab {
            NowPlayingTab::UpNext => self.render_now_playing_queue(cx),
            NowPlayingTab::Lyrics => self.render_now_playing_lyrics(cx),
            NowPlayingTab::Related => self.render_now_playing_related(cx),
        };
        let favorite = self.is_favorite(&song.video_id);
        let favorite_song = song.clone();
        let local_playlist_song = song.clone();
        let cloud_liked = self.cloud_video_liked(&song.video_id);
        let cloud_like_song = song.clone();
        let cloud_playlist_song = song.clone();
        let artist_item = song.artists.iter().find_map(|artist| {
            artist.id.as_ref().map(|id| BrowseItem {
                browse_id: id.clone(),
                kind: BrowseKind::Artist,
                title: artist.name.clone(),
                subtitle: "Artist".into(),
                thumbnail_url: song.thumbnail_url.clone(),
                params: None,
                editable: false,
            })
        });
        let artist_subscription = artist_item
            .as_ref()
            .filter(|_| self.account_ready() && self.cloud_library().is_some())
            .map(|artist| {
                (
                    artist.browse_id.clone(),
                    self.cloud_artist_subscribed(&artist.browse_id),
                )
            });
        let album_item = song.album.as_ref().map(|album| BrowseItem {
            browse_id: album.browse_id.clone(),
            kind: BrowseKind::Album,
            title: album.title.clone(),
            subtitle: "Album".into(),
            thumbnail_url: album.thumbnail_url.clone(),
            params: None,
            editable: false,
        });

        v_flex()
            .size_full()
            .min_h_0()
            .bg(cx.theme().background)
            .child(
                h_flex()
                    .h(px(64.))
                    .flex_shrink_0()
                    .items_center()
                    .gap_3()
                    .px_5()
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .child(
                        Button::new("close-now-playing")
                            .ghost()
                            .icon(IconName::ArrowLeft)
                            .label("Back")
                            .on_click(cx.listener(|this, _, _, cx| this.close_now_playing(cx))),
                    )
                    .child(
                        v_flex()
                            .min_w_0()
                            .child(div().font_semibold().child("Now playing"))
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(format!("{} — {}", song.title, song.artist_line())),
                            ),
                    ),
            )
            .child(
                h_flex()
                    .flex_1()
                    .min_h_0()
                    .gap_6()
                    .p_5()
                    .child(
                        v_flex()
                            .h_full()
                            .min_w(art_size)
                            .flex_1()
                            .items_center()
                            .justify_center()
                            .gap_4()
                            .child(self.render_thumbnail(
                                song.thumbnail_url.as_deref(),
                                art_size,
                                IconName::Play,
                                cx,
                            ))
                            .child(
                                v_flex()
                                    .max_w(art_size)
                                    .items_center()
                                    .gap_1()
                                    .child(
                                        div()
                                            .text_xl()
                                            .font_semibold()
                                            .text_center()
                                            .child(song.title.clone()),
                                    )
                                    .child(
                                        div()
                                            .text_sm()
                                            .text_center()
                                            .text_color(cx.theme().muted_foreground)
                                            .child(song.artist_line()),
                                    ),
                            )
                            .child(
                                h_flex()
                                    .max_w(art_size)
                                    .flex_wrap()
                                    .justify_center()
                                    .gap_2()
                                    .child(
                                        Button::new("now-playing-favorite")
                                            .ghost()
                                            .icon(IconName::Heart)
                                            .label(if favorite { "Favorited" } else { "Favorite" })
                                            .selected(favorite)
                                            .disabled(self.library_busy())
                                            .on_click(cx.listener(move |this, _, _, cx| {
                                                this.toggle_favorite(favorite_song.clone(), cx);
                                            })),
                                    )
                                    .child(
                                        Button::new("now-playing-local-playlist")
                                            .ghost()
                                            .label("Playlist +")
                                            .on_click(cx.listener(move |this, _, _, cx| {
                                                this.open_playlist_picker(
                                                    local_playlist_song.clone(),
                                                    cx,
                                                );
                                            })),
                                    )
                                    .when(self.account_ready() && !song.is_episode, |actions| {
                                        actions
                                            .child(
                                                Button::new("now-playing-cloud-like")
                                                    .ghost()
                                                    .label(if cloud_liked {
                                                        "YT ♥"
                                                    } else {
                                                        "YT ♡"
                                                    })
                                                    .selected(cloud_liked)
                                                    .disabled(self.cloud_busy())
                                                    .on_click(cx.listener(
                                                        move |this, _, _, cx| {
                                                            this.set_cloud_video_liked(
                                                                cloud_like_song.clone(),
                                                                !cloud_liked,
                                                                cx,
                                                            );
                                                        },
                                                    )),
                                            )
                                            .child(
                                                Button::new("now-playing-cloud-playlist")
                                                    .ghost()
                                                    .label("YT +")
                                                    .disabled(self.cloud_busy())
                                                    .on_click(cx.listener(
                                                        move |this, _, _, cx| {
                                                            this.open_cloud_playlist_picker(
                                                                cloud_playlist_song.clone(),
                                                                cx,
                                                            );
                                                        },
                                                    )),
                                            )
                                    })
                                    .when_some(
                                        artist_subscription,
                                        |actions, (channel_id, subscribed)| {
                                            actions.child(
                                                Button::new("now-playing-subscribe")
                                                    .ghost()
                                                    .label(if subscribed {
                                                        "Subscribed"
                                                    } else {
                                                        "Subscribe"
                                                    })
                                                    .selected(subscribed)
                                                    .disabled(self.cloud_busy())
                                                    .on_click(cx.listener(
                                                        move |this, _, _, cx| {
                                                            this.set_cloud_subscription(
                                                                channel_id.clone(),
                                                                !subscribed,
                                                                cx,
                                                            );
                                                        },
                                                    )),
                                            )
                                        },
                                    )
                                    .when_some(artist_item, |actions, artist| {
                                        actions.child(
                                            Button::new("now-playing-open-artist")
                                                .ghost()
                                                .label("Artist")
                                                .on_click(cx.listener(move |this, _, _, cx| {
                                                    this.now_playing_visible = false;
                                                    this.open_online_browse(artist.clone(), cx);
                                                })),
                                        )
                                    })
                                    .when_some(album_item, |actions, album| {
                                        actions.child(
                                            Button::new("now-playing-open-album")
                                                .ghost()
                                                .label("Album")
                                                .on_click(cx.listener(move |this, _, _, cx| {
                                                    this.now_playing_visible = false;
                                                    this.open_online_browse(album.clone(), cx);
                                                })),
                                        )
                                    }),
                            )
                            .when_some(self.library_error.clone(), |panel, message| {
                                panel.child(
                                    div().text_xs().text_color(cx.theme().danger).child(message),
                                )
                            })
                            .when_some(self.cloud_library_error.clone(), |panel, message| {
                                panel.child(
                                    div().text_xs().text_color(cx.theme().danger).child(message),
                                )
                            }),
                    )
                    .child(
                        v_flex()
                            .h_full()
                            .min_w(px(320.))
                            .flex_1()
                            .min_h_0()
                            .overflow_hidden()
                            .rounded(cx.theme().radius_lg)
                            .border_1()
                            .border_color(cx.theme().border)
                            .child(
                                TabBar::new("now-playing-tabs")
                                    .w_full()
                                    .underline()
                                    .selected_index(self.now_playing_tab.index())
                                    .on_click(cx.listener(|this, index: &usize, _, cx| {
                                        this.select_now_playing_tab(*index, cx)
                                    }))
                                    .child(Tab::new().flex_1().label(format!(
                                        "Up next ({})",
                                        self.queue.remaining_after_current()
                                    )))
                                    .child(
                                        Tab::new()
                                            .flex_1()
                                            .label("Lyrics")
                                            .disabled(song.is_episode),
                                    )
                                    .child(Tab::new().flex_1().label("Related")),
                            )
                            .child(pane),
                    ),
            )
            .into_any_element()
    }

    fn render_player(&self, window: &Window, cx: &mut Context<Self>) -> impl IntoElement {
        let compact = window.viewport_size().width < px(1_050.);
        let snapshot = self.audio_player.snapshot();
        let state = if self.resolving_playback {
            PlaybackState::Loading
        } else if self.playback_error.is_some() {
            PlaybackState::Failed
        } else {
            snapshot.state
        };
        let title = self
            .current_song
            .as_ref()
            .map_or_else(|| "Nothing playing".to_owned(), |song| song.title.clone());
        let subtitle = self
            .playback_error
            .as_ref()
            .or(snapshot.error.as_ref())
            .cloned()
            .unwrap_or_else(|| match state {
                PlaybackState::Loading => "Resolving and buffering audio…".into(),
                _ => self.current_song.as_ref().map_or_else(
                    || "Choose a song to begin".into(),
                    |song| song.artist_line(),
                ),
            });
        let duration = snapshot
            .duration
            .or_else(|| self.current_song.as_ref().and_then(|song| song.duration));
        let display_position = self
            .seek_preview
            .or(self.pending_resume_position)
            .unwrap_or(snapshot.position);
        let host_controlled = self.listen_together_is_guest();
        let can_toggle =
            !host_controlled && self.current_song.is_some() && state != PlaybackState::Loading;
        let can_seek = !host_controlled
            && duration.is_some()
            && matches!(state, PlaybackState::Playing | PlaybackState::Paused);
        let play_icon = if state == PlaybackState::Playing {
            IconName::Pause
        } else {
            IconName::Play
        };
        let has_current_song = self.current_song.is_some();
        let radio_matches_current = self
            .current_song
            .as_ref()
            .is_some_and(|song| radio_state_matches_song(&self.radio_state, &song.video_id));
        let radio_loading_current =
            radio_matches_current && matches!(self.radio_state, RadioQueueState::Loading(_));

        h_flex()
            .h(if compact { px(160.) } else { px(88.) })
            .flex_shrink_0()
            .flex_wrap()
            .border_t_1()
            .border_color(cx.theme().border)
            .bg(cx.theme().background)
            .when(!compact, |bar| bar.px_5().gap_5())
            .when(compact, |bar| bar.px_4().py_2().gap_3())
            .items_center()
            .child(
                h_flex()
                    .id("open-now-playing")
                    .w(if compact { px(220.) } else { px(240.) })
                    .gap_3()
                    .items_center()
                    .cursor_pointer()
                    .rounded(cx.theme().radius)
                    .hover(|style| style.bg(cx.theme().muted.opacity(0.55)))
                    .on_click(cx.listener(|this, _, _, cx| this.open_now_playing(cx)))
                    .child(
                        self.render_thumbnail(
                            self.current_song
                                .as_ref()
                                .and_then(|song| song.thumbnail_url.as_deref()),
                            px(48.),
                            IconName::Play,
                            cx,
                        ),
                    )
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .child(div().text_sm().font_medium().child(title))
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(subtitle),
                            ),
                    )
                    .when(has_current_song, |metadata| {
                        metadata.child(Icon::new(IconName::Maximize).size_4())
                    }),
            )
            .child(
                v_flex()
                    .flex_1()
                    .min_w(px(360.))
                    .gap_2()
                    .items_center()
                    .child(
                        h_flex()
                            .gap_2()
                            .child(
                                Button::new("shuffle")
                                    .ghost()
                                    .label("Shuffle")
                                    .tooltip("Randomize the remaining queue")
                                    .selected(self.shuffle_enabled)
                                    .disabled(host_controlled || self.queue.is_empty())
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.toggle_shuffle(cx);
                                    })),
                            )
                            .child(
                                Button::new("previous")
                                    .ghost()
                                    .label(if compact { "Prev" } else { "Previous" })
                                    .disabled(
                                        host_controlled
                                            || self.current_song.is_none()
                                            || (!self.queue.has_previous()
                                                && self.repeat_mode != RepeatMode::All
                                                && snapshot.position < Duration::from_secs(5)),
                                    )
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.play_previous(window, cx);
                                    })),
                            )
                            .child(
                                Button::new("play")
                                    .primary()
                                    .icon(play_icon)
                                    .tooltip(if state == PlaybackState::Playing {
                                        "Pause"
                                    } else {
                                        "Play"
                                    })
                                    .disabled(!can_toggle)
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.toggle_playback(window, cx);
                                    })),
                            )
                            .child(
                                Button::new("next")
                                    .ghost()
                                    .label("Next")
                                    .disabled(
                                        host_controlled
                                            || self.current_song.is_none()
                                            || (!self.queue.has_next()
                                                && self.repeat_mode != RepeatMode::All),
                                    )
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.play_next(window, cx);
                                    })),
                            )
                            .child(
                                Button::new("repeat")
                                    .ghost()
                                    .label(self.repeat_mode.label())
                                    .tooltip("Cycle repeat off, repeat all, and repeat one")
                                    .selected(self.repeat_mode != RepeatMode::Off)
                                    .disabled(host_controlled || self.queue.is_empty())
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.cycle_repeat_mode(cx);
                                    })),
                            ),
                    )
                    .child(
                        h_flex()
                            .w_full()
                            .gap_3()
                            .items_center()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(format_duration(display_position))
                            .child(
                                div().flex_1().child(
                                    Slider::new(&self.progress_slider)
                                        .horizontal()
                                        .disabled(!can_seek),
                                ),
                            )
                            .child(
                                duration
                                    .map(format_duration)
                                    .unwrap_or_else(|| "0:00".into()),
                            ),
                    ),
            )
            .child(
                v_flex()
                    .w(px(300.))
                    .when(compact, |panel| panel.w_full())
                    .gap_1()
                    .child(
                        h_flex()
                            .w_full()
                            .gap_2()
                            .items_center()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(format!("Volume {:.0}%", snapshot.volume * 100.0))
                            .child(
                                div().flex_1().min_w(px(96.)).child(
                                    Slider::new(&self.volume_slider)
                                        .horizontal()
                                        .disabled(host_controlled),
                                ),
                            )
                            .when_some(snapshot.normalization_gain_mb, |labels, gain_mb| {
                                labels.child(
                                    div().child(format!("{:+.1} dB", gain_mb as f32 / 100.0)),
                                )
                            })
                            .when(snapshot.equalizer_active, |labels| {
                                labels.child(div().child("EQ"))
                            }),
                    )
                    .child(
                        h_flex()
                            .w_full()
                            .justify_end()
                            .gap_1()
                            .child(
                                Button::new("playback-parameters")
                                    .ghost()
                                    .label(format!(
                                        "{:.2}×",
                                        snapshot.playback_parameters.tempo_ratio()
                                    ))
                                    .tooltip(format!(
                                        "Speed & pitch: {}",
                                        format_playback_parameters(snapshot.playback_parameters)
                                    ))
                                    .selected(self.playback_parameters_visible)
                                    .disabled(self.current_song.is_none())
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.toggle_playback_parameters_panel(cx);
                                    })),
                            )
                            .child(
                                Button::new("radio")
                                    .ghost()
                                    .label("Radio")
                                    .selected(radio_matches_current)
                                    .disabled(
                                        host_controlled
                                            || self.current_song.is_none()
                                            || self
                                                .current_song
                                                .as_ref()
                                                .is_some_and(|song| song.is_episode)
                                            || radio_loading_current,
                                    )
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.start_radio_from_current(window, cx);
                                    })),
                            )
                            .child(
                                Button::new("lyrics")
                                    .ghost()
                                    .label("Lyrics")
                                    .selected(self.lyrics_visible)
                                    .disabled(
                                        self.current_song
                                            .as_ref()
                                            .is_none_or(|song| song.is_episode),
                                    )
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.toggle_lyrics(cx);
                                    })),
                            )
                            .child(
                                Button::new("queue")
                                    .ghost()
                                    .label(format!("Queue ({})", self.queue.len()))
                                    .selected(self.queue_visible)
                                    .disabled(self.queue.is_empty())
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.playlist_picker_song = None;
                                        this.cloud_playlist_picker_song = None;
                                        this.lyrics_visible = false;
                                        this.playback_parameters_visible = false;
                                        if matches!(this.lyrics_state, LyricsViewState::Loading(_))
                                        {
                                            this.lyrics_state = LyricsViewState::Idle;
                                        }
                                        this.lyrics_task = None;
                                        this.queue_visible = !this.queue_visible;
                                        cx.notify();
                                    })),
                            ),
                    ),
            )
    }
}

impl Drop for MetrolistShell {
    fn drop(&mut self) {
        if let Some(cancellation) = self.recognition_cancellation.take() {
            cancellation.cancel();
        }
    }
}

impl Render for MetrolistShell {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let side_panel = if self.playlist_picker_song.is_some() {
            Some(self.render_playlist_picker(cx))
        } else if self.cloud_playlist_picker_song.is_some() {
            Some(self.render_cloud_playlist_picker(cx))
        } else if self.playback_parameters_visible {
            Some(self.render_playback_parameters_panel(cx))
        } else if self.lyrics_visible {
            Some(self.render_lyrics_panel(cx))
        } else if self.queue_visible {
            Some(self.render_queue_panel(cx))
        } else {
            None
        };
        let upper_content = if self.now_playing_visible {
            self.render_now_playing(window, cx)
        } else {
            h_flex()
                .flex_1()
                .min_h_0()
                .child(self.render_sidebar(cx))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .h_full()
                        .overflow_y_scrollbar()
                        .p_7()
                        .child(self.render_page(cx)),
                )
                .when_some(side_panel, |layout, panel| layout.child(panel))
                .into_any_element()
        };

        v_flex()
            .size_full()
            .min_w(px(720.))
            .min_h(px(520.))
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .child(upper_content)
            .child(self.render_player(window, cx))
    }
}

fn lyrics_state_video_id(state: &LyricsViewState) -> Option<&str> {
    match state {
        LyricsViewState::Idle => None,
        LyricsViewState::Loading(video_id)
        | LyricsViewState::Loaded(video_id, _)
        | LyricsViewState::Unavailable(video_id)
        | LyricsViewState::Failed(video_id, _) => Some(video_id),
    }
}

fn lyrics_state_matches_song(state: &LyricsViewState, video_id: &str) -> bool {
    lyrics_state_video_id(state) == Some(video_id)
}

fn lyrics_request_matches_current(current: Option<&Song>, requested_video_id: &str) -> bool {
    current.is_some_and(|song| song.video_id == requested_video_id)
}

fn radio_state_seed_video_id(state: &RadioQueueState) -> Option<&str> {
    match state {
        RadioQueueState::Idle => None,
        RadioQueueState::Loading(request) | RadioQueueState::Failed(request, _) => {
            Some(request.seed_video_id())
        }
        RadioQueueState::Active(session) | RadioQueueState::Exhausted(session) => {
            Some(&session.seed_video_id)
        }
    }
}

fn radio_state_matches_song(state: &RadioQueueState, video_id: &str) -> bool {
    radio_state_seed_video_id(state) == Some(video_id)
}

fn accept_radio_continuation(
    seen: &mut HashSet<String>,
    requested: Option<String>,
    returned: Option<String>,
) -> Option<String> {
    if let Some(requested) = requested {
        seen.insert(requested);
    }
    returned.filter(|token| !token.trim().is_empty() && !seen.contains(token))
}

fn choose_playback_source(
    video_id: &str,
    active: Option<&ActivePlaybackSource>,
    persisted: Option<&PersistedPlaybackSource>,
    audio_quality: AudioQuality,
    now_ms: i64,
) -> Option<(PlaybackSource, PlaybackSourceAttempt)> {
    if let Some(active) = active.filter(|source| {
        source.video_id == video_id
            && source.expires_at_ms > now_ms
            && source.source.access == PlaybackSourceAccess::NetworkAndCache
    }) {
        return Some((active.source.clone(), PlaybackSourceAttempt::Network));
    }

    persisted
        .filter(|source| source.video_id == video_id && source.content_length > 0)
        .map(|source| {
            (
                PlaybackSource::cache_only(
                    audio_quality.playback_cache_key(&source.video_id),
                    source.mime_type.clone(),
                    source.content_length,
                )
                .with_loudness_lufs_mb(source.loudness_lufs_mb),
                PlaybackSourceAttempt::CacheOnly,
            )
        })
}

fn persisted_playback_source(
    video_id: &str,
    resolved: &ResolvedPlayback,
    audio_quality: AudioQuality,
    resolved_at_ms: i64,
    expires_at_ms: i64,
) -> Option<PersistedPlaybackSource> {
    let content_length = resolved
        .source
        .content_length
        .filter(|length| *length > 0)?;
    if resolved.source.cache_key.as_deref()
        != Some(audio_quality.playback_cache_key(video_id).as_str())
        || !resolved.source.mime_type.starts_with("audio/")
        || expires_at_ms < resolved_at_ms
    {
        return None;
    }
    Some(PersistedPlaybackSource {
        video_id: video_id.to_owned(),
        mime_type: resolved.source.mime_type.clone(),
        content_length,
        loudness_lufs_mb: resolved.source.loudness_lufs_mb,
        resolved_at_ms,
        expires_at_ms,
    })
}

fn playback_source_expiration(resolved_at_ms: i64, expires_in: Duration) -> i64 {
    let expires_in_ms = i64::try_from(expires_in.as_millis()).unwrap_or(i64::MAX);
    resolved_at_ms.saturating_add(expires_in_ms)
}

fn unix_time_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|duration| i64::try_from(duration.as_millis()).ok())
        .unwrap_or_default()
}

fn unix_time_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or_default()
}

fn format_duration(duration: std::time::Duration) -> String {
    let total = duration.as_secs();
    let hours = total / 3_600;
    let minutes = (total % 3_600) / 60;
    let seconds = total % 60;
    if hours == 0 {
        format!("{minutes}:{seconds:02}")
    } else {
        format!("{hours}:{minutes:02}:{seconds:02}")
    }
}

fn format_equalizer_frequency(frequency_hz: u32) -> String {
    if frequency_hz >= 1_000 {
        if frequency_hz.is_multiple_of(1_000) {
            format!("{} kHz", frequency_hz / 1_000)
        } else {
            format!("{:.1} kHz", frequency_hz as f32 / 1_000.0)
        }
    } else {
        format!("{frequency_hz} Hz")
    }
}

fn format_download_bytes(bytes: u64) -> String {
    const KIB: u64 = 1024;
    const MIB: u64 = 1024 * KIB;
    const GIB: u64 = 1024 * MIB;
    if bytes >= GIB {
        format!("{:.1} GiB", bytes as f64 / GIB as f64)
    } else if bytes >= MIB {
        format!("{:.1} MiB", bytes as f64 / MIB as f64)
    } else if bytes >= KIB {
        format!("{:.1} KiB", bytes as f64 / KIB as f64)
    } else {
        format!("{bytes} B")
    }
}

fn collect_thumbnail_url(url: Option<&str>, urls: &mut HashSet<String>) {
    if let Some(url) = url.map(str::trim).filter(|url| !url.is_empty()) {
        urls.insert(url.to_owned());
    }
}

fn format_history_age(played_at_ms: i64) -> String {
    let now_ms = unix_time_ms();
    let age_seconds = now_ms.saturating_sub(played_at_ms) / 1_000;
    match age_seconds {
        ..=59 => "Just now".into(),
        60..=3_599 => format!("{}m ago", age_seconds / 60),
        3_600..=86_399 => format!("{}h ago", age_seconds / 3_600),
        _ => format!("{}d ago", age_seconds / 86_400),
    }
}

fn search_suggestion_request_is_current(
    current_generation: u64,
    request_generation: u64,
    current_query: &str,
    request_query: &str,
) -> bool {
    current_generation == request_generation && current_query.trim() == request_query
}

fn extend_radio_recommendations(
    recommendations: &mut Vec<Song>,
    seed_video_id: &str,
    songs: &[Song],
) {
    for song in songs {
        if song.video_id != seed_video_id
            && !recommendations
                .iter()
                .any(|existing| existing.video_id == song.video_id)
        {
            recommendations.push(song.clone());
        }
    }
}

fn queue_end_action(repeat_mode: RepeatMode, has_current: bool, has_next: bool) -> QueueEndAction {
    if !has_current {
        return QueueEndAction::Stop;
    }
    match repeat_mode {
        RepeatMode::One => QueueEndAction::Replay,
        RepeatMode::All if has_next => QueueEndAction::Advance,
        RepeatMode::All => QueueEndAction::Wrap,
        RepeatMode::Off if has_next => QueueEndAction::Advance,
        RepeatMode::Off => QueueEndAction::Stop,
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use super::{
        AccountOperation, AccountViewState, ActivePlaybackSource, CloudLibraryData,
        HistoryViewState, LocalLibraryRetryTargets, LyricsViewState, MetrolistShell,
        PlaybackParameterChange, PlaybackSourceAttempt, QueueEndAction, SleepTimer,
        StoredViewState, accept_radio_continuation, adjusted_playback_parameters,
        choose_playback_source, extend_radio_recommendations, format_playback_parameters,
        lyrics_request_matches_current, lyrics_state_matches_song, persisted_playback_source,
        playback_source_expiration, queue_end_action, radio_state_matches_song,
        search_suggestion_request_is_current, sidebar_account_summary,
    };
    use crate::services::innertube::{AccountProfile, ResolvedPlayback};
    use crate::services::{ListenTogetherTrack, PlaybackSource, PlaybackSourceAccess, RepeatMode};
    use crate::storage::PersistedPlaybackSource;
    use crate::{
        AudioQuality, MAX_PLAYBACK_RATE_MILLI, MIN_PLAYBACK_RATE_MILLI, PlaybackParameters,
        domain::{BrowseItem, BrowseKind, Song},
    };
    use std::collections::HashSet;

    fn online_source(url: &str) -> PlaybackSource {
        PlaybackSource {
            url: url.into(),
            mime_type: "audio/mp4; codecs=mp4a.40.2".into(),
            content_length: Some(4_200_000),
            loudness_lufs_mb: Some(-1_325),
            request_headers: Vec::new(),
            cache_key: Some(AudioQuality::Auto.playback_cache_key("song")),
            access: PlaybackSourceAccess::NetworkAndCache,
        }
    }

    fn persisted_source() -> PersistedPlaybackSource {
        PersistedPlaybackSource {
            video_id: "song".into(),
            mime_type: "audio/mp4; codecs=mp4a.40.2".into(),
            content_length: 4_200_000,
            loudness_lufs_mb: Some(-1_325),
            resolved_at_ms: 100,
            expires_at_ms: 200,
        }
    }

    fn cloud_song(video_id: &str) -> Song {
        Song {
            video_id: video_id.into(),
            title: format!("Song {video_id}"),
            artists: Vec::new(),
            duration: None,
            thumbnail_url: None,
            album: None,
            is_episode: false,
        }
    }

    fn cloud_playlist(browse_id: &str) -> BrowseItem {
        BrowseItem {
            browse_id: browse_id.into(),
            kind: BrowseKind::Playlist,
            title: format!("Playlist {browse_id}"),
            subtitle: "Fixture".into(),
            thumbnail_url: None,
            params: None,
            editable: false,
        }
    }

    #[test]
    fn only_a_verified_account_state_allows_cloud_operations() {
        let profile = AccountProfile {
            name: "Fixture Listener".into(),
            email: None,
            channel_handle: None,
            thumbnail_url: None,
        };

        assert!(AccountViewState::SignedIn(profile).is_verified());
        assert!(!AccountViewState::SignedOut.is_verified());
        assert!(!AccountViewState::Checking.is_verified());
        assert!(!AccountViewState::Expired("expired".into()).is_verified());
        assert!(!AccountViewState::Failed("offline".into()).is_verified());
    }

    #[test]
    fn sidebar_account_copy_tracks_the_real_account_state() {
        assert_eq!(
            sidebar_account_summary(&AccountViewState::SignedOut, AccountOperation::Idle),
            (
                "YouTube Music".into(),
                "Anonymous search and playback".into()
            )
        );
        assert_eq!(
            sidebar_account_summary(&AccountViewState::Checking, AccountOperation::Idle).1,
            "Checking saved account…"
        );
        assert_eq!(
            sidebar_account_summary(&AccountViewState::Checking, AccountOperation::SigningIn).1,
            "Complete sign-in in the browser window…"
        );

        let signed_in = AccountViewState::SignedIn(AccountProfile {
            name: "Fixture Listener".into(),
            email: Some("listener@example.test".into()),
            channel_handle: Some("@fixture".into()),
            thumbnail_url: None,
        });
        assert_eq!(
            sidebar_account_summary(&signed_in, AccountOperation::Idle),
            (
                "Fixture Listener".into(),
                "listener@example.test · @fixture".into()
            )
        );
        assert_eq!(
            sidebar_account_summary(
                &AccountViewState::Expired("expired".into()),
                AccountOperation::Idle
            ),
            (
                "Account needs attention".into(),
                "Open Settings to reconnect".into()
            )
        );
        assert_eq!(
            sidebar_account_summary(
                &AccountViewState::Failed("offline".into()),
                AccountOperation::Idle
            )
            .1,
            "Anonymous playback remains available"
        );
    }

    #[test]
    fn local_library_retry_targets_only_failed_sections() {
        let targets = LocalLibraryRetryTargets::from_states(
            &HistoryViewState::Failed("history offline".into()),
            &StoredViewState::Loaded(Vec::new()),
            &StoredViewState::Failed("podcasts offline".into()),
            &StoredViewState::Loading,
            &StoredViewState::Loaded(Vec::new()),
            &StoredViewState::Failed("downloads offline".into()),
        );

        assert_eq!(
            targets,
            LocalLibraryRetryTargets {
                history: true,
                favorites: false,
                podcasts: true,
                episodes: false,
                playlists: false,
                downloads: true,
            }
        );
        assert!(targets.any());

        let healthy = LocalLibraryRetryTargets::from_states(
            &HistoryViewState::Loaded(Vec::new()),
            &StoredViewState::Loaded(Vec::new()),
            &StoredViewState::Loaded(Vec::new()),
            &StoredViewState::Loaded(Vec::new()),
            &StoredViewState::Loaded(Vec::new()),
            &StoredViewState::Loaded(Vec::new()),
        );
        assert_eq!(healthy, LocalLibraryRetryTargets::default());
        assert!(!healthy.any());
    }

    #[test]
    fn stale_search_suggestions_cannot_replace_the_current_query() {
        assert!(search_suggestion_request_is_current(
            7, 7, "  daft  ", "daft"
        ));
        assert!(!search_suggestion_request_is_current(8, 7, "daft", "daft"));
        assert!(!search_suggestion_request_is_current(
            7,
            7,
            "daft punk",
            "daft"
        ));
    }

    #[test]
    fn queue_end_behavior_respects_off_all_and_one_repeat_modes() {
        assert_eq!(
            queue_end_action(RepeatMode::Off, true, true),
            QueueEndAction::Advance
        );
        assert_eq!(
            queue_end_action(RepeatMode::Off, true, false),
            QueueEndAction::Stop
        );
        assert_eq!(
            queue_end_action(RepeatMode::All, true, true),
            QueueEndAction::Advance
        );
        assert_eq!(
            queue_end_action(RepeatMode::All, true, false),
            QueueEndAction::Wrap
        );
        assert_eq!(
            queue_end_action(RepeatMode::One, true, true),
            QueueEndAction::Replay
        );
        assert_eq!(
            queue_end_action(RepeatMode::One, true, false),
            QueueEndAction::Replay
        );
        assert_eq!(
            queue_end_action(RepeatMode::All, false, false),
            QueueEndAction::Stop
        );
    }

    #[test]
    fn sleep_timer_supports_deadlines_and_end_of_song() {
        let now = Instant::now();
        let deadline = SleepTimer::Deadline(now + Duration::from_secs(90));

        assert_eq!(deadline.summary(now), "Stops in 1:30");
        assert!(!deadline.deadline_reached(now + Duration::from_secs(89)));
        assert!(deadline.deadline_reached(now + Duration::from_secs(90)));
        assert!(!deadline.stops_after_song());
        assert!(SleepTimer::EndOfSong.stops_after_song());
        assert_eq!(
            SleepTimer::EndOfSong.summary(now),
            "Stops when this song ends"
        );
    }

    #[test]
    fn full_player_related_list_keeps_unique_non_seed_radio_songs() {
        let mut recommendations = vec![cloud_song("existing")];

        extend_radio_recommendations(
            &mut recommendations,
            "seed",
            &[
                cloud_song("seed"),
                cloud_song("existing"),
                cloud_song("new"),
                cloud_song("new"),
            ],
        );

        assert_eq!(
            recommendations
                .iter()
                .map(|song| song.video_id.as_str())
                .collect::<Vec<_>>(),
            ["existing", "new"]
        );
    }

    #[test]
    fn full_player_related_state_is_scoped_to_the_current_song() {
        let loading = super::RadioQueueState::Loading(super::RadioRequest::Initial {
            seed_video_id: "seed".into(),
            replace_future: true,
        });

        assert!(radio_state_matches_song(&loading, "seed"));
        assert!(!radio_state_matches_song(&loading, "next-song"));
        assert!(!radio_state_matches_song(
            &super::RadioQueueState::Idle,
            "seed"
        ));
    }

    #[test]
    fn optimistic_cloud_library_changes_are_idempotent_and_exactly_rollbackable() {
        let original = CloudLibraryData {
            liked_songs: vec![cloud_song("liked")],
            library_songs: Vec::new(),
            uploaded_songs: Vec::new(),
            playlists: vec![cloud_playlist("VLPL-liked")],
            albums: Vec::new(),
            uploaded_albums: Vec::new(),
            artists: Vec::new(),
        };
        let mut optimistic = original.clone();

        optimistic.set_video_liked(cloud_song("new"), true);
        optimistic.set_video_liked(cloud_song("new"), true);
        optimistic.set_playlist_liked(cloud_playlist("PL-liked"), false);

        assert_eq!(
            optimistic
                .liked_songs
                .iter()
                .filter(|song| song.video_id == "new")
                .count(),
            1
        );
        assert!(!optimistic.playlist_liked("VLPL-liked"));

        optimistic = original.clone();
        assert_eq!(optimistic, original);
        assert!(optimistic.video_liked("liked"));
        assert!(optimistic.playlist_liked("PL-liked"));
    }

    #[test]
    fn fresh_in_memory_stream_is_reused_before_cache_only_recovery() {
        let active = ActivePlaybackSource {
            video_id: "song".into(),
            source: online_source("https://example.invalid/stream?token=secret"),
            expires_at_ms: 200,
            playback_tracking: None,
        };
        let (source, attempt) = choose_playback_source(
            "song",
            Some(&active),
            Some(&persisted_source()),
            AudioQuality::Auto,
            199,
        )
        .unwrap();

        assert_eq!(attempt, PlaybackSourceAttempt::Network);
        assert_eq!(source.access, PlaybackSourceAccess::NetworkAndCache);
        assert_eq!(source.loudness_lufs_mb, Some(-1_325));
        assert!(source.url.contains("secret"));
    }

    #[test]
    fn expired_stream_is_never_reused_but_its_cache_metadata_is() {
        let active = ActivePlaybackSource {
            video_id: "song".into(),
            source: online_source("https://example.invalid/stream?token=secret"),
            expires_at_ms: 200,
            playback_tracking: None,
        };
        let (source, attempt) = choose_playback_source(
            "song",
            Some(&active),
            Some(&persisted_source()),
            AudioQuality::Low,
            200,
        )
        .unwrap();

        assert_eq!(attempt, PlaybackSourceAttempt::CacheOnly);
        assert_eq!(source.access, PlaybackSourceAccess::CacheOnly);
        assert!(source.url.is_empty());
        assert!(source.request_headers.is_empty());
        assert_eq!(source.cache_key.as_deref(), Some("song-quality-low"));
        assert_eq!(source.loudness_lufs_mb, Some(-1_325));
    }

    #[test]
    fn persisted_metadata_is_built_without_ephemeral_stream_secrets() {
        let resolved = ResolvedPlayback {
            source: online_source("https://example.invalid/stream?token=secret"),
            expires_in: Duration::from_secs(60),
            playback_tracking: None,
        };
        let metadata =
            persisted_playback_source("song", &resolved, AudioQuality::Auto, 100, 60_100).unwrap();

        assert_eq!(metadata.video_id, "song");
        assert_eq!(metadata.content_length, 4_200_000);
        assert_eq!(metadata.loudness_lufs_mb, Some(-1_325));
        assert_eq!(metadata.expires_at_ms, 60_100);
        let debug = format!("{metadata:?}");
        assert!(!debug.contains("secret"));
        assert!(!debug.contains("example.invalid"));
    }

    #[test]
    fn playback_source_expiration_saturates_instead_of_wrapping() {
        assert_eq!(
            playback_source_expiration(i64::MAX - 1, Duration::from_secs(10)),
            i64::MAX
        );
    }

    #[test]
    fn live_playback_adjustments_match_android_steps_and_safe_modes() {
        let mut parameters = PlaybackParameters::default();
        for _ in 0..100 {
            parameters =
                adjusted_playback_parameters(parameters, PlaybackParameterChange::SpeedDown);
        }
        assert_eq!(parameters.tempo_milli, MIN_PLAYBACK_RATE_MILLI);
        for _ in 0..100 {
            parameters = adjusted_playback_parameters(parameters, PlaybackParameterChange::SpeedUp);
        }
        assert_eq!(parameters.tempo_milli, MAX_PLAYBACK_RATE_MILLI);

        parameters.transpose_semitones = 5;
        parameters =
            adjusted_playback_parameters(parameters, PlaybackParameterChange::VarispeedMode);
        assert!(parameters.varispeed);
        assert_eq!(parameters.transpose_semitones, 0);
        assert_eq!(
            adjusted_playback_parameters(parameters, PlaybackParameterChange::TransposeUp,),
            parameters
        );
        assert_eq!(format_playback_parameters(parameters), "2.00× varispeed");

        parameters = adjusted_playback_parameters(parameters, PlaybackParameterChange::NormalMode);
        parameters =
            adjusted_playback_parameters(parameters, PlaybackParameterChange::TransposeDown);
        assert_eq!(parameters.transpose_semitones, -1);
        assert_eq!(format_playback_parameters(parameters), "2.00× · -1 st");
        assert_eq!(
            adjusted_playback_parameters(parameters, PlaybackParameterChange::Reset),
            PlaybackParameters::default()
        );
        assert!(parameters.validate().is_ok());
    }

    #[test]
    fn a_late_lyrics_result_cannot_replace_the_new_current_song() {
        let current = Song {
            video_id: "new-song".into(),
            title: "New song".into(),
            artists: Vec::new(),
            duration: None,
            thumbnail_url: None,
            album: None,
            is_episode: false,
        };

        assert!(!lyrics_request_matches_current(Some(&current), "old-song"));
        assert!(lyrics_request_matches_current(Some(&current), "new-song"));
        assert!(lyrics_state_matches_song(
            &LyricsViewState::Loading("new-song".into()),
            "new-song"
        ));
        assert!(!lyrics_state_matches_song(
            &LyricsViewState::Idle,
            "new-song"
        ));
    }

    #[test]
    fn radio_continuations_stop_on_blank_or_repeated_tokens() {
        let mut seen = HashSet::new();
        assert_eq!(
            accept_radio_continuation(&mut seen, None, Some("first".into())),
            Some("first".into())
        );
        assert_eq!(
            accept_radio_continuation(&mut seen, Some("first".into()), Some("second".into())),
            Some("second".into())
        );
        assert!(seen.contains("first"));
        assert_eq!(
            accept_radio_continuation(&mut seen, Some("second".into()), Some("second".into())),
            None
        );
        assert_eq!(
            accept_radio_continuation(&mut seen, None, Some("  ".into())),
            None
        );
    }

    #[test]
    fn guest_room_queue_keeps_episode_types_for_non_music_guards() {
        let mut episode = cloud_song("episode");
        episode.is_episode = true;
        let current = ListenTogetherTrack::from_song(&episode);
        let following_song = ListenTogetherTrack::from_song(&cloud_song("song"));
        let duplicate_current = current.clone();

        let songs = MetrolistShell::canonical_guest_songs(
            Some(&current),
            &[following_song, duplicate_current],
        );

        assert_eq!(songs.len(), 2);
        assert_eq!(songs[0].video_id, "episode");
        assert!(songs[0].is_episode);
        assert_eq!(songs[1].video_id, "song");
        assert!(!songs[1].is_episode);
    }
}
