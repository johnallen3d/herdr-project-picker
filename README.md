# Herdr Project Picker

A session-aware project picker for Herdr: saved projects, open spaces, and known Git worktrees in one searchable list. Selecting an open space focuses it; selecting a closed project or worktree creates a workspace at that path.

Requires Herdr 0.9.1+, `fzf`, and `git` on `PATH` for worktree discovery. Build and link locally:

```sh
cargo build --release
herdr plugin link .
```

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

Saved projects are scoped to the invoking session and persist after workspaces close. The picker also shows **all open spaces in that session**, including those not configured as projects, plus Git worktrees attached to saved projects or open spaces. It does not scan the whole filesystem for repositories. Entries at the same directory are merged. Git worktrees are grouped under their saved project or open repository. The picker emphasizes name and branch; a dimmed, shortened location follows for context. `●` means the space is open (selection focuses it), `★` is a saved project that is not open, and `!` is a missing directory (cannot be opened). Open spaces with unavailable cwd are still focusable. Escape cancels. An empty list shows a dismissible message.

**Add current workspace as project** registers the calling pane's cwd, without duplicates. `herdr-project-picker list` lists only saved projects for the current session; outside Herdr, set `HERDR_SESSION=personal` (or `work`) and `HERDR_PLUGIN_CONFIG_DIR` to use it. To migrate an older, unscoped `[[projects]]` file, change each heading to `[[sessions.personal.projects]]` (or whichever session should own it). If you used the earlier separate-file format, move each session's projects into sections in this single file.

Relative paths in the config resolve against its directory; `~` is supported. Herdr 0.9.1 does not expose workspace root cwd; matching uses the first pane cwd reported by the public API and registration uses the invoking pane cwd. If panes change directories, this may not match the originally created workspace. Programmatic edits atomically rewrite the TOML; comments and formatting are not preserved.
