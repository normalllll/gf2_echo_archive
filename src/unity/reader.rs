use anyhow::{Context, Result, ensure};

/// Bounds-checked cursor over a byte slice with switchable endianness.
pub struct Reader<'a> {
    pub data: &'a [u8],
    pub position: usize,
    pub big_endian: bool,
}

macro_rules! read_number {
    ($name:ident, $type:ty) => {
        pub fn $name(&mut self) -> Result<$type> {
            let bytes = self
                .bytes(size_of::<$type>())?
                .try_into()
                .expect("exact size");
            Ok(if self.big_endian {
                <$type>::from_be_bytes(bytes)
            } else {
                <$type>::from_le_bytes(bytes)
            })
        }
    };
}

impl<'a> Reader<'a> {
    pub fn new(data: &'a [u8], big_endian: bool) -> Self {
        Self {
            data,
            position: 0,
            big_endian,
        }
    }

    pub fn bytes(&mut self, length: usize) -> Result<&'a [u8]> {
        let end = self
            .position
            .checked_add(length)
            .filter(|end| *end <= self.data.len())
            .with_context(|| {
                tr!(
                    "数据被截断：位置 {}，需要 {length} 字节",
                    "Truncated data: at {}, needs {length} bytes",
                    self.position,
                    length = length
                )
            })?;
        let result = &self.data[self.position..end];
        self.position = end;
        Ok(result)
    }

    pub fn skip(&mut self, length: usize) -> Result<()> {
        self.bytes(length).map(|_| ())
    }

    pub fn u8(&mut self) -> Result<u8> {
        Ok(self.bytes(1)?[0])
    }

    read_number!(u16, u16);
    read_number!(i16, i16);
    read_number!(u32, u32);
    read_number!(i32, i32);
    read_number!(u64, u64);
    read_number!(i64, i64);
    read_number!(f32, f32);
    read_number!(f64, f64);

    pub fn cstring(&mut self) -> Result<String> {
        let rest = &self.data[self.position..];
        let end = rest
            .iter()
            .position(|b| *b == 0)
            .context(tr!("字符串缺少结束符", "The string has no terminator"))?;
        let value = String::from_utf8_lossy(&rest[..end]).into_owned();
        self.position += end + 1;
        Ok(value)
    }

    pub fn align(&mut self, alignment: usize) -> Result<()> {
        let padding = (alignment - self.position % alignment) % alignment;
        self.skip(padding)
    }

    pub fn count(&mut self) -> Result<usize> {
        let value = self.i32()?;
        ensure!(
            value >= 0,
            tr!(
                "数量为负数：{value}",
                "Negative count: {value}",
                value = value
            )
        );
        Ok(value as usize)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_both_endians_and_aligns() {
        let data = [0, 0, 0, 7, 7, 0, 0, 0, b'a', 0, 9, 9, 1];
        let mut reader = Reader::new(&data, true);
        assert_eq!(reader.u32().unwrap(), 7);
        reader.big_endian = false;
        assert_eq!(reader.u32().unwrap(), 7);
        assert_eq!(reader.cstring().unwrap(), "a");
        reader.align(4).unwrap();
        assert_eq!(reader.position, 12);
        assert!(reader.u16().is_err());
    }
}
