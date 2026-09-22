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
    /// Short display name. COSMETIC ONLY — it is the server's marketing label on an account row
    /// ("Fable" for `claude-fable-5-1[1m]`), so it does not name the model and must never be
    /// parsed. Match on `display`/`family` instead.
    pub name: String,
    /// One line of what it is for.
    pub note: String,
    pub source: Source,
    /// What `meta.model` reads for a session ON this exact value — the same `display_model`
    /// rendering the chat rows use, so a consumer can compare the two strings directly instead of
    /// re-deriving one from the other.
    ///
    /// An alias renders WITHOUT a version ("Opus"), because that is all an alias names. A session
    /// on "Opus 4.6" therefore does not equal the `opus` row, which is correct: we do not know
    /// that `/model opus` would leave it where it is.
    pub display: String,
    /// The model family this row switches into, lowercase and stated rather than parsed out of a
    /// label: `opus`, `sonnet`, `haiku`, `fable`. Lets a consumer offer a deliberate
    /// family-granular affordance without inferring one from punctuation.
    pub family: String,
    /// True when another row in the SAME response carries this `display`, so matching it does not
    /// identify one row.
    ///
    /// It is set on every `[1m]` pair, because the window marker is not part of a model's identity
    /// and `display_model` strips it: `opus` and `opus[1m]` both render "Opus". A session whose
    /// transcript stamps the bare alias therefore equals both, and only one of them would leave
    /// its context window alone.
    ///
    /// maiTerm cannot break the tie, and that is the point of saying so here rather than in prose.
    /// A session's window is inferred per FAMILY (`ASSUMED_1M_MODELS`) because the transcript does
    /// not reliably carry the marker, so "is this session on the 1M variant" is not a question this
    /// side can answer. A consumer must treat a match on an ambiguous row as "on this model",
    /// never as "on this row", and must not promise an outcome that only distinguishing the two
    /// could confirm.
    pub ambiguous: bool,
}

/// Everything after the provider prefix and the window marker: `claude-fable-5-1[1m]` → `fable`.
///
/// Empty segments are skipped, exactly as [`display_model`](super::display_model) skips them.
/// The two must agree: a single doubled hyphen in the server cache (`claude--fable-5-1[1m]`)
/// otherwise leaves `display` looking perfectly correct — "Fable 5.1" — while `family` comes back
/// empty, so one derived field silently contradicts the other on a value neither of them rejected.
/// Empty means "this value names no family", which the caller treats as naming no model.
fn family_of(value: &str) -> String {
    let s = value.replace("[1m]", "");
    let s = s.strip_prefix("claude-").unwrap_or(&s).to_string();
    let s = s.strip_suffix("-1m").unwrap_or(&s).to_string();
    s.split('-').find(|p| !p.is_empty()).unwrap_or_default().to_lowercase()
}

/// Build a row, deriving the two matchable fields from `value` so they can never disagree with it.
///
/// `ambiguous` starts false and is settled by [`merge`], since whether a `display` is unique is a
/// property of the whole response rather than of one row.
fn option(value: String, name: String, note: String, source: Source) -> ModelOption {
    let display = super::display_model(&value);
    let family = family_of(&value);
    ModelOption { value, name, note, source, display, family, ambiguous: false }
}

/// Flag every row whose `display` is shared with another row in the same response.
fn mark_ambiguous(rows: &mut [ModelOption]) {
    let mut counts: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
    for r in rows.iter() {
        *counts.entry(r.display.as_str()).or_default() += 1;
    }
    let shared: std::collections::HashSet<String> = counts
        .into_iter()
        .filter(|(_, n)| *n > 1)
        .map(|(d, _)| d.to_string())
        .collect();
    for r in rows.iter_mut() {
        r.ambiguous = shared.contains(&r.display);
    }
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
    from_file(&home.join(".claude.json"))
}

