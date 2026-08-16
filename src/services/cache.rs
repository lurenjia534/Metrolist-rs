use std::{
    fs::{self, FileTimes, OpenOptions},
    io::{self, Write as _},
    path::{Path, PathBuf},
    sync::{
        Mutex, MutexGuard,
        atomic::{AtomicU64, Ordering},
    },
    time::SystemTime,
};

static TEMPORARY_FILE_SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[derive(Debug)]
pub struct AudioCache {
    root: PathBuf,
    max_bytes: Option<u64>,
    filesystem_lock: Mutex<()>,
}

impl AudioCache {
    pub fn new(root: PathBuf, max_bytes: u64) -> io::Result<Self> {
        if max_bytes == 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "audio cache capacity must be positive",
            ));
        }
        fs::create_dir_all(&root)?;
        Ok(Self {
            root,
            max_bytes: Some(max_bytes),
            filesystem_lock: Mutex::new(()),
        })
    }

    fn unbounded(root: PathBuf) -> io::Result<Self> {
        fs::create_dir_all(&root)?;
        Ok(Self {
            root,
            max_bytes: None,
            filesystem_lock: Mutex::new(()),
        })
    }

    pub fn for_current_user(max_bytes: u64) -> io::Result<Self> {
        let root = dirs::cache_dir()
            .ok_or_else(|| io::Error::other("the operating system has no cache directory"))?
            .join("metrolist")
            .join("audio");
        Self::new(root, max_bytes)
    }

    pub fn read_chunk(
        &self,
        key: &str,
        start: u64,
        expected_length: usize,
    ) -> io::Result<Option<Vec<u8>>> {
        let _guard = self.lock_filesystem()?;
        let path = self.chunk_path(key, start);
        let bytes = match fs::read(&path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error),
        };
        if bytes.len() != expected_length {
            let _ = fs::remove_file(path);
            return Ok(None);
        }
        // The modified time is the eviction recency marker. Failure to touch
        // a readable cache entry (for example on a read-only volume) must not
        // turn a valid offline hit into a playback failure.
        let _ = OpenOptions::new()
            .write(true)
            .open(&path)
            .and_then(|file| file.set_times(FileTimes::new().set_modified(SystemTime::now())));
        Ok(Some(bytes))
    }

    pub fn write_chunk(&self, key: &str, start: u64, bytes: &[u8]) -> io::Result<()> {
        let _guard = self.lock_filesystem()?;
        let directory = self.namespace_path(key);
        fs::create_dir_all(&directory)?;
        let destination = directory.join(chunk_filename(start));
        if destination
            .metadata()
            .is_ok_and(|metadata| metadata.len() == bytes.len() as u64)
        {
            let _ = OpenOptions::new()
                .write(true)
                .open(&destination)
                .and_then(|file| file.set_times(FileTimes::new().set_modified(SystemTime::now())));
            return self.enforce_capacity(Some(&destination));
        }
        let temporary = directory.join(format!(
            ".{}.{}.{}.tmp",
            chunk_filename(start),
            std::process::id(),
            TEMPORARY_FILE_SEQUENCE.fetch_add(1, Ordering::Relaxed),
        ));
        let write_result = (|| {
            let mut file = OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(&temporary)?;
            file.write_all(bytes)?;
            file.sync_all()?;
            drop(file);

            match fs::rename(&temporary, &destination) {
                Ok(()) => Ok(()),
                Err(_first_error)
                    if destination
                        .metadata()
                        .is_ok_and(|metadata| metadata.len() == bytes.len() as u64) =>
                {
                    let _ = fs::remove_file(&temporary);
                    Ok(())
                }
                Err(first_error) => {
                    // Windows does not replace an existing destination. An
                    // invalid old chunk is never a valid fallback, so remove
                    // it and retry the atomic commit once.
                    match fs::remove_file(&destination) {
                        Ok(()) => fs::rename(&temporary, &destination),
                        Err(error) if error.kind() == io::ErrorKind::NotFound => {
                            fs::rename(&temporary, &destination)
                        }
                        Err(_) => Err(first_error),
                    }
                }
            }
        })();
        if let Err(error) = write_result {
            let _ = fs::remove_file(&temporary);
            return Err(error);
        }
        self.enforce_capacity(Some(&destination))
    }

    pub fn contains_complete_resource(
        &self,
        key: &str,
        content_length: u64,
        chunk_size: usize,
    ) -> io::Result<bool> {
        if content_length == 0 || chunk_size == 0 {
            return Ok(false);
        }
        let _guard = self.lock_filesystem()?;
        let chunk_size = u64::try_from(chunk_size)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "chunk size is too large"))?;
        let mut start = 0_u64;
        while start < content_length {
            let expected_length = chunk_size.min(content_length - start);
            let path = self.chunk_path(key, start);
            let metadata = match path.metadata() {
                Ok(metadata) => metadata,
                Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
                Err(error) => return Err(error),
            };
            if !metadata.is_file() || metadata.len() != expected_length {
                let _ = fs::remove_file(path);
                return Ok(false);
            }
            start = start.saturating_add(chunk_size);
        }
        Ok(true)
    }

    pub fn cached_resource_bytes(
        &self,
        key: &str,
        content_length: u64,
        chunk_size: usize,
    ) -> io::Result<u64> {
        if content_length == 0 || chunk_size == 0 {
            return Ok(0);
        }
        let _guard = self.lock_filesystem()?;
        let chunk_size = u64::try_from(chunk_size)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "chunk size is too large"))?;
        let mut cached = 0_u64;
        let mut start = 0_u64;
        while start < content_length {
            let expected_length = chunk_size.min(content_length - start);
            let path = self.chunk_path(key, start);
            match path.metadata() {
                Ok(metadata) if metadata.is_file() && metadata.len() == expected_length => {
                    cached = cached.saturating_add(expected_length);
                }
                Ok(_) => {
                    let _ = fs::remove_file(path);
                }
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(error),
            }
            start = start.saturating_add(chunk_size);
        }
        Ok(cached)
    }

    pub fn remove_resource(&self, key: &str) -> io::Result<()> {
        let _guard = self.lock_filesystem()?;
        let directory = self.namespace_path(key);
        let entries = match fs::read_dir(&directory) {
            Ok(entries) => entries,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error),
        };
        for entry in entries {
            let entry = entry?;
            let metadata = entry.metadata()?;
            if metadata.is_file() {
                fs::remove_file(entry.path())?;
            } else {
                return Err(io::Error::other(
                    "audio resource directory contains an unexpected nested directory",
                ));
            }
        }
        match fs::remove_dir(directory) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error),
        }
    }

    fn lock_filesystem(&self) -> io::Result<MutexGuard<'_, ()>> {
        self.filesystem_lock
            .lock()
            .map_err(|_| io::Error::other("audio cache filesystem lock was poisoned"))
    }

    fn namespace_path(&self, key: &str) -> PathBuf {
        self.root.join(sanitize_key(key))
    }

    fn chunk_path(&self, key: &str, start: u64) -> PathBuf {
        self.namespace_path(key).join(chunk_filename(start))
    }

    fn enforce_capacity(&self, protected: Option<&Path>) -> io::Result<()> {
        let Some(max_bytes) = self.max_bytes else {
            return Ok(());
        };
        let mut files = Vec::new();
        collect_cache_files(&self.root, &mut files)?;
        let mut total = files.iter().map(|file| file.length).sum::<u64>();
        if total <= max_bytes {
            return Ok(());
        }

        files.sort_by_key(|file| {
            (
                protected.is_some_and(|protected| protected == file.path),
                file.modified,
                file.path.clone(),
            )
        });
        for file in files {
            if total <= max_bytes {
                break;
            }
            match fs::remove_file(&file.path) {
                Ok(()) => total = total.saturating_sub(file.length),
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(error),
            }
        }
        Ok(())
    }
}

