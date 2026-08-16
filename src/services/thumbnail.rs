use std::{
    fs::{self, FileTimes, OpenOptions},
    io::{self, Write as _},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex, MutexGuard,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, SystemTime},
};

use futures::AsyncReadExt as _;
use gpui::ImageFormat;
use http_client::{
    AsyncBody, HttpClient, HttpRequestExt as _, RedirectPolicy, Request, StatusCode, Url,
};
use reqwest_client::ReqwestClient;

use crate::services::build_http_client;
use crate::{AppError, AppSettings, Result};

pub const DEFAULT_THUMBNAIL_CACHE_BYTES: u64 = 256 * 1024 * 1024;
const MAX_THUMBNAIL_BYTES: u64 = 16 * 1024 * 1024;
const CACHE_MAGIC: &[u8; 8] = b"MLIMG001";
const CACHE_HEADER_LENGTH: usize = CACHE_MAGIC.len() + 1 + 8 + 16;
static TEMPORARY_FILE_SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThumbnailImage {
    pub format: ImageFormat,
    pub bytes: Vec<u8>,
}

#[derive(Clone)]
pub struct ThumbnailCache {
    inner: Arc<ThumbnailCacheInner>,
}

struct ThumbnailCacheInner {
    root: PathBuf,
    max_bytes: u64,
    client: Arc<dyn HttpClient>,
    filesystem_lock: Mutex<()>,
}

impl ThumbnailCache {
    pub fn for_current_user(max_bytes: u64) -> io::Result<Self> {
        let root = dirs::cache_dir()
            .ok_or_else(|| io::Error::other("the operating system has no cache directory"))?
            .join("metrolist")
            .join("thumbnails");
        let client = ReqwestClient::user_agent("Metrolist/0.1 thumbnail loader")
            .map_err(|error| io::Error::other(error.to_string()))?;
        Self::new(root, max_bytes, Arc::new(client))
    }

    pub fn with_settings(settings: &AppSettings, max_bytes: u64) -> Result<Self> {
        let client = build_http_client(
            &settings.proxy,
            concat!(
                "Metrolist-rs/",
                env!("CARGO_PKG_VERSION"),
                " thumbnail-loader"
            ),
        )?;
        Self::new(settings.thumbnail_cache_root(), max_bytes, client).map_err(|error| {
            AppError::InvalidConfig(format!("thumbnail cache is unavailable: {error}"))
        })
    }

