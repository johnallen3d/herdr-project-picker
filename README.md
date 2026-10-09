# Herdr Project Picker

A session-aware project picker for Herdr: saved projects, local and remote open spaces, and known local Git worktrees in one searchable list. Selecting an open space focuses it on its server; selecting a closed local project or worktree creates a workspace at that path.

Requires Herdr 0.9.1+, `fzf`, and `git` on `PATH` for worktree discovery. Remote-space discovery requires Herdr 0.9.3+ on both machines and an already-running compatible remote server. Build and link locally:

```sh
cargo build --release
herdr plugin link .
```

### Saved SSH machines

The picker includes open spaces from **enabled saved SSH machine profiles** returned by `herdr machine list --json` on the host running the plugin. Each profile targets one remote session. When remote spaces are present, rows show searchable `[Local]` and `[machine label]` columns. Workspace IDs and paths remain machine-scoped: identical IDs or directory paths on different machines are separate rows. Remote paths are never checked against the local filesystem or scanned with local Git. Remote saved projects, worktrees, and individual agent rows are not discovered.

Remote queries run concurrently with a ten-second timeout per machine. Disabled profiles are skipped; failed connections show a warning without hiding local entries. On older Herdr versions, unavailable machine discovery also leaves the local picker usable. The picker does not install, start, restart, or reconnect remote servers interactively.

**Herdr 0.9.3 limitation:** selecting a remote row calls `herdr --machine <profile-id> workspace focus <workspace-id>` and shows a local notification. This focuses the remote server but **does not switch the visible client to that machine**; select the machine in Herdr's sidebar to view it. There is currently no public client-machine-switch API. Plugins themselves remain host-local; Herdr does not copy this plugin or its bookmarks to remote hosts.

### Local preview from the work profile

For a fast UI test before merging, without committing, pushing, or rebuilding Nix, run this from your work-profile terminal:

```sh
cd /Users/john.allen/dev/src/playground/herdr-project-picker
HERDR_SESSION=work \
HERDR_PLUGIN_CONFIG_DIR="$(herdr plugin config-dir herdr.project-picker)" \
cargo run -- picker
```

This uses the local debug binary and existing work-session project config, without replacing the installed plugin. **It fills the current terminal because it runs the picker directly, not inside the Herdr popup.** Press Escape to dismiss it; Enter still focuses or creates a workspace. Change `work` to `personal` to preview the personal session. Run it from the matching session: an inherited `HERDR_SOCKET_PATH` takes precedence over `HERDR_SESSION`.

To validate local changes at the original popup size (**80% width, 70% height**), build and link the local plugin instead:

```sh
cd /Users/john.allen/dev/src/playground/herdr-project-picker
cargo build --release
herdr plugin link .
```

Then use your usual project-picker shortcut in the work session (`prefix+p` with the example binding below). This needs no merge or Nix rebuild, but linking changes the registered plugin to use this local checkout; unlike the direct-terminal preview, it replaces the installed plugin registration. The popup dimensions have not changed.

For GitHub installation, the manifest builds the release binary with Cargo. Add a keybinding to Herdr's `config.toml`:

```toml
[[keys.command]]
key = "prefix+p"
type = "plugin_action"
command = "herdr.project-picker.open"
description = "projects"
```

Keep all sessions in **one writable file**, `$(herdr plugin config-dir herdr.project-picker)/projects.toml`. Edit this live file when changing projects; a Nix/Home Manager seed file installed only when it does not already exist will not update it on subsequent rebuilds:

```toml
version = 1

[[sessions.personal.projects]]
name = "calculate" # optional; defaults to directory basename
path = "~/src/calculate"

[[sessions.personal.projects]]
path = "~/src/conditions"

[[sessions.work.projects]]
path = "~/src/work-repo"
```

Saved projects are scoped to the invoking session and persist after workspaces close. The picker also shows **all open spaces in that session**, including those not configured as projects, plus Git worktrees attached to saved projects or open spaces. It does not scan the whole filesystem for repositories. Entries at the same directory are merged. Git worktrees are grouped under their saved project or open repository. The picker shows worktree names (or saved project/open space names) as the primary labels, with branches in a secondary column. The name column sizes to the longest label; neither column is truncated by the picker, and `fzf` handles horizontal scrolling in narrow popups. It searches names, branches, and displayed machine labels, never paths. Matches retain project/worktree order. `●` means the space is open (selection focuses it), `★` is a saved project that is not open, and `!` is a missing directory (cannot be opened). Open spaces with unavailable cwd are still focusable. Escape cancels. An empty list shows a dismissible message.

**Add current workspace as project** registers the calling pane's cwd, without duplicates. `herdr-project-picker list` lists only saved projects for the current session; outside Herdr, set `HERDR_SESSION=personal` (or `work`) and `HERDR_PLUGIN_CONFIG_DIR` to use it. To migrate an older, unscoped `[[projects]]` file, change each heading to `[[sessions.personal.projects]]` (or whichever session should own it). If you used the earlier separate-file format, move each session's projects into sections in this single file.

Relative paths in the config resolve against its directory; `~` is supported. Herdr 0.9.1 does not expose workspace root cwd; matching uses the first pane cwd reported by the public API and registration uses the invoking pane cwd. If panes change directories, this may not match the originally created workspace. Programmatic edits atomically rewrite the TOML; comments and formatting are not preserved.