#[derive(Debug)]
pub struct DownloadedAudioStore {
    inner: AudioCache,
}

impl DownloadedAudioStore {
    pub fn new(root: PathBuf) -> io::Result<Self> {
        Ok(Self {
            inner: AudioCache::unbounded(root)?,
        })
    }

    pub fn for_current_user() -> io::Result<Self> {
        let root = dirs::data_local_dir()
            .ok_or_else(|| io::Error::other("the operating system has no data directory"))?
            .join("metrolist")
            .join("downloads");
        Self::new(root)
    }

    pub fn read_chunk(
        &self,
        key: &str,
        start: u64,
        expected_length: usize,
    ) -> io::Result<Option<Vec<u8>>> {
        self.inner.read_chunk(key, start, expected_length)
    }

    pub fn write_chunk(&self, key: &str, start: u64, bytes: &[u8]) -> io::Result<()> {
        self.inner.write_chunk(key, start, bytes)
    }

    pub fn contains_complete_resource(
        &self,
        key: &str,
        content_length: u64,
        chunk_size: usize,
    ) -> io::Result<bool> {
        self.inner
            .contains_complete_resource(key, content_length, chunk_size)
    }

    pub fn cached_resource_bytes(
        &self,
        key: &str,
        content_length: u64,
        chunk_size: usize,
    ) -> io::Result<u64> {
        self.inner
            .cached_resource_bytes(key, content_length, chunk_size)
    }