    pub fn new(root: PathBuf, max_bytes: u64, client: Arc<dyn HttpClient>) -> io::Result<Self> {
        if max_bytes == 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "thumbnail cache capacity must be positive",
            ));
        }
        fs::create_dir_all(&root)?;
        Ok(Self {
            inner: Arc::new(ThumbnailCacheInner {
                root,
                max_bytes,
                client,
                filesystem_lock: Mutex::new(()),
            }),
        })
    }

    pub async fn load(&self, url: &str) -> Result<ThumbnailImage> {
        let url =
            Url::parse(url).map_err(|_| AppError::Protocol("thumbnail URL is invalid".into()))?;
        if url.scheme() == "file" {
            return load_local_image(&url);
        }
        if !matches!(url.scheme(), "http" | "https") {
            return Err(AppError::Protocol(
                "thumbnail URL must use HTTP, HTTPS, or a local file".into(),
            ));
        }
        let key = stable_hash(url.as_str().as_bytes());
        match self.read_cached(key) {
            Ok(Some(image)) => return Ok(image),
            Ok(None) => {}
            Err(error) => tracing::warn!(%error, "thumbnail cache read failed"),
        }

        let request = Request::builder()
            .uri(url.as_str())
            .header(
                "Accept",
                "image/avif,image/webp,image/png,image/jpeg,image/*;q=0.8",
            )
            .follow_redirects(RedirectPolicy::FollowLimit(5))
            .timeout(Duration::from_secs(30))
            .body(AsyncBody::default())
            .map_err(|error| AppError::Network(error.to_string()))?;
        let mut response = self
            .inner
            .client
            .send(request)
            .await
            .map_err(|error| AppError::Network(error.to_string()))?;
        if response.status() != StatusCode::OK {
            return Err(AppError::Network(format!(
                "thumbnail request returned HTTP {}",
                response.status()
            )));
        }
        let content_type = response
            .headers()
            .get("content-type")
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.split(';').next())
            .map(str::trim)
            .map(str::to_owned);
        let mut bytes = Vec::new();
        response
            .body_mut()
            .take(MAX_THUMBNAIL_BYTES + 1)
            .read_to_end(&mut bytes)
            .await
            .map_err(|error| AppError::Network(error.to_string()))?;
        if bytes.is_empty() {
            return Err(AppError::Protocol("thumbnail response was empty".into()));
        }
        if bytes.len() as u64 > MAX_THUMBNAIL_BYTES {
            return Err(AppError::Protocol(
                "thumbnail response exceeded the 16 MiB limit".into(),
            ));
        }
        let format = detect_image_format(content_type.as_deref(), &bytes)
            .ok_or_else(|| AppError::Protocol("thumbnail format is unsupported".into()))?;
        if !validate_image(format, &bytes) {
            return Err(AppError::Protocol(
                "thumbnail image data could not be decoded".into(),
            ));
        }
        let image = ThumbnailImage { format, bytes };
        if let Err(error) = self.write_cached(key, &image) {
            tracing::warn!(%error, "thumbnail cache write failed");
        }
        Ok(image)
    }

    pub fn clear(&self) -> io::Result<()> {
        let _guard = self.lock_filesystem()?;
        match fs::remove_dir_all(&self.inner.root) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        fs::create_dir_all(&self.inner.root)
    }

    fn read_cached(&self, key: u128) -> io::Result<Option<ThumbnailImage>> {
        let _guard = self.lock_filesystem()?;
        let path = self.cache_path(key);
        let document = match fs::read(&path) {
            Ok(document) => document,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error),
        };
        let Some(image) = decode_cache_document(&document) else {
            let _ = fs::remove_file(path);
            return Ok(None);
        };
        let _ = OpenOptions::new()
            .write(true)
            .open(&path)
            .and_then(|file| file.set_times(FileTimes::new().set_modified(SystemTime::now())));
        Ok(Some(image))
    }

    fn write_cached(&self, key: u128, image: &ThumbnailImage) -> io::Result<()> {
        let document = encode_cache_document(image);
        let _guard = self.lock_filesystem()?;
        fs::create_dir_all(&self.inner.root)?;
        let destination = self.cache_path(key);
        let temporary = self.inner.root.join(format!(
            ".{key:032x}.{}.{}.tmp",
            std::process::id(),
            TEMPORARY_FILE_SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        let write_result = (|| {
            let mut file = OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(&temporary)?;
            file.write_all(&document)?;
            file.sync_all()?;
            drop(file);

            if let Err(first_error) = fs::rename(&temporary, &destination) {
                match fs::remove_file(&destination) {
                    Ok(()) => fs::rename(&temporary, &destination),
                    Err(error) if error.kind() == io::ErrorKind::NotFound => {
                        fs::rename(&temporary, &destination)
                    }
                    Err(_) => Err(first_error),
                }
            } else {
                Ok(())
            }
        })();
        if let Err(error) = write_result {
            let _ = fs::remove_file(&temporary);
            return Err(error);
        }
        self.enforce_capacity(Some(&destination))
    }

    fn lock_filesystem(&self) -> io::Result<MutexGuard<'_, ()>> {
        self.inner
            .filesystem_lock
            .lock()
            .map_err(|_| io::Error::other("thumbnail cache filesystem lock was poisoned"))
    }

    fn cache_path(&self, key: u128) -> PathBuf {
        self.inner.root.join(format!("{key:032x}.bin"))
    }

    fn enforce_capacity(&self, protected: Option<&Path>) -> io::Result<()> {
        let mut files = fs::read_dir(&self.inner.root)?
            .filter_map(std::result::Result::ok)
            .filter_map(|entry| {
                let path = entry.path();
                let metadata = entry.metadata().ok()?;
                (metadata.is_file() && path.extension().is_some_and(|ext| ext == "bin")).then(
                    || {
                        (
                            path,
                            metadata.len(),
                            metadata.modified().unwrap_or(SystemTime::UNIX_EPOCH),
                        )
                    },
                )
            })
            .collect::<Vec<_>>();
        let mut total = files.iter().map(|(_, length, _)| length).sum::<u64>();
        files.sort_by_key(|(path, _, modified)| {
            (
                protected.is_some_and(|protected| protected == path),
                *modified,
                path.clone(),
            )
        });
        for (path, length, _) in files {
            if total <= self.inner.max_bytes {
                break;
            }
            match fs::remove_file(path) {
                Ok(()) => total = total.saturating_sub(length),
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(error),
            }
        }
        Ok(())
    }
}

