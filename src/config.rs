use eyre::{bail, eyre, Result, WrapErr};
use serde::{Deserialize, Serialize};
use std::{
    env, fs,
    path::{Component, Path, PathBuf},
};

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Project {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub path: String,
}

impl Project {
    pub fn name(&self) -> String {
        self.name.clone().unwrap_or_else(|| {
            Path::new(self.path.trim_end_matches('/'))
                .file_name()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| self.path.clone())
        })
    }
}

pub fn normalize(path: &str, base: &Path) -> Result<PathBuf> {
    let expanded = if path == "~" || path.starts_with("~/") {
        let home = env::var_os("HOME").ok_or_else(|| eyre!("HOME is not set"))?;
        PathBuf::from(home).join(path.strip_prefix("~/").unwrap_or(""))
    } else {
        PathBuf::from(path)
    };
    let absolute = if expanded.is_absolute() {
        expanded
    } else {
        base.join(expanded)
    };
    let mut cleaned = PathBuf::new();
    for component in absolute.components() {
        match component {
            Component::ParentDir => {
                cleaned.pop();
            }
            Component::CurDir => (),
            _ => cleaned.push(component.as_os_str()),
        }
    }
    if cleaned.exists() {
        Ok(cleaned
            .canonicalize()
            .wrap_err_with(|| format!("Cannot resolve {}", cleaned.display()))?)
    } else {
        Ok(cleaned)
    }
}

fn valid_session_name(session: &str) -> bool {
    // Keep session names safe as TOML table keys.
    !session.is_empty()
        && session != "."
        && session != ".."
        && session
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
}

pub fn config_path() -> Result<PathBuf> {
    Ok(PathBuf::from(
        env::var_os("HERDR_PLUGIN_CONFIG_DIR")
            .ok_or_else(|| eyre!("HERDR_PLUGIN_CONFIG_DIR is not set"))?,
    )
    .join("projects.toml"))
}

pub struct Registry {
    pub projects: Vec<Project>,
    document: toml::Table,
    file: PathBuf,
    session: String,
}

impl Registry {
    pub fn load(file: PathBuf, session: &str) -> Result<Self> {
        if !valid_session_name(session) {
            bail!("Invalid Herdr session name: {session}");
        }
        let document: toml::Table = match fs::read_to_string(&file) {
            Ok(text) => toml::from_str(&text)
                .wrap_err_with(|| format!("Unable to parse {}", file.display()))?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => toml::Table::new(),
            Err(e) => return Err(e).wrap_err_with(|| format!("Unable to read {}", file.display())),
        };
        let version = match document.get("version") {
            Some(value) => value
                .as_integer()
                .ok_or_else(|| eyre!("Invalid projects.toml version"))?,
            None => 1,
        };
        if version != 1 {
            bail!("Unsupported projects.toml version: {version}");
        }
        if document.contains_key("projects") {
            bail!("Legacy top-level [[projects]] in projects.toml: move entries under [[sessions.{session}.projects]]");
        }
        let sessions = match document.get("sessions") {
            Some(value) => Some(
                value
                    .as_table()
                    .ok_or_else(|| eyre!("Invalid sessions in projects.toml"))?,
            ),
            None => None,
        };
        let projects: Vec<Project> = match sessions.and_then(|table| table.get(session)) {
            Some(value) => {
                let table = value
                    .as_table()
                    .ok_or_else(|| eyre!("Invalid sessions.{session} in projects.toml"))?;
                match table.get("projects") {
                    Some(value) => value.clone().try_into().wrap_err_with(|| {
                        format!("Invalid sessions.{session}.projects in projects.toml")
                    })?,
                    None => vec![],
                }
            }
            None => vec![],
        };
        let base = file.parent().ok_or_else(|| eyre!("Invalid config path"))?;
        let mut seen = std::collections::HashSet::new();
        for p in &projects {
            if p.path.is_empty() {
                bail!("Project path cannot be empty");
            }
            let path = normalize(&p.path, base)?;
            if !seen.insert(path.clone()) {
                bail!("Duplicate project path: {}", path.display());
            }
        }
        Ok(Self {
            projects,
            document,
            file,
            session: session.to_owned(),
        })
    }

    pub fn path(&self, project: &Project) -> Result<PathBuf> {
        normalize(&project.path, self.file.parent().unwrap())
    }

    pub fn add(&mut self, path: &Path) -> Result<bool> {
        let normalized = normalize(&path.to_string_lossy(), self.file.parent().unwrap())?;
        for project in &self.projects {
            if self.path(project)? == normalized {
                return Ok(false);
            }
        }
        let name = normalized
            .file_name()
            .ok_or_else(|| eyre!("Workspace cwd has no basename"))?
            .to_string_lossy()
            .into_owned();
        self.projects.push(Project {
            name: Some(name),
            path: normalized.to_string_lossy().into_owned(),
        });
        self.save()?;
        Ok(true)
    }

