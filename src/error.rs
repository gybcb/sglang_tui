use std::fmt;

/// Top-level error type for sgtop.
#[derive(Debug)]
pub enum SgtopError {
    /// The server is reachable but `/metrics` returned 404 — it was started
    /// without `--enable-metrics`. This is fatal per the product decision: we
    /// render an explicit message and exit rather than silently degrade.
    MetricsDisabled,
    Poll(PollError),
    Io(std::io::Error),
    Http(reqwest::Error),
    Other(String),
}

/// Classifies why a single poll attempt failed, so the poller can decide
/// between Degraded (keep last-known, grey out) and Lost (banner + reconnect).
#[derive(Debug, Clone)]
pub enum PollError {
    /// Transport failure: connection refused, DNS, TLS, timeout.
    Transport(String),
    /// HTTP 4xx/5xx. `.status` is the raw code.
    Status { status: u16, path: String },
    /// Body arrived but did not decode into the expected shape.
    Decode(String),
    /// The endpoint is known to be unavailable on this server (e.g. a metric
    /// family that is not present because the relevant flag is off).
    Disabled(&'static str),
}

impl fmt::Display for SgtopError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SgtopError::MetricsDisabled => {
                write!(
                    f,
                    "server started without --enable-metrics (GET /metrics -> 404)"
                )
            }
            SgtopError::Poll(e) => write!(f, "poll failed: {e}"),
            SgtopError::Io(e) => write!(f, "io: {e}"),
            SgtopError::Http(e) => write!(f, "http: {e}"),
            SgtopError::Other(s) => write!(f, "{s}"),
        }
    }
}

impl fmt::Display for PollError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PollError::Transport(s) => write!(f, "transport: {s}"),
            PollError::Status { status, path } => write!(f, "http {status} on {path}"),
            PollError::Decode(s) => write!(f, "decode: {s}"),
            PollError::Disabled(s) => write!(f, "disabled: {s}"),
        }
    }
}

impl std::error::Error for SgtopError {}

impl From<std::io::Error> for SgtopError {
    fn from(e: std::io::Error) -> Self {
        SgtopError::Io(e)
    }
}

impl From<reqwest::Error> for SgtopError {
    fn from(e: reqwest::Error) -> Self {
        SgtopError::Http(e)
    }
}
