use std::{cmp::Ordering, collections::HashSet, sync::Arc, time::Duration};

use futures::AsyncReadExt as _;
use http_client::{
    AsyncBody, HttpClient, HttpRequestExt as _, RedirectPolicy, Request, StatusCode, Url,
};
use serde::Deserialize;

use crate::domain::{LyricsDocument, LyricsLine, Song};
use crate::services::build_http_client;
use crate::{AppError, AppSettings, ProxySettings, Result};

const LRCLIB_SEARCH_URL: &str = "https://lrclib.net/api/search";
const LRCLIB_PROVIDER: &str = "LRCLIB";
const MAX_RESPONSE_BYTES: u64 = 4 * 1024 * 1024;

#[derive(Clone)]
pub struct LyricsClient {
    http: Arc<dyn HttpClient>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct LrcLibTrack {
    id: u64,
    track_name: String,
    artist_name: String,
    #[serde(default)]
    duration: f64,
    plain_lyrics: Option<String>,
    synced_lyrics: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum LyricsQuery {
    Metadata { title: String, artist: String },
    Title(String),
    FreeText(String),
}

impl LyricsClient {
    pub fn anonymous() -> Self {
        let http = build_http_client(
            &ProxySettings::default(),
            concat!("Metrolist-rs/", env!("CARGO_PKG_VERSION")),
        )
        .expect("the built-in HTTP configuration must be valid");
        Self::new(http)
    }

    pub fn with_settings(settings: &AppSettings) -> Result<Self> {
        Ok(Self::new(build_http_client(
            &settings.proxy,
            concat!("Metrolist-rs/", env!("CARGO_PKG_VERSION")),
        )?))
    }

    pub fn new(http: Arc<dyn HttpClient>) -> Self {
        Self { http }
    }

    pub async fn lyrics_for_song(&self, song: &Song) -> Result<Option<LyricsDocument>> {
        let title = clean_title(&song.title);
        let artist = song
            .artists
            .first()
            .map(|artist| clean_artist(&artist.name))
            .unwrap_or_default();
        if title.is_empty() || artist.is_empty() {
            return Ok(None);
        }

        let mut queries = vec![
            LyricsQuery::Metadata {
                title: title.clone(),
                artist: artist.clone(),
            },
            LyricsQuery::Title(title.clone()),
            LyricsQuery::FreeText(format!("{artist} {title}")),
            LyricsQuery::FreeText(title.clone()),
        ];
        let original_title = song.title.trim();
        let original_artist = song
            .artists
            .first()
            .map(|artist| artist.name.trim())
            .unwrap_or_default();
        if original_title != title || original_artist != artist {
            queries.push(LyricsQuery::Metadata {
                title: original_title.to_owned(),
                artist: original_artist.to_owned(),
            });
        }

        let mut seen = HashSet::new();
        let mut successful_request = false;
        let mut last_error = None;
        for query in queries
            .into_iter()
            .filter(|query| seen.insert(query.clone()))
        {
            match self.search(query).await {
                Ok(tracks) => {
                    successful_request = true;
                    if let Some(track) = best_track(&tracks, &title, &artist, song.duration)
                        && let Some(document) = document_from_track(track)
                    {
                        return Ok(Some(document));
                    }
                }
                Err(error) => last_error = Some(error),
            }
        }
        if successful_request {
            Ok(None)
        } else {
            Err(last_error
                .unwrap_or_else(|| AppError::Network("every lyrics lookup failed".into())))
        }
    }

    async fn search(&self, query: LyricsQuery) -> Result<Vec<LrcLibTrack>> {
        let mut url =
            Url::parse(LRCLIB_SEARCH_URL).map_err(|error| AppError::Protocol(error.to_string()))?;
        {
            let mut pairs = url.query_pairs_mut();
            match query {
                LyricsQuery::Metadata { title, artist } => {
                    pairs.append_pair("track_name", &title);
                    pairs.append_pair("artist_name", &artist);
                }
                LyricsQuery::Title(title) => {
                    pairs.append_pair("track_name", &title);
                }
                LyricsQuery::FreeText(query) => {
                    pairs.append_pair("q", &query);
                }
            }
        }
        let request = Request::builder()
            .uri(url.as_str())
            .header("Accept", "application/json")
            .follow_redirects(RedirectPolicy::FollowLimit(3))
            .timeout(Duration::from_secs(8))
            .body(AsyncBody::default())
            .map_err(|error| AppError::Network(error.to_string()))?;
        let mut response = self
            .http
            .send(request)
            .await
            .map_err(|error| AppError::Network(error.to_string()))?;
        if response.status() != StatusCode::OK {
            return Err(AppError::Network(format!(
                "LRCLIB search returned HTTP {}",
                response.status()
            )));
        }
        let mut body = Vec::new();
        response
            .body_mut()
            .take(MAX_RESPONSE_BYTES + 1)
            .read_to_end(&mut body)
            .await
            .map_err(|error| AppError::Network(error.to_string()))?;
        if body.len() as u64 > MAX_RESPONSE_BYTES {
            return Err(AppError::Protocol(
                "LRCLIB response exceeded the safety limit".into(),
            ));
        }
        serde_json::from_slice(&body).map_err(|error| AppError::Protocol(error.to_string()))
    }
}

fn best_track<'a>(
    tracks: &'a [LrcLibTrack],
    title: &str,
    artist: &str,
    duration: Option<Duration>,
) -> Option<&'a LrcLibTrack> {
    let mut candidates = tracks
        .iter()
        .filter(|track| track.synced_lyrics.is_some() || track.plain_lyrics.is_some())
        .filter_map(|track| {
            let title_score = string_similarity(title, &track.track_name);
            let artist_score = string_similarity(artist, &track.artist_name);
            let metadata_score = (title_score + artist_score) / 2.0;
            let duration_difference =
                duration.map(|duration| (track.duration - duration.as_secs_f64()).abs());
            if duration_difference.is_some_and(|difference| difference > 5.0)
                || (duration.is_none() && metadata_score <= 0.6)
            {
                return None;
            }
            Some((
                track,
                metadata_score,
                duration_difference.unwrap_or_default(),
            ))
        })
        .collect::<Vec<_>>();
    candidates.sort_by(|left, right| {
        right
            .0
            .synced_lyrics
            .is_some()
            .cmp(&left.0.synced_lyrics.is_some())
            .then_with(|| left.2.partial_cmp(&right.2).unwrap_or(Ordering::Equal))
            .then_with(|| right.1.partial_cmp(&left.1).unwrap_or(Ordering::Equal))
            .then_with(|| left.0.id.cmp(&right.0.id))
    });
    candidates.first().map(|candidate| candidate.0)
}