fn from_file(path: &std::path::Path) -> Vec<ModelOption> {
    let Ok(raw) = std::fs::read(path) else { return Vec::new() };
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
            // A value that names no family names no model — `claude-`, `[1m]`, a bare marker.
            // These survive the alteration check above (nothing needed cleaning) but there is
            // nothing for `/model` to switch to, so drop them rather than emit a row whose
            // `family` contradicts its own documented contract.
            if family_of(&value).is_empty() {
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
            Some(option(value, name, note, Source::Account))
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
            out.push(option(
                value.to_string(),
                name.to_string(),
                note.to_string(),
                Source::Builtin,
            ));
        }
    }
    mark_ambiguous(&mut out);
    out
}

/// Everything this machine can switch a Claude tab to.
pub fn available() -> Vec<ModelOption> {
    merge(from_account_cache())
}

/// Whose `additionalModelOptionsCache` a list may be built from (maiLink §14.4).
pub enum Cache {
    /// `~/.claude.json` — right only when that file IS the tab's login (an unmanaged tab).
    Home,
    /// A managed account's own root.
    At(std::path::PathBuf),
    /// Not known whose cache would apply (an unknown or remote tab): builtins only. Never an
    /// empty list — to the phone `[]` means a desktop that predates `/models`.
    None,
}

