mod account_login;
mod audio;
pub mod auth;
mod autoeq;
mod cache;
mod desktop_media;
mod discord_presence;
mod download;
mod equalizer;
mod http;
pub mod innertube;
mod lastfm;
mod listen_together;
mod lyrics;
mod playback;
mod recognition;
mod thumbnail;

use std::sync::Arc;

use crate::{AppError, AppSettings, Result};

pub use account_login::{
    account_login_helper_requested, launch_account_login, run_account_login_helper,
};
pub use audio::{DEFAULT_AUDIO_CACHE_BYTES, DesktopAudioPlayer};
pub use auth::{AuthSession, CredentialStore, SystemCredentialStore};
pub use autoeq::{
    AUTO_EQ_CACHE_TTL, AutoEqClient, AutoEqEntry, AutoEqIndex, AutoEqIndexOrigin, AutoEqModel,
    MAX_AUTO_EQ_NAME_INDEX_BYTES, MAX_AUTO_EQ_SEARCH_RESULTS, MAX_AUTO_EQ_TREE_BYTES,
    normalize_model_name, search_auto_eq_models,
};
pub use cache::{AudioCache, DownloadedAudioStore};
pub use desktop_media::{DesktopMediaCommand, DesktopMediaSession, DesktopSeekDirection};
pub use discord_presence::{
    DISCORD_APPLICATION_ID, DiscordPlaybackObservation, DiscordPlaybackState,
    DiscordPresenceAction, DiscordPresenceService, DiscordPresenceSnapshot, DiscordPresenceState,
    DiscordPresenceTracker,
};
pub use download::{DownloadOutcome, DownloadReceipt, DownloadUpdate, download_song};
pub use equalizer::{
    EQUALIZER_RESPONSE_DB_STEP, EQUALIZER_RESPONSE_MAX_FREQUENCY_HZ,
    EQUALIZER_RESPONSE_MIN_FREQUENCY_HZ, EQUALIZER_RESPONSE_POINT_COUNT,
    EQUALIZER_RESPONSE_SAMPLE_RATE, EqualizerFrequencyResponse, EqualizerResponsePoint,
    MAX_EQUALIZER_APO_FILE_BYTES, equalizer_frequency_response, format_equalizer_apo,
    parse_equalizer_apo,
};
pub use http::build_http_client;
pub use lastfm::{
    LastFmApiCredentials, LastFmClient, LastFmCredentialStore, LastFmPlaybackAction,
    LastFmPlaybackObservation, LastFmPlaybackTracker, LastFmScrobblePolicy, LastFmSession,
    LastFmSystemCredentialStore, LastFmTrack,
};
pub use listen_together::{
    DEFAULT_LISTEN_TOGETHER_SERVER_URL, ListenTogetherClient, ListenTogetherConnectionState,
    ListenTogetherEvent, ListenTogetherLocalPlaybackState, ListenTogetherPlaybackAction,
    ListenTogetherPlaybackActionPayload, ListenTogetherPlaybackObservation,
    ListenTogetherPlaybackTracker, ListenTogetherRoomRole, ListenTogetherRoomState,
    ListenTogetherSnapshot, ListenTogetherSuggestion, ListenTogetherTrack, ListenTogetherUser,
};
pub use lyrics::{LyricsClient, parse_lrc};
pub(crate) use playback::RANGE_CHUNK_SIZE;
pub use playback::{
    AudioDeviceOperation, AudioDeviceSnapshot, AudioOutputDevice, AudioPlayer, DecodedAudioInfo,
    HttpRangeMediaSource, PlaybackSnapshot, PlaybackSource, PlaybackSourceAccess, PlaybackState,
    Queue, QueueItem, RepeatMode, probe_audio_bytes, probe_audio_source,
};
pub use recognition::{
    MicrophoneCancellation, MicrophoneCapture, MicrophoneRecorder, RECOGNITION_CAPTURE_DURATION,
    RECOGNITION_SAMPLE_RATE, RecognitionClient, RecognitionResult, RecordedPcm, ShazamSignature,
    SystemMicrophoneRecorder, generate_shazam_signature, linear_resample_mono_i16,
};
pub use thumbnail::{DEFAULT_THUMBNAIL_CACHE_BYTES, ThumbnailCache, ThumbnailImage};

pub struct DesktopServices {
    pub innertube: Arc<innertube::InnerTubeClient>,
    pub lyrics: Arc<LyricsClient>,
    pub thumbnails: ThumbnailCache,
    pub audio_cache: Arc<AudioCache>,
    pub downloaded_audio: Arc<DownloadedAudioStore>,
    pub audio: DesktopAudioPlayer,
    pub microphone: Arc<dyn MicrophoneRecorder>,
    pub recognition: Arc<RecognitionClient>,
}

impl DesktopServices {
    pub fn with_settings(settings: &AppSettings) -> Result<Self> {
        Self::with_settings_and_auth(settings, None)
    }

    pub fn with_settings_and_auth(
        settings: &AppSettings,
        auth: Option<AuthSession>,
    ) -> Result<Self> {
        let settings = settings.clone().validate()?;
        let audio_cache = Arc::new(
            AudioCache::new(settings.audio_cache_root(), settings.audio_cache_bytes).map_err(
                |error| AppError::InvalidConfig(format!("audio cache is unavailable: {error}")),
            )?,
        );
        let downloaded_audio =
            Arc::new(DownloadedAudioStore::for_current_user().map_err(|error| {
                AppError::InvalidConfig(format!("downloaded audio store is unavailable: {error}"))
            })?);
        let audio = DesktopAudioPlayer::with_settings_and_stores(
            &settings,
            audio_cache.clone(),
            downloaded_audio.clone(),
        )?;
        Ok(Self {
            innertube: Arc::new(innertube::InnerTubeClient::with_settings(
                innertube::InnerTubeSession::default().with_auth(auth),
                &settings,
            )?),
            lyrics: Arc::new(LyricsClient::with_settings(&settings)?),
            thumbnails: ThumbnailCache::with_settings(&settings, DEFAULT_THUMBNAIL_CACHE_BYTES)?,
            audio_cache,
            downloaded_audio,
            audio,
            microphone: Arc::new(SystemMicrophoneRecorder),
            recognition: Arc::new(RecognitionClient::with_settings(&settings)?),
        })
    }
}
