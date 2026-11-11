//! `Texture2D` fields and pixel decoding. Decoded images are RGBA8 in
//! Unity's row order (bottom row first); callers flip when cropping.

use super::typetree::Value;
use anyhow::{Context, Result, bail, ensure};

/// Unity `TextureFormat` values this reader knows by name.
pub fn format_name(format: i64) -> String {
    let name = match format {
        1 => "Alpha8",
        2 => "ARGB4444",
        3 => "RGB24",
        4 => "RGBA32",
        5 => "ARGB32",
        7 => "RGB565",
        9 => "R16",
        10 => "DXT1",
        12 => "DXT5",
        13 => "RGBA4444",
        14 => "BGRA32",
        15 => "RHalf",
        16 => "RGHalf",
        17 => "RGBAHalf",
        18 => "RFloat",
        19 => "RGFloat",
        20 => "RGBAFloat",
        22 => "RGB9e5Float",
        24 => "BC6H",
        25 => "BC7",
        26 => "BC4",
        27 => "BC5",
        28 => "DXT1Crunched",
        29 => "DXT5Crunched",
        30 => "PVRTC_RGB2",
        31 => "PVRTC_RGBA2",
        32 => "PVRTC_RGB4",
        33 => "PVRTC_RGBA4",
        34 => "ETC_RGB4",
        41 => "EAC_R",
        42 => "EAC_R_SIGNED",
        43 => "EAC_RG",
        44 => "EAC_RG_SIGNED",
        45 => "ETC2_RGB",
        46 => "ETC2_RGBA1",
        47 => "ETC2_RGBA8",
        48 => "ASTC_RGB_4x4",
        49 => "ASTC_RGB_5x5",
        50 => "ASTC_RGB_6x6",
        51 => "ASTC_RGB_8x8",
        52 => "ASTC_RGB_10x10",
        53 => "ASTC_RGB_12x12",
        54 => "ASTC_RGBA_4x4",
        55 => "ASTC_RGBA_5x5",
        56 => "ASTC_RGBA_6x6",
        57 => "ASTC_RGBA_8x8",
        58 => "ASTC_RGBA_10x10",
        59 => "ASTC_RGBA_12x12",
        62 => "RG16",
        63 => "R8",
        64 => "ETC_RGB4Crunched",
        65 => "ETC2_RGBA8Crunched",
        66 => "ASTC_HDR_4x4",
        67 => "ASTC_HDR_5x5",
        68 => "ASTC_HDR_6x6",
        69 => "ASTC_HDR_8x8",
        70 => "ASTC_HDR_10x10",
        71 => "ASTC_HDR_12x12",
        _ => return format!("TextureFormat {format}"),
    };
    name.to_owned()
}

