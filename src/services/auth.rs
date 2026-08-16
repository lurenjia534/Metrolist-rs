use std::fmt;
use std::time::{SystemTime, UNIX_EPOCH};

use keyring::v1::{Entry, Error as KeyringError};
use serde::{Deserialize, Serialize};
use sha1::{Digest as _, Sha1};
use zeroize::{Zeroize as _, Zeroizing};

use crate::{AppError, Result};

const KEYRING_SERVICE: &str = "io.metrolist.desktop";
const KEYRING_ACCOUNT: &str = "youtube-music-session";
const MAX_COOKIE_BYTES: usize = 64 * 1024;
const ORIGIN: &str = "https://music.youtube.com";

const COOKIE_PREFIX: &str = "***INNERTUBE COOKIE*** =";
const VISITOR_PREFIX: &str = "***VISITOR DATA*** =";
const DATA_SYNC_PREFIX: &str = "***DATA SYNC ID*** =";
const ANDROID_DATA_SYNC_PREFIX: &str = "***DATASYNC ID*** =";

#[derive(Clone, PartialEq, Eq)]
pub struct AuthSession {
    cookie: String,
    visitor_data: Option<String>,
    data_sync_id: Option<String>,
}

impl AuthSession {
    pub fn from_import(value: &str) -> Result<Self> {
        let trimmed = value.trim();
        if trimmed.is_empty() {
            return Err(AppError::Credential(
                "the YouTube Music Cookie header is empty".into(),
            ));
        }

        let template_cookie = template_value(trimmed, COOKIE_PREFIX);
        let cookie = match template_cookie {
            Some(cookie) => cookie,
            None if trimmed.contains(COOKIE_PREFIX) => {
                return Err(AppError::Credential(
                    "the session template contains no Cookie value".into(),
                ));
            }
            None => trimmed
                .strip_prefix("Cookie:")
                .or_else(|| trimmed.strip_prefix("cookie:"))
                .unwrap_or(trimmed)
                .trim(),
        };
        Self::from_parts(
            cookie.to_owned(),
            template_value(trimmed, VISITOR_PREFIX).map(str::to_owned),
            template_value(trimmed, DATA_SYNC_PREFIX)
                .or_else(|| template_value(trimmed, ANDROID_DATA_SYNC_PREFIX))
                .map(str::to_owned),
        )
    }

    pub(crate) fn from_parts(
        cookie: String,
        visitor_data: Option<String>,
        data_sync_id: Option<String>,
    ) -> Result<Self> {
        let mut cookie = Zeroizing::new(cookie);
        let visitor_data = Zeroizing::new(visitor_data);
        let data_sync_id = Zeroizing::new(data_sync_id);
        if cookie.is_empty() || cookie.len() > MAX_COOKIE_BYTES {
            return Err(AppError::Credential(
                "the YouTube Music Cookie header has an invalid length".into(),
            ));
        }
        if cookie.chars().any(char::is_control) {
            return Err(AppError::Credential(
                "the YouTube Music Cookie header contains control characters".into(),
            ));
        }
        let sapisid = cookie_value(&cookie, "SAPISID").filter(|value| !value.is_empty());
        if sapisid.is_none() {
            return Err(AppError::Credential(
                "the YouTube Music Cookie header does not contain SAPISID".into(),
            ));
        }
        for value in [visitor_data.as_ref(), data_sync_id.as_ref()]
            .into_iter()
            .flatten()
        {
            if value.chars().any(char::is_control) {
                return Err(AppError::Credential(
                    "the imported session metadata contains control characters".into(),
                ));
            }
        }
        Ok(Self {
            cookie: std::mem::take(&mut *cookie),
            visitor_data: nonempty(visitor_data.as_deref()),
            data_sync_id: nonempty(data_sync_id.as_deref()),
        })
    }

    pub fn cookie_header(&self) -> &str {
        &self.cookie
    }

    pub fn visitor_data(&self) -> Option<&str> {
        self.visitor_data.as_deref()
    }

    pub fn data_sync_id(&self) -> Option<&str> {
        self.data_sync_id.as_deref()
    }

    pub fn authorization_now(&self) -> Result<String> {
        self.authorization_at(unix_time_seconds())
    }

