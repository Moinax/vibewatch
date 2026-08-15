use anyhow::{Context, Result};
use serde_json::Value;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

pub struct Options {
    pub no_service: bool,
    pub no_hooks: bool,
    pub dry_run: bool,
    pub uninstall: bool,
}

fn settings_path() -> PathBuf {
    if let Ok(dir) = std::env::var("CLAUDE_CONFIG_DIR") {
        PathBuf::from(dir).join("settings.json")
    } else {
        dirs::home_dir()
            .expect("HOME must resolve")
            .join(".claude")
            .join("settings.json")
    }
}

fn read_json(path: &Path) -> Result<Value> {
    let contents =
        fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    serde_json::from_str(&contents).with_context(|| format!("parsing {}", path.display()))
}

fn write_json(path: &Path, value: &Value) -> Result<()> {
    let mut out = serde_json::to_string_pretty(value)?;
    out.push('\n');
    fs::write(path, out).with_context(|| format!("writing {}", path.display()))
}

/// Like every `apply_*_install`, returns whether it changed (or would change)
/// anything.
pub fn apply_hooks_merge(path: &Path, dry_run: bool) -> Result<bool> {
    if !path.exists() {
        eprintln!(
            "vibewatch install: {} does not exist yet; skipping hook merge. \
             Run vibewatch install again after Claude Code creates it.",
            path.display()
        );
        return Ok(false);
    }
    let original = read_json(path)?;
    let merged = merge_hooks(original.clone());
    if merged == original {
        return Ok(false);
    }
    if dry_run {
        eprintln!(
            "vibewatch install: [dry-run] would merge hooks into {}",
            path.display()
        );
        return Ok(true);
    }
    write_json(path, &merged)?;
    eprintln!("vibewatch install: merged hooks into {}", path.display());
    Ok(true)
}

pub fn apply_hooks_unmerge(path: &Path, dry_run: bool) -> Result<()> {
    if !path.exists() {
        return Ok(());
    }
    let original = read_json(path)?;
    let stripped = unmerge_hooks(original.clone());
    if stripped == original {
        return Ok(());
    }
    if dry_run {
        eprintln!(
            "vibewatch install: [dry-run] would remove vibewatch hooks from {}",
            path.display()
        );
        return Ok(());
    }
    write_json(path, &stripped)?;
    eprintln!(
        "vibewatch install: removed vibewatch hooks from {}",
        path.display()
    );
    Ok(())
}

pub fn run(opts: Options) -> Result<()> {
    let path = settings_path();
    if opts.uninstall {
        if !opts.no_hooks {
            apply_hooks_unmerge(&path, opts.dry_run)?;
        }
        if !opts.no_service {
            apply_service_uninstall(opts.dry_run)?;
        }
        apply_logos_uninstall(opts.dry_run)?;
        apply_waybar_uninstall(opts.dry_run)?;
        eprintln!("vibewatch install: uninstall complete");
    } else {
        // Separate bindings, never one `||` chain: every step has to run, and
        // `||` would short-circuit the rest as soon as one reported a change.
        let service = !opts.no_service && apply_service_install(opts.dry_run)?;
        let hooks = !opts.no_hooks && apply_hooks_merge(&path, opts.dry_run)?;
        let waybar = apply_waybar_install(opts.dry_run)?;
        let logos = apply_logos_install(opts.dry_run)?;
        // A re-run that changed nothing is already installed: the manual steps
        // are a first-install checklist, not a status report.
        if !(service || hooks || waybar || logos) {
            // "nothing left to set up", not "nothing to do": the service step
            // can still have started a stopped unit on the way here.
            eprintln!("vibewatch install: already installed, nothing left to set up");
        } else if !opts.dry_run {
            print_manual_steps();
        }
    }
    Ok(())
}

/// Every Claude Code hook event vibewatch registers. The tuple is
/// `(PascalCase event name, notify subcommand slug, async flag)`. The
/// slug must match the `vibewatch notify` subcommand arms in
/// `src/notify.rs`. The synchronous entry (PermissionRequest) is what
/// powers the widget approve/deny + AskUserQuestion flows.
pub(crate) const HOOK_EVENTS: [(&str, &str, bool); 7] = [
    ("SessionStart", "session-start", true),
    ("UserPromptSubmit", "user-prompt-submit", true),
    ("PreToolUse", "pre-tool-use", true),
    ("PostToolUse", "post-tool-use", true),
    ("PermissionRequest", "permission-request", false),
    ("Stop", "stop", true),
    // Sub-agent completions arrive on their own event, never as a `Stop` — so
    // without this entry a session that launches sub-agents counts them up and
    // never back down, and its finishes stay held and silent.
    ("SubagentStop", "subagent-stop", true),
];

