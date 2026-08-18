use std::collections::HashSet;

use crate::domain::{BrowseItem, BrowseKind, Song};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HomeChip {
    pub title: String,
    pub params: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HomeItem {
    Song(Song),
    Browse(BrowseItem),
}

impl HomeItem {
    fn stable_key(&self) -> String {
        match self {
            Self::Song(song) => format!("song:{}", song.video_id),
            Self::Browse(item) => format!("browse:{:?}:{}", item.kind, item.browse_id),
        }
    }

    pub fn thumbnail_url(&self) -> Option<&str> {
        match self {
            Self::Song(song) => song.thumbnail_url.as_deref(),
            Self::Browse(item) => item.thumbnail_url.as_deref(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HomeSection {
    pub title: String,
    pub label: Option<String>,
    pub thumbnail_url: Option<String>,
    pub more: Option<BrowseItem>,
    pub items: Vec<HomeItem>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HomePage {
    pub chips: Vec<HomeChip>,
    pub sections: Vec<HomeSection>,
    pub continuation: Option<String>,
}

impl HomePage {
    pub fn append_continuation(&mut self, mut next: Self) -> usize {
        let mut seen = self
            .sections
            .iter()
            .flat_map(|section| section.items.iter())
            .map(HomeItem::stable_key)
            .collect::<HashSet<_>>();
        let mut added = 0;
        for mut section in next.sections.drain(..) {
            section.items.retain(|item| seen.insert(item.stable_key()));
            added += section.items.len();
            if !section.items.is_empty() {
                self.sections.push(section);
            }
        }
        self.continuation = next.continuation;
        if added == 0 {
            self.continuation = None;
        }
        added
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExploreCategory {
    pub title: String,
    pub browse_id: String,
    pub params: Option<String>,
    pub stripe_color: Option<u32>,
}

impl ExploreCategory {
    pub fn browse_item(&self) -> BrowseItem {
        BrowseItem {
            browse_id: self.browse_id.clone(),
            kind: BrowseKind::Category,
            title: self.title.clone(),
            subtitle: "Mood & genre".into(),
            thumbnail_url: None,
            params: self.params.clone(),
            editable: false,
            explicit: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExplorePage {
    pub chart_sections: Vec<HomeSection>,
    pub new_release_albums: Vec<BrowseItem>,
    pub new_releases_more: Option<BrowseItem>,
    pub categories: Vec<ExploreCategory>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn song(id: &str) -> HomeItem {
        HomeItem::Song(Song {
            video_id: id.into(),
            title: id.into(),
            artists: Vec::new(),
            duration: None,
            thumbnail_url: None,
            album: None,
            is_episode: false,
            explicit: false,
            music_video_type: None,
        })
    }

    fn page(ids: &[&str], continuation: Option<&str>) -> HomePage {
        HomePage {
            chips: Vec::new(),
            sections: vec![HomeSection {
                title: "Shelf".into(),
                label: None,
                thumbnail_url: None,
                more: None,
                items: ids.iter().map(|id| song(id)).collect(),
            }],
            continuation: continuation.map(str::to_owned),
        }
    }

    #[test]
    fn home_continuation_deduplicates_items_and_stops_without_progress() {
        let mut current = page(&["one", "two"], Some("next"));
        assert_eq!(
            current.append_continuation(page(&["two", "three"], Some("last"))),
            1
        );
        assert_eq!(current.continuation.as_deref(), Some("last"));
        assert_eq!(
            current.append_continuation(page(&["three"], Some("repeat"))),
            0
        );
        assert_eq!(current.continuation, None);
    }
}
