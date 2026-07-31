use super::{CheckType, ConfigSchema, Field, FieldKind};
use crate::report::{CheckReport, Component};
use crate::status::Status;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
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

#[derive(Serialize)]
pub struct MetricInfo {
    pub labels: BTreeMap<String, Vec<String>>,
}

#[derive(Serialize)]
pub struct InspectResult {
    pub metrics: BTreeMap<String, MetricInfo>,
}

/// Build a metric-name → {label key → sorted unique values} map for autocomplete.
pub fn inspect_result(series: &[Series]) -> InspectResult {
    let mut acc: BTreeMap<String, BTreeMap<String, BTreeSet<String>>> = BTreeMap::new();
    for s in series {
        let entry = acc.entry(s.metric.clone()).or_default();
        for (k, v) in &s.labels {
            entry.entry(k.clone()).or_default().insert(v.clone());
        }
    }
    let metrics = acc
        .into_iter()
        .map(|(metric, labels)| {
            let labels = labels
                .into_iter()
                .map(|(k, vs)| (k, vs.into_iter().collect::<Vec<_>>()))
                .collect();
            (metric, MetricInfo { labels })
        })
        .collect();
    InspectResult { metrics }
}

fn series_name(metric: &str, labels: &BTreeMap<String, String>) -> String {
    if labels.is_empty() {
        metric.to_string()
    } else {
        let inner: Vec<String> = labels.iter().map(|(k, v)| format!("{k}=\"{v}\"")).collect();
        format!("{metric}{{{}}}", inner.join(","))
    }
}

/// Pure: map rules against parsed series into a rolled-up CheckReport.
pub fn evaluate(series: &[Series], rules: &[Rule]) -> CheckReport {
    if rules.is_empty() {
        return CheckReport::new(Status::Unknown, "no rules configured");
    }

    let mut components = Vec::new();
    for rule in rules {
        let matchers = match parse_matchers(&rule.labels) {
            Ok(m) => m,
            Err(e) => {
                components.push(Component::new(
                    format!("{} {{{}}}", rule.metric, rule.labels),
                    Status::Unknown,
                    rule.critical,
                    format!("invalid label matcher: {e}"),
                ));
                continue;
            }
        };

        let matched: Vec<&Series> = series
            .iter()
            .filter(|s| s.metric == rule.metric)
            .filter(|s| {
                matchers
                    .iter()
                    .all(|(k, v)| s.labels.get(k).map(String::as_str) == Some(v.as_str()))
            })
            .collect();

        if matched.is_empty() {
            let sel = if rule.labels.is_empty() {
                rule.metric.clone()
            } else {
                format!("{}{{{}}}", rule.metric, rule.labels)
            };
            components.push(Component::new(
                sel,
                Status::Unknown,
                rule.critical,
                "no series matched",
            ));
            continue;
        }

        for s in matched {
            let breached = rule.op.test(s.value, rule.threshold);
            let status = if !breached {
                Status::Ok
            } else if rule.critical {
                Status::Critical
            } else {
                Status::Degraded
            };
            let msg = format!("{} ({} {})", s.value, rule.op.as_str(), rule.threshold);
            components.push(Component::new(
                series_name(&s.metric, &s.labels),
                status,
                rule.critical,
                msg,
            ));
        }
    }

    CheckReport::from_components(components)
}

pub struct PrometheusCheck;

impl PrometheusCheck {
    fn map_scrape_error(e: ScrapeError, url: &str) -> CheckReport {
        match e {
            ScrapeError::Timeout => {
                CheckReport::new(Status::Unknown, format!("timed out scraping {url}"))
            }
            ScrapeError::Unreachable(m) => {
                CheckReport::new(Status::Critical, format!("cannot scrape {url}: {m}"))
            }
            ScrapeError::BadStatus(code) => {
                CheckReport::new(Status::Critical, format!("{url} returned HTTP {code}"))
            }
            ScrapeError::Unparseable(m) => CheckReport::new(
                Status::Unknown,
                format!("response was not Prometheus metrics: {m}"),
            ),
        }
    }
}

