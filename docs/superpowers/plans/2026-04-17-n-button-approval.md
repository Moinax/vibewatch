# N-Button Approval UI Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the fixed Accept / Deny pair in the vibewatch approval card with the exact set of buttons Claude Code's terminal TUI would have shown, driven by the `permission_suggestions` array the `PermissionRequest` hook already carries. User's click binds Claude Code's decision just as the terminal dialog would.

**Architecture:** The hook builds a list of `ApprovalChoice { label, behavior, suggestion }` by prepending a plain `"Yes"` (allow), expanding each `permission_suggestion` into its own choice, and appending `"No"` (deny). It passes the list verbatim to the daemon inside the existing `PermissionRequest` event. The panel stores them in `session.pending_approval.choices` and renders one GTK `Button` per choice. On click, the panel sends `ApprovalDecision { request_id, choice_index }` to the daemon; the daemon looks up the chosen entry and writes `{"behavior","suggestion"}` back on the stashed socket. The hook translates the chosen `behavior` + optional `suggestion` into Claude Code's decision JSON.

**Tech Stack:** Rust 2021, `tokio`, `serde_json`, `gtk4`. No new crates.

**Related spec:** `docs/superpowers/specs/2026-04-17-n-button-approval-design.md` (commit `2f6b5b3`).

---

## File Structure

**Modified files:**

| Path | Change |
|---|---|
| `src/session.rs` | `PendingApproval` gains a `choices: Vec<ApprovalChoice>` field. Add `ApprovalChoice` and `PermissionSuggestion`/`PermissionRule` structs. |
| `src/ipc.rs` | Extend `InboundEvent::PermissionRequest` with `permission_suggestions: Vec<PermissionSuggestion>`. Change `InboundEvent::ApprovalDecision` from `{approved: bool}` to `{choice_index: usize}`. |
| `src/notify.rs` | Extract `permission_suggestions` from hook stdin. Build `ApprovalChoice` list with labels. Response line format changes from `{approved}` to `{behavior, suggestion}`. Attach the suggestion to the Claude decision output. Remove the `/tmp/vibewatch-permission-request.json` diagnostic dump. |
| `src/main.rs` | `PermissionRequest` arm stores `choices` on the session's `pending_approval`. `ApprovalDecision` arm looks up the chosen entry by `choice_index` and writes the new wire-format response. Remove the diagnostic `eprintln!`s from commit `cb92eae`. |
| `src/panel/session_row.rs` | `build_approval_bar` → `build_choice_bar(choices)`: renders N buttons, each click sends its own `choice_index`. |
| `assets/style.css` | New `.approval-scope` class (softer green) for suggestion-based allow buttons. |

**New files:** none.

---

## Shared Types & Signatures

Keep these consistent across tasks:

```rust
// src/session.rs
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PermissionRule {
    pub tool_name: String,
    pub rule_content: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PermissionSuggestion {
    #[serde(rename = "type")]
    pub kind: String,                      // "addRules"
    #[serde(default)]
    pub rules: Vec<PermissionRule>,
    pub behavior: String,                  // "allow" | "deny"
    pub destination: String,               // "session" | "project" | "user"
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApprovalChoice {
    pub label: String,
    pub behavior: String,                  // "allow" | "deny"
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub suggestion: Option<PermissionSuggestion>,
}

pub struct PendingApproval {
    pub request_id: String,
    pub tool: String,
    #[serde(default)]
    pub detail: Option<String>,
    #[serde(default)]
    pub choices: Vec<ApprovalChoice>,
}

// src/ipc.rs
InboundEvent::PermissionRequest {
    session_id: String,
    request_id: Option<String>,
    tool: Option<String>,
    detail: Option<String>,
    pid: Option<u32>,
    #[serde(default)]
    permission_suggestions: Vec<PermissionSuggestion>,
}

InboundEvent::ApprovalDecision {
    request_id: String,
    choice_index: usize,
}
```

Wire format for the daemon→hook response line (one JSON, then `\n`):

```json
{"behavior":"allow","suggestion":null}
{"behavior":"allow","suggestion":{"type":"addRules","rules":[{"toolName":"Read","ruleContent":"//home/moinax/.claude/**"}],"behavior":"allow","destination":"session"}}
{"behavior":"deny","suggestion":null}
```

---

## Task 1: Add `PermissionRule`, `PermissionSuggestion`, `ApprovalChoice` + extend `PendingApproval`

**Files:**
- Modify: `src/session.rs` (add three structs; add `choices` field on `PendingApproval`)

- [ ] **Step 1: Write the failing test**

Append to `tests` module in `src/session.rs`:

