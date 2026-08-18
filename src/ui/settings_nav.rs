/// Official Settings sidebar pages, in display order.
///
/// `settings_pages` in the shell builds one `SettingPage` per entry so the
/// searchable nav and this catalog cannot drift.
pub const SETTINGS_PAGES: &[(&str, &str)] = &[
    ("Account", "YouTube Music and Last.fm"),
    ("Connections", "Listen Together and Discord"),
    ("Playback", "Speed, timers, and queue behavior"),
    ("Audio", "Output, equalizer, and loudness"),
    ("Appearance", "Theme, locale, and content filters"),
    ("Network", "Proxy for Music, lyrics, and streams"),
    ("Library", "History, cache, and downloads"),
];

pub fn settings_page_title(index: usize) -> &'static str {
    SETTINGS_PAGES[index].0
}

pub fn settings_page_description(index: usize) -> &'static str {
    SETTINGS_PAGES[index].1
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_nav_lists_the_seven_official_pages() {
        let titles: Vec<_> = SETTINGS_PAGES.iter().map(|(title, _)| *title).collect();
        assert_eq!(
            titles,
            [
                "Account",
                "Connections",
                "Playback",
                "Audio",
                "Appearance",
                "Network",
                "Library"
            ]
        );
        let unique: std::collections::HashSet<_> = titles.iter().copied().collect();
        assert_eq!(unique.len(), titles.len());
        assert_eq!(settings_page_title(0), "Account");
        assert_eq!(
            settings_page_description(5),
            "Proxy for Music, lyrics, and streams"
        );
    }
}
