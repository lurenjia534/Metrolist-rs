use std::{
    collections::{HashMap, HashSet},
    fs::{self, OpenOptions},
    io::Write as _,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, SystemTime},
};

use futures::{AsyncReadExt as _, future::join_all};
use http_client::{AsyncBody, HttpClient, Request, Url};
use serde::Deserialize;
use sha1::{Digest as _, Sha1};

use crate::{
    AppError, AppSettings, EqualizerProfile, ParametricEqualizer, Result,
    services::{MAX_EQUALIZER_APO_FILE_BYTES, build_http_client, parse_equalizer_apo},
};

const AUTO_EQ_TREE_URL: &str =
    "https://api.github.com/repos/ndellagrotte/AutoEq/git/trees/master?recursive=1";
const AUTO_EQ_RAW_BASE_URL: &str = "https://raw.githubusercontent.com/ndellagrotte/AutoEq/master/";
const AUTO_EQ_USER_AGENT: &str = "Metrolist-rs/0.1 AutoEQ client";
const GITHUB_ACCEPT: &str = "application/vnd.github+json";
const GITHUB_API_VERSION: &str = "2022-11-28";

pub const AUTO_EQ_CACHE_TTL: Duration = Duration::from_secs(24 * 60 * 60);
pub const MAX_AUTO_EQ_TREE_BYTES: usize = 32 * 1024 * 1024;
pub const MAX_AUTO_EQ_NAME_INDEX_BYTES: usize = 8 * 1024 * 1024;
pub const MAX_AUTO_EQ_SEARCH_RESULTS: usize = 100;

