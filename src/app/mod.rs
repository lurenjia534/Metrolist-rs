mod router;

pub use router::Route;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppModel {
    route: Route,
    search_query: String,
}

impl AppModel {
    pub fn new(route: Route) -> Self {
        Self {
            route,
            search_query: String::new(),
        }
    }

    pub fn route(&self) -> Route {
        self.route
    }

    pub fn navigate(&mut self, route: Route) {
        self.route = route;
    }

    pub fn search_query(&self) -> &str {
        &self.search_query
    }

    pub fn set_search_query(&mut self, query: impl Into<String>) {
        self.search_query = query.into();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn navigation_updates_the_active_route() {
        let mut model = AppModel::new(Route::Home);
        model.navigate(Route::Search);
        assert_eq!(model.route(), Route::Search);
    }

    #[test]
    fn search_query_survives_navigation() {
        let mut model = AppModel::new(Route::Search);
        model.set_search_query("Porter Robinson");
        model.navigate(Route::Library);
        model.navigate(Route::Search);
        assert_eq!(model.search_query(), "Porter Robinson");
    }
}
