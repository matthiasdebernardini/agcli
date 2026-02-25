use std::collections::BTreeMap;

use serde::Serialize;
use serde_json::Value;

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
}

impl ErrorBody {
    pub fn new(message: impl Into<String>, code: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            code: code.into(),
        }
    }
}

/// Success response envelope.
#[derive(Clone, Debug, Serialize, PartialEq)]
#[cfg_attr(feature = "deserialize", derive(serde::Deserialize))]
pub struct SuccessEnvelope {
    pub ok: bool,
    pub command: String,
    pub result: Value,
    pub next_actions: Vec<NextAction>,
}

impl SuccessEnvelope {
    pub fn new(command: impl Into<String>, result: Value, next_actions: Vec<NextAction>) -> Self {
        Self {
            ok: true,
            command: command.into(),
            result,
            next_actions,
        }
    }
}

/// Error response envelope.
#[derive(Clone, Debug, Serialize, PartialEq)]
#[cfg_attr(feature = "deserialize", derive(serde::Deserialize))]
pub struct ErrorEnvelope {
    pub ok: bool,
    pub command: String,
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
            ok: false,
            command: command.into(),
            error: ErrorBody::new(message, code),
            fix: fix.into(),
            next_actions,
        }
    }
}

/// Unified envelope enum.
#[derive(Clone, Debug, Serialize, PartialEq)]
#[cfg_attr(feature = "deserialize", derive(serde::Deserialize))]
#[serde(untagged)]
pub enum Envelope {
    Success(SuccessEnvelope),
    Error(ErrorEnvelope),
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
        serde_json::to_string(self)
            .unwrap_or_else(|e| format!(r#"{{"ok":false,"error":"serialization failed: {e}"}}"#))
    }

    pub fn to_json_pretty(&self) -> String {
        serde_json::to_string_pretty(self)
            .unwrap_or_else(|e| format!(r#"{{"ok":false,"error":"serialization failed: {e}"}}"#))
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

        let encoded = serde_json::to_value(envelope).expect("must serialize");
        assert_eq!(encoded["ok"], Value::Bool(true));
        assert_eq!(
            encoded["command"],
            Value::String("wokhei status".to_string())
        );
        assert!(encoded["next_actions"].is_array());
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

        let encoded = serde_json::to_value(envelope).expect("must serialize");
        assert_eq!(encoded["ok"], Value::Bool(false));
        assert_eq!(
            encoded["error"]["code"],
            Value::String("INVALID_JSON".to_string())
        );
        assert_eq!(
            encoded["fix"],
            Value::String("Use valid JSON input".to_string())
        );
    }
}
