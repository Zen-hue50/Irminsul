# Irminsul

<img width="1920" height="1080" alt="image" src="https://github.com/user-attachments/assets/287b39e5-d5a8-42af-a2c5-f903680ab5bc" />

<img width="1920" height="1080" alt="image" src="https://github.com/user-attachments/assets/6cfbf1ff-b98f-4d19-b7c2-87b09e95764c" />


**Digital wellbeing for Linux / Hyprland.** Irminsul is a small Rust daemon with a private browser dashboard. It records foreground app time, active screen time, and switches between apps, then turns those totals into a daily, seven-day, or 30-day report. It does not need a browser tab or a tray app to record activity.

## Install

**One command** — run this in any terminal (a Hyprland session is needed for
tracking, not for installing):

```sh
curl -fsSL https://raw.githubusercontent.com/Zen-hue50/Irminsul/main/install.sh | sh
```

or, if you prefer git:

```sh
git clone https://github.com/Zen-hue50/Irminsul
cd Irminsul
./install.sh
```

The installer figures out everything by itself:

- If a **prebuilt binary** for your machine exists in GitHub Releases, it is
  downloaded and used directly — no Rust, no compiler, no build.
- Otherwise it downloads the source and builds it. A missing **Rust 1.85+**
  is installed user-locally with rustup (asks first; `--yes` skips the
  question). A missing **C compiler** (needed to build the bundled SQLite) is
  offered through your distro's package manager with your consent (sudo).
- It places the binary at `~/.local/bin/irminsul` and adds a marked
  autostart block to your Hyprland config, keeping a `.irminsul.bak` backup
  and never duplicating the block. Daemon settings live in
  `~/.config/irminsul/config.toml` (see *Configure and theme* below).
- Run inside Hyprland, it also starts the daemon now; otherwise it starts at
  the next login.

No root is needed for the normal path. Open **http://127.0.0.1:8765/** when
the daemon runs.

Options:

| Flag | Effect |
| --- | --- |
| `--hyprland-config FILE` | Use a specific `hyprland.lua` or `hyprland.conf` |
| `--no-autostart` | Do not edit the Hyprland config (add the snippet below yourself) |
| `--no-start` | Wait for the next Hyprland login to start tracking |
| `--bin-dir DIR` | Install into `DIR` instead of `~/.local/bin` |
| `--build-from-source` | Ignore release binaries; always compile locally |
| `--ref REF` | Install from another branch or tag (curl mode) |
| `--yes`, `-y` | Install missing dependencies without asking |
| `--no-install-deps` | Never install Rust or a C compiler; explain what is missing and exit |

If `irminsul` is not on your `PATH`, use `~/.local/bin/irminsul` in the
commands below (the installer prints a note when this applies).

## Upgrade an earlier installation

Run the same install command again — from anywhere, or `./install.sh` inside
an existing clone. The installer replaces the binary and leaves your
database, TOML configuration, and theme files untouched; the previous
executable is kept at `~/.local/state/irminsul/previous-executable`. Before
upgrading, stop the current daemon: `pgrep -a -x irminsul`, then
`kill -TERM PID` — a running daemon keeps using the old binary until it is
restarted, and the installer prints a reminder if it detects one. Then
refresh the dashboard.

If you use a custom autostart script or a user service, point its command at
`irminsul run`.

<details>
<summary>Manual build and autostart snippets</summary>

```sh
cargo build --locked --release
install -Dm755 target/release/irminsul "$HOME/.local/bin/irminsul"
~/.local/bin/irminsul migrate  # optional for an earlier install
~/.local/bin/irminsul init
~/.local/bin/irminsul run
```

For **`hyprland.lua`**:

```lua
hl.on("hyprland.start", function()
    hl.exec_cmd("~/.local/bin/irminsul run")
end)
```

For **`hyprland.conf`**:

```ini
exec-once = ~/.local/bin/irminsul run
```

Use one of these, not both. The daemon needs the `HYPRLAND_INSTANCE_SIGNATURE`, `XDG_RUNTIME_DIR`, and Wayland socket environment of the graphical session. It disconnects when the session ends. If you bring your own user service, pass the session environment and don't also use the compositor hook.

</details>

## What the reports mean

