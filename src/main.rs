use gpui::*;
use gpui_component::{Root, Theme, ThemeMode};
use gpui_component_assets::Assets;
use metrolist_rs::{
    AppConfig, AppSettings, AppTheme, Result,
    services::{
        CredentialStore, DISCORD_APPLICATION_ID, DesktopServices, DiscordPresenceService,
        LastFmApiCredentials, LastFmClient, LastFmCredentialStore, LastFmSystemCredentialStore,
        ListenTogetherClient, SystemCredentialStore, account_login_helper_requested,
        run_account_login_helper,
    },
    storage::DesktopStore,
    ui::{AccountBootstrap, IntegrationBootstrap, LastFmBootstrap, MetrolistShell},
};
use std::sync::Arc;
use tracing_subscriber::EnvFilter;

fn main() {
    if account_login_helper_requested() {
        run_account_login_helper();
    }
    if let Err(error) = run() {
        eprintln!("Metrolist could not start: {error}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    init_logging();
    let config = AppConfig::default().validate()?;
    let store = DesktopStore::open_default()?;
    let settings = futures::executor::block_on(store.load_settings())?
        .unwrap_or(AppSettings::for_current_user(config.audio_cache_bytes)?);
    let credential_store: Arc<dyn CredentialStore> = Arc::new(SystemCredentialStore::default());
    let (auth_session, credential_warning) = match credential_store.load() {
        Ok(session) => (session, None),
        Err(error) => {
            tracing::warn!(%error, "system credential store unavailable; continuing anonymously");
            (None, Some(error.to_string()))
        }
    };
    let services = DesktopServices::with_settings_and_auth(&settings, auth_session.clone())?;
    let account = AccountBootstrap::new(credential_store, auth_session, credential_warning);
    let lastfm_credential_store: Arc<dyn LastFmCredentialStore> =
        Arc::new(LastFmSystemCredentialStore::default());
    let mut lastfm_warning = None;
    let lastfm_credentials = match LastFmApiCredentials::from_environment() {
        Ok(credentials) => credentials,
        Err(error) => {
            tracing::warn!(%error, "Last.fm application credentials are incomplete");
            lastfm_warning = Some(error.to_string());
            None
        }
    };
    let lastfm_session = match lastfm_credential_store.load() {
        Ok(session) => session,
        Err(error) => {
            tracing::warn!(%error, "Last.fm credential store unavailable");
            lastfm_warning = Some(error.to_string());
            None
        }
    };
    let lastfm_client = lastfm_credentials
        .clone()
        .map(|credentials| {
            LastFmClient::with_proxy(credentials, lastfm_session.clone(), &settings.proxy)
        })
        .transpose()?;
    let lastfm = LastFmBootstrap::new(
        lastfm_credential_store,
        lastfm_credentials,
        lastfm_client,
        lastfm_session,
        lastfm_warning,
    );
    let (discord_presence, discord_warning) =
        match DiscordPresenceService::new(DISCORD_APPLICATION_ID) {
            Ok(service) => (Some(service), None),
            Err(error) => {
                tracing::warn!(%error, "Discord Rich Presence worker unavailable");
                (None, Some(error.to_string()))
            }
        };
    let (listen_together, listen_together_warning) =
        match ListenTogetherClient::new(settings.listen_together.server_url.clone()) {
            Ok(client) => (Some(client), None),
            Err(error) => {
                tracing::warn!(%error, "Listen Together worker unavailable");
                (None, Some(error.to_string()))
            }
        };
    let integrations = IntegrationBootstrap::new(
        account,
        lastfm,
        discord_presence,
        discord_warning,
        listen_together,
        listen_together_warning,
    );
    let app = gpui_platform::application().with_assets(Assets);

    app.run(move |cx| {
        gpui_component::init(cx);

        let window_options = WindowOptions {
            window_bounds: Some(WindowBounds::centered(
                size(px(config.window_width), px(config.window_height)),
                cx,
            )),
            ..Default::default()
        };

        cx.spawn(async move |cx| {
            cx.open_window(window_options, |window, cx| {
                window.activate_window();
                window.set_window_title("Metrolist");
                let theme_mode = match settings.theme {
                    AppTheme::Light => ThemeMode::Light,
                    AppTheme::Dark => ThemeMode::Dark,
                };
                Theme::change(theme_mode, Some(window), cx);

                let view = cx.new(|cx| {
                    MetrolistShell::new(
                        config.initial_route,
                        settings,
                        services,
                        store.clone(),
                        integrations,
                        window,
                        cx,
                    )
                });
                cx.new(|cx| Root::new(view, window, cx))
            })
            .expect("failed to open the Metrolist window");
        })
        .detach();
    });

    Ok(())
}

fn init_logging() {
    let filter = EnvFilter::try_from_env("METROLIST_LOG")
        .unwrap_or_else(|_| EnvFilter::new("metrolist_rs=info"));
    let _ = tracing_subscriber::fmt().with_env_filter(filter).try_init();
}
