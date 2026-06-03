use std::fmt;

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// How the client wants the assistant's reply shaped, mirroring `OpenAI`'s
/// `response_format` request field. `Text` is the default free-form reply;
/// `JsonObject` asks for any valid JSON object; `JsonSchema` pins the reply
/// to a caller-supplied JSON Schema.
///
/// Coulisse enforces every variant the same way for every provider —
/// the instruction is appended to the system preamble and the reply is
/// validated server-side — so structured output works even on models that
/// have no native structured-output mode.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "snake_case", tag = "type")]
pub enum ResponseFormat {
    JsonObject,
    JsonSchema { json_schema: JsonSchemaSpec },
    Text,
}

/// The `json_schema` payload of a `json_schema` response format. Field names
/// match `OpenAI`'s so existing SDK calls deserialize unchanged.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct JsonSchemaSpec {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub name: String,
    pub schema: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub strict: Option<bool>,
}

impl ResponseFormat {
    /// True when the reply must be JSON. `Text` returns false, and the
    /// caller skips instruction injection and validation entirely.
    #[must_use]
    pub fn requires_json(&self) -> bool {
        !matches!(self, Self::Text)
    }

    /// Reject a malformed schema before any model call, so the client gets a
    /// 400 describing its own mistake rather than a wasted completion that
    /// can never validate. No-op for the non-schema variants.
    ///
    /// # Errors
    ///
    /// Returns `InvalidSchema` if the supplied JSON Schema cannot be compiled.
    pub fn check_schema(&self) -> Result<(), ResponseFormatError> {
        if let Self::JsonSchema { json_schema } = self {
            jsonschema::validator_for(&json_schema.schema)
                .map_err(|e| ResponseFormatError::InvalidSchema(e.to_string()))?;
        }
        Ok(())
    }

    /// A directive to append to the system preamble so the model emits JSON
    /// in the requested shape. `None` for `Text`. Phrased as a hard
    /// constraint and explicit about omitting markdown fences, because the
    /// server-side extractor tolerates fences but cleaner output needs fewer
    /// repair round-trips.
    #[must_use]
    pub fn instruction(&self) -> Option<String> {
        match self {
            Self::JsonObject => Some(
                "You must respond with a single valid JSON object and nothing else. \
                 Do not wrap it in markdown code fences and do not add any explanatory text."
                    .to_string(),
            ),
            Self::JsonSchema { json_schema } => {
                let schema = serde_json::to_string_pretty(&json_schema.schema)
                    .unwrap_or_else(|_| json_schema.schema.to_string());
                let purpose = json_schema
                    .description
                    .as_deref()
                    .map(|d| format!(" The schema describes: {d}."))
                    .unwrap_or_default();
                Some(format!(
                    "You must respond with a single JSON value that strictly conforms to the \
                     JSON Schema named `{name}`.{purpose} Output only the raw JSON value — no \
                     markdown code fences, no explanatory text before or after.\n\nJSON Schema:\n{schema}",
                    name = json_schema.name,
                ))
            }
            Self::Text => None,
        }
    }

    /// Validate `text` against this format and return the cleaned JSON string
    /// (re-serialized, so any surrounding prose or code fences the model
    /// added are stripped). `Text` passes through unchanged.
    ///
    /// # Errors
    ///
    /// Returns `NotJson` if the reply is not parseable JSON, or
    /// `SchemaViolation` if it parses but breaks the schema.
    pub fn validate(&self, text: &str) -> Result<String, ResponseFormatError> {
        match self {
            Self::JsonObject => Ok(extract_json(text)?.to_string()),
            Self::JsonSchema { json_schema } => {
                let value = extract_json(text)?;
                let validator = jsonschema::validator_for(&json_schema.schema)
                    .map_err(|e| ResponseFormatError::InvalidSchema(e.to_string()))?;
                let errors: Vec<String> = validator
                    .iter_errors(&value)
                    .take(MAX_REPORTED_VIOLATIONS)
                    .map(|e| e.to_string())
                    .collect();
                if errors.is_empty() {
                    Ok(value.to_string())
                } else {
                    Err(ResponseFormatError::SchemaViolation(errors.join("; ")))
                }
            }
            Self::Text => Ok(text.to_string()),
        }
    }

    /// A correction message to feed back to the model after a failed
    /// validation, naming the exact problem so the retry is targeted rather
    /// than a blind re-roll.
    #[must_use]
    pub fn repair_instruction(&self, error: &ResponseFormatError) -> String {
        format!(
            "Your previous response did not satisfy the required response format: {error}. \
             Respond again with only the corrected JSON value and nothing else."
        )
    }
}

/// Cap on schema violations surfaced in one error message. A deeply wrong
/// reply can break dozens of constraints; the first few are enough for the
/// model (or a human) to see the shape of the problem without an unbounded
/// wall of text.
const MAX_REPORTED_VIOLATIONS: usize = 5;

