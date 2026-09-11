//! Workspace stack — the suggester (docs/stack.md §8).
//!
//! `suggest_stack(cwd)` reads the files a project already uses to say what it runs and
//! offers rows to import: `package.json` scripts, a `Procfile`, compose services, `justfile`
//! recipes and `Makefile` targets. No shell-outs, no YAML crate (compose is scanned for its
//! `services:` keys, which is all that is needed), and it runs on the blocking pool because
//! it touches the filesystem from a Tauri command.

use serde::Serialize;
use std::path::Path;

#[derive(Debug, Clone, Serialize)]
pub struct StackSuggestion {
    pub name: String,
    pub command: String,
    pub cwd: String,
    /// Where it came from, for the checklist: "package.json", "Procfile", "compose", "justfile", "Makefile".
    pub source: String,
    /// Pre-ticked in the import checklist: the dev/start-shaped ones.
    pub recommended: bool,
}

fn looks_like_a_service(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    ["dev", "start", "serve", "watch", "storybook", "preview", "run", "up"]
        .iter()
        .any(|k| n == *k || n.starts_with(&format!("{k}:")) || n.starts_with(&format!("{k}-")) || n.ends_with(&format!(":{k}")) || n.ends_with(&format!("-{k}")))
}

fn is_recommended(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    n == "dev" || n == "start" || n == "serve" || n == "up"
}

fn package_manager(dir: &Path) -> &'static str {
    if dir.join("pnpm-lock.yaml").exists() {
        "pnpm"
    } else if dir.join("bun.lockb").exists() || dir.join("bun.lock").exists() {
        "bun"
    } else if dir.join("yarn.lock").exists() {
        "yarn"
    } else {
        "npm"
    }
}

fn from_package_json(dir: &Path, out: &mut Vec<StackSuggestion>) {
    let Ok(text) = std::fs::read_to_string(dir.join("package.json")) else { return };
    let Ok(json) = serde_json::from_str::<serde_json::Value>(&text) else { return };
    let Some(scripts) = json.get("scripts").and_then(|s| s.as_object()) else { return };
    let pm = package_manager(dir);
    for (name, _) in scripts {
        if !looks_like_a_service(name) {
            continue;
        }
        out.push(StackSuggestion {
            name: name.clone(),
            command: format!("{pm} run {name}"),
            cwd: dir.display().to_string(),
            source: "package.json".into(),
            recommended: is_recommended(name),
        });
    }
}

fn from_procfile(dir: &Path, out: &mut Vec<StackSuggestion>) {
    let Ok(text) = std::fs::read_to_string(dir.join("Procfile")) else { return };
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((name, cmd)) = line.split_once(':') else { continue };
        let (name, cmd) = (name.trim(), cmd.trim());
        if name.is_empty() || cmd.is_empty() {
            continue;
        }
        out.push(StackSuggestion {
            name: name.to_string(),
            command: cmd.to_string(),
            cwd: dir.display().to_string(),
            source: "Procfile".into(),
            recommended: true,
        });
    }
}

/// Top-level `services:` keys of a compose file, without a YAML parser: the block's direct
/// children are the lines indented by exactly one level that end in `:`.
fn compose_services(text: &str) -> Vec<String> {
    let mut names = Vec::new();
    let mut in_services = false;
    let mut child_indent: Option<usize> = None;
    for raw in text.lines() {
        let line = raw.trim_end();
        if line.trim().is_empty() || line.trim_start().starts_with('#') {
            continue;
        }
        let indent = line.len() - line.trim_start().len();
        if indent == 0 {
            in_services = line.trim_end_matches(':') == "services" && line.ends_with(':');
            child_indent = None;
            continue;
        }
        if !in_services {
            continue;
        }
        let ci = *child_indent.get_or_insert(indent);
        if indent == ci {
            if let Some(name) = line.trim().strip_suffix(':') {
                if !name.is_empty() && !name.contains(' ') {
                    names.push(name.to_string());
                }
            }
        }
    }
    names
}

