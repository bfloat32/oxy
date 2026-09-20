//! Providers that read the machine: processes, files, recents, clipboard and
//! ssh config — plus the local-state sliders and radios (volume, brightness,
//! windows, bluetooth, wifi).

pub mod bri;
pub mod bt;
pub mod clipboard;
pub mod file;
pub mod herdr;
pub mod img;
pub mod kill;
mod kill_windows;
pub mod pass;
pub mod recent;
pub mod ssh;
mod ssh_config;
pub mod sys;
pub mod vol;
pub mod wifi;
pub mod win;