```rust
#[test]
fn pending_approval_has_choices_field_defaulting_empty() {
    let p = PendingApproval {
        request_id: "r1".into(),
        tool: "Bash".into(),
        detail: None,
        choices: vec![],
    };
    assert!(p.choices.is_empty());
}

#[test]
fn permission_suggestion_serializes_with_type_rename() {
    let s = PermissionSuggestion {
        kind: "addRules".into(),
        rules: vec![PermissionRule {
            tool_name: "Read".into(),
            rule_content: "//home/**".into(),
        }],
        behavior: "allow".into(),
        destination: "session".into(),
    };
    let json = serde_json::to_string(&s).unwrap();
    assert!(json.contains(r#""type":"addRules""#), "got {json}");
    assert!(json.contains(r#""behavior":"allow""#));
    assert!(json.contains(r#""destination":"session""#));
    assert!(json.contains(r#""toolName":"Read""#) == false,
        "PermissionRule serializes with snake_case by default; got {json}");
}

#[test]
fn approval_choice_omits_suggestion_when_none() {
    let c = ApprovalChoice {
        label: "Yes".into(),
        behavior: "allow".into(),
        suggestion: None,
    };
    let json = serde_json::to_string(&c).unwrap();
    assert!(!json.contains("suggestion"), "got {json}");
    assert!(json.contains(r#""label":"Yes""#));
}
```

Note: the third assertion in the second test is deliberate — the hook will serialize `PermissionRule` with snake_case `tool_name`/`rule_content` via the struct's default derive, not camelCase. We'll rename via `#[serde(rename_all = "camelCase")]` to match Claude's payload, flipping that assertion in Step 3.

- [ ] **Step 2: Run tests to verify they fail**

```bash
cd /home/moinax/Projects/labs/vibewatch
cargo test --lib session -- --nocapture
```

Expected: compile errors on `PermissionSuggestion`, `ApprovalChoice`, `PermissionRule`, and `PendingApproval::choices`.

- [ ] **Step 3: Add the structs + field**

In `src/session.rs`, above the existing `PendingApproval` struct (around line 69), insert:

```rust
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PermissionRule {
    pub tool_name: String,
    pub rule_content: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PermissionSuggestion {
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(default)]
    pub rules: Vec<PermissionRule>,
    pub behavior: String,
    pub destination: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApprovalChoice {
    pub label: String,
    pub behavior: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub suggestion: Option<PermissionSuggestion>,
}
```

Replace the existing `PendingApproval` block with:

```rust
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingApproval {
    pub request_id: String,
    pub tool: String,
    #[serde(default)]
    pub detail: Option<String>,
    #[serde(default)]
    pub choices: Vec<ApprovalChoice>,
}
```

In the second test, flip the assertion so it expects camelCase (because we added `#[serde(rename_all = "camelCase")]`):

```rust
    assert!(json.contains(r#""toolName":"Read""#),
        "PermissionRule must serialize with camelCase to match Claude payload; got {json}");
    assert!(!json.contains("tool_name"));
```

- [ ] **Step 4: Fix compile errors at existing call sites**

The existing `PendingApproval` constructor in `src/main.rs` (the `PermissionRequest` arm) passes the old three fields — add `choices: vec![]` to keep the current shape. That arm is fully rewritten in Task 5; this is just a stopgap.

```rust
                    session.pending_approval = Some(crate::session::PendingApproval {
                        request_id: request_id.clone(),
                        tool: tool_name,
                        detail,
                        choices: vec![],
                    });
```

- [ ] **Step 5: Run the full lib test suite**

```bash
cd /home/moinax/Projects/labs/vibewatch
cargo test --lib -- --nocapture
```

Expected: all pass (74 prior + 3 new = 77).

- [ ] **Step 6: Commit**

```bash
cd /home/moinax/Projects/labs/vibewatch
git add src/session.rs src/main.rs
git commit -m "session: add ApprovalChoice + PermissionSuggestion; choices on PendingApproval"
```

---

## Task 2: Extend IPC events for permission_suggestions + choice_index

**Files:**
- Modify: `src/ipc.rs` (extend `PermissionRequest`; replace `ApprovalDecision` body)
- Modify: `src/main.rs`, `src/notify.rs`, `src/panel/session_row.rs` (fix all call sites)

- [ ] **Step 1: Write the failing test**

Append to `tests` module in `src/ipc.rs`:

```rust
    #[test]
    fn test_parse_permission_request_with_suggestions() {
        let json = r#"{"event":"permission_request","session_id":"s1","request_id":"r1","tool":"Read","detail":"/etc/hosts","permission_suggestions":[{"type":"addRules","rules":[{"toolName":"Read","ruleContent":"//etc/**"}],"behavior":"allow","destination":"session"}]}"#;
        let e: InboundEvent = serde_json::from_str(json).unwrap();
        match e {
            InboundEvent::PermissionRequest { permission_suggestions, .. } => {
                assert_eq!(permission_suggestions.len(), 1);
                assert_eq!(permission_suggestions[0].kind, "addRules");
                assert_eq!(permission_suggestions[0].destination, "session");
                assert_eq!(permission_suggestions[0].rules[0].tool_name, "Read");
                assert_eq!(permission_suggestions[0].rules[0].rule_content, "//etc/**");
            }
            _ => panic!("expected PermissionRequest"),
        }
    }

    #[test]
    fn test_parse_permission_request_without_suggestions_defaults_empty() {
        let json = r#"{"event":"permission_request","session_id":"s1","tool":"Bash"}"#;
        let e: InboundEvent = serde_json::from_str(json).unwrap();
        match e {
            InboundEvent::PermissionRequest { permission_suggestions, .. } => {
                assert!(permission_suggestions.is_empty());
            }
            _ => panic!("expected PermissionRequest"),
        }
    }

    #[test]
    fn test_parse_approval_decision_with_choice_index() {
        let json = r#"{"event":"approval_decision","request_id":"r1","choice_index":2}"#;
        let e: InboundEvent = serde_json::from_str(json).unwrap();
        match e {
            InboundEvent::ApprovalDecision { request_id, choice_index } => {
                assert_eq!(request_id, "r1");
                assert_eq!(choice_index, 2);
            }
            _ => panic!("expected ApprovalDecision"),
        }
    }
```

Remove (or rewrite) the existing `test_parse_approval_decision` test which uses `approved: true` — replace it with the choice_index version above.

- [ ] **Step 2: Run tests to verify they fail**

```bash
cd /home/moinax/Projects/labs/vibewatch
cargo test --lib ipc::tests -- --nocapture
```

Expected: compile errors on `permission_suggestions` field and `choice_index`.

- [ ] **Step 3: Extend the variants**

In `src/ipc.rs`, replace the `PermissionRequest` variant:

```rust
    PermissionRequest {
        session_id: String,
        #[serde(default)]
        request_id: Option<String>,
        #[serde(default)]
        tool: Option<String>,
        #[serde(default)]
        detail: Option<String>,
        #[serde(default)]
        pid: Option<u32>,
        #[serde(default)]
        permission_suggestions: Vec<crate::session::PermissionSuggestion>,
    },
```

Replace the `ApprovalDecision` variant:

```rust
    ApprovalDecision {
        request_id: String,
        choice_index: usize,
    },
```

- [ ] **Step 4: Fix call sites**

In `src/main.rs`, update the `PermissionRequest` destructure to include `permission_suggestions` (ignore with `_` for now — Task 3 wires it up):

```rust
            InboundEvent::PermissionRequest {
                session_id,
                request_id,
                tool,
                detail,
                pid,
                permission_suggestions: _,
            } => {
                // ...existing body...
            }
```

Update the `ApprovalDecision` destructure to use `choice_index` (remap the existing logic to behave as "approved" when `choice_index == 0`, and deny for any other value — temporary scaffold for Task 5):

```rust
            InboundEvent::ApprovalDecision { request_id, choice_index } => {
                let approved = choice_index == 0; // temporary until Task 5
                if let Some(mut entry) = approval_registry.take(&request_id).await {
                    eprintln!(
                        "vibewatch: took ApprovalRegistry entry for request_id={} session_id={}",
                        request_id, entry.session_id
                    );
                    let line = if approved {
                        b"{\"approved\":true}\n".as_slice()
                    } else {
                        b"{\"approved\":false}\n".as_slice()
                    };
                    // ...rest of the arm unchanged...
                }
            }
```

In `src/notify.rs`, the `"permission-request" =>` arm inside `parse_claude_code` currently does:

```rust
        "permission-request" => {
            let pid = parent_pid();
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0);
            let request_id = format!("{}-{}-{}", hook.session_id, pid, nanos);
            Ok(InboundEvent::PermissionRequest {
                session_id: hook.session_id,
                request_id: Some(request_id),
                tool: hook.tool_name,
                detail: extract_tool_detail(&hook.tool_input),
                pid: Some(pid),
            })
        }
```

Add `permission_suggestions: vec![]` to the constructor — Task 3 populates it properly:

```rust
            Ok(InboundEvent::PermissionRequest {
                session_id: hook.session_id,
                request_id: Some(request_id),
                tool: hook.tool_name,
                detail: extract_tool_detail(&hook.tool_input),
                pid: Some(pid),
                permission_suggestions: vec![],
            })
```

In `src/panel/session_row.rs`, update `send_approval_decision` to take `choice_index: usize` (rename the bool path to Task 5):

```rust
fn send_approval_decision(request_id: &str, choice_index: usize) {
    // ...existing body, but:
    let event = crate::ipc::InboundEvent::ApprovalDecision {
        request_id,
        choice_index,
    };
    // ...
}
```

And update both click handlers in `build_approval_bar` (still Accept/Deny at this point) to pass `0` (Accept) and `1` (Deny) — temporary mapping; Task 5 replaces this function entirely.

- [ ] **Step 5: Build + test**