pub fn available_from(cache: Cache) -> Vec<ModelOption> {
    merge(match cache {
        Cache::Home => from_account_cache(),
        Cache::At(path) => from_file(&path),
        Cache::None => Vec::new(),
    })
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
        let out = merge(vec![option(
            "opus[1m]".into(),
            "Opus 5".into(),
            "from the server".into(),
            Source::Account,
        )]);
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
        let mk = |note: &str| option("dup".into(), "Dup".into(), note.into(), Source::Account);
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
    fn the_label_never_names_the_model_but_display_always_does() {
        use serde_json::json;
        // The live defect this pair exists to end: the server's label for
        // `claude-fable-5-1[1m]` is the bare family word "Fable", so a consumer matching on
        // `name` lit "you are already on this" for four sessions running Fable 5 against a row
        // pinned to Fable 5.1 — a row that would in fact have moved them.
        let out = entries_from(&json!({ "additionalModelOptionsCache": [
            { "value": "claude-fable-5-1[1m]", "label": "Fable", "description": "d" },
        ]}));
        assert_eq!(out[0].name, "Fable", "the label stays exactly as the server wrote it");
        assert_eq!(out[0].display, "Fable 5.1", "but display names the actual model");
        assert_eq!(out[0].family, "fable");
        assert_ne!(out[0].display, "Fable 5", "so a Fable 5 session cannot match this row");
    }

    #[test]
    fn an_alias_row_renders_without_a_version_because_that_is_all_it_names() {
        // We do NOT know what `/model opus` resolves to — this machine ran claude-opus-4-8 and
        // claude-opus-5 within two minutes of each other, so even observation cannot settle it.
        // `display` therefore stops at the family, and an "Opus 4.6" session correctly fails to
        // equal it rather than being told it is already there.
        let opus = merge(Vec::new()).into_iter().find(|m| m.value == "opus").unwrap();
        assert_eq!(opus.display, "Opus");
        assert_eq!(opus.family, "opus");
        assert_ne!(opus.display, "Opus 4.6");

        // The window marker is not part of the model's identity, so both windows render alike.
        let opus_1m = merge(Vec::new()).into_iter().find(|m| m.value == "opus[1m]").unwrap();
        assert_eq!(opus_1m.display, "Opus", "[1m] is a window, not a different model");
        assert_eq!(opus_1m.family, "opus");
    }

    #[test]
    fn a_malformed_id_never_reaches_the_wire_with_a_contradictory_family() {
        use serde_json::json;
        // Found by review. `family_of` split on '-' without skipping empty segments while
        // `display_model` skips them, so these four passed the alteration check (nothing needed
        // cleaning) and shipped with `family: ""`. The first is the sharp one: a single doubled
        // hyphen leaves `display` reading a perfect "Fable 5.1" while family silently disagrees.
        //
        // This drives the ACCOUNT path deliberately. The previous version of this check iterated
        // `merge(Vec::new())` — builtin rows only, all compile-time literals — so it never touched
        // the one path where `value` is untrusted, which is the only path that can produce this.
        for bad in ["claude--fable-5-1[1m]", "claude-", "-opus-5", "[1m]", "--", "-"] {
            let out = entries_from(&json!({ "additionalModelOptionsCache": [{ "value": bad }] }));
            let families: Vec<&str> = out.iter().map(|m| m.family.as_str()).collect();
            assert!(
                out.is_empty() || !families.contains(&""),
                "{bad} emitted a row with no family: {out:?}"
            );
        }

        // A doubled hyphen now resolves to the same family the display already implied, rather
        // than being dropped — repairing the derivation, not the value.
        let ok = entries_from(&json!({ "additionalModelOptionsCache": [
            { "value": "claude--fable-5-1[1m]" },
        ]}));
        assert_eq!(ok.len(), 1);
        assert_eq!(ok[0].family, "fable");
        assert_eq!(ok[0].display, "Fable 5.1", "and display was right all along");

        // A value naming no family at all names no model, so it is dropped outright.
        for empty in ["claude-", "[1m]", "-", "--"] {
            let out = entries_from(&json!({ "additionalModelOptionsCache": [{ "value": empty }] }));
            assert!(out.is_empty(), "{empty} should name no model, got {out:?}");
        }
    }

    #[test]
    fn a_shared_display_is_flagged_so_a_match_is_not_read_as_identifying_one_row() {
        // Found by review. `display_model` strips `[1m]`, so `opus` and `opus[1m]` both render
        // "Opus" — and a transcript stamping the bare alias (53 real occurrences on this machine)
        // equals BOTH, one of which would change the context window. maiTerm cannot break the tie:
        // a session's window is inferred per family, not observed. So the response says so.
        let out = merge(Vec::new());
        for pair in ["opus", "sonnet"] {
            let rows: Vec<&ModelOption> = out.iter().filter(|m| m.family == pair).collect();
            assert_eq!(rows.len(), 2, "{pair} should have a 1M pair");
            assert_eq!(rows[0].display, rows[1].display, "which render identically");
            assert!(rows.iter().all(|r| r.ambiguous), "{pair} rows must admit the tie");
        }
        // Haiku is unpaired, so its match DOES identify its row.
        let haiku: Vec<&ModelOption> = out.iter().filter(|m| m.family == "haiku").collect();
        assert_eq!(haiku.len(), 1);
        assert!(!haiku[0].ambiguous, "an unpaired row must not be flagged");
    }

    #[test]
    fn an_account_row_that_collides_with_a_builtin_display_is_flagged_too() {
        // Ambiguity is a property of the RESPONSE, not of the builtin table: an account row can
        // collide with a curated one. Here a pinned id renders "Haiku", the same as the alias row,
        // which on its own would have left the previously-unique haiku row silently ambiguous.
        let out = merge(vec![option(
            "claude-haiku[1m]".into(),
            "Haiku".into(),
            "pinned".into(),
            Source::Account,
        )]);
        let haiku: Vec<&ModelOption> = out.iter().filter(|m| m.display == "Haiku").collect();
        assert_eq!(haiku.len(), 2, "the account row and the alias row both render Haiku");
        assert!(haiku.iter().all(|r| r.ambiguous), "both must be flagged, not just the builtin");
    }

    #[test]
    fn every_row_derives_its_matchable_fields_from_its_own_value() {
        // The two fields a consumer matches on are built from `value` at construction, so no
        // future edit to a label or note can leave them disagreeing with what the row switches to.
        for m in merge(Vec::new()) {
            assert_eq!(m.display, super::super::display_model(&m.value), "{}", m.value);
            assert_eq!(m.family, family_of(&m.value), "{}", m.value);
            assert!(!m.family.is_empty(), "{} produced no family", m.value);
        }
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