fn load_local_image(url: &Url) -> Result<ThumbnailImage> {
    let path = url
        .to_file_path()
        .map_err(|_| AppError::Protocol("local thumbnail path is invalid".into()))?;
    let metadata = fs::metadata(&path).map_err(|error| {
        AppError::Storage(format!(
            "could not inspect local thumbnail '{}': {error}",
            path.display()
        ))
    })?;
    if !metadata.is_file() {
        return Err(AppError::Protocol(
            "local thumbnail must be an image file".into(),
        ));
    }
    if metadata.len() > MAX_THUMBNAIL_BYTES {
        return Err(AppError::Protocol(
            "local thumbnail exceeded the 16 MiB limit".into(),
        ));
    }
    let bytes = fs::read(&path).map_err(|error| {
        AppError::Storage(format!(
            "could not read local thumbnail '{}': {error}",
            path.display()
        ))
    })?;
    if bytes.is_empty() {
        return Err(AppError::Protocol("local thumbnail was empty".into()));
    }
    let format = detect_image_format(None, &bytes)
        .ok_or_else(|| AppError::Protocol("local thumbnail format is unsupported".into()))?;
    if !validate_image(format, &bytes) {
        return Err(AppError::Protocol(
            "local thumbnail image data could not be decoded".into(),
        ));
    }
    Ok(ThumbnailImage { format, bytes })
}

fn encode_cache_document(image: &ThumbnailImage) -> Vec<u8> {
    let mut document = Vec::with_capacity(CACHE_HEADER_LENGTH + image.bytes.len());
    document.extend_from_slice(CACHE_MAGIC);
    document.push(format_code(image.format));
    document.extend_from_slice(&(image.bytes.len() as u64).to_le_bytes());
    document.extend_from_slice(&stable_hash(&image.bytes).to_le_bytes());
    document.extend_from_slice(&image.bytes);
    document
}

fn decode_cache_document(document: &[u8]) -> Option<ThumbnailImage> {
    if document.len() < CACHE_HEADER_LENGTH || &document[..CACHE_MAGIC.len()] != CACHE_MAGIC {
        return None;
    }
    let format = format_from_code(document[CACHE_MAGIC.len()])?;
    let length_start = CACHE_MAGIC.len() + 1;
    let expected_length =
        u64::from_le_bytes(document[length_start..length_start + 8].try_into().ok()?);
    let hash_start = length_start + 8;
    let expected_hash = u128::from_le_bytes(document[hash_start..hash_start + 16].try_into().ok()?);
    let bytes = document.get(CACHE_HEADER_LENGTH..)?.to_vec();
    if bytes.len() as u64 != expected_length
        || stable_hash(&bytes) != expected_hash
        || detect_image_format(Some(format.mime_type()), &bytes) != Some(format)
        || !validate_image(format, &bytes)
    {
        return None;
    }
    Some(ThumbnailImage { format, bytes })
}