```bash
cd /home/moinax/Projects/labs/vibewatch
cargo build
cargo test --lib -- --nocapture
```

Expected: clean build, tests green.

- [ ] **Step 6: Commit**

```bash
cd /home/moinax/Projects/labs/vibewatch
git add src/ipc.rs src/main.rs src/notify.rs src/panel/session_row.rs
git commit -m "ipc: carry permission_suggestions on PermissionRequest; replace ApprovalDecision.approved with choice_index"
```

---

## Task 3: Parse `permission_suggestions` on the hook side + build choices

**Files:**
- Modify: `src/notify.rs` — extend `ClaudeCodeHook` struct with `permission_suggestions` field. Build `Vec<ApprovalChoice>` in the `"permission-request"` arm, attaching it to the event.

- [ ] **Step 1: Write the failing test**

Append to the `tests` module in `src/notify.rs`:

```rust
    #[test]
    fn permission_request_parses_permission_suggestions() {
        let json = r#"{"session_id":"s1","hook_event_name":"permission-request","tool_name":"Read","tool_input":{"file_path":"/etc/hosts"},"permission_suggestions":[{"type":"addRules","rules":[{"toolName":"Read","ruleContent":"//etc/**"}],"behavior":"allow","destination":"session"}]}"#;
        let event = parse_claude_code(json, "permission-request").unwrap();
        match event {
            InboundEvent::PermissionRequest { permission_suggestions, .. } => {
                assert_eq!(permission_suggestions.len(), 1);
                assert_eq!(permission_suggestions[0].kind, "addRules");
                assert_eq!(permission_suggestions[0].behavior, "allow");
                assert_eq!(permission_suggestions[0].destination, "session");
                assert_eq!(permission_suggestions[0].rules.len(), 1);
                assert_eq!(permission_suggestions[0].rules[0].tool_name, "Read");
                assert_eq!(permission_suggestions[0].rules[0].rule_content, "//etc/**");
            }
            _ => panic!("expected PermissionRequest"),
        }
    }
```

- [ ] **Step 2: Run test; watch it fail**

```bash
cd /home/moinax/Projects/labs/vibewatch
cargo test --lib notify::tests::permission_request_parses_permission_suggestions -- --nocapture
```

Expected: test fails because `ClaudeCodeHook` doesn't have a `permission_suggestions` field and the constructor doesn't populate it.

- [ ] **Step 3: Extend `ClaudeCodeHook`**

In `src/notify.rs`, find the `ClaudeCodeHook` struct (around line 70). Add a new field at the bottom (before the closing `}`):

```rust
    #[serde(default)]
    pub permission_suggestions: Vec<crate::session::PermissionSuggestion>,
```

- [ ] **Step 4: Populate in the `permission-request` arm**

Update the `permission-request` arm of `parse_claude_code`:

```rust
        "permission-request" => {
            let pid = parent_pid();
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0);
            let request_id = format!("{}-{}-{}", hook.session_id, pid, nanos);
            Ok(InboundEvent::PermissionRequest {
                session_id: hook.session_id,
                request_id: Some(request_id),
                tool: hook.tool_name,
                detail: extract_tool_detail(&hook.tool_input),
                pid: Some(pid),
                permission_suggestions: hook.permission_suggestions,
            })
        }
```

- [ ] **Step 5: Run test**

```bash
cd /home/moinax/Projects/labs/vibewatch
cargo test --lib notify::tests -- --nocapture
```

Expected: pass.

- [ ] **Step 6: Commit**

```bash
cd /home/moinax/Projects/labs/vibewatch
git add src/notify.rs
git commit -m "notify: parse permission_suggestions from hook stdin"
```

---

## Task 4: Build `ApprovalChoice` list in the daemon, store on session

**Files:**
- Modify: `src/main.rs` — in the `PermissionRequest` arm, convert `permission_suggestions` into a `Vec<ApprovalChoice>` and store it on `session.pending_approval`.
- Modify: `src/session.rs` — add a small pure helper `ApprovalChoice::from_permission_event(tool_name, suggestions)` for testability.

- [ ] **Step 1: Write the failing test**

Append to the `tests` module in `src/session.rs`:

