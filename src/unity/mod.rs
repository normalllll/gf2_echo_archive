//! Minimal Unity asset readers: physical bundle splitting, sub-bundle header
//! decryption, UnityFS, SerializedFile, type tree values, sprites and
//! textures.

pub mod bundle;
pub mod reader;
pub mod serialized;
pub mod sprite;
pub mod texture;
pub mod typetree;

pub use bundle::{SubBundle, UnityFs, split_physical_bundle};
pub use typetree::Value;
