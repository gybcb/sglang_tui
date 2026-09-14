use std::time::Duration;

use crate::config::Config;
use crate::error::PollError;

/// An HTTP GET that succeeded, with the wall-clock round trip.
pub struct Fetched {
    pub status: u16,
    pub body: String,
    pub rtt: Duration,
}

/// Thin wrapper over reqwest holding the shared client + auth header. Every
/// sgtop HTTP read goes through here so timeout and the bearer token are
/// applied uniformly.
pub struct Http {
    client: reqwest::Client,
    base: String,
    api_key: String,
}

impl Http {
    pub fn new(cfg: &Config) -> Http {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_millis(cfg.timeout_ms))
            .gzip(true)
            .build()
            // reqwest only fails to build on a TLS backend init error; fall
            // back to a default client rather than killing the whole program.
            .unwrap_or_default();
        let base = cfg.url.trim_end_matches('/').to_string();
        Http {
            client,
            base,
            api_key: cfg.api_key.clone(),
        }
    }

    fn with_auth(&self, req: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        if self.api_key.is_empty() {
            req
        } else {
            req.header(
                reqwest::header::AUTHORIZATION,
                format!("Bearer {}", self.api_key),
            )
        }
    }

    /// GET `path` returning the raw body. Transport errors map to
    /// `PollError::Transport`, non-success statuses to `Status`.
    pub async fn get(&self, path: &str) -> Result<Fetched, PollError> {
        let url = format!("{}{}", self.base, path);
        let req = self.with_auth(self.client.get(&url));
        let t0 = std::time::Instant::now();
        let resp = req.send().await.map_err(|e| {
            PollError::Transport(if e.is_timeout() {
                format!("timeout {path}")
            } else {
                format!("{e}")
            })
        })?;
        let status = resp.status().as_u16();
        let body = resp
            .text()
            .await
            .map_err(|e| PollError::Transport(format!("read {path}: {e}")))?;
        let rtt = t0.elapsed();
        if !(200..300).contains(&status) {
            return Err(PollError::Status {
                status,
                path: path.to_string(),
            });
        }
        Ok(Fetched { status, body, rtt })
    }

    /// GET returning `Some(body)` on 2xx and `None` on 404 (endpoint absent on
    /// this server build), other failures as `Err`. Used for the candidate
    /// fallback (`/model_info` ← `/get_model_info`).
    pub async fn get_optional(&self, path: &str) -> Result<Option<String>, PollError> {
        match self.get(path).await {
            Ok(f) => Ok(Some(f.body)),
            Err(PollError::Status { status: 404, .. }) => Ok(None),
            Err(e) => Err(e),
        }
    }
}