| Metric | How it's measured |
| --- | --- |
| **Screen time** | Time while the session is recently active, unlocked, and has at least one on display, when these signals are available. Time with no app focused or with an excluded app focused still counts under **Desktop & excluded**, without naming that app. Multiple monitors don't multiply time. |
| **App time** | Time assigned only to the currently focused Hyprland app class. App windows in the background don't accumulate time. Titles, URLs, keystrokes, screenshots, and notification contents are never stored. |
| **App switches** | A direct focus change between two different recorded apps while tracking is active. Returning from idle, lock, an excluded app, or an empty desktop does not count. |
| **Daily goal** | A target for the charts, not a blocker. Set it in Settings; enter `0` to turn it off. |

The **Overview** dashboard opens on **Today**, including Top applications. Use the 7-day or 30-day controls for a longer report. Use **Applications** to search, sort, inspect, or export the selected period. In **Activity**, select a bar to filter the day's apps by local hour. The current day is partial. Repeated local hours on a daylight-saving rollback are combined, and history uses the timezone in effect when it was recorded. Irminsul can't reconstruct usage from before it ran.

The default **three-minute idle grace period counts** as screen time. Uninterrupted playback can look idle in input-only mode; use `respect_idle_inhibitors = true` if your player marks playback with a Wayland idle inhibitor. Irminsul prefers native Wayland idle, lock, and display-power signals. Where Hyprland doesn't expose lock or display events, it uses read-only snapshots on session changes and, by default, at most once a minute while non-idle. Those transitions may be approximate or unavailable on older builds. With idle tracking enabled but no idle protocol, capture waits rather than counting unattended sessions. Suspend time is never intentionally counted: if clock readings show a suspend or clock jump, the ambiguous interval since the last event or checkpoint is discarded.

## Keep it lightweight

- One Rust I/O reactor, no tracing daemon, input-device access, screen capture, browser worker, or external service.
- Focus changes arrive over Hyprland's Unix socket. Native Wayland signals do not need periodic compositor requests. Compatibility snapshots can be disabled as a periodic check by setting `fallback_probe_seconds = 0`.
- Time and switch counts are aggregated in memory by **local date, hour, and app**. SQLite gets one transaction per active checkpoint (60 seconds by default), plus a final flush on normal shutdown. After idle or pause, the save timer stops once pending activity is saved. A crash can lose the unflushed batch.
- The UI has no framework or CDN. Data refreshes at most about once a minute automatically, only on a visible report ending today.

## Configure and theme

```sh
irminsul paths                         # actual XDG paths
irminsul theme init                    # copies editable files; never overwrites
${EDITOR:-nano} "${XDG_CONFIG_HOME:-$HOME/.config}/irminsul/web/theme.css"
```

The shipped dashboard works without these copies. You can override **the complete HTML, CSS, JavaScript, or favicon**, not just choose a preset. `palettes.css` defines the built-in presets and `theme.css` loads last; refresh the browser to see your file edits. Pick **Lavender**, **Sage**, **Rosewater**, **Oat**, **Blue Mist**, or **Soft Clay** in **Settings → Color palette**. Each palette works with **Light**, **Dark**, and **System** appearance. The palette and appearance are saved separately in your browser.

```css
/* web/theme.css: adjust only the light Sage preset */
:root[data-palette="sage"][data-theme="light"] {
  --paper: #f4f7f2;
  --rail: #eef2eb;
  --accent: #35614d;
  --accent-ink: #305643;
  --accent-soft: #e5eee4;
  --accent-panel: #e9f1e7;
  --accent-chart: #4e7f63;
  --accent-bars: #aec9b4;
  --radius: 12px;
}
```

Place optional local fonts, images, CSS, and scripts in `web/extras/` and reference them as `/extras/name.ext` (one directory level; up to 4 MiB per asset). The dashboard has a local-only Content Security Policy, so load scripts from separate files, not inline script tags. Delete an override to restore the embedded version. When upgrading, existing overrides still take precedence. The JSON data contract starts at `GET /api/v1/report?days=7`; pass `days=1`, `7`, or `30` and an optional `end=YYYY-MM-DD` (local date). See `src/tracker.rs` for the response types.