#[async_trait]
impl CheckType for PrometheusCheck {
    fn type_id(&self) -> &'static str {
        "prometheus"
    }

    fn schema(&self) -> ConfigSchema {
        ConfigSchema {
            fields: vec![
                Field {
                    name: "url",
                    kind: FieldKind::String,
                    required: true,
                    default: None,
                    help: "URL of the service's Prometheus metrics endpoint",
                    secret: false,
                    options: None,
                    fields: None,
                },
                Field {
                    name: "timeout_secs",
                    kind: FieldKind::Int,
                    required: false,
                    default: Some(json!(10)),
                    help: "Request timeout in seconds",
                    secret: false,
                    options: None,
                    fields: None,
                },
                Field {
                    name: "rules",
                    kind: FieldKind::List,
                    required: false,
                    default: None,
                    help: "Health rules evaluated against the scraped metrics",
                    secret: false,
                    options: None,
                    fields: Some(vec![
                        Field {
                            name: "metric",
                            kind: FieldKind::String,
                            required: true,
                            default: None,
                            help: "Metric family name",
                            secret: false,
                            options: None,
                            fields: None,
                        },
                        Field {
                            name: "labels",
                            kind: FieldKind::String,
                            required: false,
                            default: Some(json!("")),
                            help: "Label matchers, e.g. task_type=\"backup\" (blank = all series)",
                            secret: false,
                            options: None,
                            fields: None,
                        },
                        Field {
                            name: "op",
                            kind: FieldKind::String,
                            required: true,
                            default: None,
                            help: "Comparison",
                            secret: false,
                            options: Some(vec![
                                json!(">"),
                                json!(">="),
                                json!("<"),
                                json!("<="),
                                json!("=="),
                                json!("!="),
                            ]),
                            fields: None,
                        },
                        Field {
                            name: "threshold",
                            kind: FieldKind::Float,
                            required: true,
                            default: None,
                            help: "Value compared against the series",
                            secret: false,
                            options: None,
                            fields: None,
                        },
                        Field {
                            name: "critical",
                            kind: FieldKind::Bool,
                            required: false,
                            default: Some(json!(false)),
                            help: "Breach reds the monitor (vs. degrades it)",
                            secret: false,
                            options: None,
                            fields: None,
                        },
                    ]),
                },
            ],
        }
    }

    async fn run(&self, cfg: &Value) -> CheckReport {
        let cfg: PrometheusConfig = match serde_json::from_value(cfg.clone()) {
            Ok(c) => c,
            Err(e) => return CheckReport::new(Status::Unknown, format!("bad config: {e}")),
        };
        match fetch_and_parse(&cfg.url, cfg.timeout_secs).await {
            Ok(series) => evaluate(&series, &cfg.rules),
            Err(e) => PrometheusCheck::map_scrape_error(e, &cfg.url),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::method;
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn series(metric: &str, labels: &[(&str, &str)], value: f64) -> Series {
        Series {
            metric: metric.into(),
            labels: labels
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
            value,
        }
    }
    fn rule(metric: &str, labels: &str, op: Op, threshold: f64, critical: bool) -> Rule {
        Rule {
            metric: metric.into(),
            labels: labels.into(),
            op,
            threshold,
            critical,
        }
    }

    #[test]
    fn empty_rules_is_unknown() {
        let r = evaluate(&[], &[]);
        assert_eq!(r.status, Status::Unknown);
        assert!(r.message.contains("no rules"));
    }

    #[test]
    fn critical_breach_reds_the_monitor() {
        let s = vec![series(
            "backrest_last_task_status",
            &[("task_type", "backup")],
            1.0,
        )];
        let r = evaluate(
            &s,
            &[rule(
                "backrest_last_task_status",
                "task_type=\"backup\"",
                Op::Ne,
                0.0,
                true,
            )],
        );
        assert_eq!(r.status, Status::Critical);
        assert_eq!(r.components.len(), 1);
        assert_eq!(r.components[0].status, Status::Critical);
    }

    #[test]
    fn noncritical_breach_caps_at_degraded() {
        let s = vec![series("warnings", &[], 3.0)];
        let r = evaluate(&s, &[rule("warnings", "", Op::Gt, 0.0, false)]);
        assert_eq!(r.status, Status::Degraded);
    }

    #[test]
    fn no_breach_is_ok() {
        let s = vec![series("warnings", &[], 0.0)];
        let r = evaluate(&s, &[rule("warnings", "", Op::Gt, 0.0, false)]);
        assert_eq!(r.status, Status::Ok);
    }

    #[test]
    fn loose_labels_fan_out_to_one_component_per_series() {
        let s = vec![
            series("status", &[("task", "backup")], 0.0),
            series("status", &[("task", "forget")], 0.0),
            series("status", &[("task", "hook")], 1.0),
        ];
        let r = evaluate(&s, &[rule("status", "", Op::Ne, 0.0, false)]);
        assert_eq!(r.components.len(), 3);
        assert_eq!(r.status, Status::Degraded); // the "hook" series breaches
    }

    #[test]
    fn label_matcher_selects_one_series() {
        let s = vec![
            series("status", &[("task", "backup")], 0.0),
            series("status", &[("task", "hook")], 1.0),
        ];
        let r = evaluate(&s, &[rule("status", "task=\"backup\"", Op::Ne, 0.0, true)]);
        assert_eq!(r.components.len(), 1);
        assert_eq!(r.status, Status::Ok);
    }

    #[test]
    fn zero_matches_is_unknown_component() {
        let r = evaluate(&[], &[rule("missing_metric", "", Op::Gt, 0.0, true)]);
        assert_eq!(r.components.len(), 1);
        assert_eq!(r.components[0].status, Status::Unknown);
        assert_eq!(r.status, Status::Unknown); // critical + unknown surfaces unknown
    }

    #[test]
    fn malformed_matcher_is_unknown_component() {
        let s = vec![series("m", &[], 1.0)];
        let r = evaluate(&s, &[rule("m", "garbage", Op::Gt, 0.0, false)]);
        assert_eq!(r.components[0].status, Status::Unknown);
    }

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

    const BACKREST: &str = concat!(
        "# TYPE backrest_last_task_status gauge\n",
        "backrest_last_task_status{task_type=\"backup\"} 0\n",
        "backrest_last_task_status{task_type=\"hook\"} 1\n",
        "# TYPE backrest_backup_file_warnings gauge\n",
        "backrest_backup_file_warnings{plan_id=\"stacks\"} 0\n",
    );

    #[tokio::test]
    async fn run_evaluates_rules_over_fetched_metrics() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_string(BACKREST))
            .mount(&server)
            .await;
        let cfg = json!({
            "url": server.uri(),
            "rules": [
                { "metric": "backrest_last_task_status", "labels": "task_type=\"backup\"", "op": "!=", "threshold": 0, "critical": true },
                { "metric": "backrest_backup_file_warnings", "op": ">", "threshold": 0 }
            ]
        });
        let report = PrometheusCheck.run(&cfg).await;
        assert_eq!(report.status, Status::Ok); // backup succeeded, no warnings
        assert_eq!(report.components.len(), 2);
    }

    #[tokio::test]
    async fn run_non_2xx_is_critical() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(503))
            .mount(&server)
            .await;
        let report = PrometheusCheck
            .run(&json!({ "url": server.uri(), "rules": [] }))
            .await;
        assert_eq!(report.status, Status::Critical);
    }

    // NOTE: prometheus-parse 0.2.5's `Scrape::parse` cannot actually fail on
    // body content — it silently skips lines it doesn't understand and only
    // ever returns Err when the *line iterator itself* yields an Err, which
    // never happens here since `fetch_and_parse` wraps every line in `Ok`.
    // Verified empirically: `<html>login</html>`, null bytes, unterminated
    // label braces, non-numeric bucket bounds, and free-text garbage all
    // parse successfully (0 or partial samples, no Err). So a genuine HTTP
    // round trip can never hit `ScrapeError::Unparseable`, and a test that
    // claimed otherwise would be passing for the wrong reason (or not at
    // all). Instead, exercise `run()`'s error-mapping for that variant
    // directly against a synthetic `ScrapeError::Unparseable`.
    #[test]
    fn unparseable_scrape_error_maps_to_unknown() {
        let report =
            PrometheusCheck::map_scrape_error(ScrapeError::Unparseable("bad body".into()), "url");
        assert_eq!(report.status, Status::Unknown);
    }

    #[tokio::test]
    async fn run_bad_config_is_unknown() {
        let report = PrometheusCheck
            .run(&json!({ "url": "http://x", "bogus": 1 }))
            .await;
        assert_eq!(report.status, Status::Unknown);
    }

    #[test]
    fn inspect_result_groups_label_values() {
        let s = vec![
            series("status", &[("task", "backup"), ("repo", "unraid")], 0.0),
            series("status", &[("task", "hook"), ("repo", "unraid")], 1.0),
            series("warnings", &[("plan", "stacks")], 0.0),
        ];
        let r = inspect_result(&s);
        let status = r.metrics.get("status").unwrap();
        assert_eq!(
            status.labels.get("task").unwrap(),
            &vec!["backup".to_string(), "hook".to_string()]
        );
        assert_eq!(
            status.labels.get("repo").unwrap(),
            &vec!["unraid".to_string()]
        ); // de-duped
        assert!(r.metrics.contains_key("warnings"));
    }
}