fn document_from_track(track: &LrcLibTrack) -> Option<LyricsDocument> {
    track
        .synced_lyrics
        .as_deref()
        .and_then(|lyrics| LyricsDocument::synced(LRCLIB_PROVIDER, parse_lrc(lyrics)))
        .or_else(|| {
            track
                .plain_lyrics
                .as_deref()
                .and_then(|lyrics| LyricsDocument::plain(LRCLIB_PROVIDER, lyrics))
        })
}

pub fn parse_lrc(lyrics: &str) -> Vec<LyricsLine> {
    let lyrics = lyrics.trim_start_matches('\u{feff}');
    let mut offset_ms = 0_i64;
    let mut parsed = Vec::<(u64, usize, String)>::new();
    for (source_index, source_line) in lyrics.lines().enumerate() {
        let mut rest = source_line.trim();
        let mut timestamps = Vec::new();
        while let Some(tag) = rest.strip_prefix('[').and_then(|line| {
            let end = line.find(']')?;
            Some((&line[..end], &line[end + 1..]))
        }) {
            let (tag, remaining) = tag;
            rest = remaining.trim_start();
            if let Some(offset) = tag
                .strip_prefix("offset:")
                .or_else(|| tag.strip_prefix("OFFSET:"))
                .and_then(|offset| offset.trim().parse::<i64>().ok())
            {
                offset_ms = offset;
            } else if let Some(timestamp) = parse_lrc_timestamp(tag) {
                timestamps.push(timestamp);
            }
        }
        let text = decode_basic_html_entities(rest.trim());
        if text.is_empty() {
            continue;
        }
        for timestamp in timestamps {
            parsed.push((timestamp, source_index, text.clone()));
        }
    }
    parsed.sort_by_key(|(timestamp, source_index, _)| (*timestamp, *source_index));
    parsed.dedup_by(|left, right| left.0 == right.0 && left.2 == right.2);
    parsed
        .into_iter()
        .map(|(timestamp, _, text)| LyricsLine {
            start: Some(Duration::from_millis(
                (timestamp as i128 + offset_ms as i128).clamp(0, u64::MAX as i128) as u64,
            )),
            text,
        })
        .collect()
}

