//! The role catalogue (docs/hive.md §7.7): what the coordinator picks for each worker. A role is
//! data, `resources/hive/roles/<name>.md`: a small frontmatter, then the mission, rules,
//! deliverable, what done means and the tools, with no persona text. The coordinator's brief
//! lists names and one-line descriptions; only the chosen role's body reaches the worker's brief.
//! Never installed as Claude subagents: every description in every worker's context is the
//! bloat this avoids.

use std::sync::OnceLock;

#[derive(Debug, Clone, PartialEq)]
pub struct Role {
    pub name: String,
    pub description: String,
    /// The builder or checker roles it's usually paired with.
    pub pairs_with: Vec<String>,
    /// WebSearch stays in its tools (WebFetch never does, §6.7).
    pub web: bool,
    /// Its worker gets the Hive's generator MCP servers (`settings::worker_mcp_config`, plan D1).
    pub generators: bool,
    /// Who wrote it: `maiterm` for every shipped role.
    pub source: String,
    pub body: String,
}

macro_rules! role_files {
    ($($name:literal),* $(,)?) => {
        &[$(($name, include_str!(concat!("../../resources/hive/roles/", $name, ".md")))),*]
    };
}

const FILES: &[(&str, &str)] = role_files!(
    "backend",
    "frontend",
    "test",
    "refactor",
    "docs",
    "api-tester",
    "ui-reviewer",
    "code-reviewer",
    "image",
    "video",
    "audio",
    "edit",
    "art-director",
);

/// Every shipped role, in catalogue order. A file that doesn't parse is a build defect the
/// tests catch; at runtime it's left out, logged, rather than taking the catalogue down.
pub fn catalogue() -> &'static [Role] {
    static ROLES: OnceLock<Vec<Role>> = OnceLock::new();
    ROLES.get_or_init(|| {
        FILES
            .iter()
            .filter_map(|(file, text)| match parse(text) {
                Ok(r) if r.name == *file => Some(r),
                Ok(r) => {
                    log::error!("hive: role file {file}.md names itself {}", r.name);
                    None
                }
                Err(e) => {
                    log::error!("hive: role file {file}.md: {e}");
                    None
                }
            })
            .collect()
    })
}

pub fn role(name: &str) -> Option<&'static Role> {
    catalogue().iter().find(|r| r.name == name)
}

/// The coordinator's view of the catalogue: one line per role.
pub fn catalogue_lines() -> String {
    catalogue()
        .iter()
        .map(|r| {
            let pairs = if r.pairs_with.is_empty() { String::new() } else { format!(" Pairs with {}.", r.pairs_with.join(", ")) };
            format!("- **{}**: {}{pairs}\n", r.name, r.description)
        })
        .collect()
}

/// `---`, `key: value` lines, `---`, then the body.
fn parse(text: &str) -> Result<Role, String> {
    // A Windows checkout may have turned the files' line endings into CRLF.
    let text = text.replace("\r\n", "\n");
    let rest = text.strip_prefix("---\n").ok_or("no frontmatter")?;
    let (front, body) = rest.split_once("\n---\n").ok_or("unterminated frontmatter")?;
    let mut role = Role { name: String::new(), description: String::new(), pairs_with: Vec::new(), web: false, generators: false, source: String::new(), body: body.trim().to_string() };
    for line in front.lines().filter(|l| !l.trim().is_empty()) {
        let (k, v) = line.split_once(':').ok_or_else(|| format!("\"{line}\" isn't key: value"))?;
        let v = v.trim();
        match k.trim() {
            "name" => role.name = v.to_string(),
            "description" => role.description = v.to_string(),
            "pairs_with" => role.pairs_with = v.split(',').map(str::trim).filter(|s| !s.is_empty()).map(str::to_string).collect(),
            "web" => role.web = v.parse().map_err(|_| format!("web: \"{v}\" isn't true or false"))?,
            "generators" => role.generators = v.parse().map_err(|_| format!("generators: \"{v}\" isn't true or false"))?,
            "source" => role.source = v.to_string(),
            other => return Err(format!("unknown key \"{other}\"")),
        }
    }
    if role.name.is_empty() || role.description.is_empty() || role.source.is_empty() || role.body.is_empty() {
        return Err("name, description, source and a body are required".to_string());
    }
    Ok(role)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_shipped_role_parses_and_is_ours() {
        assert_eq!(catalogue().len(), FILES.len(), "every file parses and names itself");
        for r in catalogue() {
            assert_eq!(r.source, "maiterm", "{}: shipped roles are written by us", r.name);
            for section in ["## Mission", "## Rules", "## Deliverable", "## Done means", "## Tools"] {
                assert!(r.body.contains(section), "{} lacks {section}", r.name);
            }
            assert!(r.body.lines().count() <= 60, "{} is {} lines: a role is a few dozen lines", r.name, r.body.lines().count());
            assert!(r.description.chars().count() <= 120 && !r.description.contains('\n'), "{}: one short line", r.name);
            assert!(!r.pairs_with.is_empty(), "{} pairs with something", r.name);
            for p in &r.pairs_with {
                assert!(role(p).is_some(), "{} pairs with unknown role {p}", r.name);
            }
        }
    }

    #[test]
    fn the_catalogue_has_the_builders_and_their_checkers() {
        for name in ["backend", "frontend", "test", "refactor", "docs", "api-tester", "ui-reviewer", "code-reviewer", "image", "video", "audio", "edit"] {
            assert!(role(name).is_some(), "{name}");
        }
        assert_eq!(role("backend").unwrap().pairs_with, vec!["api-tester"]);
        assert_eq!(role("frontend").unwrap().pairs_with, vec!["ui-reviewer"]);
        assert!(role("docs").unwrap().web && !role("backend").unwrap().web);
        assert!(role("nope").is_none());
    }

    #[test]
    fn catalogue_lines_are_one_per_role() {
        let lines = catalogue_lines();
        assert_eq!(lines.lines().count(), catalogue().len());
        assert!(lines.contains("- **backend**: Builds server-side features"));
        assert!(lines.contains("Pairs with api-tester."));
        assert!(!lines.contains("## Mission"), "bodies go to workers only");
    }

    #[test]
    fn malformed_frontmatter_is_refused() {
        assert!(parse("no frontmatter").is_err());
        assert!(parse("---\nname: x\n").is_err());
        assert!(parse("---\nname: x\ndescription: d\nsource: s\nweb: maybe\n---\nbody").is_err());
        assert!(parse("---\nname: x\ndescription: d\nsource: s\ncolour: red\n---\nbody").is_err());
        assert!(parse("---\nname: x\ndescription: d\n---\nbody").is_err(), "source is required");
        let r = parse("---\nname: x\ndescription: d\nsource: s\n---\n\nbody\n").unwrap();
        assert_eq!((r.body.as_str(), r.web, r.generators, r.pairs_with.len()), ("body", false, false, 0));
        assert!(parse("---\nname: x\ndescription: d\nsource: s\ngenerators: true\n---\nbody").unwrap().generators);
        assert!(parse("---\nname: x\ndescription: d\nsource: s\ngenerators: often\n---\nbody").is_err());
    }

    #[test]
    fn only_the_media_roles_get_the_generators() {
        let with: Vec<&str> = catalogue().iter().filter(|r| r.generators).map(|r| r.name.as_str()).collect();
        assert_eq!(with, vec!["image", "video", "audio", "edit"]);
    }
}
