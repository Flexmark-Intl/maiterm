//! The decision log, `logs/decisions.jsonl` (docs/hive.md §7.6): one line per event on a decision
//! (asked, recommended, answered, lapsed…), append-only, every kind a string. Lines of one
//! decision share its `D-…` id; reading them as decisions (the fold, the view) is step 7d's.

use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use super::paths::HivePaths;

pub const LOG_VERSION: u32 = 1;

pub const EVENT_ASKED: &str = "asked";
/// The coordinator's answer under Ask me: recorded, never sent (plan D10).
pub const EVENT_RECOMMENDED: &str = "recommended";
pub const EVENT_ANSWERED: &str = "answered";
/// The prompt closed with no answer through maiTerm.
pub const EVENT_LAPSED: &str = "lapsed";

/// What no checkpoint can hand to the coordinator (§7.4).
pub const CATEGORY_NEVER: &str = "never";

pub const SOURCE_PROMPT: &str = "prompt";
pub const SOURCE_QUESTION: &str = "question";
pub const SOURCE_BLOCKER: &str = "blocker";

pub const BY_COORDINATOR: &str = "coordinator";
pub const BY_HUMAN: &str = "human";
pub const BY_MAITERM: &str = "maiterm";

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct DecisionOption {
    pub label: String,
    pub recommended: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct DecisionLine {
    pub v: u32,
    /// `D-<8hex>`, shared by every line of one decision.
    pub id: String,
    /// `asked` | `recommended` | `answered` | `lapsed` | `overridden` | `offer` | `digest`.
    pub event: String,
    pub hive_id: String,
    /// The checkpoint it fell under, `never`, or later steps' own (`config`, `other`, `offer`…).
    pub category: String,
    /// `prompt` | `question` | `blocker` | `land` | `resolve` | `plan` | `offer`.
    pub source: String,
    pub task_id: Option<String>,
    /// The worker's slug.
    pub worker: Option<String>,
    /// The prompt id (`p_…`, `q_…`) or blocker key (`b_<task>_<asked_at>`) it answers.
    pub key: Option<String>,
    /// maiTerm's words for what was asked, worker text flattened and escaped.
    pub question: String,
    pub context: Option<String>,
    pub options: Vec<DecisionOption>,
    pub recommended: Option<String>,
    pub answer: Option<String>,
    pub reason: Option<String>,
    /// `coordinator` | `human` | `maiterm`.
    pub by: String,
    /// Answered after the coordinator's time ran out (§4.7).
    pub after_timeout: bool,
    /// The precedent the coordinator cited, as given (checked from step 7d).
    pub precedent: Option<String>,
    pub outcome: Option<String>,
    pub at: String,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

impl Default for DecisionLine {
    fn default() -> Self {
        Self {
            v: LOG_VERSION,
            id: String::new(),
            event: EVENT_ASKED.to_string(),
            hive_id: String::new(),
            category: String::new(),
            source: String::new(),
            task_id: None,
            worker: None,
            key: None,
            question: String::new(),
            context: None,
            options: Vec::new(),
            recommended: None,
            answer: None,
            reason: None,
            by: BY_MAITERM.to_string(),
            after_timeout: false,
            precedent: None,
            outcome: None,
            at: String::new(),
            extra: Map::new(),
        }
    }
}

pub fn new_id() -> String {
    format!("D-{}", &uuid::Uuid::new_v4().simple().to_string()[..8])
}

/// One writer at a time in this process; each line one `O_APPEND` write.
static WRITE: Mutex<()> = Mutex::new(());

pub fn append(paths: &HivePaths, l: &DecisionLine) -> Result<(), String> {
    let _held = WRITE.lock().unwrap_or_else(|e| e.into_inner());
    let v = serde_json::to_value(l).map_err(|e| e.to_string())?;
    super::queue::append_line(&paths.decision_log(), &v)
}

/// `append`, logged rather than returned: a decision that went through is never undone because
/// its line didn't write.
pub fn record(paths: &HivePaths, l: &DecisionLine) {
    if let Err(e) = append(paths, l) {
        log::warn!("hive {}: decision {} ({}) not logged: {e}", l.hive_id, l.id, l.event);
    }
}

/// Every line that parses, oldest first.
pub fn read(paths: &HivePaths) -> Vec<DecisionLine> {
    let Ok(text) = std::fs::read_to_string(paths.decision_log()) else { return Vec::new() };
    text.lines().filter_map(|l| serde_json::from_str::<DecisionLine>(l.trim_end_matches('\r')).ok()).collect()
}

/// The latest line logged under a prompt id or blocker key.
pub fn by_key<'a>(lines: &'a [DecisionLine], key: &str) -> Option<&'a DecisionLine> {
    lines.iter().rev().find(|l| l.key.as_deref() == Some(key))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lines_append_and_read_back_with_a_later_builds_fields() {
        let base = std::env::temp_dir().join(format!("hive-decisions-{}", uuid::Uuid::new_v4()));
        let paths = HivePaths::new(&base);
        let id = new_id();
        assert!(id.starts_with("D-") && id.len() == 10, "{id}");
        let asked = DecisionLine { id: id.clone(), key: Some("p_t_1".into()), question: "Bash: `ls`".into(), at: "now".into(), ..DecisionLine::default() };
        append(&paths, &asked).unwrap();
        let answered = DecisionLine { event: EVENT_ANSWERED.into(), answer: Some("Yes".into()), by: BY_COORDINATOR.into(), ..asked.clone() };
        append(&paths, &answered).unwrap();
        let text = std::fs::read_to_string(paths.decision_log()).unwrap();
        // A CRLF line, a broken one and a later build's field.
        std::fs::write(paths.decision_log(), format!("{text}not json\r\n{}\r\n", r#"{"v":1,"id":"D-x","event":"offer","key":"b_t_2","later":7}"#)).unwrap();
        let all = read(&paths);
        assert_eq!(all.len(), 3, "a line that doesn't parse is skipped");
        assert_eq!((all[0].clone(), all[1].clone()), (asked, answered.clone()));
        assert_eq!(all[2].extra.get("later"), Some(&serde_json::json!(7)));
        assert_eq!(by_key(&all, "p_t_1"), Some(&answered), "the latest line of the key");
        assert!(by_key(&all, "nope").is_none());
        let first: Value = serde_json::from_str(text.lines().next().unwrap()).unwrap();
        for k in ["task_id", "recommended", "answer", "precedent", "after_timeout"] {
            assert!(first.get(k).is_some(), "{k} is written even when empty");
        }
        let _ = std::fs::remove_dir_all(&base);
    }
}
