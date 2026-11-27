//! The queen's progress estimate (docs/hive.md §4.1.1): `hiveSetProgress` puts a percentage and a
//! one-line note on the Hive record, and the Hive header shows them as hers, with when she set
//! them. It is a claim, never a measure: Done (`hiveFinish`) is the only thing that says complete,
//! and the header shows Done in its place once the Hive is done.
//!
//! A running queen was briefed before this tool existed, so the brief alone doesn't make the bar
//! move: the results of the milestone tools she already calls (a plan, a land) carry a one-line
//! reminder while her estimate is missing or old (`reminder`).

use serde_json::{json, Value};

use super::finish::write_status;
use super::fuses::STATUS_PAUSED;
use super::record::{Hive, HiveProgress, STATUS_ACTIVE, STATUS_DONE};
use super::rpc::HiveWindow;
use super::tools::{Caller, Ctx};

pub const TOOL: &str = "hiveSetProgress";
/// The note's length cap, in characters: one line in the header.
pub const MAX_NOTE: usize = 200;
/// How old an estimate may get before a milestone tool's result reminds her (milliseconds).
pub const STALE_AFTER_MS: i64 = 20 * 60 * 1000;

pub fn tool_definition(tab_id: &Value) -> Value {
    json!({
        "name": TOOL,
        "description": format!(
            "Set your estimate of how far this Hive's current goal has come, so your human can tell where the Hive is \
             without reading your chat. maiTerm shows it in the Hive header as a bar with your note and the time you set \
             it, labelled as your estimate. Call it once you have a plan, then whenever it moves: a wave finishes, a big \
             land, a setback (it may go down: say why in the note). Weigh it by the work, not by worker count: what's \
             landed and proven against the completion criteria, not what's started. The note is one line, at most \
             {MAX_NOTE} characters, that stands on its own: what's done and what's next (\"Auth and checkout landed; \
             payments in review, then the admin pages\"). 100 means you're about to call hiveFinish; only Done says \
             complete. Refused once the Hive is done."
        ),
        "inputSchema": {
            "type": "object",
            "properties": {
                "tabId": tab_id,
                "percent": { "type": "integer", "minimum": 0, "maximum": 100, "description": "Your estimate, 0 to 100" },
                "note": { "type": "string", "description": "One line: what's done and what's next" }
            },
            "required": ["percent", "note"]
        }
    })
}

/// The percentage and note, checked: a whole number 0 to 100, and one non-empty line of at most
/// `MAX_NOTE` characters.
pub fn parse(args: &Value) -> Result<(u8, String), String> {
    let p = args.get("percent").ok_or("percent is required: your estimate, 0 to 100.")?;
    let percent = p
        .as_u64()
        .or_else(|| p.as_f64().filter(|f| f.fract() == 0.0 && *f >= 0.0).map(|f| f as u64))
        .filter(|n| *n <= 100)
        .ok_or("percent is a whole number from 0 to 100.")?;
    let raw = args.get("note").and_then(Value::as_str).ok_or("note is required: one line on what's done and what's next.")?;
    if raw.contains(['\n', '\r', '\u{2028}', '\u{2029}', '\u{85}']) {
        return Err("The note spans lines: write it as one line.".to_string());
    }
    let note = raw.trim();
    if note.is_empty() {
        return Err("note is required: one line on what's done and what's next.".to_string());
    }
    if note.chars().count() > MAX_NOTE {
        return Err(format!("The note is {} characters: keep it to {MAX_NOTE}.", note.chars().count()));
    }
    Ok((percent as u8, note.to_string()))
}

/// `hiveSetProgress`: the estimate on the record, announced to the window.
pub(crate) async fn set_tool<W: HiveWindow>(ctx: &Ctx<'_, W>, caller: &Caller, args: &Value) -> Result<Value, String> {
    let (percent, note) = parse(args)?;
    let current = super::autonomy::current_hive(ctx, caller).ok_or("This Hive is gone.")?;
    if current.status == STATUS_DONE {
        return Err(format!("This Hive is done: the header shows Done now. {}", super::tools::DONE_WAKE_HINT));
    }
    let progress = HiveProgress { percent, note, at: super::fuses::now_ms() };
    let set = progress.clone();
    write_status(ctx, caller, &[STATUS_ACTIVE, STATUS_PAUSED], None, move |h| h.progress = Some(set))?;
    log::info!("hive {}: progress set to {percent}%", caller.hive.id);
    let mut out = json!({ "percent": percent, "note": progress.note });
    if percent == 100 {
        out["next"] = json!("At 100, call hiveFinish: the header says complete only once the Hive is Done.");
    }
    Ok(out)
}

/// The line a milestone tool's result carries while the queen's estimate is missing or older than
/// `STALE_AFTER_MS`. None once the Hive is done.
pub fn reminder(hive: &Hive, now_ms: i64) -> Option<String> {
    if hive.status == STATUS_DONE {
        return None;
    }
    match &hive.progress {
        None => Some(format!("Your human can't see how far this Hive has come: set your estimate with {TOOL}.")),
        Some(p) if now_ms - p.at >= STALE_AFTER_MS => Some(format!(
            "Your progress estimate ({}%) is {} minutes old: update it with {TOOL} if this moved it.",
            p.percent,
            (now_ms - p.at) / 60_000
        )),
        Some(_) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_progress_is_a_whole_percentage_and_one_line() {
        assert_eq!(parse(&json!({ "percent": 40, "note": "  Auth landed; payments next " })).unwrap(), (40, "Auth landed; payments next".to_string()));
        assert_eq!(parse(&json!({ "percent": 100.0, "note": "x" })).unwrap().0, 100);
        for bad in [json!(101), json!(-1), json!(12.5), json!("40"), Value::Null] {
            assert!(parse(&json!({ "percent": bad, "note": "x" })).is_err(), "{bad}");
        }
        assert!(parse(&json!({ "note": "x" })).is_err(), "percent is required");
        assert!(parse(&json!({ "percent": 1 })).is_err(), "note is required");
        assert!(parse(&json!({ "percent": 1, "note": "   " })).is_err(), "blank note");
        assert!(parse(&json!({ "percent": 1, "note": "a\nb" })).is_err(), "two lines");
        assert!(parse(&json!({ "percent": 1, "note": "é".repeat(MAX_NOTE) })).is_ok(), "the cap counts characters");
        assert!(parse(&json!({ "percent": 1, "note": "é".repeat(MAX_NOTE + 1) })).is_err());
    }

    #[test]
    fn a_milestone_reminds_only_while_the_estimate_is_missing_or_old() {
        let mut hive = Hive::default();
        assert!(reminder(&hive, 0).unwrap().contains(TOOL), "never set");
        hive.progress = Some(HiveProgress { percent: 30, note: "n".into(), at: 1_000 });
        assert_eq!(reminder(&hive, 1_000 + STALE_AFTER_MS - 1), None, "fresh");
        let old = reminder(&hive, 1_000 + STALE_AFTER_MS).unwrap();
        assert!(old.contains("30%") && old.contains("20 minutes"), "{old}");
        hive.status = STATUS_DONE.to_string();
        assert_eq!(reminder(&hive, i64::MAX), None, "a Done Hive is never nagged");
    }
}
