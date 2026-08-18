use std::{
    collections::HashSet,
    fmt,
    io::{self, Cursor, Read, Seek, SeekFrom},
    sync::{Arc, Mutex},
    time::Duration,
};

use futures::AsyncReadExt as _;
use http_client::{
    AsyncBody, HttpClient, HttpRequestExt as _, RedirectPolicy, Request, StatusCode,
    http::{HeaderName, HeaderValue},
};
use symphonia::core::{
    codecs::audio::AudioDecoderOptions,
    errors::Error as SymphoniaError,
    formats::{FormatOptions, TrackType, probe::Hint},
    io::{MediaSource, MediaSourceStream},
    meta::MetadataOptions,
};

use crate::domain::Song;
use crate::services::{AudioCache, DownloadedAudioStore};
use crate::{EqualizerSettings, PlaybackParameters, Result};

#[derive(Clone, PartialEq, Eq)]
pub struct PlaybackSource {
    pub url: String,
    pub mime_type: String,
    pub content_length: Option<u64>,
    pub loudness_lufs_mb: Option<i32>,
    pub request_headers: Vec<(HeaderName, HeaderValue)>,
    pub cache_key: Option<String>,
    pub access: PlaybackSourceAccess,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PlaybackSourceAccess {
    #[default]
    NetworkAndCache,
    CacheOnly,
}

impl PlaybackSource {
    pub fn cache_only(
        cache_key: impl Into<String>,
        mime_type: impl Into<String>,
        content_length: u64,
    ) -> Self {
        Self {
            url: String::new(),
            mime_type: mime_type.into(),
            content_length: Some(content_length),
            loudness_lufs_mb: None,
            request_headers: Vec::new(),
            cache_key: Some(cache_key.into()),
            access: PlaybackSourceAccess::CacheOnly,
        }
    }

    pub fn with_loudness_lufs_mb(mut self, loudness_lufs_mb: Option<i32>) -> Self {
        self.loudness_lufs_mb = loudness_lufs_mb.filter(|value| (-10_000..=2_000).contains(value));
        self
    }

    pub(crate) fn disk_cache_key(&self) -> Option<String> {
        self.cache_key.as_ref().map(|key| {
            format!(
                "{key}-{}",
                self.content_length
                    .map_or_else(|| "unknown".into(), |length| length.to_string())
            )
        })
    }
}

impl fmt::Debug for PlaybackSource {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PlaybackSource")
            .field("url", &"[redacted]")
            .field("mime_type", &self.mime_type)
            .field("content_length", &self.content_length)
            .field("has_loudness", &self.loudness_lufs_mb.is_some())
            .field("request_header_count", &self.request_headers.len())
            .field("has_cache_key", &self.cache_key.is_some())
            .field("access", &self.access)
            .finish()
    }
}

pub(crate) const RANGE_CHUNK_SIZE: usize = 512 * 1024;

#[derive(Debug, Default)]
pub(crate) struct PlaybackReadFailure {
    message: Mutex<Option<String>>,
}

impl PlaybackReadFailure {
    fn record(&self, error: &io::Error) {
        if let Ok(mut message) = self.message.lock()
            && message.is_none()
        {
            *message = Some(error.to_string());
        }
    }

    pub(crate) fn message(&self) -> Option<String> {
        self.message.lock().ok().and_then(|message| message.clone())
    }
}

/// A blocking, seekable view over an HTTP resource backed by bounded Range
/// requests. It belongs on the audio worker thread, never GPUI's render thread.
pub struct HttpRangeMediaSource {
    client: Arc<dyn HttpClient>,
    source: PlaybackSource,
    position: u64,
    cache_start: u64,
    cache: Vec<u8>,
    disk_cache: Option<Arc<AudioCache>>,
    download_store: Option<Arc<DownloadedAudioStore>>,
    write_to_download_store: bool,
    failure_reporter: Option<Arc<PlaybackReadFailure>>,
}

impl HttpRangeMediaSource {
    pub fn new(client: Arc<dyn HttpClient>, source: PlaybackSource) -> Self {
        Self {
            client,
            source,
            position: 0,
            cache_start: 0,
            cache: Vec::new(),
            disk_cache: None,
            download_store: None,
            write_to_download_store: false,
            failure_reporter: None,
        }
    }

    pub fn with_disk_cache(mut self, cache: Option<Arc<AudioCache>>) -> Self {
        self.disk_cache = cache;
        self
    }

    pub(crate) fn with_download_store(mut self, store: Option<Arc<DownloadedAudioStore>>) -> Self {
        self.download_store = store;
        self
    }

    pub(crate) fn with_download_target(mut self, store: Arc<DownloadedAudioStore>) -> Self {
        self.download_store = Some(store);
        self.write_to_download_store = true;
        self
    }

    pub(crate) fn with_failure_reporter(mut self, reporter: Arc<PlaybackReadFailure>) -> Self {
        self.failure_reporter = Some(reporter);
        self
    }

    fn report_failure(&self, error: &io::Error) {
        if let Some(reporter) = &self.failure_reporter {
            reporter.record(error);
        }
    }

