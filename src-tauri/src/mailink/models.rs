//! What models this machine can switch a Claude tab to.
//!
//! maiLink's model picker used to be a hardcoded list in the phone app, which went stale the day
//! Fable 5.1 shipped — the app offered no way to select a model the account already had. Moving
//! the list here does not make it self-maintaining, but it moves the stale part to the side that
//! redeploys far more often, and it lets the one genuinely live source do the work.
//!
//! **Two sources, and they are not equally trustworthy — the wire says which is which.**
//!
//! `Source::Account` comes from `~/.claude.json` → `additionalModelOptionsCache`, which Claude
//! Code populates from the server for THIS account. Fable arrived there with its exact label,
//! description and `[1m]` value with no code change anywhere, which is the whole argument for
//! reading it. An entry here is one the account was actually told about.
//!
//! `Source::Builtin` is the curated base table below. These are the tiers every account is
//! expected to have, but nothing here verifies that — `modelAccessCache` is the entitlement hook
//! and it is empty on the machine this was written against, so there is nothing to check against.
//! A builtin entry is "generally available", not "confirmed for you". Saying so on the wire is
//! cheaper than a picker that offers a model the switch will refuse.

use serde::Serialize;

/// Where an entry came from, and therefore how much it can be trusted.
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Source {
    /// From this account's server-pushed model cache. Known to be offered to this account.
    Account,
    /// From maiTerm's curated table. Expected, not verified.
    Builtin,
}

#[derive(Clone, Debug, Serialize)]
pub struct ModelOption {
    /// Exactly what goes after `/model`. Aliases where we have one, so a point release inside a
    /// tier follows on its own; the account cache supplies pinned ids and we pass those through
    /// unchanged rather than inventing an alias we cannot verify (see `from_account_cache`).
    pub value: String,
    /// Short display name.
    pub name: String,
    /// One line of what it is for.
    pub note: String,
    pub source: Source,
}

/// The tiers every Claude Code install is expected to offer, as `/model` aliases.
///
/// Aliases, never pinned ids: `opus` keeps meaning the current Opus after a point release, so this
/// table ages only in its labels. Deliberately short — anything genuinely new arrives through the
/// account cache, which is the half that actually moves.
///
/// **`sonnet` and `sonnet[1m]` may be one model, and the pair stays anyway.** Strings in the CLI
/// binary suggest `claude-sonnet-5` is natively 1M, which would make these two rows for one thing.
/// I could not confirm it with anything better: no Sonnet session on this machine has ever passed
/// 200k tokens (77k is the high-water mark), so the transcripts cannot settle it either way. Under
/// that uncertainty, keeping both is the cheap error — a redundant row — while collapsing them
/// removes a capability if the suffix does mean something on some account. The visible symptom is
/// that only the non-1M row highlights, since `ASSUMED_1M_MODELS` has no `sonnet` entry and must
/// not gain one on this evidence: guessing 1M for an account that has 200k overstates the window,
/// and that direction has no backstop. Settle it with a Sonnet session that exceeds 200k, not with
/// another read of the binary.
const BUILTIN: [(&str, &str, &str); 5] = [
    ("opus", "Opus", "Everyday complex work"),
    ("opus[1m]", "Opus", "Everyday complex work · 1M context"),
    ("sonnet", "Sonnet", "Balanced speed and capability"),
    ("sonnet[1m]", "Sonnet", "Balanced · 1M context"),
    ("haiku", "Haiku", "Fastest, for simple work"),
];

/// Read `~/.claude.json` and return its `additionalModelOptionsCache` entries.
/// Values are passed through EXACTLY as the cache spells them, pinned release and all
/// (`claude-fable-5-1[1m]`). Rewriting that to a `fable[1m]` alias would be nicer to read and a
/// guess: nothing here knows that `fable` is a valid `/model` alias on this account, and a wrong
/// value fails at the TUI where the human sees it rather than here where we could have not said
/// it. The label ages instead, which is the recoverable direction.
fn from_account_cache() -> Vec<ModelOption> {
    let Some(home) = dirs::home_dir() else { return Vec::new() };
    let Ok(raw) = std::fs::read(home.join(".claude.json")) else { return Vec::new() };
    let Ok(doc) = serde_json::from_slice::<serde_json::Value>(&raw) else { return Vec::new() };
    entries_from(&doc)
}

