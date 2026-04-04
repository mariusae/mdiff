use anyhow::Context;
use anyhow::Result;
use std::collections::HashMap;
use std::ffi::OsString;
use std::fs;
use std::path::Path;
use std::path::PathBuf;
use std::process::Command;
use std::process::Output;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Backend {
    Git,
    Hg,
    PlainDiff,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Detection {
    pub backend: Backend,
    pub root: Option<PathBuf>,
}

#[derive(Clone, Debug)]
pub struct FileFetcher {
    backend: Backend,
    cwd: PathBuf,
    root: Option<PathBuf>,
    args: Vec<OsString>,
    git_right_blobs: HashMap<String, String>,
}

pub fn detect(cwd: &Path) -> Backend {
    detect_details(cwd).backend
}

pub fn detect_details(cwd: &Path) -> Detection {
    for dir in cwd.ancestors() {
        if dir.join(".git").exists() {
            return Detection {
                backend: Backend::Git,
                root: Some(dir.to_path_buf()),
            };
        }
        if dir.join(".hg").exists() {
            return Detection {
                backend: Backend::Hg,
                root: Some(dir.to_path_buf()),
            };
        }
    }

    Detection {
        backend: Backend::PlainDiff,
        root: None,
    }
}

impl Backend {
    pub fn describe(self) -> &'static str {
        match self {
            Self::Git => "git diff",
            Self::Hg => "hg diff",
            Self::PlainDiff => "diff",
        }
    }

    pub fn run(self, args: &[OsString]) -> Result<Output> {
        let mut command = self.command(args);
        command
            .output()
            .with_context(|| format!("unable to execute {}", self.describe()))
    }

    pub fn command_preview(self, args: &[OsString]) -> String {
        let mut parts: Vec<String> = match self {
            Self::Git => vec![
                "git".into(),
                "-c".into(),
                "color.ui=false".into(),
                "-c".into(),
                "core.pager=cat".into(),
                "diff".into(),
            ],
            Self::Hg => vec![
                "hg".into(),
                "--config".into(),
                "ui.color=off".into(),
                "diff".into(),
            ],
            Self::PlainDiff => vec!["diff".into()],
        };

        parts.extend(
            args.iter()
                .map(|arg| shell_quote_lossy(&arg.to_string_lossy())),
        );
        parts.join(" ")
    }

    fn command(self, args: &[OsString]) -> Command {
        match self {
            Self::Git => {
                let mut command = Command::new("git");
                command.arg("-c").arg("color.ui=false");
                command.arg("-c").arg("core.pager=cat");
                command.arg("diff");
                command.args(args);
                command.env("NO_COLOR", "1");
                command.env("CLICOLOR", "0");
                command.env("GIT_PAGER", "cat");
                command
            }
            Self::Hg => {
                let mut command = Command::new("hg");
                command.arg("--config").arg("ui.color=off");
                command.arg("diff");
                command.args(args);
                command.env("NO_COLOR", "1");
                command.env("CLICOLOR", "0");
                command
            }
            Self::PlainDiff => {
                let mut command = Command::new("diff");
                command.args(args);
                command
            }
        }
    }
}

impl FileFetcher {
    pub fn new(
        backend: Backend,
        cwd: PathBuf,
        root: Option<PathBuf>,
        args: Vec<OsString>,
        git_right_blobs: HashMap<String, String>,
    ) -> Self {
        Self {
            backend,
            cwd,
            root,
            args,
            git_right_blobs,
        }
    }

    pub fn fetch_right_file(&self, path: &str) -> Result<String> {
        match self.backend {
            Backend::Git => self.fetch_git_right_file(path),
            Backend::Hg => self.fetch_hg_right_file(path),
            Backend::PlainDiff => fs::read_to_string(self.cwd.join(path))
                .with_context(|| format!("failed to read {path} from disk")),
        }
    }

