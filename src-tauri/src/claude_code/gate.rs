//! Which agent a Claude permission prompt is holding up, and when it has stopped holding it up.
//!
//! Claude's `permission_prompt` Notification says a human is being asked, and says nothing about
//! WHO is asking. The hook-input builder in Claude Code (verified against 2.1.281) takes
//! `agent_id` from the tool-use context, and the Notification is built without one — so it never
//! carries an `agent_id`, even when the gated call belongs to a subagent. `PreToolUse`,
//! `PermissionRequest`, `PostToolUse`, `PostToolUseFailure` and `SubagentStop` DO carry it, and
//! every subagent hook carries the PARENT's `session_id`, so they all land on one session row.
//!
//! That used to bury prompts. The session left `WaitingPermission` on any `PreToolUse`, so a
//! background subagent that kept working while its parent waited on a decision flipped the tab
//! back to active within a second: the prompt vanished from `/chats`, the WS and the doorbell,
//! and was visible only in the terminal. Newer Claude Code asks for permission more often,
//! which made it common.
//!
//! Four facts about Claude Code's side, all read out of the 2.1.281 binary, shape everything
//! below:
//! - **`PermissionRequest` fires when a call actually asks**, before the dialog shows, and names
//!   the agent, the tool and its input. It carries no `tool_use_id`.
//! - **The Notification fires 6 SECONDS after the dialog opens**, from a cancellable timer, and
//!   not at all if the dialog closes sooner. By then other agents have usually started and
//!   finished calls of their own, so "what was in flight when the Notification came" is a poor
//!   answer to "which call is asking" on its own.
//! - **A denied call fires no hook at all.** Its agent moving on, or stopping, is the only
//!   evidence that it is over.
//! - **`idle_prompt` is never sent while a dialog is on screen**, so it closes any prompt.
//!
//! The rule, built on evidence rather than timing:
//!
//! - **The candidates are the calls that ASKED** (`PermissionRequest` matched to an in-flight
//!   call) and have not ended. When none asked — a `PermissionRequest` hook this Claude or this
//!   host doesn't have yet — every call in flight is a candidate, which is looser but still
//!   contains the gated call.
//! - **A candidate leaves when it provably isn't waiting**: its own `PostToolUse` or
//!   `PostToolUseFailure` arrives (it ran), or its agent starts another call (an agent blocked on
//!   a dialog issues nothing new), or its agent stops.
//! - **The prompt holds until no candidate is left.** A busy subagent drains only its own
//!   entries, so it can never clear a gate its parent holds.
//!
//! When the prompt opens with nothing in flight there is no evidence to attribute it with, and
//! the old rule applies unchanged (see [`GateLedger::held`]).
//!
//! Known limit: an agent's sibling calls run concurrently, so a sibling that STARTS after the
//! gated call asked would read as its agent moving on. That is the old behaviour, narrowed to
//! one agent's own parallel batch.

/// The agent that made a call. `""` is the main thread — a hook with no `agent_id`.
pub fn agent_key(event: &serde_json::Value) -> String {
    event
        .get("agent_id")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string()
}

/// Tools that never wait on a permission prompt of their own, so they must never be candidates.
/// A foreground `Agent` call stays in flight for the whole subagent run, and as a candidate it
/// would hold a prompt the subagent raised long after that subagent's approval.
fn never_gated(tool_name: &str) -> bool {
    matches!(tool_name, "Agent" | "Task")
}

/// Bounds the in-flight list. A call whose end we never see (a hook that failed to post) would
/// otherwise stay forever; the oldest go first.
const MAX_IN_FLIGHT: usize = 64;

#[derive(Clone, Debug, PartialEq)]
pub struct InFlightCall {
    pub tool_use_id: String,
    pub agent: String,
    pub tool_name: String,
    pub detail: Option<String>,
    /// Digest of `tool_input`, the only thing that can pair a `PermissionRequest` (which has no
    /// `tool_use_id`) with the call that raised it.
    pub fingerprint: String,
    /// A `PermissionRequest` was matched to this call: it asked.
    pub asked: bool,
}

#[derive(Clone, Debug, Default)]
pub struct GateLedger {
    in_flight: Vec<InFlightCall>,
    /// `Some` while a permission prompt is open AND could be attributed: the calls it may be
    /// holding. Emptying it is what releases the prompt.
    candidates: Option<Vec<InFlightCall>>,
}

