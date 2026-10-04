//! Agent runtimes' per-project state (docs/relocate.md §3).

use std::path::Path;

use serde::Serialize;

#[derive(Debug, Default, Clone, Serialize)]
pub struct AgentPlan {}

#[derive(Debug, Default, Clone, Serialize)]
pub struct AgentReport {}

pub fn plan(_old: &Path, _new: &Path) -> AgentPlan {
    AgentPlan::default()
}

pub fn apply(_old: &Path, _new: &Path) -> AgentReport {
    AgentReport::default()
}
