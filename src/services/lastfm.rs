use std::{collections::BTreeMap, fmt, sync::Arc, time::Duration};

use futures::AsyncReadExt as _;
use http_client::{AsyncBody, HttpClient, HttpRequestExt as _, Method, Request, http::StatusCode};
use keyring::v1::{Entry, Error as KeyringError};
use md5::{Digest as _, Md5};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use zeroize::{Zeroize as _, Zeroizing};

use crate::{AppError, ProxySettings, Result, domain::Song, services::build_http_client};

const API_ROOT: &str = "https://ws.audioscrobbler.com/2.0/";
const USER_AGENT: &str = concat!("Metrolist-rs/", env!("CARGO_PKG_VERSION"));
const KEYRING_SERVICE: &str = "io.metrolist.desktop";
const KEYRING_ACCOUNT: &str = "lastfm-session";
const MAX_CREDENTIAL_BYTES: usize = 1_024;
const MAX_ATTEMPTS: usize = 3;

#[derive(Clone, PartialEq, Eq)]
pub struct LastFmApiCredentials {
    api_key: String,
    shared_secret: String,
}

impl LastFmApiCredentials {
    pub fn new(api_key: impl Into<String>, shared_secret: impl Into<String>) -> Result<Self> {
        let api_key = api_key.into();
        let shared_secret = shared_secret.into();
        validate_secret("Last.fm API key", &api_key)?;
        validate_secret("Last.fm shared secret", &shared_secret)?;
        Ok(Self {
            api_key,
            shared_secret,
        })
    }

    pub fn from_environment() -> Result<Option<Self>> {
        let api_key = std::env::var("LASTFM_API_KEY").ok();
        let shared_secret = std::env::var("LASTFM_SHARED_SECRET")
            .or_else(|_| std::env::var("LASTFM_SECRET"))
            .ok();
        match (api_key, shared_secret) {
            (None, None) => Ok(None),
            (Some(api_key), Some(shared_secret)) => Self::new(api_key, shared_secret).map(Some),
            _ => Err(AppError::InvalidConfig(
                "LASTFM_API_KEY and LASTFM_SHARED_SECRET (or LASTFM_SECRET) must be configured together"
                    .into(),
            )),
        }
    }
}

impl fmt::Debug for LastFmApiCredentials {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("LastFmApiCredentials")
            .field("api_key", &"[REDACTED]")
            .field("shared_secret", &"[REDACTED]")
            .finish()
    }
}

impl Drop for LastFmApiCredentials {
    fn drop(&mut self) {
        self.api_key.zeroize();
        self.shared_secret.zeroize();
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct LastFmSession {
    username: String,
    session_key: String,
}

impl LastFmSession {
    pub fn new(username: impl Into<String>, session_key: impl Into<String>) -> Result<Self> {
        let username = username.into();
        let session_key = session_key.into();
        validate_public_text("Last.fm username", &username)?;
        validate_secret("Last.fm session key", &session_key)?;
        Ok(Self {
            username,
            session_key,
        })
    }

    pub fn username(&self) -> &str {
        &self.username
    }

    fn session_key(&self) -> &str {
        &self.session_key
    }

    fn encode_for_storage(&self) -> Result<Zeroizing<String>> {
        serde_json::to_string(&StoredLastFmSession {
            username: &self.username,
            session_key: &self.session_key,
        })
        .map(Zeroizing::new)
        .map_err(|_| AppError::Credential("the Last.fm session could not be encoded".into()))
    }

    fn decode_from_storage(value: &str) -> Result<Self> {
        let stored: OwnedLastFmSession = serde_json::from_str(value)
            .map_err(|_| AppError::Credential("the stored Last.fm session is malformed".into()))?;
        Self::new(stored.username, stored.session_key)
    }
}

impl fmt::Debug for LastFmSession {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("LastFmSession")
            .field("username", &self.username)
            .field("session_key", &"[REDACTED]")
            .finish()
    }
}

impl Drop for LastFmSession {
    fn drop(&mut self) {
        self.session_key.zeroize();
    }
}

#[derive(Serialize)]
struct StoredLastFmSession<'a> {
    username: &'a str,
    session_key: &'a str,
}

#[derive(Deserialize)]
struct OwnedLastFmSession {
    username: String,
    session_key: String,
}

