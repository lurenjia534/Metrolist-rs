use crate::domain::{AlbumCredit, Song};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BrowseKind {
    Album,
    Artist,
    Playlist,
    Podcast,
    Category,
}

impl BrowseKind {
    pub const fn label(self) -> &'static str {
        match self {
            Self::Album => "Album",
            Self::Artist => "Artist",
            Self::Playlist => "Playlist",
            Self::Podcast => "Podcast",
            Self::Category => "Collection",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BrowseItem {
    pub browse_id: String,
    pub kind: BrowseKind,
    pub title: String,
    pub subtitle: String,
    pub thumbnail_url: Option<String>,
    pub params: Option<String>,
    pub editable: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlaylistEntry {
    pub song: Song,
    pub set_video_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChannelSubscription {
    pub channel_id: String,
    pub subscribed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BrowsePlaybackEndpoint {
    pub video_id: Option<String>,
    pub playlist_id: Option<String>,
    pub playlist_set_video_id: Option<String>,
    pub params: Option<String>,
    pub index: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BrowsePage {
    pub item: BrowseItem,
    pub playlist_id: Option<String>,
    pub shuffle_endpoint: Option<BrowsePlaybackEndpoint>,
    pub radio_endpoint: Option<BrowsePlaybackEndpoint>,
    pub description: Option<String>,
    pub subscriber_count: Option<String>,
    pub monthly_listener_count: Option<String>,
    pub songs: Vec<Song>,
    pub playlist_entries: Vec<PlaylistEntry>,
    pub related: Vec<BrowseItem>,
    pub section_links: Vec<BrowseItem>,
    pub creator_links: Vec<BrowseItem>,
    pub channel_subscription: Option<ChannelSubscription>,
    pub continuation: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BrowseContinuation {
    pub songs: Vec<Song>,
    pub playlist_entries: Vec<PlaylistEntry>,
    pub items: Vec<BrowseItem>,
    pub continuation: Option<String>,
}

impl BrowsePage {
    pub fn append_continuation(&mut self, next: BrowseContinuation) -> usize {
        let previous_song_count = self.songs.len();
        let previous_entry_count = self.playlist_entries.len();
        let previous_related_count = self.related.len();
        let album = (self.item.kind == BrowseKind::Album).then(|| AlbumCredit {
            browse_id: self.item.browse_id.clone(),
            title: self.item.title.clone(),
            thumbnail_url: self.item.thumbnail_url.clone(),
        });
        for mut song in next.songs {
            if song.album.is_none() {
                song.album.clone_from(&album);
            }
            if !self
                .songs
                .iter()
                .any(|existing| existing.video_id == song.video_id)
            {
                self.songs.push(song);
            }
        }
        for mut entry in next.playlist_entries {
            if entry.song.album.is_none() {
                entry.song.album.clone_from(&album);
            }
            if !self
                .playlist_entries
                .iter()
                .any(|existing| existing.set_video_id == entry.set_video_id)
            {
                self.playlist_entries.push(entry);
            }
        }
        for item in next.items {
            if item.browse_id != self.item.browse_id
                && !self
                    .related
                    .iter()
                    .any(|existing| existing.browse_id == item.browse_id)
            {
                self.related.push(item);
            }
        }
        self.continuation = next.continuation;
        (self.songs.len() - previous_song_count)
            .max(self.playlist_entries.len() - previous_entry_count)
            + (self.related.len() - previous_related_count)
    }
}
