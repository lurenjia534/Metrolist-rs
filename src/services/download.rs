use std::{
    io::Read as _,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
};

use futures::channel::oneshot;

use crate::services::innertube::InnerTubeClient;
use crate::services::playback::RANGE_CHUNK_SIZE;
use crate::services::{AudioCache, DownloadedAudioStore, PlaybackSource, probe_audio_source};
use crate::{AppError, AudioQuality, Result};

const DOWNLOAD_RESOLUTION_ATTEMPTS: usize = 2;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DownloadUpdate {
    Prepared {
        mime_type: String,
        content_length: u64,
        downloaded_bytes: u64,
        loudness_lufs_mb: Option<i32>,
    },
    Progress {
        downloaded_bytes: u64,
        content_length: u64,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DownloadReceipt {
    pub audio_quality: AudioQuality,
    pub mime_type: String,
    pub content_length: u64,
    pub loudness_lufs_mb: Option<i32>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DownloadOutcome {
    Completed(DownloadReceipt),
    Cancelled,
}

enum TransferOutcome {
    Completed,
    Cancelled,
}

pub async fn download_song(
    client: Arc<InnerTubeClient>,
    player_cache: Arc<AudioCache>,
    downloads: Arc<DownloadedAudioStore>,
    video_id: &str,
    audio_quality: AudioQuality,
    cancelled: Arc<AtomicBool>,
    mut on_update: impl FnMut(DownloadUpdate) -> Result<()> + Send + 'static,
) -> Result<DownloadOutcome> {
    let video_id = video_id.trim();
    if video_id.is_empty() {
        return Err(AppError::Download("video id cannot be empty".into()));
    }
    let expected_cache_key = audio_quality.playback_cache_key(video_id);
    let mut previous_resource_key: Option<String> = None;
    let mut last_error = None;

    for attempt in 0..DOWNLOAD_RESOLUTION_ATTEMPTS {
        if cancelled.load(Ordering::Acquire) {
            return Ok(DownloadOutcome::Cancelled);
        }
        let resolved = match client.resolve_playback_source(video_id).await {
            Ok(resolved) => resolved,
            Err(error) if attempt + 1 < DOWNLOAD_RESOLUTION_ATTEMPTS => {
                last_error = Some(error);
                continue;
            }
            Err(error) => return Err(error),
        };
        let source = resolved.source;
        let content_length = source
            .content_length
            .filter(|length| *length > 0)
            .ok_or_else(|| {
                AppError::Download("the selected audio stream has no content length".into())
            })?;
        if !source.mime_type.starts_with("audio/") {
            return Err(AppError::Download(
                "the selected stream is not an audio resource".into(),
            ));
        }
        if source.cache_key.as_deref() != Some(expected_cache_key.as_str()) {
            return Err(AppError::Download(
                "the selected audio stream has no stable cache identity".into(),
            ));
        }
        let resource_key = source.disk_cache_key().ok_or_else(|| {
            AppError::Download("the selected audio stream has no resource identity".into())
        })?;
        let receipt_mime_type = source.mime_type.clone();
        let receipt_loudness_lufs_mb = source.loudness_lufs_mb;
        let stale_resource_key = previous_resource_key
            .replace(resource_key.clone())
            .filter(|previous| previous != &resource_key);
        let (transfer, returned_update) = run_transfer_worker(
            client.clone(),
            source,
            player_cache.clone(),
            downloads.clone(),
            cancelled.clone(),
            stale_resource_key,
            on_update,
        )
        .await?;
        on_update = returned_update;
        match transfer {
            Ok(TransferOutcome::Cancelled) => return Ok(DownloadOutcome::Cancelled),
            Ok(TransferOutcome::Completed) => {
                return Ok(DownloadOutcome::Completed(DownloadReceipt {
                    audio_quality,
                    mime_type: receipt_mime_type,
                    content_length,
                    loudness_lufs_mb: receipt_loudness_lufs_mb,
                }));
            }
            Err(error) if attempt + 1 < DOWNLOAD_RESOLUTION_ATTEMPTS => {
                last_error = Some(error);
            }
            Err(error) => return Err(error),
        }
    }

    Err(last_error.unwrap_or_else(|| AppError::Download("download retry limit reached".into())))
}

async fn run_transfer_worker<F>(
    client: Arc<InnerTubeClient>,
    source: PlaybackSource,
    player_cache: Arc<AudioCache>,
    downloads: Arc<DownloadedAudioStore>,
    cancelled: Arc<AtomicBool>,
    stale_resource_key: Option<String>,
    mut on_update: F,
) -> Result<(Result<TransferOutcome>, F)>
where
    F: FnMut(DownloadUpdate) -> Result<()> + Send + 'static,
{
    let (sender, receiver) = oneshot::channel();
    thread::Builder::new()
        .name("metrolist-download-transfer".into())
        .spawn(move || {
            let transfer = prepare_transfer_and_validate(
                client.as_ref(),
                source,
                player_cache,
                downloads,
                cancelled.as_ref(),
                stale_resource_key.as_deref(),
                &mut on_update,
            );
            let _ = sender.send((transfer, on_update));
        })
        .map_err(|error| AppError::Download(format!("download worker could not start: {error}")))?;
    receiver
        .await
        .map_err(|_| AppError::Download("download worker stopped unexpectedly".into()))
}

fn prepare_transfer_and_validate(
    client: &InnerTubeClient,
    source: PlaybackSource,
    player_cache: Arc<AudioCache>,
    downloads: Arc<DownloadedAudioStore>,
    cancelled: &AtomicBool,
    stale_resource_key: Option<&str>,
    on_update: &mut impl FnMut(DownloadUpdate) -> Result<()>,
) -> Result<TransferOutcome> {
    if let Some(previous) = stale_resource_key {
        downloads.remove_resource(previous).map_err(|error| {
            AppError::Download(format!(
                "stale partial download could not be removed: {error}"
            ))
        })?;
    }
    let content_length = source
        .content_length
        .filter(|length| *length > 0)
        .ok_or_else(|| AppError::Download("audio download requires a content length".into()))?;
    let resource_key = source
        .disk_cache_key()
        .ok_or_else(|| AppError::Download("audio download requires a cache identity".into()))?;
    let downloaded_bytes = downloads
        .cached_resource_bytes(&resource_key, content_length, RANGE_CHUNK_SIZE)
        .map_err(|error| {
            AppError::Download(format!("download progress could not be read: {error}"))
        })?;
    on_update(DownloadUpdate::Prepared {
        mime_type: source.mime_type.clone(),
        content_length,
        downloaded_bytes,
        loudness_lufs_mb: source.loudness_lufs_mb,
    })?;
    let outcome = transfer_source(
        client,
        source.clone(),
        player_cache,
        downloads.clone(),
        cancelled,
        on_update,
    )?;
    if matches!(outcome, TransferOutcome::Completed)
        && let Err(validation_error) = validate_download(client, &source, downloads.clone())
    {
        return match downloads.remove_resource(&resource_key) {
            Ok(()) => Err(validation_error),
            Err(removal_error) => Err(AppError::Download(format!(
                "downloaded audio failed media validation and could not be discarded: {removal_error}"
            ))),
        };
    }
    Ok(outcome)
}

fn transfer_source(
    client: &InnerTubeClient,
    source: PlaybackSource,
    player_cache: Arc<AudioCache>,
    downloads: Arc<DownloadedAudioStore>,
    cancelled: &AtomicBool,
    on_update: &mut impl FnMut(DownloadUpdate) -> Result<()>,
) -> Result<TransferOutcome> {
    let content_length = source
        .content_length
        .filter(|length| *length > 0)
        .ok_or_else(|| AppError::Download("audio download requires a content length".into()))?;
    let resource_key = source
        .disk_cache_key()
        .ok_or_else(|| AppError::Download("audio download requires a cache identity".into()))?;
    let mut reader = client
        .open_playback_source(source)
        .with_disk_cache(Some(player_cache))
        .with_download_target(downloads.clone());
    let mut buffer = vec![0_u8; RANGE_CHUNK_SIZE];

    loop {
        if cancelled.load(Ordering::Acquire) {
            return Ok(TransferOutcome::Cancelled);
        }
        let count = reader
            .read(&mut buffer)
            .map_err(|error| AppError::Download(format!("audio transfer failed: {error}")))?;
        if count == 0 {
            break;
        }
        let downloaded_bytes = downloads
            .cached_resource_bytes(&resource_key, content_length, RANGE_CHUNK_SIZE)
            .map_err(|error| {
                AppError::Download(format!("download progress could not be read: {error}"))
            })?;
        on_update(DownloadUpdate::Progress {
            downloaded_bytes,
            content_length,
        })?;
    }

    if !downloads
        .contains_complete_resource(&resource_key, content_length, RANGE_CHUNK_SIZE)
        .map_err(|error| {
            AppError::Download(format!("download completion could not be checked: {error}"))
        })?
    {
        return Err(AppError::Download(
            "audio transfer ended before every chunk was committed".into(),
        ));
    }
    Ok(TransferOutcome::Completed)
}

fn validate_download(
    client: &InnerTubeClient,
    source: &PlaybackSource,
    downloads: Arc<DownloadedAudioStore>,
) -> Result<()> {
    let content_length = source
        .content_length
        .ok_or_else(|| AppError::Download("downloaded audio has no content length".into()))?;
    let cache_key = source
        .cache_key
        .clone()
        .ok_or_else(|| AppError::Download("downloaded audio has no cache identity".into()))?;
    let offline = PlaybackSource::cache_only(cache_key, source.mime_type.clone(), content_length);
    let reader = client
        .open_playback_source(offline)
        .with_download_store(Some(downloads));
    probe_audio_source(Box::new(reader), Some("m4a"))
        .map(|_| ())
        .map_err(|_| AppError::Download("downloaded audio failed media validation".into()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        fs,
        path::PathBuf,
        sync::{Mutex, atomic::AtomicU64},
    };

    use base64::{Engine as _, engine::general_purpose::STANDARD};
    use futures::future::BoxFuture;
    use http_client::{
        AsyncBody, HttpClient, Response, StatusCode, Url,
        http::{HeaderValue, Request},
    };

    use crate::services::PlaybackSourceAccess;
    use crate::services::innertube::InnerTubeSession;

    static TEST_DIRECTORY_SEQUENCE: AtomicU64 = AtomicU64::new(0);

    struct TestDirectory(PathBuf);

    impl TestDirectory {
        fn new(name: &str) -> Self {
            let sequence = TEST_DIRECTORY_SEQUENCE.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "metrolist-download-test-{name}-{}-{sequence}",
                std::process::id()
            ));
            let _ = fs::remove_dir_all(&path);
            fs::create_dir_all(&path).unwrap();
            Self(path)
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    struct MemoryHttpClient {
        bytes: Arc<[u8]>,
        requested_ranges: Mutex<Vec<String>>,
    }

    struct DownloadHttpClient {
        bytes: Arc<[u8]>,
        player_requests: AtomicU64,
        requested_ranges: Mutex<Vec<String>>,
    }

    impl DownloadHttpClient {
        fn new(bytes: Vec<u8>) -> Arc<Self> {
            Arc::new(Self {
                bytes: bytes.into(),
                player_requests: AtomicU64::new(0),
                requested_ranges: Mutex::new(Vec::new()),
            })
        }
    }

    impl HttpClient for DownloadHttpClient {
        fn user_agent(&self) -> Option<&HeaderValue> {
            None
        }

        fn proxy(&self) -> Option<&Url> {
            None
        }

        fn send(
            &self,
            request: Request<AsyncBody>,
        ) -> BoxFuture<'static, http_client::Result<Response<AsyncBody>>> {
            if request.uri().path().ends_with("/player") {
                self.player_requests.fetch_add(1, Ordering::Relaxed);
                let body = serde_json::to_vec(&serde_json::json!({
                    "playabilityStatus": { "status": "OK" },
                    "playerConfig": {
                        "audioConfig": { "perceptualLoudnessDb": -13.5 }
                    },
                    "streamingData": {
                        "expiresInSeconds": "3600",
                        "adaptiveFormats": [{
                            "itag": 140,
                            "mimeType": "audio/mp4; codecs=\"mp4a.40.2\"",
                            "bitrate": 128000,
                            "audioQuality": "AUDIO_QUALITY_HIGH",
                            "contentLength": self.bytes.len().to_string(),
                            "url": "https://example.invalid/download-fixture?signature=secret"
                        }]
                    }
                }))
                .unwrap();
                return Box::pin(async move {
                    Ok(Response::builder()
                        .status(StatusCode::OK)
                        .body(AsyncBody::from(body))
                        .unwrap())
                });
            }

            let range = request
                .headers()
                .get("range")
                .expect("audio request must contain a Range header")
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

    impl MemoryHttpClient {
        fn new(bytes: Vec<u8>) -> Arc<Self> {
            Arc::new(Self {
                bytes: bytes.into(),
                requested_ranges: Mutex::new(Vec::new()),
            })
        }

        fn requested_ranges(&self) -> Vec<String> {
            self.requested_ranges.lock().unwrap().clone()
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
            request: Request<AsyncBody>,
        ) -> BoxFuture<'static, http_client::Result<Response<AsyncBody>>> {
            let range = request
                .headers()
                .get("range")
                .expect("audio request must contain a Range header")
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

    fn test_stores(
        directory: &TestDirectory,
        capacity: u64,
    ) -> (Arc<AudioCache>, Arc<DownloadedAudioStore>) {
        (
            Arc::new(AudioCache::new(directory.0.join("player-cache"), capacity).unwrap()),
            Arc::new(DownloadedAudioStore::new(directory.0.join("downloads")).unwrap()),
        )
    }

    fn source(cache_key: &str, byte_count: usize) -> PlaybackSource {
        PlaybackSource {
            url: "https://example.invalid/private-audio?signature=secret".into(),
            mime_type: "audio/mp4; codecs=mp4a.40.2".into(),
            content_length: Some(byte_count as u64),
            loudness_lufs_mb: None,
            request_headers: Vec::new(),
            cache_key: Some(cache_key.into()),
            access: PlaybackSourceAccess::NetworkAndCache,
        }
    }

    fn read_downloaded(downloads: &DownloadedAudioStore, source: &PlaybackSource) -> Vec<u8> {
        let resource_key = source.disk_cache_key().unwrap();
        let content_length = source.content_length.unwrap();
        let mut bytes = Vec::new();
        let mut start = 0_u64;
        while start < content_length {
            let length = (RANGE_CHUNK_SIZE as u64).min(content_length - start) as usize;
            bytes.extend(
                downloads
                    .read_chunk(&resource_key, start, length)
                    .unwrap()
                    .unwrap(),
            );
            start += length as u64;
        }
        bytes
    }

    #[test]
    fn transfer_reuses_player_cache_and_commits_every_chunk_to_persistent_storage() {
        let bytes = (0..RANGE_CHUNK_SIZE * 2 + 137)
            .map(|index| (index % 251) as u8)
            .collect::<Vec<_>>();
        let directory = TestDirectory::new("reuse-player-cache");
        let (player_cache, downloads) = test_stores(&directory, bytes.len() as u64 * 2);
        let http = MemoryHttpClient::new(bytes.clone());
        let client = InnerTubeClient::new(
            InnerTubeSession::default(),
            http.clone(),
            AudioQuality::Auto,
        );
        let source = source("download-reuse", bytes.len());
        let resource_key = source.disk_cache_key().unwrap();
        player_cache
            .write_chunk(&resource_key, 0, &bytes[..RANGE_CHUNK_SIZE])
            .unwrap();
        let cancelled = AtomicBool::new(false);
        let mut updates = Vec::new();

        let outcome = transfer_source(
            &client,
            source.clone(),
            player_cache,
            downloads.clone(),
            &cancelled,
            &mut |update| {
                updates.push(update);
                Ok(())
            },
        )
        .unwrap();

        assert!(matches!(outcome, TransferOutcome::Completed));
        assert_eq!(
            http.requested_ranges(),
            [
                format!("bytes={}-{}", RANGE_CHUNK_SIZE, RANGE_CHUNK_SIZE * 2 - 1),
                format!("bytes={}-{}", RANGE_CHUNK_SIZE * 2, bytes.len() - 1),
            ]
        );
        assert_eq!(read_downloaded(&downloads, &source), bytes);
        assert_eq!(
            updates.last(),
            Some(&DownloadUpdate::Progress {
                downloaded_bytes: source.content_length.unwrap(),
                content_length: source.content_length.unwrap(),
            })
        );
    }

    #[test]
    fn cancelled_transfer_resumes_without_requesting_committed_chunks_again() {
        let bytes = (0..RANGE_CHUNK_SIZE * 2 + 73)
            .map(|index| (index % 239) as u8)
            .collect::<Vec<_>>();
        let directory = TestDirectory::new("cancel-resume");
        let (player_cache, downloads) = test_stores(&directory, bytes.len() as u64 * 2);
        let http = MemoryHttpClient::new(bytes.clone());
        let client = InnerTubeClient::new(
            InnerTubeSession::default(),
            http.clone(),
            AudioQuality::Auto,
        );
        let source = source("download-resume", bytes.len());
        let resource_key = source.disk_cache_key().unwrap();
        let cancelled = AtomicBool::new(false);

        let first = transfer_source(
            &client,
            source.clone(),
            player_cache.clone(),
            downloads.clone(),
            &cancelled,
            &mut |update| {
                if matches!(update, DownloadUpdate::Progress { .. }) {
                    cancelled.store(true, Ordering::Release);
                }
                Ok(())
            },
        )
        .unwrap();
        assert!(matches!(first, TransferOutcome::Cancelled));
        assert_eq!(
            downloads
                .cached_resource_bytes(&resource_key, bytes.len() as u64, RANGE_CHUNK_SIZE)
                .unwrap(),
            RANGE_CHUNK_SIZE as u64
        );

        cancelled.store(false, Ordering::Release);
        let second = transfer_source(
            &client,
            source.clone(),
            player_cache,
            downloads.clone(),
            &cancelled,
            &mut |_| Ok(()),
        )
        .unwrap();

        assert!(matches!(second, TransferOutcome::Completed));
        assert_eq!(
            http.requested_ranges(),
            [
                format!("bytes=0-{}", RANGE_CHUNK_SIZE - 1),
                format!("bytes={}-{}", RANGE_CHUNK_SIZE, RANGE_CHUNK_SIZE * 2 - 1),
                format!("bytes={}-{}", RANGE_CHUNK_SIZE * 2, bytes.len() - 1),
            ]
        );
        assert_eq!(read_downloaded(&downloads, &source), bytes);
    }

    #[test]
    fn media_validation_reads_a_complete_download_without_network() {
        let encoded = include_str!("../../tests/fixtures/audio/tone_aac_lc.m4a.b64")
            .chars()
            .filter(|character| !character.is_whitespace())
            .collect::<String>();
        let bytes = STANDARD.decode(encoded).unwrap();
        let directory = TestDirectory::new("media-validation");
        let (_, downloads) = test_stores(&directory, bytes.len() as u64 * 2);
        let http = MemoryHttpClient::new(bytes.clone());
        let client = InnerTubeClient::new(
            InnerTubeSession::default(),
            http.clone(),
            AudioQuality::Auto,
        );
        let source = source("valid-download", bytes.len());
        downloads
            .write_chunk(&source.disk_cache_key().unwrap(), 0, &bytes)
            .unwrap();

        validate_download(&client, &source, downloads).unwrap();

        assert!(http.requested_ranges().is_empty());
    }

    #[test]
    fn public_download_flow_resolves_transfers_validates_and_reports_completion() {
        let encoded = include_str!("../../tests/fixtures/audio/tone_aac_lc.m4a.b64")
            .chars()
            .filter(|character| !character.is_whitespace())
            .collect::<String>();
        let bytes = STANDARD.decode(encoded).unwrap();
        let directory = TestDirectory::new("public-flow");
        let (player_cache, downloads) = test_stores(&directory, bytes.len() as u64 * 2);
        let http = DownloadHttpClient::new(bytes.clone());
        let mut session = InnerTubeSession::default();
        session.visitor_data = Some("CgtDownloadFixtureVisitor".into());
        let client = Arc::new(InnerTubeClient::new(
            session,
            http.clone(),
            AudioQuality::Auto,
        ));
        let cancelled = Arc::new(AtomicBool::new(false));
        let updates = Arc::new(Mutex::new(Vec::new()));
        let recorded_updates = updates.clone();

        let outcome = futures::executor::block_on(download_song(
            client,
            player_cache,
            downloads.clone(),
            "fixture-video",
            AudioQuality::Auto,
            cancelled,
            move |update| {
                recorded_updates.lock().unwrap().push(update);
                Ok(())
            },
        ))
        .unwrap();
        let updates = updates.lock().unwrap();

        assert_eq!(
            outcome,
            DownloadOutcome::Completed(DownloadReceipt {
                audio_quality: AudioQuality::Auto,
                mime_type: "audio/mp4; codecs=\"mp4a.40.2\"".into(),
                content_length: bytes.len() as u64,
                loudness_lufs_mb: Some(-1_350),
            })
        );
        assert_eq!(http.player_requests.load(Ordering::Relaxed), 1);
        assert_eq!(http.requested_ranges.lock().unwrap().len(), 1);
        assert!(matches!(
            updates.first(),
            Some(DownloadUpdate::Prepared {
                downloaded_bytes: 0,
                ..
            })
        ));
        assert_eq!(
            updates.last(),
            Some(&DownloadUpdate::Progress {
                downloaded_bytes: bytes.len() as u64,
                content_length: bytes.len() as u64,
            })
        );
        let cache_key = AudioQuality::Auto.playback_cache_key("fixture-video");
        let source = PlaybackSource::cache_only(
            cache_key,
            "audio/mp4; codecs=\"mp4a.40.2\"",
            bytes.len() as u64,
        );
        assert_eq!(read_downloaded(&downloads, &source), bytes);
    }

    #[test]
    fn public_download_flow_discards_complete_bytes_that_fail_media_validation() {
        let bytes = vec![9; 4_096];
        let directory = TestDirectory::new("public-invalid-flow");
        let (player_cache, downloads) = test_stores(&directory, bytes.len() as u64 * 2);
        let http = DownloadHttpClient::new(bytes.clone());
        let mut session = InnerTubeSession::default();
        session.visitor_data = Some("CgtDownloadFixtureVisitor".into());
        let client = Arc::new(InnerTubeClient::new(
            session,
            http.clone(),
            AudioQuality::Auto,
        ));

        let error = futures::executor::block_on(download_song(
            client,
            player_cache,
            downloads.clone(),
            "invalid-fixture-video",
            AudioQuality::Auto,
            Arc::new(AtomicBool::new(false)),
            |_| Ok(()),
        ))
        .unwrap_err();
        let cache_key = AudioQuality::Auto.playback_cache_key("invalid-fixture-video");
        let resource_key = format!("{cache_key}-{}", bytes.len());

        assert!(error.to_string().contains("media validation"));
        assert!(!error.to_string().contains("example.invalid"));
        assert_eq!(http.player_requests.load(Ordering::Relaxed), 2);
        assert_eq!(http.requested_ranges.lock().unwrap().len(), 2);
        assert_eq!(
            downloads
                .cached_resource_bytes(&resource_key, bytes.len() as u64, RANGE_CHUNK_SIZE)
                .unwrap(),
            0
        );
    }

    #[test]
    fn invalid_media_never_exposes_its_source_url_in_the_error() {
        let bytes = vec![7; 4_096];
        let directory = TestDirectory::new("invalid-media");
        let (_, downloads) = test_stores(&directory, bytes.len() as u64 * 2);
        let http = MemoryHttpClient::new(bytes.clone());
        let client = InnerTubeClient::new(
            InnerTubeSession::default(),
            http.clone(),
            AudioQuality::Auto,
        );
        let source = source("invalid-download", bytes.len());
        downloads
            .write_chunk(&source.disk_cache_key().unwrap(), 0, &bytes)
            .unwrap();

        let error = validate_download(&client, &source, downloads).unwrap_err();
        let message = error.to_string();

        assert!(message.contains("media validation"));
        assert!(!message.contains("example.invalid"));
        assert!(!message.contains("secret"));
        assert!(http.requested_ranges().is_empty());
    }

    #[test]
    #[ignore = "downloads and validates a complete anonymous YouTube Music audio stream"]
    fn live_audio_download_completes_in_isolated_storage() {
        let video_id = "FGBhQbmPwH8";
        let directory = TestDirectory::new("live-anonymous");
        let (player_cache, downloads) = test_stores(&directory, 64 * 1024 * 1024);
        let client = Arc::new(InnerTubeClient::anonymous(InnerTubeSession::default()));

        let outcome = futures::executor::block_on(download_song(
            client,
            player_cache,
            downloads.clone(),
            video_id,
            AudioQuality::Auto,
            Arc::new(AtomicBool::new(false)),
            |_| Ok(()),
        ))
        .unwrap();
        let DownloadOutcome::Completed(receipt) = outcome else {
            panic!("live download was cancelled unexpectedly");
        };
        let resource_key = format!(
            "{}-{}",
            receipt.audio_quality.playback_cache_key(video_id),
            receipt.content_length
        );

        assert!(receipt.mime_type.starts_with("audio/"));
        assert!(receipt.content_length > 0);
        assert!(receipt.loudness_lufs_mb.is_some());
        assert!(
            downloads
                .contains_complete_resource(&resource_key, receipt.content_length, RANGE_CHUNK_SIZE)
                .unwrap()
        );
    }
}
