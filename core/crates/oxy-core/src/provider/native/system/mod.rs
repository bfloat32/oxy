//! Providers that read the machine: processes, files, recents, clipboard and
//! ssh config.

pub mod clipboard;
pub mod file;
pub mod kill;
mod kill_windows;
pub mod recent;
pub mod ssh;
mod ssh_config;
pub mod sys;
