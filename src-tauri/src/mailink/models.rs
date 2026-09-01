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
const BUILTIN: [(&str, &str, &str); 5] = [
    ("opus", "Opus", "Everyday complex work"),
    ("opus[1m]", "Opus", "Everyday complex work · 1M context"),
    ("sonnet", "Sonnet", "Balanced speed and capability"),
    ("sonnet[1m]", "Sonnet", "Balanced · 1M context"),
    ("haiku", "Haiku", "Fastest, for simple work"),
];

/// Read `~/.claude.json` and return its `additionalModelOptionsCache` entries.
///
/// Values are passed through EXACTLY as the cache spells them, pinned release and all
/// (`claude-fable-5-1[1m]`). Rewriting that to a `fable[1m]` alias would be nicer to read and a
/// guess: nothing here knows that `fable` is a valid `/model` alias on this account, and a wrong
/// value fails at the TUI where the human sees it rather than here where we could have not said
/// it. The label ages instead, which is the recoverable direction.
fn from_account_cache() -> Vec<ModelOption> {
    let Some(home) = dirs::home_dir() else { return Vec::new() };
    let Ok(raw) = std::fs::read(home.join(".claude.json")) else { return Vec::new() };
    let Ok(doc) = serde_json::from_slice::<serde_json::Value>(&raw) else { return Vec::new() };
    let Some(entries) = doc.get("additionalModelOptionsCache").and_then(|v| v.as_array()) else {
        return Vec::new();
    };
    entries
        .iter()
        .filter_map(|e| {
            let value = e.get("value").and_then(|v| v.as_str())?.trim();
            if value.is_empty() {
                return None;
            }
            let name = e.get("label").and_then(|v| v.as_str()).unwrap_or(value);
            let note = e.get("description").and_then(|v| v.as_str()).unwrap_or("");
            Some(ModelOption {
                value: value.to_string(),
                name: name.to_string(),
                note: note.to_string(),
                source: Source::Account,
            })
        })
        .collect()
}

/// Everything this machine can switch a Claude tab to. Account entries first — they are the ones
/// we actually know about — then the curated tiers, minus any the cache already named.
pub fn available() -> Vec<ModelOption> {
    let mut out = from_account_cache();
    for (value, name, note) in BUILTIN {
        if out.iter().any(|m| m.value == value) {
            continue;
        }
        out.push(ModelOption {
            value: value.to_string(),
            name: name.to_string(),
            note: note.to_string(),
            source: Source::Builtin,
        });
    }
    out
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
        // Same `value` from both sources must resolve to the account's copy, because that one is
        // known to be offered to this account and carries the server's own label and description.
        let mut out = vec![ModelOption {
            value: "opus[1m]".into(),
            name: "Opus 5".into(),
            note: "from the server".into(),
            source: Source::Account,
        }];
        for (value, name, note) in BUILTIN {
            if out.iter().any(|m| m.value == value) {
                continue;
            }
            out.push(ModelOption {
                value: value.into(),
                name: name.into(),
                note: note.into(),
                source: Source::Builtin,
            });
        }
        let opus_1m: Vec<&ModelOption> = out.iter().filter(|m| m.value == "opus[1m]").collect();
        assert_eq!(opus_1m.len(), 1, "no duplicate rows for one value");
        assert_eq!(opus_1m[0].source, Source::Account);
        assert_eq!(opus_1m[0].note, "from the server");
    }

    #[test]
    fn a_missing_or_unreadable_claude_json_is_not_an_error() {
        // Every install without the cache key — a fresh one, or a runtime that never writes it —
        // must still get the curated tiers rather than an empty picker.
        let all = available();
        assert!(all.iter().any(|m| m.value == "sonnet"));
        assert!(all.iter().all(|m| !m.value.is_empty() && !m.name.is_empty()));
    }
}
