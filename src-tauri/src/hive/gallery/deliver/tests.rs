//! Delivery targets, names and the exclusive writes, on scratch folders.

use std::fs;
use std::process::Command;

use super::*;
use crate::hive::record::{HiveFolder, KIND_GIT};

struct Scratch(PathBuf);

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn scratch(tag: &str) -> Scratch {
    let t = std::env::temp_dir().join(format!("hive-gallery-deliver-{tag}-{}", uuid::Uuid::new_v4()));
    fs::create_dir_all(&t).unwrap();
    Scratch(canon(&t))
}

fn folder(name: &str, p: &Path, kind: &str) -> HiveFolder {
    HiveFolder { name: name.into(), path: p.to_string_lossy().into(), kind: kind.into(), ..HiveFolder::default() }
}

fn hive(folders: Vec<HiveFolder>) -> Hive {
    Hive { id: "h-deliver".into(), folders, ..Hive::default() }
}

fn git(dir: &Path, args: &[&str]) {
    let out = Command::new("git").current_dir(dir).env("GIT_CONFIG_GLOBAL", "/dev/null").env("GIT_CONFIG_NOSYSTEM", "1").args(args).output().unwrap();
    assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
}

fn same(a: &Path, b: &Path) -> bool {
    a.to_string_lossy().replace('\\', "/") == b.to_string_lossy().replace('\\', "/")
}

#[test]
fn a_plain_folder_or_an_existing_subfolder_is_a_target_and_nothing_else_is() {
    let s = scratch("target");
    let (brand, brand2, repo) = (s.0.join("brand"), s.0.join("brand2"), s.0.join("repo"));
    for d in [brand.join("icons"), brand.join(".claude").join("commands"), brand2.clone(), repo.join("assets"), repo.join("src")] {
        fs::create_dir_all(&d).unwrap();
    }
    git(&repo, &["init", "-q"]);
    git(&repo, &["config", "core.autocrlf", "false"]);
    let h = hive(vec![folder("brand", &brand, KIND_PLAIN), folder("site", &repo, KIND_GIT), folder("inrepo", &repo.join("assets"), KIND_PLAIN)]);

    let (dir, name) = resolve_target(&h, "brand").unwrap();
    assert!(same(&dir, &brand) && name == "brand", "{dir:?}");
    let (dir, _) = resolve_target(&h, "brand/icons").unwrap();
    assert!(same(&dir, &brand.join("icons")));
    let (dir, name) = resolve_target(&h, &brand.join("icons").to_string_lossy()).unwrap();
    assert!(same(&dir, &brand.join("icons")) && name == "brand", "an absolute path inside one");

    for (target, says) in [
        // A git folder of the Hive, by name or by a path into it: the refusal names the reviewed route.
        ("site".to_string(), "land them"),
        (repo.join("src").to_string_lossy().to_string(), "land them"),
        ("nothing".to_string(), "No plain folder called \"nothing\""),
        ("inrepo".to_string(), "inside a git work tree"),
        // Agent config below a plain folder: `.claude/commands` is read by every Hive agent.
        ("brand/.claude/commands".to_string(), "hidden folder"),
        (brand.join(".claude").to_string_lossy().to_string(), "hidden folder"),
        (repo.join("assets").to_string_lossy().to_string(), "inside a git work tree"),
        ("brand/missing".to_string(), "doesn't make folders"),
        ("brand/../brand2".to_string(), "no \"..\""),
        (brand2.to_string_lossy().to_string(), "isn't in one of this Hive's plain folders"),
        (brand.join("icons").join("..").join("..").join("brand2").to_string_lossy().to_string(), "isn't in one of this Hive's plain folders"),
        (String::new(), "folder is required"),
    ] {
        let e = resolve_target(&h, &target).unwrap_err();
        assert!(e.contains(says), "{target}: {e}");
    }
    // A `.git` file (a worktree's or a submodule's) counts as much as a folder.
    fs::write(brand.join("icons").join(".git"), "gitdir: elsewhere").unwrap();
    assert!(resolve_target(&h, "brand/icons").unwrap_err().contains("git work tree"));
    assert!(resolve_target(&h, "brand").is_ok(), "only at or above the target");
}

