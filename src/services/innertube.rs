use std::collections::HashSet;
use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use futures::AsyncReadExt as _;
use http_client::{
    AsyncBody, HttpClient, HttpRequestExt as _, Method, Request, Url,
    http::{HeaderName, HeaderValue},
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::domain::{
    AlbumCredit, ArtistCredit, BrowseContinuation, BrowseItem, BrowseKind, BrowsePage,
    BrowsePlaybackEndpoint, ChannelSubscription, ExploreCategory, ExplorePage, HomeChip, HomeItem,
    HomePage, HomeSection, PlaylistEntry, RemoteHistoryEntry, RemoteHistoryPage,
    RemoteHistorySection, Song,
};
use crate::services::auth::AuthSession;
use crate::services::build_http_client;
#[cfg(test)]
use crate::services::{
    AudioDeviceOperation, AudioPlayer, DesktopAudioPlayer, PlaybackState, probe_audio_source,
};
use crate::services::{HttpRangeMediaSource, PlaybackSource, PlaybackSourceAccess};
use crate::{AppError, AppSettings, AudioQuality, ProxySettings, Result};

const API_ROOT: &str = "https://music.youtube.com/youtubei/v1";
const ORIGIN: &str = "https://music.youtube.com";
const USER_AGENT: &str =
    "Mozilla/5.0 (Windows NT 10.0; Win64; x64; rv:140.0) Gecko/20100101 Firefox/140.0";
const CLIENT_NAME: &str = "WEB_REMIX";
const CLIENT_ID: &str = "67";
const CLIENT_VERSION: &str = "1.20260114.03.00";
const WEB_CLIENT_NAME: &str = "WEB";
const WEB_CLIENT_ID: &str = "1";
const WEB_CLIENT_VERSION: &str = "2.20260114.08.00";
const RETURN_YOUTUBE_DISLIKE_API: &str = "https://returnyoutubedislikeapi.com/Votes";
const SONG_FILTER: &str = "EgWKAQIIAWoKEAkQBRAKEAMQBA%3D%3D";
const VIDEO_FILTER: &str = "EgWKAQIQAWoKEAkQChAFEAMQBA%3D%3D";
const ALBUM_FILTER: &str = "EgWKAQIYAWoKEAkQChAFEAMQBA%3D%3D";
const ARTIST_FILTER: &str = "EgWKAQIgAWoKEAkQChAFEAMQBA%3D%3D";
const PLAYLIST_FILTER: &str = "EgeKAQQoAEABagoQAxAEEAoQCRAF";
const FEATURED_PLAYLIST_FILTER: &str = "EgeKAQQoADgBagwQDhAKEAMQBRAJEAQ%3D";
const PODCAST_FILTER: &str = "EgWKAQJQAWoKEAkQChAFEAMQBA%3D%3D";
const EPISODE_FILTER: &str = "EgWKAQJYAWoKEAkQChAFEAMQBA%3D%3D";
const PROFILE_FILTER: &str = "EgWKAQJYAWoSEAUQCRADEAQQEBAVEAoQDhAR";
const CHARTS_PARAMS: &str = "ggMGCgQIgAQ%3D";
const PLAYBACK_CLIENT_NAME: &str = "VISIONOS";
const PLAYBACK_CLIENT_ID: &str = "101";
const PLAYBACK_CLIENT_VERSION: &str = "0.1";
const PLAYBACK_USER_AGENT: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/18.0 Safari/605.1.15";
const RADIO_MAX_ATTEMPTS: usize = 4;
const ACCOUNT_MAX_ATTEMPTS: usize = 3;
const IDEMPOTENT_MUTATION_MAX_ATTEMPTS: usize = 3;
const HISTORY_CONTINUATION_LIMIT: usize = 64;
const PLAYBACK_TRACKING_VERSION: &str = "2";
const CPN_ALPHABET: &[u8; 64] = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789-_";

#[derive(Clone, PartialEq, Eq)]
pub struct ResolvedPlayback {
    pub source: PlaybackSource,
    pub expires_in: Duration,
    pub(crate) playback_tracking: Option<PlaybackTrackingUrl>,
}

impl ResolvedPlayback {
    pub fn playback_tracking(&self) -> Option<&PlaybackTrackingUrl> {
        self.playback_tracking.as_ref()
    }
}

impl fmt::Debug for ResolvedPlayback {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ResolvedPlayback")
            .field("source", &self.source)
            .field("expires_in", &self.expires_in)
            .field("has_playback_tracking", &self.playback_tracking.is_some())
            .finish()
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct PlaybackTrackingUrl(String);

impl fmt::Debug for PlaybackTrackingUrl {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("PlaybackTrackingUrl([redacted])")
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchResult {
    pub songs: Vec<Song>,
    pub items: Vec<BrowseItem>,
    pub continuation: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchSuggestions {
    pub queries: Vec<String>,
    pub songs: Vec<Song>,
    pub items: Vec<BrowseItem>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemotePodcastLibrary {
    pub podcasts: Vec<BrowseItem>,
    pub episodes: Vec<RemotePodcastEpisode>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemotePodcastEpisode {
    pub song: Song,
    pub set_video_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountProfile {
    pub name: String,
    pub email: Option<String>,
    pub channel_handle: Option<String>,
    pub thumbnail_url: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MediaInfo {
    pub video_id: String,
    pub title: Option<String>,
    pub author: Option<String>,
    pub author_id: Option<String>,
    pub description: Option<String>,
    pub upload_date: Option<String>,
    pub subscribers: Option<String>,
    pub view_count: Option<u64>,
    pub likes: Option<u64>,
    pub dislikes: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RadioEndpoint {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub video_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub playlist_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub playlist_set_video_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub params: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub index: Option<u32>,
}

impl RadioEndpoint {
    pub fn song_radio(video_id: &str) -> Result<Self> {
        let video_id = video_id.trim();
        if video_id.is_empty() {
            return Err(AppError::Protocol(
                "radio seed video id cannot be empty".into(),
            ));
        }
        Ok(Self {
            video_id: Some(video_id.to_owned()),
            playlist_id: Some(format!("RDAMVM{video_id}")),
            playlist_set_video_id: None,
            params: None,
            index: None,
        })
    }

    fn song(video_id: &str) -> Self {
        Self {
            video_id: Some(video_id.to_owned()),
            playlist_id: None,
            playlist_set_video_id: None,
            params: None,
            index: None,
        }
    }
}

impl From<BrowsePlaybackEndpoint> for RadioEndpoint {
    fn from(endpoint: BrowsePlaybackEndpoint) -> Self {
        Self {
            video_id: endpoint.video_id,
            playlist_id: endpoint.playlist_id,
            playlist_set_video_id: endpoint.playlist_set_video_id,
            params: endpoint.params,
            index: endpoint.index,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RadioPage {
    pub title: Option<String>,
    pub songs: Vec<Song>,
    pub current_index: Option<usize>,
    pub continuation: Option<String>,
    pub endpoint: RadioEndpoint,
    related_endpoint: Option<RelatedEndpoint>,
}

impl RadioPage {
    pub fn append_unique(&mut self, next: Self) -> usize {
        let before = self.songs.len();
        for song in next.songs {
            if !self
                .songs
                .iter()
                .any(|existing| existing.video_id == song.video_id)
            {
                self.songs.push(song);
            }
        }
        self.continuation = next.continuation;
        self.endpoint = next.endpoint;
        self.related_endpoint = next.related_endpoint;
        self.songs.len() - before
    }

    fn has_recommendation_for(&self, seed_video_id: &str) -> bool {
        self.songs.iter().any(|song| song.video_id != seed_video_id)
    }

    pub fn recommendations_after_current(&self, seed_video_id: &str) -> Vec<Song> {
        let start = self
            .current_index
            .map_or(0, |index| index.saturating_add(1))
            .min(self.songs.len());
        self.songs[start..]
            .iter()
            .filter(|song| song.video_id != seed_video_id)
            .cloned()
            .collect()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RelatedEndpoint {
    browse_id: String,
    params: Option<String>,
}

struct ParsedRadioPage {
    page: RadioPage,
    automix_endpoint: Option<RadioEndpoint>,
}

impl SearchResult {
    pub fn append_continuation(&mut self, next: Self) -> usize {
        let previous_song_count = self.songs.len();
        let previous_item_count = self.items.len();
        for song in next.songs {
            if !self
                .songs
                .iter()
                .any(|existing| existing.video_id == song.video_id)
            {
                self.songs.push(song);
            }
        }
        for item in next.items {
            if !self
                .items
                .iter()
                .any(|existing| existing.browse_id == item.browse_id)
            {
                self.items.push(item);
            }
        }
        self.continuation = next.continuation;
        (self.songs.len() - previous_song_count) + (self.items.len() - previous_item_count)
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum SearchFilter {
    #[default]
    All,
    Songs,
    Videos,
    Albums,
    Artists,
    Playlists,
    FeaturedPlaylists,
    Podcasts,
    Episodes,
    Profiles,
}

impl SearchFilter {
    pub const ALL: [Self; 10] = [
        Self::All,
        Self::Songs,
        Self::Videos,
        Self::Albums,
        Self::Artists,
        Self::Playlists,
        Self::FeaturedPlaylists,
        Self::Podcasts,
        Self::Episodes,
        Self::Profiles,
    ];

    pub const fn label(self) -> &'static str {
        match self {
            Self::All => "All",
            Self::Songs => "Songs",
            Self::Videos => "Videos",
            Self::Albums => "Albums",
            Self::Artists => "Artists",
            Self::Playlists => "Community playlists",
            Self::FeaturedPlaylists => "Featured playlists",
            Self::Podcasts => "Podcasts",
            Self::Episodes => "Episodes",
            Self::Profiles => "Profiles",
        }
    }

    pub const fn returns_songs(self) -> bool {
        matches!(
            self,
            Self::All | Self::Songs | Self::Videos | Self::Episodes
        )
    }

    const fn params(self) -> Option<&'static str> {
        match self {
            Self::All => None,
            Self::Songs => Some(SONG_FILTER),
            Self::Videos => Some(VIDEO_FILTER),
            Self::Albums => Some(ALBUM_FILTER),
            Self::Artists => Some(ARTIST_FILTER),
            Self::Playlists => Some(PLAYLIST_FILTER),
            Self::FeaturedPlaylists => Some(FEATURED_PLAYLIST_FILTER),
            Self::Podcasts => Some(PODCAST_FILTER),
            Self::Episodes => Some(EPISODE_FILTER),
            Self::Profiles => Some(PROFILE_FILTER),
        }
    }
}

#[derive(Debug, Clone)]
pub struct InnerTubeSession {
    pub language: String,
    pub region: String,
    pub visitor_data: Option<String>,
    auth: Option<AuthSession>,
}

impl Default for InnerTubeSession {
    fn default() -> Self {
        Self {
            language: "en".into(),
            region: "US".into(),
            visitor_data: None,
            auth: None,
        }
    }
}

impl InnerTubeSession {
    pub fn with_auth(mut self, auth: Option<AuthSession>) -> Self {
        if self.visitor_data.is_none() {
            self.visitor_data = auth
                .as_ref()
                .and_then(AuthSession::visitor_data)
                .map(str::to_owned);
        }
        self.auth = auth;
        self
    }

    pub fn is_authenticated(&self) -> bool {
        self.auth.is_some()
    }
}

pub struct InnerTubeClient {
    http: Arc<dyn HttpClient>,
    session: InnerTubeSession,
    audio_quality: AudioQuality,
}

impl InnerTubeClient {
    pub fn anonymous(session: InnerTubeSession) -> Self {
        let http = build_http_client(&ProxySettings::default(), USER_AGENT)
            .expect("the built-in HTTP configuration must be valid");
        Self::new(session, http, AudioQuality::Auto)
    }

    pub fn with_settings(mut session: InnerTubeSession, settings: &AppSettings) -> Result<Self> {
        let http = build_http_client(&settings.proxy, USER_AGENT)?;
        session.language = settings.resolved_content_language();
        session.region = settings.resolved_content_country();
        Ok(Self::new(session, http, settings.audio_quality))
    }

    pub fn new(
        session: InnerTubeSession,
        http: Arc<dyn HttpClient>,
        audio_quality: AudioQuality,
    ) -> Self {
        Self {
            http,
            session,
            audio_quality,
        }
    }

    pub fn is_authenticated(&self) -> bool {
        self.session.is_authenticated()
    }

    fn apply_login_headers(
        &self,
        mut request: http_client::http::request::Builder,
        set_login: bool,
    ) -> Result<http_client::http::request::Builder> {
        let Some(auth) = self.session.auth.as_ref().filter(|_| set_login) else {
            return Ok(request);
        };
        request = request
            .header("Cookie", auth.cookie_header())
            .header("Authorization", auth.authorization_now()?)
            .header("Origin", ORIGIN)
            .header("X-Goog-AuthUser", "0");
        Ok(request)
    }

    pub async fn search_songs(&self, query: &str) -> Result<SearchResult> {
        self.search(query, SearchFilter::Songs).await
    }

    pub async fn search_suggestions(&self, input: &str) -> Result<SearchSuggestions> {
        let input = input.trim();
        if input.is_empty() {
            return Ok(SearchSuggestions {
                queries: Vec::new(),
                songs: Vec::new(),
                items: Vec::new(),
            });
        }
        if input.len() > 1_024 || input.chars().any(char::is_control) {
            return Err(AppError::Protocol(
                "search suggestion input must contain at most 1024 non-control bytes".into(),
            ));
        }

        let body = serde_json::to_vec(&SearchSuggestionsBody {
            context: RequestContext {
                client: ClientContext {
                    client_name: CLIENT_NAME,
                    client_version: CLIENT_VERSION,
                    language: &self.session.language,
                    region: &self.session.region,
                    visitor_data: self.session.visitor_data.as_deref(),
                },
                request: RequestMetadata { use_ssl: true },
                user: UserContext {
                    locked_safety_mode: false,
                    on_behalf_of_user: None,
                },
            },
            input,
        })
        .map_err(|error| AppError::Protocol(error.to_string()))?;

        // Android deliberately performs this request anonymously, even while an account is
        // active. Keep the same privacy boundary here by never applying login headers.
        let request = Request::builder()
            .method(Method::POST)
            .uri(format!(
                "{API_ROOT}/music/get_search_suggestions?prettyPrint=false"
            ))
            .header("Accept", "application/json")
            .header("Accept-Language", "en-US,en;q=0.9")
            .header("Cache-Control", "no-cache")
            .header("Content-Type", "application/json")
            .header("X-Goog-Api-Format-Version", "1")
            .header("X-YouTube-Client-Name", CLIENT_ID)
            .header("X-YouTube-Client-Version", CLIENT_VERSION)
            .header("X-Origin", ORIGIN)
            .header("Referer", format!("{ORIGIN}/"))
            .when_some(self.session.visitor_data.as_deref(), |request, visitor| {
                request.header("X-Goog-Visitor-Id", visitor)
            })
            .timeout(Duration::from_secs(30))
            .body(AsyncBody::from(body))
            .map_err(|error| AppError::Network(error.to_string()))?;

        let mut response = self
            .http
            .send(request)
            .await
            .map_err(|error| AppError::Network(error.to_string()))?;
        let status = response.status();
        let mut response_body = Vec::new();
        response
            .body_mut()
            .read_to_end(&mut response_body)
            .await
            .map_err(|error| AppError::Network(error.to_string()))?;
        if !status.is_success() {
            return Err(AppError::Network(format!(
                "InnerTube search suggestions returned HTTP {status}"
            )));
        }
        parse_search_suggestions_response(response_body)
    }

    pub async fn search(&self, query: &str, filter: SearchFilter) -> Result<SearchResult> {
        let query = query.trim();
        if query.is_empty() {
            return Ok(SearchResult {
                songs: Vec::new(),
                items: Vec::new(),
                continuation: None,
            });
        }

        let response_body = self.fetch_search_page(query, filter.params()).await?;
        if filter != SearchFilter::Episodes {
            return parse_search_response(response_body);
        }

        let episodes = parse_episode_search_response(&response_body)?;
        if !episodes.songs.is_empty() {
            return Ok(episodes);
        }

        // The service currently maps the Android episode-filter token to profiles for some
        // anonymous WEB_REMIX sessions. Unfiltered results still carry explicit podcast metadata,
        // so fall back once and return only renderer-verified episodes.
        let response_body = self.fetch_search_page(query, None).await?;
        parse_episode_search_response(response_body)
    }

    async fn fetch_search_page(&self, query: &str, params: Option<&str>) -> Result<Vec<u8>> {
        let body = serde_json::to_vec(&SearchBody {
            context: RequestContext {
                client: ClientContext {
                    client_name: CLIENT_NAME,
                    client_version: CLIENT_VERSION,
                    language: &self.session.language,
                    region: &self.session.region,
                    visitor_data: self.session.visitor_data.as_deref(),
                },
                request: RequestMetadata { use_ssl: true },
                user: UserContext {
                    locked_safety_mode: false,
                    on_behalf_of_user: None,
                },
            },
            query,
            params,
        })
        .map_err(|error| AppError::Protocol(error.to_string()))?;

        let request = Request::builder()
            .method(Method::POST)
            .uri(format!("{API_ROOT}/search?prettyPrint=false"))
            .header("Accept", "application/json")
            .header("Accept-Language", "en-US,en;q=0.9")
            .header("Cache-Control", "no-cache")
            .header("Content-Type", "application/json")
            .header("X-Goog-Api-Format-Version", "1")
            .header("X-YouTube-Client-Name", CLIENT_ID)
            .header("X-YouTube-Client-Version", CLIENT_VERSION)
            .header("X-Origin", ORIGIN)
            .header("Referer", format!("{ORIGIN}/"))
            .when_some(self.session.visitor_data.as_deref(), |request, visitor| {
                request.header("X-Goog-Visitor-Id", visitor)
            })
            .timeout(Duration::from_secs(60))
            .body(AsyncBody::from(body))
            .map_err(|error| AppError::Network(error.to_string()))?;

        let mut response = self
            .http
            .send(request)
            .await
            .map_err(|error| AppError::Network(error.to_string()))?;
        let status = response.status();
        let mut response_body = Vec::new();
        response
            .body_mut()
            .read_to_end(&mut response_body)
            .await
            .map_err(|error| AppError::Network(error.to_string()))?;

        if !status.is_success() {
            return Err(AppError::Network(format!(
                "InnerTube search returned HTTP {status}"
            )));
        }

        Ok(response_body)
    }

    pub async fn search_continuation(&self, continuation: &str) -> Result<SearchResult> {
        let response = self.fetch_continuation("search", continuation).await?;
        parse_search_continuation_response(response)
    }

    pub async fn browse(&self, item: &BrowseItem) -> Result<BrowsePage> {
        let browse_id = item.browse_id.trim();
        if browse_id.is_empty() {
            return Err(AppError::Protocol("browse id cannot be empty".into()));
        }
        let normalized_id = if item.kind == BrowseKind::Playlist && !browse_id.starts_with("VL") {
            format!("VL{browse_id}")
        } else {
            browse_id.to_owned()
        };
        let response_body = self
            .fetch_browse_page(&normalized_id, item.params.as_deref())
            .await?;
        parse_browse_response(&response_body, item.clone())
    }

    pub async fn browse_continuation(&self, continuation: &str) -> Result<BrowseContinuation> {
        let response = self.fetch_continuation("browse", continuation).await?;
        parse_browse_continuation_response(response)
    }

    pub async fn home(&self, params: Option<&str>) -> Result<HomePage> {
        let response = self.fetch_browse_page("FEmusic_home", params).await?;
        parse_home_response(response)
    }

    pub async fn home_continuation(&self, continuation: &str) -> Result<HomePage> {
        let response = self.fetch_continuation("browse", continuation).await?;
        parse_home_response(response)
    }

    pub async fn explore(&self) -> Result<ExplorePage> {
        let (explore_response, charts_response) = futures::join!(
            self.fetch_browse_page("FEmusic_explore", None),
            self.fetch_browse_page("FEmusic_charts", Some(CHARTS_PARAMS)),
        );
        let mut page = parse_explore_response(explore_response?)?;
        page.chart_sections = parse_home_response(charts_response?)?.sections;
        Ok(page)
    }

    pub async fn library_page(&self, browse_id: &str, title: &str) -> Result<BrowsePage> {
        if !self.is_authenticated() {
            return Err(AppError::Credential(
                "a YouTube Music session is required for the cloud library".into(),
            ));
        }
        let browse_id = browse_id.trim();
        if !browse_id.starts_with("FEmusic_") {
            return Err(AppError::Protocol(
                "unsupported YouTube Music library endpoint".into(),
            ));
        }
        let requested = BrowseItem {
            browse_id: browse_id.to_owned(),
            params: None,
            kind: BrowseKind::Category,
            title: title.to_owned(),
            subtitle: "YouTube Music".into(),
            thumbnail_url: None,
            editable: false,
        };
        let response = self.fetch_browse_page(browse_id, None).await?;
        parse_browse_response(response, requested)
    }

    pub async fn completed_library_page(&self, browse_id: &str, title: &str) -> Result<BrowsePage> {
        let page = self.library_page(browse_id, title).await?;
        self.complete_browse_page(page).await
    }

    pub async fn completed_library_page_at_tab(
        &self,
        browse_id: &str,
        title: &str,
        tab_index: usize,
    ) -> Result<BrowsePage> {
        if !self.is_authenticated() {
            return Err(AppError::Credential(
                "a YouTube Music session is required for the cloud library".into(),
            ));
        }
        let browse_id = browse_id.trim();
        if !browse_id.starts_with("FEmusic_") {
            return Err(AppError::Protocol(
                "unsupported YouTube Music library endpoint".into(),
            ));
        }
        let requested = BrowseItem {
            browse_id: browse_id.to_owned(),
            params: None,
            kind: BrowseKind::Category,
            title: title.to_owned(),
            subtitle: "YouTube Music".into(),
            thumbnail_url: None,
            editable: false,
        };
        let response = self.fetch_browse_page(browse_id, None).await?;
        let page = parse_browse_tab_response(response, requested, tab_index)?;
        self.complete_browse_page(page).await
    }

    pub async fn completed_playlist_page(
        &self,
        playlist_id: &str,
        title: &str,
    ) -> Result<BrowsePage> {
        if !self.is_authenticated() {
            return Err(AppError::Credential(
                "a YouTube Music session is required for the cloud library".into(),
            ));
        }
        let playlist_id = playlist_id.trim();
        if playlist_id.is_empty() {
            return Err(AppError::Protocol("playlist id cannot be empty".into()));
        }
        let item = BrowseItem {
            browse_id: playlist_id.to_owned(),
            params: None,
            kind: BrowseKind::Playlist,
            title: title.to_owned(),
            subtitle: "YouTube Music".into(),
            thumbnail_url: None,
            editable: false,
        };
        let page = self.browse(&item).await?;
        self.complete_browse_page(page).await
    }

    pub async fn complete_browse_page(&self, mut page: BrowsePage) -> Result<BrowsePage> {
        let mut seen = HashSet::new();
        for _ in 0..HISTORY_CONTINUATION_LIMIT {
            let Some(token) = page.continuation.clone() else {
                return Ok(page);
            };
            if !seen.insert(token.clone()) {
                page.continuation = None;
                return Ok(page);
            }
            let next = self.browse_continuation(&token).await?;
            if page.append_continuation(next) == 0 {
                page.continuation = None;
                return Ok(page);
            }
        }
        Err(AppError::Protocol(
            "YouTube Music collection exceeded the continuation limit".into(),
        ))
    }

    pub async fn episodes_for_later(&self) -> Result<BrowsePage> {
        if !self.is_authenticated() {
            return Err(AppError::Credential(
                "a YouTube Music session is required to sync episodes for later".into(),
            ));
        }
        let requested = BrowseItem {
            browse_id: "VLSE".into(),
            params: None,
            kind: BrowseKind::Playlist,
            title: "Episodes for Later".into(),
            subtitle: "YouTube Music".into(),
            thumbnail_url: None,
            editable: false,
        };
        let response = self.fetch_browse_page("VLSE", None).await?;
        let mut page = parse_browse_response(response, requested)?;
        let mut seen = HashSet::new();
        for _ in 0..HISTORY_CONTINUATION_LIMIT {
            let Some(token) = page.continuation.clone() else {
                for song in &mut page.songs {
                    song.is_episode = true;
                }
                for entry in &mut page.playlist_entries {
                    entry.song.is_episode = true;
                }
                return Ok(page);
            };
            if !seen.insert(token.clone()) {
                page.continuation = None;
                continue;
            }
            let next = self.browse_continuation(&token).await?;
            if page.append_continuation(next) == 0 {
                page.continuation = None;
            }
        }
        Err(AppError::Protocol(
            "YouTube Music Episodes for Later exceeded the safe continuation limit".into(),
        ))
    }

    pub async fn podcast_library_snapshot(&self) -> Result<RemotePodcastLibrary> {
        if !self.is_authenticated() {
            return Err(AppError::Credential(
                "a YouTube Music session is required to sync the podcast library".into(),
            ));
        }
        let (saved_shows, subscribed_channels, episodes) = futures::join!(
            self.completed_library_page(
                "FEmusic_library_non_music_audio_list",
                "Saved podcast shows",
            ),
            self.completed_library_page(
                "FEmusic_library_non_music_audio_channels_list",
                "Subscribed podcast channels",
            ),
            self.episodes_for_later(),
        );
        let mut podcasts = Vec::new();
        for (page, include_channels) in [(saved_shows?, false), (subscribed_channels?, true)] {
            for mut item in page.related {
                let accepted = item.kind == BrowseKind::Podcast
                    || (include_channels
                        && item.kind == BrowseKind::Artist
                        && item.browse_id.starts_with("UC"));
                if !accepted {
                    continue;
                }
                item.kind = BrowseKind::Podcast;
                if !podcasts
                    .iter()
                    .any(|existing: &BrowseItem| existing.browse_id == item.browse_id)
                {
                    podcasts.push(item);
                }
            }
        }

        let episodes = episodes?;
        let mut remote_episodes = Vec::new();
        for song in episodes.songs {
            let set_video_id = episodes
                .playlist_entries
                .iter()
                .find(|entry| entry.song.video_id == song.video_id)
                .map(|entry| entry.set_video_id.clone());
            if !remote_episodes
                .iter()
                .any(|existing: &RemotePodcastEpisode| existing.song.video_id == song.video_id)
            {
                remote_episodes.push(RemotePodcastEpisode { song, set_video_id });
            }
        }
        Ok(RemotePodcastLibrary {
            podcasts,
            episodes: remote_episodes,
        })
    }

    pub async fn history_page(&self) -> Result<RemoteHistoryPage> {
        if !self.is_authenticated() {
            return Err(AppError::Credential(
                "a YouTube Music session is required for remote history".into(),
            ));
        }
        let response = self.fetch_browse_page("FEmusic_history", None).await?;
        parse_history_response(response)
    }

    pub async fn completed_history_page(&self) -> Result<RemoteHistoryPage> {
        let mut page = self.history_page().await?;
        let mut seen = HashSet::new();
        for _ in 0..HISTORY_CONTINUATION_LIMIT {
            let Some(token) = page.continuation.clone() else {
                return Ok(page);
            };
            if !seen.insert(token.clone()) {
                page.continuation = None;
                return Ok(page);
            }
            let response = self.fetch_continuation("browse", &token).await?;
            let next = parse_history_response(response)?;
            if page.append(next) == 0 {
                page.continuation = None;
                return Ok(page);
            }
        }
        Err(AppError::Protocol(
            "YouTube Music history exceeded the safe continuation limit".into(),
        ))
    }

    pub async fn account_info(&self) -> Result<AccountProfile> {
        if !self.is_authenticated() {
            return Err(AppError::Credential(
                "a YouTube Music session must be imported first".into(),
            ));
        }
        let body = serde_json::to_vec(&AccountBody {
            context: RequestContext {
                client: ClientContext {
                    client_name: CLIENT_NAME,
                    client_version: CLIENT_VERSION,
                    language: &self.session.language,
                    region: &self.session.region,
                    visitor_data: self.session.visitor_data.as_deref(),
                },
                request: RequestMetadata { use_ssl: true },
                user: UserContext {
                    locked_safety_mode: false,
                    on_behalf_of_user: self
                        .session
                        .auth
                        .as_ref()
                        .and_then(AuthSession::data_sync_id),
                },
            },
        })
        .map_err(|error| AppError::Protocol(error.to_string()))?;

        let mut last_error = None;
        for _ in 0..ACCOUNT_MAX_ATTEMPTS {
            let request = self
                .apply_login_headers(
                    Request::builder()
                        .method(Method::POST)
                        .uri(format!("{API_ROOT}/account/account_menu?prettyPrint=false"))
                        .header("Accept", "application/json")
                        .header("Accept-Language", "en-US,en;q=0.9")
                        .header("Cache-Control", "no-cache")
                        .header("Content-Type", "application/json")
                        .header("X-Goog-Api-Format-Version", "1")
                        .header("X-YouTube-Client-Name", CLIENT_ID)
                        .header("X-YouTube-Client-Version", CLIENT_VERSION)
                        .header("X-Origin", ORIGIN)
                        .header("Referer", format!("{ORIGIN}/"))
                        .when_some(self.session.visitor_data.as_deref(), |request, visitor| {
                            request.header("X-Goog-Visitor-Id", visitor)
                        })
                        .timeout(Duration::from_secs(60)),
                    true,
                )?
                .body(AsyncBody::from(body.clone()))
                .map_err(|error| AppError::Network(error.to_string()))?;
            let mut response = match self.http.send(request).await {
                Ok(response) => response,
                Err(error) => {
                    last_error = Some(AppError::Network(error.to_string()));
                    continue;
                }
            };
            let status = response.status();
            if matches!(status.as_u16(), 401 | 403) {
                return Err(AppError::SessionExpired(format!(
                    "the YouTube Music session was rejected (HTTP {status})"
                )));
            }
            if !status.is_success() {
                let error = AppError::Network(format!(
                    "YouTube Music account verification returned HTTP {status}"
                ));
                if status.as_u16() == 429 || status.is_server_error() {
                    last_error = Some(error);
                    continue;
                }
                return Err(error);
            }
            let mut response_body = Vec::new();
            if let Err(error) = response.body_mut().read_to_end(&mut response_body).await {
                last_error = Some(AppError::Network(error.to_string()));
                continue;
            }
            return parse_account_info_response(response_body);
        }
        Err(last_error.unwrap_or_else(|| {
            AppError::Network("YouTube Music account verification exhausted its retries".into())
        }))
    }

    pub async fn set_video_liked(&self, video_id: &str, liked: bool) -> Result<()> {
        let video_id = validated_identifier(video_id, "video id")?;
        let body = serde_json::to_vec(&LikeBody {
            context: self.authenticated_context()?,
            target: LikeTarget {
                video_id: Some(video_id),
                playlist_id: None,
            },
        })
        .map_err(|error| AppError::Protocol(error.to_string()))?;
        self.send_authenticated_mutation(
            if liked {
                "like/like"
            } else {
                "like/removelike"
            },
            body,
            true,
        )
        .await
        .map(|_| ())
    }

    pub async fn set_song_in_library(&self, video_id: &str, in_library: bool) -> Result<()> {
        let video_id = validated_identifier(video_id, "video id")?;
        self.authenticated_context()?;
        let response = self
            .fetch_next_page(&RadioEndpoint::song(video_id), None)
            .await?;
        let feedback_token = parse_song_library_feedback_token(response, video_id, in_library)?;
        let feedback_token = validated_feedback_token(&feedback_token)?;
        let body = serde_json::to_vec(&FeedbackBody {
            context: self.authenticated_context()?,
            feedback_tokens: [feedback_token],
        })
        .map_err(|error| AppError::Protocol(error.to_string()))?;
        let response = self
            .send_authenticated_mutation("feedback", body, false)
            .await?;
        let response: FeedbackResponse = serde_json::from_slice(&response).map_err(|_| {
            AppError::Protocol(
                "song library update returned no usable result; refresh the account library before retrying"
                    .into(),
            )
        })?;
        if response.feedback_responses.len() == 1
            && response
                .feedback_responses
                .iter()
                .all(|response| response.is_processed)
        {
            Ok(())
        } else {
            Err(AppError::Protocol(
                "YouTube Music did not process the song library update".into(),
            ))
        }
    }

    pub async fn set_playlist_liked(&self, playlist_id: &str, liked: bool) -> Result<()> {
        let playlist_id = normalized_playlist_id(playlist_id)?;
        let body = serde_json::to_vec(&LikeBody {
            context: self.authenticated_context()?,
            target: LikeTarget {
                video_id: None,
                playlist_id: Some(playlist_id),
            },
        })
        .map_err(|error| AppError::Protocol(error.to_string()))?;
        self.send_authenticated_mutation(
            if liked {
                "like/like"
            } else {
                "like/removelike"
            },
            body,
            true,
        )
        .await
        .map(|_| ())
    }

    pub async fn set_podcast_saved(&self, podcast_id: &str, saved: bool) -> Result<()> {
        let podcast_id = validated_identifier(podcast_id, "podcast id")?;
        let playlist_id = podcast_id.strip_prefix("MPSP").unwrap_or(podcast_id);
        self.set_playlist_liked(playlist_id, saved).await
    }

    pub async fn set_episode_saved(&self, video_id: &str, saved: bool) -> Result<()> {
        let video_id = validated_identifier(video_id, "episode video id")?;
        if saved {
            return self.add_video_to_playlist("SE", video_id).await;
        }

        let remote = self.episodes_for_later().await?;
        let entry = remote
            .playlist_entries
            .into_iter()
            .find(|entry| entry.song.video_id == video_id);
        let Some(entry) = entry else {
            if remote.songs.iter().any(|song| song.video_id == video_id) {
                return Err(AppError::Protocol(
                    "YouTube Music returned the saved episode without a removal token".into(),
                ));
            }
            return Ok(());
        };
        self.remove_video_from_playlist("SE", video_id, &entry.set_video_id)
            .await
    }

    pub async fn set_channel_subscribed(
        &self,
        channel_id: &str,
        subscribed: bool,
        params: Option<&str>,
    ) -> Result<()> {
        let channel_id = validated_identifier(channel_id, "channel id")?;
        let params = params
            .map(|value| validated_identifier(value, "subscription params"))
            .transpose()?
            .or(Some("EgIIAhgA"));
        let body = serde_json::to_vec(&SubscribeBody {
            context: self.authenticated_context()?,
            channel_ids: [channel_id],
            params,
        })
        .map_err(|error| AppError::Protocol(error.to_string()))?;
        self.send_authenticated_mutation(
            if subscribed {
                "subscription/subscribe"
            } else {
                "subscription/unsubscribe"
            },
            body,
            true,
        )
        .await
        .map(|_| ())
    }

    pub async fn create_playlist(&self, title: &str) -> Result<String> {
        let title = validated_playlist_title(title)?;
        let body = serde_json::to_vec(&CreatePlaylistBody {
            context: self.authenticated_context()?,
            title,
            privacy_status: "PRIVATE",
        })
        .map_err(|error| AppError::Protocol(error.to_string()))?;
        let response = self
            .send_authenticated_mutation("playlist/create", body, false)
            .await?;
        let response: CreatePlaylistResponse = serde_json::from_slice(&response).map_err(|_| {
            AppError::Protocol(
                "playlist creation returned no usable id; the remote outcome is unknown, so refresh before retrying"
                    .into(),
            )
        })?;
        validated_identifier(&response.playlist_id, "created playlist id")
            .map(str::to_owned)
            .map_err(|_| {
                AppError::Protocol(
                    "playlist creation returned an invalid id; the remote outcome is unknown, so refresh before retrying"
                        .into(),
                )
            })
    }

    pub async fn add_video_to_playlist(&self, playlist_id: &str, video_id: &str) -> Result<()> {
        let playlist_id = normalized_playlist_id(playlist_id)?;
        let video_id = validated_identifier(video_id, "video id")?;
        let body = serde_json::to_vec(&EditPlaylistBody {
            context: self.authenticated_context()?,
            playlist_id,
            actions: [EditPlaylistAction::AddVideo {
                action: "ACTION_ADD_VIDEO",
                added_video_id: video_id,
            }],
        })
        .map_err(|error| AppError::Protocol(error.to_string()))?;
        self.send_authenticated_mutation("browse/edit_playlist", body, false)
            .await
            .map(|_| ())
    }

    pub async fn remove_video_from_playlist(
        &self,
        playlist_id: &str,
        video_id: &str,
        set_video_id: &str,
    ) -> Result<()> {
        let playlist_id = normalized_playlist_id(playlist_id)?;
        let video_id = validated_identifier(video_id, "video id")?;
        let set_video_id = validated_identifier(set_video_id, "playlist set video id")?;
        let body = serde_json::to_vec(&EditPlaylistBody {
            context: self.authenticated_context()?,
            playlist_id,
            actions: [EditPlaylistAction::RemoveVideo {
                action: "ACTION_REMOVE_VIDEO",
                removed_video_id: video_id,
                set_video_id,
            }],
        })
        .map_err(|error| AppError::Protocol(error.to_string()))?;
        self.send_authenticated_mutation("browse/edit_playlist", body, false)
            .await
            .map(|_| ())
    }

    pub async fn rename_playlist(&self, playlist_id: &str, title: &str) -> Result<()> {
        let playlist_id = normalized_playlist_id(playlist_id)?;
        let title = validated_playlist_title(title)?;
        let body = serde_json::to_vec(&EditPlaylistBody {
            context: self.authenticated_context()?,
            playlist_id,
            actions: [EditPlaylistAction::Rename {
                action: "ACTION_SET_PLAYLIST_NAME",
                playlist_name: title,
            }],
        })
        .map_err(|error| AppError::Protocol(error.to_string()))?;
        self.send_authenticated_mutation("browse/edit_playlist", body, false)
            .await
            .map(|_| ())
    }

    pub async fn delete_playlist(&self, playlist_id: &str) -> Result<()> {
        let playlist_id = normalized_playlist_id(playlist_id)?;
        let body = serde_json::to_vec(&DeletePlaylistBody {
            context: self.authenticated_context()?,
            playlist_id,
        })
        .map_err(|error| AppError::Protocol(error.to_string()))?;
        self.send_authenticated_mutation("playlist/delete", body, false)
            .await
            .map(|_| ())
    }

    pub async fn remove_history_item(&self, feedback_token: &str) -> Result<()> {
        let feedback_token = validated_feedback_token(feedback_token)?;
        let body = serde_json::to_vec(&FeedbackBody {
            context: self.authenticated_context()?,
            feedback_tokens: [feedback_token],
        })
        .map_err(|error| AppError::Protocol(error.to_string()))?;
        let response = self
            .send_authenticated_mutation("feedback", body, false)
            .await?;
        let response: FeedbackResponse = serde_json::from_slice(&response).map_err(|_| {
            AppError::Protocol(
                "history removal returned no usable result; the remote outcome is unknown, so refresh before retrying"
                    .into(),
            )
        })?;
        if response.feedback_responses.len() == 1
            && response
                .feedback_responses
                .iter()
                .all(|response| response.is_processed)
        {
            Ok(())
        } else {
            Err(AppError::Protocol(
                "YouTube Music did not process the history removal request".into(),
            ))
        }
    }

    pub async fn register_playback(&self, tracking: &PlaybackTrackingUrl) -> Result<()> {
        if !self.is_authenticated() {
            return Err(AppError::Credential(
                "a YouTube Music session is required to sync listening history".into(),
            ));
        }
        let mut url = validated_playback_tracking_url(&tracking.0)?;
        let cpn = playback_cpn();
        url.query_pairs_mut()
            .append_pair("c", CLIENT_NAME)
            .append_pair("cpn", &cpn)
            .append_pair("ver", PLAYBACK_TRACKING_VERSION);

        let mut last_error = None;
        for attempt in 0..IDEMPOTENT_MUTATION_MAX_ATTEMPTS {
            let request = self
                .apply_login_headers(
                    Request::builder()
                        .method(Method::GET)
                        .uri(url.as_str())
                        .header("Accept", "*/*")
                        .header("Accept-Language", "en-US,en;q=0.9")
                        .header("User-Agent", USER_AGENT)
                        .header("X-Goog-Api-Format-Version", "1")
                        .header("X-YouTube-Client-Name", CLIENT_ID)
                        .header("X-YouTube-Client-Version", CLIENT_VERSION)
                        .header("X-Origin", ORIGIN)
                        .header("Referer", format!("{ORIGIN}/"))
                        .when_some(self.session.visitor_data.as_deref(), |request, visitor| {
                            request.header("X-Goog-Visitor-Id", visitor)
                        })
                        .timeout(Duration::from_secs(30)),
                    true,
                )?
                .body(AsyncBody::default())
                .map_err(|_| {
                    AppError::Protocol(
                        "YouTube playback registration request could not be constructed".into(),
                    )
                })?;
            let response = match self.http.send(request).await {
                Ok(response) => response,
                Err(_) => {
                    let error =
                        AppError::Network("YouTube playback registration request failed".into());
                    if attempt + 1 < IDEMPOTENT_MUTATION_MAX_ATTEMPTS {
                        last_error = Some(error);
                        continue;
                    }
                    return Err(error);
                }
            };
            let status = response.status();
            if matches!(status.as_u16(), 401 | 403) {
                return Err(AppError::SessionExpired(format!(
                    "the YouTube Music session was rejected (HTTP {status})"
                )));
            }
            if status.is_success() {
                return Ok(());
            }
            let error = AppError::Network(format!(
                "YouTube playback registration returned HTTP {status}"
            ));
            if attempt + 1 < IDEMPOTENT_MUTATION_MAX_ATTEMPTS
                && (status.as_u16() == 429 || status.is_server_error())
            {
                last_error = Some(error);
                continue;
            }
            return Err(error);
        }
        Err(last_error.unwrap_or_else(|| {
            AppError::Network("YouTube playback registration exhausted its retries".into())
        }))
    }

    fn authenticated_context(&self) -> Result<RequestContext<'_>> {
        let auth = self.session.auth.as_ref().ok_or_else(|| {
            AppError::Credential("a YouTube Music session must be imported first".into())
        })?;
        Ok(RequestContext {
            client: ClientContext {
                client_name: CLIENT_NAME,
                client_version: CLIENT_VERSION,
                language: &self.session.language,
                region: &self.session.region,
                visitor_data: self.session.visitor_data.as_deref(),
            },
            request: RequestMetadata { use_ssl: true },
            user: UserContext {
                locked_safety_mode: false,
                on_behalf_of_user: auth.data_sync_id(),
            },
        })
    }

    async fn send_authenticated_mutation(
        &self,
        endpoint: &'static str,
        body: Vec<u8>,
        retry_safe: bool,
    ) -> Result<Vec<u8>> {
        if !self.is_authenticated() {
            return Err(AppError::Credential(
                "a YouTube Music session must be imported first".into(),
            ));
        }
        let max_attempts = if retry_safe {
            IDEMPOTENT_MUTATION_MAX_ATTEMPTS
        } else {
            1
        };
        let mut last_error = None;
        for attempt in 0..max_attempts {
            let request = self
                .apply_login_headers(
                    Request::builder()
                        .method(Method::POST)
                        .uri(format!("{API_ROOT}/{endpoint}?prettyPrint=false"))
                        .header("Accept", "application/json")
                        .header("Accept-Language", "en-US,en;q=0.9")
                        .header("Cache-Control", "no-cache")
                        .header("Content-Type", "application/json")
                        .header("X-Goog-Api-Format-Version", "1")
                        .header("X-YouTube-Client-Name", CLIENT_ID)
                        .header("X-YouTube-Client-Version", CLIENT_VERSION)
                        .header("X-Origin", ORIGIN)
                        .header("Referer", format!("{ORIGIN}/"))
                        .when_some(self.session.visitor_data.as_deref(), |request, visitor| {
                            request.header("X-Goog-Visitor-Id", visitor)
                        })
                        .timeout(Duration::from_secs(60)),
                    true,
                )?
                .body(AsyncBody::from(body.clone()))
                .map_err(|error| AppError::Network(error.to_string()))?;
            let mut response = match self.http.send(request).await {
                Ok(response) => response,
                Err(error) => {
                    let error = AppError::Network(error.to_string());
                    if retry_safe && attempt + 1 < max_attempts {
                        last_error = Some(error);
                        continue;
                    }
                    return Err(if retry_safe {
                        error
                    } else {
                        uncertain_write_error(error)
                    });
                }
            };
            let status = response.status();
            if matches!(status.as_u16(), 401 | 403) {
                return Err(AppError::SessionExpired(format!(
                    "the YouTube Music session was rejected (HTTP {status})"
                )));
            }
            if !status.is_success() {
                let error = AppError::Network(format!(
                    "YouTube Music write request returned HTTP {status}"
                ));
                if retry_safe
                    && attempt + 1 < max_attempts
                    && (status.as_u16() == 429 || status.is_server_error())
                {
                    last_error = Some(error);
                    continue;
                }
                return Err(if retry_safe {
                    error
                } else {
                    uncertain_write_error(error)
                });
            }
            let mut response_body = Vec::new();
            if let Err(error) = response.body_mut().read_to_end(&mut response_body).await {
                let error = AppError::Network(error.to_string());
                if retry_safe && attempt + 1 < max_attempts {
                    last_error = Some(error);
                    continue;
                }
                return Err(if retry_safe {
                    error
                } else {
                    uncertain_write_error(error)
                });
            }
            return Ok(response_body);
        }
        Err(last_error.unwrap_or_else(|| {
            AppError::Network("YouTube Music write request exhausted its retries".into())
        }))
    }

    pub async fn queue_song(&self, video_id: &str) -> Result<Song> {
        let video_id = video_id.trim();
        if video_id.is_empty() {
            return Err(AppError::Protocol("video id cannot be empty".into()));
        }
        let response = self.fetch_queue_song_page(video_id).await?;
        parse_queue_song_response(response, video_id)
    }

    pub async fn media_info(&self, video_id: &str) -> Result<MediaInfo> {
        let video_id = video_id.trim();
        if video_id.is_empty() {
            return Err(AppError::Protocol("video id cannot be empty".into()));
        }
        let (watch_page, public_counts) = futures::join!(
            self.fetch_media_info_page(video_id),
            self.fetch_public_video_counts(video_id),
        );
        let mut info = parse_media_info_response(watch_page?, video_id)?;
        let counts: ReturnYoutubeDislikeResponse = serde_json::from_slice(&public_counts?)
            .map_err(|error| AppError::Protocol(error.to_string()))?;
        info.view_count = counts.view_count;
        info.likes = counts.likes;
        info.dislikes = counts.dislikes;
        Ok(info)
    }

    pub async fn radio(&self, seed_video_id: &str) -> Result<RadioPage> {
        let preferred = RadioEndpoint::song_radio(seed_video_id)?;
        let preferred_result = self.fetch_radio_chain(preferred, None).await;
        if let Ok(page) = preferred_result.as_ref()
            && page.has_recommendation_for(seed_video_id)
        {
            return Ok(page.clone());
        }

        let mut fallback = self
            .fetch_radio_chain(RadioEndpoint::song(seed_video_id), None)
            .await
            .or(preferred_result)?;
        if !fallback.has_recommendation_for(seed_video_id)
            && let Some(related) = fallback.related_endpoint.clone()
        {
            let response = self
                .fetch_browse_page(&related.browse_id, related.params.as_deref())
                .await?;
            let related_songs = parse_related_songs(&response)?;
            for song in related_songs {
                if song.video_id != seed_video_id
                    && !fallback
                        .songs
                        .iter()
                        .any(|existing| existing.video_id == song.video_id)
                {
                    fallback.songs.push(song);
                }
            }
        }
        Ok(fallback)
    }

    pub async fn playback_queue(&self, endpoint: BrowsePlaybackEndpoint) -> Result<RadioPage> {
        self.fetch_radio_chain(endpoint.into(), None).await
    }

    pub async fn radio_continuation(
        &self,
        endpoint: RadioEndpoint,
        continuation: &str,
    ) -> Result<RadioPage> {
        let continuation = continuation.trim();
        if continuation.is_empty() {
            return Err(AppError::Protocol(
                "radio continuation cannot be empty".into(),
            ));
        }
        let mut page = self.fetch_radio_chain(endpoint, Some(continuation)).await?;
        if page.songs.is_empty() {
            page.continuation = None;
        }
        Ok(page)
    }

    async fn fetch_radio_chain(
        &self,
        endpoint: RadioEndpoint,
        continuation: Option<&str>,
    ) -> Result<RadioPage> {
        let mut endpoint = endpoint;
        let mut continuation = continuation.map(str::to_owned);
        let mut combined: Option<RadioPage> = None;
        for _ in 0..3 {
            let parsed = self
                .fetch_radio_segment(&endpoint, continuation.as_deref())
                .await?;
            let automix = parsed.automix_endpoint.clone();
            if let Some(combined) = &mut combined {
                combined.append_unique(parsed.page);
            } else {
                combined = Some(parsed.page);
            }
            let Some(next_endpoint) = automix.filter(|next| next != &endpoint) else {
                return combined
                    .ok_or_else(|| AppError::Protocol("radio response contained no queue".into()));
            };
            endpoint = next_endpoint;
            continuation = None;
        }
        Err(AppError::Protocol(
            "radio response contained an automix endpoint cycle".into(),
        ))
    }

    async fn fetch_radio_segment(
        &self,
        endpoint: &RadioEndpoint,
        continuation: Option<&str>,
    ) -> Result<ParsedRadioPage> {
        let mut last_error = None;
        for _ in 0..RADIO_MAX_ATTEMPTS {
            match self.fetch_next_page(endpoint, continuation).await {
                Ok(response) => match parse_radio_response(response, endpoint.clone()) {
                    Ok(page) => return Ok(page),
                    Err(error) => last_error = Some(error),
                },
                Err(error) => last_error = Some(error),
            }
        }
        Err(last_error.unwrap_or_else(|| {
            AppError::Protocol("radio request exhausted without an error".into())
        }))
    }

    /// Resolve a directly usable M4A/AAC source through the anonymous visionOS
    /// client. This deliberately avoids WEB_REMIX signature and PoToken
    /// handling until those paths have their own validated resolver.
    pub async fn resolve_playback_source(&self, video_id: &str) -> Result<ResolvedPlayback> {
        let video_id = video_id.trim();
        if video_id.is_empty() {
            return Err(AppError::Protocol("video id cannot be empty".into()));
        }

        let visitor_data = match self.session.visitor_data.clone() {
            Some(visitor_data) => Some(visitor_data),
            None => self.fetch_visitor_data().await.ok(),
        };
        let body = serde_json::to_vec(&PlayerBody {
            context: PlayerRequestContext {
                client: PlayerClientContext {
                    client_name: PLAYBACK_CLIENT_NAME,
                    client_version: PLAYBACK_CLIENT_VERSION,
                    language: &self.session.language,
                    region: &self.session.region,
                    os_name: "visionOS",
                    os_version: "1.3.21O771",
                    device_make: "Apple",
                    device_model: "RealityDevice14,1",
                    visitor_data: visitor_data.as_deref(),
                },
                request: RequestMetadata { use_ssl: true },
                user: UserContext {
                    locked_safety_mode: false,
                    on_behalf_of_user: None,
                },
            },
            video_id,
        })
        .map_err(|error| AppError::Protocol(error.to_string()))?;

        let request = Request::builder()
            .method(Method::POST)
            .uri(format!("{API_ROOT}/player?prettyPrint=false"))
            .header("Accept", "application/json")
            .header("Content-Type", "application/json")
            .header("User-Agent", PLAYBACK_USER_AGENT)
            .header("X-Goog-Api-Format-Version", "1")
            .header("X-YouTube-Client-Name", PLAYBACK_CLIENT_ID)
            .header("X-YouTube-Client-Version", PLAYBACK_CLIENT_VERSION)
            .header("X-Origin", ORIGIN)
            .header("Referer", format!("{ORIGIN}/"))
            .when_some(visitor_data.as_deref(), |request, visitor| {
                request.header("X-Goog-Visitor-Id", visitor)
            })
            .timeout(Duration::from_secs(60))
            .body(AsyncBody::from(body))
            .map_err(|error| AppError::Network(error.to_string()))?;

        let mut response = self
            .http
            .send(request)
            .await
            .map_err(|error| AppError::Network(error.to_string()))?;
        let status = response.status();
        let mut response_body = Vec::new();
        response
            .body_mut()
            .read_to_end(&mut response_body)
            .await
            .map_err(|error| AppError::Network(error.to_string()))?;
        if !status.is_success() {
            return Err(AppError::Network(format!(
                "InnerTube player returned HTTP {status}"
            )));
        }

        let mut resolved = parse_player_response_with_quality(&response_body, self.audio_quality)?;
        resolved.source.cache_key = Some(self.audio_quality.playback_cache_key(video_id));
        Ok(resolved)
    }

    pub async fn resolve_playback_tracking(
        &self,
        video_id: &str,
    ) -> Result<Option<PlaybackTrackingUrl>> {
        Ok(self
            .resolve_playback_source(video_id)
            .await?
            .playback_tracking)
    }

    pub fn open_playback_source(&self, source: PlaybackSource) -> HttpRangeMediaSource {
        HttpRangeMediaSource::new(self.http.clone(), source)
    }

    async fn fetch_visitor_data(&self) -> Result<String> {
        let request = Request::builder()
            .uri(format!("{ORIGIN}/sw.js_data"))
            .header("Accept", "application/json")
            .header("User-Agent", USER_AGENT)
            .timeout(Duration::from_secs(30))
            .body(AsyncBody::default())
            .map_err(|error| AppError::Network(error.to_string()))?;
        let mut response = self
            .http
            .send(request)
            .await
            .map_err(|error| AppError::Network(error.to_string()))?;
        if !response.status().is_success() {
            return Err(AppError::Network(format!(
                "visitor session endpoint returned HTTP {}",
                response.status()
            )));
        }

        let mut body = Vec::new();
        response
            .body_mut()
            .read_to_end(&mut body)
            .await
            .map_err(|error| AppError::Network(error.to_string()))?;
        let json = body.strip_prefix(b")]}'\n").unwrap_or(&body);
        let value: Value =
            serde_json::from_slice(json).map_err(|error| AppError::Protocol(error.to_string()))?;
        find_visitor_data(&value)
            .map(str::to_owned)
            .ok_or_else(|| AppError::Protocol("visitor session response contained no id".into()))
    }

    async fn fetch_browse_page(&self, browse_id: &str, params: Option<&str>) -> Result<Vec<u8>> {
        let browse_id = browse_id.trim();
        if browse_id.is_empty() {
            return Err(AppError::Protocol("browse id cannot be empty".into()));
        }
        let body = serde_json::to_vec(&BrowseBody {
            context: RequestContext {
                client: ClientContext {
                    client_name: CLIENT_NAME,
                    client_version: CLIENT_VERSION,
                    language: &self.session.language,
                    region: &self.session.region,
                    visitor_data: self.session.visitor_data.as_deref(),
                },
                request: RequestMetadata { use_ssl: true },
                user: UserContext {
                    locked_safety_mode: false,
                    on_behalf_of_user: self
                        .session
                        .auth
                        .as_ref()
                        .and_then(AuthSession::data_sync_id),
                },
            },
            browse_id,
            params: params.map(str::trim).filter(|params| !params.is_empty()),
        })
        .map_err(|error| AppError::Protocol(error.to_string()))?;
        let request = self
            .apply_login_headers(
                Request::builder()
                    .method(Method::POST)
                    .uri(format!("{API_ROOT}/browse?prettyPrint=false"))
                    .header("Accept", "application/json")
                    .header("Accept-Language", "en-US,en;q=0.9")
                    .header("Cache-Control", "no-cache")
                    .header("Content-Type", "application/json")
                    .header("X-Goog-Api-Format-Version", "1")
                    .header("X-YouTube-Client-Name", CLIENT_ID)
                    .header("X-YouTube-Client-Version", CLIENT_VERSION)
                    .header("X-Origin", ORIGIN)
                    .header("Referer", format!("{ORIGIN}/"))
                    .when_some(self.session.visitor_data.as_deref(), |request, visitor| {
                        request.header("X-Goog-Visitor-Id", visitor)
                    })
                    .timeout(Duration::from_secs(60)),
                true,
            )?
            .body(AsyncBody::from(body))
            .map_err(|error| AppError::Network(error.to_string()))?;
        let mut response = self
            .http
            .send(request)
            .await
            .map_err(|error| AppError::Network(error.to_string()))?;
        let status = response.status();
        let mut response_body = Vec::new();
        response
            .body_mut()
            .read_to_end(&mut response_body)
            .await
            .map_err(|error| AppError::Network(error.to_string()))?;
        if matches!(status.as_u16(), 401 | 403) && self.is_authenticated() {
            return Err(AppError::SessionExpired(format!(
                "the YouTube Music session was rejected (HTTP {status})"
            )));
        }
        if !status.is_success() {
            return Err(AppError::Network(format!(
                "InnerTube browse returned HTTP {status}"
            )));
        }
        Ok(response_body)
    }

    async fn fetch_next_page(
        &self,
        endpoint: &RadioEndpoint,
        continuation: Option<&str>,
    ) -> Result<Vec<u8>> {
        let body = serde_json::to_vec(&NextBody {
            context: RequestContext {
                client: ClientContext {
                    client_name: CLIENT_NAME,
                    client_version: CLIENT_VERSION,
                    language: &self.session.language,
                    region: &self.session.region,
                    visitor_data: self.session.visitor_data.as_deref(),
                },
                request: RequestMetadata { use_ssl: true },
                user: UserContext {
                    locked_safety_mode: false,
                    on_behalf_of_user: self
                        .session
                        .auth
                        .as_ref()
                        .and_then(AuthSession::data_sync_id),
                },
            },
            endpoint,
            continuation,
        })
        .map_err(|error| AppError::Protocol(error.to_string()))?;
        let request = self
            .apply_login_headers(
                Request::builder()
                    .method(Method::POST)
                    .uri(format!("{API_ROOT}/next?prettyPrint=false"))
                    .header("Accept", "application/json")
                    .header("Accept-Language", "en-US,en;q=0.9")
                    .header("Cache-Control", "no-cache")
                    .header("Content-Type", "application/json")
                    .header("X-Goog-Api-Format-Version", "1")
                    .header("X-YouTube-Client-Name", CLIENT_ID)
                    .header("X-YouTube-Client-Version", CLIENT_VERSION)
                    .header("X-Origin", ORIGIN)
                    .header("Referer", format!("{ORIGIN}/"))
                    .when_some(self.session.visitor_data.as_deref(), |request, visitor| {
                        request.header("X-Goog-Visitor-Id", visitor)
                    })
                    .timeout(Duration::from_secs(60)),
                true,
            )?
            .body(AsyncBody::from(body))
            .map_err(|error| AppError::Network(error.to_string()))?;
        let mut response = self
            .http
            .send(request)
            .await
            .map_err(|error| AppError::Network(error.to_string()))?;
        let status = response.status();
        let mut response_body = Vec::new();
        response
            .body_mut()
            .read_to_end(&mut response_body)
            .await
            .map_err(|error| AppError::Network(error.to_string()))?;
        if matches!(status.as_u16(), 401 | 403) && self.is_authenticated() {
            return Err(AppError::SessionExpired(format!(
                "the YouTube Music session was rejected (HTTP {status})"
            )));
        }
        if !status.is_success() {
            return Err(AppError::Network(format!(
                "InnerTube next returned HTTP {status}"
            )));
        }
        Ok(response_body)
    }

    async fn fetch_queue_song_page(&self, video_id: &str) -> Result<Vec<u8>> {
        let video_ids = [video_id];
        let body = serde_json::to_vec(&GetQueueBody {
            context: RequestContext {
                client: ClientContext {
                    client_name: CLIENT_NAME,
                    client_version: CLIENT_VERSION,
                    language: &self.session.language,
                    region: &self.session.region,
                    visitor_data: self.session.visitor_data.as_deref(),
                },
                request: RequestMetadata { use_ssl: true },
                user: UserContext {
                    locked_safety_mode: false,
                    on_behalf_of_user: None,
                },
            },
            video_ids: &video_ids,
            playlist_id: None,
        })
        .map_err(|error| AppError::Protocol(error.to_string()))?;
        let request = Request::builder()
            .method(Method::POST)
            .uri(format!("{API_ROOT}/music/get_queue?prettyPrint=false"))
            .header("Accept", "application/json")
            .header("Accept-Language", "en-US,en;q=0.9")
            .header("Cache-Control", "no-cache")
            .header("Content-Type", "application/json")
            .header("X-Goog-Api-Format-Version", "1")
            .header("X-YouTube-Client-Name", CLIENT_ID)
            .header("X-YouTube-Client-Version", CLIENT_VERSION)
            .header("X-Origin", ORIGIN)
            .header("Referer", format!("{ORIGIN}/"))
            .when_some(self.session.visitor_data.as_deref(), |request, visitor| {
                request.header("X-Goog-Visitor-Id", visitor)
            })
            .timeout(Duration::from_secs(60))
            .body(AsyncBody::from(body))
            .map_err(|error| AppError::Network(error.to_string()))?;
        self.read_response_body(request, "YouTube Music get_queue")
            .await
    }

    async fn fetch_media_info_page(&self, video_id: &str) -> Result<Vec<u8>> {
        let endpoint = RadioEndpoint::song(video_id);
        let body = serde_json::to_vec(&NextBody {
            context: RequestContext {
                client: ClientContext {
                    client_name: WEB_CLIENT_NAME,
                    client_version: WEB_CLIENT_VERSION,
                    language: &self.session.language,
                    region: &self.session.region,
                    visitor_data: self.session.visitor_data.as_deref(),
                },
                request: RequestMetadata { use_ssl: true },
                user: UserContext {
                    locked_safety_mode: false,
                    on_behalf_of_user: None,
                },
            },
            endpoint: &endpoint,
            continuation: None,
        })
        .map_err(|error| AppError::Protocol(error.to_string()))?;
        let request = Request::builder()
            .method(Method::POST)
            .uri(format!("{API_ROOT}/next?prettyPrint=false"))
            .header("Accept", "application/json")
            .header("Accept-Language", "en-US,en;q=0.9")
            .header("Cache-Control", "no-cache")
            .header("Content-Type", "application/json")
            .header("X-Goog-Api-Format-Version", "1")
            .header("X-YouTube-Client-Name", WEB_CLIENT_ID)
            .header("X-YouTube-Client-Version", WEB_CLIENT_VERSION)
            .header("X-Origin", ORIGIN)
            .header("Referer", format!("{ORIGIN}/"))
            .when_some(self.session.visitor_data.as_deref(), |request, visitor| {
                request.header("X-Goog-Visitor-Id", visitor)
            })
            .timeout(Duration::from_secs(60))
            .body(AsyncBody::from(body))
            .map_err(|error| AppError::Network(error.to_string()))?;
        self.read_response_body(request, "YouTube watch next").await
    }

    async fn fetch_public_video_counts(&self, video_id: &str) -> Result<Vec<u8>> {
        let mut url = Url::parse(RETURN_YOUTUBE_DISLIKE_API)
            .map_err(|error| AppError::Protocol(error.to_string()))?;
        url.query_pairs_mut().append_pair("videoId", video_id);
        let request = Request::builder()
            .method(Method::GET)
            .uri(url.as_str())
            .header("Accept", "application/json")
            .timeout(Duration::from_secs(30))
            .body(AsyncBody::default())
            .map_err(|error| AppError::Network(error.to_string()))?;
        self.read_response_body(request, "public video counts")
            .await
    }

    async fn read_response_body(
        &self,
        request: Request<AsyncBody>,
        operation: &str,
    ) -> Result<Vec<u8>> {
        let mut response = self
            .http
            .send(request)
            .await
            .map_err(|error| AppError::Network(error.to_string()))?;
        let status = response.status();
        let mut response_body = Vec::new();
        response
            .body_mut()
            .read_to_end(&mut response_body)
            .await
            .map_err(|error| AppError::Network(error.to_string()))?;
        if !status.is_success() {
            return Err(AppError::Network(format!(
                "{operation} returned HTTP {status}"
            )));
        }
        Ok(response_body)
    }

    async fn fetch_continuation(&self, endpoint: &str, continuation: &str) -> Result<Vec<u8>> {
        let continuation = continuation.trim();
        if continuation.is_empty() {
            return Err(AppError::Protocol(
                "continuation token cannot be empty".into(),
            ));
        }
        let uri = continuation_uri(endpoint, continuation)?;
        let body = serde_json::to_vec(&ContinuationBody {
            context: RequestContext {
                client: ClientContext {
                    client_name: CLIENT_NAME,
                    client_version: CLIENT_VERSION,
                    language: &self.session.language,
                    region: &self.session.region,
                    visitor_data: self.session.visitor_data.as_deref(),
                },
                request: RequestMetadata { use_ssl: true },
                user: UserContext {
                    locked_safety_mode: false,
                    on_behalf_of_user: if endpoint == "browse" {
                        self.session
                            .auth
                            .as_ref()
                            .and_then(AuthSession::data_sync_id)
                    } else {
                        None
                    },
                },
            },
        })
        .map_err(|error| AppError::Protocol(error.to_string()))?;
        let request = self
            .apply_login_headers(
                Request::builder()
                    .method(Method::POST)
                    .uri(uri)
                    .header("Accept", "application/json")
                    .header("Accept-Language", "en-US,en;q=0.9")
                    .header("Cache-Control", "no-cache")
                    .header("Content-Type", "application/json")
                    .header("X-Goog-Api-Format-Version", "1")
                    .header("X-YouTube-Client-Name", CLIENT_ID)
                    .header("X-YouTube-Client-Version", CLIENT_VERSION)
                    .header("X-Origin", ORIGIN)
                    .header("Referer", format!("{ORIGIN}/"))
                    .when_some(self.session.visitor_data.as_deref(), |request, visitor| {
                        request.header("X-Goog-Visitor-Id", visitor)
                    })
                    .timeout(Duration::from_secs(60)),
                endpoint == "browse",
            )?
            .body(AsyncBody::from(body))
            .map_err(|error| AppError::Network(error.to_string()))?;
        let mut response = self
            .http
            .send(request)
            .await
            .map_err(|error| AppError::Network(error.to_string()))?;
        let status = response.status();
        let mut response_body = Vec::new();
        response
            .body_mut()
            .read_to_end(&mut response_body)
            .await
            .map_err(|error| AppError::Network(error.to_string()))?;
        if matches!(status.as_u16(), 401 | 403) && endpoint == "browse" && self.is_authenticated() {
            return Err(AppError::SessionExpired(format!(
                "the YouTube Music session was rejected (HTTP {status})"
            )));
        }
        if !status.is_success() {
            return Err(AppError::Network(format!(
                "InnerTube continuation returned HTTP {status}"
            )));
        }
        Ok(response_body)
    }
}

fn continuation_uri(endpoint: &str, continuation: &str) -> Result<String> {
    let endpoint = match endpoint {
        "search" | "browse" => endpoint,
        _ => {
            return Err(AppError::Protocol(
                "unsupported continuation endpoint".into(),
            ));
        }
    };
    let mut url = Url::parse(&format!("{API_ROOT}/{endpoint}"))
        .map_err(|error| AppError::Protocol(error.to_string()))?;
    url.query_pairs_mut()
        .append_pair("prettyPrint", "false")
        .append_pair("continuation", continuation)
        .append_pair("ctoken", continuation);
    Ok(url.into())
}

fn validated_identifier<'a>(value: &'a str, label: &str) -> Result<&'a str> {
    let value = value.trim();
    if value.is_empty() || value.len() > 2_048 || value.chars().any(char::is_control) {
        return Err(AppError::Protocol(format!("{label} is invalid")));
    }
    Ok(value)
}

fn normalized_playlist_id(value: &str) -> Result<&str> {
    let value = validated_identifier(value, "playlist id")?;
    validated_identifier(value.strip_prefix("VL").unwrap_or(value), "playlist id")
}

fn validated_playlist_title(value: &str) -> Result<&str> {
    let value = value.trim();
    if value.is_empty() || value.len() > 256 || value.chars().any(char::is_control) {
        return Err(AppError::Protocol(
            "playlist title must contain 1 to 256 non-control bytes".into(),
        ));
    }
    Ok(value)
}

fn validated_feedback_token(value: &str) -> Result<&str> {
    let value = value.trim();
    if value.is_empty() || value.len() > 16_384 || value.chars().any(char::is_control) {
        return Err(AppError::Protocol(
            "YouTube Music feedback token is invalid".into(),
        ));
    }
    Ok(value)
}

fn validated_playback_tracking_url(value: &str) -> Result<Url> {
    if value.len() > 16_384 || value.chars().any(char::is_control) {
        return Err(AppError::Protocol(
            "YouTube playback tracking URL is invalid".into(),
        ));
    }
    let url = Url::parse(value)
        .map_err(|_| AppError::Protocol("YouTube playback tracking URL is invalid".into()))?;
    let trusted_host = matches!(
        url.host_str(),
        Some("s.youtube.com" | "www.youtube.com" | "music.youtube.com")
    );
    if url.scheme() != "https"
        || !trusted_host
        || url.path() != "/api/stats/playback"
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
    {
        return Err(AppError::Protocol(
            "YouTube playback tracking URL is not a trusted playback endpoint".into(),
        ));
    }
    Ok(url)
}

fn playback_cpn() -> String {
    (0..16)
        .map(|_| CPN_ALPHABET[fastrand::usize(..CPN_ALPHABET.len())] as char)
        .collect()
}

fn uncertain_write_error(error: AppError) -> AppError {
    let detail = match error {
        AppError::Network(detail) => detail,
        error => error.to_string(),
    };
    AppError::Network(format!(
        "{detail}; the remote outcome is unknown, so refresh before retrying"
    ))
}

#[derive(Serialize)]
struct SearchBody<'a> {
    context: RequestContext<'a>,
    query: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    params: Option<&'a str>,
}

#[derive(Serialize)]
struct SearchSuggestionsBody<'a> {
    context: RequestContext<'a>,
    input: &'a str,
}

#[derive(Serialize)]
struct AccountBody<'a> {
    context: RequestContext<'a>,
}

#[derive(Serialize)]
struct LikeBody<'a> {
    context: RequestContext<'a>,
    target: LikeTarget<'a>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct LikeTarget<'a> {
    #[serde(skip_serializing_if = "Option::is_none")]
    video_id: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    playlist_id: Option<&'a str>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SubscribeBody<'a> {
    context: RequestContext<'a>,
    channel_ids: [&'a str; 1],
    #[serde(skip_serializing_if = "Option::is_none")]
    params: Option<&'a str>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct CreatePlaylistBody<'a> {
    context: RequestContext<'a>,
    title: &'a str,
    privacy_status: &'static str,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct EditPlaylistBody<'a> {
    context: RequestContext<'a>,
    playlist_id: &'a str,
    actions: [EditPlaylistAction<'a>; 1],
}

#[derive(Serialize)]
#[serde(untagged)]
enum EditPlaylistAction<'a> {
    AddVideo {
        action: &'static str,
        #[serde(rename = "addedVideoId")]
        added_video_id: &'a str,
    },
    RemoveVideo {
        action: &'static str,
        #[serde(rename = "removedVideoId")]
        removed_video_id: &'a str,
        #[serde(rename = "setVideoId")]
        set_video_id: &'a str,
    },
    Rename {
        action: &'static str,
        #[serde(rename = "playlistName")]
        playlist_name: &'a str,
    },
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct DeletePlaylistBody<'a> {
    context: RequestContext<'a>,
    playlist_id: &'a str,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct FeedbackBody<'a> {
    context: RequestContext<'a>,
    feedback_tokens: [&'a str; 1],
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct FeedbackResponse {
    #[serde(default)]
    feedback_responses: Vec<FeedbackResult>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct FeedbackResult {
    is_processed: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct CreatePlaylistResponse {
    playlist_id: String,
}

#[derive(Serialize)]
struct ContinuationBody<'a> {
    context: RequestContext<'a>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct BrowseBody<'a> {
    context: RequestContext<'a>,
    browse_id: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    params: Option<&'a str>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct NextBody<'a> {
    context: RequestContext<'a>,
    #[serde(flatten)]
    endpoint: &'a RadioEndpoint,
    #[serde(skip_serializing_if = "Option::is_none")]
    continuation: Option<&'a str>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct GetQueueBody<'a> {
    context: RequestContext<'a>,
    video_ids: &'a [&'a str],
    #[serde(skip_serializing_if = "Option::is_none")]
    playlist_id: Option<&'a str>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ReturnYoutubeDislikeResponse {
    view_count: Option<u64>,
    likes: Option<u64>,
    dislikes: Option<u64>,
}

#[derive(Serialize)]
struct RequestContext<'a> {
    client: ClientContext<'a>,
    request: RequestMetadata,
    user: UserContext<'a>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ClientContext<'a> {
    client_name: &'static str,
    client_version: &'static str,
    #[serde(rename = "hl")]
    language: &'a str,
    #[serde(rename = "gl")]
    region: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    visitor_data: Option<&'a str>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct RequestMetadata {
    use_ssl: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct UserContext<'a> {
    locked_safety_mode: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    on_behalf_of_user: Option<&'a str>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct PlayerBody<'a> {
    context: PlayerRequestContext<'a>,
    video_id: &'a str,
}

#[derive(Serialize)]
struct PlayerRequestContext<'a> {
    client: PlayerClientContext<'a>,
    request: RequestMetadata,
    user: UserContext<'a>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct PlayerClientContext<'a> {
    client_name: &'static str,
    client_version: &'static str,
    #[serde(rename = "hl")]
    language: &'a str,
    #[serde(rename = "gl")]
    region: &'a str,
    os_name: &'static str,
    os_version: &'static str,
    device_make: &'static str,
    device_model: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    visitor_data: Option<&'a str>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PlayerResponse {
    playability_status: Option<PlayabilityStatus>,
    player_config: Option<PlayerConfig>,
    streaming_data: Option<StreamingData>,
    playback_tracking: Option<PlaybackTracking>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PlayerConfig {
    audio_config: Option<PlayerAudioConfig>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PlayerAudioConfig {
    loudness_db: Option<f64>,
    perceptual_loudness_db: Option<f64>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PlaybackTracking {
    videostats_playback_url: Option<PlaybackTrackingEndpoint>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PlaybackTrackingEndpoint {
    base_url: Option<String>,
}

#[derive(Deserialize)]
struct PlayabilityStatus {
    status: Option<String>,
    reason: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct StreamingData {
    expires_in_seconds: Option<String>,
    #[serde(default)]
    adaptive_formats: Vec<AdaptiveFormat>,
}

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AdaptiveFormat {
    mime_type: String,
    bitrate: Option<u64>,
    audio_quality: Option<String>,
    content_length: Option<String>,
    loudness_db: Option<f64>,
    url: Option<String>,
}

pub fn parse_player_response(json: impl AsRef<[u8]>) -> Result<ResolvedPlayback> {
    parse_player_response_with_quality(json, AudioQuality::Auto)
}

pub fn parse_player_response_with_quality(
    json: impl AsRef<[u8]>,
    audio_quality: AudioQuality,
) -> Result<ResolvedPlayback> {
    let response: PlayerResponse = serde_json::from_slice(json.as_ref())
        .map_err(|error| AppError::Protocol(error.to_string()))?;
    let playability = response.playability_status.unwrap_or(PlayabilityStatus {
        status: None,
        reason: None,
    });
    if playability.status.as_deref() != Some("OK") {
        return Err(AppError::Playback(
            playability
                .reason
                .unwrap_or_else(|| "the selected song is not playable".into()),
        ));
    }

    let streaming = response
        .streaming_data
        .ok_or_else(|| AppError::Playback("player response contained no streams".into()))?;
    let formats = streaming
        .adaptive_formats
        .into_iter()
        .filter(|format| {
            format.url.is_some()
                && format.mime_type.starts_with("audio/mp4")
                && format.mime_type.contains("mp4a.40.2")
        })
        .collect::<Vec<_>>();
    let format = select_audio_format(&formats, audio_quality)
        .cloned()
        .ok_or_else(|| {
            AppError::Playback("player returned no directly usable M4A/AAC-LC audio stream".into())
        })?;
    let audio_config = response
        .player_config
        .and_then(|config| config.audio_config);
    let loudness_lufs_mb = measured_loudness_lufs_mb(
        audio_config
            .as_ref()
            .and_then(|config| config.perceptual_loudness_db),
        audio_config
            .and_then(|config| config.loudness_db)
            .or(format.loudness_db),
    );

    Ok(ResolvedPlayback {
        source: PlaybackSource {
            url: format
                .url
                .expect("direct URL was checked while selecting format"),
            mime_type: format.mime_type,
            content_length: format
                .content_length
                .and_then(|length| length.parse::<u64>().ok()),
            loudness_lufs_mb,
            request_headers: playback_request_headers(),
            cache_key: None,
            access: PlaybackSourceAccess::NetworkAndCache,
        },
        expires_in: Duration::from_secs(
            streaming
                .expires_in_seconds
                .and_then(|seconds| seconds.parse().ok())
                .unwrap_or_default(),
        ),
        playback_tracking: response
            .playback_tracking
            .and_then(|tracking| tracking.videostats_playback_url)
            .and_then(|endpoint| endpoint.base_url)
            .and_then(|url| {
                validated_playback_tracking_url(&url)
                    .ok()
                    .map(|_| PlaybackTrackingUrl(url))
            }),
    })
}

fn measured_loudness_lufs_mb(
    perceptual_loudness_db: Option<f64>,
    relative_loudness_db: Option<f64>,
) -> Option<i32> {
    let measured_lufs = perceptual_loudness_db
        .or_else(|| relative_loudness_db.map(|loudness_db| loudness_db - 7.0))?;
    if !measured_lufs.is_finite() || !(-100.0..=20.0).contains(&measured_lufs) {
        return None;
    }
    Some((measured_lufs * 100.0) as i32)
}

fn select_audio_format(
    formats: &[AdaptiveFormat],
    audio_quality: AudioQuality,
) -> Option<&AdaptiveFormat> {
    match audio_quality {
        AudioQuality::Auto => formats
            .iter()
            .max_by_key(|format| format.bitrate.unwrap_or_default()),
        AudioQuality::High => formats.iter().max_by_key(|format| {
            (
                audio_quality_rank(format.audio_quality.as_deref()),
                format.bitrate.unwrap_or_default(),
            )
        }),
        AudioQuality::Low => formats
            .iter()
            .filter(|format| format.bitrate.is_some_and(|bitrate| bitrate <= 128_000))
            .max_by_key(|format| format.bitrate.unwrap_or_default())
            .or_else(|| {
                formats
                    .iter()
                    .min_by_key(|format| format.bitrate.unwrap_or(u64::MAX))
            }),
    }
}

fn audio_quality_rank(quality: Option<&str>) -> u8 {
    match quality {
        Some("AUDIO_QUALITY_HIGH") => 3,
        Some("AUDIO_QUALITY_MEDIUM") => 2,
        Some("AUDIO_QUALITY_LOW") => 1,
        _ => 0,
    }
}

fn playback_request_headers() -> Vec<(HeaderName, HeaderValue)> {
    vec![
        (
            HeaderName::from_static("user-agent"),
            HeaderValue::from_static(PLAYBACK_USER_AGENT),
        ),
        (
            HeaderName::from_static("accept"),
            HeaderValue::from_static("*/*"),
        ),
        (
            HeaderName::from_static("accept-language"),
            HeaderValue::from_static("en-US,en;q=0.9"),
        ),
        (
            HeaderName::from_static("origin"),
            HeaderValue::from_static("https://www.youtube.com"),
        ),
        (
            HeaderName::from_static("referer"),
            HeaderValue::from_static("https://www.youtube.com/"),
        ),
    ]
}

pub fn parse_account_info_response(json: impl AsRef<[u8]>) -> Result<AccountProfile> {
    let response: Value = serde_json::from_slice(json.as_ref())
        .map_err(|error| AppError::Protocol(error.to_string()))?;
    let header = find_value_named(&response, "activeAccountHeaderRenderer").ok_or_else(|| {
        AppError::SessionExpired("the imported session returned no active account".into())
    })?;
    let name = text_at(header, &["accountName"])
        .filter(|name| !name.is_empty())
        .ok_or_else(|| AppError::Protocol("the active account has no display name".into()))?;
    Ok(AccountProfile {
        name,
        email: text_at(header, &["email"]).filter(|value| !value.is_empty()),
        channel_handle: text_at(header, &["channelHandle"]).filter(|value| !value.is_empty()),
        thumbnail_url: header
            .get("accountPhoto")
            .and_then(thumbnail_url)
            .map(str::to_owned),
    })
}

pub fn parse_search_response(json: impl AsRef<[u8]>) -> Result<SearchResult> {
    let root: Value = serde_json::from_slice(json.as_ref())
        .map_err(|error| AppError::Protocol(error.to_string()))?;
    let mut renderers = Vec::new();
    collect_values_named(&root, "musicResponsiveListItemRenderer", &mut renderers);

    let mut songs = Vec::new();
    let mut items = Vec::new();
    for renderer in renderers {
        if let Some(song) = parse_catalog_song(renderer, &[])
            && !songs
                .iter()
                .any(|existing: &Song| existing.video_id == song.video_id)
        {
            songs.push(song);
        }
        if let Some(item) = parse_browse_item(renderer)
            && !items
                .iter()
                .any(|existing: &BrowseItem| existing.browse_id == item.browse_id)
        {
            items.push(item);
        }
    }
    let mut multi_row_renderers = Vec::new();
    collect_values_named(
        &root,
        "musicMultiRowListItemRenderer",
        &mut multi_row_renderers,
    );
    for renderer in multi_row_renderers {
        if let Some(song) = parse_compact_home_song(renderer)
            && !songs
                .iter()
                .any(|existing: &Song| existing.video_id == song.video_id)
        {
            songs.push(song);
        }
    }
    let mut two_row_renderers = Vec::new();
    collect_values_named(&root, "musicTwoRowItemRenderer", &mut two_row_renderers);
    for renderer in two_row_renderers {
        if let Some(item) = parse_browse_item(renderer)
            && !items
                .iter()
                .any(|existing: &BrowseItem| existing.browse_id == item.browse_id)
        {
            items.push(item);
        }
    }

    Ok(SearchResult {
        songs,
        items,
        continuation: find_continuation(&root).map(str::to_owned),
    })
}

pub fn parse_search_suggestions_response(json: impl AsRef<[u8]>) -> Result<SearchSuggestions> {
    let root: Value = serde_json::from_slice(json.as_ref())
        .map_err(|error| AppError::Protocol(error.to_string()))?;

    let mut suggestion_renderers = Vec::new();
    collect_values_named(&root, "searchSuggestionRenderer", &mut suggestion_renderers);
    let mut queries = Vec::new();
    for renderer in suggestion_renderers {
        let Some(query) = text_at(renderer, &["suggestion"])
            .map(|query| query.trim().to_owned())
            .filter(|query| !query.is_empty())
        else {
            continue;
        };
        if !queries.contains(&query) {
            queries.push(query);
        }
    }

    let mut responsive_renderers = Vec::new();
    collect_values_named(
        &root,
        "musicResponsiveListItemRenderer",
        &mut responsive_renderers,
    );
    let mut songs = Vec::new();
    let mut items = Vec::new();
    for renderer in responsive_renderers {
        if let Some(song) = parse_catalog_song(renderer, &[])
            && !songs
                .iter()
                .any(|existing: &Song| existing.video_id == song.video_id)
        {
            songs.push(song);
        }
        if let Some(item) = parse_browse_item(renderer)
            && !items
                .iter()
                .any(|existing: &BrowseItem| existing.browse_id == item.browse_id)
        {
            items.push(item);
        }
    }

    Ok(SearchSuggestions {
        queries,
        songs,
        items,
    })
}

fn parse_episode_search_response(json: impl AsRef<[u8]>) -> Result<SearchResult> {
    let root: Value = serde_json::from_slice(json.as_ref())
        .map_err(|error| AppError::Protocol(error.to_string()))?;
    let mut renderers = Vec::new();
    collect_values_named(&root, "musicResponsiveListItemRenderer", &mut renderers);
    let mut songs = Vec::new();
    for renderer in renderers
        .into_iter()
        .filter(|renderer| is_episode_renderer(renderer))
    {
        if let Some(song) = parse_catalog_song(renderer, &[])
            && !songs
                .iter()
                .any(|existing: &Song| existing.video_id == song.video_id)
        {
            songs.push(song);
        }
    }

    let mut multi_row_renderers = Vec::new();
    collect_values_named(
        &root,
        "musicMultiRowListItemRenderer",
        &mut multi_row_renderers,
    );
    for renderer in multi_row_renderers {
        if let Some(song) = parse_compact_home_song(renderer)
            && !songs
                .iter()
                .any(|existing: &Song| existing.video_id == song.video_id)
        {
            songs.push(song);
        }
    }

    Ok(SearchResult {
        songs,
        items: Vec::new(),
        // A fallback response is unfiltered; its continuation cannot safely retain episode-only
        // semantics without remembering the original filter at the API boundary.
        continuation: None,
    })
}

pub fn parse_search_continuation_response(json: impl AsRef<[u8]>) -> Result<SearchResult> {
    let mut result = parse_search_response(json)?;
    if result.songs.is_empty() && result.items.is_empty() {
        result.continuation = None;
    }
    Ok(result)
}

pub fn parse_history_response(json: impl AsRef<[u8]>) -> Result<RemoteHistoryPage> {
    let root: Value = serde_json::from_slice(json.as_ref())
        .map_err(|error| AppError::Protocol(error.to_string()))?;
    let mut shelf_renderers = Vec::new();
    collect_values_named(&root, "musicShelfRenderer", &mut shelf_renderers);
    let mut sections = Vec::new();
    for shelf in shelf_renderers {
        let title = text_at(shelf, &["title"])
            .filter(|title| !title.trim().is_empty())
            .unwrap_or_else(|| "History".into());
        let entries = parse_history_entries(shelf.get("contents").unwrap_or(shelf));
        if !entries.is_empty() {
            sections.push(RemoteHistorySection { title, entries });
        }
    }
    if sections.is_empty() {
        let entries = parse_history_entries(&root);
        if !entries.is_empty() {
            sections.push(RemoteHistorySection {
                title: "History".into(),
                entries,
            });
        }
    }
    let continuation = (!sections.is_empty())
        .then(|| find_continuation(&root).map(str::to_owned))
        .flatten();
    Ok(RemoteHistoryPage {
        sections,
        continuation,
    })
}

fn parse_history_entries(value: &Value) -> Vec<RemoteHistoryEntry> {
    let mut item_renderers = Vec::new();
    collect_values_named(
        value,
        "musicResponsiveListItemRenderer",
        &mut item_renderers,
    );
    let mut entries = Vec::new();
    for renderer in item_renderers {
        let Some(song) = parse_song(renderer, &[]) else {
            continue;
        };
        let feedback_token = history_feedback_token(renderer).map(str::to_owned);
        let duplicate = entries.iter().any(|existing: &RemoteHistoryEntry| {
            match (&existing.feedback_token, &feedback_token) {
                (Some(existing), Some(candidate)) => existing == candidate,
                (None, None) => existing.song.video_id == song.video_id,
                _ => false,
            }
        });
        if !duplicate {
            entries.push(RemoteHistoryEntry {
                song,
                feedback_token,
            });
        }
    }
    entries
}

fn parse_browse_tab_response(
    json: impl AsRef<[u8]>,
    requested: BrowseItem,
    tab_index: usize,
) -> Result<BrowsePage> {
    let root: Value = serde_json::from_slice(json.as_ref())
        .map_err(|error| AppError::Protocol(error.to_string()))?;
    let tabs = value_at(
        &root,
        &["contents", "singleColumnBrowseResultsRenderer", "tabs"],
    )
    .or_else(|| {
        value_at(
            &root,
            &["contents", "twoColumnBrowseResultsRenderer", "tabs"],
        )
    })
    .and_then(Value::as_array)
    .ok_or_else(|| AppError::Protocol("YouTube Music library tabs are missing".into()))?;
    let tab = tabs.get(tab_index).ok_or_else(|| {
        AppError::Protocol(format!(
            "YouTube Music library tab {tab_index} is unavailable"
        ))
    })?;
    let content = value_at(tab, &["tabRenderer", "content"]).unwrap_or(tab);
    let scoped = serde_json::json!({
        "header": root.get("header").cloned().unwrap_or(Value::Null),
        "contents": content,
    });
    let scoped =
        serde_json::to_vec(&scoped).map_err(|error| AppError::Protocol(error.to_string()))?;
    parse_browse_response(scoped, requested)
}

fn album_playlist_id(root: &Value, header: Option<&Value>) -> Option<String> {
    value_at(
        root,
        &["microformat", "microformatDataRenderer", "urlCanonical"],
    )
    .and_then(Value::as_str)
    .and_then(|url| {
        url.split('?').nth(1)?.split('&').find_map(|pair| {
            pair.strip_prefix("list=")
                .filter(|id| !id.trim().is_empty())
                .map(str::to_owned)
        })
    })
    .or_else(|| {
        header
            .and_then(|header| find_value_named(header, "watchPlaylistEndpoint"))
            .and_then(|endpoint| endpoint.get("playlistId"))
            .and_then(Value::as_str)
            .filter(|id| !id.trim().is_empty())
            .map(str::to_owned)
    })
}

fn browse_playback_endpoint(value: &Value) -> Option<BrowsePlaybackEndpoint> {
    let string_field = |name: &str| {
        value
            .get(name)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_owned)
    };
    let video_id = string_field("videoId");
    let playlist_id = string_field("playlistId");
    if video_id.is_none() && playlist_id.is_none() {
        return None;
    }
    Some(BrowsePlaybackEndpoint {
        video_id,
        playlist_id,
        playlist_set_video_id: string_field("playlistSetVideoId"),
        params: string_field("params"),
        index: value
            .get("index")
            .and_then(Value::as_u64)
            .and_then(|index| u32::try_from(index).ok()),
    })
}

fn artist_shuffle_endpoint(root: &Value, header: Option<&Value>) -> Option<BrowsePlaybackEndpoint> {
    header
        .and_then(|header| {
            value_at(
                header,
                &[
                    "playButton",
                    "buttonRenderer",
                    "navigationEndpoint",
                    "watchEndpoint",
                ],
            )
        })
        .and_then(browse_playback_endpoint)
        .or_else(|| {
            let shelf = find_value_named(root, "musicShelfRenderer")?;
            let first_song = find_value_named(shelf, "musicResponsiveListItemRenderer")?;
            value_at(first_song, &["navigationEndpoint", "watchPlaylistEndpoint"])
                .and_then(browse_playback_endpoint)
        })
}

fn artist_radio_endpoint(header: Option<&Value>) -> Option<BrowsePlaybackEndpoint> {
    header
        .and_then(|header| {
            value_at(
                header,
                &[
                    "startRadioButton",
                    "buttonRenderer",
                    "navigationEndpoint",
                    "watchEndpoint",
                ],
            )
        })
        .and_then(browse_playback_endpoint)
}

fn playlist_menu_endpoint(
    header: Option<&Value>,
    icon_type: &str,
) -> Option<BrowsePlaybackEndpoint> {
    let mut items = Vec::new();
    collect_values_named(header?, "menuNavigationItemRenderer", &mut items);
    items.into_iter().find_map(|item| {
        (value_at(item, &["icon", "iconType"]).and_then(Value::as_str) == Some(icon_type))
            .then(|| {
                value_at(item, &["navigationEndpoint", "watchPlaylistEndpoint"])
                    .and_then(browse_playback_endpoint)
            })
            .flatten()
    })
}

fn artist_section_link(title: String, browse: &Value) -> Option<BrowseItem> {
    let title = title.trim();
    let browse_id = browse.get("browseId")?.as_str()?.trim();
    if title.is_empty() || browse_id.is_empty() {
        return None;
    }
    Some(BrowseItem {
        browse_id: browse_id.to_owned(),
        kind: BrowseKind::Category,
        title: title.to_owned(),
        subtitle: format!("View all {title}"),
        thumbnail_url: None,
        params: browse
            .get("params")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|params| !params.is_empty())
            .map(str::to_owned),
        editable: false,
    })
}

fn artist_section_links(root: &Value) -> Vec<BrowseItem> {
    let mut links = Vec::new();
    let mut shelves = Vec::new();
    collect_values_named(root, "musicShelfRenderer", &mut shelves);
    for shelf in shelves {
        let link = text_at(shelf, &["title"]).and_then(|title| {
            let first_title_run = value_at(shelf, &["title", "runs"])
                .and_then(Value::as_array)
                .and_then(|runs| runs.first())?;
            let browse = value_at(first_title_run, &["navigationEndpoint", "browseEndpoint"])?;
            artist_section_link(title, browse)
        });
        if let Some(link) = link
            && !links.iter().any(|existing: &BrowseItem| {
                existing.browse_id == link.browse_id && existing.params == link.params
            })
        {
            links.push(link);
        }
    }

    let mut carousels = Vec::new();
    collect_values_named(root, "musicCarouselShelfRenderer", &mut carousels);
    for carousel in carousels {
        let link = value_at(
            carousel,
            &["header", "musicCarouselShelfBasicHeaderRenderer"],
        )
        .and_then(|header| {
            let title = text_at(header, &["title"])?;
            let browse = value_at(
                header,
                &[
                    "moreContentButton",
                    "buttonRenderer",
                    "navigationEndpoint",
                    "browseEndpoint",
                ],
            )?;
            artist_section_link(title, browse)
        });
        if let Some(link) = link
            && !links.iter().any(|existing| {
                existing.browse_id == link.browse_id && existing.params == link.params
            })
        {
            links.push(link);
        }
    }
    links
}

pub fn parse_browse_response(json: impl AsRef<[u8]>, requested: BrowseItem) -> Result<BrowsePage> {
    let root: Value = serde_json::from_slice(json.as_ref())
        .map_err(|error| AppError::Protocol(error.to_string()))?;
    let header = [
        "musicResponsiveHeaderRenderer",
        "musicImmersiveHeaderRenderer",
        "musicVisualHeaderRenderer",
        "musicDetailHeaderRenderer",
        "musicHeaderRenderer",
    ]
    .into_iter()
    .find_map(|name| find_value_named(&root, name));

    let mut item = requested;
    if let Some(header) = header {
        if let Some(title) = text_at(header, &["title"]).filter(|title| !title.is_empty()) {
            item.title = title;
        }
        let subtitle = ["straplineTextOne", "subtitle", "secondSubtitle"]
            .into_iter()
            .filter_map(|field| text_at(header, &[field]))
            .filter(|text| !text.is_empty())
            .collect::<Vec<_>>();
        if !subtitle.is_empty() {
            item.subtitle = subtitle.join(" · ");
        }
        if let Some(thumbnail) = thumbnail_url(header) {
            item.thumbnail_url = Some(thumbnail.to_owned());
        }
    }

    let fallback_artists = header.map(extract_artist_credits).unwrap_or_default();
    let mut creator_links = Vec::new();
    if matches!(item.kind, BrowseKind::Album | BrowseKind::Playlist) {
        for artist in &fallback_artists {
            let Some(browse_id) = artist.id.as_ref().filter(|id| !id.trim().is_empty()) else {
                continue;
            };
            if creator_links
                .iter()
                .any(|existing: &BrowseItem| existing.browse_id == *browse_id)
            {
                continue;
            }
            creator_links.push(BrowseItem {
                browse_id: browse_id.clone(),
                kind: BrowseKind::Artist,
                title: artist.name.clone(),
                subtitle: if item.kind == BrowseKind::Playlist {
                    "Playlist author".into()
                } else {
                    "Album artist".into()
                },
                thumbnail_url: None,
                params: None,
                editable: false,
            });
        }
    }
    let album_credit = (item.kind == BrowseKind::Album).then(|| AlbumCredit {
        browse_id: item.browse_id.clone(),
        title: item.title.clone(),
        thumbnail_url: item.thumbnail_url.clone(),
    });
    let mut renderers = Vec::new();
    collect_values_named(&root, "musicResponsiveListItemRenderer", &mut renderers);
    let mut songs = Vec::new();
    let mut playlist_entries = Vec::new();
    let mut related = Vec::new();
    for renderer in renderers {
        if let Some(mut song) = parse_catalog_song(renderer, &fallback_artists) {
            if song.thumbnail_url.is_none() {
                song.thumbnail_url.clone_from(&item.thumbnail_url);
            }
            if song.album.is_none() {
                song.album.clone_from(&album_credit);
            }
            if let Some(set_video_id) = playlist_set_video_id(renderer)
                && !playlist_entries
                    .iter()
                    .any(|entry: &PlaylistEntry| entry.set_video_id == set_video_id)
            {
                playlist_entries.push(PlaylistEntry {
                    song: song.clone(),
                    set_video_id: set_video_id.to_owned(),
                });
            }
            if !songs
                .iter()
                .any(|existing: &Song| existing.video_id == song.video_id)
            {
                songs.push(song);
            }
        }
        if let Some(related_item) = parse_browse_item(renderer)
            && related_item.browse_id != item.browse_id
            && !related
                .iter()
                .any(|existing: &BrowseItem| existing.browse_id == related_item.browse_id)
        {
            related.push(related_item);
        }
    }

    if item.kind == BrowseKind::Podcast {
        let mut episode_renderers = Vec::new();
        collect_values_named(
            &root,
            "musicMultiRowListItemRenderer",
            &mut episode_renderers,
        );
        for renderer in episode_renderers {
            let Some(mut episode) = parse_compact_home_song(renderer) else {
                continue;
            };
            if !fallback_artists.is_empty() {
                episode.artists.clone_from(&fallback_artists);
            }
            if episode.thumbnail_url.is_none() {
                episode.thumbnail_url.clone_from(&item.thumbnail_url);
            }
            if !songs
                .iter()
                .any(|existing: &Song| existing.video_id == episode.video_id)
            {
                songs.push(episode);
            }
        }
        for song in &mut songs {
            song.is_episode = true;
        }
        for entry in &mut playlist_entries {
            entry.song.is_episode = true;
        }
    }

    let mut related_renderers = Vec::new();
    collect_values_named(&root, "musicTwoRowItemRenderer", &mut related_renderers);
    for renderer in related_renderers {
        let Some(related_item) = parse_browse_item(renderer) else {
            continue;
        };
        if related_item.browse_id != item.browse_id
            && !related
                .iter()
                .any(|existing: &BrowseItem| existing.browse_id == related_item.browse_id)
        {
            related.push(related_item);
        }
    }

    let description = find_value_named(&root, "musicDescriptionShelfRenderer")
        .and_then(|renderer| text_at(renderer, &["description"]))
        .or_else(|| header.and_then(|header| text_at(header, &["description"])))
        .filter(|description| !description.is_empty());
    let subscriber_count = (item.kind == BrowseKind::Artist)
        .then(|| {
            let header = header?;
            text_at(
                header,
                &[
                    "subscriptionButton2",
                    "subscribeButtonRenderer",
                    "subscriberCountWithSubscribeText",
                ],
            )
            .or_else(|| {
                text_at(
                    header,
                    &[
                        "subscriptionButton",
                        "subscribeButtonRenderer",
                        "longSubscriberCountText",
                    ],
                )
            })
            .or_else(|| {
                text_at(
                    header,
                    &[
                        "subscriptionButton",
                        "subscribeButtonRenderer",
                        "shortSubscriberCountText",
                    ],
                )
            })
            .filter(|text| !text.is_empty())
        })
        .flatten();
    let monthly_listener_count = (item.kind == BrowseKind::Artist)
        .then(|| {
            header
                .and_then(|header| text_at(header, &["monthlyListenerCount"]))
                .filter(|text| !text.is_empty())
        })
        .flatten();

    if item.kind == BrowseKind::Playlist
        && find_value_named(&root, "musicEditablePlaylistDetailHeaderRenderer").is_some()
    {
        item.editable = true;
    }
    let channel_subscription = header.and_then(parse_channel_subscription);
    let playlist_id = (item.kind == BrowseKind::Album)
        .then(|| album_playlist_id(&root, header))
        .flatten();
    let shuffle_endpoint = match item.kind {
        BrowseKind::Artist => artist_shuffle_endpoint(&root, header),
        BrowseKind::Playlist => playlist_menu_endpoint(header, "MUSIC_SHUFFLE"),
        BrowseKind::Album | BrowseKind::Podcast | BrowseKind::Category => None,
    };
    let radio_endpoint = match item.kind {
        BrowseKind::Artist => artist_radio_endpoint(header),
        BrowseKind::Playlist => playlist_menu_endpoint(header, "MIX"),
        BrowseKind::Album | BrowseKind::Podcast | BrowseKind::Category => None,
    };
    let section_links = if item.kind == BrowseKind::Artist {
        artist_section_links(&root)
    } else {
        Vec::new()
    };

    Ok(BrowsePage {
        item,
        playlist_id,
        shuffle_endpoint,
        radio_endpoint,
        description,
        subscriber_count,
        monthly_listener_count,
        songs,
        playlist_entries,
        related,
        section_links,
        creator_links,
        channel_subscription,
        continuation: find_continuation(&root).map(str::to_owned),
    })
}

pub fn parse_browse_continuation_response(json: impl AsRef<[u8]>) -> Result<BrowseContinuation> {
    let root: Value = serde_json::from_slice(json.as_ref())
        .map_err(|error| AppError::Protocol(error.to_string()))?;
    let mut renderers = Vec::new();
    collect_values_named(&root, "musicResponsiveListItemRenderer", &mut renderers);
    let mut songs = Vec::new();
    let mut playlist_entries = Vec::new();
    let mut items = Vec::new();
    for renderer in renderers {
        if let Some(song) = parse_catalog_song(renderer, &[]) {
            if let Some(set_video_id) = playlist_set_video_id(renderer)
                && !playlist_entries
                    .iter()
                    .any(|entry: &PlaylistEntry| entry.set_video_id == set_video_id)
            {
                playlist_entries.push(PlaylistEntry {
                    song: song.clone(),
                    set_video_id: set_video_id.to_owned(),
                });
            }
            if !songs
                .iter()
                .any(|existing: &Song| existing.video_id == song.video_id)
            {
                songs.push(song);
            }
        }
        if let Some(item) = parse_browse_item(renderer)
            && !items
                .iter()
                .any(|existing: &BrowseItem| existing.browse_id == item.browse_id)
        {
            items.push(item);
        }
    }
    let mut multi_row_renderers = Vec::new();
    collect_values_named(
        &root,
        "musicMultiRowListItemRenderer",
        &mut multi_row_renderers,
    );
    for renderer in multi_row_renderers {
        if let Some(song) = parse_compact_home_song(renderer)
            && !songs
                .iter()
                .any(|existing: &Song| existing.video_id == song.video_id)
        {
            songs.push(song);
        }
    }
    let mut two_row_renderers = Vec::new();
    collect_values_named(&root, "musicTwoRowItemRenderer", &mut two_row_renderers);
    for renderer in two_row_renderers {
        if let Some(item) = parse_browse_item(renderer)
            && !items
                .iter()
                .any(|existing: &BrowseItem| existing.browse_id == item.browse_id)
        {
            items.push(item);
        }
    }

    let continuation = (!(songs.is_empty() && items.is_empty()))
        .then(|| find_continuation(&root).map(str::to_owned))
        .flatten();
    Ok(BrowseContinuation {
        songs,
        playlist_entries,
        items,
        continuation,
    })
}

fn parse_radio_response(
    json: impl AsRef<[u8]>,
    requested_endpoint: RadioEndpoint,
) -> Result<ParsedRadioPage> {
    let root: Value = serde_json::from_slice(json.as_ref())
        .map_err(|error| AppError::Protocol(error.to_string()))?;
    let panel = find_value_named(&root, "playlistPanelContinuation")
        .or_else(|| find_value_named(&root, "playlistPanelRenderer"))
        .ok_or_else(|| AppError::Protocol("next response contained no playlist panel".into()))?;

    let mut renderers = Vec::new();
    collect_values_named(panel, "playlistPanelVideoRenderer", &mut renderers);
    let mut songs = Vec::new();
    let mut current_index = None;
    for renderer in renderers {
        let selected = renderer
            .get("selected")
            .and_then(Value::as_bool)
            .unwrap_or_default();
        let Some(song) = parse_playlist_panel_song(renderer) else {
            continue;
        };
        if let Some(existing) = songs
            .iter()
            .position(|existing: &Song| existing.video_id == song.video_id)
        {
            if selected {
                current_index = Some(existing);
            }
            continue;
        }
        if selected {
            current_index = Some(songs.len());
        }
        songs.push(song);
    }

    let title = find_value_named(&root, "musicQueueHeaderRenderer")
        .and_then(|header| text_at(header, &["subtitle"]).or_else(|| text_at(header, &["title"])))
        .filter(|title| !title.is_empty());
    let automix_endpoint = find_value_named(panel, "automixPreviewVideoRenderer")
        .and_then(|automix| {
            find_value_named(automix, "watchPlaylistEndpoint")
                .or_else(|| find_value_named(automix, "watchEndpoint"))
        })
        .and_then(parse_radio_endpoint);
    let related_endpoint = parse_related_endpoint(&root);
    let continuation = (!songs.is_empty())
        .then(|| find_continuation(panel).map(str::to_owned))
        .flatten();
    Ok(ParsedRadioPage {
        page: RadioPage {
            title,
            songs,
            current_index,
            continuation,
            endpoint: requested_endpoint,
            related_endpoint,
        },
        automix_endpoint,
    })
}

fn parse_media_info_response(json: impl AsRef<[u8]>, video_id: &str) -> Result<MediaInfo> {
    let root: Value = serde_json::from_slice(json.as_ref())
        .map_err(|error| AppError::Protocol(error.to_string()))?;
    let primary = find_value_named(&root, "videoPrimaryInfoRenderer");
    let secondary = find_value_named(&root, "videoSecondaryInfoRenderer");
    let owner = secondary.and_then(|secondary| find_value_named(secondary, "videoOwnerRenderer"));
    Ok(MediaInfo {
        video_id: video_id.to_owned(),
        title: primary.and_then(|primary| text_at(primary, &["title"])),
        author: owner.and_then(|owner| text_at(owner, &["title"])),
        author_id: owner
            .and_then(|owner| {
                value_at(owner, &["navigationEndpoint", "browseEndpoint", "browseId"])
            })
            .and_then(Value::as_str)
            .map(str::to_owned),
        description: secondary
            .and_then(|secondary| value_at(secondary, &["attributedDescription", "content"]))
            .and_then(Value::as_str)
            .map(str::to_owned)
            .or_else(|| secondary.and_then(|secondary| text_at(secondary, &["description"]))),
        upload_date: primary.and_then(|primary| text_at(primary, &["dateText"])),
        subscribers: owner
            .and_then(|owner| text_at(owner, &["subscriberCountText"]))
            .and_then(|subscribers| subscribers.split_whitespace().next().map(str::to_owned)),
        view_count: None,
        likes: None,
        dislikes: None,
    })
}

fn parse_song_library_feedback_token(
    json: impl AsRef<[u8]>,
    video_id: &str,
    in_library: bool,
) -> Result<String> {
    let root: Value = serde_json::from_slice(json.as_ref())
        .map_err(|error| AppError::Protocol(error.to_string()))?;
    let mut renderers = Vec::new();
    collect_values_named(&root, "playlistPanelVideoRenderer", &mut renderers);
    let renderer = renderers
        .into_iter()
        .find(|renderer| {
            renderer.get("videoId").and_then(Value::as_str) == Some(video_id)
                || value_at(
                    renderer,
                    &["navigationEndpoint", "watchEndpoint", "videoId"],
                )
                .and_then(Value::as_str)
                    == Some(video_id)
        })
        .ok_or_else(|| {
            AppError::Protocol(
                "YouTube Music next response omitted the requested library song".into(),
            )
        })?;

    let mut toggles = Vec::new();
    collect_values_named(renderer, "toggleMenuServiceItemRenderer", &mut toggles);
    for toggle in toggles {
        let icon = value_at(toggle, &["defaultIcon", "iconType"])
            .and_then(Value::as_str)
            .unwrap_or_default();
        let add_state = matches!(icon, "LIBRARY_ADD" | "BOOKMARK_BORDER");
        let saved_state = matches!(icon, "LIBRARY_SAVED" | "BOOKMARK" | "LIBRARY_REMOVE");
        if !add_state && !saved_state {
            continue;
        }
        let endpoint = match (in_library, add_state) {
            (true, true) | (false, false) => "defaultServiceEndpoint",
            (true, false) | (false, true) => "toggledServiceEndpoint",
        };
        if let Some(token) = value_at(toggle, &[endpoint, "feedbackEndpoint", "feedbackToken"])
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|token| !token.is_empty())
        {
            return Ok(token.to_owned());
        }
    }

    Err(AppError::Protocol(if in_library {
        "YouTube Music did not provide a fresh add-to-library token".into()
    } else {
        "YouTube Music did not provide a fresh remove-from-library token".into()
    }))
}

fn parse_queue_song_response(json: impl AsRef<[u8]>, video_id: &str) -> Result<Song> {
    let root: Value = serde_json::from_slice(json.as_ref())
        .map_err(|error| AppError::Protocol(error.to_string()))?;
    let mut renderers = Vec::new();
    collect_values_named(&root, "playlistPanelVideoRenderer", &mut renderers);
    renderers
        .into_iter()
        .filter_map(parse_playlist_panel_song)
        .find(|song| song.video_id == video_id)
        .ok_or_else(|| AppError::Protocol("YouTube Music queue omitted the requested song".into()))
}

fn parse_playlist_panel_song(renderer: &Value) -> Option<Song> {
    let video_id = renderer
        .get("videoId")
        .and_then(Value::as_str)
        .or_else(|| {
            value_at(
                renderer,
                &["navigationEndpoint", "watchEndpoint", "videoId"],
            )
            .and_then(Value::as_str)
        })?
        .trim();
    let title = text_at(renderer, &["title"])?;
    if video_id.is_empty() || title.is_empty() {
        return None;
    }
    let byline = renderer
        .get("longBylineText")
        .or_else(|| renderer.get("shortBylineText"));
    let mut artists = byline.map(extract_artist_credits).unwrap_or_default();
    let byline_runs = byline
        .and_then(|byline| byline.get("runs"))
        .and_then(Value::as_array)
        .map(|runs| runs.iter().collect::<Vec<_>>())
        .unwrap_or_default();
    if artists.is_empty() {
        artists = extract_artists(&byline_runs);
    }
    if artists.is_empty() {
        return None;
    }
    let thumbnail_url = thumbnail_url(renderer).map(str::to_owned);
    let album = extract_album_credit(&byline_runs, thumbnail_url.as_deref());
    Some(Song {
        video_id: video_id.to_owned(),
        title,
        artists,
        duration: text_at(renderer, &["lengthText"])
            .as_deref()
            .and_then(parse_duration),
        thumbnail_url,
        album,
        is_episode: is_episode_renderer(renderer),
    })
}

fn parse_radio_endpoint(value: &Value) -> Option<RadioEndpoint> {
    let endpoint = serde_json::from_value::<RadioEndpoint>(value.clone()).ok()?;
    (endpoint.video_id.is_some() || endpoint.playlist_id.is_some()).then_some(endpoint)
}

fn parse_related_endpoint(root: &Value) -> Option<RelatedEndpoint> {
    let tabs = find_value_named(root, "watchNextTabbedResultsRenderer")?
        .get("tabs")?
        .as_array()?;
    let browse = value_at(tabs.get(2)?, &["tabRenderer", "endpoint", "browseEndpoint"])?;
    Some(RelatedEndpoint {
        browse_id: browse.get("browseId")?.as_str()?.to_owned(),
        params: browse
            .get("params")
            .and_then(Value::as_str)
            .map(str::to_owned),
    })
}

fn parse_related_songs(json: impl AsRef<[u8]>) -> Result<Vec<Song>> {
    let root: Value = serde_json::from_slice(json.as_ref())
        .map_err(|error| AppError::Protocol(error.to_string()))?;
    let mut songs = Vec::new();
    let mut responsive = Vec::new();
    collect_values_named(&root, "musicResponsiveListItemRenderer", &mut responsive);
    for renderer in responsive {
        if let Some(song) = parse_song(renderer, &[])
            && !songs
                .iter()
                .any(|existing: &Song| existing.video_id == song.video_id)
        {
            songs.push(song);
        }
    }
    let mut two_row = Vec::new();
    collect_values_named(&root, "musicTwoRowItemRenderer", &mut two_row);
    for renderer in two_row {
        if let Some(song) = parse_compact_home_song(renderer)
            && !songs
                .iter()
                .any(|existing: &Song| existing.video_id == song.video_id)
        {
            songs.push(song);
        }
    }
    Ok(songs)
}

pub fn parse_home_response(json: impl AsRef<[u8]>) -> Result<HomePage> {
    let root: Value = serde_json::from_slice(json.as_ref())
        .map_err(|error| AppError::Protocol(error.to_string()))?;

    let mut chip_renderers = Vec::new();
    collect_values_named(&root, "chipCloudChipRenderer", &mut chip_renderers);
    let mut chips = Vec::new();
    for renderer in chip_renderers {
        let Some(title) = text_at(renderer, &["text"]).filter(|title| !title.is_empty()) else {
            continue;
        };
        let params = value_at(
            renderer,
            &["navigationEndpoint", "browseEndpoint", "params"],
        )
        .and_then(Value::as_str)
        .map(str::to_owned);
        if !chips
            .iter()
            .any(|chip: &HomeChip| chip.title == title && chip.params == params)
        {
            chips.push(HomeChip { title, params });
        }
    }

    let mut shelf_renderers = Vec::new();
    collect_values_named(&root, "musicCarouselShelfRenderer", &mut shelf_renderers);
    let sections = shelf_renderers
        .into_iter()
        .filter_map(parse_home_section)
        .collect::<Vec<_>>();
    let continuation = (!sections.is_empty())
        .then(|| find_continuation(&root).map(str::to_owned))
        .flatten();
    Ok(HomePage {
        chips,
        sections,
        continuation,
    })
}

pub fn parse_explore_response(json: impl AsRef<[u8]>) -> Result<ExplorePage> {
    let root: Value = serde_json::from_slice(json.as_ref())
        .map_err(|error| AppError::Protocol(error.to_string()))?;
    let mut shelf_renderers = Vec::new();
    collect_values_named(&root, "musicCarouselShelfRenderer", &mut shelf_renderers);
    let mut new_release_albums = Vec::new();
    let mut new_releases_more = None;
    let mut categories = Vec::new();
    for shelf in shelf_renderers {
        let more_browse = value_at(
            shelf,
            &[
                "header",
                "musicCarouselShelfBasicHeaderRenderer",
                "moreContentButton",
                "buttonRenderer",
                "navigationEndpoint",
                "browseEndpoint",
            ],
        );
        let more_browse_id = more_browse
            .and_then(|browse| browse.get("browseId"))
            .and_then(Value::as_str);
        let contents = shelf
            .get("contents")
            .and_then(Value::as_array)
            .into_iter()
            .flatten();
        match more_browse_id {
            Some("FEmusic_new_releases_albums") => {
                new_releases_more = Some(BrowseItem {
                    browse_id: "FEmusic_new_releases_albums".into(),
                    kind: BrowseKind::Category,
                    title: "New releases".into(),
                    subtitle: "YouTube Music".into(),
                    thumbnail_url: None,
                    params: more_browse
                        .and_then(|browse| browse.get("params"))
                        .and_then(Value::as_str)
                        .map(str::to_owned),
                    editable: false,
                });
                for content in contents {
                    let Some(renderer) = content.get("musicTwoRowItemRenderer") else {
                        continue;
                    };
                    if let Some(item) = parse_browse_item(renderer)
                        && item.kind == BrowseKind::Album
                        && !new_release_albums
                            .iter()
                            .any(|existing: &BrowseItem| existing.browse_id == item.browse_id)
                    {
                        new_release_albums.push(item);
                    }
                }
            }
            Some("FEmusic_moods_and_genres") => {
                for content in contents {
                    let Some(renderer) = content.get("musicNavigationButtonRenderer") else {
                        continue;
                    };
                    if let Some(category) = parse_explore_category(renderer)
                        && !categories.iter().any(|existing: &ExploreCategory| {
                            existing.browse_id == category.browse_id
                                && existing.params == category.params
                        })
                    {
                        categories.push(category);
                    }
                }
            }
            _ => {}
        }
    }
    Ok(ExplorePage {
        chart_sections: Vec::new(),
        new_release_albums,
        new_releases_more,
        categories,
    })
}

fn parse_home_section(shelf: &Value) -> Option<HomeSection> {
    let header = value_at(shelf, &["header", "musicCarouselShelfBasicHeaderRenderer"])?;
    let title = text_at(header, &["title"])?.trim().to_owned();
    if title.is_empty() {
        return None;
    }
    let mut items = Vec::new();
    for content in shelf.get("contents")?.as_array()? {
        let item = content
            .get("musicResponsiveListItemRenderer")
            .and_then(|renderer| parse_song(renderer, &[]).map(HomeItem::Song))
            .or_else(|| {
                content
                    .get("musicTwoRowItemRenderer")
                    .and_then(parse_two_row_home_item)
            })
            .or_else(|| {
                content
                    .get("musicMultiRowListItemRenderer")
                    .and_then(parse_compact_home_song)
                    .map(HomeItem::Song)
            });
        if let Some(item) = item {
            let duplicate = items
                .iter()
                .any(|existing: &HomeItem| match (existing, &item) {
                    (HomeItem::Song(left), HomeItem::Song(right)) => {
                        left.video_id == right.video_id
                    }
                    (HomeItem::Browse(left), HomeItem::Browse(right)) => {
                        left.browse_id == right.browse_id
                    }
                    _ => false,
                });
            if !duplicate {
                items.push(item);
            }
        }
    }
    if items.is_empty() {
        return None;
    }

    let more = value_at(
        header,
        &[
            "moreContentButton",
            "buttonRenderer",
            "navigationEndpoint",
            "browseEndpoint",
        ],
    )
    .and_then(|browse| browse_item_from_endpoint(browse, &title));
    Some(HomeSection {
        title,
        label: text_at(header, &["strapline"]).filter(|label| !label.is_empty()),
        thumbnail_url: thumbnail_url(header).map(str::to_owned),
        more,
        items,
    })
}

fn parse_two_row_home_item(renderer: &Value) -> Option<HomeItem> {
    parse_compact_home_song(renderer)
        .map(HomeItem::Song)
        .or_else(|| parse_browse_item(renderer).map(HomeItem::Browse))
}

fn parse_compact_home_song(renderer: &Value) -> Option<Song> {
    let video_id = value_at(
        renderer,
        &["navigationEndpoint", "watchEndpoint", "videoId"],
    )
    .or_else(|| value_at(renderer, &["onTap", "watchEndpoint", "videoId"]))
    .or_else(|| {
        value_at(
            renderer,
            &[
                "thumbnailOverlay",
                "musicItemThumbnailOverlayRenderer",
                "content",
                "musicPlayButtonRenderer",
                "playNavigationEndpoint",
                "watchEndpoint",
                "videoId",
            ],
        )
    })
    .and_then(Value::as_str)?
    .trim();
    if video_id.is_empty() {
        return None;
    }
    let title = text_at(renderer, &["title"])?;
    if title.is_empty() {
        return None;
    }
    let subtitle = renderer.get("subtitle");
    let mut artists = subtitle.map(extract_artist_credits).unwrap_or_default();
    let subtitle_runs = subtitle
        .and_then(|subtitle| subtitle.get("runs"))
        .and_then(Value::as_array)
        .map(|runs| runs.iter().collect::<Vec<_>>())
        .unwrap_or_default();
    if artists.is_empty() {
        artists = extract_artists(&subtitle_runs);
    }
    let duration = subtitle_runs
        .iter()
        .filter_map(|run| run.get("text").and_then(Value::as_str))
        .find_map(parse_duration);
    let thumbnail_url = thumbnail_url(renderer).map(str::to_owned);
    let album = extract_album_credit(&subtitle_runs, thumbnail_url.as_deref());
    Some(Song {
        video_id: video_id.to_owned(),
        title,
        artists,
        duration,
        thumbnail_url,
        album,
        is_episode: true,
    })
}

fn parse_explore_category(renderer: &Value) -> Option<ExploreCategory> {
    let title = text_at(renderer, &["buttonText"])?.trim().to_owned();
    let browse = value_at(renderer, &["clickCommand", "browseEndpoint"])?;
    let browse_id = browse.get("browseId")?.as_str()?.trim().to_owned();
    if title.is_empty() || browse_id.is_empty() {
        return None;
    }
    Some(ExploreCategory {
        title,
        browse_id,
        params: browse
            .get("params")
            .and_then(Value::as_str)
            .map(str::to_owned),
        stripe_color: value_at(renderer, &["solid", "leftStripeColor"])
            .and_then(Value::as_u64)
            .and_then(|color| u32::try_from(color).ok()),
    })
}

fn browse_item_from_endpoint(browse: &Value, title: &str) -> Option<BrowseItem> {
    let browse_id = browse.get("browseId")?.as_str()?.trim();
    if browse_id.is_empty() {
        return None;
    }
    let kind = browse_kind(browse_id, browse)?;
    Some(BrowseItem {
        browse_id: browse_id.to_owned(),
        kind,
        title: title.to_owned(),
        subtitle: kind.label().to_owned(),
        thumbnail_url: None,
        params: browse
            .get("params")
            .and_then(Value::as_str)
            .map(str::to_owned),
        editable: false,
    })
}

fn parse_song(renderer: &Value, fallback_artists: &[ArtistCredit]) -> Option<Song> {
    let columns = renderer.get("flexColumns")?.as_array()?;
    let title_run = runs_from_column(columns.first()?)?.first()?;
    let title = title_run.get("text")?.as_str()?.trim();
    if title.is_empty() {
        return None;
    }

    let video_id = renderer
        .get("videoId")
        .and_then(Value::as_str)
        .or_else(|| value_at(renderer, &["playlistItemData", "videoId"]).and_then(Value::as_str))
        .or_else(|| {
            value_at(
                renderer,
                &["navigationEndpoint", "watchEndpoint", "videoId"],
            )
            .and_then(Value::as_str)
        })
        .or_else(|| {
            value_at(
                title_run,
                &["navigationEndpoint", "watchEndpoint", "videoId"],
            )
            .and_then(Value::as_str)
        })
        .or_else(|| {
            value_at(
                renderer,
                &[
                    "overlay",
                    "musicItemThumbnailOverlayRenderer",
                    "content",
                    "musicPlayButtonRenderer",
                    "playNavigationEndpoint",
                    "watchEndpoint",
                    "videoId",
                ],
            )
            .and_then(Value::as_str)
        })?;

    let metadata_runs = columns
        .iter()
        .skip(1)
        .filter_map(runs_from_column)
        .flatten()
        .collect::<Vec<_>>();
    let artists = extract_artists(&metadata_runs);
    let artists = if artists.is_empty() {
        fallback_artists.to_vec()
    } else {
        artists
    };
    if artists.is_empty() {
        return None;
    }

    let duration = metadata_runs
        .iter()
        .filter_map(|run| run.get("text")?.as_str())
        .find_map(parse_duration)
        .or_else(|| {
            renderer
                .get("fixedColumns")?
                .as_array()?
                .iter()
                .filter_map(runs_from_column)
                .flatten()
                .filter_map(|run| run.get("text")?.as_str())
                .find_map(parse_duration)
        });

    let thumbnail_url = thumbnail_url(renderer).map(str::to_owned);
    let album = extract_album_credit(&metadata_runs, thumbnail_url.as_deref());
    Some(Song {
        video_id: video_id.to_owned(),
        title: title.to_owned(),
        artists,
        duration,
        thumbnail_url,
        album,
        is_episode: is_episode_renderer(renderer),
    })
}

fn parse_catalog_song(renderer: &Value, fallback_artists: &[ArtistCredit]) -> Option<Song> {
    let podcast = renderer
        .get("flexColumns")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(runs_from_column)
        .flatten()
        .find_map(|run| {
            let browse = value_at(run, &["navigationEndpoint", "browseEndpoint"])?;
            let page_type = value_at(
                browse,
                &[
                    "browseEndpointContextSupportedConfigs",
                    "browseEndpointContextMusicConfig",
                    "pageType",
                ],
            )
            .and_then(Value::as_str)
            .unwrap_or_default();
            let name = run.get("text")?.as_str()?.trim();
            page_type.contains("PODCAST").then(|| ArtistCredit {
                id: browse
                    .get("browseId")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
                name: name.to_owned(),
            })
        });
    let podcast = podcast.filter(|podcast| !podcast.name.is_empty());
    let song_fallback = podcast
        .as_ref()
        .map(std::slice::from_ref)
        .unwrap_or(fallback_artists);
    let mut song = parse_song(renderer, song_fallback)?;
    if let Some(podcast) = podcast {
        song.artists = vec![podcast];
        song.is_episode = true;
    }
    Some(song)
}

fn is_episode_renderer(renderer: &Value) -> bool {
    let has_playable_id = renderer.get("videoId").and_then(Value::as_str).is_some()
        || value_at(renderer, &["playlistItemData", "videoId"])
            .and_then(Value::as_str)
            .is_some()
        || find_value_named(renderer, "watchEndpoint")
            .and_then(|endpoint| endpoint.get("videoId"))
            .and_then(Value::as_str)
            .is_some();
    if !has_playable_id {
        return false;
    }

    if find_value_named(renderer, "musicVideoType")
        .and_then(Value::as_str)
        .is_some_and(|kind| kind.contains("PODCAST"))
    {
        return true;
    }

    let first_subtitle = renderer
        .get("flexColumns")
        .and_then(Value::as_array)
        .and_then(|columns| columns.get(1))
        .and_then(runs_from_column)
        .and_then(|runs| runs.first())
        .and_then(|run| run.get("text"))
        .and_then(Value::as_str)
        .map(str::trim);
    if first_subtitle == Some("Episode") {
        return true;
    }

    let mut page_types = Vec::new();
    collect_values_named(renderer, "pageType", &mut page_types);
    page_types.into_iter().any(|page_type| {
        page_type.as_str().is_some_and(|page_type| {
            page_type == "MUSIC_PAGE_TYPE_NON_MUSIC_AUDIO_TRACK_PAGE"
                || page_type == "MUSIC_PAGE_TYPE_PODCAST_SHOW_DETAIL_PAGE"
        })
    })
}

fn parse_browse_item(renderer: &Value) -> Option<BrowseItem> {
    let browse = value_at(renderer, &["navigationEndpoint", "browseEndpoint"])
        .or_else(|| value_at(renderer, &["onTap", "browseEndpoint"]))?;
    let browse_id = browse.get("browseId")?.as_str()?.trim();
    if browse_id.is_empty() {
        return None;
    }
    let kind = browse_kind(browse_id, browse)?;

    let title = renderer
        .get("flexColumns")
        .and_then(Value::as_array)
        .and_then(|columns| columns.first())
        .and_then(runs_from_column)
        .and_then(|runs| runs.first())
        .and_then(|run| run.get("text"))
        .and_then(Value::as_str)
        .map(str::to_owned)
        .or_else(|| text_at(renderer, &["title"]))?;
    let title = title.trim().to_owned();
    if title.is_empty() {
        return None;
    }

    let subtitle = renderer
        .get("flexColumns")
        .and_then(Value::as_array)
        .map(|columns| {
            columns
                .iter()
                .skip(1)
                .filter_map(runs_from_column)
                .flatten()
                .filter_map(|run| run.get("text").and_then(Value::as_str))
                .collect::<String>()
        })
        .filter(|subtitle| !subtitle.trim().is_empty())
        .or_else(|| text_at(renderer, &["subtitle"]))
        .map_or_else(|| kind.label().to_owned(), |text| normalize_text(&text));

    Some(BrowseItem {
        browse_id: browse_id.to_owned(),
        kind,
        title,
        subtitle,
        thumbnail_url: thumbnail_url(renderer).map(str::to_owned),
        params: browse
            .get("params")
            .and_then(Value::as_str)
            .map(str::to_owned),
        editable: renderer_has_edit_action(renderer),
    })
}

fn renderer_has_edit_action(renderer: &Value) -> bool {
    let mut icons = Vec::new();
    collect_values_named(renderer, "iconType", &mut icons);
    icons
        .into_iter()
        .filter_map(Value::as_str)
        .any(|icon| icon == "EDIT")
}

fn playlist_set_video_id(renderer: &Value) -> Option<&str> {
    value_at(renderer, &["playlistItemData", "playlistSetVideoId"])
        .or_else(|| find_value_named(renderer, "playlistSetVideoId"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

fn history_feedback_token(renderer: &Value) -> Option<&str> {
    let mut menu_items = Vec::new();
    collect_values_named(renderer, "menuServiceItemRenderer", &mut menu_items);
    menu_items.into_iter().find_map(|item| {
        (value_at(item, &["icon", "iconType"]).and_then(Value::as_str)
            == Some("REMOVE_FROM_HISTORY"))
        .then(|| {
            value_at(
                item,
                &["serviceEndpoint", "feedbackEndpoint", "feedbackToken"],
            )
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|token| !token.is_empty())
        })
        .flatten()
    })
}

fn parse_channel_subscription(header: &Value) -> Option<ChannelSubscription> {
    let renderer = find_value_named(header, "subscribeButtonRenderer")?;
    let channel_id = renderer.get("channelId")?.as_str()?.trim();
    if channel_id.is_empty() {
        return None;
    }
    Some(ChannelSubscription {
        channel_id: channel_id.to_owned(),
        subscribed: renderer
            .get("subscribed")
            .and_then(Value::as_bool)
            .unwrap_or_default(),
    })
}

fn browse_kind(browse_id: &str, browse: &Value) -> Option<BrowseKind> {
    let page_type = value_at(
        browse,
        &[
            "browseEndpointContextSupportedConfigs",
            "browseEndpointContextMusicConfig",
            "pageType",
        ],
    )
    .and_then(Value::as_str)
    .unwrap_or_default();
    if page_type.contains("PODCAST") || browse_id.starts_with("MPSP") {
        Some(BrowseKind::Podcast)
    } else if page_type.contains("ALBUM") || browse_id.starts_with("MPRE") {
        Some(BrowseKind::Album)
    } else if page_type.contains("ARTIST")
        || page_type.contains("USER_CHANNEL")
        || browse_id.starts_with("UC")
    {
        Some(BrowseKind::Artist)
    } else if page_type.contains("PLAYLIST") || browse_id.starts_with("VL") {
        Some(BrowseKind::Playlist)
    } else if browse_id.starts_with("FEmusic_") {
        Some(BrowseKind::Category)
    } else {
        None
    }
}

fn runs_from_column(column: &Value) -> Option<&Vec<Value>> {
    column
        .get("musicResponsiveListItemFlexColumnRenderer")
        .or_else(|| column.get("musicResponsiveListItemFixedColumnRenderer"))?
        .get("text")?
        .get("runs")?
        .as_array()
}

fn extract_album_credit(runs: &[&Value], thumbnail_url: Option<&str>) -> Option<AlbumCredit> {
    runs.iter().find_map(|run| {
        let title = run.get("text")?.as_str()?.trim();
        let browse = value_at(run, &["navigationEndpoint", "browseEndpoint"])?;
        let browse_id = browse.get("browseId")?.as_str()?.trim();
        let page_type = value_at(
            browse,
            &[
                "browseEndpointContextSupportedConfigs",
                "browseEndpointContextMusicConfig",
                "pageType",
            ],
        )
        .and_then(Value::as_str)
        .unwrap_or_default();
        (!title.is_empty()
            && !browse_id.is_empty()
            && (page_type.contains("ALBUM") || browse_id.starts_with("MPRE")))
        .then(|| AlbumCredit {
            browse_id: browse_id.to_owned(),
            title: title.to_owned(),
            thumbnail_url: thumbnail_url.map(str::to_owned),
        })
    })
}

fn extract_artists(runs: &[&Value]) -> Vec<ArtistCredit> {
    let linked = runs
        .iter()
        .filter_map(|run| {
            let text = run.get("text")?.as_str()?.trim();
            let browse = value_at(run, &["navigationEndpoint", "browseEndpoint"])?;
            let browse_id = browse.get("browseId").and_then(Value::as_str);
            let page_type = value_at(
                browse,
                &[
                    "browseEndpointContextSupportedConfigs",
                    "browseEndpointContextMusicConfig",
                    "pageType",
                ],
            )
            .and_then(Value::as_str);
            let is_artist = browse_id.is_some_and(|id| id.starts_with("UC"))
                || page_type.is_some_and(|kind| kind.contains("ARTIST"));
            is_artist.then(|| ArtistCredit {
                id: browse_id.map(str::to_owned),
                name: text.to_owned(),
            })
        })
        .collect::<Vec<_>>();
    if !linked.is_empty() {
        return linked;
    }

    let sections = runs.split(|run| {
        run.get("text")
            .and_then(Value::as_str)
            .is_some_and(|text| matches!(text.trim(), "•" | "·"))
    });
    for section in sections {
        let names = section
            .iter()
            .filter_map(|run| run.get("text")?.as_str())
            .flat_map(split_artist_names)
            .filter(|text| !is_metadata_text(text))
            .map(|name| ArtistCredit {
                id: None,
                name: name.to_owned(),
            })
            .collect::<Vec<_>>();
        if !names.is_empty() {
            return names;
        }
    }

    Vec::new()
}

fn split_artist_names(text: &str) -> impl Iterator<Item = &str> {
    text.split([',', '&'])
        .map(str::trim)
        .filter(|part| !part.is_empty())
}

fn is_metadata_text(text: &str) -> bool {
    let text = text.trim();
    let lower = text.to_ascii_lowercase();
    parse_duration(text).is_some()
        || (text.len() == 4 && text.parse::<u16>().is_ok())
        || matches!(
            lower.as_str(),
            "song" | "video" | "single" | "album" | "episode" | "playlist" | "podcast"
        )
}

fn text_at(value: &Value, path: &[&str]) -> Option<String> {
    text_from_value(value_at(value, path)?)
}

fn text_from_value(value: &Value) -> Option<String> {
    if let Some(text) = value.as_str() {
        return Some(normalize_text(text));
    }
    if let Some(text) = value.get("simpleText").and_then(Value::as_str) {
        return Some(normalize_text(text));
    }
    if let Some(text) = value.get("content").and_then(Value::as_str) {
        return Some(normalize_text(text));
    }
    let runs = value.get("runs")?.as_array()?;
    let text = runs
        .iter()
        .filter_map(|run| run.get("text").and_then(Value::as_str))
        .collect::<String>();
    Some(normalize_text(&text))
}

fn normalize_text(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn extract_artist_credits(value: &Value) -> Vec<ArtistCredit> {
    fn visit(value: &Value, artists: &mut Vec<ArtistCredit>) {
        match value {
            Value::Object(object) => {
                let artist = object
                    .get("text")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|name| !name.is_empty())
                    .and_then(|name| {
                        let browse = value_at(value, &["navigationEndpoint", "browseEndpoint"])?;
                        let browse_id = browse.get("browseId").and_then(Value::as_str);
                        let page_type = value_at(
                            browse,
                            &[
                                "browseEndpointContextSupportedConfigs",
                                "browseEndpointContextMusicConfig",
                                "pageType",
                            ],
                        )
                        .and_then(Value::as_str)
                        .unwrap_or_default();
                        (page_type.contains("ARTIST")
                            || page_type.contains("USER_CHANNEL")
                            || browse_id.is_some_and(|id| id.starts_with("UC")))
                        .then(|| ArtistCredit {
                            id: browse_id.map(str::to_owned),
                            name: name.to_owned(),
                        })
                    });
                if let Some(artist) = artist
                    && !artists
                        .iter()
                        .any(|existing| existing.id == artist.id && existing.name == artist.name)
                {
                    artists.push(artist);
                }
                for child in object.values() {
                    visit(child, artists);
                }
            }
            Value::Array(array) => {
                for child in array {
                    visit(child, artists);
                }
            }
            _ => {}
        }
    }

    let mut artists = Vec::new();
    visit(value, &mut artists);
    artists
}

fn parse_duration(text: &str) -> Option<Duration> {
    let parts = text
        .trim()
        .split(':')
        .map(str::parse::<u64>)
        .collect::<std::result::Result<Vec<_>, _>>()
        .ok()?;
    match parts.as_slice() {
        [minutes, seconds] if *seconds < 60 => Some(Duration::from_secs(minutes * 60 + seconds)),
        [hours, minutes, seconds] if *minutes < 60 && *seconds < 60 => Some(Duration::from_secs(
            hours * 60 * 60 + minutes * 60 + seconds,
        )),
        _ => None,
    }
}

fn thumbnail_url(renderer: &Value) -> Option<&str> {
    let mut thumbnail_lists = Vec::new();
    collect_values_named(renderer, "thumbnails", &mut thumbnail_lists);
    thumbnail_lists
        .into_iter()
        .filter_map(Value::as_array)
        .flatten()
        .filter_map(|thumbnail| {
            let url = thumbnail.get("url")?.as_str()?;
            let area = thumbnail
                .get("width")
                .and_then(Value::as_u64)
                .unwrap_or_default()
                .saturating_mul(
                    thumbnail
                        .get("height")
                        .and_then(Value::as_u64)
                        .unwrap_or_default(),
                );
            Some((area, url))
        })
        .max_by_key(|(area, _)| *area)
        .map(|(_, url)| url)
}

fn value_at<'a>(value: &'a Value, path: &[&str]) -> Option<&'a Value> {
    path.iter().try_fold(value, |value, key| value.get(key))
}

fn collect_values_named<'a>(value: &'a Value, name: &str, values: &mut Vec<&'a Value>) {
    match value {
        Value::Object(object) => {
            for (key, value) in object {
                if key == name {
                    values.push(value);
                } else {
                    collect_values_named(value, name, values);
                }
            }
        }
        Value::Array(array) => {
            for value in array {
                collect_values_named(value, name, values);
            }
        }
        _ => {}
    }
}

fn find_value_named<'a>(value: &'a Value, name: &str) -> Option<&'a Value> {
    let mut values = Vec::new();
    collect_values_named(value, name, &mut values);
    values.into_iter().next()
}

fn find_continuation(value: &Value) -> Option<&str> {
    match value {
        Value::Object(object) => {
            if let Some(token) = object
                .get("continuationCommand")
                .and_then(|command| command.get("token"))
                .and_then(Value::as_str)
                .or_else(|| {
                    object
                        .get("nextContinuationData")
                        .and_then(|data| data.get("continuation"))
                        .and_then(Value::as_str)
                })
                .or_else(|| {
                    object
                        .get("nextRadioContinuationData")
                        .and_then(|data| data.get("continuation"))
                        .and_then(Value::as_str)
                })
            {
                return Some(token);
            }
            object.values().find_map(find_continuation)
        }
        Value::Array(array) => array.iter().find_map(find_continuation),
        _ => None,
    }
}

fn find_visitor_data(value: &Value) -> Option<&str> {
    match value {
        Value::String(candidate)
            if (candidate.starts_with("Cgt") || candidate.starts_with("Cgs"))
                && candidate.len() > 10 =>
        {
            Some(candidate)
        }
        Value::Object(object) => object.values().find_map(find_visitor_data),
        Value::Array(array) => array.iter().find_map(find_visitor_data),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;
    use std::sync::{
        Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    };

    use futures::future::BoxFuture;
    use http_client::{Response, StatusCode};

    const SEARCH_FIXTURE: &[u8] = include_bytes!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/innertube/search_songs.json"
    ));
    const SEARCH_SUGGESTIONS_FIXTURE: &[u8] = include_bytes!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/innertube/search_suggestions.json"
    ));
    const PLAYER_FIXTURE: &[u8] = include_bytes!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/innertube/player_visionos.json"
    ));
    const CATALOG_FIXTURE: &[u8] = include_bytes!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/innertube/search_catalog.json"
    ));
    const ALBUM_FIXTURE: &[u8] = include_bytes!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/innertube/browse_album.json"
    ));
    const ARTIST_FIXTURE: &[u8] = include_bytes!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/innertube/browse_artist.json"
    ));
    const PLAYLIST_FIXTURE: &[u8] = include_bytes!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/innertube/browse_playlist.json"
    ));
    const PODCAST_SEARCH_FIXTURE: &[u8] = include_bytes!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/innertube/search_podcasts.json"
    ));
    const PODCAST_FIXTURE: &[u8] = include_bytes!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/innertube/browse_podcast.json"
    ));
    const SEARCH_CONTINUATION_FIXTURE: &[u8] = include_bytes!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/innertube/search_continuation.json"
    ));
    const RADIO_INITIAL_FIXTURE: &[u8] = include_bytes!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/innertube/radio_initial.json"
    ));
    const RADIO_CONTINUATION_FIXTURE: &[u8] = include_bytes!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/innertube/radio_continuation.json"
    ));
    const ACCOUNT_FIXTURE: &[u8] = include_bytes!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/innertube/account_menu.json"
    ));
    const BROWSE_CONTINUATION_FIXTURE: &[u8] = include_bytes!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/innertube/browse_continuation.json"
    ));
    const HISTORY_FIXTURE: &[u8] = include_bytes!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/innertube/history.json"
    ));
    const HISTORY_CONTINUATION_FIXTURE: &[u8] = include_bytes!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/innertube/history_continuation.json"
    ));
    const HOME_FIXTURE: &[u8] = include_bytes!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/innertube/home.json"
    ));
    const HOME_CONTINUATION_FIXTURE: &[u8] = include_bytes!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/innertube/home_continuation.json"
    ));
    const EXPLORE_FIXTURE: &[u8] = include_bytes!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/innertube/explore.json"
    ));

    struct FlakyRadioClient {
        attempts: AtomicUsize,
        succeed_on: Option<usize>,
    }

    impl FlakyRadioClient {
        fn new(succeed_on: Option<usize>) -> Arc<Self> {
            Arc::new(Self {
                attempts: AtomicUsize::new(0),
                succeed_on,
            })
        }
    }

    impl HttpClient for FlakyRadioClient {
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
            let attempt = self.attempts.fetch_add(1, Ordering::Relaxed) + 1;
            let success = self.succeed_on == Some(attempt);
            Box::pin(async move {
                Response::builder()
                    .status(if success {
                        StatusCode::OK
                    } else {
                        StatusCode::SERVICE_UNAVAILABLE
                    })
                    .body(if success {
                        AsyncBody::from(RADIO_INITIAL_FIXTURE.to_vec())
                    } else {
                        AsyncBody::default()
                    })
                    .map_err(Into::into)
            })
        }
    }

    #[derive(Debug, Clone, Copy, Default)]
    struct CapturedAccountRequest {
        account_uri: bool,
        cookie_matches: bool,
        authorization_shape: bool,
        origin_matches: bool,
        auth_user_matches: bool,
        visitor_matches: bool,
    }

    struct AccountHttpClient {
        attempts: AtomicUsize,
        captured: Mutex<CapturedAccountRequest>,
    }

    impl AccountHttpClient {
        fn new() -> Arc<Self> {
            Arc::new(Self {
                attempts: AtomicUsize::new(0),
                captured: Mutex::new(CapturedAccountRequest::default()),
            })
        }
    }

    impl HttpClient for AccountHttpClient {
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
            let attempt = self.attempts.fetch_add(1, Ordering::Relaxed) + 1;
            *self.captured.lock().unwrap() = CapturedAccountRequest {
                account_uri: request.uri().path().ends_with("/account/account_menu"),
                cookie_matches: request
                    .headers()
                    .get("cookie")
                    .and_then(|value| value.to_str().ok())
                    == Some("SID=fixture; SAPISID=sapisid-secret"),
                authorization_shape: request
                    .headers()
                    .get("authorization")
                    .and_then(|value| value.to_str().ok())
                    .is_some_and(|value| {
                        value.starts_with("SAPISIDHASH ") && !value.contains("sapisid-secret")
                    }),
                origin_matches: request
                    .headers()
                    .get("origin")
                    .and_then(|value| value.to_str().ok())
                    == Some(ORIGIN),
                auth_user_matches: request
                    .headers()
                    .get("x-goog-authuser")
                    .and_then(|value| value.to_str().ok())
                    == Some("0"),
                visitor_matches: request
                    .headers()
                    .get("x-goog-visitor-id")
                    .and_then(|value| value.to_str().ok())
                    == Some("visitor-fixture"),
            };
            Box::pin(async move {
                Response::builder()
                    .status(if attempt == 1 {
                        StatusCode::SERVICE_UNAVAILABLE
                    } else {
                        StatusCode::OK
                    })
                    .body(if attempt == 1 {
                        AsyncBody::default()
                    } else {
                        AsyncBody::from(ACCOUNT_FIXTURE.to_vec())
                    })
                    .map_err(Into::into)
            })
        }
    }

    struct SearchPrivacyHttpClient {
        no_login_headers: AtomicBool,
        suggestions_path: AtomicBool,
        bodies: Arc<Mutex<Vec<Value>>>,
    }

    impl SearchPrivacyHttpClient {
        fn new() -> Arc<Self> {
            Arc::new(Self {
                no_login_headers: AtomicBool::new(true),
                suggestions_path: AtomicBool::new(false),
                bodies: Arc::new(Mutex::new(Vec::new())),
            })
        }
    }

    impl HttpClient for SearchPrivacyHttpClient {
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
            let has_login_header = ["cookie", "authorization", "x-goog-authuser"]
                .into_iter()
                .any(|name| request.headers().contains_key(name));
            self.no_login_headers
                .fetch_and(!has_login_header, Ordering::Relaxed);
            let is_suggestions = request
                .uri()
                .path()
                .ends_with("/music/get_search_suggestions");
            self.suggestions_path
                .store(is_suggestions, Ordering::Relaxed);
            let (_, mut request_body) = request.into_parts();
            let bodies = self.bodies.clone();
            Box::pin(async move {
                let mut bytes = Vec::new();
                request_body.read_to_end(&mut bytes).await?;
                bodies
                    .lock()
                    .unwrap()
                    .push(serde_json::from_slice(&bytes).unwrap());
                Response::builder()
                    .status(StatusCode::OK)
                    .body(AsyncBody::from(if is_suggestions {
                        SEARCH_SUGGESTIONS_FIXTURE.to_vec()
                    } else {
                        SEARCH_FIXTURE.to_vec()
                    }))
                    .map_err(Into::into)
            })
        }
    }

    struct EpisodeFallbackHttpClient {
        attempts: AtomicUsize,
        bodies: Arc<Mutex<Vec<Value>>>,
    }

    impl EpisodeFallbackHttpClient {
        fn new() -> Arc<Self> {
            Arc::new(Self {
                attempts: AtomicUsize::new(0),
                bodies: Arc::new(Mutex::new(Vec::new())),
            })
        }
    }

    impl HttpClient for EpisodeFallbackHttpClient {
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
            let attempt = self.attempts.fetch_add(1, Ordering::Relaxed);
            let (_, mut request_body) = request.into_parts();
            let bodies = self.bodies.clone();
            Box::pin(async move {
                let mut bytes = Vec::new();
                request_body.read_to_end(&mut bytes).await?;
                bodies
                    .lock()
                    .unwrap()
                    .push(serde_json::from_slice(&bytes).unwrap());
                Response::builder()
                    .status(StatusCode::OK)
                    .body(AsyncBody::from(if attempt == 0 {
                        b"{}".to_vec()
                    } else {
                        PODCAST_SEARCH_FIXTURE.to_vec()
                    }))
                    .map_err(Into::into)
            })
        }
    }

    #[derive(Debug, Clone)]
    struct CapturedMutation {
        method: String,
        path: String,
        query: Option<String>,
        authenticated: bool,
        body: Value,
    }

    struct MutationHttpClient {
        attempts: AtomicUsize,
        statuses: Mutex<VecDeque<StatusCode>>,
        captured: Arc<Mutex<Vec<CapturedMutation>>>,
    }

    impl MutationHttpClient {
        fn new(statuses: impl IntoIterator<Item = StatusCode>) -> Arc<Self> {
            Arc::new(Self {
                attempts: AtomicUsize::new(0),
                statuses: Mutex::new(statuses.into_iter().collect()),
                captured: Arc::new(Mutex::new(Vec::new())),
            })
        }
    }

    impl HttpClient for MutationHttpClient {
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
            self.attempts.fetch_add(1, Ordering::Relaxed);
            let status = self
                .statuses
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or(StatusCode::OK);
            let authenticated = request.headers().contains_key("cookie")
                && request.headers().contains_key("authorization")
                && request.headers().contains_key("x-goog-authuser");
            let method = request.method().as_str().to_owned();
            let path = request.uri().path().to_owned();
            let query = request.uri().query().map(str::to_owned);
            let (_, mut body) = request.into_parts();
            let captured = self.captured.clone();
            Box::pin(async move {
                let mut bytes = Vec::new();
                body.read_to_end(&mut bytes).await?;
                let body = if bytes.is_empty() {
                    Value::Null
                } else {
                    serde_json::from_slice(&bytes)
                        .expect("mutation request body should contain fixture JSON")
                };
                captured.lock().unwrap().push(CapturedMutation {
                    method,
                    path: path.clone(),
                    query,
                    authenticated,
                    body,
                });
                Response::builder()
                    .status(status)
                    .body(
                        if status.is_success() && path.ends_with("/playlist/create") {
                            AsyncBody::from(br#"{"playlistId":"PL-created-fixture"}"#.to_vec())
                        } else if status.is_success() && path.ends_with("/feedback") {
                            AsyncBody::from(
                                br#"{"feedbackResponses":[{"isProcessed":true}]}"#.to_vec(),
                            )
                        } else {
                            AsyncBody::from(b"{}".to_vec())
                        },
                    )
                    .map_err(Into::into)
            })
        }
    }

    struct LibraryHttpClient {
        attempts: AtomicUsize,
        every_request_authenticated: AtomicBool,
    }

    struct PodcastLibraryHttpClient {
        browse_ids: Arc<Mutex<Vec<String>>>,
        every_request_authenticated: AtomicBool,
    }

    struct HistoryHttpClient {
        attempts: AtomicUsize,
        every_request_authenticated: AtomicBool,
    }

    impl HistoryHttpClient {
        fn new() -> Arc<Self> {
            Arc::new(Self {
                attempts: AtomicUsize::new(0),
                every_request_authenticated: AtomicBool::new(true),
            })
        }
    }

    impl HttpClient for HistoryHttpClient {
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
            let attempt = self.attempts.fetch_add(1, Ordering::Relaxed);
            let authenticated = request.headers().contains_key("cookie")
                && request.headers().contains_key("authorization")
                && request.headers().contains_key("x-goog-authuser");
            self.every_request_authenticated
                .fetch_and(authenticated, Ordering::Relaxed);
            let bytes = if attempt == 0 {
                HISTORY_FIXTURE.to_vec()
            } else {
                HISTORY_CONTINUATION_FIXTURE.to_vec()
            };
            Box::pin(async move {
                Response::builder()
                    .status(StatusCode::OK)
                    .body(AsyncBody::from(bytes))
                    .map_err(Into::into)
            })
        }
    }

    impl LibraryHttpClient {
        fn new() -> Arc<Self> {
            Arc::new(Self {
                attempts: AtomicUsize::new(0),
                every_request_authenticated: AtomicBool::new(true),
            })
        }
    }

    impl PodcastLibraryHttpClient {
        fn new() -> Arc<Self> {
            Arc::new(Self {
                browse_ids: Arc::new(Mutex::new(Vec::new())),
                every_request_authenticated: AtomicBool::new(true),
            })
        }
    }

    impl HttpClient for PodcastLibraryHttpClient {
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
            let authenticated = request.headers().contains_key("cookie")
                && request.headers().contains_key("authorization")
                && request.headers().contains_key("x-goog-authuser");
            self.every_request_authenticated
                .fetch_and(authenticated, Ordering::Relaxed);
            let (_, mut body) = request.into_parts();
            let browse_ids = self.browse_ids.clone();
            Box::pin(async move {
                let mut bytes = Vec::new();
                body.read_to_end(&mut bytes).await?;
                let body: Value = serde_json::from_slice(&bytes).unwrap();
                let browse_id = body["browseId"].as_str().unwrap().to_owned();
                browse_ids.lock().unwrap().push(browse_id.clone());
                let bytes = match browse_id.as_str() {
                    "FEmusic_library_non_music_audio_list" => {
                        let mut value: Value =
                            serde_json::from_slice(PODCAST_SEARCH_FIXTURE).unwrap();
                        value.as_object_mut().unwrap().remove("continuations");
                        serde_json::to_vec(&value).unwrap()
                    }
                    "FEmusic_library_non_music_audio_channels_list" => {
                        let mut value: Value =
                            serde_json::from_slice(PODCAST_SEARCH_FIXTURE).unwrap();
                        value.as_object_mut().unwrap().remove("continuations");
                        serde_json::to_string(&value)
                            .unwrap()
                            .replace("MPSPfixture-podcast", "UCfixture-podcast-channel")
                            .replace("Fixture Podcast", "Fixture Channel Podcast")
                            .into_bytes()
                    }
                    "VLSE" => PLAYLIST_FIXTURE.to_vec(),
                    other => panic!("unexpected podcast library browse id: {other}"),
                };
                Response::builder()
                    .status(StatusCode::OK)
                    .body(AsyncBody::from(bytes))
                    .map_err(Into::into)
            })
        }
    }

    impl HttpClient for LibraryHttpClient {
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
            let attempt = self.attempts.fetch_add(1, Ordering::Relaxed);
            let authenticated = request.headers().contains_key("cookie")
                && request.headers().contains_key("authorization")
                && request.headers().contains_key("x-goog-authuser");
            self.every_request_authenticated
                .fetch_and(authenticated, Ordering::Relaxed);
            let bytes = if attempt == 0 {
                ALBUM_FIXTURE.to_vec()
            } else {
                let continuation: Value =
                    serde_json::from_slice(BROWSE_CONTINUATION_FIXTURE).unwrap();
                serde_json::to_string(&continuation)
                    .unwrap()
                    .replace("playlist-song-two", "album-song-two")
                    .replace("browse-continuation-two", "album-continuation")
                    .into_bytes()
            };
            Box::pin(async move {
                Response::builder()
                    .status(StatusCode::OK)
                    .body(AsyncBody::from(bytes))
                    .map_err(Into::into)
            })
        }
    }

    fn browse_item(kind: BrowseKind, browse_id: &str, title: &str) -> BrowseItem {
        BrowseItem {
            browse_id: browse_id.into(),
            kind,
            title: title.into(),
            subtitle: kind.label().into(),
            thumbnail_url: None,
            params: None,
            editable: false,
        }
    }

    #[test]
    fn parses_song_search_fixture_into_stable_domain_models() {
        let result = parse_search_response(SEARCH_FIXTURE).unwrap();

        assert_eq!(result.songs.len(), 2);
        assert_eq!(result.songs[0].video_id, "video-one");
        assert_eq!(result.songs[0].title, "First Song");
        assert_eq!(result.songs[0].artist_line(), "First Artist, Guest Artist");
        assert_eq!(result.songs[0].duration, Some(Duration::from_secs(225)));
        assert_eq!(
            result.songs[0].thumbnail_url.as_deref(),
            Some("https://i.ytimg.com/first-large.jpg")
        );
        assert_eq!(result.songs[1].artist_line(), "Unlinked Artist");
        assert!(result.items.is_empty());
        assert_eq!(
            result.continuation.as_deref(),
            Some("redacted-continuation")
        );
    }

    #[test]
    fn parses_search_suggestions_queries_songs_and_browse_items() {
        let suggestions = parse_search_suggestions_response(SEARCH_SUGGESTIONS_FIXTURE).unwrap();

        assert_eq!(suggestions.queries, ["daft punk", "daft punk discovery"]);
        assert_eq!(suggestions.songs.len(), 1);
        assert_eq!(suggestions.songs[0].video_id, "suggested-video");
        assert_eq!(suggestions.songs[0].title, "Suggested Song");
        assert_eq!(suggestions.songs[0].artist_line(), "Suggested Artist");
        assert_eq!(
            suggestions.songs[0].duration,
            Some(Duration::from_secs(201))
        );
        assert_eq!(suggestions.items.len(), 1);
        assert_eq!(suggestions.items[0].kind, BrowseKind::Album);
        assert_eq!(suggestions.items[0].browse_id, "MPRE-suggested-album");
        assert_eq!(suggestions.items[0].title, "Suggested Album");
    }

    #[test]
    fn authenticated_session_keeps_suggestion_requests_anonymous_and_uses_android_shape() {
        let auth = AuthSession::from_import(
            "***INNERTUBE COOKIE*** = SID=fixture; SAPISID=sapisid-secret\n***VISITOR DATA*** = visitor-fixture",
        )
        .unwrap();
        let http = SearchPrivacyHttpClient::new();
        let client = InnerTubeClient::new(
            InnerTubeSession::default().with_auth(Some(auth)),
            http.clone(),
            AudioQuality::Auto,
        );

        let result = futures::executor::block_on(client.search_suggestions("  daft  ")).unwrap();

        assert!(!result.queries.is_empty());
        assert!(http.no_login_headers.load(Ordering::Relaxed));
        assert!(http.suggestions_path.load(Ordering::Relaxed));
        let bodies = http.bodies.lock().unwrap();
        assert_eq!(bodies.len(), 1);
        assert_eq!(bodies[0]["input"], "daft");
        assert_eq!(bodies[0]["context"]["client"]["clientName"], CLIENT_NAME);
        assert_eq!(
            bodies[0]["context"]["client"]["visitorData"],
            "visitor-fixture"
        );
        assert!(bodies[0]["context"]["user"]["onBehalfOfUser"].is_null());
    }

    #[test]
    fn empty_or_invalid_suggestion_input_never_reaches_the_network() {
        let http = SearchPrivacyHttpClient::new();
        let client = InnerTubeClient::new(
            InnerTubeSession::default(),
            http.clone(),
            AudioQuality::Auto,
        );

        let empty = futures::executor::block_on(client.search_suggestions(" \t ")).unwrap();
        assert!(empty.queries.is_empty());
        assert!(empty.songs.is_empty());
        assert!(empty.items.is_empty());
        let error =
            futures::executor::block_on(client.search_suggestions("bad\nquery")).unwrap_err();
        assert!(matches!(error, AppError::Protocol(_)));
        assert!(http.bodies.lock().unwrap().is_empty());
    }

    #[test]
    fn parses_podcast_and_episode_search_into_reusable_catalog_models() {
        let result = parse_search_response(PODCAST_SEARCH_FIXTURE).unwrap();

        assert_eq!(result.items.len(), 1);
        assert_eq!(result.items[0].kind, BrowseKind::Podcast);
        assert_eq!(result.items[0].browse_id, "MPSPfixture-podcast");
        assert_eq!(result.items[0].title, "Fixture Podcast");
        assert_eq!(result.songs.len(), 1);
        assert_eq!(result.songs[0].video_id, "fixture-episode-one");
        assert_eq!(result.songs[0].artist_line(), "Fixture Podcast");
        assert_eq!(result.songs[0].duration, Some(Duration::from_secs(3_723)));
        assert!(result.songs[0].is_episode);
        let episodes = parse_episode_search_response(PODCAST_SEARCH_FIXTURE).unwrap();
        assert_eq!(episodes.songs.len(), 1);
        assert!(episodes.items.is_empty());
        assert!(episodes.continuation.is_none());
        assert_eq!(
            result.continuation.as_deref(),
            Some("podcast-search-continuation")
        );

        let page = parse_browse_response(
            PODCAST_FIXTURE,
            browse_item(BrowseKind::Podcast, "MPSPfixture-podcast", "Podcast"),
        )
        .unwrap();
        assert_eq!(page.item.title, "Fixture Podcast");
        assert_eq!(page.item.kind, BrowseKind::Podcast);
        assert_eq!(page.songs.len(), 1);
        assert_eq!(page.songs[0].video_id, "fixture-detail-episode");
        assert_eq!(page.songs[0].artist_line(), "Fixture Host");
        assert_eq!(page.songs[0].duration, Some(Duration::from_secs(3_487)));
        assert!(page.songs[0].is_episode);
        assert_eq!(
            page.continuation.as_deref(),
            Some("podcast-detail-continuation")
        );
    }

    #[test]
    fn episode_search_retries_unfiltered_and_keeps_only_episode_renderers() {
        let http = EpisodeFallbackHttpClient::new();
        let client = InnerTubeClient::new(
            InnerTubeSession::default(),
            http.clone(),
            AudioQuality::Auto,
        );
        let result =
            futures::executor::block_on(client.search("Fixture", SearchFilter::Episodes)).unwrap();

        assert_eq!(http.attempts.load(Ordering::Relaxed), 2);
        assert_eq!(result.songs.len(), 1);
        assert!(result.songs[0].is_episode);
        assert!(result.items.is_empty());
        assert!(result.continuation.is_none());
        let bodies = http.bodies.lock().unwrap();
        assert_eq!(bodies[0]["params"], EPISODE_FILTER);
        assert!(bodies[1].get("params").is_none());
    }

    #[test]
    fn skips_incomplete_renderers_and_deduplicates_video_ids() {
        let result = parse_search_response(SEARCH_FIXTURE).unwrap();
        assert_eq!(
            result
                .songs
                .iter()
                .filter(|song| song.video_id == "video-one")
                .count(),
            1
        );
        assert!(result.songs.iter().all(|song| !song.title.is_empty()));
    }

    #[test]
    fn rejects_malformed_json_without_exposing_response_data() {
        let error = parse_search_response(b"not json").unwrap_err();
        assert!(matches!(error, AppError::Protocol(_)));
    }

    #[test]
    fn parses_home_chips_mixed_shelves_and_continuation() {
        let page = parse_home_response(HOME_FIXTURE).unwrap();

        assert_eq!(
            page.chips,
            [
                HomeChip {
                    title: "Energize".into(),
                    params: Some("energize-params".into()),
                },
                HomeChip {
                    title: "Relax".into(),
                    params: Some("relax-params".into()),
                },
            ]
        );
        assert_eq!(page.sections.len(), 2);
        assert_eq!(page.sections[0].title, "Quick picks");
        assert_eq!(
            page.sections[0].label.as_deref(),
            Some("Start a radio from a song")
        );
        assert_eq!(page.sections[0].items.len(), 1);
        let HomeItem::Song(song) = &page.sections[0].items[0] else {
            panic!("quick pick should be a song");
        };
        assert_eq!(song.video_id, "home-song-one");
        assert_eq!(song.artist_line(), "Home Artist");
        assert_eq!(song.duration, Some(Duration::from_secs(192)));
        assert_eq!(
            song.thumbnail_url.as_deref(),
            Some("https://example.invalid/home-one.jpg")
        );
        let more = page.sections[0].more.as_ref().unwrap();
        assert_eq!(more.kind, BrowseKind::Category);
        assert_eq!(more.params.as_deref(), Some("quick-picks-params"));

        assert_eq!(page.sections[1].items.len(), 2);
        let HomeItem::Browse(album) = &page.sections[1].items[0] else {
            panic!("first catalog item should be an album");
        };
        assert_eq!(album.browse_id, "MPRE-home-album");
        assert_eq!(album.kind, BrowseKind::Album);
        let HomeItem::Song(song) = &page.sections[1].items[1] else {
            panic!("second catalog item should be a song");
        };
        assert_eq!(song.video_id, "home-song-two");
        assert_eq!(page.continuation.as_deref(), Some("home-continuation-one"));
    }

    #[test]
    fn home_parser_skips_incomplete_renderers_and_merges_new_items_only() {
        let mut page = parse_home_response(HOME_FIXTURE).unwrap();
        let next = parse_home_response(HOME_CONTINUATION_FIXTURE).unwrap();

        assert_eq!(next.sections.len(), 1);
        assert_eq!(next.sections[0].items.len(), 2);
        assert_eq!(page.append_continuation(next), 1);
        assert_eq!(page.sections.len(), 3);
        assert_eq!(page.sections[2].items.len(), 1);
        let HomeItem::Song(song) = &page.sections[2].items[0] else {
            panic!("continued item should be a song");
        };
        assert_eq!(song.video_id, "home-song-three");
        assert_eq!(song.artist_line(), "Continued Artist");
        assert_eq!(song.duration, Some(Duration::from_secs(154)));
        assert_eq!(page.continuation.as_deref(), Some("home-continuation-two"));
    }

    #[test]
    fn parses_explore_new_releases_and_parameterized_categories() {
        let page = parse_explore_response(EXPLORE_FIXTURE).unwrap();

        assert_eq!(page.new_release_albums.len(), 1);
        let album = &page.new_release_albums[0];
        assert_eq!(album.browse_id, "MPRE-fresh-album");
        assert_eq!(album.title, "Fresh Album");
        assert_eq!(album.kind, BrowseKind::Album);
        assert_eq!(album.params.as_deref(), Some("album-params"));
        assert_eq!(
            album.thumbnail_url.as_deref(),
            Some("https://example.invalid/fresh-album.jpg")
        );

        assert_eq!(page.categories.len(), 2);
        assert_eq!(page.categories[0].title, "Focus");
        assert_eq!(page.categories[0].params.as_deref(), Some("focus-params"));
        assert_eq!(page.categories[0].stripe_color, Some(4_281_545_523));
        assert_eq!(page.categories[1].title, "Workout");
        let category_item = page.categories[1].browse_item();
        assert_eq!(category_item.kind, BrowseKind::Category);
        assert_eq!(category_item.params.as_deref(), Some("workout-params"));
    }

    #[test]
    fn parses_album_artist_and_playlist_search_results() {
        let result = parse_search_response(CATALOG_FIXTURE).unwrap();

        assert!(result.songs.is_empty());
        assert_eq!(result.items.len(), 3);
        assert_eq!(result.items[0].kind, BrowseKind::Album);
        assert_eq!(result.items[0].browse_id, "MPRE-album-one");
        assert_eq!(result.items[0].title, "Fixture Album");
        assert_eq!(
            result.items[0].thumbnail_url.as_deref(),
            Some("https://example.invalid/album.jpg")
        );
        assert_eq!(result.items[1].kind, BrowseKind::Artist);
        assert_eq!(result.items[2].kind, BrowseKind::Playlist);
        assert_eq!(result.items[2].subtitle, "Fixture Curator • 12 songs");
        assert_eq!(result.continuation.as_deref(), Some("catalog-continuation"));
    }

    #[test]
    fn parses_album_header_tracks_fallback_metadata_and_related_items() {
        let page = parse_browse_response(
            ALBUM_FIXTURE,
            browse_item(BrowseKind::Album, "MPRE-album-one", "Fixture Album"),
        )
        .unwrap();

        assert_eq!(page.item.title, "Fixture Album (Deluxe)");
        assert_eq!(page.item.subtitle, "Fixture Artist · Album • 2026");
        assert_eq!(
            page.item.thumbnail_url.as_deref(),
            Some("https://example.invalid/album-header.jpg")
        );
        assert_eq!(
            page.description.as_deref(),
            Some("A deterministic album fixture.")
        );
        assert_eq!(page.songs.len(), 2);
        assert_eq!(page.songs[0].artist_line(), "Fixture Artist");
        assert_eq!(page.songs[0].duration, Some(Duration::from_secs(201)));
        assert_eq!(page.songs[0].thumbnail_url, page.item.thumbnail_url);
        assert_eq!(page.related.len(), 1);
        assert_eq!(page.related[0].browse_id, "MPRE-related");
        assert_eq!(page.continuation.as_deref(), Some("album-continuation"));
    }

    #[test]
    fn parses_artist_top_songs_description_and_album_sections() {
        let page = parse_browse_response(
            ARTIST_FIXTURE,
            browse_item(BrowseKind::Artist, "UC-artist-one", "Artist"),
        )
        .unwrap();

        assert_eq!(page.item.title, "Fixture Artist");
        assert_eq!(
            page.description.as_deref(),
            Some("An artist fixture used without the network.")
        );
        assert_eq!(page.songs.len(), 1);
        assert_eq!(page.songs[0].video_id, "artist-song-one");
        assert_eq!(page.related[0].kind, BrowseKind::Album);
        assert_eq!(
            page.channel_subscription,
            Some(ChannelSubscription {
                channel_id: "UC-fixture-channel".into(),
                subscribed: true,
            })
        );
    }

    #[test]
    fn parses_online_playlist_header_and_ordered_tracks() {
        let page = parse_browse_response(
            PLAYLIST_FIXTURE,
            browse_item(BrowseKind::Playlist, "VL-playlist-one", "Playlist"),
        )
        .unwrap();

        assert_eq!(page.item.title, "Fixture Playlist");
        assert_eq!(page.item.subtitle, "Fixture Curator · 2 songs");
        assert!(page.item.editable);
        assert_eq!(
            page.songs
                .iter()
                .map(|song| song.video_id.as_str())
                .collect::<Vec<_>>(),
            ["playlist-song-one", "playlist-song-two"]
        );
        assert_eq!(
            page.playlist_entries
                .iter()
                .map(|entry| entry.set_video_id.as_str())
                .collect::<Vec<_>>(),
            ["set-video-one", "set-video-two"]
        );
        assert_eq!(
            page.description.as_deref(),
            Some("A local representation of an online playlist.")
        );
    }

    #[test]
    fn parses_grouped_remote_history_and_preserves_distinct_replays() {
        let mut page = parse_history_response(HISTORY_FIXTURE).unwrap();

        assert_eq!(page.sections.len(), 2);
        assert_eq!(page.sections[0].title, "Today");
        assert_eq!(page.sections[1].title, "Yesterday");
        assert_eq!(page.entry_count(), 3);
        assert_eq!(
            page.sections[0].entries[0].song.video_id,
            "history-song-one"
        );
        assert_eq!(
            page.sections[0].entries[1].song.video_id,
            "history-song-one"
        );
        assert_eq!(
            page.sections[0].entries[0].feedback_token.as_deref(),
            Some("history-token-one")
        );
        assert!(!format!("{page:?}").contains("history-token-one"));
        assert_eq!(
            page.continuation.as_deref(),
            Some("history-continuation-one")
        );

        let continuation = parse_history_response(HISTORY_CONTINUATION_FIXTURE).unwrap();
        assert_eq!(continuation.entry_count(), 3);
        assert_eq!(page.append(continuation), 2);
        assert_eq!(page.entry_count(), 5);
        assert_eq!(page.sections.len(), 3);
        assert_eq!(page.sections[2].title, "Older");
    }

    #[test]
    fn library_parser_preserves_editable_responsive_playlists() {
        let page = parse_browse_response(
            serde_json::json!({
                "contents": {
                    "musicResponsiveListItemRenderer": {
                        "navigationEndpoint": {
                            "browseEndpoint": {
                                "browseId": "VLPL-owned",
                                "browseEndpointContextSupportedConfigs": {
                                    "browseEndpointContextMusicConfig": {
                                        "pageType": "MUSIC_PAGE_TYPE_PLAYLIST"
                                    }
                                }
                            }
                        },
                        "flexColumns": [
                            { "musicResponsiveListItemFlexColumnRenderer": {
                                "text": { "runs": [{ "text": "Owned playlist" }] }
                            } },
                            { "musicResponsiveListItemFlexColumnRenderer": {
                                "text": { "runs": [{ "text": "3 songs" }] }
                            } }
                        ],
                        "menu": { "menuRenderer": { "items": [
                            { "menuNavigationItemRenderer": {
                                "icon": { "iconType": "EDIT" }
                            } }
                        ] } }
                    }
                }
            })
            .to_string(),
            browse_item(BrowseKind::Category, "FEmusic_liked_playlists", "Library"),
        )
        .unwrap();

        assert_eq!(page.related.len(), 1);
        assert_eq!(page.related[0].browse_id, "VLPL-owned");
        assert!(page.related[0].editable);
    }

    #[test]
    fn continuation_uri_carries_both_encoded_opaque_token_parameters() {
        let uri = continuation_uri("browse", "opaque+/token==").unwrap();
        let url = Url::parse(&uri).unwrap();
        let pairs = url.query_pairs().collect::<Vec<_>>();

        assert_eq!(url.path(), "/youtubei/v1/browse");
        assert!(pairs.contains(&("prettyPrint".into(), "false".into())));
        assert!(pairs.contains(&("continuation".into(), "opaque+/token==".into())));
        assert!(pairs.contains(&("ctoken".into(), "opaque+/token==".into())));
        assert!(!uri.contains("opaque+/token=="));
        assert!(continuation_uri("player", "token").is_err());
    }

    #[test]
    fn parses_search_continuation_items_and_next_token() {
        let result = parse_search_continuation_response(SEARCH_CONTINUATION_FIXTURE).unwrap();

        assert_eq!(result.songs.len(), 1);
        assert_eq!(result.songs[0].video_id, "continued-song-one");
        assert_eq!(result.songs[0].artist_line(), "Continued Artist");
        assert_eq!(result.songs[0].duration, Some(Duration::from_secs(201)));
        assert_eq!(
            result.continuation.as_deref(),
            Some("search-continuation-two")
        );
    }

    #[test]
    fn search_continuation_merge_deduplicates_across_page_boundaries() {
        let mut result = parse_search_response(SEARCH_FIXTURE).unwrap();
        let mut continuation =
            parse_search_continuation_response(SEARCH_CONTINUATION_FIXTURE).unwrap();
        continuation.songs.push(result.songs[0].clone());

        assert_eq!(result.append_continuation(continuation), 1);
        assert_eq!(result.songs.len(), 3);
        assert_eq!(
            result
                .songs
                .iter()
                .filter(|song| song.video_id == "video-one")
                .count(),
            1
        );
        assert_eq!(
            result.continuation.as_deref(),
            Some("search-continuation-two")
        );
    }

    #[test]
    fn empty_search_continuation_page_drops_its_token() {
        let result = parse_search_continuation_response(
            br#"{"continuationContents":{"musicShelfContinuation":{"contents":[],"continuations":[{"nextContinuationData":{"continuation":"same-token"}}]}}}"#,
        )
        .unwrap();

        assert!(result.songs.is_empty());
        assert!(result.items.is_empty());
        assert_eq!(result.continuation, None);
    }

    #[test]
    fn browse_continuation_merges_unique_tracks_and_related_items() {
        let mut page = parse_browse_response(
            PLAYLIST_FIXTURE,
            browse_item(BrowseKind::Playlist, "VL-playlist-one", "Playlist"),
        )
        .unwrap();
        let continuation = parse_browse_continuation_response(BROWSE_CONTINUATION_FIXTURE).unwrap();

        assert_eq!(continuation.songs.len(), 2);
        assert_eq!(continuation.items.len(), 1);
        assert_eq!(
            continuation.continuation.as_deref(),
            Some("browse-continuation-two")
        );
        assert_eq!(page.append_continuation(continuation), 2);

        assert_eq!(
            page.songs
                .iter()
                .map(|song| song.video_id.as_str())
                .collect::<Vec<_>>(),
            [
                "playlist-song-one",
                "playlist-song-two",
                "playlist-song-three"
            ]
        );
        assert_eq!(page.related.len(), 1);
        assert_eq!(page.related[0].browse_id, "MPRE-continuation-related");
        assert_eq!(
            page.continuation.as_deref(),
            Some("browse-continuation-two")
        );
    }

    #[test]
    fn empty_continuation_page_drops_a_repeated_token() {
        let continuation = parse_browse_continuation_response(
            br#"{"continuationContents":{"musicShelfContinuation":{"contents":[],"continuations":[{"nextContinuationData":{"continuation":"same-token"}}]}}}"#,
        )
        .unwrap();

        assert!(continuation.songs.is_empty());
        assert!(continuation.items.is_empty());
        assert_eq!(continuation.continuation, None);
    }

    #[test]
    fn selects_the_highest_bitrate_direct_aac_lc_stream() {
        let resolved = parse_player_response(PLAYER_FIXTURE).unwrap();

        assert_eq!(resolved.source.mime_type, "audio/mp4; codecs=\"mp4a.40.2\"");
        assert_eq!(resolved.source.content_length, Some(4_200_000));
        assert_eq!(resolved.source.loudness_lufs_mb, Some(-1_325));
        assert_eq!(resolved.expires_in, Duration::from_secs(21_540));
        assert!(resolved.source.url.contains("selected-aac"));
    }

    #[test]
    fn loudness_prefers_perceptual_metadata_and_validates_the_fallback() {
        assert_eq!(
            measured_loudness_lufs_mb(Some(-14.25), Some(-5.0)),
            Some(-1_425)
        );
        assert_eq!(measured_loudness_lufs_mb(None, Some(-6.0)), Some(-1_300));
        assert_eq!(measured_loudness_lufs_mb(Some(f64::NAN), None), None);
        assert_eq!(measured_loudness_lufs_mb(Some(-101.0), None), None);
        assert_eq!(measured_loudness_lufs_mb(None, Some(28.0)), None);
        assert_eq!(measured_loudness_lufs_mb(None, None), None);
    }

    #[test]
    fn parses_account_menu_and_sends_redacted_authenticated_headers_with_retry() {
        let parsed = parse_account_info_response(ACCOUNT_FIXTURE).unwrap();
        assert_eq!(parsed.name, "Fixture Listener");
        assert_eq!(parsed.email.as_deref(), Some("listener@example.invalid"));
        assert_eq!(parsed.channel_handle.as_deref(), Some("@fixture-listener"));
        assert_eq!(
            parsed.thumbnail_url.as_deref(),
            Some("https://example.invalid/avatar-large.jpg")
        );
        assert!(matches!(
            parse_account_info_response(br#"{}"#).unwrap_err(),
            AppError::SessionExpired(_)
        ));

        let auth = AuthSession::from_import(
            "***INNERTUBE COOKIE*** = SID=fixture; SAPISID=sapisid-secret\n***VISITOR DATA*** = visitor-fixture\n***DATA SYNC ID*** = sync-fixture",
        )
        .unwrap();
        let http = AccountHttpClient::new();
        let client = InnerTubeClient::new(
            InnerTubeSession::default().with_auth(Some(auth)),
            http.clone(),
            AudioQuality::Auto,
        );
        let profile = futures::executor::block_on(client.account_info()).unwrap();

        assert_eq!(profile, parsed);
        assert_eq!(http.attempts.load(Ordering::Relaxed), 2);
        let captured = *http.captured.lock().unwrap();
        assert!(captured.account_uri);
        assert!(captured.cookie_matches);
        assert!(captured.authorization_shape);
        assert!(captured.origin_matches);
        assert!(captured.auth_user_matches);
        assert!(captured.visitor_matches);
    }

    #[test]
    fn authenticated_session_keeps_search_requests_anonymous() {
        let auth = AuthSession::from_import(
            "***INNERTUBE COOKIE*** = SID=fixture; SAPISID=sapisid-secret\n***VISITOR DATA*** = visitor-fixture",
        )
        .unwrap();
        let http = SearchPrivacyHttpClient::new();
        let client = InnerTubeClient::new(
            InnerTubeSession::default().with_auth(Some(auth)),
            http.clone(),
            AudioQuality::Auto,
        );

        let result = futures::executor::block_on(client.search_songs("fixture")).unwrap();

        assert!(!result.songs.is_empty());
        assert!(http.no_login_headers.load(Ordering::Relaxed));
    }

    #[test]
    fn authenticated_mutations_use_android_request_shapes_and_normalize_playlist_ids() {
        let auth = AuthSession::from_import(
            "***INNERTUBE COOKIE*** = SID=fixture; SAPISID=sapisid-secret\n***VISITOR DATA*** = visitor-fixture\n***DATA SYNC ID*** = sync-fixture",
        )
        .unwrap();
        let http = MutationHttpClient::new([]);
        let client = InnerTubeClient::new(
            InnerTubeSession::default().with_auth(Some(auth)),
            http.clone(),
            AudioQuality::Auto,
        );

        futures::executor::block_on(async {
            client.set_video_liked("video-one", true).await.unwrap();
            client
                .set_playlist_liked("VLPL-liked", false)
                .await
                .unwrap();
            client
                .set_channel_subscribed("UC-channel", true, None)
                .await
                .unwrap();
            assert_eq!(
                client.create_playlist("Fixture cloud list").await.unwrap(),
                "PL-created-fixture"
            );
            client
                .add_video_to_playlist("VLPL-owned", "video-two")
                .await
                .unwrap();
            client
                .remove_video_from_playlist("PL-owned", "video-two", "set-video-two")
                .await
                .unwrap();
            client
                .rename_playlist("VLPL-owned", "Renamed cloud list")
                .await
                .unwrap();
            client.delete_playlist("VLPL-owned").await.unwrap();
            client
                .remove_history_item("history-feedback-token")
                .await
                .unwrap();
            client
                .set_podcast_saved("MPSPpodcast-fixture", true)
                .await
                .unwrap();
            client
                .set_episode_saved("episode-for-later", true)
                .await
                .unwrap();
        });

        let captured = http.captured.lock().unwrap();
        assert_eq!(captured.len(), 11);
        assert!(captured.iter().all(|request| request.authenticated));
        assert!(captured.iter().all(|request| request.method == "POST"));
        assert!(captured.iter().all(|request| {
            request.body["context"]["user"]["onBehalfOfUser"] == "sync-fixture"
        }));
        assert_eq!(captured[0].path, "/youtubei/v1/like/like");
        assert_eq!(captured[0].body["target"]["videoId"], "video-one");
        assert_eq!(captured[1].path, "/youtubei/v1/like/removelike");
        assert_eq!(captured[1].body["target"]["playlistId"], "PL-liked");
        assert_eq!(
            captured[2].body["channelIds"],
            serde_json::json!(["UC-channel"])
        );
        assert_eq!(captured[2].body["params"], "EgIIAhgA");
        assert_eq!(captured[3].path, "/youtubei/v1/playlist/create");
        assert_eq!(captured[3].body["privacyStatus"], "PRIVATE");
        assert_eq!(captured[4].body["playlistId"], "PL-owned");
        assert_eq!(captured[4].body["actions"][0]["action"], "ACTION_ADD_VIDEO");
        assert_eq!(captured[4].body["actions"][0]["addedVideoId"], "video-two");
        assert_eq!(
            captured[5].body["actions"][0]["action"],
            "ACTION_REMOVE_VIDEO"
        );
        assert_eq!(
            captured[5].body["actions"][0]["setVideoId"],
            "set-video-two"
        );
        assert_eq!(
            captured[6].body["actions"][0]["action"],
            "ACTION_SET_PLAYLIST_NAME"
        );
        assert_eq!(
            captured[6].body["actions"][0]["playlistName"],
            "Renamed cloud list"
        );
        assert_eq!(captured[7].path, "/youtubei/v1/playlist/delete");
        assert_eq!(captured[7].body["playlistId"], "PL-owned");
        assert_eq!(captured[8].path, "/youtubei/v1/feedback");
        assert_eq!(
            captured[8].body["feedbackTokens"],
            serde_json::json!(["history-feedback-token"])
        );
        assert_eq!(captured[9].path, "/youtubei/v1/like/like");
        assert_eq!(captured[9].body["target"]["playlistId"], "podcast-fixture");
        assert_eq!(captured[10].path, "/youtubei/v1/browse/edit_playlist");
        assert_eq!(captured[10].body["playlistId"], "SE");
        assert_eq!(
            captured[10].body["actions"][0]["addedVideoId"],
            "episode-for-later"
        );
    }

    #[test]
    fn playback_registration_uses_a_redacted_trusted_url_and_stable_retry_cpn() {
        let resolved = parse_player_response(PLAYER_FIXTURE).unwrap();
        let tracking = resolved
            .playback_tracking()
            .expect("fixture should carry a trusted playback URL");
        let debug = format!("{resolved:?}");
        assert!(!debug.contains("redacted-fixture"));
        assert!(!debug.contains("s.youtube.com"));

        let auth = AuthSession::from_import("SID=fixture; SAPISID=sapisid-secret").unwrap();
        let http = MutationHttpClient::new([StatusCode::SERVICE_UNAVAILABLE, StatusCode::OK]);
        let client = InnerTubeClient::new(
            InnerTubeSession::default().with_auth(Some(auth)),
            http.clone(),
            AudioQuality::Auto,
        );
        futures::executor::block_on(client.register_playback(tracking)).unwrap();

        assert_eq!(http.attempts.load(Ordering::Relaxed), 2);
        let captured = http.captured.lock().unwrap();
        assert_eq!(captured.len(), 2);
        assert!(captured.iter().all(|request| request.method == "GET"));
        assert!(captured.iter().all(|request| request.authenticated));
        assert!(captured.iter().all(|request| request.body.is_null()));
        assert!(
            captured
                .iter()
                .all(|request| request.path == "/api/stats/playback")
        );
        let query = captured
            .iter()
            .map(|request| {
                Url::parse(&format!(
                    "https://s.youtube.com{}?{}",
                    request.path,
                    request.query.as_deref().unwrap_or_default()
                ))
                .unwrap()
            })
            .map(|url| {
                url.query_pairs()
                    .map(|(key, value)| (key.into_owned(), value.into_owned()))
                    .collect::<std::collections::HashMap<_, _>>()
            })
            .collect::<Vec<_>>();
        assert_eq!(query[0].get("c").map(String::as_str), Some(CLIENT_NAME));
        assert_eq!(
            query[0].get("ver").map(String::as_str),
            Some(PLAYBACK_TRACKING_VERSION)
        );
        assert_eq!(query[0].get("cpn"), query[1].get("cpn"));
        assert_eq!(query[0]["cpn"].len(), 16);
        assert!(
            query[0]["cpn"]
                .bytes()
                .all(|byte| CPN_ALPHABET.contains(&byte))
        );
        assert!(
            validated_playback_tracking_url(
                "https://attacker.invalid/api/stats/playback?token=secret"
            )
            .is_err()
        );
        assert!(
            validated_playback_tracking_url("http://s.youtube.com/api/stats/playback?token=secret")
                .is_err()
        );
    }

    #[test]
    fn only_idempotent_mutations_retry_transient_http_failures() {
        let auth = AuthSession::from_import("SID=fixture; SAPISID=sapisid-secret").unwrap();
        let safe_http = MutationHttpClient::new([StatusCode::SERVICE_UNAVAILABLE, StatusCode::OK]);
        let safe_client = InnerTubeClient::new(
            InnerTubeSession::default().with_auth(Some(auth.clone())),
            safe_http.clone(),
            AudioQuality::Auto,
        );
        futures::executor::block_on(safe_client.set_video_liked("video-one", true)).unwrap();
        assert_eq!(safe_http.attempts.load(Ordering::Relaxed), 2);

        let unsafe_http =
            MutationHttpClient::new([StatusCode::SERVICE_UNAVAILABLE, StatusCode::OK]);
        let unsafe_client = InnerTubeClient::new(
            InnerTubeSession::default().with_auth(Some(auth)),
            unsafe_http.clone(),
            AudioQuality::Auto,
        );
        let error =
            futures::executor::block_on(unsafe_client.create_playlist("No duplicate")).unwrap_err();
        assert!(matches!(error, AppError::Network(_)));
        assert!(error.to_string().contains("remote outcome is unknown"));
        assert!(error.to_string().contains("refresh before retrying"));
        assert_eq!(unsafe_http.attempts.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn mutations_require_authentication_and_reject_invalid_inputs_before_network() {
        let http = MutationHttpClient::new([]);
        let anonymous = InnerTubeClient::new(
            InnerTubeSession::default(),
            http.clone(),
            AudioQuality::Auto,
        );

        let auth_error =
            futures::executor::block_on(anonymous.set_video_liked("video", true)).unwrap_err();
        assert!(matches!(auth_error, AppError::Credential(_)));
        let title_error = futures::executor::block_on(anonymous.create_playlist("\n")).unwrap_err();
        assert!(matches!(title_error, AppError::Protocol(_)));
        let id_error = futures::executor::block_on(anonymous.add_video_to_playlist("VL", "video"))
            .unwrap_err();
        assert!(matches!(id_error, AppError::Protocol(_)));
        assert_eq!(http.attempts.load(Ordering::Relaxed), 0);

        let auth = AuthSession::from_import("SID=fixture; SAPISID=sapisid-secret").unwrap();
        let rejected_http = MutationHttpClient::new([StatusCode::UNAUTHORIZED]);
        let rejected = InnerTubeClient::new(
            InnerTubeSession::default().with_auth(Some(auth)),
            rejected_http,
            AudioQuality::Auto,
        );
        let rejected_error =
            futures::executor::block_on(rejected.set_video_liked("video", true)).unwrap_err();
        assert!(matches!(rejected_error, AppError::SessionExpired(_)));
    }

    #[test]
    fn authenticated_browse_rejection_is_reported_as_expired_credentials() {
        let auth = AuthSession::from_import("SID=fixture; SAPISID=sapisid-secret").unwrap();
        let http = MutationHttpClient::new([StatusCode::UNAUTHORIZED]);
        let client = InnerTubeClient::new(
            InnerTubeSession::default().with_auth(Some(auth)),
            http.clone(),
            AudioQuality::Auto,
        );

        let error =
            futures::executor::block_on(client.library_page("FEmusic_liked_videos", "Liked songs"))
                .unwrap_err();

        assert!(matches!(error, AppError::SessionExpired(_)));
        assert_eq!(http.attempts.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn authenticated_library_snapshot_follows_continuations_and_deduplicates_tracks() {
        let auth = AuthSession::from_import("SID=fixture; SAPISID=sapisid-secret").unwrap();
        let http = LibraryHttpClient::new();
        let client = InnerTubeClient::new(
            InnerTubeSession::default().with_auth(Some(auth)),
            http.clone(),
            AudioQuality::Auto,
        );
        let page = futures::executor::block_on(
            client.completed_library_page("FEmusic_liked_videos", "Liked songs"),
        )
        .unwrap();

        assert_eq!(
            page.songs
                .iter()
                .map(|song| song.video_id.as_str())
                .collect::<Vec<_>>(),
            ["album-song-one", "album-song-two", "playlist-song-three"]
        );
        assert_eq!(http.attempts.load(Ordering::Relaxed), 2);
        assert!(http.every_request_authenticated.load(Ordering::Relaxed));
        assert!(page.continuation.is_none());

        let anonymous = InnerTubeClient::new(InnerTubeSession::default(), http, AudioQuality::Auto);
        let error = futures::executor::block_on(
            anonymous.library_page("FEmusic_liked_videos", "Liked songs"),
        )
        .err()
        .unwrap();
        assert!(matches!(error, AppError::Credential(_)));
    }

    #[test]
    fn authenticated_podcast_snapshot_merges_shows_channels_and_saved_episodes() {
        let auth = AuthSession::from_import("SID=fixture; SAPISID=sapisid-secret").unwrap();
        let http = PodcastLibraryHttpClient::new();
        let client = InnerTubeClient::new(
            InnerTubeSession::default().with_auth(Some(auth)),
            http.clone(),
            AudioQuality::Auto,
        );

        let library = futures::executor::block_on(client.podcast_library_snapshot()).unwrap();

        assert_eq!(library.podcasts.len(), 2);
        assert!(
            library
                .podcasts
                .iter()
                .all(|podcast| podcast.kind == BrowseKind::Podcast)
        );
        assert!(
            library
                .podcasts
                .iter()
                .any(|podcast| podcast.browse_id == "MPSPfixture-podcast")
        );
        assert!(
            library
                .podcasts
                .iter()
                .any(|podcast| podcast.browse_id == "UCfixture-podcast-channel")
        );
        assert_eq!(library.episodes.len(), 2);
        assert!(
            library
                .episodes
                .iter()
                .all(|episode| episode.song.is_episode && episode.set_video_id.is_some())
        );
        let browse_ids = http.browse_ids.lock().unwrap();
        assert_eq!(browse_ids.len(), 3);
        assert!(browse_ids.contains(&"FEmusic_library_non_music_audio_list".into()));
        assert!(browse_ids.contains(&"FEmusic_library_non_music_audio_channels_list".into()));
        assert!(browse_ids.contains(&"VLSE".into()));
        assert!(http.every_request_authenticated.load(Ordering::Relaxed));
    }

    #[test]
    fn authenticated_history_snapshot_follows_continuations_without_merging_replays() {
        let auth = AuthSession::from_import("SID=fixture; SAPISID=sapisid-secret").unwrap();
        let http = HistoryHttpClient::new();
        let client = InnerTubeClient::new(
            InnerTubeSession::default().with_auth(Some(auth)),
            http.clone(),
            AudioQuality::Auto,
        );

        let page = futures::executor::block_on(client.completed_history_page()).unwrap();

        assert_eq!(page.entry_count(), 5);
        assert_eq!(
            page.songs()
                .iter()
                .filter(|song| song.video_id == "history-song-one")
                .count(),
            2
        );
        assert!(page.continuation.is_none());
        assert_eq!(http.attempts.load(Ordering::Relaxed), 2);
        assert!(http.every_request_authenticated.load(Ordering::Relaxed));
    }

    #[test]
    fn parses_radio_panel_selection_automix_related_and_radio_continuation() {
        let endpoint = RadioEndpoint::song_radio("seed-song").unwrap();
        let parsed = parse_radio_response(RADIO_INITIAL_FIXTURE, endpoint.clone()).unwrap();

        assert_eq!(parsed.page.title.as_deref(), Some("Fixture radio"));
        assert_eq!(parsed.page.current_index, Some(0));
        assert_eq!(parsed.page.continuation.as_deref(), Some("radio-token-one"));
        assert_eq!(parsed.page.endpoint, endpoint);
        assert_eq!(
            parsed
                .page
                .songs
                .iter()
                .map(|song| song.video_id.as_str())
                .collect::<Vec<_>>(),
            ["seed-song", "recommended-one"]
        );
        assert_eq!(
            parsed.page.songs[1].duration,
            Some(Duration::from_secs(242))
        );
        assert_eq!(
            parsed
                .page
                .recommendations_after_current("seed-song")
                .into_iter()
                .map(|song| song.video_id)
                .collect::<Vec<_>>(),
            ["recommended-one"]
        );
        assert_eq!(
            parsed.automix_endpoint,
            Some(RadioEndpoint {
                video_id: None,
                playlist_id: Some("RDAMVM-automix".into()),
                playlist_set_video_id: None,
                params: Some("wAEB".into()),
                index: None,
            })
        );
        assert_eq!(
            parsed.page.related_endpoint,
            Some(RelatedEndpoint {
                browse_id: "MPTR-related".into(),
                params: Some("related-params".into()),
            })
        );
    }

    #[test]
    fn radio_continuation_merge_deduplicates_and_keeps_the_next_token() {
        let endpoint = RadioEndpoint {
            video_id: None,
            playlist_id: Some("RDAMVM-automix".into()),
            playlist_set_video_id: None,
            params: Some("wAEB".into()),
            index: None,
        };
        let mut initial = parse_radio_response(
            RADIO_INITIAL_FIXTURE,
            RadioEndpoint::song_radio("seed-song").unwrap(),
        )
        .unwrap()
        .page;
        let continuation = parse_radio_response(RADIO_CONTINUATION_FIXTURE, endpoint.clone())
            .unwrap()
            .page;

        assert_eq!(initial.append_unique(continuation), 1);
        assert_eq!(initial.endpoint, endpoint);
        assert_eq!(initial.continuation.as_deref(), Some("radio-token-two"));
        assert_eq!(
            initial
                .songs
                .iter()
                .map(|song| song.video_id.as_str())
                .collect::<Vec<_>>(),
            ["seed-song", "recommended-one", "recommended-two"]
        );
    }

    #[test]
    fn next_body_carries_the_endpoint_and_continuation_in_json() {
        let endpoint = RadioEndpoint::song_radio("seed-song").unwrap();
        let body = serde_json::to_value(NextBody {
            context: RequestContext {
                client: ClientContext {
                    client_name: CLIENT_NAME,
                    client_version: CLIENT_VERSION,
                    language: "en",
                    region: "US",
                    visitor_data: Some("visitor-fixture"),
                },
                request: RequestMetadata { use_ssl: true },
                user: UserContext {
                    locked_safety_mode: false,
                    on_behalf_of_user: None,
                },
            },
            endpoint: &endpoint,
            continuation: Some("radio token / +="),
        })
        .unwrap();

        assert_eq!(body["videoId"], "seed-song");
        assert_eq!(body["playlistId"], "RDAMVMseed-song");
        assert_eq!(body["continuation"], "radio token / +=");
        assert_eq!(body["context"]["client"]["visitorData"], "visitor-fixture");
    }

    #[test]
    fn radio_segment_retries_transient_failures_and_stops_after_four_attempts() {
        let endpoint = RadioEndpoint::song_radio("seed-song").unwrap();
        let flaky = FlakyRadioClient::new(Some(3));
        let client = InnerTubeClient::new(
            InnerTubeSession::default(),
            flaky.clone(),
            AudioQuality::Auto,
        );
        let page = futures::executor::block_on(client.fetch_radio_segment(&endpoint, None))
            .unwrap()
            .page;

        assert_eq!(flaky.attempts.load(Ordering::Relaxed), 3);
        assert!(page.has_recommendation_for("seed-song"));

        let offline = FlakyRadioClient::new(None);
        let client = InnerTubeClient::new(
            InnerTubeSession::default(),
            offline.clone(),
            AudioQuality::Auto,
        );
        let error = futures::executor::block_on(client.fetch_radio_segment(&endpoint, None))
            .err()
            .unwrap();
        assert!(matches!(error, AppError::Network(_)));
        assert_eq!(offline.attempts.load(Ordering::Relaxed), RADIO_MAX_ATTEMPTS);
    }

    #[test]
    #[ignore = "requires live anonymous YouTube Music radio access"]
    fn anonymous_live_radio_returns_recommendations_and_can_continue() {
        let client = InnerTubeClient::anonymous(InnerTubeSession::default());
        let page = futures::executor::block_on(client.radio("dQw4w9WgXcQ")).unwrap();
        assert!(page.songs.len() > 1);
        assert!(page.songs.iter().any(|song| song.video_id != "dQw4w9WgXcQ"));
        if let Some(continuation) = page.continuation.clone() {
            let next = futures::executor::block_on(
                client.radio_continuation(page.endpoint, &continuation),
            )
            .unwrap();
            assert!(!next.songs.is_empty());
        }
    }

    #[test]
    fn low_quality_caps_direct_aac_lc_at_128_kbps() {
        let resolved =
            parse_player_response_with_quality(PLAYER_FIXTURE, AudioQuality::Low).unwrap();

        assert!(resolved.source.url.contains("low-direct-aac"));
        assert_eq!(resolved.source.content_length, Some(3_100_000));
    }

    #[test]
    #[ignore = "requires live access to music.youtube.com"]
    fn anonymous_live_search_returns_real_songs() {
        let client = InnerTubeClient::anonymous(InnerTubeSession::default());
        let result = futures::executor::block_on(client.search_songs("Daft Punk")).unwrap();

        assert!(!result.songs.is_empty());
        assert!(
            result
                .songs
                .iter()
                .all(|song| !song.video_id.is_empty() && !song.artists.is_empty())
        );
    }

    #[test]
    #[ignore = "requires live access to YouTube Music search suggestions"]
    fn anonymous_live_search_suggestions_return_supported_models() {
        let client = InnerTubeClient::anonymous(InnerTubeSession::default());
        let result = futures::executor::block_on(client.search_suggestions("Daft Punk")).unwrap();

        assert!(
            !result.queries.is_empty() || !result.songs.is_empty() || !result.items.is_empty(),
            "the suggestion endpoint returned no supported suggestions"
        );
        assert!(result.queries.iter().all(|query| !query.trim().is_empty()));
        assert!(
            result
                .songs
                .iter()
                .all(|song| !song.video_id.is_empty() && !song.title.is_empty())
        );
        assert!(
            result
                .items
                .iter()
                .all(|item| !item.browse_id.is_empty() && !item.title.is_empty())
        );
    }

    #[test]
    #[ignore = "requires live anonymous Home and Explore access"]
    fn anonymous_live_home_and_explore_return_shelves() {
        let client = InnerTubeClient::anonymous(InnerTubeSession::default());
        let (home, explore) = futures::executor::block_on(async {
            futures::join!(client.home(None), client.explore())
        });
        let home = home.unwrap();
        let explore = explore.unwrap();

        assert!(!home.sections.is_empty());
        assert!(
            home.sections
                .iter()
                .all(|section| { !section.title.is_empty() && !section.items.is_empty() })
        );
        assert!(
            !explore.new_release_albums.is_empty() || !explore.categories.is_empty(),
            "Explore returned no supported shelves"
        );
        assert!(explore.new_release_albums.iter().all(|album| {
            album.kind == BrowseKind::Album
                && !album.browse_id.is_empty()
                && !album.title.is_empty()
        }));
        assert!(
            explore
                .categories
                .iter()
                .all(|category| { !category.browse_id.is_empty() && !category.title.is_empty() })
        );
    }

    #[test]
    #[ignore = "requires live catalog search and browse access"]
    fn anonymous_live_catalog_filters_open_pages_with_playable_tracks() {
        let client = InnerTubeClient::anonymous(InnerTubeSession::default());
        for (filter, kind) in [
            (SearchFilter::Albums, BrowseKind::Album),
            (SearchFilter::Artists, BrowseKind::Artist),
            (SearchFilter::Playlists, BrowseKind::Playlist),
        ] {
            let result = futures::executor::block_on(client.search("Daft Punk", filter)).unwrap();
            if let Some(continuation) = result.continuation.as_deref() {
                let continued =
                    futures::executor::block_on(client.search_continuation(continuation)).unwrap();
                assert!(
                    continued
                        .songs
                        .iter()
                        .all(|song| { !song.video_id.is_empty() && !song.title.is_empty() })
                );
                assert!(
                    continued
                        .items
                        .iter()
                        .all(|item| { !item.browse_id.is_empty() && !item.title.is_empty() })
                );
            }
            let item = result
                .items
                .into_iter()
                .find(|item| item.kind == kind)
                .unwrap();
            let page = futures::executor::block_on(client.browse(&item)).unwrap();

            assert!(!page.item.title.is_empty());
            assert!(!page.songs.is_empty(), "{} page had no songs", kind.label());
            assert!(page.songs.iter().all(|song| {
                !song.video_id.is_empty() && !song.title.is_empty() && !song.artists.is_empty()
            }));
            if let Some(continuation) = page.continuation.as_deref() {
                let continued =
                    futures::executor::block_on(client.browse_continuation(continuation)).unwrap();
                assert!(
                    continued
                        .songs
                        .iter()
                        .all(|song| !song.video_id.is_empty() && !song.title.is_empty())
                );
                assert!(
                    continued
                        .items
                        .iter()
                        .all(|item| !item.browse_id.is_empty() && !item.title.is_empty())
                );
            }
        }
    }

    #[test]
    #[ignore = "requires live anonymous podcast search and browse access"]
    fn anonymous_live_podcast_and_episode_filters_open_playable_catalog() {
        let client = InnerTubeClient::anonymous(InnerTubeSession::default());
        let podcasts =
            futures::executor::block_on(client.search("Radiolab", SearchFilter::Podcasts)).unwrap();
        let podcast = podcasts
            .items
            .into_iter()
            .find(|item| item.kind == BrowseKind::Podcast)
            .expect("podcast filter returned no podcast");
        let page = futures::executor::block_on(client.browse(&podcast)).unwrap();
        assert_eq!(page.item.kind, BrowseKind::Podcast);
        assert!(!page.item.title.is_empty());
        assert!(!page.songs.is_empty(), "podcast page returned no episodes");
        assert!(page.songs.iter().all(|episode| {
            episode.is_episode
                && !episode.video_id.is_empty()
                && !episode.title.is_empty()
                && !episode.artists.is_empty()
        }));

        let episodes =
            futures::executor::block_on(client.search("Radiolab", SearchFilter::Episodes)).unwrap();
        assert!(
            !episodes.songs.is_empty(),
            "episode filter returned no episodes"
        );
        assert!(episodes.songs.iter().all(|episode| {
            episode.is_episode
                && !episode.video_id.is_empty()
                && !episode.title.is_empty()
                && !episode.artists.is_empty()
        }));
        let resolved = futures::executor::block_on(
            client.resolve_playback_source(&episodes.songs[0].video_id),
        )
        .unwrap();
        assert!(resolved.source.mime_type.starts_with("audio/"));
        assert!(
            resolved
                .source
                .content_length
                .is_some_and(|length| length > 0)
        );
    }

    #[test]
    #[ignore = "downloads and decodes a live YouTube Music audio stream"]
    fn live_player_source_ranges_and_decodes_as_aac() {
        let client = InnerTubeClient::anonymous(InnerTubeSession::default());
        let resolved =
            futures::executor::block_on(client.resolve_playback_source("FGBhQbmPwH8")).unwrap();
        assert!(resolved.source.mime_type.starts_with("audio/mp4"));
        assert!(resolved.playback_tracking().is_some());

        let source = client.open_playback_source(resolved.source);
        let decoded = probe_audio_source(Box::new(source), Some("m4a")).unwrap();
        assert!(decoded.sample_rate > 0);
        assert!(decoded.channels > 0);
        assert!(decoded.decoded_frames >= 4_096);
    }

    #[test]
    #[ignore = "opens the default output device and plays a muted live stream"]
    fn live_audio_backend_advances_playback_position() {
        let client = InnerTubeClient::anonymous(InnerTubeSession::default());
        let resolved =
            futures::executor::block_on(client.resolve_playback_source("FGBhQbmPwH8")).unwrap();
        let mut player = DesktopAudioPlayer::new();
        player.set_volume(0.0);
        player.load(resolved.source).unwrap();
        player.play().unwrap();

        let deadline = std::time::Instant::now() + Duration::from_secs(20);
        loop {
            let snapshot = player.snapshot();
            if snapshot.state == PlaybackState::Failed {
                panic!("audio backend failed: {:?}", snapshot.error);
            }
            if snapshot.state == PlaybackState::Playing
                && snapshot.position >= Duration::from_millis(250)
            {
                player.pause().unwrap();
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "audio backend did not advance before the deadline; last state: {snapshot:?}"
            );
            std::thread::sleep(Duration::from_millis(100));
        }

        player.seek(Duration::from_secs(30)).unwrap();
        player.play().unwrap();
        let seek_deadline = std::time::Instant::now() + Duration::from_secs(10);
        loop {
            let snapshot = player.snapshot();
            if snapshot.state == PlaybackState::Failed {
                panic!("audio seek failed: {:?}", snapshot.error);
            }
            if snapshot.state == PlaybackState::Playing
                && snapshot.position >= Duration::from_millis(30_250)
            {
                player.pause().unwrap();
                break;
            }
            assert!(
                std::time::Instant::now() < seek_deadline,
                "audio backend did not advance after seek; last state: {snapshot:?}"
            );
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    #[test]
    #[ignore = "opens real output devices and switches one while playing a live stream"]
    fn live_audio_output_switch_preserves_playback() {
        let client = InnerTubeClient::anonymous(InnerTubeSession::default());
        let resolved =
            futures::executor::block_on(client.resolve_playback_source("FGBhQbmPwH8")).unwrap();
        let mut player = DesktopAudioPlayer::new();
        player.set_volume(0.0);
        player.refresh_output_devices().unwrap();

        let device_deadline = std::time::Instant::now() + Duration::from_secs(10);
        let devices = loop {
            let snapshot = player.device_snapshot();
            if snapshot.operation == AudioDeviceOperation::Idle {
                if let Some(error) = snapshot.error {
                    panic!("audio output enumeration failed: {error}");
                }
                assert!(!snapshot.devices.is_empty());
                break snapshot;
            }
            assert!(
                std::time::Instant::now() < device_deadline,
                "audio output enumeration did not finish; last state: {snapshot:?}"
            );
            std::thread::sleep(Duration::from_millis(50));
        };
        player.load(resolved.source).unwrap();
        player.play().unwrap();
        let playback_deadline = std::time::Instant::now() + Duration::from_secs(20);
        let before = loop {
            let snapshot = player.snapshot();
            if snapshot.state == PlaybackState::Failed {
                panic!(
                    "audio backend failed before switching: {:?}",
                    snapshot.error
                );
            }
            if snapshot.state == PlaybackState::Playing
                && snapshot.position >= Duration::from_millis(250)
            {
                break snapshot.position;
            }
            assert!(
                std::time::Instant::now() < playback_deadline,
                "audio backend did not advance before switching; last state: {snapshot:?}"
            );
            std::thread::sleep(Duration::from_millis(100));
        };

        let target = devices
            .devices
            .iter()
            .find(|device| Some(device.id.as_str()) != devices.selected_id.as_deref())
            .or_else(|| devices.devices.first())
            .unwrap();
        let target_id = target.id.clone();
        player.select_output_device(target_id.clone()).unwrap();

        let switch_deadline = std::time::Instant::now() + Duration::from_secs(20);
        loop {
            let snapshot = player.device_snapshot();
            if snapshot.operation == AudioDeviceOperation::Idle {
                assert_eq!(snapshot.error, None);
                assert_eq!(snapshot.selected_id.as_deref(), Some(target_id.as_str()));
                break;
            }
            assert!(
                std::time::Instant::now() < switch_deadline,
                "audio output switch did not finish; last state: {snapshot:?}"
            );
            std::thread::sleep(Duration::from_millis(50));
        }

        let resume_deadline = std::time::Instant::now() + Duration::from_secs(20);
        loop {
            let snapshot = player.snapshot();
            if snapshot.state == PlaybackState::Failed {
                panic!("audio backend failed after switching: {:?}", snapshot.error);
            }
            if snapshot.state == PlaybackState::Playing && snapshot.position > before {
                player.pause().unwrap();
                break;
            }
            assert!(
                std::time::Instant::now() < resume_deadline,
                "playback did not resume after switching; last state: {snapshot:?}"
            );
            std::thread::sleep(Duration::from_millis(100));
        }
    }
}
