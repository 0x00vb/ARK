//! The person's interactive shell, for a terminal pane.

use std::path::PathBuf;

/// The shell a new terminal runs: `$SHELL` on Unix (else zsh, bash, sh); on Windows
/// PowerShell 7, then Windows PowerShell, then `%ComSpec%` (cmd).
pub fn interactive() -> PathBuf {
    if cfg!(windows) {
        ["pwsh", "powershell"]
            .iter()
            .find_map(|n| crate::find::program(n))
            .or_else(|| std::env::var_os("ComSpec").map(PathBuf::from))
            .unwrap_or_else(|| PathBuf::from("cmd.exe"))
    } else {
        std::env::var_os("SHELL")
            .map(PathBuf::from)
            .filter(|p| p.is_file())
            .or_else(|| {
                ["/bin/zsh", "/bin/bash", "/bin/sh"]
                    .iter()
                    .map(PathBuf::from)
                    .find(|p| p.is_file())
            })
            .unwrap_or_else(|| PathBuf::from("/bin/sh"))
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn there_is_a_shell() {
        assert!(super::interactive().is_file() || cfg!(windows));
    }
}
