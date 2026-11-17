//! The model behind the judge: a small trait ([`Provider`]) and its one implementation for
//! the Gemini Decisions API ([`GeminiDecisions`]).

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use serde_json::{Map, Value, json};

/// One question for the model.
#[derive(Debug, Clone, PartialEq)]
pub struct ModelQuestion {
    /// Unique within the request, e.g. `waiting` or `advice_3`.
    pub id: String,
    pub kind: QuestionShape,
}

#[derive(Debug, Clone, PartialEq)]
pub enum QuestionShape {
    /// Is the statement in `instructions` true? The answer is its probability.
    Statement { instructions: String },
    /// Which option answers `instructions`? Options are `(key, description)`; keys are
    /// unique.
    Choice {
        instructions: String,
        options: Vec<(String, String)>,
    },
}

/// The model's answer to one question.
#[derive(Debug, Clone, PartialEq)]
pub enum ModelAnswer {
    /// Probability that the statement is true, 0 to 1.
    Probability(f64),
    /// The first choice and the probability of each option.
    Choice {
        first: String,
        probabilities: BTreeMap<String, f64>,
    },
}

/// The answers of one call, by question id. Questions the model did not answer are missing.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ModelAnswers {
    pub answers: BTreeMap<String, ModelAnswer>,
    /// Time the model took, in milliseconds.
    pub ms: u64,
}

/// A model that answers the judge's questions about one turn.
pub trait Provider {
    /// One call: the turn description and all questions. `Err` is a short reason for the
    /// `judge.run` row; it must never contain the API key.
    fn decide(&self, input: &str, questions: &[ModelQuestion]) -> Result<ModelAnswers, String>;
}

/// Gemini Decisions API (`POST <endpoint>`, header `x-goog-api-key`).
pub struct GeminiDecisions {
    pub endpoint: String,
    pub model: String,
    pub api_key: String,
    pub timeout: Duration,
}

/// `criteria` of a statement question.
const TRUE_DESCRIPTION: &str = "The statement is true.";
const FALSE_DESCRIPTION: &str = "The statement is false.";

impl GeminiDecisions {
    /// The request body.
    pub fn body(&self, input: &str, questions: &[ModelQuestion]) -> Value {
        let mut qs = Map::new();
        for q in questions {
            let v = match &q.kind {
                QuestionShape::Statement { instructions } => json!({
                    "type": "noul",
                    "instructions": instructions,
                    "criteria": {
                        "true_description": TRUE_DESCRIPTION,
                        "false_description": FALSE_DESCRIPTION,
                    },
                }),
                QuestionShape::Choice {
                    instructions,
                    options,
                } => {
                    let criteria: Map<String, Value> = options
                        .iter()
                        .map(|(k, d)| (k.clone(), Value::String(d.clone())))
                        .collect();
                    json!({ "type": "choice", "instructions": instructions, "criteria": criteria })
                }
            };
            qs.insert(q.id.clone(), v);
        }
        json!({ "model": self.model, "input": input, "questions": qs })
    }

    /// Removes the key from a text that may echo it.
    fn scrub(&self, text: &str) -> String {
        if self.api_key.is_empty() {
            text.to_string()
        } else {
            text.replace(&self.api_key, "[key]")
        }
    }

    fn agent(&self) -> ureq::Agent {
        use ureq::tls::{RootCerts, TlsConfig, TlsProvider};
        let config = ureq::Agent::config_builder()
            .timeout_global(Some(self.timeout))
            .http_status_as_error(false)
            .tls_config(
                TlsConfig::builder()
                    .provider(TlsProvider::NativeTls)
                    .root_certs(RootCerts::PlatformVerifier)
                    .build(),
            )
            .build();
        ureq::Agent::new_with_config(config)
    }
}

impl Provider for GeminiDecisions {
    fn decide(&self, input: &str, questions: &[ModelQuestion]) -> Result<ModelAnswers, String> {
        let body = self.body(input, questions).to_string();
        let start = Instant::now();
        let response = self
            .agent()
            .post(&self.endpoint)
            .header("content-type", "application/json")
            .header("accept", "application/json")
            .header("x-goog-api-key", &self.api_key)
            .send(body.as_bytes());
        let mut response = match response {
            Ok(r) => r,
            Err(ureq::Error::Timeout(_)) => return Err("timeout".to_string()),
            Err(e) => return Err(short(&self.scrub(&format!("request failed: {e}")))),
        };
        let status = response.status().as_u16();
        let text = match response.body_mut().read_to_string() {
            Ok(t) => t,
            Err(ureq::Error::Timeout(_)) => return Err("timeout".to_string()),
            Err(e) => return Err(short(&self.scrub(&format!("cannot read the answer: {e}")))),
        };
        let ms = start.elapsed().as_millis() as u64;
        if status != 200 {
            let message = serde_json::from_str::<Value>(&text)
                .ok()
                .and_then(|v| v["error"]["message"].as_str().map(str::to_string))
                .unwrap_or_default();
            let reason = if message.is_empty() {
                format!("HTTP {status}")
            } else {
                format!("HTTP {status}: {message}")
            };
            return Err(short(&self.scrub(&reason)));
        }
        let mut answers = parse_answers(&text)?;
        answers.ms = ms;
        Ok(answers)
    }
}

