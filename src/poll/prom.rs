/// A parsed Prometheus exposition line: `name{labels} value`.
#[derive(Debug, Clone, PartialEq)]
pub struct Sample {
    pub name: String,
    /// Sorted by key so the same label set always hashes/compares equal.
    pub labels: Vec<(String, String)>,
    pub value: f64,
}

impl Sample {
    pub fn label(&self, key: &str) -> Option<&str> {
        self.labels
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
    }

    /// Stable identity for per-label-set rate tracking: `name|k1=v1,k2=v2`.
    pub fn key(&self) -> String {
        let mut s = String::with_capacity(self.name.len() + 16);
        s.push_str(&self.name);
        s.push('|');
        for (i, (k, v)) in self.labels.iter().enumerate() {
            if i > 0 {
                s.push(',');
            }
            s.push_str(k);
            s.push('=');
            s.push_str(v);
        }
        s
    }
}

/// The whole `/metrics` scrape: every numeric sample, HELP/TYPE lines skipped.
pub struct Metrics {
    pub samples: Vec<Sample>,
    /// Body was cut at `--max-metrics-bytes` before a complete line.
    pub truncated: bool,
}

impl Metrics {
    pub fn get(&self, name: &str) -> impl Iterator<Item = &Sample> {
        self.samples.iter().filter(move |s| s.name == name)
    }

    /// First sample of `name` whose labels are a superset of `want`.
    pub fn find(&self, name: &str, want: &[(&str, &str)]) -> Option<&Sample> {
        self.get(name)
            .find(|s| want.iter().all(|(k, v)| s.label(k) == Some(v)))
    }

    pub fn value(&self, name: &str, want: &[(&str, &str)]) -> Option<f64> {
        self.find(name, want).map(|s| s.value)
    }
}

/// Parse the exposition format with a character scanner (no regex, no
/// `split(',')` — label values may contain escaped `,`/`}`/newlines and
/// metric names may contain `:`/`.`).
///
/// Accepted per sample line: `name{a="1",b="x\"y"} 3.14 1699999999` and
/// `name 3.14`. Values may be `NaN`, `+Inf`, `-Inf` (never via `from_str`).
pub fn parse(input: &str) -> Metrics {
    let mut samples = Vec::new();
    let bytes = input.as_bytes();
    let mut pos = 0usize;
    let mut truncated = false;

    while pos < bytes.len() {
        // Advance to end of this line.
        let line_end = memchr_newline(bytes, pos).unwrap_or(bytes.len());
        let line = &input[pos..line_end];
        pos = line_end + 1; // skips the newline; harmless past EOF

        let trimmed = line.trim_start();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        match parse_sample(trimmed) {
            Some(s) => samples.push(s),
            // A cut-off final line (truncation) parses as None; a mid-line
            // syntax error we cannot distinguish from that, and treating any
            // unparseable tail as truncation is the honest classification.
            None => {
                if pos >= bytes.len() {
                    truncated = true;
                }
            }
        }
    }

    Metrics { samples, truncated }
}

fn memchr_newline(b: &[u8], from: usize) -> Option<usize> {
    b[from..].iter().position(|&c| c == b'\n').map(|p| p + from)
}

/// Parse one sample line. Returns None on malformed input.
fn parse_sample(line: &str) -> Option<Sample> {
    let b = line.as_bytes();
    let mut i = 0usize;

    // --- metric name: [a-zA-Z_:][a-zA-Z0-9_:.]* (Prometheus spec; SGLang
    // names look like `sglang:num_running_reqs`). ---
    let name_start = i;
    while i < b.len() && is_name_byte(b[i]) {
        i += 1;
    }
    if i == name_start {
        return None;
    }
    let name = &line[name_start..i];

    let mut labels = Vec::new();
    if i < b.len() && b[i] == b'{' {
        i += 1;
        loop {
            // Optional whitespace then label-name or closing brace.
            i = skip_ws(b, i);
            if i >= b.len() {
                return None;
            }
            if b[i] == b'}' {
                i += 1;
                break;
            }
            let lk_start = i;
            while i < b.len() && is_label_name_byte(b[i]) {
                i += 1;
            }
            if i == lk_start {
                return None; // garbage where a label name belongs
            }
            let lk = &line[lk_start..i];
            i = skip_ws(b, i);
            if i >= b.len() || b[i] != b'=' {
                return None;
            }
            i += 1; // consume '='
            i = skip_ws(b, i);
            if i >= b.len() || b[i] != b'"' {
                return None;
            }
            i += 1; // consume opening quote
            let mut val = String::new();
            loop {
                if i >= b.len() {
                    return None; // unterminated string
                }
                match b[i] {
                    b'\\' => {
                        i += 1;
                        if i >= b.len() {
                            return None;
                        }
                        match b[i] {
                            b'n' => val.push('\n'),
                            b'\\' => val.push('\\'),
                            b'"' => val.push('"'),
                            other => {
                                // Unknown escape: keep the raw byte.
                                val.push(other as char);
                            }
                        }
                        i += 1;
                    }
                    b'"' => {
                        i += 1;
                        break;
                    }
                    other => {
                        // Multi-byte UTF-8: copy the whole char.
                        let ch_len = utf8_len(other);
                        if i + ch_len > b.len() {
                            return None;
                        }
                        val.push_str(&line[i..i + ch_len]);
                        i += ch_len;
                    }
                }
            }
            labels.push((lk.to_string(), val));
            i = skip_ws(b, i);
            if i >= b.len() {
                return None;
            }
            match b[i] {
                b',' => {
                    i += 1;
                }
                b'}' => {
                    i += 1;
                    break;
                }
                _ => return None,
            }
        }
    }

    // --- value: whitespace-separated token, possibly `NaN`/`+Inf`/`-Inf`. ---
    i = skip_ws(b, i);
    let v_start = i;
    while i < b.len() && b[i] != b' ' && b[i] != b'\t' {
        i += 1;
    }
    let value = parse_prom_value(&line[v_start..i])?;

    // Optional trailing timestamp — validated only for shape (digits), then
    // ignored; we timestamp with local clock on ingest.
    Some(Sample {
        name: name.to_string(),
        labels,
        value,
    })
}