```rust
#[test]
fn build_choices_always_has_yes_first_and_no_last() {
    let choices = ApprovalChoice::build_from(
        "Read",
        &[],
    );
    assert_eq!(choices.len(), 2);
    assert_eq!(choices[0].label, "Yes");
    assert_eq!(choices[0].behavior, "allow");
    assert!(choices[0].suggestion.is_none());
    assert_eq!(choices[1].label, "No");
    assert_eq!(choices[1].behavior, "deny");
}

#[test]
fn build_choices_expands_session_suggestion_with_human_label() {
    let sug = PermissionSuggestion {
        kind: "addRules".into(),
        rules: vec![PermissionRule {
            tool_name: "Read".into(),
            rule_content: "//home/moinax/.claude/**".into(),
        }],
        behavior: "allow".into(),
        destination: "session".into(),
    };
    let choices = ApprovalChoice::build_from("Read", std::slice::from_ref(&sug));
    assert_eq!(choices.len(), 3);
    assert_eq!(choices[0].label, "Yes");
    assert!(choices[1].label.contains("Read"));
    assert!(choices[1].label.contains("/home/moinax/.claude/**"));
    assert!(choices[1].label.contains("session"));
    assert_eq!(choices[1].behavior, "allow");
    assert_eq!(choices[1].suggestion.as_ref().unwrap().destination, "session");
    assert_eq!(choices[2].label, "No");
}

#[test]
fn build_choices_multiple_rules_joined_with_plus() {
    let sug = PermissionSuggestion {
        kind: "addRules".into(),
        rules: vec![
            PermissionRule { tool_name: "Read".into(), rule_content: "//a/**".into() },
            PermissionRule { tool_name: "Read".into(), rule_content: "//b/**".into() },
        ],
        behavior: "allow".into(),
        destination: "session".into(),
    };
    let choices = ApprovalChoice::build_from("Read", std::slice::from_ref(&sug));
    assert!(choices[1].label.contains("/a/**"));
    assert!(choices[1].label.contains("/b/**"));
    assert!(choices[1].label.contains("+"));
}
```

- [ ] **Step 2: Run tests; fail**

```bash
cd /home/moinax/Projects/labs/vibewatch
cargo test --lib session::tests::build_choices -- --nocapture
```

Expected: compile error `ApprovalChoice::build_from` doesn't exist.

- [ ] **Step 3: Add the helper**

In `src/session.rs`, impl `ApprovalChoice`:

```rust
impl ApprovalChoice {
    /// Build the ordered list of buttons for a permission dialog.
    /// Always prepends "Yes" / appends "No"; each suggestion becomes a
    /// middle button with a human-readable label.
    pub fn build_from(tool_name: &str, suggestions: &[PermissionSuggestion]) -> Vec<ApprovalChoice> {
        let mut out = Vec::with_capacity(2 + suggestions.len());
        out.push(ApprovalChoice {
            label: "Yes".to_string(),
            behavior: "allow".to_string(),
            suggestion: None,
        });
        for sug in suggestions {
            let rules_label = sug
                .rules
                .iter()
                .map(|r| r.rule_content.trim_start_matches('/').to_string())
                .collect::<Vec<_>>()
                .join(" + ");
            let label = format!(
                "{} {} for /{} ({})",
                if sug.behavior == "allow" { "Yes, allow" } else { "No, deny" },
                tool_name,
                rules_label,
                sug.destination,
            );
            out.push(ApprovalChoice {
                label,
                behavior: sug.behavior.clone(),
                suggestion: Some(sug.clone()),
            });
        }
        out.push(ApprovalChoice {
            label: "No".to_string(),
            behavior: "deny".to_string(),
            suggestion: None,
        });
        out
    }
}
```

Label format mirrors Claude Code's terminal phrasing closely enough that the button is readable at a glance. `//home/moinax/.claude/**` → `/home/moinax/.claude/**` via `trim_start_matches('/')`.

- [ ] **Step 4: Run tests; they pass**

```bash
cd /home/moinax/Projects/labs/vibewatch
cargo test --lib session::tests::build_choices -- --nocapture
```

- [ ] **Step 5: Use the helper in `handle_connection`**

In `src/main.rs`, in the `PermissionRequest` arm, replace the existing `pending_approval` construction (which currently passes `choices: vec![]`) with:

```rust
                if let Some(mut session) = lookup_session(&registry, &session_id, pid) {
                    session.status = SessionStatus::WaitingApproval;
                    session.current_tool = Some(tool_name.clone());
                    session.tool_detail = detail.clone();
                    let choices = crate::session::ApprovalChoice::build_from(
                        &tool_name,
                        &permission_suggestions,
                    );
                    session.pending_approval = Some(crate::session::PendingApproval {
                        request_id: request_id.clone(),
                        tool: tool_name,
                        detail,
                        choices,
                    });
                    session.touch();
                    registry.register(session);
                }
```

Also update the destructure to bind `permission_suggestions` (no longer `_`):

```rust
            InboundEvent::PermissionRequest {
                session_id,
                request_id,
                tool,
                detail,
                pid,
                permission_suggestions,
            } => {
```

- [ ] **Step 6: Build + full test suite**

```bash
cd /home/moinax/Projects/labs/vibewatch
cargo build
cargo test --lib
```

Expected: clean build, all tests pass.

- [ ] **Step 7: Commit**

```bash
cd /home/moinax/Projects/labs/vibewatch
git add src/session.rs src/main.rs
git commit -m "session: ApprovalChoice::build_from helper; daemon populates choices from suggestions"
```

---

