//! The `[judge]` table of the rules file (docs/hooks.md section 10): same file and loading
//! rules as the rule levels ([`crate::rules::Levels::load`]). Every key is optional; an
//! invalid value falls back to its default and is returned as a warning.

use crate::rules::Levels;

/// The settings of one question (`[judge.advice]`, `[judge.waiting]`, ...).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct QuestionConfig {
    /// The question is asked at all.
    pub enabled: bool,
    /// A probability at or above it gives a finding (band `record`).
    pub record: f64,
    /// A finding at or above it wakes the agent (band `wake`). 1 means never: the model's
    /// probabilities stay below 1.
    pub wake: f64,
}

impl QuestionConfig {
    const fn new(record: f64, wake: f64) -> QuestionConfig {
        QuestionConfig {
            enabled: true,
            record,
            wake,
        }
    }
}

/// The model judge's settings.
#[derive(Debug, Clone, PartialEq)]
pub struct JudgeConfig {
    /// The judge runs only when this is true and the key variable is set.
    pub enabled: bool,
    pub model: String,
    pub endpoint: String,
    /// Name of the environment variable that holds the API key; the key is never in a file.
    pub api_key_env: String,
    /// Time limit of one model request.
    pub timeout_ms: u64,
    /// Most questions in one turn's request.
    pub max_questions: usize,
    /// How long the judge waits for the turn's `turn.summary` (docs/hooks.md section 3.7).
    pub summary_wait_ms: u64,
    pub advice: QuestionConfig,
    pub waiting: QuestionConfig,
    pub offcard_work: QuestionConfig,
    pub unconfirmed_work: QuestionConfig,
}

pub const DEFAULT_MODEL: &str = "gemini-decision-1";
pub const DEFAULT_ENDPOINT: &str = "https://generativelanguage.googleapis.com/v1beta/decisions";
pub const DEFAULT_API_KEY_ENV: &str = "KB_JUDGE_API_KEY";

impl Default for JudgeConfig {
    fn default() -> Self {
        JudgeConfig {
            enabled: true,
            model: DEFAULT_MODEL.to_string(),
            endpoint: DEFAULT_ENDPOINT.to_string(),
            api_key_env: DEFAULT_API_KEY_ENV.to_string(),
            timeout_ms: 8000,
            max_questions: 40,
            // Short on purpose: after a blocked stop the agent continues, and a judge that
            // still waited could pair its earlier message with the continuation's summary.
            summary_wait_ms: 2000,
            advice: QuestionConfig::new(0.3, 0.7),
            waiting: QuestionConfig::new(0.5, 1.0),
            offcard_work: QuestionConfig::new(0.5, 1.0),
            unconfirmed_work: QuestionConfig::new(0.5, 1.0),
        }
    }
}

impl JudgeConfig {
    /// Loads `[judge]` from the rules file (`$KB_RULES_FILE`, else
    /// `~/.config/kb/rules.toml`). A missing file gives the defaults. Problems fall back to
    /// defaults and are returned as warnings; an unreadable or invalid file is reported by
    /// [`Levels::load`] already, so it gives no warning here.
    pub fn load() -> (JudgeConfig, Vec<String>) {
        let Some(path) = Levels::file_path() else {
            return (JudgeConfig::default(), Vec::new());
        };
        match std::fs::read_to_string(&path) {
            Ok(text) => {
                let (config, mut warnings) = Self::parse(&text);
                for w in &mut warnings {
                    *w = format!("{}: {w}", path.display());
                }
                (config, warnings)
            }
            Err(_) => (JudgeConfig::default(), Vec::new()),
        }
    }