    pub fn authorization_at(&self, timestamp: u64) -> Result<String> {
        let sapisid = cookie_value(&self.cookie, "SAPISID").ok_or_else(|| {
            AppError::Credential("the stored YouTube Music session is missing SAPISID".into())
        })?;
        let hash_input = Zeroizing::new(format!("{timestamp} {sapisid} {ORIGIN}"));
        let digest = Sha1::digest(hash_input.as_bytes());
        Ok(format!("SAPISIDHASH {timestamp}_{digest:x}"))
    }

    fn encode_for_storage(&self) -> Result<Zeroizing<String>> {
        #[derive(Serialize)]
        struct StoredSession<'a> {
            cookie: &'a str,
            #[serde(skip_serializing_if = "Option::is_none")]
            visitor_data: Option<&'a str>,
            #[serde(skip_serializing_if = "Option::is_none")]
            data_sync_id: Option<&'a str>,
        }

        serde_json::to_string(&StoredSession {
            cookie: &self.cookie,
            visitor_data: self.visitor_data(),
            data_sync_id: self.data_sync_id(),
        })
        .map(Zeroizing::new)
        .map_err(|_| AppError::Credential("the session could not be encoded securely".into()))
    }

    fn decode_from_storage(secret: &str) -> Result<Self> {
        #[derive(Deserialize)]
        struct StoredSession {
            cookie: String,
            #[serde(default)]
            visitor_data: Option<String>,
            #[serde(default)]
            data_sync_id: Option<String>,
        }

        let stored: StoredSession = serde_json::from_str(secret).map_err(|_| {
            AppError::Credential("the stored YouTube Music session is malformed".into())
        })?;
        Self::from_parts(stored.cookie, stored.visitor_data, stored.data_sync_id)
    }
}

impl fmt::Debug for AuthSession {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AuthSession")
            .field("cookie", &"[REDACTED]")
            .field("has_visitor_data", &self.visitor_data.is_some())
            .field("has_data_sync_id", &self.data_sync_id.is_some())
            .finish()
    }
}

impl Drop for AuthSession {
    fn drop(&mut self) {
        self.cookie.zeroize();
        self.visitor_data.zeroize();
        self.data_sync_id.zeroize();
    }
}

pub trait CredentialStore: Send + Sync {
    fn load(&self) -> Result<Option<AuthSession>>;
    fn save(&self, session: &AuthSession) -> Result<()>;
    fn delete(&self) -> Result<()>;
    fn backend_label(&self) -> &'static str;
}

#[derive(Debug)]
pub struct SystemCredentialStore {
    service: String,
    account: String,
}

impl Default for SystemCredentialStore {
    fn default() -> Self {
        Self {
            service: KEYRING_SERVICE.into(),
            account: KEYRING_ACCOUNT.into(),
        }
    }
}

impl SystemCredentialStore {
    fn entry(&self) -> Result<Entry> {
        Entry::new(&self.service, &self.account)
            .map_err(|error| credential_store_error("open", error))
    }

    #[cfg(test)]
    fn isolated(service: String, account: String) -> Self {
        debug_assert_ne!(service, KEYRING_SERVICE);
        debug_assert_ne!(account, KEYRING_ACCOUNT);
        Self { service, account }
    }
}

impl CredentialStore for SystemCredentialStore {
    fn load(&self) -> Result<Option<AuthSession>> {
        let entry = self.entry()?;
        let secret = match entry.get_password() {
            Ok(secret) => Zeroizing::new(secret),
            Err(KeyringError::NoEntry) => return Ok(None),
            Err(error) => return Err(credential_store_error("read", error)),
        };
        AuthSession::decode_from_storage(&secret).map(Some)
    }

    fn save(&self, session: &AuthSession) -> Result<()> {
        let secret = session.encode_for_storage()?;
        self.entry()?
            .set_password(&secret)
            .map_err(|error| credential_store_error("save", error))
    }

    fn delete(&self) -> Result<()> {
        match self.entry()?.delete_credential() {
            Ok(()) | Err(KeyringError::NoEntry) => Ok(()),
            Err(error) => Err(credential_store_error("delete", error)),
        }
    }

    fn backend_label(&self) -> &'static str {
        if cfg!(target_os = "macos") {
            "macOS Keychain"
        } else if cfg!(target_os = "windows") {
            "Windows Credential Manager"
        } else {
            "Secret Service"
        }
    }
}

fn template_value<'a>(input: &'a str, prefix: &str) -> Option<&'a str> {
    input.lines().find_map(|line| {
        line.trim_start()
            .strip_prefix(prefix)
            .map(str::trim)
            .filter(|value| !value.is_empty())
    })
}