fn detect_image_format(content_type: Option<&str>, bytes: &[u8]) -> Option<ImageFormat> {
    let detected = if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some(ImageFormat::Png)
    } else if bytes.starts_with(&[0xff, 0xd8, 0xff]) {
        Some(ImageFormat::Jpeg)
    } else if bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(b"WEBP") {
        Some(ImageFormat::Webp)
    } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        Some(ImageFormat::Gif)
    } else if bytes.starts_with(b"BM") {
        Some(ImageFormat::Bmp)
    } else if bytes.starts_with(b"II*\0") || bytes.starts_with(b"MM\0*") {
        Some(ImageFormat::Tiff)
    } else if bytes.starts_with(&[0, 0, 1, 0]) {
        Some(ImageFormat::Ico)
    } else if bytes.starts_with(b"P1")
        || bytes.starts_with(b"P2")
        || bytes.starts_with(b"P3")
        || bytes.starts_with(b"P4")
        || bytes.starts_with(b"P5")
        || bytes.starts_with(b"P6")
    {
        Some(ImageFormat::Pnm)
    } else if content_type == Some("image/svg+xml")
        && std::str::from_utf8(bytes)
            .ok()
            .is_some_and(|text| text.trim_start().starts_with("<svg"))
    {
        Some(ImageFormat::Svg)
    } else {
        None
    };
    let declared = content_type.and_then(ImageFormat::from_mime_type);
    match (detected, declared) {
        (Some(detected), Some(declared)) if detected != declared => None,
        (Some(detected), _) => Some(detected),
        _ => None,
    }
}

fn validate_image(format: ImageFormat, bytes: &[u8]) -> bool {
    let decoder_format = match format {
        ImageFormat::Png => image::ImageFormat::Png,
        ImageFormat::Jpeg => image::ImageFormat::Jpeg,
        ImageFormat::Webp => image::ImageFormat::WebP,
        ImageFormat::Gif => image::ImageFormat::Gif,
        ImageFormat::Bmp => image::ImageFormat::Bmp,
        ImageFormat::Tiff => image::ImageFormat::Tiff,
        ImageFormat::Ico => image::ImageFormat::Ico,
        ImageFormat::Pnm => image::ImageFormat::Pnm,
        ImageFormat::Svg => {
            return std::str::from_utf8(bytes)
                .ok()
                .is_some_and(|text| text.trim_start().starts_with("<svg"));
        }
    };
    image::load_from_memory_with_format(bytes, decoder_format).is_ok()
}

const fn format_code(format: ImageFormat) -> u8 {
    match format {
        ImageFormat::Png => 1,
        ImageFormat::Jpeg => 2,
        ImageFormat::Webp => 3,
        ImageFormat::Gif => 4,
        ImageFormat::Svg => 5,
        ImageFormat::Bmp => 6,
        ImageFormat::Tiff => 7,
        ImageFormat::Ico => 8,
        ImageFormat::Pnm => 9,
    }
}

const fn format_from_code(code: u8) -> Option<ImageFormat> {
    match code {
        1 => Some(ImageFormat::Png),
        2 => Some(ImageFormat::Jpeg),
        3 => Some(ImageFormat::Webp),
        4 => Some(ImageFormat::Gif),
        5 => Some(ImageFormat::Svg),
        6 => Some(ImageFormat::Bmp),
        7 => Some(ImageFormat::Tiff),
        8 => Some(ImageFormat::Ico),
        9 => Some(ImageFormat::Pnm),
        _ => None,
    }
}

