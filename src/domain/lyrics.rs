use std::time::Duration;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LyricsLine {
    pub start: Option<Duration>,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LyricsDocument {
    pub provider: String,
    pub lines: Vec<LyricsLine>,
}

impl LyricsDocument {
    pub fn synced(provider: impl Into<String>, lines: Vec<LyricsLine>) -> Option<Self> {
        (!lines.is_empty() && lines.iter().all(|line| line.start.is_some())).then(|| Self {
            provider: provider.into(),
            lines,
        })
    }

    pub fn plain(provider: impl Into<String>, text: &str) -> Option<Self> {
        let lines = text
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .map(|text| LyricsLine {
                start: None,
                text: text.to_owned(),
            })
            .collect::<Vec<_>>();
        (!lines.is_empty()).then(|| Self {
            provider: provider.into(),
            lines,
        })
    }

    pub fn is_synced(&self) -> bool {
        !self.lines.is_empty() && self.lines.iter().all(|line| line.start.is_some())
    }

    pub fn active_line_index(&self, position: Duration) -> Option<usize> {
        if !self.is_synced() {
            return None;
        }
        let threshold = position.saturating_add(Duration::from_millis(100));
        self.lines
            .partition_point(|line| line.start.is_some_and(|start| start <= threshold))
            .checked_sub(1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(milliseconds: u64, text: &str) -> LyricsLine {
        LyricsLine {
            start: Some(Duration::from_millis(milliseconds)),
            text: text.into(),
        }
    }

    #[test]
    fn active_line_uses_the_android_compatible_early_threshold() {
        let document = LyricsDocument::synced(
            "fixture",
            vec![line(1_000, "one"), line(2_000, "two"), line(3_000, "three")],
        )
        .unwrap();

        assert_eq!(document.active_line_index(Duration::from_millis(899)), None);
        assert_eq!(
            document.active_line_index(Duration::from_millis(900)),
            Some(0)
        );
        assert_eq!(
            document.active_line_index(Duration::from_millis(1_950)),
            Some(1)
        );
        assert_eq!(document.active_line_index(Duration::from_secs(20)), Some(2));
    }

    #[test]
    fn plain_lyrics_do_not_claim_a_timeline() {
        let document = LyricsDocument::plain("fixture", " first \n\n second ").unwrap();
        assert!(!document.is_synced());
        assert_eq!(document.lines.len(), 2);
        assert_eq!(document.active_line_index(Duration::from_secs(1)), None);
    }
}
