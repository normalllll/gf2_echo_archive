//! Physical `.bundle` files hold many concatenated UnityFS sub-bundles. Each
//! sub-bundle's first 32 KiB is XOR-ed with a 16-byte key that can be
//! recovered from the known plain UnityFS header.

use super::reader::Reader;
use anyhow::{Context, Result, bail, ensure};
use std::{
    collections::HashMap,
    io::{Read, Seek, SeekFrom},
};

pub const KNOWN_HEADER: &[u8; 16] = b"UnityFS\0\0\0\0\x075.x.";
pub const XOR_LIMIT: u64 = 32_768;
/// Long enough for the UnityFS signature, versions and size fields.
const HEADER_PROBE: usize = 96;

pub fn header_key(prefix: &[u8]) -> Result<[u8; 16]> {
    ensure!(
        prefix.len() >= 16,
        tr!(
            "UnityFS 加密头被截断",
            "The UnityFS encryption header is truncated"
        )
    );
    let mut key = [0u8; 16];
    for (index, value) in key.iter_mut().enumerate() {
        *value = prefix[index] ^ KNOWN_HEADER[index];
    }
    Ok(key)
}

/// Decrypts `data`, which starts at byte `position` of its sub-bundle.
pub fn decrypt_in_place(data: &mut [u8], key: &[u8; 16], position: u64) {
    if position >= XOR_LIMIT {
        return;
    }
    let end = ((XOR_LIMIT - position) as usize).min(data.len());
    for (index, value) in data[..end].iter_mut().enumerate() {
        *value ^= key[((position as usize) + index) % 16];
    }
}