/// Canonical hook command for a given `vibewatch notify` slug.
pub(crate) fn command_for(slug: &str) -> String {
    format!("~/.cargo/bin/vibewatch notify {} --agent claude-code", slug)
}

/// Merge vibewatch's hook entries into a parsed settings.json value.
/// Idempotent: re-running on an already-merged value returns an equal Value.
pub fn merge_hooks(mut settings: Value) -> Value {
    let hooks = settings
        .as_object_mut()
        .expect("settings.json root must be a JSON object")
        .entry("hooks")
        .or_insert_with(|| serde_json::json!({}));
    let hooks_obj = hooks
        .as_object_mut()
        .expect("settings.json \"hooks\" key must be a JSON object");

    for (event, slug, async_flag) in HOOK_EVENTS {
        let command = command_for(slug);
        let entry = hooks_obj
            .entry(event)
            .or_insert_with(|| serde_json::json!([]));
        let array = entry
            .as_array_mut()
            .expect("settings.json hooks.<event> must be an array");

        // Find or create the matcher-"" group.
        let group_idx = array
            .iter()
            .position(|g| g.get("matcher").and_then(|m| m.as_str()) == Some(""));
        let group = match group_idx {
            Some(idx) => &mut array[idx],
            None => {
                array.push(serde_json::json!({ "matcher": "", "hooks": [] }));
                array.last_mut().unwrap()
            }
        };

        let group_hooks = group
            .get_mut("hooks")
            .and_then(|v| v.as_array_mut())
            .expect("settings.json hooks.<event>[*].hooks must be an array");

        let already_present = group_hooks
            .iter()
            .any(|h| h.get("command").and_then(|c| c.as_str()) == Some(command.as_str()));
        if !already_present {
            let mut hook_entry = serde_json::json!({
                "type": "command",
                "command": command,
            });
            if async_flag {
                hook_entry
                    .as_object_mut()
                    .unwrap()
                    .insert("async".to_string(), Value::Bool(true));
            }
            group_hooks.push(hook_entry);
        }
    }

    settings
}

/// Remove vibewatch's hook entries (anything whose command string contains
/// "vibewatch"). Other tools' hooks in the same event array are preserved.
/// Only inspects the seven known HOOK_EVENTS keys; vibewatch commands nested
/// under non-standard event names are left alone.
pub fn unmerge_hooks(mut settings: Value) -> Value {
    let Some(hooks_obj) = settings
        .as_object_mut()
        .and_then(|o| o.get_mut("hooks"))
        .and_then(|v| v.as_object_mut())
    else {
        return settings;
    };

    for (event, _, _) in HOOK_EVENTS {
        let Some(entry) = hooks_obj.get_mut(event) else {
            continue;
        };
        let Some(array) = entry.as_array_mut() else {
            continue;
        };

        for group in array.iter_mut() {
            if let Some(group_hooks) = group.get_mut("hooks").and_then(|v| v.as_array_mut()) {
                group_hooks.retain(|h| {
                    h.get("command")
                        .and_then(|c| c.as_str())
                        .map(|s| !s.contains("vibewatch"))
                        .unwrap_or(true)
                });
            }
        }

        // Drop now-empty matcher groups.
        array.retain(|g| {
            g.get("hooks")
                .and_then(|h| h.as_array())
                .map(|a| !a.is_empty())
                .unwrap_or(false)
        });

        if array.is_empty() {
            hooks_obj.remove(event);
        }
    }

    settings
}

const SERVICE_NAME: &str = "vibewatch.service";
const SERVICE_BODY: &str = include_str!("../contrib/vibewatch.service");

fn systemd_unit_path() -> PathBuf {
    dirs::config_dir()
        .expect("XDG_CONFIG_HOME or ~/.config must resolve")
        .join("systemd")
        .join("user")
        .join(SERVICE_NAME)
}

