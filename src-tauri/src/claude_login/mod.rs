//! Managed Claude Code identities — see `docs/login.md`.
//!
//! An identity is a `CLAUDE_CONFIG_DIR` of its own. maiTerm owns the *directory*; Claude Code
//! owns the credential inside it (§5). We never read, write or parse a `/login` credential, and
//! we never touch Claude Code's Keychain item.
//!
//! The catch this module exists to handle: `CLAUDE_CONFIG_DIR` relocates the ENTIRE config root
//! (§5.4). A bare identity directory loses the `SessionStart` hook that establishes tab identity,
//! every MCP server including maiTerm's own, transcript discovery for maiLink, and the user's
//! skills, commands and permissions. So an identity root is a farm of symlinks back to the real
//! `~/.claude`, and only the credential is per-identity.

use std::fs;
use std::path::{Path, PathBuf};

use crate::state::persistence::app_data_slug;

/// Entries symlinked back to the user's real config, shared by every identity.
///
/// `docs/login.md` §5.4 pins this list. Each was verified to matter: dropping `settings.json`
/// loses hooks and tab identity, dropping `.claude.json` loses every MCP server, dropping
/// `projects` strands maiLink's transcript tail, dropping `ide` loses lockfile discovery.
/// Additions belong in the doc's table first — a missing entry fails silently, which is the
/// whole reason that table exists.
pub const SHARED_ENTRIES: &[&str] = &[
    "settings.json",
    "settings.local.json",
    ".claude.json",
    "projects",
    "ide",
    "commands",
    "skills",
    "plugins",
    "CLAUDE.md",
    "statusline-command.sh",
];

/// Never symlink this, on any platform. On Linux it is the credential store and Claude Code
/// refuses to follow a symlink for it (`O_NOFOLLOW` on read and write, plus an `lstat` +
/// `is_symlink` rejection). A hard link is no better: the write is temp-file-plus-rename, so the
/// first refresh diverges the inodes. Isolating it is the point of the split — see §5.4.
pub const NEVER_LINK: &str = ".credentials.json";

/// Where an entry lives under the user's home. `.claude.json` is the odd one out: it sits at the
/// home root rather than inside `~/.claude`, which is exactly why it is easy to miss that
/// `CLAUDE_CONFIG_DIR` relocates it too.
fn source_for(entry: &str, home: &Path) -> PathBuf {
    if entry == ".claude.json" {
        home.join(".claude.json")
    } else {
        home.join(".claude").join(entry)
    }
}

/// `<data>/<slug>/logins` — the parent of every identity root.
pub fn identities_dir() -> Option<PathBuf> {
    dirs::data_dir().map(|p| p.join(app_data_slug()).join("logins"))
}

/// The `CLAUDE_CONFIG_DIR` for one identity.
pub fn identity_root(identity_id: &str) -> Option<PathBuf> {
    identities_dir().map(|p| p.join(identity_id))
}

/// What `reconcile` did, for logging and for surfacing drift in the UI.
#[derive(Debug, Default, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Reconciled {
    /// Links created or repointed this pass.
    pub linked: Vec<String>,
    /// Entries skipped because the user has no such file — not an error.
    pub absent: Vec<String>,
    /// Real files found where a symlink belongs, renamed aside rather than deleted.
    pub displaced: Vec<String>,
}

/// Create or repair an identity root.
///
/// Deliberately a reconciler, not a one-shot: the user installs skills and plugins after an
/// identity exists, and `~/.claude` gains entries across Claude Code versions, so this runs on
/// every spawn. Idempotent.
///
/// Drift is repaired **non-destructively**. A real file where a symlink belongs is renamed aside,
/// never removed — it may be the only copy of something, and this module is not entitled to that
/// call.
pub fn reconcile(identity_id: &str, home: &Path) -> Result<Reconciled, String> {
    let root = identity_root(identity_id)
        .ok_or_else(|| "no data directory available".to_string())?;
    reconcile_at(&root, home)
}

/// `reconcile`, against an explicit root. Split out so tests need no real data directory.
pub fn reconcile_at(root: &Path, home: &Path) -> Result<Reconciled, String> {
    fs::create_dir_all(root).map_err(|e| format!("creating {}: {e}", root.display()))?;

    let mut out = Reconciled::default();

    for entry in SHARED_ENTRIES {
        let source = source_for(entry, home);
        if !source.exists() {
            out.absent.push((*entry).to_string());
            continue;
        }

        let link = root.join(entry);

        // Already correct? Compare the link target, not the resolved path — a resolved
        // comparison would call a correct link "wrong" whenever the source is itself a symlink.
        match fs::read_link(&link) {
            Ok(target) if target == source => continue,
            Ok(_) => {
                fs::remove_file(&link)
                    .map_err(|e| format!("removing stale link {}: {e}", link.display()))?;
            }
            Err(_) if link.exists() => {
                // A real file or directory. Move it aside; do not delete.
                let aside = displaced_name(&link);
                fs::rename(&link, &aside)
                    .map_err(|e| format!("displacing {}: {e}", link.display()))?;
                out.displaced.push((*entry).to_string());
            }
            Err(_) => {}
        }

        symlink(&source, &link)
            .map_err(|e| format!("linking {} -> {}: {e}", link.display(), source.display()))?;
        out.linked.push((*entry).to_string());
    }

    Ok(out)
}

