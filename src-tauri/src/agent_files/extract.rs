//! Which files a tool call names (docs/file-tree.md §9.1), read off the hook's `tool_input`.
//!
//! Only what the call SAYS it touches: a Read's `file_path`, an Edit's, a Grep's folder, the
//! file lines of a Codex patch. Bash is never parsed: a command line is a guess about what it
//! writes, and the tree shows evidence (§9.2), so a shell call is reported as one and what it
//! changes comes from the watcher while it runs.
//!
//! Runs in the hook's path, so it does no IO and copies no content: a Write's `tool_input`
//! carries the whole file, and only its path is taken from it.

use serde_json::Value;

/// One path a call names, as the call spells it (absolute, or relative to the session's
/// folder for a Codex patch), and what it does to it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Touch {
    pub path: String,
    /// "read" | "search" | "edit" | "write" | "create" | "delete". A Write is "write" here;
    /// whether it made the file is known only by looking before it runs.
    pub op: &'static str,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Extracted {
    /// Nothing on disk is named (a web fetch, a task tool, an MCP call).
    Nothing,
    Files(Vec<Touch>),
    /// A shell call: what it changes is attributed from the watcher while it runs.
    Shell,
}

/// The shell tools of each runtime. Claude's `Bash` and `PowerShell`; Codex's are spelled as
/// its function names (`shell`, `local_shell`, `exec_command`).
pub fn is_shell(tool: &str) -> bool {
    matches!(tool, "Bash" | "PowerShell" | "shell" | "local_shell" | "exec_command")
}

/// The ops that change a file, as opposed to looking at one.
pub fn is_write_op(op: &str) -> bool {
    matches!(op, "edit" | "write" | "create" | "delete" | "changed")
}

pub fn extract(tool: &str, input: &Value) -> Extracted {
    let str_field = |k: &str| input.get(k).and_then(|v| v.as_str()).filter(|s| !s.is_empty());
    let one = |path: Option<&str>, op: &'static str| match path {
        Some(p) => Extracted::Files(vec![Touch { path: p.to_string(), op }]),
        None => Extracted::Nothing,
    };
    // A Codex patch arrives as `apply_patch` or, in the rollouts on disk, as its `shell` tool
    // running `["apply_patch", "<patch>"]`: either way the patch text is what names the files.
    // Checked before the shell rule, so a patch is never mistaken for an opaque command.
    if tool == "apply_patch" || is_shell(tool) {
        if let Some(text) = patch_text(input) {
            return Extracted::Files(patch_paths(text));
        }
    }
    match tool {
        "Read" => one(str_field("file_path"), "read"),
        "Edit" | "MultiEdit" => one(str_field("file_path"), "edit"),
        "Write" => one(str_field("file_path"), "write"),
        "NotebookEdit" => one(str_field("notebook_path"), "edit"),
        // A search names a folder only when it was given one; without, it searched the session's
        // folder, which would mark the whole root and say nothing.
        "Grep" | "Glob" => one(str_field("path"), "search"),
        t if is_shell(t) => Extracted::Shell,
        _ => Extracted::Nothing,
    }
}

/// The text of a Codex patch in `input`: the input itself, a string field, or an element of a
/// command array.
fn patch_text(input: &Value) -> Option<&str> {
    let is_patch = |s: &&str| s.trim_start().starts_with("*** Begin Patch");
    match input {
        Value::String(s) => Some(s.as_str()).filter(is_patch),
        Value::Array(items) => items.iter().filter_map(|v| v.as_str()).find(is_patch),
        Value::Object(map) => map.values().find_map(|v| match v {
            Value::String(s) => Some(s.as_str()).filter(is_patch),
            Value::Array(items) => items.iter().filter_map(|v| v.as_str()).find(is_patch),
            _ => None,
        }),
        _ => None,
    }
}

