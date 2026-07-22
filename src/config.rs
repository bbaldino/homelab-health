/// Bootstrap configuration. Only the essentials needed before the app can
/// serve requests come from the environment; everything else lives in the DB.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    pub bind: String,
    pub db_url: String,
}

impl Config {
    /// Resolve config from a generic getter (so it is testable without touching
    /// process-global env state).
    pub fn resolve(get: impl Fn(&str) -> Option<String>) -> Config {
        let bind = get("HEALTH_BIND").unwrap_or_else(|| "0.0.0.0:8080".to_string());
        let db_path = get("HEALTH_DB").unwrap_or_else(|| "health.db".to_string());
        Config {
            bind,
            db_url: format!("sqlite://{db_path}"),
        }
    }

    pub fn from_env() -> Config {
        Config::resolve(|k| std::env::var(k).ok())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn getter(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let map: HashMap<String, String> = pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        move |k: &str| map.get(k).cloned()
    }

    #[test]
    fn defaults_when_unset() {
        let cfg = Config::resolve(getter(&[]));
        assert_eq!(cfg.bind, "0.0.0.0:8080");
        assert_eq!(cfg.db_url, "sqlite://health.db");
    }

    #[test]
    fn reads_env_values() {
        let cfg = Config::resolve(getter(&[
            ("HEALTH_BIND", "127.0.0.1:9000"),
            ("HEALTH_DB", "/data/h.db"),
        ]));
        assert_eq!(cfg.bind, "127.0.0.1:9000");
        assert_eq!(cfg.db_url, "sqlite:///data/h.db");
    }
}
