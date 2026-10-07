//! Spotizgeg desktop command.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod entrypoint;

fn main() -> eframe::Result<()> {
    entrypoint::run()
}
