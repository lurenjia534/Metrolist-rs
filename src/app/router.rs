#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Route {
    #[default]
    Home,
    Explore,
    Search,
    Recognition,
    History,
    Stats,
    Library,
    Settings,
}

impl Route {
    pub const ALL: [Self; 8] = [
        Self::Home,
        Self::Explore,
        Self::Search,
        Self::Recognition,
        Self::History,
        Self::Stats,
        Self::Library,
        Self::Settings,
    ];

    pub const fn title(self) -> &'static str {
        match self {
            Self::Home => "Home",
            Self::Explore => "Explore",
            Self::Search => "Search",
            Self::Recognition => "Recognize",
            Self::History => "History",
            Self::Stats => "Stats",
            Self::Library => "Library",
            Self::Settings => "Settings",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_route_has_a_non_empty_title() {
        for route in Route::ALL {
            assert!(!route.title().is_empty());
        }
    }
}