#[cfg(unix)]
#[test]
fn a_subfolder_through_a_link_that_leads_out_is_refused() {
    let s = scratch("link");
    let (brand, outside) = (s.0.join("brand"), s.0.join("outside"));
    fs::create_dir_all(&brand).unwrap();
    fs::create_dir_all(&outside).unwrap();
    std::os::unix::fs::symlink(&outside, brand.join("out")).unwrap();
    fs::create_dir_all(brand.join("real")).unwrap();
    std::os::unix::fs::symlink(brand.join("real"), brand.join("inside")).unwrap();
    let h = hive(vec![folder("brand", &brand, KIND_PLAIN)]);
    assert!(resolve_target(&h, "brand/out").unwrap_err().contains("isn't in one of this Hive's plain folders"));
    let (dir, _) = resolve_target(&h, "brand/inside").unwrap();
    assert!(same(&dir, &brand.join("real")), "a link that stays inside resolves to where it points");
}

/// §8.1's config never arrives by delivery: Claude loads a plain folder's CLAUDE.md as project
/// instructions in every Hive agent (each plain folder is an `--add-dir`).
#[test]
fn agent_and_build_config_names_are_refused_in_any_case() {
    let s = scratch("config");
    let brand = s.0.join("brand");
    fs::create_dir_all(brand.join("scripts")).unwrap();
    let mut h = hive(vec![folder("brand", &brand, KIND_PLAIN)]);
    h.settings.land.config_paths = vec!["scripts/**".into()];
    for bad in ["CLAUDE.md", "claude.md", "AGENTS.md", "CLAUDE.local.md", "package.json", "Build.rs"] {
        let e = check_config_names(&h, &brand, &["ok.png".into(), bad.into()]).unwrap_err();
        assert!(e.contains("agent or build config") && e.contains("land it"), "{bad}: {e}");
    }
    assert!(check_config_names(&h, &brand.join("scripts"), &["run.txt".into()]).is_err(), "the human's own patterns, by the path under the plain folder");
    assert!(check_config_names(&h, &brand, &["run.txt".into(), "claude-notes.md".into()]).is_ok());
}

#[test]
fn names_are_one_plain_component_each() {
    assert!(check_names(&["hero.png".into(), "Logo v2 (final).PNG".into()]).is_ok());
    for (bad, says) in [
        ("a/b", "no folders"),
        ("a\\b", "no folders"),
        ("..", "no folders"),
        (".env", "starts with a dot"),
        ("", "empty"),
        ("  ", "empty"),
        ("a\nb", "usable"),
        // Shows as "herolanimret.png" in Finder: a bidi override hides the real extension.
        ("hero\u{202E}gnp.terminal", "usable"),
        ("hero\u{200B}.png", "usable"),
        ("hero\u{2066}.png", "usable"),
        ("hero\u{FEFF}.png", "usable"),
    ] {
        let e = check_names(&[bad.to_string()]).unwrap_err();
        assert!(e.contains(says), "{bad:?}: {e}");
    }
    assert!(check_names(&["x".repeat(MAX_NAME_BYTES + 1)]).is_err());
    assert!(check_names(&["Hero.png".into(), "hero.PNG".into()]).unwrap_err().contains("named twice"), "most volumes ignore case");

    let a = |title: &str, file: &str| Asset { title: title.into(), file: file.into(), ..Asset::default() };
    assert_eq!(slug("  Hero Shot: Final!! "), "hero-shot-final");
    assert_eq!(slug("..."), "asset");
    assert_eq!(derived_names(&[a("Hero", "a-1.png"), a("Hero", "a-2")], false), vec!["hero.png", "hero"]);
    // Unique within one batch, so Replace never writes one of its own files over another.
    assert_eq!(derived_names(&[a("Hero", "a.png"), a("hero!", "b.PNG"), a("Hero", "c.png")], false), vec!["hero.png", "hero (2).PNG", "hero (3).png"]);
    let many: Vec<Asset> = (0..100).map(|i| a(&format!("t{i}"), "x.jpg")).collect();
    assert_eq!(derived_names(&many, true)[0], "001-t0.jpg", "as wide as the count needs");
    assert_eq!(derived_names(&many[..3], true), vec!["01-t0.jpg", "02-t1.jpg", "03-t2.jpg"]);
    assert_eq!(given_names(&[a("x", "a.png")], &["icon".into()]).unwrap(), vec!["icon.png"]);
    assert_eq!(given_names(&[a("x", "a.png")], &["icon.webp".into()]).unwrap(), vec!["icon.webp"], "a name's own extension stands");
    assert_eq!((suffixed("hero.png", 1), suffixed("hero.png", 2), suffixed("README", 3), suffixed(".png", 2)), ("hero.png".into(), "hero (2).png".into(), "README (3)".into(), ".png (2)".into()));
}