/// At most 200 characters, on one line.
fn short(text: &str) -> String {
    let one_line = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if one_line.chars().count() <= 200 {
        one_line
    } else {
        let mut s: String = one_line.chars().take(199).collect();
        s.push('…');
        s
    }
}

/// Reads a Decisions API response: `answers.<id>` is `{type: "noul", noul: p}` or
/// `{type: "choice", choice, probabilities}`. Answers of another shape are left out.
pub fn parse_answers(text: &str) -> Result<ModelAnswers, String> {
    let value: Value =
        serde_json::from_str(text).map_err(|_| "unparsable answer: not JSON".to_string())?;
    let Some(items) = value.get("answers").and_then(Value::as_object) else {
        return Err("unparsable answer: no answers".to_string());
    };
    let mut answers = BTreeMap::new();
    for (id, a) in items {
        let answer = if let Some(p) = a.get("noul").and_then(Value::as_f64) {
            ModelAnswer::Probability(p)
        } else if let Some(first) = a.get("choice").and_then(Value::as_str) {
            let probabilities = a
                .get("probabilities")
                .and_then(Value::as_object)
                .map(|m| {
                    m.iter()
                        .filter_map(|(k, v)| v.as_f64().map(|p| (k.clone(), p)))
                        .collect()
                })
                .unwrap_or_default();
            ModelAnswer::Choice {
                first: first.to_string(),
                probabilities,
            }
        } else {
            continue;
        };
        answers.insert(id.clone(), answer);
    }
    Ok(ModelAnswers { answers, ms: 0 })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gemini() -> GeminiDecisions {
        GeminiDecisions {
            endpoint: "http://127.0.0.1:1/x".into(),
            model: "gemini-decision-1".into(),
            api_key: "secret-key".into(),
            timeout: Duration::from_millis(100),
        }
    }

    #[test]
    fn request_body_shape() {
        let qs = vec![
            ModelQuestion {
                id: "waiting".into(),
                kind: QuestionShape::Statement {
                    instructions: "S".into(),
                },
            },
            ModelQuestion {
                id: "offcard_work".into(),
                kind: QuestionShape::Choice {
                    instructions: "Q".into(),
                    options: vec![("#1 a".into(), String::new()), ("none".into(), "D".into())],
                },
            },
        ];
        assert_eq!(
            gemini().body("IN", &qs),
            json!({
                "model": "gemini-decision-1",
                "input": "IN",
                "questions": {
                    "waiting": {"type": "noul", "instructions": "S", "criteria": {
                        "true_description": "The statement is true.",
                        "false_description": "The statement is false."}},
                    "offcard_work": {"type": "choice", "instructions": "Q",
                        "criteria": {"#1 a": "", "none": "D"}},
                }
            })
        );
    }

    #[test]
    fn parses_noul_and_choice_answers() {
        let a = parse_answers(
            r#"{"answers": {
                "waiting": {"confidence": 0.95, "noul": 0.95, "type": "noul"},
                "offcard_work": {"confidence": 0.99, "choice": "no project work this turn",
                    "probabilities": {"project work that no listed card covers": 0.007,
                                      "no project work this turn": 0.993}, "type": "choice"},
                "odd": {"type": "other"}
            }, "usage": {"total_tokens": 2607}}"#,
        )
        .unwrap();
        assert_eq!(a.answers.len(), 2);
        assert_eq!(a.answers["waiting"], ModelAnswer::Probability(0.95));
        let ModelAnswer::Choice {
            first,
            probabilities,
        } = &a.answers["offcard_work"]
        else {
            panic!()
        };
        assert_eq!(first, "no project work this turn");
        assert_eq!(probabilities["project work that no listed card covers"], 0.007);
        assert_eq!(
            parse_answers("<html>").unwrap_err(),
            "unparsable answer: not JSON"
        );
        assert_eq!(
            parse_answers("{\"error\": 1}").unwrap_err(),
            "unparsable answer: no answers"
        );
    }

    #[test]
    fn errors_never_contain_the_key() {
        // Nothing listens on port 1: a connection failure.
        let e = gemini().decide("x", &[]).unwrap_err();
        assert!(!e.contains("secret-key"), "{e}");
        assert!(e.starts_with("request failed") || e == "timeout", "{e}");
        assert_eq!(gemini().scrub("a secret-key b"), "a [key] b");
        assert_eq!(short(&"x".repeat(300)).chars().count(), 200);
        assert_eq!(short("a\n b"), "a b");
    }
}
