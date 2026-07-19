use std::process::ExitCode;

use thiserror::Error;

/// Process exit codes (agent contract).
pub const EXIT_OK: u8 = 0;
pub const EXIT_USAGE: u8 = 1;
pub const EXIT_AUTH: u8 = 2;
pub const EXIT_PARTIAL: u8 = 3;
pub const EXIT_UNEXPECTED: u8 = 10;

#[derive(Debug, Error)]
pub enum ImagoError {
    #[error("{0}")]
    Usage(String),

    #[error("authentication failed: {0}")]
    Auth(String),

    #[error("session expired or invalid — run: imago auth login")]
    SessionDead,

    #[error("rate limited by Instagram: {0}")]
    RateLimited(String),

    #[error("profile not found: {0}")]
    NotFound(String),

    #[error("network error: {0}")]
    Network(String),

    #[error("parse error: {0}")]
    Parse(String),

    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    #[error("{0}")]
    Other(String),
}

impl ImagoError {
    pub fn exit_code(&self) -> u8 {
        match self {
            Self::Usage(_) => EXIT_USAGE,
            Self::Auth(_) | Self::SessionDead => EXIT_AUTH,
            Self::RateLimited(_)
            | Self::NotFound(_)
            | Self::Network(_)
            | Self::Parse(_)
            | Self::Io(_)
            | Self::Other(_) => EXIT_UNEXPECTED,
        }
    }

    pub fn to_exit_code(&self) -> ExitCode {
        ExitCode::from(self.exit_code())
    }
}

pub type Result<T> = std::result::Result<T, ImagoError>;

impl From<serde_json::Error> for ImagoError {
    fn from(e: serde_json::Error) -> Self {
        Self::Parse(e.to_string())
    }
}

impl From<anyhow::Error> for ImagoError {
    fn from(e: anyhow::Error) -> Self {
        Self::Other(e.to_string())
    }
}
