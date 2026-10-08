use eyre::{bail, eyre, Result, WrapErr};
use serde::Deserialize;
use std::{
    env,
    io::Read,
    path::{Path, PathBuf},
    process::{Command, Output, Stdio},
    thread,
    time::{Duration, Instant},
};

#[derive(Clone, Debug, Deserialize)]
pub struct Workspace {
    pub workspace_id: String,
    pub label: String,
    #[serde(default)]
    pub cwd: Option<PathBuf>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct Machine {
    pub id: String,
    pub label: String,
    pub enabled: bool,
}

pub struct MachineWorkspaces {
    pub machine: Machine,
    pub workspaces: Vec<Workspace>,
}

#[derive(Default)]
pub struct RemoteDiscovery {
    pub machines: Vec<MachineWorkspaces>,
    pub warnings: Vec<String>,
}

// Saved profiles are local configuration; their IDs, not SSH hostnames or pane IDs,
// are the selectors understood by Herdr's forwarding API.
pub fn discover_remote(runner: &(impl Runner + Sync)) -> RemoteDiscovery {
    let mut discovery = RemoteDiscovery::default();
    let machines = runner.run(&["machine", "list", "--json"]).and_then(|text| {
        serde_json::from_str::<Vec<Machine>>(&text).wrap_err("Unable to parse Herdr machine list")
    });
    let machines = match machines {
        Ok(machines) => machines,
        Err(error) => {
            eprintln!("Remote discovery unavailable: {error:?}");
            discovery
                .warnings
                .push("Remote discovery unavailable".into());
            return discovery;
        }
    };
    // Check machines concurrently so an unreachable host does not hold up every
    // other host. Herdr::run bounds each remote command's wall-clock duration.
    thread::scope(|scope| {
        let pending: Vec<_> = machines
            .into_iter()
            .filter(|m| m.enabled)
            .map(|machine| {
                scope.spawn(move || {
                    let result = runner
                        .run(&["--machine", &machine.id, "api", "snapshot"])
                        .and_then(|text| parse_snapshot(&text))
                        .map(|(workspaces, _)| workspaces);
                    (machine, result)
                })
            })
            .collect();
        for pending in pending {
            match pending.join() {
                Ok((machine, Ok(workspaces))) => discovery.machines.push(MachineWorkspaces {
                    machine,
                    workspaces,
                }),
                Ok((machine, Err(error))) => {
                    eprintln!("{}: {error:?}", machine.label);
                    discovery
                        .warnings
                        .push(format!("{} unavailable", machine.label));
                }
                Err(_) => discovery.warnings.push("Remote discovery failed".into()),
            }
        }
    });
    discovery
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
    fn focus_remote_workspace(&self, machine: &Machine, id: &str) -> Result<()>;
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
        parse_snapshot(&text)
    }
}
impl WorkspaceControl for Herdr {
    fn focus_workspace(&self, id: &str) -> Result<()> {
        self.run(&["workspace", "focus", id])?;
        Ok(())
    }
    fn focus_remote_workspace(&self, machine: &Machine, id: &str) -> Result<()> {
        self.run(&["--machine", &machine.id, "workspace", "focus", id])?;
        // Herdr 0.9.3 has no public API for activating another endpoint in a
        // running TUI. Remote focus must not be presented as a client switch.
        self.notify(
            "Project Picker",
            &format!(
                "Focused space on {}. Select {} in the sidebar to view it.",
                machine.label, machine.label
            ),
        )?;
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
        let mut command = Command::new(&self.executable);
        command.args(args);
        let output = if args.first() == Some(&"--machine") {
            bounded_output(&mut command, Duration::from_secs(10))
        } else {
            command.output().map_err(Into::into)
        }
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

fn parse_snapshot(text: &str) -> Result<(Vec<Workspace>, Vec<Pane>)> {
    let mut snapshot = serde_json::from_str::<Envelope>(text)
        .wrap_err("Unable to parse Herdr API snapshot")?
        .result
        .snapshot;
    // WorkspaceInfo has no cwd; use the earliest pane's cwd as public API approximation.
    for workspace in &mut snapshot.workspaces {
        if workspace.cwd.is_none() {
            workspace.cwd = snapshot
                .panes
                .iter()
                .find(|p| p.workspace_id == workspace.workspace_id)
                .and_then(|p| p.cwd.clone());
        }
    }
    Ok((snapshot.workspaces, snapshot.panes))
}

fn bounded_output(command: &mut Command, timeout: Duration) -> Result<Output> {
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let mut stdout = child.stdout.take().ok_or_else(|| eyre!("Missing stdout"))?;
    let mut stderr = child.stderr.take().ok_or_else(|| eyre!("Missing stderr"))?;
    // Drain pipes while waiting: snapshots can exceed the OS pipe capacity.
    // Channels let us bound pipe EOF too, if a descendant inherits the pipes.
    let (out_tx, out) = std::sync::mpsc::channel();
    let (err_tx, err) = std::sync::mpsc::channel();
    thread::spawn(move || {
        let mut bytes = Vec::new();
        let _ = out_tx.send(stdout.read_to_end(&mut bytes).map(|_| bytes));
    });
    thread::spawn(move || {
        let mut bytes = Vec::new();
        let _ = err_tx.send(stderr.read_to_end(&mut bytes).map(|_| bytes));
    });
    let start = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if start.elapsed() < timeout => thread::sleep(Duration::from_millis(20)),
            result => {
                let _ = child.kill();
                let _ = child.wait();
                // Readers exit when all pipe owners close. Do not wait for a
                // descendant here: the UI timeout must stay bounded.
                if let Err(error) = result {
                    return Err(error.into());
                }
                bail!(
                    "Remote Herdr command timed out after {} seconds",
                    timeout.as_secs()
                );
            }
        }
    };
    Ok(Output {
        status,
        stdout: out
            .recv_timeout(timeout.saturating_sub(start.elapsed()))
            .map_err(|_| eyre!("Remote Herdr command timed out reading stdout"))??,
        stderr: err
            .recv_timeout(timeout.saturating_sub(start.elapsed()))
            .map_err(|_| eyre!("Remote Herdr command timed out reading stderr"))??,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn discovers_enabled_machines_and_isolates_failures() {
        use std::sync::Mutex;
        struct Mock(Mutex<Vec<Vec<String>>>);
        impl Runner for Mock {
            fn run(&self, args: &[&str]) -> Result<String> {
                self.0
                    .lock()
                    .unwrap()
                    .push(args.iter().map(|s| s.to_string()).collect());
                match args {
                    ["machine", "list", "--json"] => Ok(r#"[
                        {"id":"good","label":"Work box","enabled":true},
                        {"id":"disabled","label":"Disabled","enabled":false},
                        {"id":"bad","label":"Offline","enabled":true},
                        {"id":"invalid","label":"Invalid","enabled":true}
                    ]"#.into()),
                    ["--machine", "good", "api", "snapshot"] => Ok(r#"{"result":{"snapshot":{"workspaces":[{"workspace_id":"w1","label":"remote"}],"panes":[{"pane_id":"w1:p1","workspace_id":"w1","cwd":"/remote/project"}]}}}"#.into()),
                    ["--machine", "bad", "api", "snapshot"] => bail!("SSH unavailable"),
                    ["--machine", "invalid", "api", "snapshot"] => Ok("not JSON".into()),
                    _ => panic!("unexpected command: {args:?}"),
                }
            }
        }
        let mock = Mock(Mutex::new(vec![]));
        let remote = discover_remote(&mock);
        assert_eq!(remote.machines.len(), 1);
        assert_eq!(remote.machines[0].machine.id, "good");
        assert_eq!(
            remote.machines[0].workspaces[0].cwd.as_deref(),
            Some(Path::new("/remote/project"))
        );
        assert_eq!(
            remote.warnings,
            ["Offline unavailable", "Invalid unavailable"]
        );
        assert_eq!(mock.0.lock().unwrap().len(), 4);
    }

    #[test]
    fn unavailable_machine_catalog_does_not_fail_picker() {
        struct Mock;
        impl Runner for Mock {
            fn run(&self, _args: &[&str]) -> Result<String> {
                bail!("Older Herdr has no machine command")
            }
        }
        let remote = discover_remote(&Mock);
        assert!(remote.machines.is_empty());
        assert_eq!(remote.warnings, ["Remote discovery unavailable"]);
    }

    #[test]
    fn remote_focus_uses_profile_id_and_notifies_locally() {
        use std::{fs, os::unix::fs::PermissionsExt};
        let dir = env::temp_dir().join(format!("picker-routing-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let executable = dir.join("herdr");
        fs::write(
            &executable,
            "#!/bin/sh\nprintf '%s\\n' \"$*\" >> \"$(dirname \"$0\")/commands\"\nprintf '{}\\n'\n",
        )
        .unwrap();
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o700)).unwrap();
        let log = dir.join("commands");
        let _ = fs::remove_file(&log);
        let herdr = Herdr { executable };
        let machine = Machine {
            id: "opaque-profile".into(),
            label: "Friendly label".into(),
            enabled: true,
        };
        herdr.focus_remote_workspace(&machine, "w1").unwrap();
        let commands = fs::read_to_string(log).unwrap();
        let lines: Vec<_> = commands.lines().collect();
        assert_eq!(lines[0], "--machine opaque-profile workspace focus w1");
        assert!(lines[1].starts_with("notification show Project Picker --body"));
        assert!(lines[1].contains("Select Friendly label in the sidebar"));
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn remote_process_timeout_and_large_output() {
        let start = Instant::now();
        let error = bounded_output(
            Command::new("sh").args(["-c", "exec sleep 2"]),
            Duration::from_millis(40),
        )
        .unwrap_err();
        assert!(error.to_string().contains("timed out"));
        assert!(start.elapsed() < Duration::from_secs(1));
        // The direct child can exit before its descendant releases stdout.
        let start = Instant::now();
        assert!(bounded_output(
            Command::new("sh").args(["-c", "sleep 2 & printf ready"]),
            Duration::from_millis(40)
        )
        .is_err());
        assert!(start.elapsed() < Duration::from_secs(1));
        let output = bounded_output(
            Command::new("sh").args(["-c", "head -c 200000 /dev/zero"]),
            Duration::from_secs(5),
        )
        .unwrap();
        assert!(output.status.success());
        assert_eq!(output.stdout.len(), 200000);
    }

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
