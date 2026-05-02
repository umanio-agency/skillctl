//! Typed errors with stable exit-code categories. Most code paths can keep
//! using `anyhow!()` for ad-hoc errors (those map to `ExitCode::Generic`);
//! when an error has a meaningful category for downstream agents, return
//! `AppError::*` directly so the `main` extractor can read it back.

use std::fmt;

#[derive(Debug)]
pub enum AppError {
    Config(String),
    Conflict(String),
    Git(String),
}

impl fmt::Display for AppError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Config(s) | Self::Conflict(s) | Self::Git(s) => f.write_str(s),
        }
    }
}

impl std::error::Error for AppError {}

impl AppError {
    pub fn code(&self) -> ExitCode {
        match self {
            Self::Config(_) => ExitCode::Config,
            Self::Conflict(_) => ExitCode::Conflict,
            Self::Git(_) => ExitCode::Git,
        }
    }
}

#[derive(Clone, Copy, Debug)]
#[repr(u8)]
pub enum ExitCode {
    Success = 0,
    Generic = 1,
    Config = 2,
    Conflict = 3,
    Git = 4,
}

impl From<ExitCode> for std::process::ExitCode {
    fn from(c: ExitCode) -> Self {
        std::process::ExitCode::from(c as u8)
    }
}

/// Walk the error chain looking for an AppError variant; fall back to Generic.
pub fn classify(err: &anyhow::Error) -> ExitCode {
    err.chain()
        .find_map(|e| e.downcast_ref::<AppError>().map(AppError::code))
        .unwrap_or(ExitCode::Generic)
}
