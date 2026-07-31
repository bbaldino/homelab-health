use serde::Deserialize;
use std::collections::BTreeMap;
use std::time::Duration;

/// A single scalar metric series, decoupled from the parser crate so `evaluate`
/// and its tests never touch `prometheus_parse` types.
#[derive(Clone, Debug)]
pub struct Series {
    pub metric: String,
    pub labels: BTreeMap<String, String>,
    pub value: f64,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
pub enum Op {
    #[serde(rename = ">")]
    Gt,
    #[serde(rename = ">=")]
    Ge,
    #[serde(rename = "<")]
    Lt,
    #[serde(rename = "<=")]
    Le,
    #[serde(rename = "==")]
    Eq,
    #[serde(rename = "!=")]
    Ne,
}

impl Op {
    pub fn test(self, value: f64, threshold: f64) -> bool {
        match self {
            Op::Gt => value > threshold,
            Op::Ge => value >= threshold,
            Op::Lt => value < threshold,
            Op::Le => value <= threshold,
            Op::Eq => value == threshold,
            Op::Ne => value != threshold,
        }
    }
    pub fn as_str(self) -> &'static str {
        match self {
            Op::Gt => ">",
            Op::Ge => ">=",
            Op::Lt => "<",
            Op::Le => "<=",
            Op::Eq => "==",
            Op::Ne => "!=",
        }
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Rule {
    pub metric: String,
    #[serde(default)]
    pub labels: String,
    pub op: Op,
    pub threshold: f64,
    #[serde(default)]
    pub critical: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PrometheusConfig {
    pub url: String,
    #[serde(default = "default_timeout")]
    pub timeout_secs: u64,
    #[serde(default)]
    pub rules: Vec<Rule>,
}

fn default_timeout() -> u64 {
    10
}

/// Parse Prometheus equality matchers: `k="v",k2="v2"`. Empty string → no
/// matchers. Not full PromQL — no commas inside values, equality only.
pub fn parse_matchers(s: &str) -> Result<Vec<(String, String)>, String> {
    let s = s.trim();
    if s.is_empty() {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    for part in s.split(',') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        let (k, v) = part
            .split_once('=')
            .ok_or_else(|| format!("bad matcher '{part}' (expected key=\"value\")"))?;
        let key = k.trim().to_string();
        if key.is_empty() {
            return Err(format!("bad matcher '{part}' (empty key)"));
        }
        let val = v.trim().trim_matches('"').to_string();
        out.push((key, val));
    }
    Ok(out)
}

pub enum ScrapeError {
    Timeout,
    Unreachable(String),
    BadStatus(u16),
    Unparseable(String),
}

fn sample_value(v: &prometheus_parse::Value) -> Option<f64> {
    use prometheus_parse::Value::*;
    match v {
        Counter(x) | Gauge(x) | Untyped(x) => Some(*x),
        _ => None, // histograms/summaries have no single scalar; skip.
    }
}

/// Fetch the endpoint and parse it into scalar series. Non-2xx → BadStatus,
/// reqwest timeout → Timeout, other transport errors → Unreachable, and a body
/// that isn't valid exposition format → Unparseable.
pub async fn fetch_and_parse(url: &str, timeout_secs: u64) -> Result<Vec<Series>, ScrapeError> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(timeout_secs))
        .build()
        .map_err(|e| ScrapeError::Unreachable(e.to_string()))?;

    let resp = client.get(url).send().await.map_err(|e| {
        if e.is_timeout() {
            ScrapeError::Timeout
        } else {
            ScrapeError::Unreachable(e.to_string())
        }
    })?;

    let status = resp.status();
    if !status.is_success() {
        return Err(ScrapeError::BadStatus(status.as_u16()));
    }

    let body = resp
        .text()
        .await
        .map_err(|e| ScrapeError::Unreachable(e.to_string()))?;

    let lines = body.lines().map(|l| Ok::<_, std::io::Error>(l.to_owned()));
    let scrape = prometheus_parse::Scrape::parse(lines)
        .map_err(|e| ScrapeError::Unparseable(e.to_string()))?;

    let series = scrape
        .samples
        .iter()
        .filter_map(|s| {
            let value = sample_value(&s.value)?;
            let labels = s
                .labels
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect();
            Some(Series {
                metric: s.metric.clone(),
                labels,
                value,
            })
        })
        .collect();
    Ok(series)
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::method;
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[test]
    fn op_tests_compare_correctly() {
        assert!(Op::Ne.test(1.0, 0.0));
        assert!(!Op::Ne.test(0.0, 0.0));
        assert!(Op::Gt.test(2.0, 1.0));
        assert!(Op::Ge.test(1.0, 1.0));
        assert!(Op::Le.test(1.0, 1.0));
        assert!(Op::Eq.test(0.0, 0.0));
    }

    #[test]
    fn parse_matchers_handles_pairs_quotes_and_empty() {
        assert_eq!(parse_matchers("").unwrap(), Vec::<(String, String)>::new());
        assert_eq!(
            parse_matchers("task_type=\"backup\"").unwrap(),
            vec![("task_type".into(), "backup".into())]
        );
        assert_eq!(
            parse_matchers("a=\"1\", b=\"2\"").unwrap(),
            vec![("a".into(), "1".into()), ("b".into(), "2".into())]
        );
        assert!(parse_matchers("garbage no equals").is_err());
    }

    #[test]
    fn config_rejects_unknown_fields_and_bad_op() {
        let ok: Result<PrometheusConfig, _> = serde_json::from_value(serde_json::json!({
            "url": "http://x/metrics",
            "rules": [{ "metric": "m", "op": "!=", "threshold": 0 }]
        }));
        let cfg = ok.unwrap();
        assert_eq!(cfg.timeout_secs, 10);
        assert_eq!(cfg.rules[0].labels, "");
        assert!(!cfg.rules[0].critical);

        let bad_op: Result<PrometheusConfig, _> = serde_json::from_value(serde_json::json!({
            "url": "http://x", "rules": [{ "metric": "m", "op": "≈", "threshold": 0 }]
        }));
        assert!(bad_op.is_err());

        let unknown: Result<PrometheusConfig, _> = serde_json::from_value(serde_json::json!({
            "url": "http://x", "rules": [], "bogus": 1
        }));
        assert!(unknown.is_err());
    }

    const SAMPLE: &str = "# TYPE up gauge\nup{job=\"a\"} 1\nup{job=\"b\"} 0\n";

    #[tokio::test]
    async fn fetch_and_parse_returns_scalar_series() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_string(SAMPLE))
            .mount(&server)
            .await;
        let series = fetch_and_parse(&server.uri(), 5).await.ok().unwrap();
        assert_eq!(series.len(), 2);
        let a = series
            .iter()
            .find(|s| s.labels.get("job").map(String::as_str) == Some("a"))
            .unwrap();
        assert_eq!(a.metric, "up");
        assert_eq!(a.value, 1.0);
    }

    #[tokio::test]
    async fn fetch_and_parse_flags_non_2xx() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(503))
            .mount(&server)
            .await;
        assert!(matches!(
            fetch_and_parse(&server.uri(), 5).await,
            Err(ScrapeError::BadStatus(503))
        ));
    }
}