    /// Parses the rules file text. Keys not in the file keep their default; unknown keys
    /// are ignored.
    pub fn parse(text: &str) -> (JudgeConfig, Vec<String>) {
        let mut config = JudgeConfig::default();
        let mut warnings = Vec::new();
        let Ok(value) = toml::from_str::<toml::Table>(text) else {
            return (config, warnings);
        };
        let Some(judge) = value.get("judge") else {
            return (config, warnings);
        };
        let Some(judge) = judge.as_table() else {
            warnings.push("[judge] is not a table; using the default judge settings".to_string());
            return (config, warnings);
        };
        let mut w = |key: &str, v: &toml::Value, expected: &str, default: String| {
            warnings.push(format!(
                "invalid value for judge.{key}: {v} (expected {expected}); using {default}"
            ));
        };
        if let Some(v) = judge.get("enabled") {
            match v.as_bool() {
                Some(b) => config.enabled = b,
                None => w("enabled", v, "true or false", config.enabled.to_string()),
            }
        }
        for (key, slot) in [
            ("model", &mut config.model),
            ("api_key_env", &mut config.api_key_env),
        ] {
            if let Some(v) = judge.get(key) {
                match v.as_str().map(str::trim).filter(|s| !s.is_empty()) {
                    Some(s) => *slot = s.to_string(),
                    None => w(key, v, "a non-empty string", format!("\"{slot}\"")),
                }
            }
        }
        if let Some(v) = judge.get("endpoint") {
            match v
                .as_str()
                .map(str::trim)
                .filter(|s| s.starts_with("https://") || s.starts_with("http://"))
            {
                Some(s) => config.endpoint = s.to_string(),
                None => w(
                    "endpoint",
                    v,
                    "an http:// or https:// URL",
                    format!("\"{}\"", config.endpoint),
                ),
            }
        }
        for (key, slot) in [
            ("timeout_ms", &mut config.timeout_ms),
            ("summary_wait_ms", &mut config.summary_wait_ms),
        ] {
            if let Some(v) = judge.get(key) {
                match v.as_integer().filter(|n| (1..=60_000).contains(n)) {
                    Some(n) => *slot = n as u64,
                    None => w(key, v, "an integer from 1 to 60000", slot.to_string()),
                }
            }
        }
        if let Some(v) = judge.get("max_questions") {
            match v.as_integer().filter(|n| (1..=500).contains(n)) {
                Some(n) => config.max_questions = n as usize,
                None => w(
                    "max_questions",
                    v,
                    "an integer from 1 to 500",
                    config.max_questions.to_string(),
                ),
            }
        }
        for (name, q) in [
            ("advice", &mut config.advice),
            ("waiting", &mut config.waiting),
            ("offcard_work", &mut config.offcard_work),
            ("unconfirmed_work", &mut config.unconfirmed_work),
        ] {
            let Some(v) = judge.get(name) else {
                continue;
            };
            let Some(t) = v.as_table() else {
                warnings.push(format!(
                    "[judge.{name}] is not a table; using its default settings"
                ));
                continue;
            };
            if let Some(v) = t.get("enabled") {
                match v.as_bool() {
                    Some(b) => q.enabled = b,
                    None => warnings.push(format!(
                        "invalid value for judge.{name}.enabled: {v} (expected true or false); \
                         using {}",
                        q.enabled
                    )),
                }
            }
            for (key, slot) in [("record", &mut q.record), ("wake", &mut q.wake)] {
                if let Some(v) = t.get(key) {
                    let p = v.as_float().or_else(|| v.as_integer().map(|n| n as f64));
                    match p.filter(|p| (0.0..=1.0).contains(p)) {
                        Some(p) => *slot = p,
                        None => warnings.push(format!(
                            "invalid value for judge.{name}.{key}: {v} (expected a number from \
                             0 to 1); using {slot}"
                        )),
                    }
                }
            }
        }
        (config, warnings)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_without_a_judge_table() {
        for text in ["", "[rules]\nR1 = \"off\"\n", "not toml ["] {
            let (c, w) = JudgeConfig::parse(text);
            assert_eq!(c, JudgeConfig::default());
            assert!(w.is_empty(), "{w:?}");
        }
        let c = JudgeConfig::default();
        assert!(c.enabled);
        assert_eq!(c.model, "gemini-decision-1");
        assert_eq!(
            c.endpoint,
            "https://generativelanguage.googleapis.com/v1beta/decisions"
        );
        assert_eq!(c.api_key_env, "KB_JUDGE_API_KEY");
        assert_eq!((c.timeout_ms, c.max_questions, c.summary_wait_ms), (8000, 40, 2000));
        assert_eq!(c.advice, QuestionConfig::new(0.3, 0.7));
        assert_eq!(c.waiting, QuestionConfig::new(0.5, 1.0));
        assert_eq!(c.offcard_work, QuestionConfig::new(0.5, 1.0));
        assert_eq!(c.unconfirmed_work, QuestionConfig::new(0.5, 1.0));
    }

    #[test]
    fn overrides() {
        let (c, w) = JudgeConfig::parse(
            r#"
[rules]
R1 = "warn"

[judge]
enabled = false
model = "other-model"
endpoint = "http://127.0.0.1:9/x"
api_key_env = "MY_KEY"
timeout_ms = 3000
max_questions = 12
summary_wait_ms = 800
unknown = 1

[judge.advice]
enabled = false
record = 0.4
wake = 1

[judge.waiting]
wake = 0.7
"#,
        );
        assert!(w.is_empty(), "{w:?}");
        assert!(!c.enabled);
        assert_eq!(c.model, "other-model");
        assert_eq!(c.endpoint, "http://127.0.0.1:9/x");
        assert_eq!(c.api_key_env, "MY_KEY");
        assert_eq!((c.timeout_ms, c.max_questions, c.summary_wait_ms), (3000, 12, 800));
        assert_eq!(
            c.advice,
            QuestionConfig {
                enabled: false,
                record: 0.4,
                wake: 1.0
            }
        );
        assert_eq!(c.waiting, QuestionConfig::new(0.5, 0.7));
        assert_eq!(c.offcard_work, QuestionConfig::new(0.5, 1.0));
    }

    #[test]
    fn invalid_values_fall_back_with_warnings() {
        let (c, w) = JudgeConfig::parse(
            r#"
[judge]
enabled = "yes"
model = ""
endpoint = "ftp://x"
timeout_ms = 0
max_questions = "many"
summary_wait_ms = 999999

[judge.advice]
record = 1.5
wake = "high"
enabled = 1

[judge.waiting]
record = -0.1

[judge.offcard_work]
record = 0.6

[judge.unconfirmed_work]
"#,
        );
        assert_eq!(
            c,
            JudgeConfig {
                offcard_work: QuestionConfig::new(0.6, 1.0),
                ..JudgeConfig::default()
            }
        );
        assert_eq!(w.len(), 10, "{w:#?}");
        assert!(w.contains(
            &"invalid value for judge.enabled: \"yes\" (expected true or false); using true"
                .to_string()
        ));
        assert!(w.contains(
            &"invalid value for judge.advice.record: 1.5 (expected a number from 0 to 1); \
              using 0.3"
                .to_string()
        ));
        assert!(w.iter().any(|x| x.starts_with("invalid value for judge.endpoint")));
        assert!(w.iter().any(|x| x.starts_with("invalid value for judge.timeout_ms: 0")));

        let (c, w) = JudgeConfig::parse("judge = 3\n");
        assert_eq!(c, JudgeConfig::default());
        assert_eq!(w, ["[judge] is not a table; using the default judge settings"]);
        let (_, w) = JudgeConfig::parse("[judge]\nadvice = 0.5\n");
        assert_eq!(w, ["[judge.advice] is not a table; using its default settings"]);
    }
}