/// Strip control characters and surrounding whitespace from a server-pushed string.
///
/// These come from a cache maiTerm does not write, and go to a separate codebase that renders
/// them. Interior control characters survive a plain `trim()`, and a newline is not cosmetic in a
/// field documented as "exactly what goes after `/model`": the later feature that types it would
/// submit the first line and hand the rest to the agent as a second input, chosen by a human who
/// only ever saw the friendly name.
fn clean(s: &str) -> String {
    s.chars().filter(|c| !c.is_control()).collect::<String>().trim().to_string()
}

/// Project a parsed `~/.claude.json` into account model options. Pure, so it can be driven with a
/// hostile document instead of whatever happens to be in the developer's home directory.
fn entries_from(doc: &serde_json::Value) -> Vec<ModelOption> {
    let Some(entries) = doc.get("additionalModelOptionsCache").and_then(|v| v.as_array()) else {
        return Vec::new();
    };
    entries
        .iter()
        .filter_map(|e| {
            // A value we had to alter is a value we no longer know is correct, so drop the row
            // rather than offer a repaired id that may switch to something else. Absent is
            // visible; subtly wrong is not.
            let raw = e.get("value").and_then(|v| v.as_str())?;
            let value = clean(raw);
            if value.is_empty() || value != raw.trim() {
                return None;
            }
            // Display fields are repaired rather than dropped — losing a whole model over a stray
            // character in its description would be the worse trade. An empty or blank label
            // falls back to the value, so a row can never render nameless.
            let name = e
                .get("label")
                .and_then(|v| v.as_str())
                .map(clean)
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| value.clone());
            let note = e.get("description").and_then(|v| v.as_str()).map(clean).unwrap_or_default();
            Some(ModelOption { value, name, note, source: Source::Account })
        })
        .collect()
}

/// Account entries first — the ones we actually know about — then the curated tiers, minus any
/// value already claimed. Pure, and deduping by a set covers account-vs-account duplicates too,
/// which a scan against `BUILTIN` alone never saw.
fn merge(account: Vec<ModelOption>) -> Vec<ModelOption> {
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut out = Vec::with_capacity(account.len() + BUILTIN.len());
    for m in account {
        if seen.insert(m.value.clone()) {
            out.push(m);
        }
    }
    for (value, name, note) in BUILTIN {
        if seen.insert(value.to_string()) {
            out.push(ModelOption {
                value: value.to_string(),
                name: name.to_string(),
                note: note.to_string(),
                source: Source::Builtin,
            });
        }
    }
    out
}

