//! `Sprite` fields and cropping a sprite out of its decoded texture.

use super::typetree::Value;
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};

/// A `PPtr`: `file_id` 0 is the same SerializedFile, n is external n - 1.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PPtr {
    pub file_id: i32,
    pub path_id: i64,
}

impl PPtr {
    pub fn from_value(value: Option<&Value>) -> Self {
        let field = |name| value.and_then(|v| v.get(name)).and_then(Value::as_i64);
        Self {
            file_id: field("m_FileID").unwrap_or(0) as i32,
            path_id: field("m_PathID").unwrap_or(0),
        }
    }

    pub fn is_null(&self) -> bool {
        self.path_id == 0
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

impl Rect {
    fn from_value(value: Option<&Value>) -> Result<Self> {
        let value = value.context(tr!("缺少矩形字段", "A rectangle field is missing"))?;
        let field = |name| match value.get(name) {
            Some(Value::Float(v)) => Ok(*v as f32),
            _ => anyhow::bail!(tr!(
                "矩形缺少 {name}",
                "The rectangle has no {name}",
                name = name
            )),
        };
        Ok(Self {
            x: field("x")?,
            y: field("y")?,
            width: field("width")?,
            height: field("height")?,
        })
    }
}

/// `SpriteRenderData.settingsRaw` bit fields.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpriteSettings(pub u32);

impl SpriteSettings {
    pub fn packed(self) -> bool {
        self.0 & 1 != 0
    }
    /// 0 = tight, 1 = rectangle.
    pub fn packing_mode(self) -> u32 {
        (self.0 >> 1) & 1
    }
    pub fn packing_rotation(self) -> u32 {
        (self.0 >> 2) & 0xf
    }
    /// Non-empty when the sprite was packed tightly or rotated; such sprites
    /// are still cropped by their rectangle only.
    pub fn packing_label(self) -> Option<String> {
        if !self.packed() {
            return None;
        }
        let mut parts = Vec::new();
        if self.packing_mode() == 0 {
            parts.push("tight".to_owned());
        }
        if self.packing_rotation() != 0 {
            parts.push(format!("rotation {}", self.packing_rotation()));
        }
        (!parts.is_empty()).then(|| parts.join(", "))
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Sprite {
    pub name: String,
    pub rect: Rect,
    pub texture: PPtr,
    pub alpha_texture: PPtr,
    pub sprite_atlas: PPtr,
    pub texture_rect: Rect,
    pub settings: SpriteSettings,
}

impl Sprite {
    pub fn from_value(value: &Value) -> Result<Self> {
        let name = value
            .get("m_Name")
            .and_then(Value::as_str)
            .context(tr!("Sprite 缺少 m_Name", "The sprite has no m_Name"))?
            .to_owned();
        let render = value
            .get("m_RD")
            .context(tr!("Sprite 缺少 m_RD", "The sprite has no m_RD"))?;
        Ok(Self {
            name,
            rect: Rect::from_value(value.get("m_Rect"))?,
            texture: PPtr::from_value(render.get("texture")),
            alpha_texture: PPtr::from_value(render.get("alphaTexture")),
            sprite_atlas: PPtr::from_value(value.get("m_SpriteAtlas")),
            texture_rect: Rect::from_value(render.get("textureRect"))?,
            settings: SpriteSettings(
                render
                    .get("settingsRaw")
                    .and_then(Value::as_i64)
                    .unwrap_or(0) as u32,
            ),
        })
    }
}

/// Python-style `round()` (ties to even), as Pillow's `crop` applies it.
fn round_coordinate(value: f32) -> i64 {
    f64::from(value).round_ties_even() as i64
}

/// Integer crop box `(left, bottom, width, height)` in texture pixels,
/// clamped to the texture.
pub fn crop_box(rect: Rect, texture_width: usize, texture_height: usize) -> Result<[usize; 4]> {
    let left = round_coordinate(rect.x).clamp(0, texture_width as i64);
    let bottom = round_coordinate(rect.y).clamp(0, texture_height as i64);
    let right = round_coordinate(rect.x + rect.width).clamp(left, texture_width as i64);
    let top = round_coordinate(rect.y + rect.height).clamp(bottom, texture_height as i64);
    ensure!(
        right > left && top > bottom,
        tr!("Sprite 区域为空", "The sprite area is empty")
    );
    Ok([
        left as usize,
        bottom as usize,
        (right - left) as usize,
        (top - bottom) as usize,
    ])
}

/// Crops `rect` from a bottom-up RGBA texture and returns top-down RGBA rows.
pub fn crop_flip(
    texture: &[u8],
    texture_width: usize,
    texture_height: usize,
    rect: Rect,
) -> Result<(usize, usize, Vec<u8>)> {
    ensure!(
        texture.len() == texture_width * texture_height * 4,
        tr!(
            "贴图像素数与尺寸不符",
            "The texture pixel count does not match its size"
        )
    );
    let [left, bottom, width, height] = crop_box(rect, texture_width, texture_height)?;
    let mut output = Vec::with_capacity(width * height * 4);
    for row in (bottom..bottom + height).rev() {
        let start = (row * texture_width + left) * 4;
        output.extend_from_slice(&texture[start..start + width * 4]);
    }
    Ok((width, height, output))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 3×2 texture, bottom row first: pixel value = (x, y).
    fn texture() -> Vec<u8> {
        let mut data = Vec::new();
        for y in 0..2u8 {
            for x in 0..3u8 {
                data.extend([x, y, 0, 255]);
            }
        }
        data
    }

    #[test]
    fn flips_full_texture() {
        let rect = Rect {
            x: 0.0,
            y: 0.0,
            width: 3.0,
            height: 2.0,
        };
        let (w, h, pixels) = crop_flip(&texture(), 3, 2, rect).unwrap();
        assert_eq!((w, h), (3, 2));
        // First output row is the texture's top row (y = 1).
        assert_eq!(&pixels[..4], &[0, 1, 0, 255]);
        assert_eq!(&pixels[12..16], &[0, 0, 0, 255]);
    }

    #[test]
    fn crops_sub_rect_with_python_rounding() {
        let rect = Rect {
            x: 1.4,
            y: 0.5,
            width: 1.2,
            height: 0.9,
        };
        // left round(1.4)=1, right round(2.6)=3, bottom round(0.5)=0 (even),
        // top round(1.4)=1.
        assert_eq!(crop_box(rect, 3, 2).unwrap(), [1, 0, 2, 1]);
        let (w, h, pixels) = crop_flip(&texture(), 3, 2, rect).unwrap();
        assert_eq!((w, h), (2, 1));
        assert_eq!(pixels, [1, 0, 0, 255, 2, 0, 0, 255]);
        let empty = Rect {
            x: 5.0,
            y: 0.0,
            width: 1.0,
            height: 1.0,
        };
        assert!(crop_box(empty, 3, 2).is_err());
        assert!(crop_flip(&texture()[..20], 3, 2, rect).is_err());
    }

    #[test]
    fn packing_labels() {
        assert_eq!(SpriteSettings(64).packing_label(), None);
        assert_eq!(SpriteSettings(3).packing_label(), None);
        assert_eq!(SpriteSettings(1).packing_label().as_deref(), Some("tight"));
        assert_eq!(
            SpriteSettings(3 | (4 << 2)).packing_label().as_deref(),
            Some("rotation 4")
        );
    }
}
