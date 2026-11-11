use anyhow::{Context, Result};
use serde::Deserialize;
use std::collections::HashSet;

const CHARACTER_CATALOG_JSON: &str = include_str!("../resources/characters.json");

#[derive(Clone, Debug, Deserialize)]
pub struct CharacterOption {
    pub official_name: String,
    /// Official name in the English (global) release.
    pub english_name: String,
    pub resource_name: String,
    #[serde(default)]
    pub aliases: Vec<String>,
}

#[derive(Deserialize)]
struct CharacterCatalog {
    characters: Vec<CharacterOption>,
}

pub fn load_character_options() -> Result<Vec<CharacterOption>> {
    let catalog: CharacterCatalog = serde_json::from_str(CHARACTER_CATALOG_JSON).context(tr!(
        "内置角色目录 JSON 格式无效",
        "The built-in character list is not valid JSON"
    ))?;
    validate_catalog(&catalog.characters)?;
    Ok(catalog.characters)
}

fn validate_catalog(characters: &[CharacterOption]) -> Result<()> {
    anyhow::ensure!(
        !characters.is_empty(),
        tr!("内置角色目录为空", "The built-in character list is empty")
    );

    let mut known_resource_names = HashSet::new();
    for character in characters {
        anyhow::ensure!(
            !character.official_name.trim().is_empty(),
            tr!(
                "角色的官方中文名不能为空",
                "A character's official Chinese name cannot be empty"
            )
        );
        anyhow::ensure!(
            !character.english_name.trim().is_empty(),
            tr!(
                "角色“{}”的英文名不能为空",
                "The English name of character \"{}\" cannot be empty",
                character.official_name
            )
        );
        anyhow::ensure!(
            !character.resource_name.trim().is_empty(),
            tr!(
                "角色“{}”的资源名不能为空",
                "The resource name of character \"{}\" cannot be empty",
                character.official_name
            )
        );
        for resource_name in std::iter::once(&character.resource_name).chain(&character.aliases) {
            anyhow::ensure!(
                known_resource_names.insert(resource_name.to_ascii_lowercase()),
                tr!(
                    "资源名或别名“{resource_name}”重复",
                    "Duplicate resource name or alias \"{resource_name}\"",
                    resource_name = resource_name
                )
            );
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_character_catalog_is_valid() {
        let characters = load_character_options().unwrap();

        assert!(characters.len() >= 60);
        assert!(characters.iter().any(|character| {
            character.official_name == "托洛洛" && character.resource_name == "Tololo"
        }));
    }
}