    fn save(&mut self) -> Result<()> {
        self.document
            .insert("version".into(), toml::Value::Integer(1));
        let sessions = self
            .document
            .entry("sessions")
            .or_insert_with(|| toml::Value::Table(toml::Table::new()))
            .as_table_mut()
            .ok_or_else(|| eyre!("Invalid sessions in projects.toml"))?;
        let session = sessions
            .entry(&self.session)
            .or_insert_with(|| toml::Value::Table(toml::Table::new()))
            .as_table_mut()
            .ok_or_else(|| eyre!("Invalid session in projects.toml"))?;
        session.insert("projects".into(), toml::Value::try_from(&self.projects)?);
        let parent = self.file.parent().unwrap();
        fs::create_dir_all(parent)?;
        // Unique temp file in same directory allows atomic rename and avoids concurrent temp collisions.
        let mut temp = None;
        for n in 0..100 {
            let candidate = parent.join(format!(".projects.toml.{}.{}.tmp", std::process::id(), n));
            match fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&candidate)
            {
                Ok(f) => {
                    temp = Some((candidate, f));
                    break;
                }
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(e) => return Err(e.into()),
            }
        }
        let (tmp, mut f) = temp.ok_or_else(|| eyre!("Could not create temporary config file"))?;
        let result = (|| -> Result<()> {
            use std::io::Write;
            f.write_all(toml::to_string_pretty(&self.document)?.as_bytes())?;
            f.sync_all()?;
            fs::rename(&tmp, &self.file)?;
            Ok(())
        })();
        if result.is_err() {
            let _ = fs::remove_file(tmp);
        }
        result.wrap_err("Unable to save projects.toml")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn dir() -> PathBuf {
        let d = env::temp_dir().join(format!(
            "herdr-picker-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        fs::create_dir_all(&d).unwrap();
        d
    }
    #[test]
    fn registry_and_registration() {
        let file = dir().join("projects.toml");
        let _ = fs::remove_file(&file);
        let mut personal = Registry::load(file.clone(), "personal").unwrap();
        assert!(personal.projects.is_empty());
        let path = dir().join("foo");
        fs::create_dir_all(&path).unwrap();
        assert!(personal.add(&path).unwrap());
        assert!(!personal.add(&path).unwrap());
        assert!(Registry::load(file.clone(), "work")
            .unwrap()
            .projects
            .is_empty());
        let mut work = Registry::load(file.clone(), "work").unwrap();
        assert!(work.add(&path).unwrap()); // registrations are independent per session
        let personal = Registry::load(file.clone(), "personal").unwrap();
        assert_eq!(personal.projects[0].name(), "foo");
        assert_eq!(personal.projects.len(), 1); // independent of workspace lifetime
        assert_eq!(
            Registry::load(file.clone(), "work").unwrap().projects.len(),
            1
        );
        fs::write(
            &file,
            "version = 1\n[[sessions.personal.projects]]\npath = '~/src/foo'\n",
        )
        .unwrap();
        assert_eq!(
            Registry::load(file.clone(), "personal").unwrap().projects[0].name(),
            "foo"
        );
        fs::write(&file, "[[sessions.work.projects]]\npath = 'oops'\n[[sessions.work.projects]]\npath = 'oops/'\n").unwrap();
        assert!(Registry::load(file.clone(), "work").is_err());
        fs::write(&file, "[[projects]]\npath = 'legacy'\n").unwrap();
        assert!(Registry::load(file.clone(), "personal").is_err());
        fs::write(&file, "[[sessions.work.projects]\n").unwrap();
        assert!(Registry::load(file, "work").is_err());
    }
    #[test]
    fn rejects_unsafe_session_names() {
        assert!(!valid_session_name("../work"));
        assert!(!valid_session_name(".."));
        assert!(valid_session_name("personal"));
        assert!(valid_session_name("work"));
    }
    #[test]
    fn paths_match() {
        let d = dir();
        let target = d.join("target");
        fs::create_dir_all(&target).unwrap();
        let link = d.join("link");
        #[cfg(unix)]
        {
            let _ = std::os::unix::fs::symlink(&target, &link);
        }
        assert_eq!(
            normalize(&format!("{}/", target.display()), &d).unwrap(),
            normalize(&link.to_string_lossy(), &d).unwrap()
        );
        assert_eq!(
            normalize("target/../target", &d).unwrap(),
            target.canonicalize().unwrap()
        );
        if let Some(home) = env::var_os("HOME") {
            assert_eq!(
                normalize("~/src/foo", &d).unwrap(),
                normalize("src/foo", &PathBuf::from(home)).unwrap()
            );
        }
    }
}
