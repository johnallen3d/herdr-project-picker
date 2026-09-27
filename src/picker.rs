use crate::{
    config::{normalize, Registry},
    herdr::{Workspace, WorkspaceControl},
};
use eyre::{bail, eyre, Result, WrapErr};
use std::{
    collections::{HashMap, HashSet},
    env,
    ffi::OsStr,
    io::Write,
    os::unix::ffi::OsStrExt,
    path::PathBuf,
    process::{Command, Stdio},
};
use unicode_width::UnicodeWidthChar;

pub struct Entry {
    pub name: String,
    pub path: Option<PathBuf>,
    pub workspace_id: Option<String>,
    pub exists: bool,
    pub pinned: bool,
    pub worktree: bool,
    pub repo: Option<PathBuf>,
    pub branch: Option<String>,
}

struct GitWorktree {
    path: PathBuf,
    repo: PathBuf,
    branch: Option<String>,
}

// Discover worktrees attached to repositories represented by a saved project or open space.
// Git's NUL-delimited porcelain output supports spaces and newlines in worktree paths.
fn worktrees(roots: &[PathBuf]) -> Vec<GitWorktree> {
    let mut seen_repos = HashSet::new();
    let mut paths = Vec::new();
    for root in roots {
        if !root.is_dir() {
            continue;
        }
        let Ok(common) = Command::new("git")
            .args(["-C"])
            .arg(root)
            .args(["rev-parse", "--git-common-dir"])
            .output()
        else {
            continue;
        };
        if !common.status.success() {
            continue;
        }
        let common = PathBuf::from(OsStr::from_bytes(common.stdout.trim_ascii()));
        let common = if common.is_absolute() {
            common
        } else {
            root.join(common)
        };
        let Ok(common) = common.canonicalize() else {
            continue;
        };
        if !seen_repos.insert(common.clone()) {
            continue;
        }
        let Ok(output) = Command::new("git")
            .args(["-C"])
            .arg(root)
            .args(["worktree", "list", "--porcelain", "-z"])
            .output()
        else {
            continue;
        };
        if !output.status.success() {
            continue;
        }
        let mut current: Option<GitWorktree> = None;
        for field in output.stdout.split(|b| *b == 0) {
            if let Some(path) = field.strip_prefix(b"worktree ") {
                if let Some(tree) = current.take() {
                    paths.push(tree);
                }
                current = Some(GitWorktree {
                    path: PathBuf::from(OsStr::from_bytes(path)),
                    repo: common.clone(),
                    branch: None,
                });
            } else if let Some(branch) = field.strip_prefix(b"branch refs/heads/") {
                if let Some(tree) = &mut current {
                    tree.branch = Some(String::from_utf8_lossy(branch).into_owned());
                }
            }
        }
        if let Some(tree) = current {
            paths.push(tree);
        }
    }
    paths
}

