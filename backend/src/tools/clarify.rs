use std::collections::BTreeSet;

use serde::{Deserialize, Deserializer};
use serde_json::{Value as JsonValue, json};
use thiserror::Error;

use super::runtime::{ToolRisk, ToolSpec};

const MAX_ARGUMENT_BYTES: usize = 64 * 1024;
pub(crate) const MAX_QUESTION_CHARS: usize = 2_000;
pub(crate) const MAX_CHOICES: usize = 4;
pub(crate) const MAX_CHOICE_CHARS: usize = 500;

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub(crate) enum ClarifyContractError {
    #[error("tool arguments exceed the bounded input limit")]
    InputTooLarge,
    #[error("tool arguments are invalid")]
    InvalidArguments,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PreparedClarification {
    pub(crate) question: String,
    pub(crate) choices: Vec<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawClarifyArguments {
    question: String,
    #[serde(default, deserialize_with = "deserialize_choices")]
    choices: Vec<String>,
}

fn deserialize_choices<'de, D>(deserializer: D) -> Result<Vec<String>, D::Error>
where
    D: Deserializer<'de>,
{
    Vec::<String>::deserialize(deserializer)
}

pub(crate) fn prepare_clarification(
    raw_arguments_json: &str,
) -> Result<PreparedClarification, ClarifyContractError> {
    if raw_arguments_json.len() > MAX_ARGUMENT_BYTES {
        return Err(ClarifyContractError::InputTooLarge);
    }
    if raw_arguments_json.is_empty() {
        return Err(ClarifyContractError::InvalidArguments);
    }

    let value: JsonValue = serde_json::from_str(raw_arguments_json)
        .map_err(|_| ClarifyContractError::InvalidArguments)?;
    if !value.is_object() {
        return Err(ClarifyContractError::InvalidArguments);
    }
    let raw: RawClarifyArguments = serde_json::from_str(raw_arguments_json)
        .map_err(|_| ClarifyContractError::InvalidArguments)?;

    let question = normalize_bounded(raw.question, MAX_QUESTION_CHARS)?;
    if raw.choices.len() > MAX_CHOICES {
        return Err(ClarifyContractError::InvalidArguments);
    }

    let mut seen = BTreeSet::new();
    let mut choices = Vec::with_capacity(raw.choices.len());
    for choice in raw.choices {
        let choice = normalize_bounded(choice, MAX_CHOICE_CHARS)?;
        if !seen.insert(choice.clone()) {
            return Err(ClarifyContractError::InvalidArguments);
        }
        choices.push(choice);
    }

    Ok(PreparedClarification { question, choices })
}

pub(crate) fn clarify_spec() -> ToolSpec {
    ToolSpec {
        name: "clarify",
        toolset_id: "clarify",
        description: "Ask the user one open-ended or multiple-choice question when a meaningful decision requires interactive input. Put selectable options only in choices; omit choices or pass an empty array for free-form input.",
        input_schema: json!({
            "type": "object",
            "additionalProperties": false,
            "properties": {
                "question": {
                    "type": "string",
                    "minLength": 1,
                    "maxLength": MAX_QUESTION_CHARS,
                    "description": "The question text without enumerating choices."
                },
                "choices": {
                    "type": "array",
                    "minItems": 0,
                    "maxItems": MAX_CHOICES,
                    "uniqueItems": true,
                    "items": {
                        "type": "string",
                        "minLength": 1,
                        "maxLength": MAX_CHOICE_CHARS
                    },
                    "description": "Zero to four distinct selectable answers. Empty or omitted means free-form input."
                }
            },
            "required": ["question"]
        }),
        // Clarification has no external side effect. Its executor must suspend the
        // Run for interactive handling rather than treating this as a direct tool.
        risk: ToolRisk::ReadOnly,
    }
}

fn normalize_bounded(value: String, maximum_chars: usize) -> Result<String, ClarifyContractError> {
    let value = value.trim();
    if value.is_empty() || value.chars().count() > maximum_chars {
        return Err(ClarifyContractError::InvalidArguments);
    }
    Ok(value.to_owned())
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn schema_is_strict_bounded_and_marks_interaction_as_read_only() {
        let spec = clarify_spec();

        assert_eq!(spec.name, "clarify");
        assert_eq!(spec.toolset_id, "clarify");
        assert_eq!(spec.risk, ToolRisk::ReadOnly);
        assert_eq!(spec.input_schema["type"], "object");
        assert_eq!(spec.input_schema["additionalProperties"], false);
        assert_eq!(spec.input_schema["required"], json!(["question"]));
        assert_eq!(spec.input_schema["properties"]["question"]["minLength"], 1);
        assert_eq!(
            spec.input_schema["properties"]["question"]["maxLength"],
            MAX_QUESTION_CHARS
        );
        assert_eq!(spec.input_schema["properties"]["choices"]["minItems"], 0);
        assert_eq!(
            spec.input_schema["properties"]["choices"]["maxItems"],
            MAX_CHOICES
        );
        assert_eq!(
            spec.input_schema["properties"]["choices"]["uniqueItems"],
            true
        );
        assert_eq!(
            spec.input_schema["properties"]["choices"]["items"]["minLength"],
            1
        );
        assert_eq!(
            spec.input_schema["properties"]["choices"]["items"]["maxLength"],
            MAX_CHOICE_CHARS
        );
    }

    #[test]
    fn preparation_trims_question_and_choices_and_supports_free_form() {
        assert_eq!(
            prepare_clarification(r#"{"question":"  Which target?  "}"#).unwrap(),
            PreparedClarification {
                question: "Which target?".to_owned(),
                choices: Vec::new(),
            }
        );
        assert_eq!(
            prepare_clarification(r#"{"question":"Why?","choices":[]}"#).unwrap(),
            PreparedClarification {
                question: "Why?".to_owned(),
                choices: Vec::new(),
            }
        );
        assert_eq!(
            prepare_clarification(
                r#"{"question":" Pick one ","choices":[" alpha ","beta","gamma","delta"]}"#,
            )
            .unwrap(),
            PreparedClarification {
                question: "Pick one".to_owned(),
                choices: vec![
                    "alpha".to_owned(),
                    "beta".to_owned(),
                    "gamma".to_owned(),
                    "delta".to_owned(),
                ],
            }
        );
    }

    #[test]
    fn unknown_null_and_wrong_types_fail_closed() {
        for raw in [
            "",
            "{",
            "null",
            "[]",
            "{}",
            r#"{"question":null}"#,
            r#"{"question":1}"#,
            r#"{"question":"First?","question":"Second?"}"#,
            r#"{"question":"Q?","choices":null}"#,
            r#"{"question":"Q?","choices":[],"choices":[]}"#,
            r#"{"question":"Q?","choices":"yes"}"#,
            r#"{"question":"Q?","choices":[1]}"#,
            r#"{"question":"Q?","unknown":true}"#,
        ] {
            assert_eq!(
                prepare_clarification(raw),
                Err(ClarifyContractError::InvalidArguments),
                "accepted invalid arguments: {raw}"
            );
        }
    }

    #[test]
    fn question_and_choice_bounds_are_enforced_after_trim() {
        let exact_question = json!({"question": "q".repeat(MAX_QUESTION_CHARS)}).to_string();
        assert!(prepare_clarification(&exact_question).is_ok());
        let long_question = json!({"question": "q".repeat(MAX_QUESTION_CHARS + 1)}).to_string();
        assert_eq!(
            prepare_clarification(&long_question),
            Err(ClarifyContractError::InvalidArguments)
        );

        let exact_choice = json!({
            "question": "Q?",
            "choices": ["c".repeat(MAX_CHOICE_CHARS)]
        })
        .to_string();
        assert!(prepare_clarification(&exact_choice).is_ok());
        let long_choice = json!({
            "question": "Q?",
            "choices": ["c".repeat(MAX_CHOICE_CHARS + 1)]
        })
        .to_string();
        assert_eq!(
            prepare_clarification(&long_choice),
            Err(ClarifyContractError::InvalidArguments)
        );

        for raw in [
            r#"{"question":"   "}"#,
            r#"{"question":"Q?","choices":[" "]}"#,
        ] {
            assert_eq!(
                prepare_clarification(raw),
                Err(ClarifyContractError::InvalidArguments)
            );
        }
    }

    #[test]
    fn choice_count_and_normalized_uniqueness_are_enforced() {
        assert_eq!(
            prepare_clarification(r#"{"question":"Q?","choices":["a","b","c","d","e"]}"#),
            Err(ClarifyContractError::InvalidArguments)
        );
        for raw in [
            r#"{"question":"Q?","choices":["same","same"]}"#,
            r#"{"question":"Q?","choices":["same"," same "]}"#,
        ] {
            assert_eq!(
                prepare_clarification(raw),
                Err(ClarifyContractError::InvalidArguments)
            );
        }
    }

    #[test]
    fn raw_input_has_a_hard_byte_limit() {
        let oversized = " ".repeat(MAX_ARGUMENT_BYTES + 1);
        assert_eq!(
            prepare_clarification(&oversized),
            Err(ClarifyContractError::InputTooLarge)
        );
    }
}