static CACHE_TEMPORARY_SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AutoEqIndexOrigin {
    Downloaded,
    FreshCache,
    StaleCache,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AutoEqEntry {
    pub label: String,
    pub form: String,
    pub rig: String,
    pub source: String,
    pub form_directory: String,
    pub repo_path: String,
}

impl AutoEqEntry {
    pub fn profile_id(&self) -> String {
        let digest = Sha1::digest(self.repo_path.as_bytes());
        let mut id = String::with_capacity(47);
        id.push_str("autoeq:");
        for byte in digest {
            use std::fmt::Write as _;
            let _ = write!(id, "{byte:02x}");
        }
        id
    }

    pub fn into_profile(
        self,
        equalizer: ParametricEqualizer,
        added_at_ms: i64,
    ) -> Result<EqualizerProfile> {
        let profile = EqualizerProfile {
            id: self.profile_id(),
            name: self.label.clone(),
            device_model: self.label,
            equalizer,
            source: self.source,
            rig: self.rig,
            // This mirrors Android's downloaded-profile behavior: a saved
            // AutoEQ result remains editable and removable by the user.
            is_custom: true,
            added_at_ms,
        };
        profile.validate()?;
        Ok(profile)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AutoEqIndex {
    pub revision: String,
    pub entries: Vec<AutoEqEntry>,
    pub truncated: bool,
    pub origin: AutoEqIndexOrigin,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AutoEqModel {
    pub name: String,
    pub variants: Vec<AutoEqEntry>,
}

#[derive(Clone)]
pub struct AutoEqClient {
    http: Arc<dyn HttpClient>,
    cache_root: PathBuf,
}

impl AutoEqClient {
    pub fn with_settings(settings: &AppSettings) -> Result<Self> {
        Ok(Self {
            http: build_http_client(&settings.proxy, AUTO_EQ_USER_AGENT)?,
            cache_root: settings.cache_root.join("autoeq"),
        })
    }

    #[cfg(test)]
    fn new(http: Arc<dyn HttpClient>, cache_root: PathBuf) -> Self {
        Self { http, cache_root }
    }

    pub fn is_database_cached(&self) -> bool {
        self.tree_cache_path().is_file()
    }

    pub async fn build_index(&self) -> Result<AutoEqIndex> {
        fs::create_dir_all(&self.cache_root).map_err(cache_error)?;
        let cache_path = self.tree_cache_path();
        let cached = read_cache(&cache_path, MAX_AUTO_EQ_TREE_BYTES)
            .and_then(|bytes| parse_tree(&bytes).ok().map(|tree| (bytes, tree)));

        if let Some((_, tree)) = cached.as_ref()
            && cache_is_fresh(&cache_path)
        {
            return index_from_tree(tree, AutoEqIndexOrigin::FreshCache);
        }

        match self
            .fetch(AUTO_EQ_TREE_URL, MAX_AUTO_EQ_TREE_BYTES, true)
            .await
        {
            Ok(bytes) => match parse_tree(&bytes) {
                Ok(tree) => {
                    let index = index_from_tree(&tree, AutoEqIndexOrigin::Downloaded)?;
                    write_cache(&cache_path, &bytes).map_err(cache_error)?;
                    Ok(index)
                }
                Err(error) => cached
                    .as_ref()
                    .map(|(_, tree)| index_from_tree(tree, AutoEqIndexOrigin::StaleCache))
                    .unwrap_or(Err(error)),
            },
            Err(error) => cached
                .as_ref()
                .map(|(_, tree)| index_from_tree(tree, AutoEqIndexOrigin::StaleCache))
                .unwrap_or(Err(error)),
        }
    }

    pub async fn resolve_variant_rigs(&self, variants: Vec<AutoEqEntry>) -> Vec<AutoEqEntry> {
        let sources = variants
            .iter()
            .filter(|entry| entry.rig == "unknown")
            .map(|entry| entry.source.clone())
            .collect::<HashSet<_>>();
        let lookups = join_all(sources.into_iter().map(|source| {
            let client = self.clone();
            async move {
                let result = client.load_rig_index(&source).await.unwrap_or_default();
                (source, result)
            }
        }))
        .await
        .into_iter()
        .collect::<HashMap<_, _>>();

        variants
            .into_iter()
            .map(|mut entry| {
                if entry.rig == "unknown"
                    && let Some(rig) = lookups
                        .get(&entry.source)
                        .and_then(|lookup| lookup.get(&entry.label))
                {
                    entry.rig.clone_from(rig);
                }
                entry
            })
            .collect()
    }

    pub async fn load_equalizer(&self, entry: &AutoEqEntry) -> Result<ParametricEqualizer> {
        validate_repo_path(&entry.repo_path)?;
        if !entry.repo_path.starts_with("results/")
            || !entry.repo_path.ends_with(" ParametricEQ.txt")
        {
            return Err(AppError::InvalidConfig(
                "AutoEQ entry does not identify a ParametricEQ result".into(),
            ));
        }
        let bytes = self
            .cached_or_fetch_raw(&entry.repo_path, MAX_EQUALIZER_APO_FILE_BYTES)
            .await?;
        let content = std::str::from_utf8(&bytes).map_err(|_| {
            AppError::Protocol("AutoEQ ParametricEQ result is not valid UTF-8".into())
        })?;
        parse_equalizer_apo(content)
    }

    async fn load_rig_index(&self, source: &str) -> Result<HashMap<String, String>> {
        if source == "Headphone.com Legacy" || source == "Innerfidelity" {
            return Ok(HashMap::new());
        }
        let repo_path = format!("measurements/{source}/name_index.tsv");
        let bytes = self
            .cached_or_fetch_raw(&repo_path, MAX_AUTO_EQ_NAME_INDEX_BYTES)
            .await?;
        parse_name_index(&bytes)
    }

    async fn cached_or_fetch_raw(&self, repo_path: &str, limit: usize) -> Result<Vec<u8>> {
        validate_repo_path(repo_path)?;
        let cache_path = cache_path_for_repo_path(&self.cache_root, repo_path)?;
        let cached = read_cache(&cache_path, limit);
        if cache_is_fresh(&cache_path)
            && let Some(bytes) = cached.as_ref()
        {
            return Ok(bytes.clone());
        }

        let url = raw_url(repo_path)?;
        match self.fetch(url.as_str(), limit, false).await {
            Ok(bytes) => {
                write_cache(&cache_path, &bytes).map_err(cache_error)?;
                Ok(bytes)
            }
            Err(error) => cached.ok_or(error),
        }
    }

    async fn fetch(&self, url: &str, limit: usize, github_api: bool) -> Result<Vec<u8>> {
        let mut builder = Request::builder().uri(url);
        if github_api {
            builder = builder
                .header("Accept", GITHUB_ACCEPT)
                .header("X-GitHub-Api-Version", GITHUB_API_VERSION);
        }
        let request = builder.body(AsyncBody::default()).map_err(|error| {
            AppError::Network(format!("could not build AutoEQ request: {error}"))
        })?;
        let mut response = self
            .http
            .send(request)
            .await
            .map_err(|error| AppError::Network(format!("AutoEQ request failed: {error}")))?;
        if !response.status().is_success() {
            return Err(AppError::Network(format!(
                "AutoEQ server returned HTTP {}",
                response.status()
            )));
        }
        let mut bytes = Vec::new();
        response
            .body_mut()
            .take((limit as u64).saturating_add(1))
            .read_to_end(&mut bytes)
            .await
            .map_err(|error| {
                AppError::Network(format!("AutoEQ response could not be read: {error}"))
            })?;
        if bytes.len() > limit {
            return Err(AppError::Protocol(format!(
                "AutoEQ response exceeds the {} MiB safety limit",
                limit.div_ceil(1024 * 1024)
            )));
        }
        Ok(bytes)
    }

    fn tree_cache_path(&self) -> PathBuf {
        self.cache_root.join("tree.json")
    }
}

pub fn search_auto_eq_models(
    index: &AutoEqIndex,
    query: &str,
    max_results: usize,
) -> Vec<AutoEqModel> {
    let query = query.trim().to_lowercase();
    let mut grouped = HashMap::<String, AutoEqModel>::new();
    for entry in &index.entries {
        let name = normalize_model_name(&entry.label);
        let key = name.to_lowercase();
        if !query.is_empty() && !key.contains(&query) {
            continue;
        }
        grouped
            .entry(key)
            .or_insert_with(|| AutoEqModel {
                name,
                variants: Vec::new(),
            })
            .variants
            .push(entry.clone());
    }
    let mut models = grouped.into_values().collect::<Vec<_>>();
    for model in &mut models {
        model.variants.sort_by(|left, right| {
            left.source
                .to_lowercase()
                .cmp(&right.source.to_lowercase())
                .then_with(|| left.rig.to_lowercase().cmp(&right.rig.to_lowercase()))
                .then_with(|| left.repo_path.cmp(&right.repo_path))
        });
    }
    models.sort_by(|left, right| {
        relevance(&left.name, &query)
            .cmp(&relevance(&right.name, &query))
            .then_with(|| left.name.to_lowercase().cmp(&right.name.to_lowercase()))
            .then_with(|| left.name.cmp(&right.name))
    });
    models.truncate(max_results.min(MAX_AUTO_EQ_SEARCH_RESULTS));
    models
}

pub fn normalize_model_name(model_name: &str) -> String {
    let mut output = String::with_capacity(model_name.len());
    let mut depth = 0_u32;
    for character in model_name.chars() {
        match character {
            '(' => depth = depth.saturating_add(1),
            ')' if depth > 0 => depth -= 1,
            _ if depth == 0 => output.push(character),
            _ => {}
        }
    }
    output.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn relevance(name: &str, query: &str) -> u8 {
    let name = name.to_lowercase();
    if query.is_empty() {
        3
    } else if name == query {
        0
    } else if name.starts_with(query) {
        1
    } else {
        2
    }
}

#[derive(Deserialize)]
struct GitHubTreeResponse {
    sha: String,
    tree: Vec<GitHubTreeNode>,
    #[serde(default)]
    truncated: bool,
}

#[derive(Deserialize)]
struct GitHubTreeNode {
    path: String,
    #[serde(rename = "type")]
    kind: String,
}

fn parse_tree(bytes: &[u8]) -> Result<GitHubTreeResponse> {
    let tree = serde_json::from_slice::<GitHubTreeResponse>(bytes)
        .map_err(|_| AppError::Protocol("AutoEQ GitHub tree response is invalid".into()))?;
    if tree.sha.len() < 7
        || tree.sha.len() > 64
        || !tree.sha.bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        return Err(AppError::Protocol(
            "AutoEQ GitHub tree response has an invalid revision".into(),
        ));
    }
    if tree.tree.len() > 100_000 {
        return Err(AppError::Protocol(
            "AutoEQ GitHub tree response contains too many entries".into(),
        ));
    }
    Ok(tree)
}

fn index_from_tree(tree: &GitHubTreeResponse, origin: AutoEqIndexOrigin) -> Result<AutoEqIndex> {
    let mut seen = HashSet::new();
    let mut entries = Vec::new();
    for node in &tree.tree {
        if node.kind != "blob"
            || !node.path.starts_with("results/")
            || !node.path.ends_with(" ParametricEQ.txt")
            || !seen.insert(node.path.as_str())
        {
            continue;
        }
        if let Some(entry) = parse_entry_path(&node.path) {
            entries.push(entry);
        }
    }
    if entries.is_empty() {
        return Err(AppError::Protocol(
            "AutoEQ GitHub tree contains no ParametricEQ results".into(),
        ));
    }
    entries.sort_by(|left, right| left.repo_path.cmp(&right.repo_path));
    Ok(AutoEqIndex {
        revision: tree.sha.clone(),
        entries,
        truncated: tree.truncated,
        origin,
    })
}

fn parse_entry_path(path: &str) -> Option<AutoEqEntry> {
    validate_repo_path(path).ok()?;
    let parts = path.split('/').collect::<Vec<_>>();
    if parts.len() != 5 || parts[0] != "results" {
        return None;
    }
    let source = parts[1];
    let form_directory = parts[2];
    let label = parts[3];
    if parts[4] != format!("{label} ParametricEQ.txt") {
        return None;
    }
    let (form, directory_rig) = parse_form_and_rig(form_directory);
    let rig = if directory_rig != "unknown" {
        directory_rig
    } else if source == "Headphone.com Legacy" || source == "Innerfidelity" {
        "HMS II.3".into()
    } else {
        "unknown".into()
    };
    Some(AutoEqEntry {
        label: label.into(),
        form,
        rig,
        source: source.into(),
        form_directory: form_directory.into(),
        repo_path: path.into(),
    })
}

fn parse_form_and_rig(form_directory: &str) -> (String, String) {
    let lower = form_directory.to_ascii_lowercase();
    for form in ["in-ear", "over-ear", "earbud"] {
        if let Some(start) = lower.find(form) {
            let end = start + form.len();
            let mut rig = String::with_capacity(form_directory.len() - form.len());
            rig.push_str(&form_directory[..start]);
            rig.push_str(&form_directory[end..]);
            let rig = rig.trim();
            return (
                form.into(),
                if rig.is_empty() { "unknown" } else { rig }.into(),
            );
        }
    }
    ("unknown".into(), form_directory.trim().to_owned())
}

fn parse_name_index(bytes: &[u8]) -> Result<HashMap<String, String>> {
    let content = std::str::from_utf8(bytes)
        .map_err(|_| AppError::Protocol("AutoEQ name index is not valid UTF-8".into()))?;
    let mut rigs = HashMap::new();
    for line in content.lines().skip(1) {
        let columns = line.split('\t').collect::<Vec<_>>();
        if columns.len() < 5 {
            continue;
        }
        let name = columns[2].trim();
        let rig = columns[4].trim();
        if !name.is_empty() && !rig.is_empty() && rig != "ignore" {
            rigs.insert(name.to_owned(), rig.to_owned());
        }
    }
    Ok(rigs)
}

fn validate_repo_path(path: &str) -> Result<()> {
    if path.is_empty()
        || path.starts_with('/')
        || path.contains('\\')
        || path.chars().any(char::is_control)
        || path
            .split('/')
            .any(|segment| segment.is_empty() || segment == "." || segment == "..")
    {
        return Err(AppError::Protocol(
            "AutoEQ repository path is unsafe".into(),
        ));
    }
    Ok(())
}

fn raw_url(repo_path: &str) -> Result<Url> {
    validate_repo_path(repo_path)?;
    let mut url = Url::parse(AUTO_EQ_RAW_BASE_URL)
        .map_err(|error| AppError::Network(format!("AutoEQ base URL is invalid: {error}")))?;
    {
        let mut segments = url
            .path_segments_mut()
            .map_err(|_| AppError::Network("AutoEQ base URL cannot accept paths".into()))?;
        segments.pop_if_empty();
        for segment in repo_path.split('/') {
            segments.push(segment);
        }
    }
    Ok(url)
}

fn cache_path_for_repo_path(cache_root: &Path, repo_path: &str) -> Result<PathBuf> {
    validate_repo_path(repo_path)?;
    let mut path = cache_root.to_owned();
    for segment in repo_path.split('/') {
        path.push(segment);
    }
    Ok(path)
}

fn cache_is_fresh(path: &Path) -> bool {
    let Ok(modified) = path.metadata().and_then(|metadata| metadata.modified()) else {
        return false;
    };
    SystemTime::now()
        .duration_since(modified)
        .map_or(true, |age| age < AUTO_EQ_CACHE_TTL)
}

fn read_cache(path: &Path, limit: usize) -> Option<Vec<u8>> {
    let metadata = path.metadata().ok()?;
    if !metadata.is_file() || metadata.len() > limit as u64 {
        return None;
    }
    fs::read(path).ok().filter(|bytes| bytes.len() <= limit)
}

fn write_cache(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| std::io::Error::other("cache path has no parent"))?;
    fs::create_dir_all(parent)?;
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("autoeq-cache");
    let temporary = parent.join(format!(
        ".{file_name}.{}.{}.tmp",
        std::process::id(),
        CACHE_TEMPORARY_SEQUENCE.fetch_add(1, Ordering::Relaxed)
    ));
    let result = (|| {
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&temporary)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        match fs::rename(&temporary, path) {
            Ok(()) => Ok(()),
            Err(first_error) if path.is_file() => {
                fs::remove_file(path)?;
                fs::rename(&temporary, path).map_err(|_| first_error)
            }
            Err(error) => Err(error),
        }
    })();
    if result.is_err() {
        let _ = fs::remove_file(temporary);
    }
    result
}

fn cache_error(error: std::io::Error) -> AppError {
    AppError::Storage(format!("AutoEQ cache is unavailable: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::future::BoxFuture;
    use http_client::{Response, StatusCode};
    use std::sync::Mutex;

    const TREE: &[u8] = br#"{
      "sha":"0123456789abcdef0123456789abcdef01234567",
      "truncated":false,
      "tree":[
        {"path":"results/oratory1990/GRAS 45BC-10 over-ear/Sony WH-1000XM5 (ANC on)/Sony WH-1000XM5 (ANC on) ParametricEQ.txt","type":"blob"},
        {"path":"results/crinacle/711 in-ear/7Hz Zero/7Hz Zero ParametricEQ.txt","type":"blob"},
        {"path":"results/Innerfidelity/over-ear/Sony WH-1000XM5/Sony WH-1000XM5 ParametricEQ.txt","type":"blob"},
        {"path":"README.md","type":"blob"}
      ]
    }"#;

    type FixtureResponse = std::result::Result<(StatusCode, Vec<u8>), String>;

    struct FixtureHttp {
        responses: Mutex<Vec<FixtureResponse>>,
        uris: Mutex<Vec<String>>,
    }

    impl FixtureHttp {
        fn new(responses: Vec<FixtureResponse>) -> Self {
            Self {
                responses: Mutex::new(responses.into_iter().rev().collect()),
                uris: Mutex::new(Vec::new()),
            }
        }
    }

    impl HttpClient for FixtureHttp {
        fn user_agent(&self) -> Option<&http_client::http::HeaderValue> {
            None
        }

        fn proxy(&self) -> Option<&Url> {
            None
        }

        fn send(
            &self,
            request: http_client::http::Request<AsyncBody>,
        ) -> BoxFuture<'static, http_client::Result<Response<AsyncBody>>> {
            self.uris.lock().unwrap().push(request.uri().to_string());
            let result = self.responses.lock().unwrap().pop().unwrap();
            Box::pin(async move {
                match result {
                    Ok((status, bytes)) => Ok(Response::builder()
                        .status(status)
                        .body(AsyncBody::from(bytes))
                        .unwrap()),
                    Err(message) => Err(std::io::Error::other(message).into()),
                }
            })
        }
    }

    fn temporary_directory(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "metrolist-autoeq-{name}-{}-{}",
            std::process::id(),
            CACHE_TEMPORARY_SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ))
    }

    #[test]
    fn tree_paths_build_searchable_models_and_preserve_variants() {
        let tree = parse_tree(TREE).unwrap();
        let index = index_from_tree(&tree, AutoEqIndexOrigin::Downloaded).unwrap();
        assert_eq!(index.entries.len(), 3);
        let in_ear = index
            .entries
            .iter()
            .find(|entry| entry.label == "7Hz Zero")
            .unwrap();
        assert_eq!(in_ear.form, "in-ear");
        assert_eq!(in_ear.rig, "711");
        let models = search_auto_eq_models(&index, "sony wh-1000xm5", 100);
        assert_eq!(models.len(), 1);
        assert_eq!(models[0].name, "Sony WH-1000XM5");
        assert_eq!(models[0].variants.len(), 2);
        assert!(
            models[0]
                .variants
                .iter()
                .any(|entry| entry.rig == "HMS II.3")
        );
    }

    #[test]
    fn search_orders_exact_then_prefix_then_substring_and_caps_results() {
        let mut index =
            index_from_tree(&parse_tree(TREE).unwrap(), AutoEqIndexOrigin::Downloaded).unwrap();
        for label in ["Zero", "Zero Red", "Super Zero"] {
            index.entries.push(AutoEqEntry {
                label: label.into(),
                form: "in-ear".into(),
                rig: "711".into(),
                source: "fixture".into(),
                form_directory: "711 in-ear".into(),
                repo_path: format!("results/fixture/711 in-ear/{label}/{label} ParametricEQ.txt"),
            });
        }
        let models = search_auto_eq_models(&index, "zero", 2);
        assert_eq!(
            models
                .iter()
                .map(|model| model.name.as_str())
                .collect::<Vec<_>>(),
            ["Zero", "Zero Red"]
        );
    }

    #[test]
    fn raw_urls_encode_each_path_segment_and_reject_traversal() {
        let url = raw_url("results/source/711 in-ear/A&B/A&B ParametricEQ.txt").unwrap();
        assert_eq!(
            url.as_str(),
            "https://raw.githubusercontent.com/ndellagrotte/AutoEq/master/results/source/711%20in-ear/A&B/A&B%20ParametricEQ.txt"
        );
        assert!(raw_url("results/../secret").is_err());
        assert!(cache_path_for_repo_path(Path::new("/tmp/cache"), "results/../../secret").is_err());
    }

    #[test]
    fn downloads_tree_once_then_uses_fresh_cache() {
        let root = temporary_directory("fresh-cache");
        let http = Arc::new(FixtureHttp::new(vec![Ok((StatusCode::OK, TREE.to_vec()))]));
        let client = AutoEqClient::new(http.clone(), root.clone());
        let first = futures::executor::block_on(client.build_index()).unwrap();
        let second = futures::executor::block_on(client.build_index()).unwrap();
        assert_eq!(first.origin, AutoEqIndexOrigin::Downloaded);
        assert_eq!(second.origin, AutoEqIndexOrigin::FreshCache);
        assert_eq!(http.uris.lock().unwrap().len(), 1);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn stale_valid_cache_survives_network_failure() {
        let root = temporary_directory("stale-cache");
        let http = Arc::new(FixtureHttp::new(vec![Err("offline".into())]));
        let client = AutoEqClient::new(http, root.clone());
        fs::create_dir_all(&root).unwrap();
        fs::write(client.tree_cache_path(), TREE).unwrap();
        let stale = SystemTime::now() - AUTO_EQ_CACHE_TTL - Duration::from_secs(1);
        let file = OpenOptions::new()
            .write(true)
            .open(client.tree_cache_path())
            .unwrap();
        file.set_times(std::fs::FileTimes::new().set_modified(stale))
            .unwrap();
        let index = futures::executor::block_on(client.build_index()).unwrap();
        assert_eq!(index.origin, AutoEqIndexOrigin::StaleCache);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn equalizer_download_is_bounded_parsed_and_cached() {
        let root = temporary_directory("profile");
        let apo = b"Preamp: -4.2 dB\nFilter 1: ON PK Fc 1000 Hz Gain 4.2 dB Q 1.0\n";
        let http = Arc::new(FixtureHttp::new(vec![Ok((StatusCode::OK, apo.to_vec()))]));
        let client = AutoEqClient::new(http.clone(), root.clone());
        let entry =
            parse_entry_path("results/source/711 in-ear/Fixture/Fixture ParametricEQ.txt").unwrap();
        let first = futures::executor::block_on(client.load_equalizer(&entry)).unwrap();
        let second = futures::executor::block_on(client.load_equalizer(&entry)).unwrap();
        assert_eq!(first.preamp_mb, -420);
        assert_eq!(first, second);
        assert_eq!(http.uris.lock().unwrap().len(), 1);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn parses_name_index_rigs_and_ignores_sentinel_rows() {
        let bytes = b"x\ty\tname\tz\trig\n1\t2\tModel A\t4\tB&K 5128\n1\t2\tModel B\t4\tignore\n";
        let rigs = parse_name_index(bytes).unwrap();
        assert_eq!(rigs.get("Model A").map(String::as_str), Some("B&K 5128"));
        assert!(!rigs.contains_key("Model B"));
    }

    #[test]
    #[ignore = "requires live GitHub AutoEQ access"]
    fn live_github_index_and_parametric_profile_smoke_test() {
        let root = temporary_directory("live");
        let mut settings = AppSettings::for_current_user(512 * 1024 * 1024).unwrap();
        settings.cache_root = root.clone();
        let client = AutoEqClient::with_settings(&settings).unwrap();
        let index = futures::executor::block_on(client.build_index()).unwrap();
        assert!(!index.truncated);
        assert!(index.entries.len() > 1_000);
        let model = search_auto_eq_models(&index, "7Hz Salnotes Zero", 1)
            .into_iter()
            .next()
            .unwrap();
        let equalizer =
            futures::executor::block_on(client.load_equalizer(&model.variants[0])).unwrap();
        equalizer.validate().unwrap();
        fs::remove_dir_all(root).unwrap();
    }
}