pub fn entries(registry: &Registry, workspaces: &[Workspace]) -> Result<Vec<Entry>> {
    let base = std::env::current_dir()?;
    let mut entries = Vec::new();
    let mut by_path = HashMap::new();
    let mut roots = Vec::new();

    // Configured projects come first; open spaces and worktrees decorate them.
    for project in &registry.projects {
        let path = registry.path(project)?;
        roots.push(path.clone());
        by_path.insert(path.clone(), entries.len());
        entries.push(Entry {
            name: project.name(),
            exists: path.is_dir(),
            path: Some(path),
            workspace_id: None,
            pinned: true,
            worktree: false,
            repo: None,
            branch: None,
        });
    }
    for workspace in workspaces {
        let path = workspace
            .cwd
            .as_ref()
            .map(|cwd| normalize(&cwd.to_string_lossy(), &base))
            .transpose()?;
        if let Some(path) = &path {
            roots.push(path.clone());
            if let Some(index) = by_path.get(path) {
                let entry: &mut Entry = &mut entries[*index];
                // Preserve the configured display name when an open space matches it.
                if entry.workspace_id.is_none() {
                    entry.workspace_id = Some(workspace.workspace_id.clone());
                }
                continue;
            }
            by_path.insert(path.clone(), entries.len());
        }
        entries.push(Entry {
            name: workspace.label.clone(),
            exists: path.as_ref().is_some_and(|p| p.is_dir()),
            path,
            workspace_id: Some(workspace.workspace_id.clone()),
            pinned: false,
            worktree: false,
            repo: None,
            branch: None,
        });
    }
    let mut trees = worktrees(&roots);
    for tree in &mut trees {
        tree.path = normalize(&tree.path.to_string_lossy(), &base)?;
    }
    for tree in &trees {
        let path = tree.path.clone();
        if let Some(index) = by_path.get(&path) {
            entries[*index].worktree = true;
            entries[*index].repo = Some(tree.repo.clone());
            entries[*index].branch = tree.branch.clone();
            continue;
        }
        by_path.insert(path.clone(), entries.len());
        entries.push(Entry {
            name: path
                .file_name()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| path.display().to_string()),
            exists: path.is_dir(),
            path: Some(path),
            workspace_id: None,
            pinned: false,
            worktree: true,
            repo: Some(tree.repo.clone()),
            branch: tree.branch.clone(),
        });
    }
    // A pane may have cd'd into a subdirectory of its worktree. Keep that
    // space visible, grouped with its checkout, even when paths do not match.
    for entry in &mut entries {
        if entry.repo.is_some() {
            continue;
        }
        if let Some(path) = &entry.path {
            if let Some(tree) = trees
                .iter()
                .filter(|tree| path.starts_with(&tree.path))
                .max_by_key(|tree| tree.path.components().count())
            {
                entry.repo = Some(tree.repo.clone());
                entry.branch = tree.branch.clone();
            }
        }
    }
    Ok(entries)
}

// Keep entries from the same repository together without losing the saved
// project order. Unrelated open spaces appear after saved projects.
fn ordered(entries: &[Entry]) -> Vec<(usize, bool)> {
    let mut seen = HashSet::new();
    let mut rows = Vec::new();
    for (index, entry) in entries.iter().enumerate() {
        if !seen.insert(index) {
            continue;
        }
        rows.push((index, false));
        if let Some(repo) = &entry.repo {
            for (other_index, other) in entries.iter().enumerate() {
                if other_index != index
                    && other.repo.as_ref() == Some(repo)
                    && seen.insert(other_index)
                {
                    rows.push((other_index, true));
                }
            }
        }
    }
    rows
}

// Width is measured in terminal cells, not bytes, so Unicode names align.
fn fit(text: &str, width: usize) -> String {
    let mut result = String::new();
    let mut used = 0;
    let clean: String = text
        .chars()
        .map(|ch| if ch.is_control() { ' ' } else { ch })
        .collect();
    let total: usize = clean.chars().map(|ch| ch.width().unwrap_or(0)).sum();
    for ch in clean.chars() {
        let next = ch.width().unwrap_or(0);
        if used + next > width || (total > width && used + next >= width) {
            break;
        }
        result.push(ch);
        used += next;
    }
    if total > width {
        result.push('…');
        used += 1;
    }
    result.push_str(&" ".repeat(width.saturating_sub(used)));
    result
}

fn fit_path(path: &str, width: usize) -> String {
    let clean: String = path
        .chars()
        .map(|ch| if ch.is_control() { ' ' } else { ch })
        .collect();
    let total: usize = clean.chars().map(|ch| ch.width().unwrap_or(0)).sum();
    if total <= width {
        return fit(&clean, width);
    }
    let mut suffix = String::new();
    let mut used = 1; // ellipsis
    for ch in clean.chars().rev() {
        let next = ch.width().unwrap_or(0);
        if used + next > width {
            break;
        }
        suffix.insert(0, ch);
        used += next;
    }
    format!("…{suffix}")
}