/// `systemctl <args>` exited 0. `.output()` rather than `.status()` so the
/// queries' `enabled` / `active` stdout doesn't leak onto the terminal.
fn systemctl_ok(args: &[&str]) -> bool {
    Command::new("systemctl")
        .args(args)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn has_systemctl() -> bool {
    systemctl_ok(&["--version"])
}

pub fn apply_service_install(dry_run: bool) -> Result<bool> {
    if !has_systemctl() {
        eprintln!("vibewatch install: systemctl not found; skipping service install");
        return Ok(false);
    }
    let path = systemd_unit_path();
    let current = fs::read_to_string(&path).unwrap_or_default();
    let needs_write = current != SERVICE_BODY;
    // Skip the work when the unit is already up: `daemon-reload` re-parses every
    // user unit (~600ms) and `enable --now` is a full bus transaction even as a
    // no-op (~300ms), against ~5ms for each read-only query.
    if !needs_write
        && systemctl_ok(&["--user", "is-enabled", SERVICE_NAME])
        && systemctl_ok(&["--user", "is-active", SERVICE_NAME])
    {
        return Ok(false);
    }
    if dry_run {
        if needs_write {
            eprintln!(
                "vibewatch install: [dry-run] would write {}",
                path.display()
            );
        }
        eprintln!(
            "vibewatch install: [dry-run] would enable --now {}",
            SERVICE_NAME
        );
        return Ok(true);
    }
    if needs_write {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(&path, SERVICE_BODY).with_context(|| format!("writing {}", path.display()))?;
        eprintln!("vibewatch install: wrote {}", path.display());
    }
    let _ = Command::new("systemctl")
        .args(["--user", "daemon-reload"])
        .status();
    match Command::new("systemctl")
        .args(["--user", "enable", "--now", SERVICE_NAME])
        .status()
    {
        Ok(s) if s.success() => {
            eprintln!("vibewatch install: enabled & started {}", SERVICE_NAME);
        }
        Ok(s) => eprintln!(
            "vibewatch install: systemctl enable --now exited with {}",
            s
        ),
        Err(e) => eprintln!("vibewatch install: systemctl enable --now failed: {e}"),
    }
    // Only the unit file counts as a change, never the enable: a machine whose
    // service is stopped, masked, or has no user bus falls through the guard
    // above on *every* run, and reporting that as a change would re-print the
    // first-install checklist forever — on exactly the machines it least suits.
    Ok(needs_write)
}

pub fn apply_service_uninstall(dry_run: bool) -> Result<()> {
    if !has_systemctl() {
        return Ok(());
    }
    if dry_run {
        eprintln!(
            "vibewatch install: [dry-run] would disable+stop {} and remove the unit file",
            SERVICE_NAME
        );
        return Ok(());
    }
    let _ = Command::new("systemctl")
        .args(["--user", "disable", "--now", SERVICE_NAME])
        .status();
    let path = systemd_unit_path();
    if path.exists() {
        fs::remove_file(&path)?;
        eprintln!("vibewatch install: removed {}", path.display());
    }
    let _ = Command::new("systemctl")
        .args(["--user", "daemon-reload"])
        .status();
    Ok(())
}

const WAYBAR_SNIPPET: &str = include_str!("../contrib/waybar-module.jsonc");

fn waybar_snippet_path() -> PathBuf {
    dirs::config_dir()
        .expect("XDG_CONFIG_HOME or ~/.config must resolve")
        .join("vibewatch")
        .join("waybar-module.jsonc")
}

pub fn apply_waybar_install(dry_run: bool) -> Result<bool> {
    let path = waybar_snippet_path();
    let current = fs::read_to_string(&path).unwrap_or_default();
    if current == WAYBAR_SNIPPET {
        return Ok(false);
    }
    if dry_run {
        eprintln!("vibewatch install: [dry-run] would drop {}", path.display());
        return Ok(true);
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(&path, WAYBAR_SNIPPET)?;
    eprintln!("vibewatch install: wrote {}", path.display());
    Ok(true)
}

/// The agent marks the widget wears, keyed by the `logo-*` class that selects
/// them (see `AgentKind::logo_class`). They have to exist as files rather than
/// travel inside the binary: GTK resolves a `background-image` URL off the
/// disk, and the stylesheet that names them is the user's, not ours.
pub(crate) const LOGOS: [(&str, &str); 5] = [
    (
        "vibewatch.svg",
        include_str!("../assets/logos/vibewatch.svg"),
    ),
    ("claude.svg", include_str!("../assets/logos/claude.svg")),
    ("codex.svg", include_str!("../assets/logos/codex.svg")),
    ("cursor.svg", include_str!("../assets/logos/cursor.svg")),
    ("webstorm.svg", include_str!("../assets/logos/webstorm.svg")),
];

/// The stylesheet that maps those classes onto those files. Installed beside
/// them so its relative `url("logos/…")` resolves — GTK rebases URLs in an
/// imported sheet onto that sheet's own directory, so the user's waybar CSS
/// needs nothing but a one-line `@import` and never learns where the marks live.
const LOGO_CSS: &str = include_str!("../contrib/waybar-logos.css");

fn vibewatch_config_dir() -> PathBuf {
    waybar_snippet_path()
        .parent()
        .expect("the snippet path always has a parent")
        .to_path_buf()
}

fn logo_css_path() -> PathBuf {
    vibewatch_config_dir().join("logos.css")
}

fn logos_dir() -> PathBuf {
    vibewatch_config_dir().join("logos")
}

/// Drop the marks next to the waybar snippet, in `~/.config/vibewatch/logos`.
/// That location is load-bearing: it sits one directory over from
/// `~/.config/waybar/style*.css`, so the stylesheet can reach them with a
/// relative `url("../vibewatch/logos/claude.svg")` and needs no absolute home
/// path baked into it.
pub fn apply_logos_install(dry_run: bool) -> Result<bool> {
    let dir = logos_dir();
    let css = logo_css_path();
    let stale: Vec<&(&str, &str)> = LOGOS
        .iter()
        .filter(|(name, body)| fs::read_to_string(dir.join(name)).unwrap_or_default() != *body)
        .collect();
    let css_stale = fs::read_to_string(&css).unwrap_or_default() != LOGO_CSS;
    if stale.is_empty() && !css_stale {
        return Ok(false);
    }
    if dry_run {
        eprintln!(
            "vibewatch install: [dry-run] would write {} mark(s) into {} (stylesheet: {})",
            stale.len(),
            dir.display(),
            if css_stale { "updated" } else { "current" },
        );
        return Ok(true);
    }
    fs::create_dir_all(&dir)?;
    for (name, body) in &stale {
        fs::write(dir.join(name), body)?;
    }
    if css_stale {
        fs::write(&css, LOGO_CSS)?;
        eprintln!("vibewatch install: wrote {}", css.display());
    }
    if !stale.is_empty() {
        eprintln!(
            "vibewatch install: wrote {} mark(s) into {}",
            stale.len(),
            dir.display()
        );
    }
    Ok(true)
}

pub fn apply_logos_uninstall(dry_run: bool) -> Result<()> {
    let dir = logos_dir();
    let css = logo_css_path();
    if !dir.exists() && !css.exists() {
        return Ok(());
    }
    if dry_run {
        eprintln!(
            "vibewatch install: [dry-run] would remove {} and {}",
            css.display(),
            dir.display()
        );
        return Ok(());
    }
    for (name, _) in &LOGOS {
        let _ = fs::remove_file(dir.join(name));
    }
    // Only if we left it empty — anything the user put there is theirs.
    let _ = fs::remove_dir(&dir);
    let _ = fs::remove_file(&css);
    eprintln!(
        "vibewatch install: removed {} and {}",
        css.display(),
        dir.display()
    );
    Ok(())
}

pub fn apply_waybar_uninstall(dry_run: bool) -> Result<()> {
    let path = waybar_snippet_path();
    if !path.exists() {
        return Ok(());
    }
    if dry_run {
        eprintln!(
            "vibewatch install: [dry-run] would remove {}",
            path.display()
        );
        return Ok(());
    }
    fs::remove_file(&path)?;
    // Best-effort: remove ~/.config/vibewatch if empty.
    if let Some(parent) = path.parent() {
        let _ = fs::remove_dir(parent);
    }
    eprintln!("vibewatch install: removed {}", path.display());
    Ok(())
}

pub fn print_manual_steps() {
    eprintln!();
    eprintln!("vibewatch install: three manual steps remain:");
    eprintln!();
    eprintln!("1. Compositor autostart (paste into your compositor config):");
    eprintln!("   Hyprland  (~/.config/hypr/*.conf):");
    eprintln!("     exec-once = ~/.cargo/bin/vibewatch daemon");
    eprintln!("   Niri      (~/.config/niri/config.kdl):");
    eprintln!("     spawn-at-startup \"sh\" \"-c\" \"~/.cargo/bin/vibewatch daemon\"");
    eprintln!();
    eprintln!("2. Waybar — include the module snippet and wire it into your layout:");
    eprintln!("   Snippet: {}", waybar_snippet_path().display());
    eprintln!("   Include it in your Waybar config and add \"custom/vibewatch\"");
    eprintln!("   to your modules-* array.");
    eprintln!();
    eprintln!("   For the agent marks, add this to the top of your Waybar");
    eprintln!("   stylesheet (both style-dark.css and style-light.css if you");
    eprintln!("   have the pair — Waybar 0.15 loads them directly):");
    eprintln!("     @import url(\"../vibewatch/logos.css\");");
    eprintln!("   Without it the widget still works, just without a logo.");
    eprintln!();
    eprintln!("3. (Optional) Hyprland click-focus tip — stop cursor from warping:");
    eprintln!("     cursor {{ no_warps     = true }}");
    eprintln!("     input  {{ mouse_refocus = false }}");
    eprintln!();
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn merge_hooks_is_idempotent() {
        let initial = json!({});
        let once = merge_hooks(initial);
        let twice = merge_hooks(once.clone());
        assert_eq!(once, twice, "merge_hooks must be idempotent");
    }

    #[test]
    fn merge_hooks_preserves_unrelated_keys() {
        let initial = json!({
            "permissions": {"defaultMode": "auto"},
            "statusLine": {"type": "command", "command": "npx ccstatusline"},
            "enabledPlugins": {"frontend-design@claude-plugins-official": true},
        });
        let merged = merge_hooks(initial.clone());
        assert_eq!(merged["permissions"], initial["permissions"]);
        assert_eq!(merged["statusLine"], initial["statusLine"]);
        assert_eq!(merged["enabledPlugins"], initial["enabledPlugins"]);
        assert!(merged["hooks"]["SessionStart"].is_array());
    }

    #[test]
    fn merge_hooks_adds_all_seven_events() {
        let merged = merge_hooks(json!({}));
        for (event, _, _) in HOOK_EVENTS {
            assert!(merged["hooks"][event].is_array(), "missing hooks.{}", event);
        }
    }

    #[test]
    fn merge_hooks_preserves_other_hook_commands() {
        let initial = json!({
            "hooks": {
                "PreToolUse": [{
                    "matcher": "",
                    "hooks": [{"type": "command", "command": "some-other-tool"}]
                }]
            }
        });
        let merged = merge_hooks(initial);
        let cmds: Vec<String> = merged["hooks"]["PreToolUse"][0]["hooks"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|h| h.get("command").and_then(|c| c.as_str()).map(String::from))
            .collect();
        assert!(cmds.iter().any(|c| c == "some-other-tool"));
        assert!(cmds.iter().any(|c| c.contains("vibewatch")));
    }

    #[test]
    fn apply_hooks_merge_is_idempotent_on_disk() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("settings.json");
        std::fs::write(&path, r#"{"permissions":{"defaultMode":"auto"}}"#).unwrap();
        apply_hooks_merge(&path, false).unwrap();
        let first = std::fs::read_to_string(&path).unwrap();
        apply_hooks_merge(&path, false).unwrap();
        let second = std::fs::read_to_string(&path).unwrap();
        assert_eq!(first, second, "second merge must produce identical output");
    }

    #[test]
    fn apply_hooks_merge_dry_run_writes_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("settings.json");
        let input = r#"{"permissions":{"defaultMode":"auto"}}"#;
        std::fs::write(&path, input).unwrap();
        apply_hooks_merge(&path, true).unwrap();
        let after = std::fs::read_to_string(&path).unwrap();
        assert_eq!(after, input, "--dry-run must not modify the file");
    }

    #[test]
    fn apply_hooks_unmerge_restores_pre_install_shape() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("settings.json");
        std::fs::write(&path, r#"{"permissions":{"defaultMode":"auto"}}"#).unwrap();
        apply_hooks_merge(&path, false).unwrap();
        apply_hooks_unmerge(&path, false).unwrap();
        let final_value: Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        // "hooks" key should be either absent or an empty object
        let hooks_empty = match final_value.get("hooks") {
            None => true,
            Some(v) => v.as_object().map(|o| o.is_empty()).unwrap_or(false),
        };
        assert!(
            hooks_empty,
            "hooks key should be empty/absent after uninstall"
        );
        assert_eq!(final_value["permissions"]["defaultMode"], "auto");
    }

    #[test]
    fn unmerge_hooks_removes_only_vibewatch_hooks() {
        let seeded = merge_hooks(json!({
            "hooks": {
                "PreToolUse": [{
                    "matcher": "",
                    "hooks": [{"type": "command", "command": "some-other-tool"}]
                }]
            }
        }));
        let stripped = unmerge_hooks(seeded);
        let cmds: Vec<String> = stripped["hooks"]["PreToolUse"][0]["hooks"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|h| h.get("command").and_then(|c| c.as_str()).map(String::from))
            .collect();
        assert_eq!(cmds, vec!["some-other-tool".to_string()]);
        let all = serde_json::to_string(&stripped).unwrap();
        assert!(
            !all.contains("vibewatch"),
            "vibewatch string still present: {all}"
        );
    }

    #[test]
    fn systemd_unit_path_points_into_config() {
        let p = systemd_unit_path();
        let s = p.to_string_lossy();
        assert!(
            s.ends_with("systemd/user/vibewatch.service"),
            "unexpected path: {s}"
        );
    }
}