/// How a format is decoded; `None` means this reader cannot decode it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Codec {
    Raw(RawLayout),
    Block(BlockCodec),
    Astc(usize),
    Crunch,
    Pvrtc2,
    Pvrtc4,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RawLayout {
    Alpha8,
    Argb4444,
    Rgb24,
    Rgba32,
    Argb32,
    Rgb565,
    R16,
    Rgba4444,
    Bgra32,
    Rg16,
    R8,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BlockCodec {
    Bc1,
    Bc3,
    Bc4,
    Bc5,
    Bc7,
    Etc1,
    Etc2Rgb,
    Etc2Rgba1,
    Etc2Rgba8,
    EacR,
    EacRSigned,
    EacRg,
    EacRgSigned,
}

fn codec(format: i64) -> Option<Codec> {
    use BlockCodec::*;
    use RawLayout::*;
    Some(match format {
        1 => Codec::Raw(Alpha8),
        2 => Codec::Raw(Argb4444),
        3 => Codec::Raw(Rgb24),
        4 => Codec::Raw(Rgba32),
        5 => Codec::Raw(Argb32),
        7 => Codec::Raw(Rgb565),
        9 => Codec::Raw(R16),
        13 => Codec::Raw(Rgba4444),
        14 => Codec::Raw(Bgra32),
        62 => Codec::Raw(Rg16),
        63 => Codec::Raw(R8),
        10 => Codec::Block(Bc1),
        12 => Codec::Block(Bc3),
        26 => Codec::Block(Bc4),
        27 => Codec::Block(Bc5),
        25 => Codec::Block(Bc7),
        34 => Codec::Block(Etc1),
        45 => Codec::Block(Etc2Rgb),
        46 => Codec::Block(Etc2Rgba1),
        47 => Codec::Block(Etc2Rgba8),
        41 => Codec::Block(EacR),
        42 => Codec::Block(EacRSigned),
        43 => Codec::Block(EacRg),
        44 => Codec::Block(EacRgSigned),
        48 | 54 => Codec::Astc(4),
        49 | 55 => Codec::Astc(5),
        50 | 56 => Codec::Astc(6),
        51 | 57 => Codec::Astc(8),
        52 | 58 => Codec::Astc(10),
        53 | 59 => Codec::Astc(12),
        28 | 29 | 64 | 65 => Codec::Crunch,
        30 | 31 => Codec::Pvrtc2,
        32 | 33 => Codec::Pvrtc4,
        _ => return None,
    })
}

pub fn is_supported(format: i64) -> bool {
    codec(format).is_some()
}

fn raw_bytes_per_pixel(layout: RawLayout) -> usize {
    use RawLayout::*;
    match layout {
        Alpha8 | R8 => 1,
        Argb4444 | Rgb565 | R16 | Rgba4444 | Rg16 => 2,
        Rgb24 => 3,
        Rgba32 | Argb32 | Bgra32 => 4,
    }
}

fn expand4(value: u16) -> u8 {
    (value & 0xf) as u8 * 17
}

fn decode_raw(layout: RawLayout, data: &[u8], pixels: usize) -> Result<Vec<u8>> {
    use RawLayout::*;
    let step = raw_bytes_per_pixel(layout);
    ensure!(
        data.len() >= pixels * step,
        tr!(
            "贴图数据不足：需要 {} 字节，只有 {}",
            "Not enough texture data: needs {} bytes, has {}",
            pixels * step,
            data.len()
        )
    );
    let mut output = Vec::with_capacity(pixels * 4);
    for px in data[..pixels * step].chunks_exact(step) {
        let rgba = match layout {
            Alpha8 => [255, 255, 255, px[0]],
            R8 => [px[0], 0, 0, 255],
            Rgb24 => [px[0], px[1], px[2], 255],
            Rgba32 => [px[0], px[1], px[2], px[3]],
            Argb32 => [px[1], px[2], px[3], px[0]],
            Bgra32 => [px[2], px[1], px[0], px[3]],
            Rg16 => [px[0], px[1], 0, 255],
            R16 => [px[1], 0, 0, 255],
            Rgb565 => {
                let v = u16::from_le_bytes([px[0], px[1]]);
                let r = ((v >> 11) & 0x1f) as u32;
                let g = ((v >> 5) & 0x3f) as u32;
                let b = (v & 0x1f) as u32;
                [
                    ((r * 255 + 15) / 31) as u8,
                    ((g * 255 + 31) / 63) as u8,
                    ((b * 255 + 15) / 31) as u8,
                    255,
                ]
            }
            Rgba4444 => {
                let v = u16::from_le_bytes([px[0], px[1]]);
                [
                    expand4(v >> 12),
                    expand4(v >> 8),
                    expand4(v >> 4),
                    expand4(v),
                ]
            }
            Argb4444 => {
                let v = u16::from_le_bytes([px[0], px[1]]);
                [
                    expand4(v >> 8),
                    expand4(v >> 4),
                    expand4(v),
                    expand4(v >> 12),
                ]
            }
        };
        output.extend_from_slice(&rgba);
    }
    Ok(output)
}

/// `texture2ddecoder` writes `u32::from_le_bytes([b, g, r, a])` per pixel.
fn bgra_words_to_rgba(words: &[u32]) -> Vec<u8> {
    let mut output = Vec::with_capacity(words.len() * 4);
    for word in words {
        let [b, g, r, a] = word.to_le_bytes();
        output.extend_from_slice(&[r, g, b, a]);
    }
    output
}

/// Decodes the first mip level to RGBA8, rows bottom-up as stored by Unity.
pub fn decode(format: i64, width: usize, height: usize, data: &[u8]) -> Result<Vec<u8>> {
    ensure!(
        width > 0 && height > 0,
        tr!(
            "贴图尺寸无效：{width}×{height}",
            "Invalid texture size: {width}×{height}",
            width = width,
            height = height
        )
    );
    ensure!(
        width <= 16_384 && height <= 16_384,
        tr!("贴图尺寸过大", "The texture is too large")
    );
    let codec = codec(format).with_context(|| {
        tr!(
            "不支持的贴图格式 {}",
            "Unsupported texture format {}",
            format_name(format)
        )
    })?;
    let pixels = width * height;
    if let Codec::Raw(layout) = codec {
        return decode_raw(layout, data, pixels);
    }
    let mut words = vec![0u32; pixels];
    let result = match codec {
        Codec::Raw(_) => unreachable!(),
        Codec::Block(block) => {
            use BlockCodec::*;
            use texture2ddecoder as t;
            let decoder = match block {
                Bc1 => t::decode_bc1,
                Bc3 => t::decode_bc3,
                Bc4 => t::decode_bc4,
                Bc5 => t::decode_bc5,
                Bc7 => t::decode_bc7,
                Etc1 => t::decode_etc1,
                Etc2Rgb => t::decode_etc2_rgb,
                Etc2Rgba1 => t::decode_etc2_rgba1,
                Etc2Rgba8 => t::decode_etc2_rgba8,
                EacR => t::decode_eacr,
                EacRSigned => t::decode_eacr_signed,
                EacRg => t::decode_eacrg,
                EacRgSigned => t::decode_eacrg_signed,
            };
            decoder(data, width, height, &mut words)
        }
        Codec::Astc(block) => {
            texture2ddecoder::decode_astc(data, width, height, block, block, &mut words)
        }
        Codec::Crunch => texture2ddecoder::decode_unity_crunch(data, width, height, &mut words),
        Codec::Pvrtc2 => texture2ddecoder::decode_pvrtc_2bpp(data, width, height, &mut words),
        Codec::Pvrtc4 => texture2ddecoder::decode_pvrtc_4bpp(data, width, height, &mut words),
    };
    if let Err(message) = result {
        bail!(tr!(
            "{} 解码失败：{message}",
            "{} failed to decode: {message}",
            format_name(format),
            message = message
        ));
    }
    Ok(bgra_words_to_rgba(&words))
}

/// Where a texture's pixels live.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ImageData {
    Inline(Vec<u8>),
    /// `m_StreamData`: a range of an external `.resS` file.
    Stream {
        path: String,
        offset: u64,
        size: u64,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Texture2D {
    pub name: String,
    pub width: usize,
    pub height: usize,
    pub format: i64,
    pub data: ImageData,
}

fn int(value: &Value, field: &str) -> Result<i64> {
    value.get(field).and_then(Value::as_i64).with_context(|| {
        tr!(
            "Texture2D 缺少 {field}",
            "The Texture2D has no {field}",
            field = field
        )
    })
}

impl Texture2D {
    pub fn from_value(value: &Value) -> Result<Self> {
        let name = value
            .get("m_Name")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        let width = usize::try_from(int(value, "m_Width")?)
            .context(tr!("贴图宽度无效", "Invalid texture width"))?;
        let height = usize::try_from(int(value, "m_Height")?)
            .context(tr!("贴图高度无效", "Invalid texture height"))?;
        let format = int(value, "m_TextureFormat")?;
        let stream = value.get("m_StreamData").and_then(|stream| {
            let path = stream.get("path")?.as_str()?;
            let offset = stream.get("offset")?.as_i64()?;
            let size = stream.get("size")?.as_i64()?;
            (!path.is_empty() && size > 0).then(|| ImageData::Stream {
                path: path.to_owned(),
                offset: offset as u64,
                size: size as u64,
            })
        });
        let data = match stream {
            Some(stream) => stream,
            None => match value.get("image data") {
                Some(Value::Bytes(bytes)) if !bytes.is_empty() => ImageData::Inline(bytes.clone()),
                _ => bail!(tr!(
                    "Texture2D {name} 没有图像数据",
                    "Texture2D {name} has no image data",
                    name = name
                )),
            },
        };
        Ok(Self {
            name,
            width,
            height,
            format,
            data,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dispatches_raw_formats() {
        // 2×1 RGBA32 and the same pixels as ARGB32 / BGRA32.
        let rgba = [1, 2, 3, 4, 5, 6, 7, 8];
        assert_eq!(decode(4, 2, 1, &rgba).unwrap(), rgba);
        assert_eq!(decode(5, 2, 1, &[4, 1, 2, 3, 8, 5, 6, 7]).unwrap(), rgba);
        assert_eq!(decode(14, 2, 1, &[3, 2, 1, 4, 7, 6, 5, 8]).unwrap(), rgba);
        assert_eq!(decode(3, 1, 1, &[9, 8, 7]).unwrap(), [9, 8, 7, 255]);
        assert_eq!(decode(1, 1, 1, &[42]).unwrap(), [255, 255, 255, 42]);
        // RGBA4444: 0xF80F -> r=f, g=8, b=0, a=f.
        assert_eq!(
            decode(13, 1, 1, &0xf80fu16.to_le_bytes()).unwrap(),
            [255, 136, 0, 255]
        );
        assert_eq!(
            decode(7, 1, 1, &0xffffu16.to_le_bytes()).unwrap(),
            [255, 255, 255, 255]
        );
        assert!(decode(4, 2, 1, &rgba[..7]).is_err());
    }

    #[test]
    fn dispatches_block_formats() {
        // One BC1 block: colour0 = white, colour1 = black, all indices 0.
        let block = [0xff, 0xff, 0, 0, 0, 0, 0, 0];
        let pixels = decode(10, 4, 4, &block).unwrap();
        assert_eq!(pixels.len(), 64);
        assert!(pixels.chunks(4).all(|px| px == [255, 255, 255, 255]));
        // Partial blocks are cropped to the image size.
        assert_eq!(decode(10, 2, 3, &block).unwrap().len(), 2 * 3 * 4);
        assert!(decode(12, 4, 4, &block).is_err(), "BC3 needs 16 bytes");
        assert!(decode(54, 4, 4, &[0; 16]).is_ok());
    }

    #[test]
    fn rejects_unknown_and_unsupported_formats() {
        assert!(!is_supported(24));
        assert!(decode(24, 4, 4, &[0; 16]).is_err());
        assert!(decode(999, 4, 4, &[0; 64]).is_err());
        assert!(decode(4, 0, 4, &[]).is_err());
        assert_eq!(format_name(25), "BC7");
        assert_eq!(format_name(999), "TextureFormat 999");
    }

    #[test]
    fn reads_stream_and_inline_fields() {
        let base = |extra: Vec<(String, Value)>| {
            let mut fields = vec![
                ("m_Name".into(), Value::String("t".into())),
                ("m_Width".into(), Value::Int(4)),
                ("m_Height".into(), Value::Int(2)),
                ("m_TextureFormat".into(), Value::Int(4)),
            ];
            fields.extend(extra);
            Value::Object(fields)
        };
        let stream = Value::Object(vec![
            ("offset".into(), Value::UInt(16)),
            ("size".into(), Value::UInt(32)),
            (
                "path".into(),
                Value::String("archive:/CAB-a/CAB-a.resS".into()),
            ),
        ]);
        let texture = Texture2D::from_value(&base(vec![
            ("image data".into(), Value::Bytes(vec![])),
            ("m_StreamData".into(), stream),
        ]))
        .unwrap();
        assert_eq!(
            texture.data,
            ImageData::Stream {
                path: "archive:/CAB-a/CAB-a.resS".into(),
                offset: 16,
                size: 32
            }
        );
        let inline = Texture2D::from_value(&base(vec![(
            "image data".into(),
            Value::Bytes(vec![1; 32]),
        )]))
        .unwrap();
        assert_eq!(inline.data, ImageData::Inline(vec![1; 32]));
        assert!(Texture2D::from_value(&base(vec![])).is_err());
    }
}
