use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::domain::MUSIC_VIDEO_TYPE_ATV;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArtistCredit {
    pub id: Option<String>,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AlbumCredit {
    pub browse_id: String,
    pub title: String,
    pub thumbnail_url: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Song {
    pub video_id: String,
    pub title: String,
    pub artists: Vec<ArtistCredit>,
    pub duration: Option<Duration>,
    pub thumbnail_url: Option<String>,
    pub album: Option<AlbumCredit>,
    pub is_episode: bool,
    pub explicit: bool,
    pub music_video_type: Option<String>,
}

impl Song {
    pub fn artist_line(&self) -> String {
        self.artists
            .iter()
            .map(|artist| artist.name.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    }

    pub fn is_video_song(&self) -> bool {
        self.music_video_type
            .as_deref()
            .is_some_and(|kind| kind != MUSIC_VIDEO_TYPE_ATV)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn artist_line_preserves_credit_order() {
        let song = Song {
            video_id: "video-id".into(),
            title: "Song".into(),
            artists: vec![
                ArtistCredit {
                    id: Some("first".into()),
                    name: "First Artist".into(),
                },
                ArtistCredit {
                    id: None,
                    name: "Guest Artist".into(),
                },
            ],
            duration: Some(Duration::from_secs(215)),
            thumbnail_url: None,
            album: None,
            is_episode: false,
            explicit: false,
            music_video_type: None,
        };

        assert_eq!(song.artist_line(), "First Artist, Guest Artist");
        assert!(!song.is_video_song());
        assert!(!song.explicit);
    }

    #[test]
    fn video_song_requires_a_non_atv_music_video_type() {
        let mut song = Song {
            video_id: "video-id".into(),
            title: "Song".into(),
            artists: Vec::new(),
            duration: None,
            thumbnail_url: None,
            album: None,
            is_episode: false,
            explicit: false,
            music_video_type: None,
        };
        assert!(!song.is_video_song());
        song.music_video_type = Some(MUSIC_VIDEO_TYPE_ATV.into());
        assert!(!song.is_video_song());
        song.music_video_type = Some("MUSIC_VIDEO_TYPE_OMV".into());
        assert!(song.is_video_song());
    }
}
