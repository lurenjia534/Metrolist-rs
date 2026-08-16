use std::{
    ffi::OsStr,
    io::{IsTerminal as _, Read as _, Write as _},
    process::{Child, Command, Stdio},
    time::Duration,
};

use gpui::BackgroundExecutor;
use http_client::Url;
use serde::{Deserialize, Serialize};
use tao::{
    dpi::LogicalSize,
    event::{Event, WindowEvent},
    event_loop::{ControlFlow, EventLoopBuilder},
    window::WindowBuilder,
};
use wry::{NewWindowResponse, PageLoadEvent, ProxyConfig, ProxyEndpoint, WebView, WebViewBuilder};
use zeroize::Zeroizing;

use crate::{AppError, ProxyKind, ProxySettings, Result};

use super::AuthSession;

const LOGIN_HELPER_ARGUMENT: &str = "--account-login-helper";
const LOGIN_URL: &str =
    "https://accounts.google.com/ServiceLogin?continue=https%3A%2F%2Fmusic.youtube.com";
const MUSIC_ORIGIN: &str = "https://music.youtube.com";
const PROXY_KIND_ENV: &str = "METROLIST_LOGIN_PROXY_KIND";
const PROXY_HOST_ENV: &str = "METROLIST_LOGIN_PROXY_HOST";
const PROXY_PORT_ENV: &str = "METROLIST_LOGIN_PROXY_PORT";
const MAX_HELPER_OUTPUT_BYTES: u64 = 96 * 1024;
const MAX_CONTEXT_MESSAGE_BYTES: usize = 16 * 1024;
const COMPLETION_GRACE_PERIOD: Duration = Duration::from_millis(600);

const READ_ACCOUNT_CONTEXT_SCRIPT: &str = r#"
(() => {
    try {
        const config = window.yt && window.yt.config_;
        if (!config || !window.ipc) return;
        window.ipc.postMessage(JSON.stringify({
            kind: "accountContext",
            visitorData: String(config.VISITOR_DATA || ""),
            dataSyncId: String(config.DATASYNC_ID || "").split("||")[0]
        }));
    } catch (_) {}
})();
"#;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct WebAccountContext {
    kind: String,
    #[serde(default)]
    visitor_data: String,
    #[serde(default)]
    data_sync_id: String,
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
enum LoginHelperOutput {
    Session {
        cookie: String,
        visitor_data: Option<String>,
        data_sync_id: Option<String>,
    },
    Cancelled,
    Error {
        message: String,
    },
}

enum LoginEvent {
    PageFinished(String),
    AccountContext(WebAccountContext),
    Complete,
}

struct LoginChild {
    child: Child,
    reaped: bool,
}

