use serde_json::Value;

use crate::error::PollError;
use crate::model::snapshot::ServerMeta;
use crate::poll::fetch::Http;

/// Fetch the static metadata endpoints once per server identity. These are
/// permanent, flag-free read-only routes — they never enter the numeric poll
/// and never drive Degraded/Lost (that's `/metrics`' job). `/server_info` is
/// auth-gated (`AuthLevel.NORMAL`), so a key-guarded server needs the bearer.
///
/// `max_total_num_tokens` is deliberately NOT read here — it's a runtime value
/// available as a constant gauge in `/metrics`; the numeric path owns it.
pub async fn fetch(http: &Http) -> Result<ServerMeta, PollError> {
    // Newer builds expose `/model_info` + `/server_info`; older ones only have
    // the `/get_`-prefixed aliases. Try canonical first, fall back on 404.
    let model = get_first(http, &["/model_info", "/get_model_info"]).await?;
    let server = get_first(http, &["/server_info", "/get_server_info"]).await?;

    let model: Value =
        serde_json::from_str(&model).map_err(|e| PollError::Decode(format!("model_info: {e}")))?;
    let server: Value = serde_json::from_str(&server)
        .map_err(|e| PollError::Decode(format!("server_info: {e}")))?;

    Ok(meta_from(&model, &server))
}

async fn get_first(http: &Http, paths: &[&str]) -> Result<String, PollError> {
    for (i, p) in paths.iter().enumerate() {
        match http.get_optional(p).await {
            Ok(Some(body)) => return Ok(body),
            Ok(None) => continue, // 404: try the next candidate
            Err(_e) if i + 1 < paths.len() => continue,
            Err(e) => return Err(e),
        }
    }
    Err(PollError::Disabled(
        "no model_info/server_info route (tried canonical + /get_ aliases)",
    ))
}

/// Project the two JSON objects onto ServerMeta. Missing keys stay at the
/// default (empty/false/0) — a partial server_info still yields a usable meta.
fn meta_from(model: &Value, server: &Value) -> ServerMeta {
    let s = |key: &str| server.get(key).and_then(Value::as_str).unwrap_or_default();
    let b = |key: &str| server.get(key).and_then(Value::as_bool).unwrap_or(false);
    let u = |key: &str| {
        server
            .get(key)
            .and_then(Value::as_u64)
            .map(|v| v as u32)
            .unwrap_or(0)
    };

    ServerMeta {
        model_path: model
            .get("model_path")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        served_model_name: model
            .get("served_model_name")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        version: s("version").to_string(),
        tp_size: u("tp_size"),
        pp_size: u("pp_size"),
        dp_size: u("dp_size"),
        enable_dp_attention: b("enable_dp_attention"),
        enable_metrics_for_all_schedulers: b("enable_metrics_for_all_schedulers"),
        enable_hierarchical_cache: b("enable_hierarchical_cache"),
        max_total_num_tokens: None, // numeric path owns this (constant gauge)
        loaded: true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn projects_flat_server_args() {
        // /server_info flattens resolved_dict() at the top level.
        let server = json!({
            "version": "0.4.9",
            "tp_size": 8,
            "pp_size": 1,
            "dp_size": 4,
            "enable_dp_attention": true,
            "enable_metrics_for_all_schedulers": false,
            "enable_hierarchical_cache": true
        });
        let model = json!({"model_path": "/models/m", "served_model_name": "李模型"});
        let m = meta_from(&model, &server);
        assert_eq!(m.tp_size, 8);
        assert_eq!(m.dp_size, 4);
        assert!(m.enable_dp_attention);
        assert!(!m.enable_metrics_for_all_schedulers);
        assert!(m.enable_hierarchical_cache);
        assert_eq!(m.served_model_name, "李模型");
        assert_eq!(m.version, "0.4.9");
    }

    #[test]
    fn missing_keys_default_safely() {
        let m = meta_from(&serde_json::json!({}), &serde_json::json!({}));
        assert_eq!(m.tp_size, 0);
        assert!(!m.enable_dp_attention);
        assert!(m.model_path.is_empty());
        assert!(m.loaded); // we did fetch, the server just told us nothing
    }
}