## Task 5: Render N buttons in the panel; click sends choice_index

**Files:**
- Modify: `src/panel/session_row.rs` — rename `build_approval_bar` → `build_choice_bar`, iterate choices.

- [ ] **Step 1: Write the failing test**

Append to `tests` module in `src/panel/session_row.rs`:

```rust
    #[test]
    fn button_label_for_plain_yes_is_yes() {
        let c = crate::session::ApprovalChoice {
            label: "Yes".into(),
            behavior: "allow".into(),
            suggestion: None,
        };
        assert_eq!(button_label(&c), "Yes");
    }

    #[test]
    fn button_css_class_for_suggestion_is_approval_scope() {
        let c = crate::session::ApprovalChoice {
            label: "Yes, allow Read for /foo (session)".into(),
            behavior: "allow".into(),
            suggestion: Some(crate::session::PermissionSuggestion {
                kind: "addRules".into(),
                rules: vec![],
                behavior: "allow".into(),
                destination: "session".into(),
            }),
        };
        assert_eq!(button_css_class(&c), "approval-scope");
    }

    #[test]
    fn button_css_class_plain_allow_is_accept() {
        let c = crate::session::ApprovalChoice {
            label: "Yes".into(),
            behavior: "allow".into(),
            suggestion: None,
        };
        assert_eq!(button_css_class(&c), "approval-accept");
    }

    #[test]
    fn button_css_class_deny_is_deny() {
        let c = crate::session::ApprovalChoice {
            label: "No".into(),
            behavior: "deny".into(),
            suggestion: None,
        };
        assert_eq!(button_css_class(&c), "approval-deny");
    }
```

- [ ] **Step 2: Add the helpers and the new build function**

In `src/panel/session_row.rs`, replace `build_approval_bar(request_id: String) -> gtk::Box` with:

```rust
pub(crate) fn button_label(choice: &crate::session::ApprovalChoice) -> &str {
    &choice.label
}

pub(crate) fn button_css_class(choice: &crate::session::ApprovalChoice) -> &'static str {
    match (choice.behavior.as_str(), choice.suggestion.is_some()) {
        ("allow", true) => "approval-scope",
        ("allow", false) => "approval-accept",
        ("deny", _) => "approval-deny",
        _ => "approval-accept",
    }
}

/// Build a horizontal box containing one button per ApprovalChoice.
fn build_choice_bar(
    request_id: String,
    choices: &[crate::session::ApprovalChoice],
) -> gtk::Box {
    let bar = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    bar.add_css_class("approval-bar");
    bar.set_halign(gtk::Align::Start);
    bar.set_margin_top(4);

    for (idx, choice) in choices.iter().enumerate() {
        let button = gtk::Button::with_label(button_label(choice));
        button.add_css_class(button_css_class(choice));
        let rid = request_id.clone();
        button.connect_clicked(move |_| {
            let rid = rid.clone();
            std::thread::spawn(move || {
                send_approval_decision(&rid, idx);
            });
        });
        bar.append(&button);
    }

    bar
}
```

Update the one call site in `build_row`:

```rust
    if let Some(ref pending) = session.pending_approval {
        let bar = build_choice_bar(pending.request_id.clone(), &pending.choices);
        content.append(&bar);
    }
```

Make sure `send_approval_decision` already takes `(request_id: &str, choice_index: usize)` from Task 2.

- [ ] **Step 3: Build + test**

```bash
cd /home/moinax/Projects/labs/vibewatch
cargo build
cargo test panel::session_row::tests -- --nocapture
```

Expected: all panel tests pass.

- [ ] **Step 4: Commit**

```bash
cd /home/moinax/Projects/labs/vibewatch
git add src/panel/session_row.rs
git commit -m "panel: render one button per ApprovalChoice; choice_index on click"
```

---

## Task 6: Daemon writes `{behavior, suggestion}` back on the stream; hook translates to Claude JSON

**Files:**
- Modify: `src/main.rs` — `ApprovalDecision` arm looks up the chosen `ApprovalChoice` by `choice_index` and writes the new wire format.
- Modify: `src/notify.rs` — `send_permission_request` parses `{behavior, suggestion}` into a new `PermissionDecisionResult` type; `handle_notify` uses it to build Claude's decision JSON, attaching the suggestion as `decision.suggestion` if present.

- [ ] **Step 1: Update the daemon's ApprovalDecision arm**

In `src/main.rs`, replace the temporary `approved = choice_index == 0` scaffold with:

```rust
            InboundEvent::ApprovalDecision { request_id, choice_index } => {
                eprintln!(
                    "vibewatch: recv ApprovalDecision request_id={} choice_index={}",
                    request_id, choice_index
                );
                let Some(entry) = approval_registry.take(&request_id).await else {
                    eprintln!(
                        "vibewatch: NO entry in ApprovalRegistry for request_id={}",
                        request_id
                    );
                    continue;
                };
                let (behavior_str, suggestion) = registry
                    .get(&entry.session_id)
                    .and_then(|s| s.pending_approval.as_ref().and_then(|p| p.choices.get(choice_index).cloned()))
                    .map(|c| (c.behavior, c.suggestion))
                    .unwrap_or_else(|| ("deny".to_string(), None));
                let response_json = serde_json::json!({
                    "behavior": behavior_str,
                    "suggestion": suggestion,
                });
                let mut line = response_json.to_string();
                line.push('\n');
                let mut wh = entry.write_half;
                match wh.write_all(line.as_bytes()).await {
                    Ok(_) => eprintln!(
                        "vibewatch: wrote decision line for request_id={}: {}",
                        request_id, line.trim()
                    ),
                    Err(e) => eprintln!(
                        "vibewatch: failed to write approval decision for {}: {}",
                        request_id, e
                    ),
                }
                if let Err(e) = wh.flush().await {
                    eprintln!(
                        "vibewatch: failed to flush approval decision for {}: {}",
                        request_id, e
                    );
                }
                if let Some(mut s) = registry.get(&entry.session_id) {
                    s.pending_approval = None;
                    s.status = SessionStatus::Thinking;
                    s.current_tool = None;
                    s.tool_detail = None;
                    s.touch();
                    registry.register(s);
                }
            }
```

Note the `Session::pending_approval` borrow problem — use `.and_then(|p| p.choices.get(choice_index).cloned())` over an owned Session clone (`registry.get` returns `Option<Session>` by clone).

- [ ] **Step 2: Update the hook side**

In `src/notify.rs`, replace `PermissionDecision` (the tri-state enum) with a richer result:

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PermissionDecisionResult {
    pub behavior: String,                                       // "allow" | "deny" | "ask"
    pub suggestion: Option<crate::session::PermissionSuggestion>,
}

impl PermissionDecisionResult {
    fn ask() -> Self {
        Self { behavior: "ask".into(), suggestion: None }
    }
}
```

Rewrite `send_permission_request` to parse both fields:

```rust
pub async fn send_permission_request(
    socket_path: &std::path::Path,
    event: &InboundEvent,
    timeout: std::time::Duration,
) -> anyhow::Result<PermissionDecisionResult> {
    use anyhow::Context;
    let mut stream = UnixStream::connect(socket_path)
        .await
        .context("connect to vibewatch daemon")?;

    let mut json = serde_json::to_string(event)?;
    json.push('\n');
    stream.write_all(json.as_bytes()).await?;
    stream.flush().await?;

    let (read_half, _write_half) = stream.into_split();
    let mut reader = tokio::io::BufReader::new(read_half);
    let mut line = String::new();

    match tokio::time::timeout(timeout, reader.read_line(&mut line)).await {
        Ok(Ok(n)) if n > 0 => {
            let v: serde_json::Value = serde_json::from_str(line.trim())?;
            let behavior = v.get("behavior").and_then(|x| x.as_str()).unwrap_or("deny").to_string();
            let suggestion = v
                .get("suggestion")
                .and_then(|x| if x.is_null() { None } else { serde_json::from_value(x.clone()).ok() });
            Ok(PermissionDecisionResult { behavior, suggestion })
        }
        Ok(Ok(_)) => Ok(PermissionDecisionResult::ask()),
        Ok(Err(e)) => Err(anyhow::anyhow!("read error: {e}")),
        Err(_) => Ok(PermissionDecisionResult::ask()),
    }
}
```

Rewrite `handle_notify`'s permission-request branch:

```rust
    if agent == "claude-code" && event_type == "permission-request" {
        eprintln!("vibewatch-hook: permission-request starting, waiting for daemon response");
        let result = match send_permission_request(
            &socket_path,
            &event,
            std::time::Duration::from_secs(580),
        )
        .await
        {
            Ok(r) => {
                eprintln!("vibewatch-hook: got decision: {:?}", r);
                r
            }
            Err(e) => {
                eprintln!("vibewatch-hook: permission-request fallback ask ({e})");
                PermissionDecisionResult::ask()
            }
        };
        let mut decision = serde_json::json!({
            "behavior": result.behavior,
            "reason": "via vibewatch widget",
        });
        if let Some(sug) = result.suggestion {
            if let serde_json::Value::Object(ref mut map) = decision {
                map.insert("suggestion".to_string(), serde_json::to_value(sug)?);
            }
        }
        let out = serde_json::json!({
            "hookSpecificOutput": {
                "hookEventName": "PermissionRequest",
                "decision": decision,
            }
        });
        let out_str = serde_json::to_string(&out)?;
        eprintln!("vibewatch-hook: emitting stdout: {}", out_str);
        println!("{}", out_str);
        return Ok(());
    }
