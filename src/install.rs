use anyhow::{Context, Result};
use serde_json::Value;
use std::fs;
use std::path::{Path, PathBuf};

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

pub fn apply_hooks_merge(path: &Path, dry_run: bool) -> Result<()> {
    if !path.exists() {
        eprintln!(
            "vibewatch install: {} does not exist yet; skipping hook merge. \
             Run vibewatch install again after Claude Code creates it.",
            path.display()
        );
        return Ok(());
    }
    let contents = fs::read_to_string(path)
        .with_context(|| format!("reading {}", path.display()))?;
    let original: Value = serde_json::from_str(&contents)
        .with_context(|| format!("parsing {}", path.display()))?;
    let merged = merge_hooks(original.clone());
    if merged == original {
        eprintln!(
            "vibewatch install: hooks already present in {}",
            path.display()
        );
        return Ok(());
    }
    if dry_run {
        eprintln!(
            "vibewatch install: [dry-run] would merge hooks into {}",
            path.display()
        );
        return Ok(());
    }
    let mut out = serde_json::to_string_pretty(&merged)?;
    out.push('\n');
    fs::write(path, out).with_context(|| format!("writing {}", path.display()))?;
    eprintln!("vibewatch install: merged hooks into {}", path.display());
    Ok(())
}

pub fn apply_hooks_unmerge(path: &Path, dry_run: bool) -> Result<()> {
    if !path.exists() {
        return Ok(());
    }
    let contents = fs::read_to_string(path)
        .with_context(|| format!("reading {}", path.display()))?;
    let original: Value = serde_json::from_str(&contents)
        .with_context(|| format!("parsing {}", path.display()))?;
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
    let mut out = serde_json::to_string_pretty(&stripped)?;
    out.push('\n');
    fs::write(path, out)
        .with_context(|| format!("writing {}", path.display()))?;
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
    } else {
        if !opts.no_hooks {
            apply_hooks_merge(&path, opts.dry_run)?;
        }
    }
    let _ = opts.no_service; // wired up in Task 4
    Ok(())
}

/// Every Claude Code hook event vibewatch registers, plus whether its
/// entry is flagged `async: true` in settings.json. The synchronous
/// entry (PermissionRequest) is what powers the widget approve/deny +
/// AskUserQuestion flows.
pub const HOOK_EVENTS: [(&str, bool); 6] = [
    ("SessionStart",      true),
    ("UserPromptSubmit",  true),
    ("PreToolUse",        true),
    ("PostToolUse",       true),
    ("PermissionRequest", false),
    ("Stop",              true),
];

/// Canonical hook command for a given event.
pub fn command_for(event: &str) -> String {
    format!(
        "~/.cargo/bin/vibewatch notify {} --agent claude-code",
        event_to_slug(event)
    )
}

fn event_to_slug(event: &str) -> String {
    // SessionStart -> session-start
    let mut out = String::new();
    for (i, c) in event.chars().enumerate() {
        if c.is_uppercase() && i > 0 {
            out.push('-');
        }
        out.extend(c.to_lowercase());
    }
    out
}

/// Merge vibewatch's hook entries into a parsed settings.json value.
/// Idempotent: re-running on an already-merged value returns an equal
/// Value; byte-level idempotence of the serialised form is verified in
/// the disk-I/O tests added in Task 3.
pub fn merge_hooks(mut settings: Value) -> Value {
    let hooks = settings
        .as_object_mut()
        .expect("settings.json root must be a JSON object")
        .entry("hooks")
        .or_insert_with(|| serde_json::json!({}));
    let hooks_obj = hooks
        .as_object_mut()
        .expect("settings.json \"hooks\" key must be a JSON object");

    for (event, async_flag) in HOOK_EVENTS {
        let command = command_for(event);
        let entry = hooks_obj
            .entry(event)
            .or_insert_with(|| serde_json::json!([]));
        let array = entry.as_array_mut().expect("settings.json hooks.<event> must be an array");

        // Find or create the matcher-"" group.
        let group_idx = array.iter().position(|g| {
            g.get("matcher").and_then(|m| m.as_str()) == Some("")
        });
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

        let already_present = group_hooks.iter().any(|h| {
            h.get("command").and_then(|c| c.as_str()) == Some(command.as_str())
        });
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
/// Only inspects the six known HOOK_EVENTS keys; vibewatch commands nested
/// under non-standard event names are left alone.
pub fn unmerge_hooks(mut settings: Value) -> Value {
    let Some(hooks_obj) = settings
        .as_object_mut()
        .and_then(|o| o.get_mut("hooks"))
        .and_then(|v| v.as_object_mut())
    else {
        return settings;
    };

    let event_names: Vec<String> = HOOK_EVENTS.iter().map(|(e, _)| e.to_string()).collect();
    for event in &event_names {
        let Some(entry) = hooks_obj.get_mut(event) else { continue };
        let Some(array) = entry.as_array_mut() else { continue };

        for group in array.iter_mut() {
            if let Some(group_hooks) = group
                .get_mut("hooks")
                .and_then(|v| v.as_array_mut())
            {
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
    fn merge_hooks_adds_all_six_events() {
        let merged = merge_hooks(json!({}));
        for (event, _) in HOOK_EVENTS {
            assert!(
                merged["hooks"][event].is_array(),
                "missing hooks.{}", event
            );
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
            .filter_map(|h| {
                h.get("command").and_then(|c| c.as_str()).map(String::from)
            })
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
        assert!(hooks_empty, "hooks key should be empty/absent after uninstall");
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
            .filter_map(|h| {
                h.get("command").and_then(|c| c.as_str()).map(String::from)
            })
            .collect();
        assert_eq!(cmds, vec!["some-other-tool".to_string()]);
        let all = serde_json::to_string(&stripped).unwrap();
        assert!(!all.contains("vibewatch"), "vibewatch string still present: {all}");
    }
}