/// What an event did to an open prompt.
#[derive(Debug, PartialEq)]
pub enum GateChange {
    /// No attributed prompt was open, or one is and it still holds.
    Unchanged,
    /// This event removed the last candidate: the prompt is over.
    Released,
}

/// Who a newly opened prompt belongs to, as far as the evidence goes.
#[derive(Debug, PartialEq)]
pub enum Attribution<'a> {
    /// Nothing was in flight. The prompt is not held by the ledger; callers keep their old rule.
    Unknown,
    /// Exactly one call could be asking. The card can describe it.
    One(&'a InFlightCall),
    /// Several could. The prompt holds, but naming any one of them on the card could put the
    /// wrong command next to an Approve button.
    Several,
}

impl GateLedger {
    /// An attributed prompt is open and at least one candidate may still be the gated call.
    /// `false` both when no prompt is open and when one opened with no evidence — callers keep
    /// their previous rule for the latter.
    pub fn held(&self) -> bool {
        self.candidates.as_ref().is_some_and(|c| !c.is_empty())
    }

    /// Whether a call to `tool_name` is in flight — used to keep an open AskUserQuestion alive
    /// while another agent's calls come and go.
    pub fn has_in_flight(&self, tool_name: &str) -> bool {
        self.in_flight.iter().any(|c| c.tool_name == tool_name)
    }

    /// `PreToolUse`. The agent is not blocked, so nothing it has in flight can be the gated call.
    pub fn call_started(&mut self, call: InFlightCall) -> GateChange {
        let change = self.drop_candidates(|c| c.agent == call.agent);
        if !never_gated(&call.tool_name) && !call.tool_use_id.is_empty() {
            self.in_flight.retain(|c| c.tool_use_id != call.tool_use_id);
            self.in_flight.push(call);
            let overflow = self.in_flight.len().saturating_sub(MAX_IN_FLIGHT);
            self.in_flight.drain(..overflow);
        }
        change
    }

    /// `PermissionRequest`: one of this agent's calls is asking. Paired by input digest first,
    /// then by the agent's newest not-yet-asking call to the same tool — a `PreToolUse` hook may
    /// rewrite the input the request later carries.
    pub fn call_asked(&mut self, agent: &str, tool_name: &str, fingerprint: &str) {
        let mine = |c: &InFlightCall| c.agent == agent && c.tool_name == tool_name && !c.asked;
        let pos = self
            .in_flight
            .iter()
            .rposition(|c| mine(c) && c.fingerprint == fingerprint)
            .or_else(|| self.in_flight.iter().rposition(mine));
        if let Some(pos) = pos {
            self.in_flight[pos].asked = true;
        }
    }

    /// `PostToolUse` or `PostToolUseFailure`: the call ran, so it is not waiting on anyone.
    pub fn call_ended(&mut self, tool_use_id: &str) -> GateChange {
        if tool_use_id.is_empty() {
            return GateChange::Unchanged;
        }
        self.in_flight.retain(|c| c.tool_use_id != tool_use_id);
        self.drop_candidates(|c| c.tool_use_id == tool_use_id)
    }

    /// `SubagentStop`, or `Stop` for the main thread (`""`): the agent has finished, so none of
    /// its calls is waiting.
    pub fn agent_ended(&mut self, agent: &str) -> GateChange {
        self.in_flight.retain(|c| c.agent != agent);
        self.drop_candidates(|c| c.agent == agent)
    }

