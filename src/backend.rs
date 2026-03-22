use anyhow::Context;
use anyhow::Result;
use std::ffi::OsString;
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