fn parse_lrc_timestamp(tag: &str) -> Option<u64> {
    let (minutes, seconds) = tag.trim().split_once(':')?;
    if minutes.is_empty() || !minutes.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    let (seconds, fraction) = seconds
        .split_once(['.', ','])
        .map_or((seconds, ""), |parts| parts);
    if seconds.len() != 2
        || !seconds.bytes().all(|byte| byte.is_ascii_digit())
        || !fraction.bytes().all(|byte| byte.is_ascii_digit())
    {
        return None;
    }
    let minutes = minutes.parse::<u64>().ok()?;
    let seconds = seconds.parse::<u64>().ok()?;
    if seconds >= 60 {
        return None;
    }
    let milliseconds = match fraction.len() {
        0 => 0,
        1 => fraction.parse::<u64>().ok()? * 100,
        2 => fraction.parse::<u64>().ok()? * 10,
        _ => fraction[..3].parse::<u64>().ok()?,
    };
    minutes
        .checked_mul(60_000)?
        .checked_add(seconds * 1_000)?
        .checked_add(milliseconds)
}

fn clean_title(title: &str) -> String {
    let mut cleaned = title.trim().to_owned();
    for (open, close) in [('(', ')'), ('[', ']')] {
        loop {
            let lower = cleaned.to_ascii_lowercase();
            let Some(start) = lower.find(open) else {
                break;
            };
            let Some(relative_end) = lower[start + 1..].find(close) else {
                break;
            };
            let end = start + 1 + relative_end;
            let section = &lower[start + 1..end];
            if title_noise(section) || section.starts_with("feat.") || section.starts_with("ft.") {
                cleaned.replace_range(start..=end, "");
            } else {
                break;
            }
        }
    }
    while let Some(start) = cleaned.find('【') {
        let Some(relative_end) = cleaned[start..].find('】') else {
            break;
        };
        let end = start + relative_end + '】'.len_utf8();
        cleaned.replace_range(start..end, "");
    }
    if let Some(index) = cleaned.find('|') {
        cleaned.truncate(index);
    }
    let lower = cleaned.to_ascii_lowercase();
    for marker in [" - official", " feat.", " ft."] {
        if let Some(index) = lower.find(marker) {
            cleaned.truncate(index);
            break;
        }
    }
    cleaned.trim().to_owned()
}

fn title_noise(section: &str) -> bool {
    [
        "official",
        "video",
        "audio",
        "lyrics",
        "lyric",
        "visualizer",
        "hd",
        "hq",
        "4k",
        "remaster",
        "remix",
        "live",
        "acoustic",
        "version",
        "edit",
        "extended",
        "radio",
        "clean",
        "explicit",
    ]
    .iter()
    .any(|marker| section.contains(marker))
}

fn clean_artist(artist: &str) -> String {
    let lower = artist.to_ascii_lowercase();
    let split_at = [
        " & ",
        " and ",
        ", ",
        " x ",
        " feat. ",
        " feat ",
        " ft. ",
        " ft ",
        " featuring ",
        " with ",
    ]
    .into_iter()
    .filter_map(|separator| lower.find(separator))
    .min()
    .unwrap_or(artist.len());
    artist[..split_at].trim().to_owned()
}

fn string_similarity(left: &str, right: &str) -> f64 {
    let left = left.trim().to_lowercase();
    let right = right.trim().to_lowercase();
    if left == right {
        return 1.0;
    }
    if left.is_empty() || right.is_empty() {
        return 0.0;
    }
    let contains: f64 = if left.contains(&right) || right.contains(&left) {
        0.8
    } else {
        0.0
    };
    let distance = levenshtein_distance(&left, &right);
    let max_length = left.chars().count().max(right.chars().count());
    contains.max(1.0 - distance as f64 / max_length as f64)
}

fn levenshtein_distance(left: &str, right: &str) -> usize {
    let right = right.chars().collect::<Vec<_>>();
    let mut previous = (0..=right.len()).collect::<Vec<_>>();
    let mut current = vec![0; right.len() + 1];
    for (left_index, left_character) in left.chars().enumerate() {
        current[0] = left_index + 1;
        for (right_index, right_character) in right.iter().enumerate() {
            current[right_index + 1] = (previous[right_index + 1] + 1)
                .min(current[right_index] + 1)
                .min(previous[right_index] + usize::from(left_character != *right_character));
        }
        std::mem::swap(&mut previous, &mut current);
    }
    previous[right.len()]
}