fn display(entry: &Entry, child: bool) -> String {
    let marker = if entry.workspace_id.is_some() {
        '●'
    } else if !entry.exists {
        '!'
    } else if entry.pinned {
        '★'
    } else {
        ' '
    };
    let label = if child {
        format!("  └ {}", entry.branch.as_deref().unwrap_or(&entry.name))
    } else {
        entry.name.clone()
    };
    let branch = if child {
        ""
    } else {
        entry.branch.as_deref().unwrap_or("")
    };
    // The name and branch are the primary information; location is only context.
    // Avoid repeating the basename when it is already the visible label.
    let path = entry.path.as_ref().map_or_else(
        || "(cwd unavailable)".into(),
        |path| {
            let location = if path
                .file_name()
                .is_some_and(|name| name == entry.name.as_str())
            {
                path.parent().unwrap_or(path)
            } else {
                path.as_path()
            };
            let home = env::var_os("HOME").map(PathBuf::from);
            home.and_then(|home| {
                location
                    .strip_prefix(home)
                    .ok()
                    .map(|relative| format!("~/{}", relative.display()))
            })
            .unwrap_or_else(|| location.display().to_string())
        },
    );
    let branch = if branch.is_empty() {
        String::new()
    } else {
        format!("  [{}]", fit(branch, 22).trim_end())
    };
    format!(
        "{marker} {}{branch}  \x1b[2m{}\x1b[0m",
        fit(&label, 36).trim_end(),
        fit_path(&path, 32).trim_end()
    )
}

