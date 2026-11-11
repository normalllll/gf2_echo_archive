use anyhow::{Context, Result, ensure};
use std::{fs::File, io::Read, path::Path};

// Read the top-level CRI @UTF AwbFile field, not arbitrary AFS2 signatures in cue data.
// ACBs can contain a memory bank even when a sibling streaming AWB also exists.
pub fn has_memory_bank(path: &Path) -> Result<bool> {
    let mut file = File::open(path)?;
    let length = file.metadata()?.len();
    let mut header = [0u8; 32];
    file.read_exact(&mut header)?;
    ensure!(
        &header[..4] == b"@UTF",
        tr!("不是 CRI @UTF 文件", "Not a CRI @UTF file")
    );
    ensure!(
        u64::from(be32(&header, 4)?) + 8 == length,
        tr!("ACB 长度无效", "Invalid ACB length")
    );
    let data_start = be32(&header, 16)? as usize + 8;
    ensure!(
        (32..=2 * 1024 * 1024).contains(&data_start),
        tr!(
            "ACB 表头过大或无效",
            "The ACB header is too large or invalid"
        )
    );
    ensure!(
        data_start as u64 <= length,
        tr!("ACB 数据偏移越界", "The ACB data offset is out of range")
    );
    let mut table = vec![0; data_start];
    table[..32].copy_from_slice(&header);
    file.read_exact(&mut table[32..])?;
    memory_bank_in_header(&table, length)
}

fn be32(bytes: &[u8], at: usize) -> Result<u32> {
    Ok(u32::from_be_bytes(
        bytes
            .get(at..at + 4)
            .context(tr!("ACB 字段越界", "An ACB field is out of range"))?
            .try_into()?,
    ))
}

fn be16(bytes: &[u8], at: usize) -> Result<usize> {
    Ok(u16::from_be_bytes(
        bytes
            .get(at..at + 2)
            .context(tr!("ACB 字段越界", "An ACB field is out of range"))?
            .try_into()?,
    ) as usize)
}

fn memory_bank_in_header(table: &[u8], file_length: u64) -> Result<bool> {
    let rows = be16(table, 10)? + 8;
    let strings = be32(table, 12)? as usize + 8;
    let data = be32(table, 16)? as usize + 8;
    let columns = be16(table, 24)?;
    let row_width = be16(table, 26)?;
    ensure!(
        32 <= rows && rows <= strings && strings <= data && data <= table.len(),
        tr!("ACB 表偏移无效", "Invalid ACB table offset")
    );
    ensure!(
        columns <= 256 && be32(table, 28)? == 1,
        tr!("不支持的 ACB 顶层表", "Unsupported ACB top-level table")
    );
    ensure!(
        rows + row_width <= strings,
        tr!("ACB 行长度无效", "Invalid ACB row length")
    );
    let mut schema = 32;
    let mut row_offset = 0;
    for _ in 0..columns {
        ensure!(
            schema + 5 <= rows,
            tr!("ACB 列定义越界", "An ACB column definition is out of range")
        );
        let flags = table[schema];
        let name_at = strings + be32(table, schema + 1)? as usize;
        let name_tail = table
            .get(name_at..data)
            .context(tr!("ACB 列名越界", "An ACB column name is out of range"))?;
        let name_end = name_tail.iter().position(|byte| *byte == 0).context(tr!(
            "ACB 列名未终止",
            "An ACB column name is not terminated"
        ))?;
        schema += 5;
        let width = match flags & 0x0f {
            0 | 1 => 1,
            2 | 3 => 2,
            4 | 5 | 8 | 10 => 4,
            6 | 7 | 9 | 11 => 8,
            12 => 16,
            _ => anyhow::bail!(tr!("不支持的 ACB 字段类型", "Unsupported ACB field type")),
        };
        ensure!(
            flags & 0x10 != 0 && flags & 0x80 == 0,
            tr!("不支持的 ACB 存储标志", "Unsupported ACB storage flag")
        );
        let value = if flags & 0x20 != 0 {
            let at = schema;
            schema += width;
            ensure!(
                schema <= rows,
                tr!("ACB 常量字段越界", "An ACB constant field is out of range")
            );
            Some(at)
        } else if flags & 0x40 != 0 {
            let at = rows + row_offset;
            row_offset += width;
            ensure!(
                row_offset <= row_width,
                tr!("ACB 行字段越界", "An ACB row field is out of range")
            );
            Some(at)
        } else {
            None
        };
        if &name_tail[..name_end] == b"AwbFile" {
            ensure!(
                flags & 0x0f == 11,
                tr!("AwbFile 不是二进制字段", "AwbFile is not a binary field")
            );
            let Some(at) = value else { return Ok(false) };
            let offset = u64::from(be32(table, at)?);
            let size = u64::from(be32(table, at + 4)?);
            ensure!(
                data as u64 + offset + size <= file_length,
                tr!("内嵌 AWB 越界", "The embedded AWB is out of range")
            );
            return Ok(size > 0);
        }
    }
    Ok(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn header(storage: u8, bank_size: u32) -> Vec<u8> {
        let mut bytes = vec![0; 64];
        bytes[10..12].copy_from_slice(&32u16.to_be_bytes()); // rows at 40
        bytes[12..16].copy_from_slice(&40u32.to_be_bytes()); // strings at 48
        bytes[16..20].copy_from_slice(&56u32.to_be_bytes()); // data at 64
        bytes[24..26].copy_from_slice(&1u16.to_be_bytes());
        bytes[26..28].copy_from_slice(&8u16.to_be_bytes());
        bytes[28..32].copy_from_slice(&1u32.to_be_bytes());
        bytes[32] = storage | 0x1b;
        let value_at = if storage == 0x20 { 37 } else { 40 };
        if storage == 0x20 {
            bytes[10..12].copy_from_slice(&37u16.to_be_bytes());
            bytes[26..28].copy_from_slice(&0u16.to_be_bytes());
        }
        bytes[value_at + 4..value_at + 8].copy_from_slice(&bank_size.to_be_bytes());
        bytes[48..56].copy_from_slice(b"AwbFile\0");
        bytes
    }

    #[test]
    fn distinguishes_memory_and_external_banks() {
        assert!(memory_bank_in_header(&header(0x40, 100), 164).unwrap());
        assert!(!memory_bank_in_header(&header(0x40, 0), 64).unwrap());
    }

    #[test]
    fn accepts_constants_and_rejects_out_of_bounds_bank() {
        assert!(memory_bank_in_header(&header(0x20, 100), 164).unwrap());
        assert!(memory_bank_in_header(&header(0x40, 100), 80).is_err());
        assert!(memory_bank_in_header(&[0; 10], 10).is_err());
    }
}