/// Decrypts a whole sub-bundle held in memory.
#[cfg(test)]
pub fn decrypt(data: &[u8]) -> Result<Vec<u8>> {
    let key = header_key(data)?;
    let mut result = data.to_vec();
    decrypt_in_place(&mut result, &key, 0);
    Ok(result)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BundleHeader {
    pub version: u32,
    pub unity_version: String,
    pub unity_revision: String,
    pub size: u64,
    pub compressed_info_size: u32,
    pub uncompressed_info_size: u32,
    pub flags: u32,
    /// Offset after the fixed header, including version 7 alignment.
    pub header_end: u64,
}

pub fn parse_header(data: &[u8]) -> Result<BundleHeader> {
    let mut reader = Reader::new(data, true);
    ensure!(
        reader.bytes(8)? == b"UnityFS\0",
        tr!("不是 UnityFS 数据", "Not UnityFS data")
    );
    let version = reader.u32()?;
    let unity_version = reader.cstring()?;
    let unity_revision = reader.cstring()?;
    let size = reader.u64()?;
    let compressed_info_size = reader.u32()?;
    let uncompressed_info_size = reader.u32()?;
    let flags = reader.u32()?;
    let mut header_end = reader.position as u64;
    if version >= 7 {
        header_end = header_end.div_ceil(16) * 16;
    }
    Ok(BundleHeader {
        version,
        unity_version,
        unity_revision,
        size,
        compressed_info_size,
        uncompressed_info_size,
        flags,
        header_end,
    })
}

fn read_at<R: Read + Seek>(source: &mut R, position: u64, length: usize) -> Result<Vec<u8>> {
    source.seek(SeekFrom::Start(position))?;
    let mut buffer = vec![0; length];
    source.read_exact(&mut buffer)?;
    Ok(buffer)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SubBundle {
    pub offset: u64,
    pub size: u64,
}

/// Finds sub-bundle boundaries. Problems end the walk and become warnings,
/// matching the reference scanner.
pub fn split_physical_bundle<R: Read + Seek>(
    source: &mut R,
    total: u64,
    label: &str,
) -> (Vec<SubBundle>, Vec<String>) {
    let mut result = Vec::new();
    let mut warnings = Vec::new();
    let mut offset = 0u64;
    while offset < total {
        let available = (total - offset).min(HEADER_PROBE as u64) as usize;
        let mut header = match read_at(source, offset, available) {
            Ok(header) => header,
            Err(error) => {
                warnings.push(format!("{label}@{offset}: {error}"));
                break;
            }
        };
        if offset == 0 && header.starts_with(b"GFF\0") {
            break; // Manifest / name table, not an asset bundle.
        }
        if header.len() < 38 {
            warnings.push(format!("{label}@{offset}: truncated header"));
            break;
        }
        let key = header_key(&header).expect("length checked");
        decrypt_in_place(&mut header, &key, 0);
        let size = match parse_header(&header) {
            Ok(parsed) => parsed.size,
            Err(_) => u64::from_be_bytes(header[30..38].try_into().expect("8 bytes")),
        };
        if size < 48 || size > total - offset {
            warnings.push(format!("{label}@{offset}: invalid sub-bundle size"));
            break;
        }
        result.push(SubBundle { offset, size });
        offset += size;
    }
    (result, warnings)
}

#[derive(Clone, Debug)]
struct Block {
    uncompressed_offset: u64,
    uncompressed_size: u32,
    compressed_offset: u64,
    compressed_size: u32,
    flags: u16,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Node {
    pub offset: u64,
    pub size: u64,
    pub flags: u32,
    pub path: String,
}

impl Node {
    pub fn is_serialized_file(&self) -> bool {
        self.flags & 4 != 0
    }
}

fn decompress(data: &[u8], mode: u32, size: usize) -> Result<Vec<u8>> {
    let result = match mode {
        0 => data.to_vec(),
        2 | 3 => lz4_flex::block::decompress(data, size)
            .context(tr!("LZ4 解压失败", "LZ4 decompression failed"))?,
        other => bail!(tr!(
            "不支持的 UnityFS 压缩方式 {other}",
            "Unsupported UnityFS compression {other}",
            other = other
        )),
    };
    ensure!(
        result.len() == size,
        tr!(
            "UnityFS 数据块大小不符",
            "The UnityFS block size does not match"
        )
    );
    Ok(result)
}

/// One UnityFS sub-bundle with lazily decompressed data blocks.
pub struct UnityFs<R> {
    source: R,
    base: u64,
    key: [u8; 16],
    pub header: BundleHeader,
    blocks: Vec<Block>,
    pub nodes: Vec<Node>,
    cache: HashMap<usize, Vec<u8>>,
}

impl<R: Read + Seek> UnityFs<R> {
    /// `base` and `size` locate the sub-bundle inside `source`.
    pub fn open(mut source: R, base: u64, size: u64) -> Result<Self> {
        let probe = read_at(&mut source, base, size.min(HEADER_PROBE as u64) as usize)?;
        let key = header_key(&probe)?;
        let mut fs = Self {
            source,
            base,
            key,
            header: BundleHeader {
                version: 0,
                unity_version: String::new(),
                unity_revision: String::new(),
                size,
                compressed_info_size: 0,
                uncompressed_info_size: 0,
                flags: 0,
                header_end: 0,
            },
            blocks: Vec::new(),
            nodes: Vec::new(),
            cache: HashMap::new(),
        };
        let mut probe = probe;
        decrypt_in_place(&mut probe, &key, 0);
        let header = parse_header(&probe)?;
        ensure!(
            header.size == size,
            tr!(
                "UnityFS 文件大小不符",
                "The UnityFS file size does not match"
            )
        );
        let flags = header.flags;
        let at_end = flags & 0x80 != 0;
        let info_at = if at_end {
            size.checked_sub(u64::from(header.compressed_info_size))
                .context(tr!(
                    "UnityFS 块信息位置无效",
                    "Invalid UnityFS block info position"
                ))?
        } else {
            header.header_end
        };
        ensure!(
            info_at + u64::from(header.compressed_info_size) <= size,
            tr!(
                "UnityFS 块信息超出文件",
                "The UnityFS block info is outside the file"
            )
        );
        let raw = fs.raw(info_at, header.compressed_info_size as usize)?;
        let info = decompress(&raw, flags & 63, header.uncompressed_info_size as usize)?;
        let mut payload_at = if at_end {
            header.header_end
        } else {
            info_at + u64::from(header.compressed_info_size)
        };
        if flags & 0x200 != 0 {
            payload_at = payload_at.div_ceil(16) * 16;
        }
        let mut reader = Reader::new(&info, true);
        reader.skip(16)?;
        let block_count = reader.count()?;
        let mut uncompressed_offset = 0u64;
        for _ in 0..block_count {
            let uncompressed_size = reader.u32()?;
            let compressed_size = reader.u32()?;
            let block_flags = reader.u16()?;
            ensure!(
                payload_at + u64::from(compressed_size) <= size,
                tr!(
                    "UnityFS 数据块超出文件",
                    "A UnityFS block is outside the file"
                )
            );
            fs.blocks.push(Block {
                uncompressed_offset,
                uncompressed_size,
                compressed_offset: payload_at,
                compressed_size,
                flags: block_flags,
            });
            uncompressed_offset += u64::from(uncompressed_size);
            payload_at += u64::from(compressed_size);
        }
        let node_count = reader.count()?;
        for _ in 0..node_count {
            let offset = reader.u64()?;
            let node_size = reader.u64()?;
            let node_flags = reader.u32()?;
            let path = reader.cstring()?;
            ensure!(
                offset
                    .checked_add(node_size)
                    .is_some_and(|end| end <= uncompressed_offset),
                tr!(
                    "UnityFS 目录项超出数据范围",
                    "A UnityFS directory entry is outside the data"
                )
            );
            fs.nodes.push(Node {
                offset,
                size: node_size,
                flags: node_flags,
                path,
            });
        }
        fs.header = header;
        Ok(fs)
    }

    /// Reads raw (still compressed) sub-bundle bytes and removes the XOR layer.
    fn raw(&mut self, position: u64, length: usize) -> Result<Vec<u8>> {
        let mut data = read_at(&mut self.source, self.base + position, length)?;
        decrypt_in_place(&mut data, &self.key, position);
        Ok(data)
    }

    fn block(&mut self, index: usize) -> Result<&[u8]> {
        if !self.cache.contains_key(&index) {
            let block = self.blocks[index].clone();
            let raw = self.raw(block.compressed_offset, block.compressed_size as usize)?;
            let data = decompress(
                &raw,
                u32::from(block.flags & 63),
                block.uncompressed_size as usize,
            )?;
            self.cache.insert(index, data);
        }
        Ok(&self.cache[&index])
    }

    pub fn total_size(&self) -> u64 {
        self.blocks.last().map_or(0, |block| {
            block.uncompressed_offset + u64::from(block.uncompressed_size)
        })
    }

    /// Reads `length` bytes of the decompressed data stream, touching only the
    /// blocks that cover the range.
    pub fn read(&mut self, start: u64, length: usize) -> Result<Vec<u8>> {
        let end = start
            .checked_add(length as u64)
            .filter(|end| *end <= self.total_size())
            .context(tr!(
                "读取范围超出 UnityFS 数据",
                "The read range is outside the UnityFS data"
            ))?;
        let mut output = Vec::with_capacity(length);
        let first = self.blocks.partition_point(|block| {
            block.uncompressed_offset + u64::from(block.uncompressed_size) <= start
        });
        let mut index = first;
        while (output.len() as u64) < end - start {
            let block_start = self.blocks[index].uncompressed_offset;
            let data = self.block(index)?;
            let from = (start + output.len() as u64 - block_start) as usize;
            let take = (data.len() - from).min((end - start) as usize - output.len());
            output.extend_from_slice(&data[from..from + take]);
            index += 1;
        }
        Ok(output)
    }

    /// Reads a range relative to a directory node.
    pub fn read_node(&mut self, node: &Node, start: u64, length: usize) -> Result<Vec<u8>> {
        ensure!(
            start
                .checked_add(length as u64)
                .is_some_and(|end| end <= node.size),
            tr!("读取范围超出 {}", "The read range is outside {}", node.path)
        );
        self.read(node.offset + start, length)
    }

    #[cfg(test)]
    pub fn decompressed_block_count(&self) -> usize {
        self.cache.len()
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::io::Cursor;

    /// Builds an uncompressed UnityFS v7 image holding the given nodes.
    pub fn build_bundle(nodes: &[(&str, &[u8], u32)], block_size: usize) -> Vec<u8> {
        let payload: Vec<u8> = nodes
            .iter()
            .flat_map(|(_, data, _)| data.to_vec())
            .collect();
        let mut info = vec![0u8; 16];
        let chunks: Vec<&[u8]> = payload.chunks(block_size).collect();
        info.extend((chunks.len() as i32).to_be_bytes());
        for chunk in &chunks {
            info.extend((chunk.len() as u32).to_be_bytes());
            info.extend((chunk.len() as u32).to_be_bytes());
            info.extend(0u16.to_be_bytes());
        }
        info.extend((nodes.len() as i32).to_be_bytes());
        let mut offset = 0u64;
        for (name, data, flags) in nodes {
            info.extend(offset.to_be_bytes());
            info.extend((data.len() as u64).to_be_bytes());
            info.extend(flags.to_be_bytes());
            info.extend(name.as_bytes());
            info.push(0);
            offset += data.len() as u64;
        }
        let mut header = b"UnityFS\0".to_vec();
        header.extend(7u32.to_be_bytes());
        header.extend(b"5.x.\0");
        header.extend(b"2019.4.29f1\0");
        let fixed = header.len() + 20;
        let aligned = fixed.div_ceil(16) * 16;
        let size = (aligned + info.len() + payload.len()) as u64;
        header.extend(size.to_be_bytes());
        header.extend((info.len() as u32).to_be_bytes());
        header.extend((info.len() as u32).to_be_bytes());
        header.extend(0u32.to_be_bytes());
        header.resize(aligned, 0);
        header.extend(info);
        header.extend(payload);
        header
    }

    pub fn encrypt(data: &[u8], key: &[u8; 16]) -> Vec<u8> {
        let mut result = data.to_vec();
        decrypt_in_place(&mut result, key, 0);
        result
    }

    #[test]
    fn xor_key_round_trip_and_limit() {
        let plain: Vec<u8> = KNOWN_HEADER
            .iter()
            .copied()
            .chain((0..40_000u32).map(|v| v as u8))
            .collect();
        let key = *b"0123456789abcdef";
        let encrypted = encrypt(&plain, &key);
        assert_eq!(header_key(&encrypted).unwrap(), key);
        assert_eq!(decrypt(&encrypted).unwrap(), plain);
        // Bytes at and beyond 32 KiB are untouched.
        assert_eq!(encrypted[32_768..], plain[32_768..]);
        assert_ne!(encrypted[32_767], plain[32_767]);
        // Partial decryption honours the absolute position.
        let mut tail = encrypted[32_760..32_780].to_vec();
        decrypt_in_place(&mut tail, &key, 32_760);
        assert_eq!(tail, plain[32_760..32_780]);
    }

    #[test]
    fn splits_sub_bundles_and_skips_manifest() {
        let first = encrypt(
            &build_bundle(&[("CAB-a", b"hello", 4)], 4),
            b"aaaaaaaaaaaaaaaa",
        );
        let second = encrypt(
            &build_bundle(&[("CAB-b", b"world!", 4)], 4),
            b"bbbbbbbbbbbbbbbb",
        );
        let mut physical = first.clone();
        physical.extend(&second);
        let total = physical.len() as u64;
        let (parts, warnings) = split_physical_bundle(&mut Cursor::new(&physical), total, "x");
        assert!(warnings.is_empty());
        assert_eq!(
            parts,
            vec![
                SubBundle {
                    offset: 0,
                    size: first.len() as u64
                },
                SubBundle {
                    offset: first.len() as u64,
                    size: second.len() as u64
                }
            ]
        );
        let mut fs = UnityFs::open(Cursor::new(&physical), parts[1].offset, parts[1].size).unwrap();
        let node = fs.nodes[0].clone();
        assert_eq!(node.path, "CAB-b");
        assert_eq!(fs.read_node(&node, 1, 4).unwrap(), b"orld");

        let manifest = b"GFF\0anything".to_vec();
        let (parts, warnings) =
            split_physical_bundle(&mut Cursor::new(&manifest), manifest.len() as u64, "m");
        assert!(parts.is_empty() && warnings.is_empty());

        let mut truncated = physical.clone();
        truncated.truncate(first.len() + 10);
        let (parts, warnings) =
            split_physical_bundle(&mut Cursor::new(&truncated), truncated.len() as u64, "t");
        assert_eq!(parts.len(), 1);
        assert_eq!(warnings.len(), 1);
    }

    #[test]
    fn reads_across_block_boundaries_lazily() {
        let data: Vec<u8> = (0..100u8).collect();
        let image = build_bundle(&[("CAB-x", &data, 4)], 16);
        let mut fs = UnityFs::open(Cursor::new(&image), 0, image.len() as u64).unwrap();
        assert_eq!(fs.total_size(), 100);
        assert_eq!(fs.read(10, 4).unwrap(), (10..14).collect::<Vec<u8>>());
        assert_eq!(fs.decompressed_block_count(), 1);
        assert_eq!(fs.read(14, 20).unwrap(), (14..34).collect::<Vec<u8>>());
        assert_eq!(fs.decompressed_block_count(), 3);
        assert!(fs.read(90, 20).is_err());
    }

    #[test]
    fn decompresses_lz4_blocks() {
        let data = b"abcabcabcabcabcabcabcabc".repeat(10);
        let compressed = lz4_flex::block::compress(&data);
        assert_eq!(decompress(&compressed, 2, data.len()).unwrap(), data);
        assert!(decompress(&compressed, 2, data.len() + 1).is_err());
        assert!(decompress(&compressed, 1, data.len()).is_err());
    }

    #[test]
    fn rejects_wrong_size_header() {
        let mut image = build_bundle(&[("CAB-x", b"abc", 4)], 16);
        let size = image.len() as u64;
        assert!(UnityFs::open(Cursor::new(&image), 0, size - 1).is_err());
        image[0] = b'X';
        assert!(parse_header(&image).is_err());
    }
}