pub trait LastFmCredentialStore: Send + Sync {
    fn load(&self) -> Result<Option<LastFmSession>>;
    fn save(&self, session: &LastFmSession) -> Result<()>;
    fn delete(&self) -> Result<()>;
    fn backend_label(&self) -> &'static str;
}

#[derive(Debug)]
pub struct LastFmSystemCredentialStore {
    service: String,
    account: String,
}

impl Default for LastFmSystemCredentialStore {
    fn default() -> Self {
        Self {
            service: KEYRING_SERVICE.into(),
            account: KEYRING_ACCOUNT.into(),
        }
    }
}

impl LastFmSystemCredentialStore {
    fn entry(&self) -> Result<Entry> {
        Entry::new(&self.service, &self.account)
            .map_err(|error| lastfm_credential_store_error("open", error))
    }
}

impl LastFmCredentialStore for LastFmSystemCredentialStore {
    fn load(&self) -> Result<Option<LastFmSession>> {
        let value = match self.entry()?.get_password() {
            Ok(value) => Zeroizing::new(value),
            Err(KeyringError::NoEntry) => return Ok(None),
            Err(error) => return Err(lastfm_credential_store_error("read", error)),
        };
        LastFmSession::decode_from_storage(&value).map(Some)
    }

    fn save(&self, session: &LastFmSession) -> Result<()> {
        let value = session.encode_for_storage()?;
        self.entry()?
            .set_password(&value)
            .map_err(|error| lastfm_credential_store_error("save", error))
    }

    fn delete(&self) -> Result<()> {
        match self.entry()?.delete_credential() {
            Ok(()) | Err(KeyringError::NoEntry) => Ok(()),
            Err(error) => Err(lastfm_credential_store_error("delete", error)),
        }
    }

