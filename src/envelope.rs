use std::collections::BTreeMap;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Serialize, Serializer};
use serde_json::Value;

fn epoch_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// HATEOAS action template that tells an agent what to run next.
#[derive(Clone, Debug, Serialize, PartialEq)]
#[cfg_attr(feature = "deserialize", derive(serde::Deserialize))]
pub struct NextAction {
    pub command: String,
    pub description: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub params: Option<BTreeMap<String, ActionParam>>,
}

impl NextAction {
    pub fn new(command: impl Into<String>, description: impl Into<String>) -> Self {
        Self {
            command: command.into(),
            description: description.into(),
            params: None,
        }
    }

    pub fn with_param(mut self, name: impl Into<String>, param: ActionParam) -> Self {
        self.params
            .get_or_insert_with(BTreeMap::new)
            .insert(name.into(), param);
        self
    }
}

/// Metadata for a templated `next_action` parameter.
#[derive(Clone, Debug, Serialize, PartialEq)]
#[cfg_attr(feature = "deserialize", derive(serde::Deserialize))]
pub struct ActionParam {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default: Option<Value>,
    #[serde(rename = "enum", skip_serializing_if = "Option::is_none")]
    pub enum_values: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub required: Option<bool>,
}

impl ActionParam {
    pub fn new() -> Self {
        Self {
            description: None,
            value: None,
            default: None,
            enum_values: None,
            required: None,
        }
    }

    pub fn description(mut self, description: impl Into<String>) -> Self {
        self.description = Some(description.into());
        self
    }

    pub fn value(mut self, value: impl Into<Value>) -> Self {
        self.value = Some(value.into());
        self
    }

    pub fn default(mut self, value: impl Into<Value>) -> Self {
        self.default = Some(value.into());
        self
    }

    pub fn enum_values<I, S>(mut self, values: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.enum_values = Some(values.into_iter().map(Into::into).collect());
        self
    }

    pub fn required(mut self, required: bool) -> Self {
        self.required = Some(required);
        self
    }
}

impl Default for ActionParam {
    fn default() -> Self {
        Self::new()
    }
}

/// Machine-readable error payload.
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[cfg_attr(feature = "deserialize", derive(serde::Deserialize))]
pub struct ErrorBody {
    pub message: String,
    pub code: String,
    pub retryable: bool,
}

impl ErrorBody {
    pub fn new(message: impl Into<String>, code: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            code: code.into(),
            retryable: false,
        }
    }

    pub fn retryable(mut self, retryable: bool) -> Self {
        self.retryable = retryable;
        self
    }
}

/// Success response envelope.
///
/// Always serializes with `"ok": true`. The `ok` field is not stored;
/// it is injected at serialization time and validated at deserialization time.
#[derive(Clone, Debug, PartialEq)]
pub struct SuccessEnvelope {
    pub command: String,
    pub timestamp: u64,
    pub schema_version: Option<String>,
    pub result: Value,
    pub next_actions: Vec<NextAction>,
}

impl SuccessEnvelope {
    pub fn new(command: impl Into<String>, result: Value, next_actions: Vec<NextAction>) -> Self {
        Self {
            command: command.into(),
            timestamp: epoch_secs(),
            schema_version: None,
            result,
            next_actions,
        }
    }

    pub fn schema_version(mut self, version: impl Into<String>) -> Self {
        self.schema_version = Some(version.into());
        self
    }
}

impl Serialize for SuccessEnvelope {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;
        let field_count = 5 + usize::from(self.schema_version.is_some());
        let mut s = serializer.serialize_struct("SuccessEnvelope", field_count)?;
        s.serialize_field("ok", &true)?;
        s.serialize_field("command", &self.command)?;
        s.serialize_field("timestamp", &self.timestamp)?;
        if let Some(ref sv) = self.schema_version {
            s.serialize_field("schema_version", sv)?;
        }
        s.serialize_field("result", &self.result)?;
        s.serialize_field("next_actions", &self.next_actions)?;
        s.end()
    }
}

#[cfg(feature = "deserialize")]
impl<'de> serde::Deserialize<'de> for SuccessEnvelope {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(serde::Deserialize)]
        struct Raw {
            ok: bool,
            command: String,
            #[serde(default)]
            timestamp: u64,
            #[serde(default)]
            schema_version: Option<String>,
            result: Value,
            next_actions: Vec<NextAction>,
        }
        let raw = Raw::deserialize(deserializer)?;
        if !raw.ok {
            return Err(serde::de::Error::custom(
                "expected ok=true for SuccessEnvelope",
            ));
        }
        Ok(SuccessEnvelope {
            command: raw.command,
            timestamp: raw.timestamp,
            schema_version: raw.schema_version,
            result: raw.result,
            next_actions: raw.next_actions,
        })
    }
}