    fn fetch_range(&self, start: u64, _minimum_size: usize) -> io::Result<(u64, Vec<u8>)> {
        let request_size = RANGE_CHUNK_SIZE as u64;
        let request_start = start / request_size * request_size;
        let mut end = request_start.saturating_add(request_size).saturating_sub(1);
        if let Some(content_length) = self.source.content_length {
            end = end.min(content_length.saturating_sub(1));
        }
        let expected_length = usize::try_from(end.saturating_sub(request_start).saturating_add(1))
            .map_err(|_| io::Error::other("audio range is too large"))?;
        let cache_key = self.source.disk_cache_key();
        if let (Some(store), Some(cache_key)) = (&self.download_store, cache_key.as_deref()) {
            match store.read_chunk(cache_key, request_start, expected_length) {
                Ok(Some(bytes)) => return Ok((request_start, bytes)),
                Ok(None) => {}
                Err(error) => tracing::warn!(%error, "downloaded audio read failed"),
            }
        }
        if let (Some(cache), Some(cache_key)) = (&self.disk_cache, cache_key.as_deref()) {
            match cache.read_chunk(cache_key, request_start, expected_length) {
                Ok(Some(bytes)) => {
                    if self.write_to_download_store
                        && let Some(store) = &self.download_store
                    {
                        store.write_chunk(cache_key, request_start, &bytes)?;
                    }
                    return Ok((request_start, bytes));
                }
                Ok(None) => {}
                Err(error) => tracing::warn!(%error, "audio cache read failed"),
            }
        }

        if self.source.access == PlaybackSourceAccess::CacheOnly {
            return Err(io::Error::new(
                io::ErrorKind::NotConnected,
                "audio range is not cached; a fresh playback source is required",
            ));
        }

        let mut builder = Request::builder()
            .uri(&self.source.url)
            .header("Accept-Encoding", "identity")
            .header("Range", format!("bytes={request_start}-{end}"))
            .follow_redirects(RedirectPolicy::FollowLimit(5))
            .timeout(Duration::from_secs(60));
        for (name, value) in &self.source.request_headers {
            builder = builder.header(name, value);
        }
        let request = builder
            .body(AsyncBody::default())
            .map_err(|_| io::Error::other("invalid audio range request"))?;

        let mut response = futures::executor::block_on(self.client.send(request))
            .map_err(|_| io::Error::other("audio range request failed"))?;
        let status = response.status();
        if status != StatusCode::PARTIAL_CONTENT && status != StatusCode::OK {
            return Err(io::Error::other(format!(
                "audio range request returned HTTP {status}"
            )));
        }

        let mut bytes = Vec::new();
        futures::executor::block_on(response.body_mut().read_to_end(&mut bytes))
            .map_err(|_| io::Error::other("audio range response could not be read"))?;

        if bytes.is_empty() && expected_length > 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "audio range response was empty",
            ));
        }
        if status == StatusCode::PARTIAL_CONTENT
            && self.source.content_length.is_some()
            && bytes.len() != expected_length
        {
            let kind = if bytes.len() < expected_length {
                io::ErrorKind::UnexpectedEof
            } else {
                io::ErrorKind::InvalidData
            };
            return Err(io::Error::new(
                kind,
                format!(
                    "audio range response contained {} bytes, expected {expected_length}",
                    bytes.len()
                ),
            ));
        }
        if status == StatusCode::OK
            && let Some(content_length) = self.source.content_length
        {
            let actual_length = u64::try_from(bytes.len())
                .map_err(|_| io::Error::other("audio response is too large"))?;
            if actual_length != content_length {
                let kind = if actual_length < content_length {
                    io::ErrorKind::UnexpectedEof
                } else {
                    io::ErrorKind::InvalidData
                };
                return Err(io::Error::new(
                    kind,
                    format!(
                        "audio response contained {actual_length} bytes, expected {content_length}"
                    ),
                ));
            }
        }

        // A server that ignores Range returns the complete object with 200, so
        // its response begins at byte zero. A 206 response begins at `start`.
        let response_start = if status == StatusCode::OK {
            0
        } else {
            request_start
        };
        if let Some(cache_key) = cache_key.as_deref() {
            if self.write_to_download_store {
                if let Some(store) = &self.download_store {
                    if status == StatusCode::PARTIAL_CONTENT && bytes.len() == expected_length {
                        store.write_chunk(cache_key, request_start, &bytes)?;
                    } else if status == StatusCode::OK {
                        for (index, chunk) in bytes.chunks(RANGE_CHUNK_SIZE).enumerate() {
                            let start = u64::try_from(index)
                                .ok()
                                .and_then(|index| index.checked_mul(RANGE_CHUNK_SIZE as u64))
                                .ok_or_else(|| io::Error::other("audio download is too large"))?;
                            store.write_chunk(cache_key, start, chunk)?;
                        }
                    }
                }
            } else if status == StatusCode::PARTIAL_CONTENT
                && bytes.len() == expected_length
                && let Some(cache) = &self.disk_cache
                && let Err(error) = cache.write_chunk(cache_key, request_start, &bytes)
            {
                tracing::warn!(%error, "audio cache write failed");
            }
        }
        Ok((response_start, bytes))
    }

    fn cached_offset(&self) -> Option<usize> {
        let offset = self.position.checked_sub(self.cache_start)?;
        let offset = usize::try_from(offset).ok()?;
        (offset < self.cache.len()).then_some(offset)
    }
}

impl Read for HttpRangeMediaSource {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        if buffer.is_empty()
            || self
                .source
                .content_length
                .is_some_and(|length| self.position >= length)
        {
            return Ok(0);
        }

        let mut written = 0;
        while written < buffer.len() {
            let offset = match self.cached_offset() {
                Some(offset) => offset,
                None => {
                    let (cache_start, cache) =
                        match self.fetch_range(self.position, buffer.len() - written) {
                            Ok(range) => range,
                            Err(error) => {
                                self.report_failure(&error);
                                return Err(error);
                            }
                        };
                    self.cache_start = cache_start;
                    self.cache = cache;
                    let Some(offset) = self.cached_offset() else {
                        break;
                    };
                    offset
                }
            };

            let available = self.cache.len() - offset;
            let count = available.min(buffer.len() - written);
            buffer[written..written + count].copy_from_slice(&self.cache[offset..offset + count]);
            written += count;
            self.position += count as u64;

            if self
                .source
                .content_length
                .is_some_and(|length| self.position >= length)
            {
                break;
            }
        }
        Ok(written)
    }
}

impl Seek for HttpRangeMediaSource {
    fn seek(&mut self, position: SeekFrom) -> io::Result<u64> {
        let next = match position {
            SeekFrom::Start(position) => i128::from(position),
            SeekFrom::Current(offset) => i128::from(self.position) + i128::from(offset),
            SeekFrom::End(offset) => {
                let length = self.source.content_length.ok_or_else(|| {
                    io::Error::new(io::ErrorKind::Unsupported, "audio length is unknown")
                })?;
                i128::from(length) + i128::from(offset)
            }
        };
        self.position = u64::try_from(next).map_err(|_| {
            io::Error::new(io::ErrorKind::InvalidInput, "cannot seek before byte zero")
        })?;
        Ok(self.position)
    }
}

