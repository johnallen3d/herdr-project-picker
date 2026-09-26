use crate::{
    config::{normalize, Project, Registry},
    herdr::{Workspace, WorkspaceControl},
};
use eyre::{bail, eyre, Result, WrapErr};
use std::{
    collections::HashMap,
    io::Write,
    path::PathBuf,
    process::{Command, Stdio},
};

pub struct Entry {
    pub project: Project,
    pub path: PathBuf,
    pub workspace_id: Option<String>,
    pub exists: bool,
}

pub fn entries(registry: &Registry, workspaces: &[Workspace]) -> Result<Vec<Entry>> {
    let mut open = HashMap::new();
    for w in workspaces {
        if let Some(cwd) = &w.cwd {
            let normalized = normalize(&cwd.to_string_lossy(), &std::env::current_dir()?)?;
            open.entry(normalized)
                .or_insert_with(|| w.workspace_id.clone());
        }
    }
    registry
        .projects
        .iter()
        .map(|project| {
            let path = registry.path(project)?;
            Ok(Entry {
                project: project.clone(),
                exists: path.is_dir(),
                workspace_id: open.get(&path).cloned(),
                path,
            })
        })
        .collect()
}

pub fn select(entries: &[Entry]) -> Result<Option<usize>> {
    let mut child = Command::new("fzf")
        .args([
            "--height=100%",
            "--layout=reverse",
            "--border=none",
            "--prompt=Projects> ",
            "--delimiter=\t",
            "--with-nth=2..",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .wrap_err("fzf is required but was not found in PATH")?;
    let write_result = (|| -> Result<()> {
        let mut input = child
            .stdin
            .take()
            .ok_or_else(|| eyre!("Cannot write to fzf"))?;
        if entries.is_empty() {
            // Keep the popup open until the user dismisses it, rather than flashing away.
            writeln!(
                input,
                "0\tNo projects registered. Use Add current workspace as project or edit projects.toml."
            )?;
        }
        for (index, entry) in entries.iter().enumerate() {
            let marker = if !entry.exists {
                "!"
            } else if entry.workspace_id.is_some() {
                "●"
            } else {
                " "
            };
            // Index is identity, not user-editable display name. Strip control chars from display.
            let clean = |s: &str| s.replace(['\t', '\n', '\r'], " ");
            writeln!(
                input,
                "{index}\t{marker} {}\t{}",
                clean(&entry.project.name()),
                clean(&entry.path.to_string_lossy())
            )?;
        }
        Ok(())
    })();
    if let Err(e) = write_result {
        let _ = child.kill();
        let _ = child.wait();
        return Err(e);
    }
    let output = child.wait_with_output()?;
    if cancelled(output.status.code()) {
        return Ok(None);
    }
    if !output.status.success() {
        bail!("fzf exited with {}", output.status);
    }
    if entries.is_empty() {
        return Ok(None);
    }
    let selection = String::from_utf8(output.stdout)?;
    let Some(index) = selection
        .trim_end()
        .split('\t')
        .next()
        .filter(|s| !s.is_empty())
    else {
        return Ok(None);
    };
    let index: usize = index.parse().wrap_err("Invalid fzf selection")?;
    if index >= entries.len() {
        bail!("Invalid fzf project index");
    }
    Ok(Some(index))
}

fn cancelled(code: Option<i32>) -> bool {
    matches!(code, Some(1 | 130))
}

pub fn activate(herdr: &impl WorkspaceControl, entry: &Entry) -> Result<()> {
    if !entry.exists || !entry.path.is_dir() {
        bail!(
            "Project directory does not exist:\n{}",
            entry.path.display()
        );
    }
    if let Some(id) = &entry.workspace_id {
        herdr.focus_workspace(id)
    } else {
        herdr.create_workspace(&entry.path, &entry.project.name())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn selection_actions() {
        use std::cell::RefCell;
        struct Mock(RefCell<Vec<String>>);
        impl WorkspaceControl for Mock {
            fn focus_workspace(&self, id: &str) -> Result<()> {
                self.0.borrow_mut().push(format!("focus {id}"));
                Ok(())
            }
            fn create_workspace(&self, cwd: &std::path::Path, label: &str) -> Result<()> {
                self.0
                    .borrow_mut()
                    .push(format!("create {} {label}", cwd.display()));
                Ok(())
            }
        }
        let dir = std::env::temp_dir();
        let mock = Mock(RefCell::new(vec![]));
        let entry = Entry {
            project: Project {
                name: Some("tmp".into()),
                path: dir.display().to_string(),
            },
            path: dir.clone(),
            workspace_id: Some("w1".into()),
            exists: true,
        };
        activate(&mock, &entry).unwrap();
        let closed = Entry {
            workspace_id: None,
            ..entry
        };
        activate(&mock, &closed).unwrap();
        assert_eq!(
            &*mock.0.borrow(),
            &["focus w1", &format!("create {} tmp", dir.display())]
        );
        let missing = Entry {
            exists: false,
            ..closed
        };
        assert!(activate(&mock, &missing).is_err());
        assert_eq!(mock.0.borrow().len(), 2);
        assert!(cancelled(Some(130)));
        assert!(cancelled(Some(1)));
    }
    #[test]
    fn matches_workspace_and_missing() {
        let base = std::env::temp_dir().join(format!("picker-match-{}", std::process::id()));
        std::fs::create_dir_all(base.join("foo")).unwrap();
        let file = base.join("projects.toml");
        std::fs::write(
            &file,
            "version = 1\n[[sessions.personal.projects]]\npath = 'foo'\n[[sessions.personal.projects]]\npath = 'bar'\n",
        )
        .unwrap();
        let registry = Registry::load(file, "personal").unwrap();
        let workspaces = vec![Workspace {
            workspace_id: "w1".into(),
            label: "foo".into(),
            cwd: Some(base.join("foo")),
        }];
        let result = entries(&registry, &workspaces).unwrap();
        assert_eq!(result[0].workspace_id.as_deref(), Some("w1"));
        assert!(result[1].workspace_id.is_none());
        assert!(!result[1].exists);
    }
}
