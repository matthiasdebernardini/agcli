//! [`NextAction`]: the HATEOAS affordance every envelope carries, telling an
//! agent what to run next, and the [`ActionParam`]s that fill its template.

use std::collections::BTreeMap;

use serde::Serialize;
use serde_json::Value;

/// A command the agent can run next, as every envelope's `next_actions`
/// lists them.
///
/// With `params` the `command` is a template: the agent fills every
/// `<placeholder>` before running it. Without `params` it is literal and runs
/// as printed.
#[derive(Clone, Debug, Serialize, PartialEq)]
#[cfg_attr(feature = "deserialize", derive(serde::Deserialize))]
#[non_exhaustive]
pub struct NextAction {
    /// The command line to run, or the template to fill, such as
    /// `app get <id>`.
    pub command: String,
    /// Why the agent would run it.
    pub description: String,
    /// The template's placeholders by name. `None` makes the action literal,
    /// and the key is then left out of the JSON.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub params: Option<BTreeMap<String, ActionParam>>,
}

impl NextAction {
    /// An action that runs `command`, described by `description`. It stays
    /// literal until [`NextAction::with_param`] adds a param.
    #[must_use]
    pub fn new(command: impl Into<String>, description: impl Into<String>) -> Self {
        Self {
            command: command.into(),
            description: description.into(),
            params: None,
        }
    }

    /// Describe the placeholder `name` in the template, which makes the action
    /// a template. Params serialize sorted by name.
    #[must_use]
    pub fn with_param(mut self, name: impl Into<String>, param: ActionParam) -> Self {
        self.params
            .get_or_insert_with(BTreeMap::new)
            .insert(name.into(), param);
        self
    }
}

/// What an agent needs to fill one placeholder of a [`NextAction`] template.
///
/// Every field is optional and left out of the JSON when unset; the keys
/// serialize in this order.
#[derive(Clone, Debug, Default, Serialize, PartialEq)]
#[cfg_attr(feature = "deserialize", derive(serde::Deserialize))]
#[non_exhaustive]
pub struct ActionParam {
    /// What the value means.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// A value pre-filled from what just happened. The agent may override it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<Value>,
    /// The value that applies when the agent leaves the placeholder out.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default: Option<Value>,
    /// The valid choices. Serialized as the `enum` key.
    #[serde(rename = "enum", skip_serializing_if = "Option::is_none")]
    pub enum_values: Option<Vec<String>>,
    /// Whether the placeholder must be filled: `true` for a `<positional>`,
    /// `false` for a part of the template inside brackets.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub required: Option<bool>,
}

impl ActionParam {
    /// A param with nothing set.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Say what the value means.
    #[must_use]
    pub fn description(mut self, description: impl Into<String>) -> Self {
        self.description = Some(description.into());
        self
    }

    /// Pre-fill the value from what just happened, such as the sum a
    /// calculator just computed, so the agent's next step needs less.
    #[must_use]
    pub fn value(mut self, value: impl Into<Value>) -> Self {
        self.value = Some(value.into());
        self
    }

    /// Set the value that applies when the agent leaves the placeholder out.
    /// Serialized as the `default` key.
    #[must_use]
    pub fn default_value(mut self, value: impl Into<Value>) -> Self {
        self.default = Some(value.into());
        self
    }

    /// List the valid choices, serialized as the `enum` key.
    #[must_use]
    pub fn enum_values<I, S>(mut self, values: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.enum_values = Some(values.into_iter().map(Into::into).collect());
        self
    }

    /// Say whether the placeholder must be filled.
    #[must_use]
    pub fn required(mut self, required: bool) -> Self {
        self.required = Some(required);
        self
    }
}
