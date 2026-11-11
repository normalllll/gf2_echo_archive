use anyhow::{Context, Result, bail};
use std::{
    env, fs,
    path::{Component, Path, PathBuf},
};

#[cfg(windows)]
use winreg::{
    RegKey,
    enums::{HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, KEY_READ, KEY_WOW64_32KEY, KEY_WOW64_64KEY},
};

pub fn validate_directories(input: &Path, output: &Path) -> Result<()> {
    let input = input
        .canonicalize()
        .context(tr!("无法解析游戏目录", "Cannot resolve the game directory"))?;
    let output = canonicalize_allow_missing(output).context(tr!(
        "无法解析输出目录",
        "Cannot resolve the output directory"
    ))?;
    if output.starts_with(&input) {
        bail!(tr!(
            "输出目录不能位于游戏目录内部，以免重复扫描输出文件。",
            "The output directory cannot be inside the game directory, or exported files would be scanned again."
        ))
    }
    Ok(())
}

fn canonicalize_allow_missing(path: &Path) -> Result<PathBuf> {
    let absolute = std::path::absolute(path)?;
    let mut result = PathBuf::new();
    for component in absolute.components() {
        match component {
            Component::ParentDir => {
                result.pop();
            }
            Component::CurDir => {}
            _ => result.push(component.as_os_str()),
        }
        if result.has_root() && result.exists() {
            result = result
                .canonicalize()
                .context(tr!("无法解析目录", "Cannot resolve the directory"))?;
        }
    }
    Ok(result)
}

pub fn auto_detect_game_directory() -> Option<PathBuf> {
    let mut candidates = registry_game_directories();
    candidates.extend(steam_game_directories());
    candidates.extend(common_game_directories());
    candidates.into_iter().find(|path| path.is_dir())
}

#[cfg(windows)]
fn registry_game_directories() -> Vec<PathBuf> {
    let mut result = Vec::new();
    let uninstall_paths = [
        r"SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall",
        r"SOFTWARE\WOW6432Node\Microsoft\Windows\CurrentVersion\Uninstall",
    ];
    let roots = [
        RegKey::predef(HKEY_CURRENT_USER),
        RegKey::predef(HKEY_LOCAL_MACHINE),
    ];

    for root in &roots {
        for uninstall_path in uninstall_paths {
            for view in [
                KEY_READ,
                KEY_READ | KEY_WOW64_64KEY,
                KEY_READ | KEY_WOW64_32KEY,
            ] {
                let Ok(uninstall) = root.open_subkey_with_flags(uninstall_path, view) else {
                    continue;
                };
                for key_name in uninstall.enum_keys().flatten() {
                    let Ok(entry) = uninstall.open_subkey_with_flags(&key_name, KEY_READ) else {
                        continue;
                    };
                    let display_name: String = entry.get_value("DisplayName").unwrap_or_default();
                    if !looks_like_game_name(&display_name) {
                        continue;
                    }
                    let install_location: String =
                        entry.get_value("InstallLocation").unwrap_or_default();
                    if !install_location.trim().is_empty() {
                        result.push(PathBuf::from(install_location.trim_matches('"')));
                    }
                    let display_icon: String = entry.get_value("DisplayIcon").unwrap_or_default();
                    if let Some(parent) = executable_parent_from_registry_value(&display_icon) {
                        result.push(parent);
                    }
                }
            }
        }
    }
    result
}

#[cfg(not(windows))]
fn registry_game_directories() -> Vec<PathBuf> {
    Vec::new()
}

fn executable_parent_from_registry_value(value: &str) -> Option<PathBuf> {
    let value = value.trim().trim_matches('"');
    let without_icon_index = value
        .rsplit_once(',')
        .filter(|(_, suffix)| suffix.trim().parse::<i32>().is_ok())
        .map_or(value, |(path, _)| path);
    Path::new(without_icon_index)
        .parent()
        .map(Path::to_path_buf)
}

fn steam_game_directories() -> Vec<PathBuf> {
    let mut steam_roots = Vec::new();
    if let Some(program_files) = env::var_os("ProgramFiles(x86)") {
        steam_roots.push(PathBuf::from(program_files).join("Steam"));
    }
    if let Some(program_files) = env::var_os("ProgramFiles") {
        steam_roots.push(PathBuf::from(program_files).join("Steam"));
    }

    let mut library_roots = steam_roots.clone();
    for steam_root in &steam_roots {
        let library_file = steam_root.join("steamapps").join("libraryfolders.vdf");
        if let Ok(contents) = fs::read_to_string(library_file) {
            for line in contents.lines() {
                let parts: Vec<&str> = line.split('"').collect();
                if parts.len() >= 4 && parts[1].trim() == "path" {
                    library_roots.push(PathBuf::from(parts[3].replace(r"\\", r"\")));
                }
            }
        }
    }

    let mut result = Vec::new();
    for root in library_roots {
        let common = root.join("steamapps").join("common");
        let Ok(entries) = fs::read_dir(common) else {
            continue;
        };
        for entry in entries.flatten() {
            if entry.file_type().is_ok_and(|kind| kind.is_dir())
                && looks_like_game_name(&entry.file_name().to_string_lossy())
            {
                result.push(entry.path());
            }
        }
    }
    result
}

fn common_game_directories() -> Vec<PathBuf> {
    let names = [
        "GIRLS' FRONTLINE 2 EXILIUM",
        "GIRLS FRONTLINE 2 EXILIUM",
        "GF2Exilium",
        "GF2_Exilium",
        "少女前线2：追放",
        "少女前线2追放",
    ];
    let mut roots = Vec::new();
    for letter in b'C'..=b'Z' {
        let drive = PathBuf::from(format!("{}:\\", letter as char));
        if drive.is_dir() {
            roots.push(drive.clone());
            roots.push(drive.join("Games"));
            roots.push(drive.join("Game"));
            roots.push(drive.join("Program Files"));
            roots.push(drive.join("Program Files (x86)"));
        }
    }

    let mut result = Vec::new();
    for root in roots {
        for name in names {
            result.push(root.join(name));
        }
    }
    result
}

fn looks_like_game_name(name: &str) -> bool {
    let lower = name.to_lowercase();
    lower.contains("少女前线2")
        || lower.contains("少女前線2")
        || (lower.contains("girls") && lower.contains("frontline") && lower.contains('2'))
        || lower.contains("gf2exilium")
        || lower.contains("gf2_exilium")
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn resolves_new_relative_output_and_parent_components() {
        let current = env::current_dir().unwrap().canonicalize().unwrap();
        assert_eq!(
            canonicalize_allow_missing(Path::new("new-gf2-output/sub")).unwrap(),
            current.join("new-gf2-output/sub")
        );
        assert!(validate_directories(&current, &current.join("missing/../nested-output")).is_err());
    }
}