fn stable_hash(bytes: &[u8]) -> u128 {
    let mut hash = 0x6c62_272e_07bb_0142_62b8_2175_6295_c58d_u128;
    for byte in bytes {
        hash ^= u128::from(*byte);
        hash = hash.wrapping_mul(0x0000_0000_0100_0000_0000_0000_0000_013b_u128);
    }
    hash
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, AtomicUsize};

    use futures::future::BoxFuture;
    use http_client::{Response, http::HeaderValue};

    fn png_bytes() -> Vec<u8> {
        use base64::{Engine as _, engine::general_purpose::STANDARD};
        STANDARD
            .decode("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNk+A8AAQUBAScY42YAAAAASUVORK5CYII=")
            .unwrap()
    }

    struct MemoryImageClient {
        online: AtomicBool,
        requests: AtomicUsize,
        bytes: Vec<u8>,
    }

    impl MemoryImageClient {
        fn new(bytes: Vec<u8>, online: bool) -> Arc<Self> {
            Arc::new(Self {
                online: AtomicBool::new(online),
                requests: AtomicUsize::new(0),
                bytes,
            })
        }
    }

    impl HttpClient for MemoryImageClient {
        fn user_agent(&self) -> Option<&HeaderValue> {
            None
        }

        fn proxy(&self) -> Option<&Url> {
            None
        }

        fn send(
            &self,
            _request: http_client::http::Request<AsyncBody>,
        ) -> BoxFuture<'static, http_client::Result<http_client::Response<AsyncBody>>> {
            self.requests.fetch_add(1, Ordering::Relaxed);
            if !self.online.load(Ordering::Relaxed) {
                return Box::pin(async { Err(http_client::anyhow!("network is offline")) });
            }
            let bytes = self.bytes.clone();
            Box::pin(async move {
                Ok(Response::builder()
                    .status(StatusCode::OK)
                    .header("Content-Type", "image/png")
                    .body(AsyncBody::from(bytes))
                    .unwrap())
            })
        }
    }

    fn temporary_root(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "metrolist-thumbnail-test-{name}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = fs::remove_dir_all(&root);
        root
    }

    #[test]
    fn thumbnail_round_trips_from_disk_without_a_network_request() {
        let root = temporary_root("offline-hit");
        let online = MemoryImageClient::new(png_bytes(), true);
        let cache = ThumbnailCache::new(root.clone(), 1_024, online.clone()).unwrap();
        let first =
            futures::executor::block_on(cache.load("https://example.test/cover.png")).unwrap();
        assert_eq!(online.requests.load(Ordering::Relaxed), 1);

        let offline = MemoryImageClient::new(Vec::new(), false);
        let reopened = ThumbnailCache::new(root.clone(), 1_024, offline.clone()).unwrap();
        let second =
            futures::executor::block_on(reopened.load("https://example.test/cover.png")).unwrap();

        assert_eq!(second, first);
        assert_eq!(offline.requests.load(Ordering::Relaxed), 0);
        assert!(fs::read_dir(&root).unwrap().all(|entry| {
            !entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .contains("cover")
        }));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn corrupt_disk_entry_is_removed_before_network_fallback() {
        let root = temporary_root("corruption");
        let online = MemoryImageClient::new(png_bytes(), true);
        let cache = ThumbnailCache::new(root.clone(), 1_024, online).unwrap();
        futures::executor::block_on(cache.load("https://example.test/corrupt.png")).unwrap();
        let path = cache.cache_path(stable_hash(b"https://example.test/corrupt.png"));
        fs::write(&path, b"partial").unwrap();

        let offline = MemoryImageClient::new(Vec::new(), false);
        let reopened = ThumbnailCache::new(root.clone(), 1_024, offline.clone()).unwrap();
        assert!(
            futures::executor::block_on(reopened.load("https://example.test/corrupt.png")).is_err()
        );
        assert_eq!(offline.requests.load(Ordering::Relaxed), 1);
        assert!(!path.exists());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn capacity_evicts_old_documents_and_preserves_valid_entries() {
        let root = temporary_root("capacity");
        let png_bytes = png_bytes();
        let document_length = (CACHE_HEADER_LENGTH + png_bytes.len()) as u64;
        let client = MemoryImageClient::new(png_bytes, true);
        let cache = ThumbnailCache::new(root.clone(), document_length, client).unwrap();

        futures::executor::block_on(cache.load("https://example.test/one.png")).unwrap();
        futures::executor::block_on(cache.load("https://example.test/two.png")).unwrap();

        let documents = fs::read_dir(&root)
            .unwrap()
            .filter_map(std::result::Result::ok)
            .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "bin"))
            .collect::<Vec<_>>();
        assert_eq!(documents.len(), 1);
        assert!(decode_cache_document(&fs::read(documents[0].path()).unwrap()).is_some());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    #[ignore = "downloads and decodes a live YouTube thumbnail"]
    fn live_youtube_thumbnail_downloads_and_decodes() {
        let root = temporary_root("live");
        let client = ReqwestClient::user_agent("Metrolist thumbnail integration test").unwrap();
        let cache = ThumbnailCache::new(root.clone(), 4 * 1024 * 1024, Arc::new(client)).unwrap();

        let image = futures::executor::block_on(
            cache.load("https://i.ytimg.com/vi/FGBhQbmPwH8/hqdefault.jpg"),
        )
        .unwrap();

        assert_eq!(image.format, ImageFormat::Jpeg);
        assert!(!image.bytes.is_empty());
        let _ = fs::remove_dir_all(root);
    }
}
