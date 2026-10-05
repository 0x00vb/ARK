//! # ark-os
//!
//! What differs between operating systems, behind one API, so the rest of ARK and the apps on
//! it never test for an OS (ADR 0022). Each function does the same thing everywhere and
//! documents what that takes on each system.
//!
//! | Module | For |
//! |---|---|
//! | [`dirs`] | the home, app data and Documents folders; paths as people read them |
//! | [`find`] | a program on `PATH` or where its installer puts it; an installed app |
//! | [`process`] | starting a program the same way everywhere; stopping it with everything it started; whether a process is alive |
//! | [`open`] | a link or a file in the person's default app, never through a shell |
//! | [`fs`] | replacing a file atomically; names every OS accepts; files only their owner reads |
//! | [`time`] | the local UTC offset |
//! | [`shell`] | the person's interactive shell, for a terminal |
//!
//! Supported: macOS, Windows, and other Unix systems (Linux) through the Unix code.

pub mod dirs;
pub mod find;
pub mod fs;
pub mod open;
pub mod process;
pub mod shell;
pub mod time;
