use crate::domain::{BrowseItem, HomeItem, HomePage, HomeSection, Song};

pub const MUSIC_VIDEO_TYPE_ATV: &str = "MUSIC_VIDEO_TYPE_ATV";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ContentFilters {
    pub hide_explicit: bool,
    pub hide_video_songs: bool,
    pub hide_youtube_shorts: bool,
}

impl ContentFilters {
    pub fn keep_song(self, song: &Song) -> bool {
        if self.hide_explicit && song.explicit {
            return false;
        }
        if self.hide_video_songs && song.is_video_song() {
            return false;
        }
        true
    }

    pub fn keep_browse_item(self, item: &BrowseItem) -> bool {
        if self.hide_explicit && item.explicit {
            return false;
        }
        if self.hide_youtube_shorts && item.is_youtube_shorts_playlist() {
            return false;
        }
        true
    }

    pub fn keep_home_item(self, item: &HomeItem) -> bool {
        match item {
            HomeItem::Song(song) => self.keep_song(song),
            HomeItem::Browse(browse) => self.keep_browse_item(browse),
        }
    }

    pub fn songs<I>(self, songs: I) -> Vec<Song>
    where
        I: IntoIterator<Item = Song>,
    {
        songs
            .into_iter()
            .filter(|song| self.keep_song(song))
            .collect()
    }

    pub fn browse_items<I>(self, items: I) -> Vec<BrowseItem>
    where
        I: IntoIterator<Item = BrowseItem>,
    {
        items
            .into_iter()
            .filter(|item| self.keep_browse_item(item))
            .collect()
    }

    pub fn home_items<I>(self, items: I) -> Vec<HomeItem>
    where
        I: IntoIterator<Item = HomeItem>,
    {
        items
            .into_iter()
            .filter(|item| self.keep_home_item(item))
            .collect()
    }

    pub fn home_section(self, mut section: HomeSection) -> HomeSection {
        section.items = self.home_items(section.items);
        section
    }

    pub fn home_page(self, mut page: HomePage) -> HomePage {
        page.sections = page
            .sections
            .into_iter()
            .map(|section| self.home_section(section))
            .collect();
        page
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{BrowseKind, Song};

    fn song(id: &str, explicit: bool, music_video_type: Option<&str>) -> Song {
        Song {
            video_id: id.into(),
            title: id.into(),
            artists: Vec::new(),
            duration: None,
            thumbnail_url: None,
            album: None,
            is_episode: false,
            explicit,
            music_video_type: music_video_type.map(str::to_owned),
        }
    }

    fn playlist(id: &str) -> BrowseItem {
        BrowseItem {
            browse_id: id.into(),
            kind: BrowseKind::Playlist,
            title: id.into(),
            subtitle: "Playlist".into(),
            thumbnail_url: None,
            params: None,
            editable: false,
            explicit: false,
        }
    }

    fn album(id: &str, explicit: bool) -> BrowseItem {
        BrowseItem {
            browse_id: id.into(),
            kind: BrowseKind::Album,
            title: id.into(),
            subtitle: "Album".into(),
            thumbnail_url: None,
            params: None,
            editable: false,
            explicit,
        }
    }

    #[test]
    fn android_rules_keep_unflagged_items_and_drop_only_matching_flags() {
        let songs = vec![
            song("plain", false, None),
            song("atv", false, Some(MUSIC_VIDEO_TYPE_ATV)),
            song("omv", false, Some("MUSIC_VIDEO_TYPE_OMV")),
            song("explicit", true, None),
        ];
        let items = vec![
            playlist("RDregular"),
            playlist("SS123"),
            playlist("VLSS456"),
            album("MPREclean", false),
            album("MPREdirty", true),
        ];

        let off = ContentFilters::default();
        assert_eq!(off.songs(songs.clone()), songs);
        assert_eq!(off.browse_items(items.clone()), items);

        let hide_explicit = ContentFilters {
            hide_explicit: true,
            ..ContentFilters::default()
        };
        assert_eq!(
            hide_explicit
                .songs(songs.clone())
                .into_iter()
                .map(|song| song.video_id)
                .collect::<Vec<_>>(),
            vec!["plain", "atv", "omv"]
        );
        assert_eq!(
            hide_explicit
                .browse_items(items.clone())
                .into_iter()
                .map(|item| item.browse_id)
                .collect::<Vec<_>>(),
            vec!["RDregular", "SS123", "VLSS456", "MPREclean"]
        );

        let hide_videos = ContentFilters {
            hide_video_songs: true,
            ..ContentFilters::default()
        };
        assert_eq!(
            hide_videos
                .songs(songs)
                .into_iter()
                .map(|song| song.video_id)
                .collect::<Vec<_>>(),
            vec!["plain", "atv", "explicit"]
        );

        let hide_shorts = ContentFilters {
            hide_youtube_shorts: true,
            ..ContentFilters::default()
        };
        assert_eq!(
            hide_shorts
                .browse_items(items)
                .into_iter()
                .map(|item| item.browse_id)
                .collect::<Vec<_>>(),
            vec!["RDregular", "MPREclean", "MPREdirty"]
        );
    }
}
