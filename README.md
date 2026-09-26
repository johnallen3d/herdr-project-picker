# Herdr Project Picker

Persistent project bookmarks for Herdr. Pick a project to focus its existing workspace or create one.

Requires Herdr 0.9.1+ and `fzf` on `PATH`. Build and link locally:

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

The picker and **Add current workspace as project** action use only the invoking session's projects. Projects persist after workspaces close. To migrate an older, unscoped `[[projects]]` file, change each heading to `[[sessions.personal.projects]]` (or whichever session should own it). If you used the earlier separate-file format, move each session's projects into sections in this single file.

`●` marks an open workspace; `!` marks a missing directory (cannot be opened). Escape cancels. An empty session shows a dismissible message instead of immediately closing the popup. **Add current workspace as project** registers the calling pane's cwd, without duplicates. `herdr-project-picker list` lists projects for the current session; outside Herdr, set `HERDR_SESSION=personal` (or `work`) and `HERDR_PLUGIN_CONFIG_DIR` to use it. Relative paths in the config resolve against its directory; `~` is supported. Herdr 0.9.1 does not expose workspace root cwd; matching uses the first pane cwd reported by the public API and registration uses the invoking pane cwd. If panes change directories, this may not match the originally created workspace. Programmatic edits atomically rewrite the TOML; comments and formatting are not preserved.
