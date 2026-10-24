//! A memory file's frontmatter, as Claude's auto memory writes it (docs/hive.md §9, spike 5):
//! `name`, `description`, and `metadata {node_type, type, originSessionId, modified}`.
//!
//! A small YAML subset: top-level `key: value`, one level under `metadata:`, quoted or plain
//! scalars, and `|`/`>` blocks. Anything it can't read is `Front::default()`, never an error: a
//! file with broken frontmatter is still a memory file, titled by its name.

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Front {
    pub name: Option<String>,
    pub description: Option<String>,
    pub node_type: Option<String>,
    pub type_: Option<String>,
    pub origin_session_id: Option<String>,
    pub modified: Option<String>,
    /// Byte offset of the body: just past the closing `---` line, or 0 with no frontmatter.
    pub body_start: usize,
}

/// One line and the byte offset just past its end (its newline included).
fn lines_with_ends(text: &str) -> Vec<(&str, usize)> {
    let mut out = Vec::new();
    let mut at = 0;
    for raw in text.split_inclusive('\n') {
        at += raw.len();
        out.push((raw.trim_end_matches(['\n', '\r']), at));
    }
    out
}

fn unquote(v: &str) -> String {
    let v = v.trim();
    if v.len() >= 2 && v.starts_with('"') && v.ends_with('"') {
        let inner = &v[1..v.len() - 1];
        let mut out = String::with_capacity(inner.len());
        let mut chars = inner.chars();
        while let Some(c) = chars.next() {
            if c != '\\' {
                out.push(c);
                continue;
            }
            match chars.next() {
                Some('n') => out.push('\n'),
                Some('t') => out.push('\t'),
                Some(o) => out.push(o),
                None => out.push('\\'),
            }
        }
        return out;
    }
    if v.len() >= 2 && v.starts_with('\'') && v.ends_with('\'') {
        return v[1..v.len() - 1].replace("''", "'");
    }
    // A plain scalar's comment: ` #` only, since `#` may sit inside a word.
    v.split_once(" #").map_or(v, |(a, _)| a).trim_end().to_string()
}

fn indent(line: &str) -> usize {
    line.len() - line.trim_start_matches([' ', '\t']).len()
}

pub fn parse(text: &str) -> Front {
    let text_nobom = text.strip_prefix('\u{feff}').unwrap_or(text);
    let bom = text.len() - text_nobom.len();
    let lines = lines_with_ends(text_nobom);
    if lines.first().map(|(l, _)| l.trim_end()) != Some("---") {
        return Front::default();
    }
    let Some(close) = lines.iter().skip(1).position(|(l, _)| matches!(l.trim_end(), "---" | "...")).map(|i| i + 1) else {
        return Front::default();
    };
    let mut f = Front { body_start: bom + lines[close].1, ..Front::default() };
    let body = &lines[1..close];
    let mut in_metadata = false;
    let mut i = 0;
    while i < body.len() {
        let line = body[i].0;
        i += 1;
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let depth = indent(line);
        if depth == 0 {
            in_metadata = false;
        } else if !in_metadata {
            continue;
        }
        let Some((key, value)) = trimmed.split_once(':') else { continue };
        let (key, value) = (key.trim(), value.trim());
        if depth == 0 && key == "metadata" && value.is_empty() {
            in_metadata = true;
            continue;
        }
        let value = if matches!(value.trim_end_matches(['-', '+']), "|" | ">") {
            // A block scalar: the more-indented lines after it, joined (`|` keeps the breaks).
            let mut parts: Vec<&str> = Vec::new();
            while i < body.len() && (body[i].0.trim().is_empty() || indent(body[i].0) > depth) {
                parts.push(body[i].0.trim());
                i += 1;
            }
            let sep = if value.starts_with('|') { "\n" } else { " " };
            parts.join(sep).trim().to_string()
        } else {
            unquote(value)
        };
        let slot = match key {
            "name" if depth == 0 => &mut f.name,
            "description" if depth == 0 => &mut f.description,
            "node_type" => &mut f.node_type,
            "type" => &mut f.type_,
            "originSessionId" => &mut f.origin_session_id,
            "modified" => &mut f.modified,
            _ => continue,
        };
        if !value.is_empty() {
            *slot = Some(value);
        }
    }
    f
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claudes_own_format_parses_with_either_line_ending() {
        let lf = "---\nname: Session store decision\ndescription: \"Redis, not Postgres\"\nmetadata:\n  node_type: memory\n  type: project\n  originSessionId: 6b1e\n  modified: 2026-10-24\n---\n\nBody here.\n";
        let crlf = lf.replace('\n', "\r\n");
        for text in [lf.to_string(), crlf] {
            let f = parse(&text);
            assert_eq!(f.name.as_deref(), Some("Session store decision"));
            assert_eq!(f.description.as_deref(), Some("Redis, not Postgres"));
            assert_eq!(f.node_type.as_deref(), Some("memory"));
            assert_eq!(f.type_.as_deref(), Some("project"));
            assert_eq!(f.origin_session_id.as_deref(), Some("6b1e"));
            assert_eq!(f.modified.as_deref(), Some("2026-10-24"));
            assert!(text[f.body_start..].trim_start().starts_with("Body here."), "{:?}", &text[f.body_start..]);
        }
    }

    #[test]
    fn scalars_blocks_and_comments() {
        let f = parse("---\nname: 'It''s here'\ndescription: >\n  folded\n  text\ntype: plain # a comment\nmetadata:\n  node_type: \"a \\\"q\\\"\"\nother: x\n  type: ignored\n---\n");
        assert_eq!(f.name.as_deref(), Some("It's here"));
        assert_eq!(f.description.as_deref(), Some("folded text"));
        assert_eq!(f.type_.as_deref(), Some("plain"));
        assert_eq!(f.node_type.as_deref(), Some("a \"q\""));
        let lit = parse("---\ndescription: |\n  one\n  two\n---\n");
        assert_eq!(lit.description.as_deref(), Some("one\ntwo"));
    }

    #[test]
    fn anything_unreadable_is_the_default() {
        for text in ["", "no frontmatter", "---\nname: never closed\n", "  ---\nname: x\n---\n", "# Title\n---\nname: x\n---\n"] {
            assert_eq!(parse(text), Front::default(), "{text:?}");
        }
        // A closed block with nothing usable still marks where the body starts.
        let f = parse("---\n: : :\n---\nbody");
        assert_eq!((f.name, f.body_start), (None, 14));
    }
}