```

- [ ] **Step 3: Update existing tests**

The two existing tests (`send_permission_request_reads_decision_line`, `send_permission_request_errors_when_daemon_missing`) will break because they assert on `PermissionDecision::Allow`. Update them to assert on `PermissionDecisionResult`:

```rust
        let result = send_permission_request(&path, &event, std::time::Duration::from_secs(2))
            .await
            .expect("round-trip succeeds");
        assert_eq!(result.behavior, "allow");
        assert!(result.suggestion.is_none());
```

And update the mock server response line to match the new wire format: `b"{\"behavior\":\"allow\",\"suggestion\":null}\n"`.

- [ ] **Step 4: Build + run tests**

```bash
cd /home/moinax/Projects/labs/vibewatch
cargo build
cargo test --lib
```

Expected: clean build, all tests pass.

- [ ] **Step 5: Commit**

```bash
cd /home/moinax/Projects/labs/vibewatch
git add src/main.rs src/notify.rs
git commit -m "daemon: write {behavior,suggestion} on stream; hook attaches suggestion to Claude decision"
```

---

## Task 7: Style + cleanup

**Files:**
- Modify: `assets/style.css` — add `.approval-scope` class (softer green, distinguishes "Yes, allow for session" from plain "Yes").
- Modify: `src/notify.rs` — remove the diagnostic `std::fs::write("/tmp/vibewatch-permission-request.json", ...)` dump (its purpose is served).

- [ ] **Step 1: Add CSS**

Append to `assets/style.css`:

```css
.approval-scope {
    color: #1e1e2e;
    background-color: #94e2d5;
    border-color: rgba(148, 226, 213, 0.6);
    border-width: 1px;
    border-style: solid;
    border-radius: 6px;
    padding: 3px 12px;
    font-size: 11px;
    font-weight: 600;
}

.approval-scope:hover {
    background-color: #a6ebe0;
    box-shadow: 0 0 6px rgba(148, 226, 213, 0.35);
}
```

- [ ] **Step 2: Remove the diagnostic dump**

In `src/notify.rs` `handle_notify`, remove the block:

```rust
    if event_type == "permission-request" {
        let _ = std::fs::write(
            "/tmp/vibewatch-permission-request.json",
            &stdin_buf,
        );
    }
```

- [ ] **Step 3: Build + test**

```bash
cd /home/moinax/Projects/labs/vibewatch
cargo build --release
cargo test --lib
```

- [ ] **Step 4: Commit**

```bash
cd /home/moinax/Projects/labs/vibewatch
git add assets/style.css src/notify.rs
git commit -m "style: approval-scope button; remove debug stdin dump"
```

---

## Task 8: Deploy + smoke test

- [ ] **Step 1: Install**

```bash
cd /home/moinax/Projects/labs/vibewatch
cargo install --path . --force
```

- [ ] **Step 2: Restart daemon**

```bash
pkill -f "vibewatch daemon" 2>/dev/null; sleep 1
> /tmp/vibewatch.log
nohup ~/.cargo/bin/vibewatch daemon >/tmp/vibewatch.log 2>&1 &
disown
sleep 1
pgrep -af "vibewatch daemon" | grep -v pgrep
```

- [ ] **Step 3: Smoke test**

In a Claude Code terminal:
1. Trigger a permission prompt (e.g., ask the assistant to read `/etc/hosts`).
2. Panel should auto-show with **3 buttons**: `Yes` / `Yes, allow Read for /etc/** (session)` / `No`.
3. Click the middle button. Verify:
   - Claude Code proceeds with the Read.
   - A subsequent Read in `/etc/` doesn't fire the widget again (session rule took effect).
4. Repeat with `Yes` and verify single-call allow.
5. Repeat with `No` and verify Claude receives the denial.
6. Inspect `/tmp/vibewatch.log` for the `wrote decision line for request_id=... : {"behavior":"allow","suggestion":{...}}` line — confirms the daemon sent the suggestion.

- [ ] **Step 4: If the session-scope button doesn't persist**

If clicking the middle button allows once but re-prompts on the next same-kind read, Phase 1 (empirical hook output) didn't work. Fallback Phase 2 (not part of this plan): the daemon writes the rule directly into `~/.claude/settings.local.json`. Open a new follow-up task rather than expanding scope here.

---

## Self-review checklist

1. **Spec coverage.** Every section of the design spec has a task:
   - Data model (Section "Data model") → Task 1.
   - Wire protocol changes → Tasks 2, 3.
   - UI changes → Task 5.
   - Style → Task 7.
   - Hook response → Tasks 2, 6.
   - Cleanup → Task 7.
   - Deploy + verify → Task 8.

2. **Placeholder scan.** No TBDs. All code blocks shown.

3. **Type consistency.** `ApprovalChoice`, `PermissionSuggestion`, `PermissionRule` names are used identically in every task. `choice_index: usize` shape is consistent. The wire format `{behavior, suggestion}` appears in Task 6 for both writer (daemon) and reader (hook).