Daemon settings live in `~/.config/irminsul/config.toml` unless you use an XDG override. Set port, idle grace period, memory-to-disk checkpoint frequency, history retention, exclusions, friendly names, categories, and aliases there. Restart the daemon after changing TOML. For example:

```toml
# Set at the top, before any table. This won't erase existing history.
excluded_apps = ["com.example.private"]

[aliases]
"code-insiders" = "code"

[apps]
"code" = { name = "VS Code", category = "Productivity", icon = "code" }
```

The dashboard's saved goal overrides `daily_goal_minutes` in the TOML. The shipped retention is 180 days (`retention_days = 0` retains indefinitely).

## App categorization

Irminsul uses your `[apps]` category overrides first, then its catalog of common apps (including Helium). For unfamiliar apps it uses Linux `.desktop` files from your XDG application directories, including common Flatpak and Snap exports. It matches the desktop file ID or an unambiguous `StartupWMClass` and reads the declared category (for example, `WebBrowser`) and other launcher metadata. It does not use window titles, inspect browsing activity, or run anything from those files. A generic `Network` tag alone does not mean Browsing.

The launcher index is read once when the live daemon starts. Restart after installing apps or editing launcher metadata. Reports classify older records the same way as new ones, without changing measured time or stored class IDs. Apps with missing, generic, or conflicting metadata may still be **Other**. Click an app for its class ID and category source, or run `irminsul app-info CLASS`.

To correct an edge case, add a category-only entry to the existing `[apps]` table in `config.toml` and restart the daemon. Use the class shown in the report, not necessarily the visible app name:

```toml
[apps]
"my-portable-browser" = { category = "Browsing" }
```

## Privacy, troubleshooting, and removal

- The real dashboard binds to IPv4 loopback only. It rejects non-local Host headers and cross-origin preference writes. There is no cloud sync or network telemetry. **Other users or software on the same machine can still access the loopback dashboard**; on a shared machine, consider running it only when needed. Don't forward its port.
- Usage and saved preferences are in `${XDG_DATA_HOME:-$HOME/.local/share}/irminsul/usage.sqlite3`. Stop Irminsul before copying the database as a backup or removing it to clear all history. Back up the config separately. CSV exports are summaries, not full database backups.
- If the dashboard can't connect, run `irminsul doctor` in a Hyprland terminal, then `irminsul run` in the foreground for logs. For the installer's immediate start, the log is `${XDG_STATE_HOME:-$HOME/.local/state}/irminsul/daemon.log`. Another daemon or local app may already be using the port. To see and stop the old process: `pgrep -a -x irminsul`, then `kill -TERM PID` for the correct `run` process. Avoid SIGKILL if you want pending totals saved.
- If there are no reports yet, check the dashboard status and focus a few apps. The demo is a different process and never records usage.
- To uninstall: stop the daemon, remove the **BEGIN/END IRMINSUL AUTOSTART** block from the Hyprland config, and remove `~/.local/bin/irminsul`. Leave the config and database directories unless you want to delete your preferences and history.

## Demo and verification

```sh
cargo run --locked --release -- demo --port 4173    # http://127.0.0.1:4173/
cargo test --locked
cargo clippy --locked --all-targets -- -D warnings
python3 tests/categorization.py target/release/irminsul
python3 tests/mock_session.py target/release/irminsul
python3 tests/upgrade.py target/release/irminsul
```

Demo mode uses generated sample data in memory; it never opens the real database. The `demo --preview` flag is for hosting a sample-only preview and listens beyond loopback. This flag can't be used with `run`.

The wire-level smoke test uses simulated Wayland and Hyprland sockets to exercise the daemon, not a physical compositor. Native protocol availability and laptop power use vary by system. For a real-session check, switch focus, wait for idle, lock/unlock, and power your displays off/on, then verify that only active time advances.

Implementation references: [Hyprland IPC](https://wiki.hypr.land/ipc/), [Wayland idle notify](https://wayland.app/protocols/ext-idle-notify-v1), and [Hyprland lock notify](https://wayland.app/protocols/hyprland-lock-notify-v1). The vendored lock protocol copyright is in its XML; licenses for the bundled DM Sans and Fraunces fonts are in `web/fonts/`.