/// A stored file as the gallery keeps it: read-only.
fn stored(dir: &Path, name: &str, bytes: &str) -> PathBuf {
    fs::create_dir_all(dir).unwrap();
    let p = dir.join(name);
    fs::write(&p, bytes).unwrap();
    let mut perm = fs::metadata(&p).unwrap().permissions();
    perm.set_readonly(true);
    fs::set_permissions(&p, perm).unwrap();
    p
}

#[test]
fn a_taken_name_gets_a_suffix_and_the_original_stays_byte_identical() {
    let s = scratch("write");
    let (files, out) = (s.0.join("files"), s.0.join("out"));
    fs::create_dir_all(&out).unwrap();
    let src = stored(&files, "a-000000000001.png", "new bytes");
    fs::write(out.join("hero.png"), "the human's").unwrap();
    let h = hive(vec![folder("out", &out, KIND_PLAIN)]);
    let (dir, _) = resolve_target(&h, "out").unwrap();
    // A name that appears between the check and the write: an exclusive create, so a suffix.
    fs::write(out.join("logo.png"), "appeared meanwhile").unwrap();
    let written = deliver_files(&dir, &[(src.clone(), "hero.png".into()), (src.clone(), "logo.png".into()), (src.clone(), "hero.png".into())]).unwrap();
    assert_eq!(written, vec!["hero (2).png", "logo (2).png", "hero (3).png"]);
    assert_eq!(fs::read_to_string(out.join("hero.png")).unwrap(), "the human's");
    assert_eq!(fs::read_to_string(out.join("logo.png")).unwrap(), "appeared meanwhile");
    assert_eq!(fs::read_to_string(out.join("hero (3).png")).unwrap(), "new bytes");
    assert!(!fs::metadata(out.join("hero (2).png")).unwrap().permissions().readonly(), "a delivered copy isn't the gallery's read-only one");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(fs::metadata(out.join("logo (2).png")).unwrap().permissions().mode() & 0o777, 0o644);
    }
    for n in 2..=MAX_SUFFIX {
        fs::write(out.join(suffixed("full.png", n)), "x").unwrap();
    }
    fs::write(out.join("full.png"), "x").unwrap();
    assert!(deliver_files(&dir, &[(src, "full.png".into())]).unwrap_err().error.contains("all taken"));
}

#[cfg(unix)]
#[test]
fn a_target_swapped_for_a_link_after_the_check_is_refused() {
    let s = scratch("swap");
    let (out, elsewhere) = (s.0.join("out"), s.0.join("elsewhere"));
    fs::create_dir_all(&out).unwrap();
    fs::create_dir_all(&elsewhere).unwrap();
    let src = stored(&s.0.join("files"), "a.png", "x");
    let h = hive(vec![folder("out", &out, KIND_PLAIN)]);
    let (dir, _) = resolve_target(&h, "out").unwrap();
    fs::remove_dir(&out).unwrap();
    std::os::unix::fs::symlink(&elsewhere, &out).unwrap();
    assert!(deliver_files(&dir, &[(src, "a.png".into())]).is_err());
    assert_eq!(fs::read_dir(&elsewhere).unwrap().count(), 0, "nothing written through the link");
}

/// A folder ABOVE the target swapped for a link: the open follows it (O_NOFOLLOW covers the last
/// part only), so the descriptor's path must be held to the target as checked, not re-resolved.
#[cfg(unix)]
#[test]
fn a_folder_above_the_target_swapped_for_a_link_is_refused() {
    let s = scratch("swap-above");
    let (out, repo) = (s.0.join("out"), s.0.join("repo"));
    fs::create_dir_all(out.join("a").join("b")).unwrap();
    fs::create_dir_all(repo.join("src").join("b")).unwrap();
    let src = stored(&s.0.join("files"), "a.png", "x");
    let h = hive(vec![folder("out", &out, KIND_PLAIN)]);
    let (dir, _) = resolve_target(&h, "out/a/b").unwrap();
    fs::rename(out.join("a"), s.0.join("moved")).unwrap();
    std::os::unix::fs::symlink(repo.join("src"), out.join("a")).unwrap();
    for run in [deliver_files(&dir, &[(src.clone(), "a.png".into())]), export_replacing(&dir, &[(src.clone(), "a.png".into())])] {
        assert!(run.unwrap_err().error.contains("changed while it was being checked"));
    }
    assert_eq!(fs::read_dir(repo.join("src").join("b")).unwrap().count(), 0, "nothing written through the link");
}

