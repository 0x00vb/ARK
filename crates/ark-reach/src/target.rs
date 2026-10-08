//! The computer to reach, as the person names it, and the one quoting function every remote
//! word goes through.

use std::fmt;

/// `host` or `user@host`, checked before it reaches `ssh`.
///
/// Only `[A-Za-z0-9._@-]`, no leading `-` in either part, at most one `@`. So an option
/// (`-oProxyCommand=...`), a quote, a space or a shell character is refused, and every `ssh`
/// ARK runs puts the target after `--` as well.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Target {
    user: Option<String>,
    host: String,
}

impl Target {
    /// The target in `text`, or why it is refused (words the window can show).
    pub fn parse(text: &str) -> Result<Target, String> {
        let text = text.trim_matches(' ');
        if text.is_empty() {
            return Err("Type a computer: host or user@host".into());
        }
        if let Some(c) = text
            .chars()
            .find(|c| !(c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '@' | '-')))
        {
            return Err(format!("`{c}` cannot be in a computer's name"));
        }
        let (user, host) = match text.split_once('@') {
            Some((u, h)) => (Some(u), h),
            None => (None, text),
        };
        if host.contains('@') {
            return Err("A computer's name has one @ at most".into());
        }
        for part in user.into_iter().chain([host]) {
            if part.is_empty() {
                return Err("Type a computer: host or user@host".into());
            }
            if part.starts_with('-') {
                return Err("A computer's name cannot start with -".into());
            }
        }
        Ok(Target {
            user: user.map(str::to_string),
            host: host.to_string(),
        })
    }

    /// The host part: the name used in the window's words ("Could not reach vps").
    pub fn host(&self) -> &str {
        &self.host
    }

    pub fn user(&self) -> Option<&str> {
        self.user.as_deref()
    }
}

impl fmt::Display for Target {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.user {
            Some(u) => write!(f, "{u}@{}", self.host),
            None => f.write_str(&self.host),
        }
    }
}

/// One word for a POSIX shell: in single quotes, with each `'` written as `'\''`.
pub fn quote(word: &str) -> String {
    let mut out = String::with_capacity(word.len() + 2);
    out.push('\'');
    for c in word.chars() {
        if c == '\'' {
            out.push_str("'\\''");
        } else {
            out.push(c);
        }
    }
    out.push('\'');
    out
}

/// A remote command line from words, each quoted. `ssh` joins its arguments with spaces and
/// the remote shell splits them again, so this is the only form ARK sends.
pub fn command_line<S: AsRef<str>>(words: &[S]) -> String {
    words
        .iter()
        .map(|w| quote(w.as_ref()))
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_names_pass() {
        let t = Target::parse("vps").unwrap();
        assert_eq!((t.user(), t.host()), (None, "vps"));
        let t = Target::parse(" deploy@build-01.example.org ").unwrap();
        assert_eq!(
            (t.user(), t.host()),
            (Some("deploy"), "build-01.example.org")
        );
        assert_eq!(t.to_string(), "deploy@build-01.example.org");
        assert!(Target::parse("10.0.0.7").is_ok());
        assert!(Target::parse("a_b").is_ok());
    }

    #[test]
    fn options_quotes_and_shell_words_are_refused() {
        for bad in [
            "-oProxyCommand=x",
            "-oProxyCommand=touch /tmp/x",
            "u@-oProxyCommand=x",
            "-u@host",
            "a'b",
            "a\"b",
            "a b",
            "a;b",
            "a|b",
            "$(id)",
            "a`b`",
            "a@b@c",
            "@host",
            "user@",
            "",
            "host\n",
            "a/b",
            "a:22",
        ] {
            assert!(Target::parse(bad).is_err(), "{bad:?} passed");
        }
    }

    #[test]
    fn quote_wraps_and_escapes() {
        assert_eq!(quote("a"), "'a'");
        assert_eq!(quote(""), "''");
        assert_eq!(quote("a'b"), "'a'\\''b'");
        assert_eq!(command_line(&["ls", "-l", "a b"]), "'ls' '-l' 'a b'");
    }

    /// Words come back unchanged through a real `sh`.
    #[cfg(unix)]
    #[test]
    fn quote_round_trips_through_sh() {
        let words = [
            "plain",
            "",
            "two words",
            "it's",
            "''",
            "\\'",
            "$HOME",
            "`id`",
            "$(id)",
            "a;b|c&d",
            "*?[x]",
            "line\nbreak",
            "tab\there",
            "-n",
            "ünï",
            "\"dq\"",
            "!bang",
        ];
        let script = format!("printf '%s\\0' {}", command_line(&words));
        let out = std::process::Command::new("/bin/sh")
            .arg("-c")
            .arg(&script)
            .output()
            .unwrap();
        assert!(out.status.success());
        let got: Vec<String> = String::from_utf8(out.stdout)
            .unwrap()
            .split_terminator('\0')
            .map(str::to_string)
            .collect();
        assert_eq!(got, words);
    }
}
