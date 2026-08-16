use std::fmt;

use crate::domain::Song;

#[derive(Clone, PartialEq, Eq)]
pub struct RemoteHistoryEntry {
    pub song: Song,
    pub feedback_token: Option<String>,
}

impl fmt::Debug for RemoteHistoryEntry {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RemoteHistoryEntry")
            .field("song", &self.song)
            .field("has_feedback_token", &self.feedback_token.is_some())
            .finish()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteHistorySection {
    pub title: String,
    pub entries: Vec<RemoteHistoryEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RemoteHistoryPage {
    pub sections: Vec<RemoteHistorySection>,
    pub continuation: Option<String>,
}

impl RemoteHistoryPage {
    pub fn entry_count(&self) -> usize {
        self.sections
            .iter()
            .map(|section| section.entries.len())
            .sum()
    }

    pub fn songs(&self) -> Vec<Song> {
        self.sections
            .iter()
            .flat_map(|section| section.entries.iter())
            .map(|entry| entry.song.clone())
            .collect()
    }

    pub fn append(&mut self, next: Self) -> usize {
        let before = self.entry_count();
        for next_section in next.sections {
            let section = if let Some(section) = self
                .sections
                .iter_mut()
                .find(|section| section.title == next_section.title)
            {
                section
            } else {
                self.sections.push(RemoteHistorySection {
                    title: next_section.title.clone(),
                    entries: Vec::new(),
                });
                self.sections
                    .last_mut()
                    .expect("a history section was just appended")
            };
            for entry in next_section.entries {
                let duplicate = section.entries.iter().any(|existing| {
                    match (&existing.feedback_token, &entry.feedback_token) {
                        (Some(existing), Some(candidate)) => existing == candidate,
                        (None, None) => existing.song.video_id == entry.song.video_id,
                        _ => false,
                    }
                });
                if !duplicate {
                    section.entries.push(entry);
                }
            }
        }
        self.continuation = next.continuation;
        self.entry_count() - before
    }

    pub fn remove_feedback_token(&mut self, token: &str) -> bool {
        let before = self.entry_count();
        for section in &mut self.sections {
            section
                .entries
                .retain(|entry| entry.feedback_token.as_deref() != Some(token));
        }
        self.sections.retain(|section| !section.entries.is_empty());
        self.entry_count() != before
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::ArtistCredit;

    fn entry(video_id: &str, token: Option<&str>) -> RemoteHistoryEntry {
        RemoteHistoryEntry {
            song: Song {
                video_id: video_id.into(),
                title: format!("Song {video_id}"),
                artists: vec![ArtistCredit {
                    id: None,
                    name: "Fixture Artist".into(),
                }],
                duration: None,
                thumbnail_url: None,
                album: None,
                is_episode: false,
            },
            feedback_token: token.map(str::to_owned),
        }
    }

    #[test]
    fn continuation_deduplicates_stable_tokens_but_preserves_distinct_replays() {
        let mut page = RemoteHistoryPage {
            sections: vec![RemoteHistorySection {
                title: "Today".into(),
                entries: vec![entry("same-song", Some("token-one"))],
            }],
            continuation: Some("one".into()),
        };
        let added = page.append(RemoteHistoryPage {
            sections: vec![RemoteHistorySection {
                title: "Today".into(),
                entries: vec![
                    entry("same-song", Some("token-one")),
                    entry("same-song", Some("token-two")),
                ],
            }],
            continuation: None,
        });

        assert_eq!(added, 1);
        assert_eq!(page.entry_count(), 2);
        assert_eq!(page.songs()[0].video_id, "same-song");
        assert_eq!(page.songs()[1].video_id, "same-song");
    }

    #[test]
    fn debug_output_redacts_feedback_tokens_and_removal_is_exact() {
        let mut page = RemoteHistoryPage {
            sections: vec![RemoteHistorySection {
                title: "Today".into(),
                entries: vec![entry("one", Some("sensitive-token"))],
            }],
            continuation: None,
        };

        assert!(!format!("{page:?}").contains("sensitive-token"));
        assert!(!page.remove_feedback_token("different-token"));
        assert!(page.remove_feedback_token("sensitive-token"));
        assert_eq!(page.entry_count(), 0);
    }
}