fn cookie_value<'a>(cookie: &'a str, name: &str) -> Option<&'a str> {
    cookie.split(';').find_map(|part| {
        let (candidate, value) = part.trim().split_once('=')?;
        (candidate.trim() == name).then_some(value.trim())
    })
}

fn nonempty(value: Option<&str>) -> Option<String> {
    value.and_then(|value| {
        let trimmed = value.trim();
        (!trimmed.is_empty()).then(|| trimmed.to_owned())
    })
}

fn credential_store_error(operation: &str, error: KeyringError) -> AppError {
    AppError::CredentialStore(format!(
        "could not {operation} the YouTube Music session: {error}"
    ))
}

fn unix_time_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs())
}

#[cfg(test)]
mod tests {
    use super::*;

    const COOKIE: &str = "SID=fixture; SAPISID=sapisid-secret; __Secure-1PAPISID=other";

    #[test]
    fn direct_and_android_template_imports_require_sapisid() {
        let direct = AuthSession::from_import(COOKIE).unwrap();
        assert_eq!(direct.visitor_data(), None);
        assert_eq!(direct.data_sync_id(), None);

        let template = AuthSession::from_import(&format!(
            "{COOKIE_PREFIX} {COOKIE}\n{VISITOR_PREFIX} visitor-fixture\n{ANDROID_DATA_SYNC_PREFIX} sync-fixture"
        ))
        .unwrap();
        assert_eq!(template.visitor_data(), Some("visitor-fixture"));
        assert_eq!(template.data_sync_id(), Some("sync-fixture"));

        let legacy_template = AuthSession::from_import(&format!(
            "{COOKIE_PREFIX} {COOKIE}\n{DATA_SYNC_PREFIX} legacy-sync"
        ))
        .unwrap();
        assert_eq!(legacy_template.data_sync_id(), Some("legacy-sync"));

        assert!(AuthSession::from_import("SID=only").is_err());
        assert!(AuthSession::from_import("SAPISID=bad\r\nInjected: yes").is_err());
    }

    #[test]
    fn sapisid_hash_matches_the_android_request_shape() {
        let session = AuthSession::from_import(COOKIE).unwrap();
        assert_eq!(
            session.authorization_at(1_700_000_000).unwrap(),
            "SAPISIDHASH 1700000000_6460356a79ebb8ed0a6d322fc9e83bab9debc4fe"
        );
    }

    #[test]
    fn storage_round_trip_and_debug_output_never_expose_cookie_values() {
        let session = AuthSession::from_import(&format!(
            "{COOKIE_PREFIX} {COOKIE}\n{VISITOR_PREFIX} visitor-fixture"
        ))
        .unwrap();
        let encoded = session.encode_for_storage().unwrap();
        let restored = AuthSession::decode_from_storage(&encoded).unwrap();

        assert_eq!(restored, session);
        let debug = format!("{session:?}");
        assert!(!debug.contains("sapisid-secret"));
        assert!(!debug.contains("SID=fixture"));
        assert!(debug.contains("REDACTED"));
    }

    #[test]
    fn platform_store_failures_are_not_reported_as_invalid_cookie_data() {
        let error = credential_store_error("read", KeyringError::NoEntry);

        assert!(matches!(error, AppError::CredentialStore(_)));
    }

    #[test]
    #[ignore = "touches the platform credential store using an isolated test entry"]
    fn real_system_credential_store_round_trip_uses_isolated_entry() {
        struct Cleanup<'a>(&'a SystemCredentialStore);

        impl Drop for Cleanup<'_> {
            fn drop(&mut self) {
                let _ = self.0.delete();
            }
        }

        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock should be after the Unix epoch")
            .as_nanos();
        let store = SystemCredentialStore::isolated(
            format!("io.metrolist.desktop.test.{}.{}", std::process::id(), nonce),
            format!("youtube-music-session-test-{nonce}"),
        );
        let _cleanup = Cleanup(&store);
        let session = AuthSession::from_import(COOKIE).unwrap();

        store.delete().unwrap();
        assert_eq!(store.load().unwrap(), None);
        store.save(&session).unwrap();
        assert_eq!(store.load().unwrap(), Some(session));
        store.delete().unwrap();
        assert_eq!(store.load().unwrap(), None);
    }
}