impl MediaSource for HttpRangeMediaSource {
    fn is_seekable(&self) -> bool {
        self.source.content_length.is_some()
    }

    fn byte_len(&self) -> Option<u64> {
        self.source.content_length
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DecodedAudioInfo {
    pub sample_rate: u32,
    pub channels: usize,
    pub decoded_frames: u64,
}

/// Probe an in-memory audio file and decode enough packets to validate the
/// demuxer/decoder path selected for playback.
pub fn probe_audio_bytes(bytes: Vec<u8>, extension: Option<&str>) -> Result<DecodedAudioInfo> {
    probe_audio_source(Box::new(Cursor::new(bytes)), extension)
}

/// Probe and partially decode a seekable media source. This is shared by the
/// in-memory fixture path and the HTTP Range integration test.
pub fn probe_audio_source(
    source: Box<dyn MediaSource>,
    extension: Option<&str>,
) -> Result<DecodedAudioInfo> {
    let source = MediaSourceStream::new(source, Default::default());
    let mut hint = Hint::new();
    if let Some(extension) = extension {
        hint.with_extension(extension);
    }

    let mut format = symphonia::default::get_probe()
        .probe(
            &hint,
            source,
            FormatOptions::default(),
            MetadataOptions::default(),
        )
        .map_err(|error| crate::AppError::Playback(format!("audio probe failed: {error}")))?;
    let track = format
        .default_track(TrackType::Audio)
        .ok_or_else(|| crate::AppError::Playback("audio stream has no default track".into()))?;
    let track_id = track.id;
    let codec_params = track
        .codec_params
        .as_ref()
        .and_then(|params| params.audio())
        .ok_or_else(|| crate::AppError::Playback("audio codec parameters are missing".into()))?;
    let mut decoder = symphonia::default::get_codecs()
        .make_audio_decoder(codec_params, &AudioDecoderOptions::default())
        .map_err(|error| {
            crate::AppError::Playback(format!("audio decoder unavailable: {error}"))
        })?;

    let mut decoded_frames = 0_u64;
    let mut sample_rate = 0_u32;
    let mut channels = 0_usize;
    loop {
        let packet = match format.next_packet() {
            Ok(Some(packet)) => packet,
            Ok(None) => break,
            Err(SymphoniaError::ResetRequired) => {
                return Err(crate::AppError::Playback(
                    "audio stream changed tracks while probing".into(),
                ));
            }
            Err(SymphoniaError::IoError(error))
                if error.kind() == std::io::ErrorKind::UnexpectedEof =>
            {
                break;
            }
            Err(error) => {
                return Err(crate::AppError::Playback(format!(
                    "audio packet read failed: {error}"
                )));
            }
        };
        if packet.track_id != track_id {
            continue;
        }

        match decoder.decode(&packet) {
            Ok(decoded) => {
                sample_rate = decoded.spec().rate();
                channels = decoded.spec().channels().count();
                decoded_frames += decoded.frames() as u64;
                if decoded_frames >= 4_096 {
                    break;
                }
            }
            Err(SymphoniaError::DecodeError(_)) => continue,
            Err(error) => {
                return Err(crate::AppError::Playback(format!(
                    "audio decode failed: {error}"
                )));
            }
        }
    }

    if decoded_frames == 0 {
        return Err(crate::AppError::Playback(
            "audio stream produced no decoded frames".into(),
        ));
    }

    Ok(DecodedAudioInfo {
        sample_rate,
        channels,
        decoded_frames,
    })
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum PlaybackState {
    #[default]
    Idle,
    Loading,
    Playing,
    Paused,
    Ended,
    Failed,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PlaybackSnapshot {
    pub state: PlaybackState,
    pub position: Duration,
    pub duration: Option<Duration>,
    pub volume: f32,
    pub normalization_gain_mb: Option<i32>,
    pub equalizer_active: bool,
    pub playback_parameters: PlaybackParameters,
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioOutputDevice {
    pub id: String,
    pub name: String,
    pub is_default: bool,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum AudioDeviceOperation {
    #[default]
    Idle,
    Refreshing,
    Switching,
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct AudioDeviceSnapshot {
    pub devices: Vec<AudioOutputDevice>,
    pub selected_id: Option<String>,
    pub operation: AudioDeviceOperation,
    pub error: Option<String>,
}

impl Default for PlaybackSnapshot {
    fn default() -> Self {
        Self {
            state: PlaybackState::Idle,
            position: Duration::ZERO,
            duration: None,
            volume: 0.8,
            normalization_gain_mb: None,
            equalizer_active: false,
            playback_parameters: PlaybackParameters::default(),
            error: None,
        }
    }
}

pub trait AudioPlayer: Send {
    fn load(&mut self, source: PlaybackSource) -> Result<()>;
    fn load_with_crossfade(&mut self, source: PlaybackSource, _duration: Duration) -> Result<()> {
        self.load(source)
    }
    fn play(&mut self) -> Result<()>;
    fn pause(&mut self) -> Result<()>;
    fn stop(&mut self) -> Result<()>;
    fn seek(&mut self, position: Duration) -> Result<()>;
    fn set_volume(&mut self, volume: f32);
    fn set_volume_multiplier(&mut self, _multiplier: f32) {}
    fn set_playback_parameters(&mut self, _parameters: PlaybackParameters) -> Result<()> {
        Err(crate::AppError::Playback(
            "live playback parameter changes are unavailable for this backend".into(),
        ))
    }
    fn set_equalizer(&mut self, _equalizer: EqualizerSettings) -> Result<()> {
        Err(crate::AppError::Playback(
            "live equalizer changes are unavailable for this backend".into(),
        ))
    }
    fn snapshot(&self) -> PlaybackSnapshot;

    fn output_devices(&self) -> Result<Vec<AudioOutputDevice>> {
        Err(crate::AppError::Playback(
            "audio output selection is unavailable for this backend".into(),
        ))
    }

    fn select_output_device(&mut self, _device_id: &str) -> Result<()> {
        Err(crate::AppError::Playback(
            "audio output selection is unavailable for this backend".into(),
        ))
    }

    fn selected_output_device_id(&self) -> Option<String> {
        None
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueueItem {
    pub song: Song,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RepeatMode {
    #[default]
    Off,
    All,
    One,
}

impl RepeatMode {
    pub fn next(self) -> Self {
        match self {
            Self::Off => Self::All,
            Self::All => Self::One,
            Self::One => Self::Off,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Off => "Repeat off",
            Self::All => "Repeat all",
            Self::One => "Repeat one",
        }
    }

    pub(crate) fn storage_value(self) -> i64 {
        match self {
            Self::Off => 0,
            Self::All => 1,
            Self::One => 2,
        }
    }

    pub(crate) fn from_storage_value(value: i64) -> Option<Self> {
        match value {
            0 => Some(Self::Off),
            1 => Some(Self::All),
            2 => Some(Self::One),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct Queue {
    items: Vec<QueueItem>,
    current: Option<usize>,
}

impl Queue {
    pub fn items(&self) -> &[QueueItem] {
        &self.items
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    pub fn current_index(&self) -> Option<usize> {
        self.current
    }

    pub fn current(&self) -> Option<&QueueItem> {
        self.current.and_then(|index| self.items.get(index))
    }

    pub fn replace_and_start(&mut self, items: Vec<QueueItem>) {
        self.replace(items, Some(0));
    }

    pub fn replace(&mut self, items: Vec<QueueItem>, current: Option<usize>) -> Option<&QueueItem> {
        self.items = items;
        self.current = current.filter(|index| *index < self.items.len());
        self.current()
    }

    pub fn replace_and_select(
        &mut self,
        items: Vec<QueueItem>,
        index: usize,
    ) -> Option<&QueueItem> {
        self.replace(items, Some(index))
    }

    pub fn select(&mut self, index: usize) -> Option<&QueueItem> {
        if index >= self.items.len() {
            return None;
        }
        self.current = Some(index);
        self.current()
    }

    pub fn has_next(&self) -> bool {
        self.current
            .is_some_and(|index| index.saturating_add(1) < self.items.len())
    }

    pub fn has_previous(&self) -> bool {
        self.current.is_some_and(|index| index > 0)
    }

    pub fn remaining_after_current(&self) -> usize {
        self.current
            .map_or(0, |index| self.items.len().saturating_sub(index + 1))
    }

    pub fn truncate_after_current(&mut self) -> usize {
        let keep = self.current.map_or(0, |index| index.saturating_add(1));
        let removed = self.items.len().saturating_sub(keep);
        self.items.truncate(keep);
        removed
    }

    pub fn remove_video_ids_except_current<'a>(
        &mut self,
        video_ids: impl IntoIterator<Item = &'a str>,
    ) -> usize {
        let video_ids = video_ids.into_iter().collect::<HashSet<_>>();
        let current = self.current;
        let indices = self
            .items
            .iter()
            .enumerate()
            .filter_map(|(index, item)| {
                (Some(index) != current && video_ids.contains(item.song.video_id.as_str()))
                    .then_some(index)
            })
            .collect::<Vec<_>>();
        let removed = indices.len();
        for index in indices.into_iter().rev() {
            self.remove(index);
        }
        removed
    }

    pub fn append_unique(&mut self, items: impl IntoIterator<Item = QueueItem>) -> usize {
        let before = self.items.len();
        for item in items {
            if !self
                .items
                .iter()
                .any(|existing| existing.song.video_id == item.song.video_id)
            {
                self.items.push(item);
            }
        }
        if self.current.is_none() && !self.items.is_empty() {
            self.current = Some(0);
        }
        self.items.len() - before
    }

    pub fn push(&mut self, item: QueueItem) {
        self.items.push(item);
        if self.current.is_none() {
            self.current = Some(0);
        }
    }

    pub fn insert_after_current(&mut self, item: QueueItem) -> usize {
        let index = self
            .current
            .and_then(|current| current.checked_add(1))
            .unwrap_or(0)
            .min(self.items.len());
        self.items.insert(index, item);
        if self.current.is_none() {
            self.current = Some(index);
        }
        index
    }

    pub fn move_item(&mut self, from: usize, to: usize) -> bool {
        if from >= self.items.len() || to >= self.items.len() {
            return false;
        }
        if from == to {
            return true;
        }

        let item = self.items.remove(from);
        self.items.insert(to, item);
        self.current = self.current.map(|current| {
            if current == from {
                to
            } else if from < current && current <= to {
                current - 1
            } else if to <= current && current < from {
                current + 1
            } else {
                current
            }
        });
        true
    }

    pub fn remove(&mut self, index: usize) -> Option<QueueItem> {
        if index >= self.items.len() {
            return None;
        }

        let removed = self.items.remove(index);
        self.current = match self.current {
            None => None,
            Some(_) if self.items.is_empty() => None,
            Some(current) if index < current => Some(current - 1),
            Some(current) if index > current => Some(current),
            Some(current) => Some(current.min(self.items.len() - 1)),
        };
        Some(removed)
    }

    pub fn shuffle_around_current(&mut self) {
        let mut rng = fastrand::Rng::new();
        self.shuffle_around_current_with_rng(&mut rng);
    }

    pub fn shuffle_upcoming(&mut self) {
        let Some(start) = self.current.and_then(|index| index.checked_add(1)) else {
            return;
        };
        if start < self.items.len() {
            fastrand::shuffle(&mut self.items[start..]);
        }
    }

    pub fn shuffle_upcoming_partitioned(&mut self, primary_len: usize) {
        let Some(start) = self.current.and_then(|index| index.checked_add(1)) else {
            return;
        };
        let len = self.items.len();
        let start = start.min(len);
        let split = primary_len.clamp(start, len);
        fastrand::shuffle(&mut self.items[start..split]);
        fastrand::shuffle(&mut self.items[split..]);
    }

    pub fn shuffle_all_partitioned(&mut self, primary_len: usize) {
        let split = primary_len.min(self.items.len());
        fastrand::shuffle(&mut self.items[..split]);
        fastrand::shuffle(&mut self.items[split..]);
    }

    fn shuffle_around_current_with_rng(&mut self, rng: &mut fastrand::Rng) {
        let Some(current) = self.current.filter(|index| *index < self.items.len()) else {
            return;
        };
        let current_item = self.items.remove(current);
        rng.shuffle(&mut self.items);
        self.items.insert(current, current_item);
    }

    pub fn next_item(&mut self) -> Option<&QueueItem> {
        let next = self.current?.checked_add(1)?;
        if next >= self.items.len() {
            return None;
        }
        self.current = Some(next);
        self.current()
    }

    pub fn previous_item(&mut self) -> Option<&QueueItem> {
        let previous = self.current?.checked_sub(1)?;
        self.current = Some(previous);
        self.current()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        Mutex,
        atomic::{AtomicUsize, Ordering},
    };

    use futures::future::BoxFuture;
    use http_client::{Response, Url};

    use crate::storage::{DesktopStore, PersistedPlaybackSource, PersistedSession};

    struct MemoryHttpClient {
        bytes: Arc<[u8]>,
        requested_ranges: Mutex<Vec<String>>,
    }

    impl MemoryHttpClient {
        fn new(bytes: Vec<u8>) -> Arc<Self> {
            Arc::new(Self {
                bytes: bytes.into(),
                requested_ranges: Mutex::new(Vec::new()),
            })
        }
    }

    impl HttpClient for MemoryHttpClient {
        fn user_agent(&self) -> Option<&HeaderValue> {
            None
        }

        fn proxy(&self) -> Option<&Url> {
            None
        }

        fn send(
            &self,
            request: http_client::http::Request<AsyncBody>,
        ) -> BoxFuture<'static, http_client::Result<Response<AsyncBody>>> {
            let range = request
                .headers()
                .get("range")
                .expect("range request must include its byte interval")
                .to_str()
                .unwrap()
                .to_owned();
            self.requested_ranges.lock().unwrap().push(range.clone());

            let interval = range.strip_prefix("bytes=").unwrap();
            let (start, end) = interval.split_once('-').unwrap();
            let start = start.parse::<usize>().unwrap();
            let end = end.parse::<usize>().unwrap().min(self.bytes.len() - 1);
            let bytes = self.bytes[start..=end].to_vec();
            Box::pin(async move {
                Ok(Response::builder()
                    .status(StatusCode::PARTIAL_CONTENT)
                    .body(AsyncBody::from(bytes))
                    .unwrap())
            })
        }
    }

    #[derive(Default)]
    struct OfflineHttpClient {
        request_count: AtomicUsize,
    }

    impl HttpClient for OfflineHttpClient {
        fn user_agent(&self) -> Option<&HeaderValue> {
            None
        }

        fn proxy(&self) -> Option<&Url> {
            None
        }

        fn send(
            &self,
            _request: http_client::http::Request<AsyncBody>,
        ) -> BoxFuture<'static, http_client::Result<Response<AsyncBody>>> {
            self.request_count.fetch_add(1, Ordering::Relaxed);
            Box::pin(async { Err(http_client::anyhow!("network is offline")) })
        }
    }

    struct StatusHttpClient {
        status: StatusCode,
        request_count: AtomicUsize,
    }

    impl HttpClient for StatusHttpClient {
        fn user_agent(&self) -> Option<&HeaderValue> {
            None
        }

        fn proxy(&self) -> Option<&Url> {
            None
        }

        fn send(
            &self,
            _request: http_client::http::Request<AsyncBody>,
        ) -> BoxFuture<'static, http_client::Result<Response<AsyncBody>>> {
            self.request_count.fetch_add(1, Ordering::Relaxed);
            let status = self.status;
            Box::pin(async move {
                Ok(Response::builder()
                    .status(status)
                    .body(AsyncBody::default())
                    .unwrap())
            })
        }
    }

    fn item(video_id: &str) -> QueueItem {
        QueueItem {
            song: Song {
                video_id: video_id.into(),
                title: video_id.into(),
                artists: Vec::new(),
                duration: None,
                thumbnail_url: None,
                album: None,
                is_episode: false,
                explicit: false,
                music_video_type: None,
            },
        }
    }

    #[test]
    fn queue_navigation_stays_within_bounds() {
        let mut queue = Queue::default();
        queue.replace_and_start(vec![item("one"), item("two")]);

        assert_eq!(queue.current().unwrap().song.video_id, "one");
        assert_eq!(queue.next_item().unwrap().song.video_id, "two");
        assert!(queue.next_item().is_none());
        assert_eq!(queue.previous_item().unwrap().song.video_id, "one");
        assert!(queue.previous_item().is_none());
    }

    #[test]
    fn pushing_first_item_selects_it() {
        let mut queue = Queue::default();
        queue.push(item("only"));
        assert_eq!(queue.current().unwrap().song.video_id, "only");
    }

    #[test]
    fn queue_can_select_an_arbitrary_item_and_reports_boundaries() {
        let mut queue = Queue::default();
        queue.replace_and_select(vec![item("one"), item("two"), item("three")], 1);

        assert_eq!(queue.current_index(), Some(1));
        assert_eq!(queue.current().unwrap().song.video_id, "two");
        assert!(queue.has_previous());
        assert!(queue.has_next());
        assert_eq!(queue.select(2).unwrap().song.video_id, "three");
        assert!(!queue.has_next());
        assert!(queue.select(3).is_none());
        assert_eq!(queue.current_index(), Some(2));
    }

    #[test]
    fn queue_appends_unique_radio_items_and_can_replace_only_the_future_tail() {
        let mut queue = Queue::default();
        queue.replace_and_select(vec![item("one"), item("two"), item("three")], 1);

        assert_eq!(queue.remaining_after_current(), 1);
        assert_eq!(
            queue.append_unique([item("two"), item("four"), item("four")]),
            1
        );
        assert_eq!(
            queue
                .items()
                .iter()
                .map(|item| item.song.video_id.as_str())
                .collect::<Vec<_>>(),
            ["one", "two", "three", "four"]
        );
        assert_eq!(queue.truncate_after_current(), 2);
        assert_eq!(queue.current().unwrap().song.video_id, "two");
        assert_eq!(queue.remaining_after_current(), 0);
    }

    #[test]
    fn repeat_mode_cycles_in_the_android_order() {
        assert_eq!(RepeatMode::Off.next(), RepeatMode::All);
        assert_eq!(RepeatMode::All.next(), RepeatMode::One);
        assert_eq!(RepeatMode::One.next(), RepeatMode::Off);
        assert_eq!(RepeatMode::default(), RepeatMode::Off);
    }

    #[test]
    fn shuffle_preserves_the_current_item_and_every_queue_entry() {
        let mut queue = Queue::default();
        queue.replace_and_select(
            vec![item("one"), item("two"), item("three"), item("four")],
            1,
        );
        let before = queue
            .items()
            .iter()
            .map(|item| item.song.video_id.clone())
            .collect::<Vec<_>>();
        let mut rng = fastrand::Rng::with_seed(7);

        queue.shuffle_around_current_with_rng(&mut rng);

        assert_eq!(queue.current_index(), Some(1));
        assert_eq!(queue.current().unwrap().song.video_id, "two");
        let mut after = queue
            .items()
            .iter()
            .map(|item| item.song.video_id.clone())
            .collect::<Vec<_>>();
        assert_ne!(after, before);
        after.sort();
        let mut expected = before;
        expected.sort();
        assert_eq!(after, expected);
    }

    #[test]
    fn shuffling_new_radio_items_never_reorders_played_or_current_entries() {
        let mut queue = Queue::default();
        queue.replace_and_select(
            vec![item("one"), item("two"), item("three"), item("four")],
            1,
        );

        queue.shuffle_upcoming();

        assert_eq!(queue.items()[0].song.video_id, "one");
        assert_eq!(queue.items()[1].song.video_id, "two");
        assert_eq!(queue.current().unwrap().song.video_id, "two");
        let mut upcoming = queue.items()[2..]
            .iter()
            .map(|item| item.song.video_id.as_str())
            .collect::<Vec<_>>();
        upcoming.sort_unstable();
        assert_eq!(upcoming, ["four", "three"]);
    }

    #[test]
    fn moving_queue_items_preserves_the_selected_song_and_rejects_bad_indices() {
        let mut queue = Queue::default();
        queue.replace_and_select(
            vec![item("one"), item("two"), item("three"), item("four")],
            1,
        );

        assert!(queue.move_item(0, 3));
        assert_eq!(queue.current_index(), Some(0));
        assert_eq!(queue.current().unwrap().song.video_id, "two");
        assert_eq!(
            queue
                .items()
                .iter()
                .map(|item| item.song.video_id.as_str())
                .collect::<Vec<_>>(),
            ["two", "three", "four", "one"]
        );

        assert!(queue.move_item(0, 2));
        assert_eq!(queue.current_index(), Some(2));
        assert_eq!(queue.current().unwrap().song.video_id, "two");
        assert!(!queue.move_item(4, 0));
        assert!(!queue.move_item(0, 4));
    }

    #[test]
    fn removing_queue_items_keeps_a_valid_adjacent_selection() {
        let mut queue = Queue::default();
        queue.replace_and_select(
            vec![item("one"), item("two"), item("three"), item("four")],
            1,
        );

        assert_eq!(queue.remove(0).unwrap().song.video_id, "one");
        assert_eq!(queue.current_index(), Some(0));
        assert_eq!(queue.current().unwrap().song.video_id, "two");

        assert_eq!(queue.remove(0).unwrap().song.video_id, "two");
        assert_eq!(queue.current_index(), Some(0));
        assert_eq!(queue.current().unwrap().song.video_id, "three");

        assert_eq!(queue.remove(1).unwrap().song.video_id, "four");
        assert_eq!(queue.current().unwrap().song.video_id, "three");
        assert_eq!(queue.remove(0).unwrap().song.video_id, "three");
        assert!(queue.is_empty());
        assert_eq!(queue.current_index(), None);
        assert!(queue.remove(0).is_none());
    }

    #[test]
    fn explicit_play_next_inserts_after_current_and_allows_duplicates() {
        let mut queue = Queue::default();
        queue.replace_and_select(vec![item("one"), item("two"), item("three")], 1);

        assert_eq!(queue.insert_after_current(item("one")), 2);
        assert_eq!(queue.current_index(), Some(1));
        assert_eq!(queue.current().unwrap().song.video_id, "two");
        assert_eq!(
            queue
                .items()
                .iter()
                .map(|item| item.song.video_id.as_str())
                .collect::<Vec<_>>(),
            ["one", "two", "one", "three"]
        );

        let mut empty = Queue::default();
        assert_eq!(empty.insert_after_current(item("first")), 0);
        assert_eq!(empty.current_index(), Some(0));
        assert_eq!(empty.current().unwrap().song.video_id, "first");
    }

    #[test]
    fn playback_source_debug_output_redacts_the_stream_url() {
        let source = PlaybackSource {
            url: "https://example.invalid/private-stream?token=secret".into(),
            mime_type: "audio/mp4".into(),
            content_length: Some(42),
            loudness_lufs_mb: None,
            request_headers: Vec::new(),
            cache_key: None,
            access: PlaybackSourceAccess::NetworkAndCache,
        };
        let debug = format!("{source:?}");

        assert!(!debug.contains("secret"));
        assert!(debug.contains("[redacted]"));
    }

    #[test]
    fn range_source_caches_reads_and_fetches_after_seeking() {
        let bytes = (0..1_200_000)
            .map(|index| (index % 251) as u8)
            .collect::<Vec<_>>();
        let client = MemoryHttpClient::new(bytes.clone());
        let source = PlaybackSource {
            url: "https://example.invalid/audio".into(),
            mime_type: "audio/mp4".into(),
            content_length: Some(bytes.len() as u64),
            loudness_lufs_mb: None,
            request_headers: Vec::new(),
            cache_key: None,
            access: PlaybackSourceAccess::NetworkAndCache,
        };
        let mut reader = HttpRangeMediaSource::new(client.clone(), source);

        let mut first = [0; 16];
        reader.read_exact(&mut first).unwrap();
        assert_eq!(first, bytes[..16]);

        let mut cached = [0; 16];
        reader.read_exact(&mut cached).unwrap();
        assert_eq!(cached, bytes[16..32]);
        assert_eq!(client.requested_ranges.lock().unwrap().len(), 1);

        reader.seek(SeekFrom::Start(700_000)).unwrap();
        let mut middle = [0; 16];
        reader.read_exact(&mut middle).unwrap();
        assert_eq!(middle, bytes[700_000..700_016]);

        reader.seek(SeekFrom::End(-4)).unwrap();
        let mut tail = [0; 4];
        reader.read_exact(&mut tail).unwrap();
        assert_eq!(tail, bytes[bytes.len() - 4..]);

        assert_eq!(
            client.requested_ranges.lock().unwrap().as_slice(),
            [
                "bytes=0-524287",
                "bytes=524288-1048575",
                "bytes=1048576-1199999"
            ]
        );
    }

    #[test]
    fn range_source_rejects_seek_before_start() {
        let client = MemoryHttpClient::new(vec![0; 16]);
        let source = PlaybackSource {
            url: "https://example.invalid/audio".into(),
            mime_type: "audio/mp4".into(),
            content_length: Some(16),
            loudness_lufs_mb: None,
            request_headers: Vec::new(),
            cache_key: None,
            access: PlaybackSourceAccess::NetworkAndCache,
        };
        let mut reader = HttpRangeMediaSource::new(client, source);

        let error = reader.seek(SeekFrom::Current(-1)).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
    }

    #[test]
    fn range_source_reuses_a_complete_disk_chunk() {
        let bytes = (0..600_000)
            .map(|index| (index % 251) as u8)
            .collect::<Vec<_>>();
        let client = MemoryHttpClient::new(bytes.clone());
        let cache_root =
            std::env::temp_dir().join(format!("metrolist-range-cache-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&cache_root);
        let cache = Arc::new(AudioCache::new(cache_root.clone(), 2_000_000).unwrap());
        let source = PlaybackSource {
            url: "https://example.invalid/audio".into(),
            mime_type: "audio/mp4".into(),
            content_length: Some(bytes.len() as u64),
            loudness_lufs_mb: None,
            request_headers: Vec::new(),
            cache_key: Some("video-one".into()),
            access: PlaybackSourceAccess::NetworkAndCache,
        };

        let mut first_reader = HttpRangeMediaSource::new(client.clone(), source.clone())
            .with_disk_cache(Some(cache.clone()));
        let mut first = [0; 16];
        first_reader.read_exact(&mut first).unwrap();
        assert_eq!(client.requested_ranges.lock().unwrap().len(), 1);

        let mut second_reader =
            HttpRangeMediaSource::new(client.clone(), source).with_disk_cache(Some(cache));
        let mut second = [0; 16];
        second_reader.read_exact(&mut second).unwrap();

        assert_eq!(second, first);
        assert_eq!(client.requested_ranges.lock().unwrap().len(), 1);
        let _ = std::fs::remove_dir_all(cache_root);
    }

    #[test]
    fn fully_cached_resource_remains_readable_while_network_is_offline() {
        let bytes = (0..1_200_000)
            .map(|index| (index % 251) as u8)
            .collect::<Vec<_>>();
        let online_client = MemoryHttpClient::new(bytes.clone());
        let offline_client = Arc::new(OfflineHttpClient::default());
        let cache_root = std::env::temp_dir().join(format!(
            "metrolist-offline-range-cache-test-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&cache_root);
        let cache = Arc::new(AudioCache::new(cache_root.clone(), 2_000_000).unwrap());
        let source = PlaybackSource {
            url: "https://example.invalid/audio".into(),
            mime_type: "audio/mp4".into(),
            content_length: Some(bytes.len() as u64),
            loudness_lufs_mb: None,
            request_headers: Vec::new(),
            cache_key: Some("offline-video".into()),
            access: PlaybackSourceAccess::NetworkAndCache,
        };

        let mut online_reader = HttpRangeMediaSource::new(online_client, source.clone())
            .with_disk_cache(Some(cache.clone()));
        let mut downloaded = Vec::new();
        online_reader.read_to_end(&mut downloaded).unwrap();
        assert_eq!(downloaded, bytes);

        let cache_only_source =
            PlaybackSource::cache_only("offline-video", source.mime_type, bytes.len() as u64);
        let mut offline_reader =
            HttpRangeMediaSource::new(offline_client.clone(), cache_only_source)
                .with_disk_cache(Some(cache));
        let mut restored = Vec::new();
        offline_reader.read_to_end(&mut restored).unwrap();

        assert_eq!(restored, bytes);
        assert_eq!(offline_client.request_count.load(Ordering::Relaxed), 0);
        let _ = std::fs::remove_dir_all(cache_root);
    }

    #[test]
    fn cold_start_restores_expired_metadata_and_complete_audio_without_network() {
        let bytes = (0..1_200_000)
            .map(|index| (index % 251) as u8)
            .collect::<Vec<_>>();
        let video_id = "cold-start-video";
        let database_path = std::env::temp_dir().join(format!(
            "metrolist-cold-start-test-{}-{:?}.sqlite3",
            std::process::id(),
            std::thread::current().id()
        ));
        let cache_root = std::env::temp_dir().join(format!(
            "metrolist-cold-start-cache-test-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_file(&database_path);
        let _ = std::fs::remove_dir_all(&cache_root);

        let song = item(video_id).song;
        let store = DesktopStore::open(&database_path).unwrap();
        futures::executor::block_on(store.save_session(PersistedSession {
            queue: vec![song],
            current_index: Some(0),
            position: Duration::from_secs(37),
            volume: 0.65,
            repeat_mode: RepeatMode::Off,
            shuffle_enabled: false,
            playback_source: Some(PersistedPlaybackSource {
                video_id: video_id.into(),
                mime_type: "audio/mp4; codecs=mp4a.40.2".into(),
                content_length: bytes.len() as u64,
                loudness_lufs_mb: None,
                resolved_at_ms: 100,
                expires_at_ms: 200,
            }),
        }))
        .unwrap();
        drop(store);

        let online_client = MemoryHttpClient::new(bytes.clone());
        let cache = Arc::new(AudioCache::new(cache_root.clone(), 2_000_000).unwrap());
        let online_source = PlaybackSource {
            url: "https://example.invalid/audio".into(),
            mime_type: "audio/mp4; codecs=mp4a.40.2".into(),
            content_length: Some(bytes.len() as u64),
            loudness_lufs_mb: None,
            request_headers: Vec::new(),
            cache_key: Some(video_id.into()),
            access: PlaybackSourceAccess::NetworkAndCache,
        };
        let mut online_reader =
            HttpRangeMediaSource::new(online_client, online_source).with_disk_cache(Some(cache));
        online_reader.read_to_end(&mut Vec::new()).unwrap();
        drop(online_reader);

        let store = DesktopStore::open(&database_path).unwrap();
        let restored_session = futures::executor::block_on(store.load_session())
            .unwrap()
            .unwrap();
        let metadata = restored_session.playback_source.unwrap();
        assert_eq!(restored_session.position, Duration::from_secs(37));
        assert_eq!(metadata.expires_at_ms, 200);
        drop(store);

        let offline_client = Arc::new(OfflineHttpClient::default());
        let reopened_cache = Arc::new(AudioCache::new(cache_root.clone(), 2_000_000).unwrap());
        let cache_only_source = PlaybackSource::cache_only(
            metadata.video_id,
            metadata.mime_type,
            metadata.content_length,
        );
        let mut restored_reader =
            HttpRangeMediaSource::new(offline_client.clone(), cache_only_source)
                .with_disk_cache(Some(reopened_cache));
        let mut restored = Vec::new();
        restored_reader.read_to_end(&mut restored).unwrap();

        assert_eq!(restored, bytes);
        assert_eq!(offline_client.request_count.load(Ordering::Relaxed), 0);
        drop(restored_reader);
        let _ = std::fs::remove_file(database_path);
        let _ = std::fs::remove_dir_all(cache_root);
    }

    #[test]
    fn cache_only_source_reports_a_missing_chunk_without_touching_network() {
        let offline_client = Arc::new(OfflineHttpClient::default());
        let cache_root = std::env::temp_dir().join(format!(
            "metrolist-cache-only-miss-test-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&cache_root);
        let cache = Arc::new(AudioCache::new(cache_root.clone(), 2_000_000).unwrap());
        let failure = Arc::new(PlaybackReadFailure::default());
        let source = PlaybackSource::cache_only("uncached-video", "audio/mp4", 600_000);
        let mut reader = HttpRangeMediaSource::new(offline_client.clone(), source)
            .with_disk_cache(Some(cache))
            .with_failure_reporter(failure.clone());

        let error = reader.read(&mut [0]).unwrap_err();

        assert_eq!(error.kind(), io::ErrorKind::NotConnected);
        assert_eq!(offline_client.request_count.load(Ordering::Relaxed), 0);
        assert!(
            failure
                .message()
                .is_some_and(|message| message.contains("fresh playback source"))
        );
        let _ = std::fs::remove_dir_all(cache_root);
    }

    #[test]
    fn expired_http_stream_failure_is_reported_to_the_audio_state_machine() {
        let client = Arc::new(StatusHttpClient {
            status: StatusCode::FORBIDDEN,
            request_count: AtomicUsize::new(0),
        });
        let failure = Arc::new(PlaybackReadFailure::default());
        let source = PlaybackSource {
            url: "https://example.invalid/expired".into(),
            mime_type: "audio/mp4".into(),
            content_length: Some(600_000),
            loudness_lufs_mb: None,
            request_headers: Vec::new(),
            cache_key: Some("expired-video".into()),
            access: PlaybackSourceAccess::NetworkAndCache,
        };
        let mut reader = HttpRangeMediaSource::new(client.clone(), source)
            .with_failure_reporter(failure.clone());

        let error = reader.read(&mut [0]).unwrap_err();

        assert!(error.to_string().contains("HTTP 403"));
        assert_eq!(client.request_count.load(Ordering::Relaxed), 1);
        assert!(
            failure
                .message()
                .is_some_and(|message| message.contains("HTTP 403"))
        );
    }

    #[test]
    fn uncached_range_reports_the_offline_request_failure() {
        let bytes = (0..600_000)
            .map(|index| (index % 251) as u8)
            .collect::<Vec<_>>();
        let online_client = MemoryHttpClient::new(bytes.clone());
        let offline_client = Arc::new(OfflineHttpClient::default());
        let cache_root = std::env::temp_dir().join(format!(
            "metrolist-partial-offline-range-cache-test-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&cache_root);
        let cache = Arc::new(AudioCache::new(cache_root.clone(), 2_000_000).unwrap());
        let source = PlaybackSource {
            url: "https://example.invalid/audio".into(),
            mime_type: "audio/mp4".into(),
            content_length: Some(bytes.len() as u64),
            loudness_lufs_mb: None,
            request_headers: Vec::new(),
            cache_key: Some("partial-offline-video".into()),
            access: PlaybackSourceAccess::NetworkAndCache,
        };

        let mut online_reader = HttpRangeMediaSource::new(online_client, source.clone())
            .with_disk_cache(Some(cache.clone()));
        let mut cached_prefix = [0; 16];
        online_reader.read_exact(&mut cached_prefix).unwrap();

        let mut offline_reader =
            HttpRangeMediaSource::new(offline_client.clone(), source).with_disk_cache(Some(cache));
        let mut restored_prefix = [0; 16];
        offline_reader.read_exact(&mut restored_prefix).unwrap();
        assert_eq!(restored_prefix, cached_prefix);
        assert_eq!(offline_client.request_count.load(Ordering::Relaxed), 0);

        offline_reader
            .seek(SeekFrom::Start(RANGE_CHUNK_SIZE as u64))
            .unwrap();
        let error = offline_reader.read(&mut [0]).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::Other);
        assert_eq!(offline_client.request_count.load(Ordering::Relaxed), 1);
        let _ = std::fs::remove_dir_all(cache_root);
    }

    #[test]
    fn known_length_range_rejects_a_truncated_response() {
        let client = MemoryHttpClient::new(vec![7; 512]);
        let source = PlaybackSource {
            url: "https://example.invalid/audio".into(),
            mime_type: "audio/mp4".into(),
            content_length: Some(1_024),
            loudness_lufs_mb: None,
            request_headers: Vec::new(),
            cache_key: None,
            access: PlaybackSourceAccess::NetworkAndCache,
        };
        let mut reader = HttpRangeMediaSource::new(client, source);

        let error = reader.read_to_end(&mut Vec::new()).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::UnexpectedEof);
    }
}