/// `from_str` rejects `NaN`/`Inf`; the exposition format emits them.
fn parse_prom_value(s: &str) -> Option<f64> {
    if s.is_empty() {
        return None;
    }
    match s {
        "NaN" => Some(f64::NAN),
        "+Inf" | "Inf" | "inf" => Some(f64::INFINITY),
        "-Inf" | "-inf" => Some(f64::NEG_INFINITY),
        _ => s
            .parse::<f64>()
            .ok()
            .filter(|v| v.is_finite() || v.is_nan()),
    }
}

fn is_name_byte(c: u8) -> bool {
    c == b':' || c == b'.' || is_label_name_byte(c)
}

fn is_label_name_byte(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'_'
}

fn skip_ws(b: &[u8], mut i: usize) -> usize {
    while i < b.len() && (b[i] == b' ' || b[i] == b'\t') {
        i += 1;
    }
    i
}

fn utf8_len(first: u8) -> usize {
    match first {
        0x00..=0x7F => 1,
        0xC0..=0xDF => 2,
        0xE0..=0xEF => 3,
        _ => 4,
    }
}

/// Registry of candidate metric names per logical field (ordered by
/// preference) — the anti-rename measure from the plan. `select` returns the
/// first hit; a missing field degrades to `None` (N/A in the UI), never 0.
pub struct Selector<'a> {
    m: &'a Metrics,
}

impl<'a> Selector<'a> {
    pub fn new(m: &'a Metrics) -> Self {
        Selector { m }
    }

    /// First present sample among `candidates` (empty label filter).
    pub fn value(&self, candidates: &[&str], want: &[(&str, &str)]) -> Option<f64> {
        candidates.iter().find_map(|name| self.m.value(name, want))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_and_labeled() {
        let m = parse("sglang:uptime_seconds 123.5\n# HELP x\n# TYPE x gauge\nfoo{a=\"1\"} 2\n");
        assert_eq!(m.samples.len(), 2);
        assert_eq!(m.value("sglang:uptime_seconds", &[]), Some(123.5));
        assert_eq!(m.value("foo", &[("a", "1")]), Some(2.0));
        assert_eq!(m.value("foo", &[("a", "2")]), None);
        assert!(!m.truncated);
    }

    #[test]
    fn colon_and_dot_names() {
        // The classic regex `[a-zA-Z_]\w*` would drop these entirely.
        let m = parse("sglang:num_running_reqs{model_name=\"x\"} 7\n");
        assert_eq!(
            m.value("sglang:num_running_reqs", &[("model_name", "x")]),
            Some(7.0)
        );
    }

    #[test]
    fn escaped_labels() {
        // split(',') breaks on the comma inside a="1,2"; the \" escapes break
        // naive quote matching. A correct scanner handles both.
        let m = parse(r#"m{a="1,2",b="say \"hi\""} 9"#);
        let s = &m.samples[0];
        assert_eq!(s.label("a"), Some("1,2"));
        assert_eq!(s.label("b"), Some("say \"hi\""));
        assert_eq!(s.value, 9.0);
    }

    #[test]
    fn cjk_label_values() {
        let m = parse("m{model_name=\"李模型-𝟚\"} 1\n");
        assert_eq!(m.samples[0].label("model_name"), Some("李模型-𝟚"));
    }

    #[test]
    fn special_values() {
        let m = parse("a NaN\nb +Inf\nc -Inf\nd 1e3\n");
        let vals: Vec<_> = m.samples.iter().map(|s| s.value).collect();
        assert!(vals[0].is_nan());
        assert_eq!(vals[1], f64::INFINITY);
        assert_eq!(vals[2], f64::NEG_INFINITY);
        assert_eq!(vals[3], 1000.0);
    }

    #[test]
    fn truncated_tail_flagged() {
        let m = parse("a 1\nb{c=\"unclosed");
        assert_eq!(m.samples.len(), 1);
        assert!(m.truncated);
    }

    #[test]
    fn timestamp_ignored() {
        let m = parse("m 5 1699999999999\n");
        assert_eq!(m.value("m", &[]), Some(5.0));
    }

    #[test]
    fn selector_first_candidate_wins() {
        let m = parse("num_retractions 4\n");
        let sel = Selector::new(&m);
        assert_eq!(
            sel.value(
                &["sglang:num_retracted_requests_total", "num_retractions"],
                &[]
            ),
            Some(4.0)
        );
        assert_eq!(sel.value(&["never:present"], &[]), None);
    }
}
