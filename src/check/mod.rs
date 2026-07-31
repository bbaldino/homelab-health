use crate::report::CheckReport;
use crate::status::Status;
use async_trait::async_trait;
use serde::Serialize;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;

#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FieldKind {
    String,
    Int,
    Float,
    Bool,
    List,
}

#[derive(Clone, Debug, Serialize)]
pub struct Field {
    pub name: &'static str,
    pub kind: FieldKind,
    pub required: bool,
    pub default: Option<Value>,
    pub help: &'static str,
    pub secret: bool,
    /// Fixed set of allowed values → the UI renders a dropdown. None = free input.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub options: Option<Vec<Value>>,
    /// Sub-field schema for a `List` item. None for scalar fields.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fields: Option<Vec<Field>>,
}

#[derive(Clone, Debug, Serialize)]
pub struct ConfigSchema {
    pub fields: Vec<Field>,
}

#[async_trait]
pub trait CheckType: Send + Sync {
    fn type_id(&self) -> &'static str;
    fn schema(&self) -> ConfigSchema;
    async fn run(&self, cfg: &Value) -> CheckReport;
}

#[derive(Default)]
pub struct Registry {
    types: HashMap<&'static str, Arc<dyn CheckType>>,
}

impl Registry {
    pub fn new() -> Self {
        Registry {
            types: HashMap::new(),
        }
    }

    pub fn register(&mut self, check: Arc<dyn CheckType>) {
        self.types.insert(check.type_id(), check);
    }

    pub fn get(&self, type_id: &str) -> Option<Arc<dyn CheckType>> {
        self.types.get(type_id).cloned()
    }

    pub async fn run(&self, type_id: &str, cfg: &Value) -> CheckReport {
        match self.get(type_id) {
            Some(check) => check.run(cfg).await,
            None => CheckReport::new(
                Status::Unknown,
                format!("no check type registered for '{type_id}'"),
            ),
        }
    }

    pub fn schemas(&self) -> Vec<(&'static str, ConfigSchema)> {
        self.types
            .values()
            .map(|c| (c.type_id(), c.schema()))
            .collect()
    }

    /// A registry pre-loaded with every built-in check type.
    pub fn with_builtins() -> Self {
        let mut reg = Registry::new();
        reg.register(Arc::new(crate::check::http::HttpCheck));
        reg.register(Arc::new(crate::check::tcp::TcpCheck));
        reg.register(Arc::new(crate::check::frigate::FrigateCameraCheck));
        reg.register(Arc::new(crate::check::json_health::JsonHealthCheck));
        reg.register(Arc::new(crate::check::music_assistant::MusicAssistantCheck));
        reg.register(Arc::new(crate::check::unraid::UnraidCheck));
        reg.register(Arc::new(crate::check::prometheus::PrometheusCheck));
        reg
    }
}

pub mod frigate;
pub mod http;
pub mod json_health;
pub mod music_assistant;
pub mod prometheus;
pub mod tcp;
pub mod unraid;

#[cfg(test)]
mod tests {
    use super::*;

    struct AlwaysOk;

    #[async_trait]
    impl CheckType for AlwaysOk {
        fn type_id(&self) -> &'static str {
            "always-ok"
        }
        fn schema(&self) -> ConfigSchema {
            ConfigSchema { fields: vec![] }
        }
        async fn run(&self, _cfg: &Value) -> CheckReport {
            CheckReport::ok("fine")
        }
    }

    #[tokio::test]
    async fn registered_type_runs_via_registry() {
        let mut reg = Registry::new();
        reg.register(Arc::new(AlwaysOk));
        let report = reg.run("always-ok", &Value::Null).await;
        assert_eq!(report.status, Status::Ok);
    }

    #[tokio::test]
    async fn unknown_type_returns_unknown() {
        let reg = Registry::new();
        let report = reg.run("nope", &Value::Null).await;
        assert_eq!(report.status, Status::Unknown);
        assert!(report.message.contains("nope"));
    }

    #[test]
    fn with_builtins_registers_all() {
        let reg = Registry::with_builtins();
        assert!(reg.get("http").is_some());
        assert!(reg.get("tcp").is_some());
        assert!(reg.get("frigate-camera").is_some());
        assert!(reg.get("json-health").is_some());
        assert!(reg.get("music-assistant").is_some());
        assert!(reg.get("unraid").is_some());
        assert!(reg.get("prometheus").is_some());
        assert_eq!(reg.schemas().len(), 7);
    }

    #[test]
    fn list_field_with_options_serializes() {
        let f = Field {
            name: "rules",
            kind: FieldKind::List,
            required: false,
            default: None,
            help: "rules",
            secret: false,
            options: None,
            fields: Some(vec![Field {
                name: "op",
                kind: FieldKind::String,
                required: true,
                default: None,
                help: "comparison",
                secret: false,
                options: Some(vec![serde_json::json!(">"), serde_json::json!("!=")]),
                fields: None,
            }]),
        };
        let v = serde_json::to_value(&f).unwrap();
        assert_eq!(v["kind"], "list");
        assert_eq!(v["fields"][0]["name"], "op");
        assert_eq!(v["fields"][0]["options"][0], ">");
        // Absent optionals must be omitted, not null (keeps existing responses stable).
        let scalar = serde_json::to_value(&f.fields.as_ref().unwrap()[0]).unwrap();
        let plain = serde_json::to_value(Field {
            name: "url",
            kind: FieldKind::String,
            required: true,
            default: None,
            help: "u",
            secret: false,
            options: None,
            fields: None,
        })
        .unwrap();
        assert!(plain.get("options").is_none());
        assert!(plain.get("fields").is_none());
        let _ = scalar;
    }

    #[test]
    fn music_assistant_token_field_is_secret() {
        let reg = Registry::with_builtins();
        let (_, schema) = reg
            .schemas()
            .into_iter()
            .find(|(id, _)| *id == "music-assistant")
            .unwrap();
        let token = schema.fields.iter().find(|f| f.name == "token").unwrap();
        assert!(token.secret, "token field must be marked secret");
        let url = schema.fields.iter().find(|f| f.name == "url").unwrap();
        assert!(!url.secret);
    }
}