pub fn select(entries: &[Entry]) -> Result<Option<usize>> {
    let mut child = Command::new("fzf")
        .args([
            "--height=100%",
            "--layout=reverse",
            "--border=none",
            "--prompt=Projects> ",
            "--delimiter=\t",
            "--with-nth=2",
            "--ansi",
            "--no-sort",
            "--header=● open space   ★ saved project   ! missing directory   Enter: focus/open",
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
            writeln!(input, "0\tNo projects, open spaces, or worktrees found.")?;
        }
        for (index, child) in ordered(entries) {
            // Index is identity, not user-editable display name.
            writeln!(input, "{index}\t{}", display(&entries[index], child))?;
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
    if let Some(id) = &entry.workspace_id {
        return herdr.focus_workspace(id);
    }
    let path = entry
        .path
        .as_ref()
        .ok_or_else(|| eyre!("Project has no directory"))?;
    if !entry.exists || !path.is_dir() {
        bail!("Project directory does not exist:\n{}", path.display());
    }
    herdr.create_workspace(path, &entry.name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{fs, path::Path};

    fn test_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("picker-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn path_is_secondary_to_name_and_branch() {
        let entry = Entry {
            name: "calculate".into(),
            path: Some(PathBuf::from("/very/long/parent/calculate")),
            workspace_id: None,
            exists: true,
            pinned: true,
            worktree: false,
            repo: None,
            branch: Some("main".into()),
        };
        let line = display(&entry, false);
        assert!(line.contains("★ calculate  [main]"));
        assert!(line.contains("\x1b[2m/very/long/parent\x1b[0m"));
        assert!(!line.contains("/very/long/parent/calculate"));
    }

    #[test]
    fn selection_actions() {
        use std::cell::RefCell;
        struct Mock(RefCell<Vec<String>>);
        impl WorkspaceControl for Mock {
            fn focus_workspace(&self, id: &str) -> Result<()> {
                self.0.borrow_mut().push(format!("focus {id}"));
                Ok(())
            }
            fn create_workspace(&self, cwd: &Path, label: &str) -> Result<()> {
                self.0
                    .borrow_mut()
                    .push(format!("create {} {label}", cwd.display()));
                Ok(())
            }
        }
        let dir = std::env::temp_dir();
        let mock = Mock(RefCell::new(vec![]));
        let entry = Entry {
            name: "tmp".into(),
            path: Some(dir.clone()),
            workspace_id: Some("w1".into()),
            exists: true,
            pinned: true,
            worktree: false,
            repo: None,
            branch: None,
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
        let no_cwd = Entry {
            path: None,
            workspace_id: Some("w2".into()),
            ..missing
        };
        activate(&mock, &no_cwd).unwrap();
        assert_eq!(mock.0.borrow().last().unwrap(), "focus w2");
        assert!(cancelled(Some(130)));
        assert!(cancelled(Some(1)));
    }

    #[test]
    fn combines_projects_spaces_and_worktrees_without_duplicates() {
        let base = test_dir("combine");
        let repo = base.join("repo");
        fs::create_dir_all(&repo).unwrap();
        assert!(Command::new("git")
            .args(["init", "-q"])
            .arg(&repo)
            .status()
            .unwrap()
            .success());
        let tree = base.join("feature");
        assert!(Command::new("git")
            .args(["-C"])
            .arg(&repo)
            .args([
                "-c",
                "user.name=Test",
                "-c",
                "user.email=test@example.com",
                "commit",
                "--allow-empty",
                "-qm",
                "init"
            ])
            .status()
            .unwrap()
            .success());
        assert!(Command::new("git")
            .args(["-C"])
            .arg(&repo)
            .args(["worktree", "add", "-q", "--detach"])
            .arg(&tree)
            .status()
            .unwrap()
            .success());
        let file = base.join("projects.toml");
        fs::write(&file, "[[sessions.personal.projects]]\nname = 'saved'\npath = 'repo'\n[[sessions.personal.projects]]\npath = 'missing'\n").unwrap();
        let registry = Registry::load(file.clone(), "personal").unwrap();
        let workspaces = vec![
            Workspace {
                workspace_id: "w1".into(),
                label: "open repo".into(),
                cwd: Some(repo.clone()),
            },
            Workspace {
                workspace_id: "w2".into(),
                label: "loose".into(),
                cwd: Some(base.clone()),
            },
            Workspace {
                workspace_id: "w3".into(),
                label: "no cwd".into(),
                cwd: None,
            },
            Workspace {
                workspace_id: "w4".into(),
                label: "feature space".into(),
                cwd: Some(tree.clone()),
            },
        ];
        let result = entries(&registry, &workspaces).unwrap();
        assert_eq!(result.len(), 5);
        assert_eq!(result[0].name, "saved");
        assert!(result[0].pinned && result[0].worktree);
        assert_eq!(result[0].workspace_id.as_deref(), Some("w1"));
        assert!(!result[1].exists);
        assert_eq!(result[2].workspace_id.as_deref(), Some("w2"));
        assert_eq!(result[3].path, None);
        assert_eq!(result[4].workspace_id.as_deref(), Some("w4"));
        assert!(result[4].worktree);
        assert_eq!(result[4].branch, None); // detached checkout
        assert!(display(&result[4], true).contains("└ feature"));
        assert_eq!(
            ordered(&result),
            vec![(0, false), (4, true), (1, false), (2, false), (3, false)]
        );
        // A closed worktree is discovered through the configured repository.
        let closed = entries(&registry, &workspaces[..3]).unwrap();
        assert_eq!(
            closed.last().unwrap().path.as_deref(),
            Some(tree.canonicalize().unwrap().as_path())
        );
        assert!(closed.last().unwrap().worktree);
        // Starting from a linked worktree still finds the main checkout.
        fs::write(
            &file,
            format!(
                "[[sessions.personal.projects]]\npath = '{}'\n",
                tree.display()
            ),
        )
        .unwrap();
        let linked = Registry::load(file.clone(), "personal").unwrap();
        let from_linked = entries(&linked, &[]).unwrap();
        assert_eq!(from_linked.len(), 2);
        assert_eq!(
            from_linked[1].path.as_deref(),
            Some(repo.canonicalize().unwrap().as_path())
        );
        assert!(from_linked[1].worktree);
        let named = base.join("checkout-named-differently");
        assert!(Command::new("git")
            .args(["-C"])
            .arg(&repo)
            .args(["worktree", "add", "-q", "-b", "feature/named"])
            .arg(&named)
            .status()
            .unwrap()
            .success());
        let from_linked = entries(&linked, &[]).unwrap();
        let branch = from_linked
            .iter()
            .find(|entry| entry.path.as_deref() == Some(named.canonicalize().unwrap().as_path()))
            .unwrap();
        assert_eq!(branch.branch.as_deref(), Some("feature/named"));
        assert!(display(branch, true).contains("└ feature/named"));
        assert_eq!(fit("a界cdef", 4), "a界…");
        assert_eq!(fit_path("/very/long/path/to/repo", 12), "…ath/to/repo");
        let work = Registry::load(file, "work").unwrap();
        assert!(entries(&work, &[]).unwrap().is_empty());
        assert_eq!(entries(&work, &workspaces[..1]).unwrap().len(), 3);
    }
}