/// Everything this machine can switch a Claude tab to.
pub fn available() -> Vec<ModelOption> {
    merge(from_account_cache())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_curated_tiers_are_aliases_so_a_point_release_needs_no_edit() {
        // A pinned id here would make this table wrong on every Claude release. `opus` keeps
        // meaning the current Opus; only the human-readable label ages.
        for (value, _, _) in BUILTIN {
            // The tier name is everything before the `[1m]` variant marker, which is a window
            // size and not a version — `opus[1m]` is still an alias.
            let tier = value.split('[').next().unwrap_or(value);
            assert!(
                !tier.contains(char::is_numeric) && !tier.contains("claude-"),
                "{value} pins a version — the curated table must hold aliases only"
            );
        }
    }

    #[test]
    fn the_account_cache_outranks_the_curated_guess() {
        // Drives the REAL merge, not a copy of it. The previous version re-implemented the loop
        // inside the test, so deleting the dedup from `merge` left it passing while the endpoint
        // emitted duplicate rows.
        let out = merge(vec![ModelOption {
            value: "opus[1m]".into(),
            name: "Opus 5".into(),
            note: "from the server".into(),
            source: Source::Account,
        }]);
        let opus: Vec<&ModelOption> = out.iter().filter(|m| m.value == "opus[1m]").collect();
        assert_eq!(opus.len(), 1, "no duplicate rows for one value");
        assert_eq!(opus[0].source, Source::Account, "the account's copy wins");
        assert_eq!(opus[0].note, "from the server");
        assert_eq!(out.len(), BUILTIN.len(), "and the other tiers still come through");
    }

    #[test]
    fn two_cache_entries_claiming_one_value_yield_one_row() {
        // The old scan only asked whether an account entry collided with BUILTIN, so
        // account-vs-account duplicates passed straight through as two rows doing the same thing.
        let mk = |note: &str| ModelOption {
            value: "dup".into(),
            name: "Dup".into(),
            note: note.into(),
            source: Source::Account,
        };
        let out = merge(vec![mk("first"), mk("second")]);
        let dups: Vec<&ModelOption> = out.iter().filter(|m| m.value == "dup").collect();
        assert_eq!(dups.len(), 1);
        assert_eq!(dups[0].note, "first", "first wins, deterministically");
    }

    #[test]
    fn a_hostile_or_broken_cache_yields_nothing_rather_than_a_bad_row() {
        use serde_json::json;
        // Every shape the server could hand us that isn't a usable entry. None may panic, and
        // none may reach the wire.
        let doc = json!({ "additionalModelOptionsCache": [
            { "label": "no value at all" },
            { "value": "" },
            { "value": "   " },
            { "value": 42 },
            { "value": { "nested": "object" } },
            "not an object",
            ["not an object either"],
            null,
            // A value we would have to repair is one we no longer know is right: typing a
            // "fixed" id could switch to a different model than the row promised.
            { "value": "opus\nrm -rf ~" },
            { "value": "opus\u{7}" },
        ]});
        assert!(entries_from(&doc).is_empty(), "{:?}", entries_from(&doc));

        // ...and the shapes that mean "no cache" rather than "bad cache".
        assert!(entries_from(&json!({})).is_empty());
        assert!(entries_from(&json!({ "additionalModelOptionsCache": null })).is_empty());
        assert!(entries_from(&json!({ "additionalModelOptionsCache": [] })).is_empty());
        assert!(entries_from(&json!({ "additionalModelOptionsCache": "nope" })).is_empty());
    }

    #[test]
    fn a_blank_label_never_renders_a_nameless_row() {
        use serde_json::json;
        // `label: ""` is PRESENT, so an `unwrap_or(value)` fallback never fired and the phone got
        // a picker row with no name. Display fields are repaired rather than dropped — losing a
        // whole model over a stray character in its description is the worse trade.
        let out = entries_from(&json!({ "additionalModelOptionsCache": [
            { "value": "claude-fable-5-1[1m]", "label": "  ", "description": " Fable 5.1 \u{7}" },
        ]}));
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].name, "claude-fable-5-1[1m]", "falls back to the value");
        assert_eq!(out[0].note, "Fable 5.1", "control characters and padding gone");
    }

    #[test]
    fn a_real_cache_entry_survives_intact() {
        use serde_json::json;
        // The shape actually observed in ~/.claude.json, passed through byte-for-byte.
        let out = entries_from(&json!({ "additionalModelOptionsCache": [{
            "description": "Fable 5.1 \u{b7} Most capable for your hardest and longest-running tasks",
            "label": "Fable",
            "value": "claude-fable-5-1[1m]",
        }]}));
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].value, "claude-fable-5-1[1m]", "pinned id, not rewritten to an alias");
        assert_eq!(out[0].name, "Fable");
        assert!(out[0].note.starts_with("Fable 5.1 \u{b7} Most capable"));
        assert_eq!(out[0].source, Source::Account);
    }
}
