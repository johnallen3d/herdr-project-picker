use eyre::{bail, eyre, Result, WrapErr};
use serde::Deserialize;
use std::{
    env,
    path::{Path, PathBuf},
    process::Command,
};

#[derive(Clone, Debug, Deserialize)]
pub struct Workspace {
    pub workspace_id: String,
    pub label: String,
    #[serde(default)]
    pub cwd: Option<PathBuf>,
}

#[derive(Deserialize)]
struct Pane {
    pane_id: String,
    workspace_id: String,
    cwd: Option<PathBuf>,
}
#[derive(Deserialize)]
struct Snapshot {
    workspaces: Vec<Workspace>,
    panes: Vec<Pane>,
}
#[derive(Deserialize)]
struct Envelope {
    result: ResultBody,
}
#[derive(Deserialize)]
struct SessionList {
    sessions: Vec<Session>,
}
#[derive(Deserialize)]
struct Session {
    name: String,
    socket_path: PathBuf,
}

fn session_for_socket(json: &str, socket: &Path) -> Result<String> {
    let list: SessionList =
        serde_json::from_str(json).wrap_err("Unable to parse Herdr session list")?;
    list.sessions
        .into_iter()
        .find(|session| session.socket_path == socket)
        .map(|session| session.name)
        .ok_or_else(|| {
            eyre!(
                "Herdr session for socket {} was not found",
                socket.display()
            )
        })
}
#[derive(Deserialize)]
struct ResultBody {
    snapshot: Snapshot,
}

pub trait Runner {
    fn run(&self, args: &[&str]) -> Result<String>;
}

pub trait WorkspaceControl {
    fn focus_workspace(&self, id: &str) -> Result<()>;
    fn create_workspace(&self, cwd: &Path, label: &str) -> Result<()>;
}

pub struct Herdr {
    executable: PathBuf,
}
impl Herdr {
    pub fn new() -> Self {
        Self {
            executable: env::var_os("HERDR_BIN_PATH")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("herdr")),
        }
    }
    pub fn notify(&self, title: &str, body: &str) -> Result<()> {
        self.run(&["notification", "show", title, "--body", body])?;
        Ok(())
    }
    pub fn open_picker_popup(&self) -> Result<()> {
        let id = env::var("HERDR_PLUGIN_ID").unwrap_or_else(|_| "herdr.project-picker".into());
        self.run(&[
            "plugin",
            "pane",
            "open",
            "--plugin",
            &id,
            "--entrypoint",
            "picker",
            "--placement",
            "popup",
        ])?;
        Ok(())
    }
    pub fn session_name(&self) -> Result<String> {
        if let Some(socket) = env::var_os("HERDR_SOCKET_PATH") {
            // Socket identity is authoritative; HERDR_SESSION is not guaranteed to be
            // injected into plugin commands and may differ from a CLI's selected session.
            let list = self.run(&["session", "list", "--json"])?;
            return session_for_socket(&list, Path::new(&socket));
        }
        Ok(env::var("HERDR_SESSION").unwrap_or_else(|_| "default".into()))
    }
    pub fn workspaces(&self) -> Result<Vec<Workspace>> {
        Ok(self.snapshot()?.0)
    }
    pub fn current_workspace(&self, id: &str) -> Result<Workspace> {
        let (workspaces, panes) = self.snapshot()?;
        let mut workspace = workspaces
            .into_iter()
            .find(|w| w.workspace_id == id)
            .ok_or_else(|| eyre!("Workspace {id} was not found"))?;
        // Herdr 0.9.1 WorkspaceInfo has no cwd. Prefer invoking pane's cwd when available.
        if let Ok(pane_id) = env::var("HERDR_PANE_ID") {
            if let Some(pane) = panes
                .iter()
                .find(|p| p.pane_id == pane_id && p.workspace_id == id)
            {
                workspace.cwd = pane.cwd.clone();
            }
        }
        Ok(workspace)
    }
    fn snapshot(&self) -> Result<(Vec<Workspace>, Vec<Pane>)> {
        let text = self
            .run(&["api", "snapshot"])
            .wrap_err("Unable to query Herdr workspaces")?;
        let mut snapshot: Snapshot = serde_json::from_str::<Envelope>(&text)
            .wrap_err("Unable to parse Herdr API snapshot")?
            .result
            .snapshot;
        // WorkspaceInfo has no cwd; use the earliest pane's cwd as public API approximation.
        for workspace in &mut snapshot.workspaces {
            workspace.cwd = snapshot
                .panes
                .iter()
                .find(|p| p.workspace_id == workspace.workspace_id)
                .and_then(|p| p.cwd.clone());
        }
        Ok((snapshot.workspaces, snapshot.panes))
    }
}
impl WorkspaceControl for Herdr {
    fn focus_workspace(&self, id: &str) -> Result<()> {
        self.run(&["workspace", "focus", id])?;
        Ok(())
    }
    fn create_workspace(&self, cwd: &Path, label: &str) -> Result<()> {
        self.run(&[
            "workspace",
            "create",
            "--cwd",
            &cwd.to_string_lossy(),
            "--label",
            label,
            "--focus",
        ])?;
        Ok(())
    }
}
impl Runner for Herdr {
    fn run(&self, args: &[&str]) -> Result<String> {
        let output = Command::new(&self.executable)
            .args(args)
            .output()
            .wrap_err_with(|| format!("Could not run {}", self.executable.display()))?;
        if !output.status.success() {
            bail!(
                "Herdr {} failed: {}",
                args.join(" "),
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
        String::from_utf8(output.stdout).wrap_err("Herdr returned non-UTF-8 output")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn selects_session_by_socket() {
        let list = r#"{"sessions":[{"name":"personal","socket_path":"/tmp/personal.sock"},{"name":"work","socket_path":"/tmp/work.sock"}]}"#;
        assert_eq!(
            session_for_socket(list, Path::new("/tmp/work.sock")).unwrap(),
            "work"
        );
        assert!(session_for_socket(list, Path::new("/tmp/other.sock")).is_err());
    }
    #[test]
    fn snapshot_shape_matches_installed_api() {
        let envelope: Envelope = serde_json::from_str(r#"{"result":{"snapshot":{"workspaces":[{"workspace_id":"w1","label":"foo"}],"panes":[{"pane_id":"w1:p1","workspace_id":"w1","cwd":"/code/foo"}]}}}"#).unwrap();
        assert_eq!(
            envelope.result.snapshot.panes[0].cwd.as_deref(),
            Some(Path::new("/code/foo"))
        );
    }
}
