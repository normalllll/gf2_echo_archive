//! `model3.json` manifests: the files a runtime model references.

use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use serde_json::Value as Json;
use std::path::{Component, Path, PathBuf};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    Manifest,
    Moc,
    Texture,
    DisplayInfo,
    Physics,
    Pose,
    UserData,
    Expression,
    Motion,
    /// Motion sound or an unknown `FileReferences` key.
    Other,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Reference {
    pub role: Role,
    /// Path exactly as written in the manifest.
    pub path: String,
}

/// Lists the manifest's file references in a stable order without
/// duplicates (the first role wins when a path is listed twice).
pub fn references(manifest: &Json) -> Result<Vec<Reference>> {
    let files = manifest
        .get("FileReferences")
        .and_then(Json::as_object)
        .context(tr!(
            "model3.json 缺少 FileReferences",
            "model3.json has no FileReferences"
        ))?;
    let mut result: Vec<Reference> = Vec::new();
    let mut push = |role, value: &Json| -> Result<()> {
        let path = value.as_str().with_context(|| {
            tr!(
                "{} 路径不是字符串",
                "The {} path is not a string",
                role_key(role)
            )
        })?;
        if !path.is_empty() && !result.iter().any(|r| r.path == path) {
            result.push(Reference {
                role,
                path: path.to_owned(),
            });
        }
        Ok(())
    };
    // Fixed role order; serde_json maps are sorted by key.
    const ORDER: [&str; 8] = [
        "Moc",
        "Textures",
        "DisplayInfo",
        "Physics",
        "Pose",
        "UserData",
        "Expressions",
        "Motions",
    ];
    let keys = ORDER.iter().copied().chain(
        files
            .keys()
            .map(String::as_str)
            .filter(|k| !ORDER.contains(k)),
    );
    for key in keys {
        let Some(value) = files.get(key) else {
            continue;
        };
        match key {
            "Moc" => push(Role::Moc, value)?,
            "Textures" => {
                for texture in value
                    .as_array()
                    .context(tr!("Textures 不是数组", "Textures is not an array"))?
                {
                    push(Role::Texture, texture)?;
                }
            }
            "DisplayInfo" => push(Role::DisplayInfo, value)?,
            "Physics" => push(Role::Physics, value)?,
            "Pose" => push(Role::Pose, value)?,
            "UserData" => push(Role::UserData, value)?,
            "Expressions" => {
                for item in value
                    .as_array()
                    .context(tr!("Expressions 不是数组", "Expressions is not an array"))?
                {
                    push(Role::Expression, item.get("File").unwrap_or(&Json::Null))?;
                }
            }
            "Motions" => {
                for group in value
                    .as_object()
                    .context(tr!("Motions 不是对象", "Motions is not an object"))?
                    .values()
                {
                    for item in group
                        .as_array()
                        .context(tr!("动作分组不是数组", "A motion group is not an array"))?
                    {
                        push(Role::Motion, item.get("File").unwrap_or(&Json::Null))?;
                        if let Some(sound) = item.get("Sound") {
                            push(Role::Other, sound)?;
                        }
                    }
                }
            }
            _ if value.is_string() => push(Role::Other, value)?,
            _ => {}
        }
    }
    ensure!(
        result.iter().any(|r| r.role == Role::Moc),
        tr!(
            "model3.json 没有 Moc 引用",
            "model3.json has no Moc reference"
        )
    );
    Ok(result)
}

fn role_key(role: Role) -> &'static str {
    match role {
        Role::Moc => "Moc",
        Role::Texture => "Textures",
        Role::DisplayInfo => "DisplayInfo",
        Role::Physics => "Physics",
        Role::Pose => "Pose",
        Role::UserData => "UserData",
        Role::Expression => "Expressions",
        Role::Motion => "Motions",
        Role::Manifest | Role::Other => "FileReferences",
    }
}

/// Model name: the manifest's moc file stem (keeps the original case,
/// `CHA_Lenna_SSR_01.moc3` → `CHA_Lenna_SSR_01`).
pub fn model_name(references: &[Reference], asset_name: &str) -> String {
    references
        .iter()
        .find(|r| r.role == Role::Moc)
        .and_then(|r| r.path.rsplit(['/', '\\']).next())
        .map(|file| {
            let lower = file.to_ascii_lowercase();
            match lower.strip_suffix(".moc3") {
                Some(stem) => file[..stem.len()].to_owned(),
                None => file.to_owned(),
            }
        })
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| {
            let lower = asset_name.to_ascii_lowercase();
            asset_name[..lower.strip_suffix(".model3").unwrap_or(&lower).len()].to_owned()
        })
}

