use thiserror::Error;

pub type Result<T> = std::result::Result<T, AppError>;

#[derive(Debug, Error)]
pub enum AppError {
    #[error("invalid configuration: {0}")]
    InvalidConfig(String),

    #[error("network request failed: {0}")]
    Network(String),

    #[error("remote response could not be understood: {0}")]
    Protocol(String),

    #[error("playback failed: {0}")]
    Playback(String),

    #[error("song recognition failed: {0}")]
    Recognition(String),

    #[error("storage operation failed: {0}")]
    Storage(String),

    #[error("credential operation failed: {0}")]
    Credential(String),

    #[error("YouTube Music session expired: {0}")]
    SessionExpired(String),

    #[error("Last.fm session expired: {0}")]
    LastFmSessionExpired(String),

    #[error("Discord integration failed: {0}")]
    Discord(String),

    #[error("Listen Together failed: {0}")]
    ListenTogether(String),

    #[error("system credential store operation failed: {0}")]
    CredentialStore(String),

    #[error("download failed: {0}")]
    Download(String),
}
