//! # ark-reach
//!
//! Setting up a computer over SSH so a chat can run there (ADR 0052, Reach). This is process
//! orchestration: the system `ssh` makes every connection with the person's config, keys and
//! agent. ARK reads the resolved config to list computers and never reads a key.
//!
//! | Module | For |
//! |---|---|
//! | [`config`] | the computers in `~/.ssh/config` ([`hosts`]) |
//! | [`target`] | a checked `host` or `user@host` ([`Target`]); the one quoting function |
//! | [`ssh`] | the `ssh` program and one connection per setup ([`Session`]) |
//! | [`setup`] | the steps: connect, look, install, start, pair; [`setup()`](setup::setup) and [`remove`] |
//! | [`tunnel`] | a local Unix socket forwarded to the computer's, kept up ([`Tunnel`]) |
//!
//! What ARK installs there is the app's: it gives a [`ServeContract`] (the program's name, how
//! it serves on a socket, makes and revokes a token, says its version).
//!
//! Rules every command here follows:
//! - `ssh [options] -- <target> <command>`: the target after `--`, checked by [`Target`];
//!   every remote word quoted by [`quote`].
//! - `BatchMode=yes`: `ssh` never asks. What needs the person (a new host key, a passphrase,
//!   `sudo`) comes back as a command for the app to run in a terminal.
//! - Host keys follow the person's `known_hosts`. Never `StrictHostKeyChecking=no` or
//!   `accept-new`.
//! - A token is a [`Secret`]: it is never printed, logged or put in an error.

use std::fmt;

use serde::Serialize;

pub mod config;
pub mod setup;
pub mod ssh;
pub mod target;
pub mod tunnel;

pub use config::{hosts, KnownHost};
pub use setup::{remove, setup, Build, Facts, Plan, ServeContract, Setup, Started};
pub use ssh::{private_dir, Session, Ssh};
pub use target::{quote, Target};
pub use tunnel::{Tunnel, TunnelState};

/// A step of setting up a computer, in order. Reported as each one begins.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Step {
    Connect,
    Look,
    Install,
    Start,
    Pair,
    Tunnel,
    /// Not part of a setup: taking the program off a computer.
    Remove,
}

/// What kind of failure, so the window can offer the right next action.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Kind {
    /// `ssh` needs the person: a host key it has not seen.
    /// The app runs [`Failure::terminal`] in a terminal once, then tries again.
    NeedsTerminal,
    /// The computer's host key is not the one in `known_hosts`. Setup stops.
    HostKeyChanged,
    /// The computer refused every key (it takes only a password, or the person's key needs a
    /// passphrase or a touch). [`Failure::terminal`] adds ARK's own key there
    /// ([`ssh::ark_key`]) after the password once, and checks that it works.
    AuthRefused,
    /// No answer: name not found, refused, timed out, connection closed.
    Unreachable,
    /// The computer runs a system ARK has no build for.
    Unsupported,
    /// The computer's `sshd` turns socket forwarding off.
    ForwardingRefused,
    /// The build's SHA-256 is not the expected one. Nothing was sent.
    BuildMismatch,
    /// The service did not answer on its socket. [`Failure::log`] has its last lines.
    NotStarted,
    /// Anything else, in [`Failure::words`].
    Failed,
}

/// A step that failed, in words the window can show as they are.
#[derive(Clone, Debug)]
pub struct Failure {
    pub step: Step,
    pub kind: Kind,
    pub words: String,
    /// A command for the app to run in a terminal (argv), when the person can fix it there.
    pub terminal: Option<Vec<String>>,
    /// The service's last log lines, for [`Kind::NotStarted`].
    pub log: Vec<String>,
}

impl Failure {
    pub(crate) fn new(step: Step, kind: Kind, words: impl Into<String>) -> Failure {
        Failure {
            step,
            kind,
            words: words.into(),
            terminal: None,
            log: Vec::new(),
        }
    }
}

impl fmt::Display for Failure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.words)
    }
}

impl std::error::Error for Failure {}

/// A token. Its `Debug` and `Display` never show it; [`Secret::expose`] is the only way in.
#[derive(Clone, PartialEq, Eq)]
pub struct Secret(String);

impl Secret {
    pub fn new(value: String) -> Secret {
        Secret(value)
    }

    /// The token itself, for the keychain and the link's header. Nowhere else.
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret(hidden)")
    }
}

impl fmt::Display for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("(hidden)")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_secret_is_never_shown() {
        let s = Secret::new("ark_s3cr3t_value".into());
        let shown = format!("{s} {s:?} {s:#?} {:?}", Some(&s));
        assert!(!shown.contains("s3cr3t"), "{shown}");
        assert_eq!(s.expose(), "ark_s3cr3t_value");
    }
}