/// Error response envelope.
///
/// Always serializes with `"ok": false`. The `ok` field is not stored;
/// it is injected at serialization time and validated at deserialization time.
#[derive(Clone, Debug, PartialEq)]
pub struct ErrorEnvelope {
    pub command: String,
    pub timestamp: u64,
    pub schema_version: Option<String>,
    pub error: ErrorBody,
    pub fix: String,
    pub next_actions: Vec<NextAction>,
}

impl ErrorEnvelope {
    pub fn new(
        command: impl Into<String>,
        message: impl Into<String>,
        code: impl Into<String>,
        fix: impl Into<String>,
        next_actions: Vec<NextAction>,
    ) -> Self {
        Self {
            command: command.into(),
            timestamp: epoch_secs(),
            schema_version: None,
            error: ErrorBody::new(message, code),
            fix: fix.into(),
            next_actions,
        }
    }

    pub fn retryable(mut self, retryable: bool) -> Self {
        self.error.retryable = retryable;
        self
    }

    pub fn schema_version(mut self, version: impl Into<String>) -> Self {
        self.schema_version = Some(version.into());
        self
    }
}

impl Serialize for ErrorEnvelope {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;
        let field_count = 6 + usize::from(self.schema_version.is_some());
        let mut s = serializer.serialize_struct("ErrorEnvelope", field_count)?;
        s.serialize_field("ok", &false)?;
        s.serialize_field("command", &self.command)?;
        s.serialize_field("timestamp", &self.timestamp)?;
        if let Some(ref sv) = self.schema_version {
            s.serialize_field("schema_version", sv)?;
        }
        s.serialize_field("error", &self.error)?;
        s.serialize_field("fix", &self.fix)?;
        s.serialize_field("next_actions", &self.next_actions)?;
        s.end()
    }
}

#[cfg(feature = "deserialize")]
impl<'de> serde::Deserialize<'de> for ErrorEnvelope {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(serde::Deserialize)]
        struct Raw {
            ok: bool,
            command: String,
            #[serde(default)]
            timestamp: u64,
            #[serde(default)]
            schema_version: Option<String>,
            error: ErrorBody,
            fix: String,
            next_actions: Vec<NextAction>,
        }
        let raw = Raw::deserialize(deserializer)?;
        if raw.ok {
            return Err(serde::de::Error::custom(
                "expected ok=false for ErrorEnvelope",
            ));
        }
        Ok(ErrorEnvelope {
            command: raw.command,
            timestamp: raw.timestamp,
            schema_version: raw.schema_version,
            error: raw.error,
            fix: raw.fix,
            next_actions: raw.next_actions,
        })
    }
}

/// Unified envelope enum.
#[derive(Clone, Debug, Serialize, PartialEq)]
#[serde(untagged)]
pub enum Envelope {
    Success(SuccessEnvelope),
    Error(ErrorEnvelope),
}

#[cfg(feature = "deserialize")]
impl<'de> serde::Deserialize<'de> for Envelope {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = Value::deserialize(deserializer)?;
        let ok = value
            .get("ok")
            .and_then(Value::as_bool)
            .ok_or_else(|| serde::de::Error::custom("missing or invalid `ok` field"))?;
        if ok {
            serde_json::from_value(value)
                .map(Envelope::Success)
                .map_err(serde::de::Error::custom)
        } else {
            serde_json::from_value(value)
                .map(Envelope::Error)
                .map_err(serde::de::Error::custom)
        }
    }
}

impl Envelope {
    pub fn command(&self) -> &str {
        match self {
            Self::Success(value) => &value.command,
            Self::Error(value) => &value.command,
        }
    }

    pub fn ok(&self) -> bool {
        matches!(self, Self::Success(_))
    }

    pub fn exit_code(&self) -> i32 {
        if self.ok() { 0 } else { 1 }
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_else(|e| {
            serde_json::to_string(&serde_json::json!({
                "ok": false,
                "error": format!("serialization failed: {e}")
            }))
            .unwrap_or_default()
        })
    }

    pub fn to_json_pretty(&self) -> String {
        serde_json::to_string_pretty(self).unwrap_or_else(|e| {
            serde_json::to_string_pretty(&serde_json::json!({
                "ok": false,
                "error": format!("serialization failed: {e}")
            }))
            .unwrap_or_default()
        })
    }
}

impl From<SuccessEnvelope> for Envelope {
    fn from(value: SuccessEnvelope) -> Self {
        Self::Success(value)
    }
}