fn from_compose(dir: &Path, out: &mut Vec<StackSuggestion>) {
    for file in ["docker-compose.yml", "docker-compose.yaml", "compose.yml", "compose.yaml"] {
        let Ok(text) = std::fs::read_to_string(dir.join(file)) else { continue };
        let names = compose_services(&text);
        if names.is_empty() {
            return;
        }
        for name in &names {
            out.push(StackSuggestion {
                name: name.clone(),
                command: format!("docker compose up {name}"),
                cwd: dir.display().to_string(),
                source: "compose".into(),
                recommended: false,
            });
        }
        out.push(StackSuggestion {
            name: "compose".into(),
            command: "docker compose up".into(),
            cwd: dir.display().to_string(),
            source: "compose".into(),
            recommended: true,
        });
        return;
    }
}

/// `name:` / `name args:` at column 0 — justfile recipes and Makefile targets share the shape.
fn recipe_names(text: &str) -> Vec<String> {
    let mut names = Vec::new();
    for line in text.lines() {
        if line.starts_with(|c: char| c.is_whitespace()) || line.starts_with('#') || line.starts_with('.') {
            continue;
        }
        let Some((head, _)) = line.split_once(':') else { continue };
        if head.contains('=') {
            continue;
        }
        let name = head.split_whitespace().next().unwrap_or("").trim();
        if name.is_empty() || name.contains('$') || name.contains('%') || name.contains('(') {
            continue;
        }
        names.push(name.to_string());
    }
    names
}

fn from_recipes(dir: &Path, file: &str, runner: &str, source: &str, out: &mut Vec<StackSuggestion>) {
    let Ok(text) = std::fs::read_to_string(dir.join(file)) else { return };
    for name in recipe_names(&text) {
        if !looks_like_a_service(&name) {
            continue;
        }
        out.push(StackSuggestion {
            name: name.clone(),
            command: format!("{runner} {name}"),
            cwd: dir.display().to_string(),
            source: source.into(),
            recommended: is_recommended(&name),
        });
    }
}

pub fn suggest_for_dir(dir: &Path) -> Vec<StackSuggestion> {
    let mut out = Vec::new();
    from_package_json(dir, &mut out);
    from_procfile(dir, &mut out);
    from_compose(dir, &mut out);
    from_recipes(dir, "justfile", "just", "justfile", &mut out);
    from_recipes(dir, "Justfile", "just", "justfile", &mut out);
    from_recipes(dir, "Makefile", "make", "Makefile", &mut out);
    out
}

#[tauri::command]
pub async fn suggest_stack(cwd: String) -> Result<Vec<StackSuggestion>, String> {
    tauri::async_runtime::spawn_blocking(move || suggest_for_dir(Path::new(&cwd)))
        .await
        .map_err(|e| format!("suggester failed to run: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compose_services_reads_the_top_level_block_only() {
        let text = "version: '3'\nservices:\n  web:\n    image: x\n    ports:\n      - 80:80\n  db:\n    image: pg\nvolumes:\n  data:\n";
        assert_eq!(compose_services(text), vec!["web", "db"]);
    }

    #[test]
    fn compose_services_tolerates_tabs_and_comments() {
        let text = "services:\n  # the app\n  api:\n    build: .\n\n  worker:\n    build: .\n";
        assert_eq!(compose_services(text), vec!["api", "worker"]);
    }

    #[test]
    fn recipe_names_skip_variables_and_indented_lines() {
        let text = "CC = gcc\n.PHONY: all\nall: dev\ndev:\n\tnpm run dev\nserve args: build\n\t./serve\n";
        assert_eq!(recipe_names(text), vec!["all", "dev", "serve"]);
    }

    #[test]
    fn service_shaped_names() {
        assert!(looks_like_a_service("dev"));
        assert!(looks_like_a_service("dev:web"));
        assert!(looks_like_a_service("storybook"));
        assert!(looks_like_a_service("test:watch"));
        assert!(!looks_like_a_service("build"));
        assert!(!looks_like_a_service("lint"));
        assert!(is_recommended("dev") && !is_recommended("test:watch"));
    }
}