/// A collision-free name to park a displaced real file under, beside where it was.
fn displaced_name(link: &Path) -> PathBuf {
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let name = link.file_name().and_then(|n| n.to_str()).unwrap_or("entry");
    let mut candidate = link.with_file_name(format!("{name}.displaced-{stamp}"));
    let mut n = 1;
    while candidate.exists() {
        candidate = link.with_file_name(format!("{name}.displaced-{stamp}-{n}"));
        n += 1;
    }
    candidate
}

#[cfg(unix)]
fn symlink(source: &Path, link: &Path) -> std::io::Result<()> {
    std::os::unix::fs::symlink(source, link)
}

#[cfg(windows)]
fn symlink(source: &Path, link: &Path) -> std::io::Result<()> {
    if source.is_dir() {
        std::os::windows::fs::symlink_dir(source, link)
    } else {
        std::os::windows::fs::symlink_file(source, link)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    /// A home directory with the entries a real user would have.
    fn fake_home(base: &Path) -> PathBuf {
        let home = base.join("home");
        fs::create_dir_all(home.join(".claude")).unwrap();
        fs::write(home.join(".claude").join("settings.json"), "{}").unwrap();
        fs::write(home.join(".claude.json"), "{}").unwrap();
        fs::create_dir_all(home.join(".claude").join("projects")).unwrap();
        fs::create_dir_all(home.join(".claude").join("ide")).unwrap();
        home
    }

    fn tmp() -> PathBuf {
        let p = std::env::temp_dir().join(format!(
            "maiterm-login-test-{}-{:?}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn credentials_are_never_in_the_shared_set() {
        // Guards the one entry that would break identity isolation AND be refused at the
        // syscall level. If someone adds it, this is where they find out.
        assert!(!SHARED_ENTRIES.contains(&NEVER_LINK));
    }

    #[test]
    fn links_the_entries_that_exist_and_reports_the_rest() {
        let base = tmp();
        let home = fake_home(&base);
        let root = base.join("identity-a");

        let r = reconcile_at(&root, &home).unwrap();

        for e in ["settings.json", ".claude.json", "projects", "ide"] {
            assert!(r.linked.contains(&e.to_string()), "expected {e} linked, got {r:?}");
            assert_eq!(fs::read_link(root.join(e)).unwrap(), source_for(e, &home));
        }
        // The user has no skills/ or plugins/ — absent, not an error.
        assert!(r.absent.contains(&"skills".to_string()));
        assert!(r.displaced.is_empty());

        fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn is_idempotent() {
        let base = tmp();
        let home = fake_home(&base);
        let root = base.join("identity-a");

        reconcile_at(&root, &home).unwrap();
        let second = reconcile_at(&root, &home).unwrap();

        // Everything was already correct, so nothing is relinked on the second pass.
        assert!(second.linked.is_empty(), "expected no relinking, got {second:?}");
        assert!(second.displaced.is_empty());

        fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn repoints_a_link_aimed_somewhere_else() {
        let base = tmp();
        let home = fake_home(&base);
        let root = base.join("identity-a");
        fs::create_dir_all(&root).unwrap();
        let wrong = base.join("wrong.json");
        fs::write(&wrong, "{}").unwrap();
        symlink(&wrong, &root.join("settings.json")).unwrap();

        let r = reconcile_at(&root, &home).unwrap();

        assert!(r.linked.contains(&"settings.json".to_string()));
        assert_eq!(
            fs::read_link(root.join("settings.json")).unwrap(),
            source_for("settings.json", &home)
        );
        fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn displaces_a_real_file_instead_of_deleting_it() {
        let base = tmp();
        let home = fake_home(&base);
        let root = base.join("identity-a");
        fs::create_dir_all(&root).unwrap();
        // Claude Code wrote its own .claude.json here before the farm was built — this is the
        // drift case, and it may be the only copy of that state.
        fs::write(root.join(".claude.json"), "irreplaceable").unwrap();

        let r = reconcile_at(&root, &home).unwrap();

        assert!(r.displaced.contains(&".claude.json".to_string()));
        assert_eq!(
            fs::read_link(root.join(".claude.json")).unwrap(),
            source_for(".claude.json", &home)
        );
        let parked: Vec<_> = fs::read_dir(&root)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().starts_with(".claude.json.displaced-"))
            .collect();
        assert_eq!(parked.len(), 1, "the real file must survive, parked aside");
        assert_eq!(fs::read_to_string(parked[0].path()).unwrap(), "irreplaceable");

        fs::remove_dir_all(&base).ok();
    }
}
