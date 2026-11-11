//! Keeps a record of unexpected failures. The release build has no console and
//! aborts on panic, so without this the program would simply disappear.

use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::PathBuf,
    sync::atomic::{AtomicBool, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

const LOG_FILE: &str = "crash.log";

/// Installs the panic hook. `interactive` also shows a message box.
pub fn install_reporter(interactive: bool) {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        static REPORTED: AtomicBool = AtomicBool::new(false);
        if !REPORTED.swap(true, Ordering::SeqCst) {
            let thread = std::thread::current();
            let report = report_text(
                unix_seconds(),
                thread.name().unwrap_or("unnamed"),
                &info.to_string(),
            );
            let path = write_report(&report);
            if interactive {
                show_message(path);
            }
        }
        previous(info);
    }));
}

fn unix_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or(0)
}

fn report_text(seconds: u64, thread: &str, message: &str) -> String {
    format!(
        "version {} | unix time {seconds} | thread {thread}\n{message}\n\n",
        env!("CARGO_PKG_VERSION")
    )
}

fn write_report(report: &str) -> Option<PathBuf> {
    let directory = crate::paths::data_dir()?;
    fs::create_dir_all(&directory).ok()?;
    let path = directory.join(LOG_FILE);
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .ok()?;
    file.write_all(report.as_bytes()).ok()?;
    Some(path)
}

fn show_message(path: Option<PathBuf>) {
    let detail = match path {
        Some(path) => tr!(
            "错误信息已保存到：\n{}",
            "The error details were saved to:\n{}",
            path.display()
        ),
        None => tr!(
            "错误信息未能保存。",
            "The error details could not be saved."
        )
        .to_owned(),
    };
    rfd::MessageDialog::new()
        .set_level(rfd::MessageLevel::Error)
        .set_title("GF2 Echo Archive")
        .set_description(tr!(
            "程序遇到意外错误，需要关闭。\n\n{detail}",
            "The program hit an unexpected error and must close.\n\n{detail}",
            detail = detail
        ))
        .set_buttons(rfd::MessageButtons::Ok)
        .show();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_report_names_the_version_thread_and_message() {
        let report = report_text(42, "export", "panicked at src/a.rs:1:1");
        assert!(report.contains(env!("CARGO_PKG_VERSION")));
        assert!(report.contains("unix time 42"));
        assert!(report.contains("thread export"));
        assert!(report.contains("panicked at src/a.rs:1:1"));
    }
}
