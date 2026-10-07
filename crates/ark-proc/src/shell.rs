//! The person's shell and its environment. An app started from the Dock or a service manager
//! does not read the person's shell setup, so the PATH it has misses version managers (nvm,
//! rustup) and Homebrew. The login shell is asked once for its environment, as editors do.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::Duration;

use tokio::sync::RwLock;

/// What the environment is read through; the output before it is the shell's own noise.
const MARK: &str = "__ARK_ENV__";
/// The longest a shell setup may take to answer.
const READ_TIME: Duration = Duration::from_secs(10);

/// A shell and the environment commands run with.
pub struct Shell {
    program: PathBuf,
    /// Read from the login shell; `None` until first asked, or after [`Shell::refresh`].
    env: RwLock<Option<Vec<(OsString, OsString)>>>,
    /// Never asked: this environment is used as given (tests, an app's own).
    fixed: bool,
}

impl Shell {
    /// The person's shell (`$SHELL`, else `/bin/sh`), its environment read on first use.
    pub fn person() -> Shell {
        let program = std::env::var_os("SHELL")
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
            .unwrap_or_else(|| PathBuf::from("/bin/sh"));
        Shell {
            program,
            env: RwLock::new(None),
            fixed: false,
        }
    }

    /// `program` with `env` as it is, never read from a login shell.
    pub fn fixed(program: impl Into<PathBuf>, env: Vec<(OsString, OsString)>) -> Shell {
        Shell {
            program: program.into(),
            env: RwLock::new(Some(env)),
            fixed: true,
        }
    }

    pub fn program(&self) -> &Path {
        &self.program
    }

    /// The environment commands run with.
    pub async fn env(&self) -> Vec<(OsString, OsString)> {
        if let Some(env) = self.env.read().await.as_ref() {
            return env.clone();
        }
        let mut slot = self.env.write().await;
        if let Some(env) = slot.as_ref() {
            return env.clone();
        }
        let env = match read_login_env(&self.program).await {
            Some(env) => env,
            None => {
                tracing::warn!(
                    "could not read the environment of {}; processes get ARK's own",
                    self.program.display()
                );
                std::env::vars_os().collect()
            }
        };
        *slot = Some(env.clone());
        env
    }

    /// Read the environment again on next use (after an install changed the shell setup).
    pub async fn refresh(&self) {
        if !self.fixed {
            *self.env.write().await = None;
        }
    }

    /// Where `program` is in `env`'s PATH, if anywhere. A path (`./run.sh`) is taken as is.
    pub fn find(program: &str, env: &[(OsString, OsString)], cwd: &Path) -> Option<PathBuf> {
        if program.contains('/') {
            let p = cwd.join(program);
            return p.exists().then_some(p);
        }
        let path = env.iter().find(|(k, _)| k == "PATH").map(|(_, v)| v)?;
        std::env::split_paths(path)
            .map(|d| d.join(program))
            .find(|p| is_executable(p))
    }
}

#[cfg(unix)]
fn is_executable(p: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(p).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

#[cfg(not(unix))]
fn is_executable(p: &Path) -> bool {
    p.is_file()
}

/// `$SHELL -i -l -c 'env -0'`, with no terminal and a time limit. `None` when it fails.
async fn read_login_env(shell: &Path) -> Option<Vec<(OsString, OsString)>> {
    let script = format!("printf '\\n{MARK}\\n'; /usr/bin/env -0");
    let out = tokio::process::Command::new(shell)
        .args(["-i", "-l", "-c", &script])
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true)
        .output();
    let out = tokio::time::timeout(READ_TIME, out).await.ok()?.ok()?;
    parse_env(&out.stdout)
}

/// The variables after the mark, `NAME=value` separated by NUL bytes.
fn parse_env(out: &[u8]) -> Option<Vec<(OsString, OsString)>> {
    let mark = format!("\n{MARK}\n");
    let at = out.windows(mark.len()).position(|w| w == mark.as_bytes())?;
    let body = &out[at + mark.len()..];
    let env: Vec<(OsString, OsString)> = body
        .split(|b| *b == 0)
        .filter_map(|kv| {
            let eq = kv.iter().position(|b| *b == b'=')?;
            let (k, v) = (&kv[..eq], &kv[eq + 1..]);
            // A shell's own bookkeeping, not the person's environment.
            if k.is_empty() || k == b"_" || k == b"SHLVL" || k == b"OLDPWD" || k == b"PWD" {
                return None;
            }
            Some((bytes_os(k), bytes_os(v)))
        })
        .collect();
    env.iter().any(|(k, _)| k == "PATH").then_some(env)
}

#[cfg(unix)]
fn bytes_os(b: &[u8]) -> OsString {
    use std::os::unix::ffi::OsStrExt;
    std::ffi::OsStr::from_bytes(b).to_os_string()
}

#[cfg(not(unix))]
fn bytes_os(b: &[u8]) -> OsString {
    OsString::from(String::from_utf8_lossy(b).into_owned())
}

/// The program a command line starts, when it can be told without running it: the first word
/// after `NAME=value` settings. `None` for a shell word (`cd`, `export`, `(`), which the shell
/// itself runs.
pub fn program_of(command: &str) -> Option<String> {
    const SHELL_WORDS: &[&str] = &[
        "cd", "export", "source", ".", "exec", "env", "set", "unset", "eval", "if", "for", "while",
        "case", "{", "(", "!", "time", "nohup", "sudo",
    ];
    let word = command
        .split_whitespace()
        .find(|w| !(w.contains('=') && !w.starts_with('=') && !w.starts_with('-')))?;
    let word = word.trim_matches(|c| c == '"' || c == '\'');
    if word.is_empty() || SHELL_WORDS.contains(&word) || word.contains(['$', '`', ';', '&', '|']) {
        return None;
    }
    Some(word.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_environment_starts_after_the_mark() {
        let out = b"noise from a prompt theme\n__ARK_ENV__\nPATH=/a:/b\0HOME=/h\0_=/usr/bin/env\0";
        let env = parse_env(out).unwrap();
        assert_eq!(
            env,
            vec![
                ("PATH".into(), "/a:/b".into()),
                ("HOME".into(), "/h".into())
            ]
        );
        assert!(parse_env(b"no mark here").is_none());
    }

    #[test]
    fn the_program_is_the_first_word_after_settings() {
        assert_eq!(program_of("npm run dev").as_deref(), Some("npm"));
        assert_eq!(
            program_of("PORT=3001 NODE_ENV=dev pnpm dev").as_deref(),
            Some("pnpm")
        );
        assert_eq!(
            program_of("./bin/server --port 3").as_deref(),
            Some("./bin/server")
        );
        assert_eq!(program_of("cd web && npm run dev"), None);
        assert_eq!(program_of("$HOME/bin/x"), None);
        assert_eq!(program_of("   "), None);
    }
}
