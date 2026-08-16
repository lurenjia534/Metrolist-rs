use std::sync::Arc;

use http_client::HttpClient;
use reqwest_client::ReqwestClient;

use crate::{AppError, ProxySettings, Result};

pub fn build_http_client(proxy: &ProxySettings, user_agent: &str) -> Result<Arc<dyn HttpClient>> {
    let proxy_url = proxy.resolved_url()?;
    let client = ReqwestClient::proxy_and_user_agent(proxy_url.clone(), user_agent)
        .map_err(|error| AppError::Network(format!("could not configure HTTP client: {error}")))?;
    if proxy_url.is_some() && client.proxy() != proxy_url.as_ref() {
        return Err(AppError::InvalidConfig(
            "the selected proxy could not be configured by the HTTP backend".into(),
        ));
    }
    Ok(Arc::new(client))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ProxyKind;
    use futures::AsyncReadExt as _;
    use http_client::{AsyncBody, Request, StatusCode};
    use std::{
        io::{Read as _, Write as _},
        net::TcpListener,
        thread,
        time::Duration,
    };

    #[test]
    fn configured_client_reports_the_exact_proxy_without_exposing_credentials() {
        let settings = ProxySettings {
            enabled: true,
            kind: ProxyKind::Http,
            address: "127.0.0.1:8080".into(),
            username: "fixture".into(),
            password: "secret".into(),
        };
        let expected = settings.resolved_url().unwrap().unwrap();
        let client = build_http_client(&settings, "Metrolist test").unwrap();

        assert_eq!(client.proxy(), Some(&expected));
    }

    #[test]
    fn disabled_proxy_builds_a_direct_client() {
        let client = build_http_client(&ProxySettings::default(), "Metrolist test").unwrap();
        assert_eq!(client.proxy(), None);
    }

    #[test]
    fn http_requests_are_actually_routed_through_the_configured_proxy() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let proxy = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut request = Vec::new();
            let mut chunk = [0_u8; 1024];
            while !request.windows(4).any(|window| window == b"\r\n\r\n") {
                let read = stream.read(&mut chunk).unwrap();
                if read == 0 {
                    break;
                }
                request.extend_from_slice(&chunk[..read]);
            }
            assert!(request.starts_with(b"GET http://example.test/proxy-proof HTTP/1.1\r\n"));
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok")
                .unwrap();
        });
        let settings = ProxySettings {
            enabled: true,
            address: address.to_string(),
            ..ProxySettings::default()
        };
        let client = build_http_client(&settings, "Metrolist proxy test").unwrap();
        let request = Request::builder()
            .uri("http://example.test/proxy-proof")
            .body(AsyncBody::default())
            .unwrap();
        let mut response = futures::executor::block_on(client.send(request)).unwrap();
        let mut body = Vec::new();
        futures::executor::block_on(response.body_mut().read_to_end(&mut body)).unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(body, b"ok");
        proxy.join().unwrap();
    }
}
