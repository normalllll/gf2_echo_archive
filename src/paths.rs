//! Where the program keeps its files. Everything persistent lives in a `data`
//! folder next to the executable, for the interface and the command line
//! alike; the location does not depend on the working directory and nothing
//! falls back to another place.

use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

/// Folder holding the running executable.
pub fn exe_dir() -> Option<PathBuf> {
    std::env::current_exe()
        .ok()?
        .parent()
        .map(Path::to_path_buf)
}

/// `<exe folder>\data`: the single root of the settings, indexes and caches.
pub fn data_dir() -> Option<PathBuf> {
    exe_dir().map(|dir| dir.join("data"))
}

/// Creates the data folder and proves it can be written to.
pub fn ensure_data_dir() -> Result<PathBuf> {
    let directory = data_dir().context(tr!(
        "无法确定程序所在目录",
        "Cannot determine the program's folder"
    ))?;
    ensure_writable(&directory)?;
    Ok(directory)
}

fn ensure_writable(directory: &Path) -> Result<()> {
    std::fs::create_dir_all(directory).with_context(|| {
        tr!(
            "无法创建数据目录 {}",
            "Cannot create the data folder {}",
            directory.display()
        )
    })?;
    let probe = directory.join(format!(".write-test-{}", std::process::id()));
    std::fs::write(&probe, b"").with_context(|| {
        tr!(
            "数据目录不可写：{}",
            "The data folder is not writable: {}",
            directory.display()
        )
    })?;
    let _ = std::fs::remove_file(probe);
    Ok(())
}

/// Game directory for the tests that need a local installation, from the
/// `GF2_GAME_DIR` environment variable; `None` skips them.
#[cfg(test)]
pub fn test_game_dir() -> Option<PathBuf> {
    std::env::var_os("GF2_GAME_DIR")
        .map(PathBuf::from)
        .filter(|path| path.is_dir())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn data_lives_beside_the_executable() {
        let executable = std::env::current_exe().unwrap();
        assert_eq!(
            data_dir().unwrap(),
            executable.parent().unwrap().join("data")
        );
    }

    #[test]
    fn writable_check_creates_the_folder_and_leaves_nothing() {
        let directory =
            std::env::temp_dir().join(format!("echo-archive-paths-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&directory);
        let nested = directory.join("a").join("data");
        ensure_writable(&nested).unwrap();
        assert_eq!(std::fs::read_dir(&nested).unwrap().count(), 0);
        // A file where the folder should be is reported, not worked around.
        let blocked = directory.join("file");
        std::fs::write(&blocked, b"x").unwrap();
        assert!(ensure_writable(&blocked.join("data")).is_err());
        std::fs::remove_dir_all(directory).unwrap();
    }
}