    fn backend_label(&self) -> &'static str {
        if cfg!(target_os = "macos") {
            "macOS Keychain"
        } else if cfg!(target_os = "windows") {
            "Windows Credential Manager"
        } else {
            "Secret Service"
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LastFmScrobblePolicy {
    pub min_track_seconds: u16,
    pub delay_percent_milli: u16,
    pub max_delay_seconds: u16,
}

impl Default for LastFmScrobblePolicy {
    fn default() -> Self {
        Self {
            min_track_seconds: 30,
            delay_percent_milli: 500,
            max_delay_seconds: 180,
        }
    }
}

impl LastFmScrobblePolicy {
    pub fn validate(self) -> Result<Self> {
        if !(10..=60).contains(&self.min_track_seconds)
            || !(300..=950).contains(&self.delay_percent_milli)
            || !(30..=360).contains(&self.max_delay_seconds)
        {
            return Err(AppError::InvalidConfig(
                "Last.fm scrobble settings are outside the Android-supported ranges".into(),
            ));
        }
        Ok(self)
    }

    pub fn threshold(self, duration: Duration) -> Option<Duration> {
        let duration_seconds = duration.as_secs();
        if duration_seconds <= u64::from(self.min_track_seconds) {
            return None;
        }
        let percent_millis = duration
            .as_millis()
            .saturating_mul(u128::from(self.delay_percent_milli))
            / 1_000;
        let percent = Duration::from_millis(percent_millis.min(u128::from(u64::MAX)) as u64);
        Some(percent.min(Duration::from_secs(u64::from(self.max_delay_seconds))))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LastFmTrack {
    pub artist: String,
    pub title: String,
    pub duration: Option<Duration>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LastFmPlaybackAction {
    UpdateNowPlaying(LastFmTrack),
    Scrobble { track: LastFmTrack, started_at: u64 },
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct LastFmPlaybackTracker {
    video_id: Option<String>,
    started_at: Option<u64>,
    now_playing_attempted: bool,
    scrobble_attempted: bool,
}

#[derive(Debug, Clone, Copy)]
pub struct LastFmPlaybackObservation<'a> {
    pub song: &'a Song,
    pub duration: Option<Duration>,
    pub is_playing: bool,
    pub played: Duration,
    pub unix_seconds: u64,
    pub now_playing_enabled: bool,
    pub scrobbling_enabled: bool,
    pub policy: LastFmScrobblePolicy,
}

impl LastFmPlaybackTracker {
    pub fn reset(&mut self) {
        *self = Self::default();
    }

    pub fn observe(
        &mut self,
        observation: LastFmPlaybackObservation<'_>,
    ) -> Result<Vec<LastFmPlaybackAction>> {
        let LastFmPlaybackObservation {
            song,
            duration,
            is_playing,
            played,
            unix_seconds,
            now_playing_enabled,
            scrobbling_enabled,
            policy,
        } = observation;
        if self.video_id.as_deref() != Some(&song.video_id) {
            self.reset();
            self.video_id = Some(song.video_id.clone());
        }
        if !is_playing {
            return Ok(Vec::new());
        }
        if self.started_at.is_none() && unix_seconds > 0 {
            self.started_at = Some(unix_seconds);
        }

        let mut track = LastFmTrack::from_song(song)?;
        track.duration = duration.or(track.duration);
        let mut actions = Vec::with_capacity(2);
        if now_playing_enabled && !self.now_playing_attempted {
            self.now_playing_attempted = true;
            actions.push(LastFmPlaybackAction::UpdateNowPlaying(track.clone()));
        }
        if scrobbling_enabled
            && !self.scrobble_attempted
            && policy
                .validate()?
                .threshold(track.duration.unwrap_or_default())
                .is_some_and(|threshold| played >= threshold)
            && let Some(started_at) = self.started_at
        {
            self.scrobble_attempted = true;
            actions.push(LastFmPlaybackAction::Scrobble { track, started_at });
        }
        Ok(actions)
    }
}

impl LastFmTrack {
    pub fn from_song(song: &Song) -> Result<Self> {
        if song.is_episode {
            return Err(AppError::Protocol(
                "podcast episodes are not Last.fm music tracks".into(),
            ));
        }
        let track = Self {
            artist: song.artist_line(),
            title: song.title.clone(),
            duration: song.duration,
        };
        track.validate()
    }

    fn validate(self) -> Result<Self> {
        validate_public_text("Last.fm artist", &self.artist)?;
        validate_public_text("Last.fm track title", &self.title)?;
        Ok(self)
    }
}

#[derive(Clone)]
pub struct LastFmClient {
    http: Arc<dyn HttpClient>,
    credentials: LastFmApiCredentials,
    session: Option<LastFmSession>,
}

impl fmt::Debug for LastFmClient {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("LastFmClient")
            .field("credentials", &self.credentials)
            .field("session", &self.session)
            .finish_non_exhaustive()
    }
}

impl LastFmClient {
    pub fn with_proxy(
        credentials: LastFmApiCredentials,
        session: Option<LastFmSession>,
        proxy: &ProxySettings,
    ) -> Result<Self> {
        let http = build_http_client(proxy, USER_AGENT)?;
        Ok(Self::new(http, credentials, session))
    }

    pub fn new(
        http: Arc<dyn HttpClient>,
        credentials: LastFmApiCredentials,
        session: Option<LastFmSession>,
    ) -> Self {
        Self {
            http,
            credentials,
            session,
        }
    }

    pub fn with_session(&self, session: Option<LastFmSession>) -> Self {
        Self {
            http: self.http.clone(),
            credentials: self.credentials.clone(),
            session,
        }
    }

    pub async fn login(&self, username: &str, password: &str) -> Result<LastFmSession> {
        validate_public_text("Last.fm username", username)?;
        validate_secret("Last.fm password", password)?;
        let response = self
            .send_call(
                "auth.getMobileSession",
                None,
                BTreeMap::from([
                    ("password".into(), password.into()),
                    ("username".into(), username.into()),
                ]),
            )
            .await?;
        let session = response
            .get("session")
            .ok_or_else(|| AppError::Protocol("Last.fm login returned no session".into()))?;
        let name = session
            .get("name")
            .and_then(Value::as_str)
            .ok_or_else(|| AppError::Protocol("Last.fm login returned no username".into()))?;
        let key = session
            .get("key")
            .and_then(Value::as_str)
            .ok_or_else(|| AppError::Protocol("Last.fm login returned no session key".into()))?;
        LastFmSession::new(name, key)
    }

    pub async fn update_now_playing(&self, track: &LastFmTrack) -> Result<()> {
        let track = track.clone().validate()?;
        self.send_call(
            "track.updateNowPlaying",
            Some(self.require_session()?),
            track_params(&track, false, None),
        )
        .await
        .map(|_| ())
    }

    pub async fn scrobble(&self, track: &LastFmTrack, started_at: u64) -> Result<()> {
        if started_at == 0 {
            return Err(AppError::Protocol(
                "Last.fm scrobble timestamp must be a positive Unix time".into(),
            ));
        }
        let track = track.clone().validate()?;
        let response = self
            .send_call(
                "track.scrobble",
                Some(self.require_session()?),
                track_params(&track, true, Some(started_at)),
            )
            .await?;
        let accepted = response
            .pointer("/scrobbles/@attr/accepted")
            .and_then(json_u64)
            .unwrap_or_default();
        if accepted == 1 {
            Ok(())
        } else {
            Err(AppError::Protocol(
                "Last.fm did not accept the scrobble".into(),
            ))
        }
    }

    pub async fn set_love_status(&self, track: &LastFmTrack, loved: bool) -> Result<()> {
        let track = track.clone().validate()?;
        self.send_call(
            if loved { "track.love" } else { "track.unlove" },
            Some(self.require_session()?),
            BTreeMap::from([
                ("artist".into(), track.artist),
                ("track".into(), track.title),
            ]),
        )
        .await
        .map(|_| ())
    }

    fn require_session(&self) -> Result<&LastFmSession> {
        self.session.as_ref().ok_or_else(|| {
            AppError::Credential("a Last.fm session is required for this operation".into())
        })
    }

    async fn send_call(
        &self,
        method: &str,
        session: Option<&LastFmSession>,
        extra: BTreeMap<String, String>,
    ) -> Result<Value> {
        let mut signed = BTreeMap::from([
            ("api_key".into(), self.credentials.api_key.clone()),
            ("method".into(), method.into()),
        ]);
        if let Some(session) = session {
            signed.insert("sk".into(), session.session_key().into());
        }
        signed.extend(extra);
        let signature = api_signature(&signed, &self.credentials.shared_secret);
        let mut wire = signed;
        wire.insert("api_sig".into(), signature);
        wire.insert("format".into(), "json".into());
        let body = Zeroizing::new(
            serde_urlencoded::to_string(&wire)
                .map_err(|_| AppError::Protocol("Last.fm request could not be encoded".into()))?,
        );
        for value in wire.values_mut() {
            value.zeroize();
        }

        let mut last_error = None;
        for attempt in 0..MAX_ATTEMPTS {
            let request = Request::builder()
                .method(Method::POST)
                .uri(API_ROOT)
                .header("Accept", "application/json")
                .header(
                    "Content-Type",
                    "application/x-www-form-urlencoded; charset=utf-8",
                )
                .header("User-Agent", USER_AGENT)
                .timeout(Duration::from_secs(30))
                .body(AsyncBody::from(body.as_bytes().to_vec()))
                .map_err(|_| AppError::Protocol("Last.fm request could not be built".into()))?;
            let mut response = match self.http.send(request).await {
                Ok(response) => response,
                Err(_) => {
                    let error = AppError::Network("Last.fm request failed".into());
                    if attempt + 1 < MAX_ATTEMPTS {
                        last_error = Some(error);
                        continue;
                    }
                    return Err(error);
                }
            };
            let status = response.status();
            let mut bytes = Vec::new();
            response
                .body_mut()
                .read_to_end(&mut bytes)
                .await
                .map_err(|_| AppError::Network("Last.fm response could not be read".into()))?;
            let parsed = serde_json::from_slice::<Value>(&bytes).ok();
            if let Some(error) = parsed.as_ref().and_then(lastfm_api_error) {
                if attempt + 1 < MAX_ATTEMPTS && error.retryable() {
                    last_error = Some(error.into_app_error());
                    continue;
                }
                return Err(error.into_app_error());
            }
            if !status.is_success() {
                let error = AppError::Network(format!("Last.fm returned HTTP {status}"));
                if attempt + 1 < MAX_ATTEMPTS && retryable_status(status) {
                    last_error = Some(error);
                    continue;
                }
                return Err(error);
            }
            return parsed.ok_or_else(|| {
                AppError::Protocol("Last.fm returned an unreadable response".into())
            });
        }
        Err(last_error.unwrap_or_else(|| AppError::Network("Last.fm retries exhausted".into())))
    }
}

fn track_params(
    track: &LastFmTrack,
    indexed: bool,
    started_at: Option<u64>,
) -> BTreeMap<String, String> {
    let suffix = if indexed { "[0]" } else { "" };
    let mut params = BTreeMap::from([
        (format!("artist{suffix}"), track.artist.clone()),
        (format!("track{suffix}"), track.title.clone()),
    ]);
    if let Some(duration) = track.duration.map(|duration| duration.as_secs()) {
        params.insert(format!("duration{suffix}"), duration.to_string());
    }
    if let Some(started_at) = started_at {
        params.insert(format!("timestamp{suffix}"), started_at.to_string());
    }
    params
}

fn api_signature(params: &BTreeMap<String, String>, shared_secret: &str) -> String {
    let mut input = Zeroizing::new(String::new());
    for (name, value) in params {
        input.push_str(name);
        input.push_str(value);
    }
    input.push_str(shared_secret);
    format!("{:x}", Md5::digest(input.as_bytes()))
}

#[derive(Debug)]
struct LastFmApiError {
    code: i64,
}

impl LastFmApiError {
    fn retryable(&self) -> bool {
        matches!(self.code, 11 | 16 | 29)
    }

    fn into_app_error(self) -> AppError {
        match self.code {
            4 => AppError::Credential("Last.fm authentication failed".into()),
            9 => AppError::LastFmSessionExpired(
                "the stored session key was rejected; sign in again".into(),
            ),
            10 | 13 | 26 => AppError::InvalidConfig(format!(
                "Last.fm rejected the application credentials (error {})",
                self.code
            )),
            _ => AppError::Protocol(format!("Last.fm returned API error {}", self.code)),
        }
    }
}

fn lastfm_api_error(value: &Value) -> Option<LastFmApiError> {
    value
        .get("error")
        .and_then(json_i64)
        .map(|code| LastFmApiError { code })
}

fn json_u64(value: &Value) -> Option<u64> {
    value
        .as_u64()
        .or_else(|| value.as_str().and_then(|value| value.parse().ok()))
}

fn json_i64(value: &Value) -> Option<i64> {
    value
        .as_i64()
        .or_else(|| value.as_str().and_then(|value| value.parse().ok()))
}

fn retryable_status(status: StatusCode) -> bool {
    status.as_u16() == 429 || status.is_server_error()
}

fn validate_public_text(label: &str, value: &str) -> Result<()> {
    let value = value.trim();
    if value.is_empty() || value.len() > MAX_CREDENTIAL_BYTES || value.chars().any(char::is_control)
    {
        return Err(AppError::Credential(format!("{label} is invalid")));
    }
    Ok(())
}

fn validate_secret(label: &str, value: &str) -> Result<()> {
    if value.is_empty() || value.len() > MAX_CREDENTIAL_BYTES || value.chars().any(char::is_control)
    {
        return Err(AppError::Credential(format!("{label} is invalid")));
    }
    Ok(())
}

fn lastfm_credential_store_error(operation: &str, error: KeyringError) -> AppError {
    AppError::CredentialStore(format!(
        "could not {operation} the Last.fm session: {error}"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        Mutex,
        atomic::{AtomicUsize, Ordering},
    };

    use futures::future::BoxFuture;
    use http_client::{Response, Url, http::HeaderValue};

    #[test]
    fn policy_matches_android_defaults_pause_safe_threshold_math_and_bounds() {
        let policy = LastFmScrobblePolicy::default();
        assert_eq!(policy.threshold(Duration::from_secs(30)), None);
        assert_eq!(
            policy.threshold(Duration::from_secs(200)),
            Some(Duration::from_secs(100))
        );
        assert_eq!(
            policy.threshold(Duration::from_secs(600)),
            Some(Duration::from_secs(180))
        );
        assert!(policy.validate().is_ok());
        assert!(
            LastFmScrobblePolicy {
                delay_percent_milli: 299,
                ..policy
            }
            .validate()
            .is_err()
        );
    }

    #[test]
    fn sessions_round_trip_and_debug_output_redacts_every_secret() {
        let session = LastFmSession::new("fixture-user", "fixture-session-secret").unwrap();
        let stored = session.encode_for_storage().unwrap();
        let restored = LastFmSession::decode_from_storage(&stored).unwrap();
        assert_eq!(session, restored);
        assert!(!format!("{session:?}").contains("fixture-session-secret"));

        let credentials =
            LastFmApiCredentials::new("fixture-key", "fixture-shared-secret").unwrap();
        let debug = format!("{credentials:?}");
        assert!(!debug.contains("fixture-key"));
        assert!(!debug.contains("fixture-shared-secret"));
    }

    #[test]
    fn api_signature_sorts_ascii_parameter_names_and_excludes_format() {
        let params = BTreeMap::from([
            ("track[0]".into(), "Track".into()),
            ("method".into(), "track.scrobble".into()),
            ("artist[0]".into(), "Artist".into()),
            ("api_key".into(), "key".into()),
            ("timestamp[0]".into(), "1700000000".into()),
        ]);
        assert_eq!(
            api_signature(&params, "secret"),
            "aa1492862fe7a995bb2ad672185bb81d"
        );
    }

    struct FixtureHttpClient {
        responses: Mutex<Vec<(StatusCode, Vec<u8>)>>,
        requests: Arc<Mutex<Vec<BTreeMap<String, String>>>>,
        attempts: AtomicUsize,
    }

    impl FixtureHttpClient {
        fn new(responses: Vec<(StatusCode, &'static [u8])>) -> Arc<Self> {
            Arc::new(Self {
                responses: Mutex::new(
                    responses
                        .into_iter()
                        .rev()
                        .map(|(status, body)| (status, body.to_vec()))
                        .collect(),
                ),
                requests: Arc::new(Mutex::new(Vec::new())),
                attempts: AtomicUsize::new(0),
            })
        }
    }

    impl HttpClient for FixtureHttpClient {
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
            assert_eq!(request.method(), Method::POST);
            assert_eq!(request.uri().to_string(), API_ROOT);
            self.attempts.fetch_add(1, Ordering::Relaxed);
            let response = self.responses.lock().unwrap().pop().unwrap();
            let requests = self.requests.clone();
            let (_, mut body) = request.into_parts();
            Box::pin(async move {
                let mut bytes = Vec::new();
                body.read_to_end(&mut bytes).await?;
                let params = serde_urlencoded::from_bytes(&bytes).unwrap();
                requests.lock().unwrap().push(params);
                Response::builder()
                    .status(response.0)
                    .body(AsyncBody::from(response.1))
                    .map_err(Into::into)
            })
        }
    }

    fn client(http: Arc<dyn HttpClient>, session: Option<LastFmSession>) -> LastFmClient {
        LastFmClient::new(
            http,
            LastFmApiCredentials::new("fixture-key", "fixture-secret").unwrap(),
            session,
        )
    }

    #[test]
    fn mobile_login_posts_signed_form_and_returns_a_redacted_session() {
        let http = FixtureHttpClient::new(vec![(
            StatusCode::OK,
            br#"{"session":{"name":"fixture-user","key":"fixture-session","subscriber":0}}"#,
        )]);
        let session = futures::executor::block_on(
            client(http.clone(), None).login("fixture-user", "fixture-password"),
        )
        .unwrap();
        assert_eq!(session.username(), "fixture-user");
        let requests = http.requests.lock().unwrap();
        assert_eq!(requests[0]["method"], "auth.getMobileSession");
        assert_eq!(requests[0]["username"], "fixture-user");
        assert_eq!(requests[0]["password"], "fixture-password");
        assert!(requests[0].contains_key("api_sig"));
        assert_eq!(requests[0]["format"], "json");
    }

    #[test]
    fn scrobble_retries_transient_errors_and_rejects_expired_sessions() {
        let track = LastFmTrack {
            artist: "Fixture Artist".into(),
            title: "Fixture Track".into(),
            duration: Some(Duration::from_secs(240)),
        };
        let session = LastFmSession::new("fixture-user", "fixture-session").unwrap();
        let retrying = FixtureHttpClient::new(vec![
            (StatusCode::OK, br#"{"error":16,"message":"temporary"}"#),
            (StatusCode::OK, br#"{"error":16,"message":"temporary"}"#),
            (
                StatusCode::OK,
                br#"{"scrobbles":{"@attr":{"accepted":1,"ignored":0}}}"#,
            ),
        ]);
        futures::executor::block_on(
            client(retrying.clone(), Some(session.clone())).scrobble(&track, 1_700_000_000),
        )
        .unwrap();
        assert_eq!(retrying.attempts.load(Ordering::Relaxed), 3);
        let request = &retrying.requests.lock().unwrap()[0];
        assert_eq!(request["artist[0]"], "Fixture Artist");
        assert_eq!(request["timestamp[0]"], "1700000000");
        assert_eq!(request["sk"], "fixture-session");

        let expired = FixtureHttpClient::new(vec![(
            StatusCode::OK,
            br#"{"error":9,"message":"Invalid session key"}"#,
        )]);
        let error =
            futures::executor::block_on(client(expired, Some(session)).update_now_playing(&track))
                .unwrap_err();
        assert!(matches!(error, AppError::LastFmSessionExpired(_)));

        let http_failure = FixtureHttpClient::new(vec![
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                b"temporary upstream failure",
            ),
            (StatusCode::OK, br#"{}"#),
        ]);
        futures::executor::block_on(
            client(
                http_failure.clone(),
                Some(LastFmSession::new("fixture-user", "fixture-session").unwrap()),
            )
            .update_now_playing(&track),
        )
        .unwrap();
        assert_eq!(http_failure.attempts.load(Ordering::Relaxed), 2);
    }

    #[test]
    fn playback_tracker_counts_only_supplied_play_time_and_emits_each_action_once() {
        let song = Song {
            video_id: "fixture-video".into(),
            title: "Fixture Track".into(),
            artists: vec![crate::domain::ArtistCredit {
                name: "Fixture Artist".into(),
                id: None,
            }],
            duration: Some(Duration::from_secs(200)),
            thumbnail_url: None,
            album: None,
            is_episode: false,
            explicit: false,
            music_video_type: None,
        };
        let mut tracker = LastFmPlaybackTracker::default();

        assert!(
            tracker
                .observe(LastFmPlaybackObservation {
                    song: &song,
                    duration: song.duration,
                    is_playing: false,
                    played: Duration::from_secs(150),
                    unix_seconds: 1_700_000_000,
                    now_playing_enabled: true,
                    scrobbling_enabled: true,
                    policy: LastFmScrobblePolicy::default(),
                })
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            tracker
                .observe(LastFmPlaybackObservation {
                    song: &song,
                    duration: song.duration,
                    is_playing: true,
                    played: Duration::from_secs(99),
                    unix_seconds: 1_700_000_010,
                    now_playing_enabled: true,
                    scrobbling_enabled: true,
                    policy: LastFmScrobblePolicy::default(),
                })
                .unwrap(),
            vec![LastFmPlaybackAction::UpdateNowPlaying(
                LastFmTrack::from_song(&song).unwrap()
            )]
        );
        let actions = tracker
            .observe(LastFmPlaybackObservation {
                song: &song,
                duration: song.duration,
                is_playing: true,
                played: Duration::from_secs(100),
                unix_seconds: 1_700_000_011,
                now_playing_enabled: true,
                scrobbling_enabled: true,
                policy: LastFmScrobblePolicy::default(),
            })
            .unwrap();
        assert!(matches!(
            actions.as_slice(),
            [LastFmPlaybackAction::Scrobble {
                started_at: 1_700_000_010,
                ..
            }]
        ));
        assert!(
            tracker
                .observe(LastFmPlaybackObservation {
                    song: &song,
                    duration: song.duration,
                    is_playing: true,
                    played: Duration::from_secs(199),
                    unix_seconds: 1_700_000_100,
                    now_playing_enabled: true,
                    scrobbling_enabled: true,
                    policy: LastFmScrobblePolicy::default(),
                })
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn podcast_episodes_never_become_lastfm_tracks() {
        let mut episode = Song {
            video_id: "episode-video".into(),
            title: "Fixture Episode".into(),
            artists: vec![crate::domain::ArtistCredit {
                name: "Fixture Podcast".into(),
                id: None,
            }],
            duration: Some(Duration::from_secs(2_400)),
            thumbnail_url: None,
            album: None,
            is_episode: true,
            explicit: false,
            music_video_type: None,
        };
        assert!(LastFmTrack::from_song(&episode).is_err());

        episode.is_episode = false;
        assert!(LastFmTrack::from_song(&episode).is_ok());
    }
}