impl From<ErrorEnvelope> for Envelope {
    fn from(value: ErrorEnvelope) -> Self {
        Self::Error(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn success_envelope_serializes_required_fields() {
        let envelope = SuccessEnvelope::new(
            "wokhei status",
            json!({ "healthy": true }),
            vec![NextAction::new("wokhei status", "Re-check health")],
        );

        let encoded = serde_json::to_value(&envelope).expect("must serialize");
        assert_eq!(encoded["ok"], Value::Bool(true));
        assert_eq!(
            encoded["command"],
            Value::String("wokhei status".to_string())
        );
        assert!(encoded["timestamp"].is_u64());
        assert!(encoded["next_actions"].is_array());
        // schema_version omitted when None
        assert!(encoded.get("schema_version").is_none());
    }

    #[test]
    fn success_envelope_with_schema_version() {
        let envelope = SuccessEnvelope::new("cmd", json!(null), vec![]).schema_version("test.v1");
        let encoded = serde_json::to_value(&envelope).expect("must serialize");
        assert_eq!(encoded["schema_version"], json!("test.v1"));
    }

    #[test]
    fn error_envelope_serializes_fix() {
        let envelope = ErrorEnvelope::new(
            "wokhei publish",
            "invalid json",
            "INVALID_JSON",
            "Use valid JSON input",
            vec![],
        );

        let encoded = serde_json::to_value(&envelope).expect("must serialize");
        assert_eq!(encoded["ok"], Value::Bool(false));
        assert_eq!(
            encoded["error"]["code"],
            Value::String("INVALID_JSON".to_string())
        );
        assert_eq!(encoded["error"]["retryable"], Value::Bool(false));
        assert!(encoded["timestamp"].is_u64());
        assert_eq!(
            encoded["fix"],
            Value::String("Use valid JSON input".to_string())
        );
        // schema_version omitted when None
        assert!(encoded.get("schema_version").is_none());
    }

    #[test]
    fn error_envelope_retryable() {
        let envelope = ErrorEnvelope::new("cmd", "err", "E", "fix", vec![])
            .retryable(true)
            .schema_version("test.v1");
        let encoded = serde_json::to_value(&envelope).expect("must serialize");
        assert_eq!(encoded["error"]["retryable"], Value::Bool(true));
        assert_eq!(encoded["schema_version"], json!("test.v1"));
    }

    #[cfg(feature = "deserialize")]
    #[test]
    fn success_envelope_roundtrips() {
        let original = SuccessEnvelope::new(
            "test cmd",
            json!({ "data": 42 }),
            vec![NextAction::new("test cmd", "Run again")],
        )
        .schema_version("test.v1");
        let json = serde_json::to_string(&original).expect("must serialize");
        let decoded: SuccessEnvelope = serde_json::from_str(&json).expect("must deserialize");
        assert_eq!(original, decoded);
    }

    #[cfg(feature = "deserialize")]
    #[test]
    fn error_envelope_roundtrips() {
        let original = ErrorEnvelope::new("test cmd", "oops", "ERR", "fix it", vec![])
            .retryable(true)
            .schema_version("test.v1");
        let json = serde_json::to_string(&original).expect("must serialize");
        let decoded: ErrorEnvelope = serde_json::from_str(&json).expect("must deserialize");
        assert_eq!(original, decoded);
    }

    #[cfg(feature = "deserialize")]
    #[test]
    fn success_envelope_rejects_ok_false() {
        let json = r#"{"ok":false,"command":"test","timestamp":0,"result":null,"next_actions":[]}"#;
        let result: Result<SuccessEnvelope, _> = serde_json::from_str(json);
        assert!(result.is_err());
    }

    #[cfg(feature = "deserialize")]
    #[test]
    fn error_envelope_rejects_ok_true() {
        let json = r#"{"ok":true,"command":"test","timestamp":0,"error":{"message":"x","code":"X","retryable":false},"fix":"y","next_actions":[]}"#;
        let result: Result<ErrorEnvelope, _> = serde_json::from_str(json);
        assert!(result.is_err());
    }

    #[cfg(feature = "deserialize")]
    #[test]
    fn envelope_dispatches_by_ok_field() {
        let success_json =
            serde_json::to_string(&SuccessEnvelope::new("cmd", json!(null), vec![])).unwrap();
        let error_json =
            serde_json::to_string(&ErrorEnvelope::new("cmd", "err", "E", "fix", vec![])).unwrap();

        let s: Envelope = serde_json::from_str(&success_json).expect("must deserialize success");
        assert!(matches!(s, Envelope::Success(_)));

        let e: Envelope = serde_json::from_str(&error_json).expect("must deserialize error");
        assert!(matches!(e, Envelope::Error(_)));
    }
}