    pub fn remove_resource(&self, key: &str) -> io::Result<()> {
        self.inner.remove_resource(key)
    }
}

struct CacheFile {
    path: PathBuf,
    length: u64,
    modified: SystemTime,
}

fn collect_cache_files(directory: &Path, files: &mut Vec<CacheFile>) -> io::Result<()> {
    let entries = match fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    for entry in entries {
        let entry = entry?;
        let metadata = entry.metadata()?;
        if metadata.is_dir() {
            collect_cache_files(&entry.path(), files)?;
        } else if metadata.is_file() && entry.path().extension().is_some_and(|ext| ext == "bin") {
            files.push(CacheFile {
                path: entry.path(),
                length: metadata.len(),
                modified: metadata.modified().unwrap_or(SystemTime::UNIX_EPOCH),
            });
        }
    }
    Ok(())
}

fn sanitize_key(key: &str) -> String {
    let sanitized = key
        .chars()
        .filter(|character| character.is_ascii_alphanumeric() || matches!(character, '-' | '_'))
        .take(96)
        .collect::<String>();
    if sanitized.is_empty() {
        "unknown".into()
    } else {
        sanitized
    }
}

fn chunk_filename(start: u64) -> String {
    format!("{start:016x}.bin")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{sync::Arc, thread, time::Duration};

    fn temporary_cache(name: &str, max_bytes: u64) -> AudioCache {
        let root = std::env::temp_dir().join(format!(
            "metrolist-cache-test-{name}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = fs::remove_dir_all(&root);
        AudioCache::new(root, max_bytes).unwrap()
    }

    fn count_temporary_files(directory: &Path) -> usize {
        fs::read_dir(directory)
            .into_iter()
            .flatten()
            .filter_map(std::result::Result::ok)
            .map(|entry| {
                if entry.path().is_dir() {
                    count_temporary_files(&entry.path())
                } else {
                    usize::from(entry.path().extension().is_some_and(|ext| ext == "tmp"))
                }
            })
            .sum()
    }

    #[test]
    fn downloaded_audio_is_unbounded_counted_and_removed_by_exact_resource() {
        let root = std::env::temp_dir().join(format!(
            "metrolist-download-store-test-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = fs::remove_dir_all(&root);
        let downloads = DownloadedAudioStore::new(root.clone()).unwrap();

        downloads.write_chunk("song-100", 0, &[1, 2, 3, 4]).unwrap();
        downloads.write_chunk("song-100", 4, &[5, 6]).unwrap();
        assert_eq!(
            downloads.cached_resource_bytes("song-100", 6, 4).unwrap(),
            6
        );
        assert!(
            downloads
                .contains_complete_resource("song-100", 6, 4)
                .unwrap()
        );
        downloads.remove_resource("song-100").unwrap();
        assert_eq!(
            downloads.cached_resource_bytes("song-100", 6, 4).unwrap(),
            0
        );

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn cache_round_trips_complete_chunks_and_rejects_partial_files() {
        let cache = temporary_cache("round-trip", 1_024);
        cache.write_chunk("video/id", 0, &[1, 2, 3, 4]).unwrap();

        assert_eq!(
            cache.read_chunk("video/id", 0, 4).unwrap(),
            Some(vec![1, 2, 3, 4])
        );
        assert_eq!(cache.read_chunk("video/id", 0, 5).unwrap(), None);
        let _ = fs::remove_dir_all(&cache.root);
    }

    #[test]
    fn complete_resource_check_requires_every_exact_chunk() {
        let cache = temporary_cache("complete-resource", 2_048);
        cache.write_chunk("video-10", 0, &[1; 4]).unwrap();
        assert!(!cache.contains_complete_resource("video-10", 10, 4).unwrap());

        cache.write_chunk("video-10", 4, &[2; 4]).unwrap();
        cache.write_chunk("video-10", 8, &[3; 2]).unwrap();
        assert!(cache.contains_complete_resource("video-10", 10, 4).unwrap());

        cache.write_chunk("video-10", 8, &[3]).unwrap();
        assert!(!cache.contains_complete_resource("video-10", 10, 4).unwrap());
        assert_eq!(cache.read_chunk("video-10", 8, 2).unwrap(), None);
        let _ = fs::remove_dir_all(&cache.root);
    }

    #[test]
    fn cache_evicts_files_until_it_is_within_capacity() {
        let cache = temporary_cache("capacity", 6);
        cache.write_chunk("one", 0, &[1, 2, 3, 4]).unwrap();
        cache.write_chunk("two", 0, &[5, 6, 7, 8]).unwrap();

        let mut files = Vec::new();
        collect_cache_files(&cache.root, &mut files).unwrap();
        assert!(files.iter().map(|file| file.length).sum::<u64>() <= 6);
        assert_eq!(files.len(), 1);
        let _ = fs::remove_dir_all(&cache.root);
    }

    #[test]
    fn concurrent_writers_commit_one_complete_chunk_without_temporary_leaks() {
        const WRITER_COUNT: usize = 16;
        const CHUNK_LENGTH: usize = 64 * 1024;
        let cache = Arc::new(temporary_cache(
            "concurrent-same-chunk",
            2 * CHUNK_LENGTH as u64,
        ));
        let barrier = Arc::new(std::sync::Barrier::new(WRITER_COUNT));
        let writers = (0..WRITER_COUNT)
            .map(|value| {
                let cache = cache.clone();
                let barrier = barrier.clone();
                thread::spawn(move || {
                    barrier.wait();
                    cache.write_chunk("shared-video", 0, &vec![value as u8; CHUNK_LENGTH])
                })
            })
            .collect::<Vec<_>>();
        for writer in writers {
            writer.join().unwrap().unwrap();
        }

        let cached = cache
            .read_chunk("shared-video", 0, CHUNK_LENGTH)
            .unwrap()
            .unwrap();
        assert_eq!(cached.len(), CHUNK_LENGTH);
        assert!(cached.iter().all(|byte| *byte == cached[0]));
        assert!((cached[0] as usize) < WRITER_COUNT);
        let mut files = Vec::new();
        collect_cache_files(&cache.root, &mut files).unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(count_temporary_files(&cache.root), 0);
        let _ = fs::remove_dir_all(&cache.root);
    }

    #[test]
    fn concurrent_eviction_keeps_capacity_and_complete_file_invariants() {
        const WRITER_COUNT: usize = 24;
        const CHUNK_LENGTH: usize = 4 * 1024;
        let capacity = 5 * CHUNK_LENGTH as u64;
        let cache = Arc::new(temporary_cache("concurrent-eviction", capacity));
        let barrier = Arc::new(std::sync::Barrier::new(WRITER_COUNT));
        let writers = (0..WRITER_COUNT)
            .map(|index| {
                let cache = cache.clone();
                let barrier = barrier.clone();
                thread::spawn(move || {
                    barrier.wait();
                    cache.write_chunk(
                        &format!("video-{index}"),
                        0,
                        &vec![index as u8; CHUNK_LENGTH],
                    )
                })
            })
            .collect::<Vec<_>>();
        for writer in writers {
            writer.join().unwrap().unwrap();
        }

        let mut files = Vec::new();
        collect_cache_files(&cache.root, &mut files).unwrap();
        assert!(files.iter().map(|file| file.length).sum::<u64>() <= capacity);
        assert!(files.iter().all(|file| file.length == CHUNK_LENGTH as u64));
        assert_eq!(count_temporary_files(&cache.root), 0);
        let _ = fs::remove_dir_all(&cache.root);
    }

    #[test]
    fn reading_a_chunk_refreshes_its_eviction_recency() {
        let cache = temporary_cache("read-recency", 8);
        cache.write_chunk("one", 0, &[1, 1, 1, 1]).unwrap();
        cache.write_chunk("two", 0, &[2, 2, 2, 2]).unwrap();
        OpenOptions::new()
            .write(true)
            .open(cache.chunk_path("one", 0))
            .unwrap()
            .set_times(
                FileTimes::new().set_modified(SystemTime::UNIX_EPOCH + Duration::from_secs(1)),
            )
            .unwrap();
        OpenOptions::new()
            .write(true)
            .open(cache.chunk_path("two", 0))
            .unwrap()
            .set_times(
                FileTimes::new().set_modified(SystemTime::UNIX_EPOCH + Duration::from_secs(2)),
            )
            .unwrap();

        assert!(cache.read_chunk("one", 0, 4).unwrap().is_some());
        cache.write_chunk("three", 0, &[3, 3, 3, 3]).unwrap();

        assert!(cache.read_chunk("one", 0, 4).unwrap().is_some());
        assert_eq!(cache.read_chunk("two", 0, 4).unwrap(), None);
        assert!(cache.read_chunk("three", 0, 4).unwrap().is_some());
        let _ = fs::remove_dir_all(&cache.root);
    }
}
