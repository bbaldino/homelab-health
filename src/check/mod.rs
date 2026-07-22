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
}

#[derive(Clone, Debug, Serialize)]
pub struct Field {
    pub name: &'static str,
    pub kind: FieldKind,
    pub required: bool,
    pub default: Option<Value>,
    pub help: &'static str,
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
}

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
}
