#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

#[macro_use]
mod i18n;

mod audio;
mod catalog;
mod characters;
mod cli;
mod crash;
mod cri;
mod discovery;
mod extraction;
mod live2d;
mod paths;
mod portrait;
mod roster;
mod settings;
mod story;
mod tables;
mod ui;
mod unity;

fn main() {
    let arguments: Vec<_> = std::env::args_os().skip(1).collect();
    i18n::set_language(i18n::resolve_for_system(
        settings::Settings::load().language,
    ));
    crash::install_reporter(arguments.is_empty());
    if !arguments.is_empty() {
        if let Err(error) = cli::run(arguments) {
            eprintln!("{error:#}");
            std::process::exit(1);
        }
        return;
    }
    ui::run();
}