    /// A permission prompt opened. The calls that asked are the candidates; failing any, every
    /// call in flight is.
    pub fn prompt_opened(&mut self) -> Attribution<'_> {
        let asked: Vec<InFlightCall> = self.in_flight.iter().filter(|c| c.asked).cloned().collect();
        let candidates = if asked.is_empty() { self.in_flight.clone() } else { asked };
        if candidates.is_empty() {
            self.candidates = None;
            return Attribution::Unknown;
        }
        self.candidates = Some(candidates);
        match self.candidates.as_deref() {
            Some([only]) => Attribution::One(only),
            _ => Attribution::Several,
        }
    }

    /// The prompt is over by other means: a new prompt from the human, `idle_prompt`, a new
    /// session.
    pub fn prompt_closed(&mut self) {
        self.candidates = None;
    }

    fn drop_candidates(&mut self, gone: impl Fn(&InFlightCall) -> bool) -> GateChange {
        let Some(c) = self.candidates.as_mut() else {
            return GateChange::Unchanged;
        };
        let before = c.len();
        c.retain(|x| !gone(x));
        if before > 0 && c.is_empty() {
            self.candidates = None;
            GateChange::Released
        } else {
            GateChange::Unchanged
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn call(id: &str, agent: &str, tool: &str) -> InFlightCall {
        InFlightCall {
            tool_use_id: id.into(),
            agent: agent.into(),
            tool_name: tool.into(),
            detail: Some(format!("{tool} {id}")),
            fingerprint: format!("fp-{id}"),
            asked: false,
        }
    }

    fn one(a: Attribution<'_>) -> Option<String> {
        match a {
            Attribution::One(c) => Some(c.tool_use_id.clone()),
            _ => None,
        }
    }

    /// The reported bug: the parent waits on a decision while a background subagent keeps
    /// working. The subagent's calls must never release the parent's prompt.
    #[test]
    fn a_busy_subagent_cannot_release_its_parents_prompt() {
        let mut g = GateLedger::default();
        g.call_started(call("sub-1", "agent-a", "Read"));
        g.call_started(call("main-1", "", "Bash"));
        g.call_asked("", "Bash", "fp-main-1");
        assert_eq!(one(g.prompt_opened()).as_deref(), Some("main-1"));

        assert_eq!(g.call_ended("sub-1"), GateChange::Unchanged);
        assert_eq!(g.call_started(call("sub-2", "agent-a", "Grep")), GateChange::Unchanged);
        assert_eq!(g.call_ended("sub-2"), GateChange::Unchanged);
        assert!(g.held(), "only the parent's own call can end its prompt");

        assert_eq!(g.call_ended("main-1"), GateChange::Released);
        assert!(!g.held());
    }

    /// Review finding 2: a long call that was merely running when the prompt opened used to be a
    /// candidate too, and held the prompt for minutes after the human approved. With the
    /// request as evidence, it isn't one.
    #[test]
    fn a_long_unrelated_call_does_not_hold_an_approved_prompt() {
        let mut g = GateLedger::default();
        g.call_started(call("build", "agent-a", "Bash"));
        g.call_started(call("main-1", "", "Bash"));
        g.call_asked("", "Bash", "fp-main-1");
        assert_eq!(one(g.prompt_opened()).as_deref(), Some("main-1"));
        assert_eq!(g.call_ended("main-1"), GateChange::Released);
    }

    /// Review finding 3: two candidates the evidence can't separate. The prompt holds, and the
    /// caller is told not to name either on the card.
    #[test]
    fn indistinguishable_candidates_are_reported_as_several() {
        let mut g = GateLedger::default();
        g.call_started(call("main-1", "", "Bash"));
        g.call_started(call("sub-1", "agent-a", "Bash"));
        assert_eq!(g.prompt_opened(), Attribution::Several);
        assert!(g.held());
    }

    /// The request carries the input as the call will run it; a PreToolUse hook may have
    /// rewritten it since. Pairing then falls back to the agent's newest call to that tool.
    #[test]
    fn a_request_whose_input_changed_still_pairs_with_its_call() {
        let mut g = GateLedger::default();
        g.call_started(call("main-1", "", "Bash"));
        g.call_asked("", "Bash", "rewritten");
        assert_eq!(one(g.prompt_opened()).as_deref(), Some("main-1"));
    }

    /// A request names its agent, so it never marks another agent's identical call.
    #[test]
    fn a_request_never_pairs_with_another_agents_call() {
        let mut g = GateLedger::default();
        let mut sub = call("sub-1", "agent-a", "Bash");
        sub.fingerprint = "same".into();
        let mut main = call("main-1", "", "Bash");
        main.fingerprint = "same".into();
        g.call_started(sub);
        g.call_started(main);
        g.call_asked("agent-a", "Bash", "same");
        assert_eq!(one(g.prompt_opened()).as_deref(), Some("sub-1"));
    }

    /// The mirror case: the SUBAGENT is gated and the parent is idle. The subagent's own calls
    /// are the only evidence there will ever be, so they must be able to release it. This is
    /// why "ignore every subagent hook" was not the fix.
    #[test]
    fn a_gated_subagent_releases_its_own_prompt() {
        let mut g = GateLedger::default();
        g.call_started(call("sub-1", "agent-a", "Bash"));
        g.call_asked("agent-a", "Bash", "fp-sub-1");
        assert_eq!(one(g.prompt_opened()).as_deref(), Some("sub-1"));
        assert_eq!(g.call_ended("sub-1"), GateChange::Released);
    }

    /// A denied call never runs, so no PostToolUse — the agent moving on is the evidence.
    #[test]
    fn a_denied_call_is_released_when_its_agent_starts_another() {
        let mut g = GateLedger::default();
        g.call_started(call("main-1", "", "Bash"));
        g.prompt_opened();
        assert_eq!(g.call_started(call("main-2", "", "Read")), GateChange::Released);
    }

    /// A subagent that is denied and then simply finishes emits no further call; its stop is
    /// the only thing that can say its prompt is gone.
    #[test]
    fn a_subagent_stopping_releases_a_prompt_only_it_could_hold() {
        let mut g = GateLedger::default();
        g.call_started(call("sub-1", "agent-a", "Bash"));
        g.call_started(call("sub-2", "agent-b", "Bash"));
        g.prompt_opened();
        assert_eq!(g.agent_ended("agent-a"), GateChange::Unchanged);
        assert_eq!(g.agent_ended("agent-b"), GateChange::Released);
    }

    /// A denied request stays marked on a call that never ends. Once its agent moves on it is
    /// gone from the ledger, so it cannot become a candidate for a LATER prompt.
    #[test]
    fn a_denied_request_does_not_haunt_the_next_prompt() {
        let mut g = GateLedger::default();
        g.call_started(call("main-1", "", "Bash"));
        g.call_asked("", "Bash", "fp-main-1");
        g.prompt_opened();
        g.prompt_closed(); // idle_prompt after the denial
        g.agent_ended(""); // …which also ends the main thread's calls
        g.call_started(call("sub-1", "agent-a", "Bash"));
        g.call_asked("agent-a", "Bash", "fp-sub-1");
        assert_eq!(one(g.prompt_opened()).as_deref(), Some("sub-1"));
    }

    /// A foreground Agent call is in flight for the whole subagent run. As a candidate it would
    /// hold the subagent's prompt until the subagent finished, long after the human approved.
    #[test]
    fn a_foreground_agent_call_is_never_a_candidate() {
        let mut g = GateLedger::default();
        g.call_started(call("main-agent", "", "Agent"));
        g.call_started(call("sub-1", "agent-a", "Bash"));
        assert_eq!(one(g.prompt_opened()).as_deref(), Some("sub-1"));
        assert_eq!(g.call_ended("sub-1"), GateChange::Released);
    }

    /// Nothing in flight means nothing to attribute with. `held()` stays false so the caller
    /// falls back to its old rule rather than holding a prompt nothing can ever release.
    #[test]
    fn a_prompt_with_nothing_in_flight_is_not_held() {
        let mut g = GateLedger::default();
        assert_eq!(g.prompt_opened(), Attribution::Unknown);
        assert!(!g.held());
        assert_eq!(g.call_started(call("x", "", "Bash")), GateChange::Unchanged);
    }

    /// A call that starts AFTER the prompt opened cannot be the gated one, and ending it must
    /// not count toward the release.
    #[test]
    fn a_call_started_after_the_prompt_is_not_a_candidate() {
        let mut g = GateLedger::default();
        g.call_started(call("main-1", "", "Bash"));
        g.prompt_opened();
        g.call_started(call("sub-1", "agent-a", "Read"));
        assert_eq!(g.call_ended("sub-1"), GateChange::Unchanged);
        assert!(g.held());
    }

    #[test]
    fn the_in_flight_list_is_bounded() {
        let mut g = GateLedger::default();
        for i in 0..(MAX_IN_FLIGHT + 10) {
            g.call_started(call(&format!("c{i}"), "agent-a", "Read"));
        }
        assert_eq!(g.in_flight.len(), MAX_IN_FLIGHT);
        assert_eq!(g.in_flight[0].tool_use_id, "c10");
    }
}
