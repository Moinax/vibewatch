# CLAUDE.md

vibewatch is a single Rust binary: a daemon that tracks AI coding agents, feeds a
waybar module, and paints a GTK4 layer-shell overlay panel. See README.md for what
it does and how it is configured.

## Checks

```bash
cargo test
cargo clippy --all-targets
```

## Rebuild and replace the running instance

The daemon runs as a user service (`~/.config/systemd/user/vibewatch.service`,
`ExecStart=%h/.cargo/bin/vibewatch daemon`), so seeing a change live means
rebuilding into `~/.cargo/bin` and restarting that unit:

```bash
cargo install --path . && systemctl --user restart vibewatch
```

Do this for any change you want to see in the real app — the panel, its CSS
(`assets/style.css` is compiled in via `include_str!`), the waybar output, and
the hook handling all live in that one process. `systemctl --user status
vibewatch` and `journalctl --user -u vibewatch -f` show whether it came back up
and what it is logging.

A restart drops the in-memory session registry; running agents reappear on the
next scan tick, and pending approvals in flight are lost, so prefer restarting
when nothing is blocked on you.