    fn fetch_git_right_file(&self, path: &str) -> Result<String> {
        if let Some(blob) = self.git_right_blobs.get(path) {
            return self.run_in_root("git", ["show"], [blob.as_str()]);
        }

        if has_flag(&self.args, "--cached") || has_flag(&self.args, "--staged") {
            return self.run_in_root("git", ["show"], [format!(":{path}")]);
        }

        self.read_working_tree_file(path)
    }

    fn fetch_hg_right_file(&self, path: &str) -> Result<String> {
        if let Some(revision) = last_flag_value(&self.args, &["-r", "--rev"]) {
            return self.run_in_root("hg", ["cat", "-r"], [revision, path.to_owned()]);
        }

        self.read_working_tree_file(path)
    }

    fn read_working_tree_file(&self, path: &str) -> Result<String> {
        let base = self.root.as_ref().unwrap_or(&self.cwd);
        fs::read_to_string(base.join(path))
            .with_context(|| format!("failed to read {path} from disk"))
    }

    fn run_in_root<const N: usize, I, S>(
        &self,
        program: &str,
        prefix: [&str; N],
        args: I,
    ) -> Result<String>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<std::ffi::OsStr>,
    {
        let mut command = Command::new(program);
        command.args(prefix);
        command.args(args);
        command.current_dir(self.root.as_ref().unwrap_or(&self.cwd));
        let output = command
            .output()
            .with_context(|| format!("unable to execute {program}"))?;
        if !output.status.success() {
            anyhow::bail!(
                "{program} exited with status {}",
                output.status.code().unwrap_or(1)
            );
        }
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    }
}

fn has_flag(args: &[OsString], flag: &str) -> bool {
    args.iter().any(|arg| arg == flag)
}

fn last_flag_value(args: &[OsString], flags: &[&str]) -> Option<String> {
    let mut result = None;
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        if flags.iter().any(|flag| arg == flag) {
            let value = iter.next()?;
            result = Some(value.to_string_lossy().into_owned());
            continue;
        }

        for flag in flags {
            let prefix = format!("{flag}=");
            let arg_text = arg.to_string_lossy();
            if let Some(value) = arg_text.strip_prefix(&prefix) {
                result = Some(value.to_owned());
                break;
            }
        }
    }
    result
}

fn shell_quote_lossy(value: &str) -> String {
    if value.is_empty() {
        return "''".into();
    }

    if value
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '/' | '.' | '_' | '-' | ':' | '='))
    {
        return value.to_owned();
    }

    format!("'{}'", value.replace('\'', r"'\''"))
}

#[cfg(test)]
mod tests {
    use super::Backend;
    use super::detect;
    use super::detect_details;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn detects_git_repository() {
        let temp = TempDir::new().unwrap();
        fs::create_dir(temp.path().join(".git")).unwrap();

        assert_eq!(detect(temp.path()), Backend::Git);
    }

    #[test]
    fn detects_hg_repository() {
        let temp = TempDir::new().unwrap();
        fs::create_dir(temp.path().join(".hg")).unwrap();

        assert_eq!(detect(temp.path()), Backend::Hg);
    }

    #[test]
    fn prefers_nearest_repository_marker() {
        let temp = TempDir::new().unwrap();
        let outer = temp.path();
        let inner = outer.join("nested");
        fs::create_dir(&inner).unwrap();
        fs::create_dir(outer.join(".git")).unwrap();
        fs::create_dir(inner.join(".hg")).unwrap();

        assert_eq!(detect(&inner), Backend::Hg);
    }

    #[test]
    fn detect_details_reports_root() {
        let temp = TempDir::new().unwrap();
        let inner = temp.path().join("nested");
        fs::create_dir(&inner).unwrap();
        fs::create_dir(temp.path().join(".git")).unwrap();

        let detection = detect_details(&inner);
        assert_eq!(detection.backend, Backend::Git);
        assert_eq!(detection.root.as_deref(), Some(temp.path()));
    }
}