/// A run that stops partway keeps what it wrote, so the log keeps it too.
#[test]
fn a_run_that_stops_partway_logs_what_it_wrote() {
    let s = scratch("partial");
    let out = s.0.join("out");
    fs::create_dir_all(&out).unwrap();
    let src = stored(&s.0.join("files"), "a.png", "x");
    let paths = HivePaths::new(s.0.join("hive"));
    let a = |id: &str| Asset { id: id.into(), ..Asset::default() };
    let assets = [a("a-000000000001"), a("a-000000000002")];
    let run = deliver_files(&out, &[(src, "one.png".into()), (s.0.join("files").join("gone.png"), "two.png".into())]);
    let e = log_run(&paths, "coordinator", &out, &assets, run).unwrap_err();
    assert!(e.contains("written before it: one.png"), "{e}");
    let text = fs::read_to_string(paths.delivery_log()).unwrap();
    let line: serde_json::Value = serde_json::from_str(text.lines().next().unwrap()).unwrap();
    assert_eq!(line["items"], serde_json::json!([{ "asset": "a-000000000001", "name": "one.png" }]));
    // Nothing written, no line.
    assert!(log_run(&paths, "coordinator", &out, &assets, Err(Stopped::from("no".to_string()))).is_err());
    assert_eq!(fs::read_to_string(paths.delivery_log()).unwrap().lines().count(), 1);
}

#[test]
fn export_with_replace_overwrites_through_a_rename_and_without_it_adds_a_suffix() {
    let s = scratch("export");
    let out = s.0.join("anywhere");
    fs::create_dir_all(&out).unwrap();
    let src = stored(&s.0.join("files"), "a.png", "fresh");
    fs::write(out.join("hero.png"), "old").unwrap();
    assert_eq!(export_replacing(&out, &[(src.clone(), "hero.png".into()), (src.clone(), "new.png".into())]).unwrap(), vec!["hero.png", "new.png"]);
    assert_eq!(fs::read_to_string(out.join("hero.png")).unwrap(), "fresh");
    assert!(!fs::metadata(out.join("hero.png")).unwrap().permissions().readonly());
    let left: Vec<String> = fs::read_dir(&out).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().to_string()).collect();
    assert!(left.iter().all(|n| !n.ends_with(TMP_TAIL)), "no temp file left: {left:?}");
    // A crash's leftover temp is replaced, not a refusal.
    fs::write(out.join(format!(".hero.png{TMP_TAIL}")), "stale").unwrap();
    export_replacing(&out, &[(src.clone(), "hero.png".into())]).unwrap();
    assert!(!out.join(format!(".hero.png{TMP_TAIL}")).exists());
    assert_eq!(deliver_files(&out, &[(src, "hero.png".into())]).unwrap(), vec!["hero (2).png"]);
    assert_eq!(fs::read_to_string(out.join("hero.png")).unwrap(), "fresh");
}

#[test]
fn each_delivery_is_one_log_line() {
    let s = scratch("log");
    let paths = HivePaths::new(s.0.join("hive"));
    log_delivery(&paths, "coordinator", Path::new("/r/brand"), &[DeliveryItem { asset: "a-000000000001".into(), name: "hero.png".into() }]);
    log_delivery(&paths, "human", Path::new("/x"), &[]);
    let text = fs::read_to_string(paths.delivery_log()).unwrap();
    let lines: Vec<serde_json::Value> = text.lines().map(|l| serde_json::from_str(l).unwrap()).collect();
    assert_eq!(lines.len(), 2);
    assert_eq!((lines[0]["by"].as_str(), lines[0]["folder"].as_str(), lines[0]["items"][0]["name"].as_str()), (Some("coordinator"), Some("/r/brand"), Some("hero.png")));
    assert!(lines[0]["at"].as_str().is_some_and(|a| !a.is_empty()));
}