/// Parse the model's reply into a JSON value, tolerating a single markdown
/// code fence (```json … ```) since models reach for one even when told not
/// to. Anything else that isn't valid JSON is a `NotJson` error.
fn extract_json(text: &str) -> Result<Value, ResponseFormatError> {
    let candidate = strip_code_fence(text.trim());
    serde_json::from_str(candidate).map_err(|e| ResponseFormatError::NotJson(e.to_string()))
}

/// Strip one leading/trailing markdown code fence if the text is wrapped in
/// one, returning the inner payload. Leaves unfenced text untouched.
fn strip_code_fence(text: &str) -> &str {
    let Some(rest) = text.strip_prefix("```") else {
        return text;
    };
    // Drop the optional language tag on the opening fence line.
    let after_lang = rest.find('\n').map_or("", |i| &rest[i + 1..]);
    after_lang
        .trim_end()
        .strip_suffix("```")
        .unwrap_or(after_lang)
        .trim()
}

#[derive(Debug)]
pub enum ResponseFormatError {
    InvalidSchema(String),
    NotJson(String),
    SchemaViolation(String),
}

impl fmt::Display for ResponseFormatError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidSchema(msg) => write!(f, "invalid JSON schema: {msg}"),
            Self::NotJson(msg) => write!(f, "response was not valid JSON: {msg}"),
            Self::SchemaViolation(msg) => write!(f, "response did not match the schema: {msg}"),
        }
    }
}

impl std::error::Error for ResponseFormatError {}

#[cfg(test)]
mod tests {
    use super::*;

    fn schema_format() -> ResponseFormat {
        ResponseFormat::JsonSchema {
            json_schema: JsonSchemaSpec {
                description: Some("a person".into()),
                name: "person".into(),
                schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "age": {"type": "integer"},
                        "name": {"type": "string"}
                    },
                    "required": ["age", "name"],
                    "additionalProperties": false
                }),
                strict: Some(true),
            },
        }
    }

    #[test]
    fn text_passes_through_and_needs_no_json() {
        let fmt = ResponseFormat::Text;
        assert!(!fmt.requires_json());
        assert!(fmt.instruction().is_none());
        assert_eq!(fmt.validate("hello").unwrap(), "hello");
    }

    #[test]
    fn json_object_accepts_object_and_strips_prose() {
        let fmt = ResponseFormat::JsonObject;
        assert!(fmt.requires_json());
        let cleaned = fmt.validate("```json\n{\"a\": 1}\n```").unwrap();
        assert_eq!(cleaned, "{\"a\":1}");
    }

    #[test]
    fn json_object_rejects_non_json() {
        let fmt = ResponseFormat::JsonObject;
        assert!(matches!(
            fmt.validate("not json"),
            Err(ResponseFormatError::NotJson(_))
        ));
    }

    #[test]
    fn schema_accepts_conforming_value() {
        let fmt = schema_format();
        let cleaned = fmt.validate("{\"name\": \"Ada\", \"age\": 36}").unwrap();
        let value: Value = serde_json::from_str(&cleaned).unwrap();
        assert_eq!(value["name"], "Ada");
        assert_eq!(value["age"], 36);
    }

    #[test]
    fn schema_rejects_missing_required_field() {
        let fmt = schema_format();
        assert!(matches!(
            fmt.validate("{\"name\": \"Ada\"}"),
            Err(ResponseFormatError::SchemaViolation(_))
        ));
    }

    #[test]
    fn schema_rejects_wrong_type() {
        let fmt = schema_format();
        assert!(matches!(
            fmt.validate("{\"name\": \"Ada\", \"age\": \"old\"}"),
            Err(ResponseFormatError::SchemaViolation(_))
        ));
    }

    #[test]
    fn check_schema_rejects_malformed_schema() {
        let fmt = ResponseFormat::JsonSchema {
            json_schema: JsonSchemaSpec {
                description: None,
                name: "broken".into(),
                schema: serde_json::json!({"type": "not-a-real-type"}),
                strict: None,
            },
        };
        assert!(matches!(
            fmt.check_schema(),
            Err(ResponseFormatError::InvalidSchema(_))
        ));
    }

    #[test]
    fn instruction_embeds_schema_and_name() {
        let instruction = schema_format().instruction().unwrap();
        assert!(instruction.contains("person"));
        assert!(instruction.contains("a person"));
        assert!(instruction.contains("\"age\""));
    }

    #[test]
    fn deserializes_openai_shape() {
        let raw = serde_json::json!({
            "type": "json_schema",
            "json_schema": {
                "name": "person",
                "schema": {"type": "object"}
            }
        });
        let fmt: ResponseFormat = serde_json::from_value(raw).unwrap();
        assert!(matches!(fmt, ResponseFormat::JsonSchema { .. }));
    }

    #[test]
    fn strip_code_fence_leaves_plain_text() {
        assert_eq!(strip_code_fence("{\"a\":1}"), "{\"a\":1}");
    }
}