/// TextAsset lookup key for a referenced JSON file: the file name without
/// `.json`, lower-cased (`expresstions/EMO_A.exp3.json` → `emo_a.exp3`).
pub fn text_key(path: &str) -> String {
    let file = path.rsplit(['/', '\\']).next().unwrap_or(path);
    let lower = file.to_ascii_lowercase();
    lower.strip_suffix(".json").unwrap_or(&lower).to_owned()
}

/// Joins a manifest path below the model directory, rejecting absolute
/// paths and parent components.
pub fn safe_join(base: &Path, relative: &str) -> Result<PathBuf> {
    let relative = Path::new(relative);
    let mut result = base.to_path_buf();
    for component in relative.components() {
        match component {
            Component::Normal(part) => result.push(part),
            Component::CurDir => {}
            _ => bail!(tr!(
                "清单路径不安全：{}",
                "Unsafe manifest path: {}",
                relative.display()
            )),
        }
    }
    ensure!(
        result != base,
        tr!("清单路径为空", "The manifest path is empty")
    );
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lenna() -> Json {
        serde_json::json!({
            "Version": 3,
            "FileReferences": {
                "Moc": "CHA_Lenna_SSR_01.moc3",
                "Textures": ["CHA_Lenna_SSR_01.2048/texture_00.png"],
                "DisplayInfo": "CHA_Lenna_SSR_01.cdi3.json",
                "Expressions": [
                    {"Name": "A.exp3.json", "File": "expresstions/A.exp3.json"},
                    {"Name": "B.exp3.json", "File": "expresstions/B.exp3.json"}
                ],
                "Motions": {
                    "": [{"File": "motions/M.motion3.json", "FadeInTime": 0}],
                    "Idle": [
                        {"File": "motions/M.motion3.json"},
                        {"File": "motions/N.motion3.json", "Sound": "sounds/n.wav"}
                    ]
                },
                "Physics": "x.physics3.json"
            },
            "Groups": []
        })
    }

    #[test]
    fn collects_references_without_duplicates() {
        let refs = references(&lenna()).unwrap();
        let listed: Vec<(Role, &str)> = refs.iter().map(|r| (r.role, r.path.as_str())).collect();
        assert_eq!(
            listed,
            [
                (Role::Moc, "CHA_Lenna_SSR_01.moc3"),
                (Role::Texture, "CHA_Lenna_SSR_01.2048/texture_00.png"),
                (Role::DisplayInfo, "CHA_Lenna_SSR_01.cdi3.json"),
                (Role::Physics, "x.physics3.json"),
                (Role::Expression, "expresstions/A.exp3.json"),
                (Role::Expression, "expresstions/B.exp3.json"),
                (Role::Motion, "motions/M.motion3.json"),
                (Role::Motion, "motions/N.motion3.json"),
                (Role::Other, "sounds/n.wav"),
            ]
        );
        assert_eq!(
            model_name(&refs, "cha_lenna_ssr_01.model3"),
            "CHA_Lenna_SSR_01"
        );
    }

    #[test]
    fn rejects_malformed_manifests() {
        assert!(references(&serde_json::json!({"Version": 3})).is_err());
        assert!(references(&serde_json::json!({"FileReferences": {"Textures": []}})).is_err());
        assert!(references(&serde_json::json!({"FileReferences": {"Moc": 3}})).is_err());
        assert!(
            references(
                &serde_json::json!({"FileReferences": {"Moc": "a.moc3", "Expressions": {}}})
            )
            .is_err()
        );
    }

    #[test]
    fn keys_and_paths() {
        assert_eq!(text_key("expresstions/EMO_A.exp3.json"), "emo_a.exp3");
        assert_eq!(text_key("X.CDI3.JSON"), "x.cdi3");
        assert_eq!(text_key("a\\b.motion3.json"), "b.motion3");
        assert_eq!(model_name(&[], "cha_x.Model3"), "cha_x");
        let base = Path::new("out");
        assert_eq!(
            safe_join(base, "a/./b.json").unwrap(),
            Path::new("out").join("a").join("b.json")
        );
        assert!(safe_join(base, "../x.json").is_err());
        assert!(safe_join(base, "/x.json").is_err());
        assert!(safe_join(base, "C:\\x.json").is_err());
        assert!(safe_join(base, "").is_err());
    }
}