fn decode_basic_html_entities(text: &str) -> String {
    text.replace("&amp;", "&")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&apos;", "'")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    use futures::future::BoxFuture;
    use http_client::{Response, http::HeaderValue};

    const LRCLIB_FIXTURE: &[u8] = include_bytes!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/lyrics/lrclib_search.json"
    ));

    struct MemoryLyricsHttpClient {
        bytes: Vec<u8>,
        uris: Mutex<Vec<String>>,
    }

    impl MemoryLyricsHttpClient {
        fn new(bytes: &[u8]) -> Arc<Self> {
            Arc::new(Self {
                bytes: bytes.to_vec(),
                uris: Mutex::new(Vec::new()),
            })
        }
    }

    impl HttpClient for MemoryLyricsHttpClient {
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
            self.uris.lock().unwrap().push(request.uri().to_string());
            let bytes = self.bytes.clone();
            Box::pin(async move {
                Ok(Response::builder()
                    .status(StatusCode::OK)
                    .header("Content-Type", "application/json")
                    .body(AsyncBody::from(bytes))
                    .unwrap())
            })
        }
    }

    fn song(title: &str, artist: &str, duration: Option<Duration>) -> Song {
        Song {
            video_id: "fixture-video".into(),
            title: title.into(),
            artists: vec![crate::domain::ArtistCredit {
                id: Some("UC-fixture".into()),
                name: artist.into(),
            }],
            duration,
            thumbnail_url: None,
            album: None,
            is_episode: false,
        }
    }

    #[test]
    fn lrc_parser_handles_metadata_offsets_multiple_tags_and_fractions() {
        let lines = parse_lrc(
            "\u{feff}[ar:Fixture]\n[offset:+50]\n[00:01.20][00:03.250]One &amp; two\n[01:02,5]Three\n[bad]ignored",
        );

        assert_eq!(lines.len(), 3);
        assert_eq!(lines[0].start, Some(Duration::from_millis(1_250)));
        assert_eq!(lines[0].text, "One & two");
        assert_eq!(lines[1].start, Some(Duration::from_millis(3_300)));
        assert_eq!(lines[2].start, Some(Duration::from_millis(62_550)));
    }

    #[test]
    fn title_and_artist_cleanup_matches_the_android_lookup_semantics() {
        assert_eq!(
            clean_title("Song (Official Video) feat. Guest | Label"),
            "Song"
        );
        assert_eq!(clean_title("歌曲【官方 MV】"), "歌曲");
        assert_eq!(clean_artist("First Artist & Guest, Third"), "First Artist");
    }

    #[test]
    fn duration_matching_prefers_synced_lyrics_within_five_seconds() {
        let tracks = vec![
            LrcLibTrack {
                id: 1,
                track_name: "Fixture Song".into(),
                artist_name: "Fixture Artist".into(),
                duration: 180.1,
                plain_lyrics: Some("plain".into()),
                synced_lyrics: None,
            },
            LrcLibTrack {
                id: 2,
                track_name: "Fixture Song".into(),
                artist_name: "Fixture Artist".into(),
                duration: 183.9,
                plain_lyrics: None,
                synced_lyrics: Some("[00:01.00]synced".into()),
            },
            LrcLibTrack {
                id: 3,
                track_name: "Fixture Song".into(),
                artist_name: "Fixture Artist".into(),
                duration: 186.0,
                plain_lyrics: None,
                synced_lyrics: Some("[00:01.00]too far".into()),
            },
        ];

        assert_eq!(
            best_track(
                &tracks,
                "Fixture Song",
                "Fixture Artist",
                Some(Duration::from_secs(180))
            )
            .unwrap()
            .id,
            2
        );
    }

    #[test]
    fn client_encodes_metadata_and_returns_a_parsed_synced_document() {
        let http = MemoryLyricsHttpClient::new(LRCLIB_FIXTURE);
        let client = LyricsClient::new(http.clone());
        let document = futures::executor::block_on(client.lyrics_for_song(&song(
            "Fixture Song (Official Video)",
            "Fixture Artist & Guest",
            Some(Duration::from_secs(180)),
        )))
        .unwrap()
        .unwrap();

        assert_eq!(document.provider, LRCLIB_PROVIDER);
        assert!(document.is_synced());
        assert_eq!(document.lines.len(), 2);
        assert_eq!(document.lines[1].start, Some(Duration::from_millis(3_250)));
        let uris = http.uris.lock().unwrap();
        assert_eq!(uris.len(), 1);
        let url = Url::parse(&uris[0]).unwrap();
        let pairs = url.query_pairs().collect::<Vec<_>>();
        assert!(pairs.contains(&("track_name".into(), "Fixture Song".into())));
        assert!(pairs.contains(&("artist_name".into(), "Fixture Artist".into())));
    }

    #[test]
    #[ignore = "requires live access to lrclib.net"]
    fn live_lrclib_returns_timed_lyrics_without_exposing_their_text() {
        let client = LyricsClient::anonymous();
        let document = futures::executor::block_on(client.lyrics_for_song(&song(
            "Never Gonna Give You Up",
            "Rick Astley",
            Some(Duration::from_secs(213)),
        )))
        .unwrap()
        .unwrap();

        assert!(document.is_synced());
        assert!(document.lines.len() > 10);
        assert!(
            document
                .lines
                .windows(2)
                .all(|pair| pair[0].start <= pair[1].start)
        );
    }
}