impl Drop for LoginChild {
    fn drop(&mut self) {
        if !self.reaped {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

pub fn account_login_helper_requested() -> bool {
    std::env::args_os().any(|argument| argument == OsStr::new(LOGIN_HELPER_ARGUMENT))
}

pub fn run_account_login_helper() -> ! {
    if std::io::stdout().is_terminal() {
        eprintln!("The Metrolist account login helper must be launched by the application.");
        std::process::exit(2);
    }

    match run_account_login_helper_inner() {
        Ok(()) => std::process::exit(0),
        Err(error) => {
            write_helper_output(&LoginHelperOutput::Error {
                message: error.to_string(),
            });
            std::process::exit(1);
        }
    }
}

pub async fn launch_account_login(
    proxy: ProxySettings,
    executor: BackgroundExecutor,
) -> Result<Option<AuthSession>> {
    let proxy = login_proxy(&proxy)?;
    let executable = std::env::current_exe().map_err(|error| {
        AppError::Credential(format!(
            "could not locate the Metrolist executable for sign-in: {error}"
        ))
    })?;
    let mut command = Command::new(executable);
    command
        .arg(LOGIN_HELPER_ARGUMENT)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .env_remove(PROXY_KIND_ENV)
        .env_remove(PROXY_HOST_ENV)
        .env_remove(PROXY_PORT_ENV);
    if let Some((kind, endpoint)) = proxy {
        command
            .env(PROXY_KIND_ENV, kind)
            .env(PROXY_HOST_ENV, endpoint.host)
            .env(PROXY_PORT_ENV, endpoint.port);
    }
    let child = command.spawn().map_err(|error| {
        AppError::Credential(format!(
            "could not open the YouTube Music sign-in window: {error}"
        ))
    })?;
    let mut child = LoginChild {
        child,
        reaped: false,
    };

    let status = loop {
        match child.child.try_wait().map_err(|error| {
            AppError::Credential(format!(
                "could not monitor the YouTube Music sign-in window: {error}"
            ))
        })? {
            Some(status) => break status,
            None => executor.timer(Duration::from_millis(100)).await,
        }
    };
    child.reaped = true;

    let mut stdout = child.child.stdout.take().ok_or_else(|| {
        AppError::Credential("the YouTube Music sign-in helper returned no result stream".into())
    })?;
    let mut encoded = Zeroizing::new(Vec::new());
    (&mut stdout)
        .take(MAX_HELPER_OUTPUT_BYTES + 1)
        .read_to_end(&mut encoded)
        .map_err(|error| {
            AppError::Credential(format!(
                "could not read the YouTube Music sign-in result: {error}"
            ))
        })?;
    if encoded.len() as u64 > MAX_HELPER_OUTPUT_BYTES {
        return Err(AppError::Credential(
            "the YouTube Music sign-in helper returned an oversized result".into(),
        ));
    }
    if encoded.is_empty() {
        return Err(AppError::Credential(format!(
            "the YouTube Music sign-in window exited unexpectedly ({status})"
        )));
    }
    let output: LoginHelperOutput = serde_json::from_slice(&encoded).map_err(|_| {
        AppError::Credential("the YouTube Music sign-in helper returned malformed data".into())
    })?;
    match output {
        LoginHelperOutput::Session {
            cookie,
            visitor_data,
            data_sync_id,
        } => AuthSession::from_parts(cookie, visitor_data, data_sync_id).map(Some),
        LoginHelperOutput::Cancelled => Ok(None),
        LoginHelperOutput::Error { message } => Err(AppError::Credential(message)),
    }
}

fn run_account_login_helper_inner() -> Result<()> {
    let event_loop = EventLoopBuilder::<LoginEvent>::with_user_event().build();
    let proxy = event_loop.create_proxy();
    let window = WindowBuilder::new()
        .with_title("Sign in to YouTube Music — Metrolist")
        .with_inner_size(LogicalSize::new(960.0, 760.0))
        .with_min_inner_size(LogicalSize::new(640.0, 520.0))
        .build(&event_loop)
        .map_err(|error| {
            AppError::Credential(format!("could not create the sign-in window: {error}"))
        })?;

    let page_proxy = proxy.clone();
    let context_proxy = proxy.clone();
    let mut builder = WebViewBuilder::new()
        .with_url(LOGIN_URL)
        .with_incognito(true)
        .with_clipboard(true)
        .with_general_autofill_enabled(true)
        .with_back_forward_navigation_gestures(true)
        .with_navigation_handler(|url| login_navigation_allowed(&url))
        .with_new_window_req_handler(|_, _| NewWindowResponse::Deny)
        .with_on_page_load_handler(move |event, url| {
            if matches!(event, PageLoadEvent::Finished) {
                let _ = page_proxy.send_event(LoginEvent::PageFinished(url));
            }
        })
        .with_ipc_handler(move |request| {
            if request.body().len() > MAX_CONTEXT_MESSAGE_BYTES
                || !music_url(request.uri().to_string().as_str())
            {
                return;
            }
            let Ok(context) = serde_json::from_str::<WebAccountContext>(request.body()) else {
                return;
            };
            if context.kind == "accountContext" {
                let _ = context_proxy.send_event(LoginEvent::AccountContext(context));
            }
        });
    if let Some(proxy) = helper_proxy_from_environment()? {
        builder = builder.with_proxy_config(proxy);
    }

    #[cfg(any(target_os = "windows", target_os = "macos"))]
    let webview = builder.build(&window).map_err(|error| {
        AppError::Credential(format!("could not initialize the system WebView: {error}"))
    })?;
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    let webview = {
        use tao::platform::unix::WindowExtUnix as _;
        use wry::WebViewBuilderExtUnix as _;

        let container = window.default_vbox().ok_or_else(|| {
            AppError::Credential("the sign-in window has no GTK container".into())
        })?;
        builder.build_gtk(container).map_err(|error| {
            AppError::Credential(format!("could not initialize WebKitGTK: {error}"))
        })?
    };
    let mut visitor_data = None;
    let mut data_sync_id = None;
    let mut completion_scheduled = false;
    let mut completed = false;
    event_loop.run(move |event, _, control_flow| {
        *control_flow = ControlFlow::Wait;
        if completed {
            *control_flow = ControlFlow::Exit;
            return;
        }

        match event {
            Event::UserEvent(LoginEvent::PageFinished(url)) if music_url(&url) => {
                let _ = webview.evaluate_script(READ_ACCOUNT_CONTEXT_SCRIPT);
                if webview_has_authenticated_cookie(&webview) && !completion_scheduled {
                    completion_scheduled = true;
                    let completion_proxy = proxy.clone();
                    std::thread::spawn(move || {
                        std::thread::sleep(COMPLETION_GRACE_PERIOD);
                        let _ = completion_proxy.send_event(LoginEvent::Complete);
                    });
                }
            }
            Event::UserEvent(LoginEvent::AccountContext(context)) => {
                visitor_data = nonempty(context.visitor_data);
                data_sync_id = nonempty(context.data_sync_id);
            }
            Event::UserEvent(LoginEvent::Complete) => {
                let output = session_output(&webview, visitor_data.take(), data_sync_id.take());
                write_helper_output(&output);
                completed = true;
                *control_flow = ControlFlow::Exit;
            }
            Event::WindowEvent {
                event: WindowEvent::CloseRequested,
                ..
            } => {
                let output = if webview_has_authenticated_cookie(&webview) {
                    session_output(&webview, visitor_data.take(), data_sync_id.take())
                } else {
                    LoginHelperOutput::Cancelled
                };
                write_helper_output(&output);
                completed = true;
                *control_flow = ControlFlow::Exit;
            }
            _ => {}
        }
    });
}

fn session_output(
    webview: &WebView,
    visitor_data: Option<String>,
    data_sync_id: Option<String>,
) -> LoginHelperOutput {
    match cookie_header(webview).and_then(|cookie| {
        AuthSession::from_parts(cookie.clone(), visitor_data.clone(), data_sync_id.clone())?;
        Ok(cookie)
    }) {
        Ok(cookie) => LoginHelperOutput::Session {
            cookie,
            visitor_data,
            data_sync_id,
        },
        Err(error) => LoginHelperOutput::Error {
            message: error.to_string(),
        },
    }
}

fn cookie_header(webview: &WebView) -> Result<String> {
    let cookies = webview.cookies_for_url(MUSIC_ORIGIN).map_err(|error| {
        AppError::Credential(format!(
            "could not read the authenticated YouTube Music session: {error}"
        ))
    })?;
    Ok(cookies
        .iter()
        .map(|cookie| format!("{}={}", cookie.name(), cookie.value()))
        .collect::<Vec<_>>()
        .join("; "))
}

fn webview_has_authenticated_cookie(webview: &WebView) -> bool {
    cookie_header(webview)
        .ok()
        .is_some_and(|cookie| cookie_has_sapisid(&cookie))
}

fn cookie_has_sapisid(cookie: &str) -> bool {
    cookie.split(';').any(|part| {
        part.trim()
            .split_once('=')
            .is_some_and(|(name, value)| name.trim() == "SAPISID" && !value.trim().is_empty())
    })
}

fn login_navigation_allowed(value: &str) -> bool {
    if matches!(value, "about:blank" | "about:srcdoc") {
        return true;
    }
    let Ok(url) = Url::parse(value) else {
        return false;
    };
    if url.scheme() != "https" {
        return false;
    }
    url.host_str().is_some_and(|host| {
        [
            "google.com",
            "youtube.com",
            "googleusercontent.com",
            "gstatic.com",
        ]
        .into_iter()
        .any(|domain| host == domain || host.ends_with(&format!(".{domain}")))
    })
}

fn music_url(value: &str) -> bool {
    Url::parse(value)
        .is_ok_and(|url| url.scheme() == "https" && url.host_str() == Some("music.youtube.com"))
}

fn nonempty(value: String) -> Option<String> {
    let trimmed = value.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_owned())
}

fn login_proxy(proxy: &ProxySettings) -> Result<Option<(&'static str, ProxyEndpoint)>> {
    let Some(url) = proxy.resolved_url()? else {
        return Ok(None);
    };
    if !proxy.username.is_empty() || !proxy.password.is_empty() {
        return Err(AppError::Credential(
            "the system WebView does not support Metrolist's authenticated proxy settings; temporarily use an unauthenticated proxy or the system proxy to sign in".into(),
        ));
    }
    let host = url
        .host_str()
        .ok_or_else(|| AppError::InvalidConfig("the login proxy address has no host".into()))?;
    let port = url
        .port_or_known_default()
        .ok_or_else(|| AppError::InvalidConfig("the login proxy address has no port".into()))?;
    let kind = match proxy.kind {
        ProxyKind::Http => "http",
        ProxyKind::Socks5 => "socks5",
    };
    Ok(Some((
        kind,
        ProxyEndpoint {
            host: host.to_owned(),
            port: port.to_string(),
        },
    )))
}

fn helper_proxy_from_environment() -> Result<Option<ProxyConfig>> {
    let Some(kind) = std::env::var_os(PROXY_KIND_ENV) else {
        return Ok(None);
    };
    let host = std::env::var(PROXY_HOST_ENV)
        .map_err(|_| AppError::InvalidConfig("the login proxy host is missing".into()))?;
    let port = std::env::var(PROXY_PORT_ENV)
        .map_err(|_| AppError::InvalidConfig("the login proxy port is missing".into()))?;
    if host.is_empty()
        || port.parse::<u16>().is_err()
        || host.chars().any(char::is_control)
        || port.chars().any(char::is_control)
    {
        return Err(AppError::InvalidConfig(
            "the login proxy endpoint is invalid".into(),
        ));
    }
    let endpoint = ProxyEndpoint { host, port };
    match kind.to_str() {
        Some("http") => Ok(Some(ProxyConfig::Http(endpoint))),
        Some("socks5") => Ok(Some(ProxyConfig::Socks5(endpoint))),
        _ => Err(AppError::InvalidConfig(
            "the login proxy kind is invalid".into(),
        )),
    }
}

fn write_helper_output(output: &LoginHelperOutput) {
    let mut stdout = std::io::BufWriter::new(std::io::stdout().lock());
    if serde_json::to_writer(&mut stdout, output).is_ok() {
        let _ = stdout.write_all(b"\n");
        let _ = stdout.flush();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn login_navigation_is_limited_to_google_and_youtube_https_pages() {
        assert!(login_navigation_allowed(LOGIN_URL));
        assert!(login_navigation_allowed("https://music.youtube.com/"));
        assert!(login_navigation_allowed(
            "https://accounts.google.com/v3/signin"
        ));
        assert!(login_navigation_allowed("about:blank"));
        assert!(!login_navigation_allowed("http://accounts.google.com/"));
        assert!(!login_navigation_allowed(
            "https://google.com.attacker.test/"
        ));
        assert!(!login_navigation_allowed("javascript:alert(1)"));
    }

    #[test]
    fn music_completion_requires_an_exact_https_origin_and_sapisid_cookie() {
        assert!(music_url("https://music.youtube.com/"));
        assert!(!music_url("https://music.youtube.com.attacker.test/"));
        assert!(!music_url("http://music.youtube.com/"));
        assert!(cookie_has_sapisid("SID=one; SAPISID=secret; OTHER=two"));
        assert!(!cookie_has_sapisid("SID=one; SAPISID=; OTHER=two"));
        assert!(!cookie_has_sapisid("NOTSAPISID=secret"));
    }

    #[test]
    fn helper_session_payload_is_validated_before_becoming_auth() {
        let payload = LoginHelperOutput::Session {
            cookie: "SID=fixture; SAPISID=secret".into(),
            visitor_data: Some("visitor".into()),
            data_sync_id: Some("sync".into()),
        };
        let encoded = serde_json::to_vec(&payload).unwrap();
        let decoded: LoginHelperOutput = serde_json::from_slice(&encoded).unwrap();
        let LoginHelperOutput::Session {
            cookie,
            visitor_data,
            data_sync_id,
        } = decoded
        else {
            panic!("expected session payload");
        };
        let session = AuthSession::from_parts(cookie, visitor_data, data_sync_id).unwrap();
        assert_eq!(session.visitor_data(), Some("visitor"));
        assert_eq!(session.data_sync_id(), Some("sync"));
    }

    #[test]
    fn helper_cancellation_round_trips_without_creating_a_session() {
        let encoded = serde_json::to_vec(&LoginHelperOutput::Cancelled).unwrap();
        let decoded: LoginHelperOutput = serde_json::from_slice(&encoded).unwrap();

        assert!(matches!(decoded, LoginHelperOutput::Cancelled));
    }

    #[test]
    fn login_proxy_uses_only_non_secret_endpoint_data() {
        let proxy = ProxySettings {
            enabled: true,
            kind: ProxyKind::Socks5,
            address: "127.0.0.1:1080".into(),
            username: String::new(),
            password: String::new(),
        };
        let (kind, endpoint) = login_proxy(&proxy).unwrap().unwrap();
        assert_eq!(kind, "socks5");
        assert_eq!(endpoint.host, "127.0.0.1");
        assert_eq!(endpoint.port, "1080");

        let authenticated = ProxySettings {
            username: "fixture".into(),
            password: "not-in-error".into(),
            ..proxy
        };
        let error = login_proxy(&authenticated).unwrap_err().to_string();
        assert!(!error.contains("fixture"));
        assert!(!error.contains("not-in-error"));
    }
}
