//! Errors returned by the SDK. They carry no transport detail; a host maps
//! [`ErrorKind`] to its own status codes.

use std::fmt;

use serde_json::Value;

/// What kind of failure an [`Error`] is, for mapping to a transport status.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ErrorKind {
    /// The request was malformed or failed validation.
    InvalidRequest,
    /// Every source was excluded; `details` and `diagnostics` say why.
    NoViableRoute,
    /// Evaluation did not finish before its deadline.
    Deadline,
    /// An unexpected internal failure.
    Internal,
}

/// A structured failure with a stable machine-readable `code`.
#[derive(Debug)]
pub struct Error {
    pub kind: ErrorKind,
    pub code: &'static str,
    pub message: String,
    pub details: Vec<String>,
    /// Evidence gathered before the failure (exclusions, observations, quotes).
    pub diagnostics: Option<Value>,
}

impl Error {
    pub(crate) fn new(kind: ErrorKind, code: &'static str, message: impl Into<String>) -> Self {
        Self {
            kind,
            code,
            message: message.into(),
            details: Vec::new(),
            diagnostics: None,
        }
    }

    pub(crate) fn validation(message: impl Into<String>, details: Vec<String>) -> Self {
        Self {
            details,
            ..Self::new(ErrorKind::InvalidRequest, "VALIDATION_ERROR", message)
        }
    }

    pub(crate) fn no_viable(message: impl Into<String>, details: Vec<String>) -> Self {
        Self {
            details,
            ..Self::new(ErrorKind::NoViableRoute, "NO_VIABLE_ROUTE", message)
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for Error {}