/// The files a patch names. `Update File` then `Move to` is a rename: the old name goes and the
/// new one is made.
pub fn patch_paths(text: &str) -> Vec<Touch> {
    let mut out: Vec<Touch> = Vec::new();
    for line in text.lines() {
        let line = line.trim_end_matches('\r');
        let (op, rest) = if let Some(p) = line.strip_prefix("*** Add File: ") {
            ("create", p)
        } else if let Some(p) = line.strip_prefix("*** Update File: ") {
            ("edit", p)
        } else if let Some(p) = line.strip_prefix("*** Delete File: ") {
            ("delete", p)
        } else if let Some(p) = line.strip_prefix("*** Move to: ") {
            if let Some(last) = out.last_mut().filter(|t| t.op == "edit") {
                last.op = "delete";
            }
            ("create", p)
        } else {
            continue;
        };
        let path = rest.trim();
        if !path.is_empty() {
            out.push(Touch { path: path.to_string(), op });
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn files(e: Extracted) -> Vec<(String, &'static str)> {
        match e {
            Extracted::Files(v) => v.into_iter().map(|t| (t.path, t.op)).collect(),
            other => panic!("expected files, got {other:?}"),
        }
    }

    // The `tool_input` shapes below are copied from real Claude Code transcripts on this
    // machine (a transcript's `tool_use.input` is what the hook carries as `tool_input`), long
    // strings shortened. MultiEdit and NotebookEdit appear nowhere in that corpus: their shapes
    // are Claude Code's documented ones and say so.

    #[test]
    fn read_edit_and_write_name_their_file() {
        let read = json!({ "file_path": "/Users/dprusak/DATA/IDE/BillingsApp/package.json", "limit": 22 });
        assert_eq!(files(extract("Read", &read)), vec![("/Users/dprusak/DATA/IDE/BillingsApp/package.json".into(), "read")]);
        let edit = json!({ "replace_all": false, "file_path": "/repo/hooks/register.ts", "old_string": "    const dialog = next(e)", "new_string": "    type First = …" });
        assert_eq!(files(extract("Edit", &edit)), vec![("/repo/hooks/register.ts".into(), "edit")]);
        let write = json!({ "file_path": "/private/tmp/claude-503/x/sink.py", "content": "\"\"\"Probe sink for the maiterm-probe mod." });
        assert_eq!(files(extract("Write", &write)), vec![("/private/tmp/claude-503/x/sink.py".into(), "write")]);
    }

    #[test]
    fn multi_edit_and_notebook_edit_documented_shapes() {
        let multi = json!({ "file_path": "/repo/a.ts", "edits": [{ "old_string": "a", "new_string": "b" }] });
        assert_eq!(files(extract("MultiEdit", &multi)), vec![("/repo/a.ts".into(), "edit")]);
        let nb = json!({ "notebook_path": "/repo/n.ipynb", "cell_id": "c1", "new_source": "x = 1", "edit_mode": "replace" });
        assert_eq!(files(extract("NotebookEdit", &nb)), vec![("/repo/n.ipynb".into(), "edit")]);
    }

    #[test]
    fn grep_and_glob_search_their_folder_and_only_a_named_one() {
        let grep = json!({ "pattern": "update_account", "path": "/Users/dprusak/DATA/IDE/maiBooks", "output_mode": "content", "-A": 30 });
        assert_eq!(files(extract("Grep", &grep)), vec![("/Users/dprusak/DATA/IDE/maiBooks".into(), "search")]);
        let glob = json!({ "pattern": "src/lib/db/queries/*.ts", "path": "/Users/dprusak/DATA/IDE/maiBooks" });
        assert_eq!(files(extract("Glob", &glob)), vec![("/Users/dprusak/DATA/IDE/maiBooks".into(), "search")]);
        assert_eq!(extract("Grep", &json!({ "pattern": "x" })), Extracted::Nothing);
    }

    #[test]
    fn bash_is_a_shell_call_and_is_never_parsed() {
        let bash = json!({ "command": "rm -rf build && touch out.txt", "description": "Clean" });
        assert_eq!(extract("Bash", &bash), Extracted::Shell);
        assert_eq!(extract("PowerShell", &json!({ "command": "Remove-Item x" })), Extracted::Shell);
        assert_eq!(extract("WebFetch", &json!({ "url": "https://example.com" })), Extracted::Nothing);
    }

    /// The patch text and its envelope are a real Codex rollout's (`function_call` "shell" with
    /// `{"command": ["apply_patch", "<patch>"]}`), codex-cli of 2025-09. Whether its HOOKS fire
    /// for apply_patch, and under which tool name, is unverified (docs/codex-integration-review.md),
    /// so both the `shell` envelope and a bare `apply_patch` are read.
    #[test]
    fn a_codex_patch_names_its_files_in_any_envelope() {
        let patch = "*** Begin Patch\n*** Update File: src/routes/+layout.ts\n@@\n export const ssr = false;\n-export const prerender = true;\n+export const prerender = false;\n*** End Patch";
        let shell = json!({ "command": ["apply_patch", patch] });
        assert_eq!(files(extract("shell", &shell)), vec![("src/routes/+layout.ts".into(), "edit")]);
        assert_eq!(files(extract("apply_patch", &json!({ "input": patch }))), vec![("src/routes/+layout.ts".into(), "edit")]);
        assert_eq!(files(extract("apply_patch", &json!(patch))), vec![("src/routes/+layout.ts".into(), "edit")]);
    }

    #[test]
    fn patch_adds_deletes_and_moves() {
        let text = "*** Begin Patch\n*** Add File: a.txt\n+hello\n*** Delete File: old.txt\n*** Update File: src/x.ts\n*** Move to: src/y.ts\n@@\n-a\n+b\n*** End Patch\n";
        let got: Vec<(String, &str)> = patch_paths(text).into_iter().map(|t| (t.path, t.op)).collect();
        assert_eq!(
            got,
            vec![("a.txt".into(), "create"), ("old.txt".into(), "delete"), ("src/x.ts".into(), "delete"), ("src/y.ts".into(), "create")]
        );
    }
}
