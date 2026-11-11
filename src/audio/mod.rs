//! Reads CRI audio packs and decodes them to WAV in-process.
//!
//! Self-contained on purpose: the comparison tool includes this file by path,
//! so what it compares against the reference decoder is the shipped code.

mod cue_names;

use anyhow::{Context, Result, bail, ensure};
use cridecoder::{
    HcaDecoder,
    acb::{AfsArchive, UtfTable, Value},
    hca::{HcaDecoderError, HcaInfo, KeyTest},
};
use std::{
    collections::HashMap,
    fs::{self, File},
    io::{Cursor, Read, Seek, SeekFrom},
    path::{Path, PathBuf},
};

/// The two keys GF2 uses for HCA.
const PC_KEY: u64 = 7152097263845921;
const ANDROID_KEY: u64 = 8930254087621254;
const WAV_HEADER_BYTES: usize = 44;

pub struct PackStream {
    /// 1-based position in the pack.
    pub index: usize,
    /// All cue names of this stream, joined with `; `.
    pub cue_name: Option<String>,
}

pub struct DecodedStream {
    pub wav: Vec<u8>,
    pub sample_rate: u32,
    pub channels: u32,
    pub samples: u64,
    /// The key used, for an encrypted stream. Only the comparison tool reads it.
    #[allow(dead_code)]
    pub key: Option<u64>,
    pub key_confirmed: bool,
    /// Frames that could not be decoded and are silent in the output.
    pub failed_frames: usize,
}

pub struct Pack {
    pub streams: Vec<PackStream>,
    source: Source,
    entries: Vec<Entry>,
    subkey: u64,
}

#[derive(Clone, Copy)]
struct Entry {
    offset: u64,
    size: u64,
}

enum Source {
    File(PathBuf),
    Memory(Vec<u8>),
}

impl Source {
    fn len(&self) -> Result<u64> {
        Ok(match self {
            Source::File(path) => path.metadata()?.len(),
            Source::Memory(bytes) => bytes.len() as u64,
        })
    }

    fn read(&self, offset: u64, size: u64) -> Result<Vec<u8>> {
        ensure!(
            offset
                .checked_add(size)
                .is_some_and(|end| end <= self.len().unwrap_or(0)),
            tr!(
                "音轨数据超出音频包范围",
                "Track data is outside the audio pack"
            )
        );
        match self {
            Source::File(path) => {
                let mut file = File::open(path)?;
                file.seek(SeekFrom::Start(offset))?;
                let mut bytes = vec![0; size as usize];
                file.read_exact(&mut bytes)?;
                Ok(bytes)
            }
            Source::Memory(bytes) => Ok(bytes[offset as usize..(offset + size) as usize].to_vec()),
        }
    }
}

/// Reads a pack's stream list and cue names; the kind comes from the extension.
pub fn read_pack(path: &Path) -> Result<Pack> {
    let extension = path
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    match extension.as_str() {
        "awb" => {
            let names = companion_acb(path)
                .and_then(|acb| streaming_names(&acb))
                .unwrap_or_default();
            read_bank(Source::File(path.to_owned()), &names)
        }
        "acb" => read_memory_bank(path),
        "hca" => read_standalone_hca(path),
        _ => bail!(tr!(
            "不支持的音频包类型：{}",
            "Unsupported audio pack type: {}",
            path.display()
        )),
    }
}

/// The sidecar names tried for an AWB, in order.
fn companion_candidates(path: &Path) -> Vec<PathBuf> {
    let mut candidates = vec![path.with_extension("acb")];
    let stem = path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or_default();
    for suffix in ["_streamfiles", "_STR"] {
        if let Some(base) = stem.strip_suffix(suffix)
            && !base.is_empty()
        {
            candidates.push(path.with_file_name(format!("{base}.acb")));
        }
    }
    candidates
}

fn companion_acb(path: &Path) -> Option<PathBuf> {
    companion_candidates(path)
        .into_iter()
        .find(|candidate| candidate.is_file())
}

fn streaming_names(acb: &Path) -> Option<HashMap<u16, String>> {
    let header = UtfTable::new(Cursor::new(fs::read(acb).ok()?)).ok()?;
    bank_names(&header, false)
}

fn bank_names(header: &UtfTable, is_memory: bool) -> Option<HashMap<u16, String>> {
    cue_names::walk(&cue_names::read_tables(header, is_memory), is_memory, 0)
}

fn read_memory_bank(path: &Path) -> Result<Pack> {
    let bytes = fs::read(path)?;
    let mut header = UtfTable::new(Cursor::new(bytes))
        .context(tr!("无法读取 ACB 表头", "Cannot read the ACB header"))?;
    ensure!(
        header.rows.len() == 1 && header.name == "Header",
        tr!("不是有效的 ACB 表头", "Not a valid ACB header")
    );
    let names = bank_names(&header, true).unwrap_or_default();
    let bank = match header.rows[0].remove("AwbFile") {
        Some(Value::Data(bank)) if !bank.is_empty() => bank,
        _ => bail!(tr!("ACB 没有内嵌音频", "The ACB has no embedded audio")),
    };
    read_bank(Source::Memory(bank), &names)
}

fn read_bank(source: Source, names: &HashMap<u16, String>) -> Result<Pack> {
    let head = source
        .read(0, 16)
        .context(tr!("AWB 文件过短", "The AWB file is too short"))?;
    ensure!(
        &head[..4] == b"AFS2",
        tr!("不是有效的 AWB 音频包", "Not a valid AWB audio pack")
    );
    let count = u32::from_le_bytes([head[8], head[9], head[10], head[11]]);
    // Guard the table allocations against a damaged header.
    ensure!(
        count > 0 && u64::from(count) * 2 <= source.len()? && (head[12] | head[13]) != 0,
        tr!("AWB 表头无效", "Invalid AWB header")
    );
    let (subkey, files) = match &source {
        Source::File(path) => {
            let archive = AfsArchive::new(File::open(path)?)
                .context(tr!("无法读取 AWB", "Cannot read the AWB"))?;
            (archive.subkey, archive.files)
        }
        Source::Memory(bytes) => {
            let archive = AfsArchive::new(Cursor::new(bytes.as_slice()))
                .context(tr!("无法读取 AWB", "Cannot read the AWB"))?;
            (archive.subkey, archive.files)
        }
    };
    let mut entries = Vec::with_capacity(files.len());
    let mut streams = Vec::with_capacity(files.len());
    for (position, file) in files.iter().enumerate() {
        let entry = Entry {
            offset: u64::from(file.offset),
            size: u64::from(file.size),
        };
        ensure!(
            entry.size >= 4 && is_hca(&source.read(entry.offset, 4)?),
            tr!(
                "音轨 {} 不是 HCA，暂不支持",
                "Track {} is not HCA and is not supported yet",
                position + 1
            )
        );
        entries.push(entry);
        streams.push(PackStream {
            index: position + 1,
            cue_name: names.get(&(file.cue_id as u16)).cloned(),
        });
    }
    Ok(Pack {
        streams,
        source,
        entries,
        subkey: u64::from(subkey),
    })
}

fn read_standalone_hca(path: &Path) -> Result<Pack> {
    let source = Source::File(path.to_owned());
    let size = source.len()?;
    ensure!(
        size >= 4 && is_hca(&source.read(0, 4)?),
        tr!("不是有效的 HCA 文件", "Not a valid HCA file")
    );
    Ok(Pack {
        streams: vec![PackStream {
            index: 1,
            cue_name: None,
        }],
        source,
        entries: vec![Entry { offset: 0, size }],
        subkey: 0,
    })
}

/// Encrypted files mask the signature's high bits.
fn is_hca(magic: &[u8]) -> bool {
    magic.len() >= 4
        && magic[0] & 0x7F == b'H'
        && magic[1] & 0x7F == b'C'
        && magic[2] & 0x7F == b'A'
        && magic[3] & 0x7F == 0
}

impl Pack {
    /// Decodes one stream to a complete WAV file. `position` is 0-based.
    pub fn decode(&self, position: usize) -> Result<DecodedStream> {
        let entry = *self
            .entries
            .get(position)
            .with_context(|| tr!("音轨 {} 不存在", "Track {} does not exist", position + 1))?;
        let bytes = self.source.read(entry.offset, entry.size)?;
        let describe = |error: HcaDecoderError| {
            anyhow::anyhow!(tr!(
                "音轨 {} 解码失败：{error}",
                "Track {} failed to decode: {error}",
                position + 1,
                error = error
            ))
        };
        let mut decoder =
            HcaDecoder::from_reader(Cursor::new(bytes.as_slice())).map_err(describe)?;
        let (key, key_confirmed) = if decoder.info().encryption_enabled {
            let (key, confirmed) = find_key(&mut decoder, self.subkey);
            decoder.set_encryption_key(key, self.subkey);
            (Some(key), confirmed)
        } else {
            (None, true)
        };
        let info = decoder.info().clone();
        let channels = info.channel_count;
        let frames = complete_frames(&info, entry.size).min(u64::from(info.block_count));
        let sample_frames = sample_count(&info, entry.size);
        let (samples, failed_frames) =
            decode_frames(&bytes, &info, key, self.subkey, frames, sample_frames)
                .map_err(describe)?;
        Ok(DecodedStream {
            samples: sample_frames,
            wav: wav_file(&samples, info.sampling_rate, channels),
            sample_rate: info.sampling_rate,
            channels,
            key,
            key_confirmed,
            failed_frames,
        })
    }
}

/// Whole frames actually stored in an entry of `size` bytes.
fn complete_frames(info: &HcaInfo, size: u64) -> u64 {
    size.saturating_sub(u64::from(info.header_size))
        .checked_div(u64::from(info.block_size))
        .unwrap_or(0)
}

/// Sample frames of an entry. An entry shorter than its header claims (the
/// prefetch part of a memory bank) counts its blocks over the whole entry,
/// header bytes included, so this can exceed what the complete frames produce.
fn sample_count(info: &HcaInfo, size: u64) -> u64 {
    let block_size = u64::from(info.block_size);
    let blocks = if u64::from(info.block_count) * block_size > size {
        size.checked_div(block_size).unwrap_or(0)
    } else {
        u64::from(info.block_count)
    };
    (blocks * info.samples_per_block as u64)
        .saturating_sub(u64::from(info.encoder_delay))
        .saturating_sub(u64::from(info.encoder_padding))
}

/// Decodes `frames` frames into exactly `sample_frames` sample frames, and
/// returns them with the number of frames that failed. Like the reference
/// decoder, a frame that can't be decoded becomes silence and decoding goes on.
fn decode_frames(
    bytes: &[u8],
    info: &HcaInfo,
    key: Option<u64>,
    subkey: u64,
    frames: u64,
    sample_frames: u64,
) -> Result<(Vec<f32>, usize), HcaDecoderError> {
    // The decoder's read-ahead needs the block count its header claims. The
    // padding of a truncated entry is never decoded.
    let claimed =
        u64::from(info.header_size) + u64::from(info.block_count) * u64::from(info.block_size);
    let mut whole = bytes.to_vec();
    if (whole.len() as u64) < claimed {
        whole.resize(claimed as usize, 0);
    }
    let mut decoder = HcaDecoder::from_reader(Cursor::new(whole))?;
    if let Some(key) = key {
        decoder.set_encryption_key(key, subkey);
    }
    let channels = info.channel_count as usize;
    let samples_per_block = info.samples_per_block as u64;
    let delay = u64::from(info.encoder_delay);
    let mut samples = Vec::with_capacity(sample_frames as usize * channels);
    let mut failed = 0;
    for frame in 0..frames {
        match decoder.decode_frame() {
            Ok((output, count)) => samples.extend_from_slice(&output[..count * channels]),
            Err(_) => {
                failed += 1;
                let end = ((frame + 1) * samples_per_block)
                    .saturating_sub(delay)
                    .min(sample_frames) as usize
                    * channels;
                samples.resize(end.max(samples.len()), 0.0);
            }
        }
    }
    samples.resize(sample_frames as usize * channels, 0.0);
    Ok((samples, failed))
}

fn find_key<R: Read + Seek>(decoder: &mut HcaDecoder<R>, subkey: u64) -> (u64, bool) {
    // A fresh test per key: a shared one carries the first key's start offset
    // into the second key's score, and lets a later key win a tie at zero.
    let mut score = |key| {
        let mut test = KeyTest {
            key,
            subkey,
            ..KeyTest::default()
        };
        decoder.test_key(&mut test);
        test.best_score
    };
    let pc = score(PC_KEY);
    let android = score(ANDROID_KEY);
    choose_key(pc, android)
}

/// PC wins unless Android has a positive score that beats it (lower is better).
fn choose_key(pc: i32, android: i32) -> (u64, bool) {
    if android > 0 && (pc <= 0 || android < pc) {
        (ANDROID_KEY, true)
    } else {
        (PC_KEY, pc > 0)
    }
}

/// `x * 32767` truncated like C's `cvttss2si` (NaN and out-of-range values give
/// `i32::MIN`), then clamped.
fn sample_to_i16(sample: f32) -> i16 {
    let scaled = sample * 32767.0;
    let value = if scaled.is_nan() || !(-2_147_483_648.0..2_147_483_648.0).contains(&scaled) {
        i32::MIN
    } else {
        scaled as i32
    };
    value.clamp(i32::from(i16::MIN), i32::from(i16::MAX)) as i16
}

fn wav_file(samples: &[f32], sample_rate: u32, channels: u32) -> Vec<u8> {
    let data_bytes = samples.len() * 2;
    let mut wav = Vec::with_capacity(WAV_HEADER_BYTES + data_bytes);
    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&((36 + data_bytes) as u32).to_le_bytes());
    wav.extend_from_slice(b"WAVEfmt ");
    wav.extend_from_slice(&16u32.to_le_bytes());
    wav.extend_from_slice(&1u16.to_le_bytes());
    wav.extend_from_slice(&(channels as u16).to_le_bytes());
    wav.extend_from_slice(&sample_rate.to_le_bytes());
    wav.extend_from_slice(&(sample_rate * channels * 2).to_le_bytes());
    wav.extend_from_slice(&((channels * 2) as u16).to_le_bytes());
    wav.extend_from_slice(&16u16.to_le_bytes());
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&(data_bytes as u32).to_le_bytes());
    for sample in samples {
        wav.extend_from_slice(&sample_to_i16(*sample).to_le_bytes());
    }
    wav
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pcm_truncates_toward_zero() {
        // 0.5 * 32767 = 16383.5
        assert_eq!(sample_to_i16(0.5), 16383);
        assert_eq!(sample_to_i16(-0.5), -16383);
        assert_eq!(sample_to_i16(0.0), 0);
        assert_eq!(sample_to_i16(1.0), 32767);
        assert_eq!(sample_to_i16(-1.0), -32767);
    }

    #[test]
    fn pcm_clamps_values_outside_the_i16_range() {
        assert_eq!(sample_to_i16(1.5), 32767);
        assert_eq!(sample_to_i16(-1.5), -32768);
    }

    #[test]
    fn pcm_maps_nan_and_values_beyond_i32_to_the_minimum() {
        assert_eq!(sample_to_i16(f32::NAN), i16::MIN);
        assert_eq!(sample_to_i16(f32::INFINITY), i16::MIN);
        assert_eq!(sample_to_i16(f32::NEG_INFINITY), i16::MIN);
        assert_eq!(sample_to_i16(1.0e6), i16::MIN);
        assert_eq!(sample_to_i16(-1.0e6), i16::MIN);
    }

    #[test]
    fn wav_header_is_the_standard_44_bytes() {
        let wav = wav_file(&[0.0, 1.0, -1.0, 0.5], 48000, 2);
        let u32_at = |at: usize| u32::from_le_bytes(wav[at..at + 4].try_into().unwrap());
        let u16_at = |at: usize| u16::from_le_bytes(wav[at..at + 2].try_into().unwrap());
        assert_eq!(wav.len(), 44 + 8);
        assert_eq!(&wav[0..4], b"RIFF");
        assert_eq!(u32_at(4), 36 + 8);
        assert_eq!(&wav[8..16], b"WAVEfmt ");
        assert_eq!(u32_at(16), 16);
        assert_eq!(u16_at(20), 1);
        assert_eq!(u16_at(22), 2);
        assert_eq!(u32_at(24), 48000);
        assert_eq!(u32_at(28), 192000);
        assert_eq!(u16_at(32), 4);
        assert_eq!(u16_at(34), 16);
        assert_eq!(&wav[36..40], b"data");
        assert_eq!(u32_at(40), 8);
        assert_eq!(u16_at(46), 32767);
        assert_eq!(u16_at(48), (-32767i16) as u16);
    }

    fn stub_info(block_count: u32, delay: u32, padding: u32) -> HcaInfo {
        HcaInfo {
            version: 0x0200,
            header_size: 96,
            sampling_rate: 48000,
            channel_count: 1,
            block_size: 341,
            block_count,
            encoder_delay: delay,
            encoder_padding: padding,
            loop_enabled: false,
            loop_start_block: 0,
            loop_end_block: 0,
            loop_start_delay: 0,
            loop_end_padding: 0,
            samples_per_block: 1024,
            comment: String::new(),
            encryption_enabled: true,
        }
    }

    #[test]
    fn truncated_entries_count_blocks_including_the_header_bytes() {
        let info = stub_info(328, 128, 870);
        assert_eq!(complete_frames(&info, 1824), 5);
        assert_eq!(sample_count(&info, 1824), 5 * 1024 - 128 - 870);
        // 2,100 bytes hold 5 complete frames but count as 6 blocks.
        assert_eq!(complete_frames(&info, 2100), 5);
        assert_eq!(sample_count(&info, 2100), 6 * 1024 - 128 - 870);
    }

    #[test]
    fn sample_count_never_underflows() {
        assert_eq!(sample_count(&stub_info(328, 9000, 0), 1824), 0);
    }

    #[test]
    fn an_entry_missing_only_header_bytes_keeps_its_claimed_length() {
        // block_count * block_size fits in the entry, so the claimed count stands.
        let info = stub_info(5, 128, 0);
        assert_eq!(complete_frames(&info, 1750), 4);
        assert_eq!(sample_count(&info, 1750), 5 * 1024 - 128);
    }

    fn encoded_tone(frames: usize) -> (Vec<u8>, HcaInfo) {
        let samples: Vec<f32> = (0..frames * 1024)
            .map(|index| (index as f32 * 0.03).sin() * 0.4)
            .collect();
        let mut encoder =
            cridecoder::HcaEncoder::new(cridecoder::HcaEncoderConfig::new(48000, 1)).unwrap();
        let mut output = Cursor::new(Vec::new());
        encoder.encode(&samples, &mut output).unwrap();
        let bytes = output.into_inner();
        let info = HcaDecoder::from_reader(Cursor::new(bytes.clone()))
            .unwrap()
            .info()
            .clone();
        (bytes, info)
    }

    fn decode_all_frames(bytes: &[u8], info: &HcaInfo) -> (Vec<f32>, usize) {
        let size = bytes.len() as u64;
        let frames = complete_frames(info, size).min(u64::from(info.block_count));
        decode_frames(bytes, info, None, 0, frames, sample_count(info, size)).unwrap()
    }

    #[test]
    fn a_bad_frame_is_silent_and_the_rest_still_decodes() {
        let (mut bytes, info) = encoded_tone(40);
        let (clean, failed) = decode_all_frames(&bytes, &info);
        assert_eq!(failed, 0);
        // Breaking the sync word makes this one frame undecodable.
        let frame = 20usize;
        let offset = info.header_size as usize + frame * info.block_size as usize;
        bytes[offset] = 0;
        bytes[offset + 1] = 0;
        let (damaged, failed) = decode_all_frames(&bytes, &info);
        assert_eq!(failed, 1);
        assert_eq!(damaged.len(), clean.len());
        let start = frame * 1024 - info.encoder_delay as usize;
        assert!(
            damaged[start..start + 1024]
                .iter()
                .all(|sample| *sample == 0.0)
        );
        assert!(damaged[start + 2048..].iter().any(|sample| *sample != 0.0));
    }

    #[test]
    fn scrambled_frames_never_change_the_length() {
        let (original, info) = encoded_tone(30);
        let expected = sample_count(&info, original.len() as u64) as usize;
        let mut state = 0x1234_5678u32;
        for round in 0..64 {
            let mut bytes = original.clone();
            for byte in &mut bytes[info.header_size as usize..] {
                state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                if round % 2 == 0 || state >> 28 == 0 {
                    *byte = (state >> 16) as u8;
                }
            }
            let (samples, _) = decode_all_frames(&bytes, &info);
            assert_eq!(samples.len(), expected);
        }
    }

    #[test]
    fn pc_key_wins_unless_android_is_really_better() {
        assert_eq!(choose_key(1, 0), (PC_KEY, true));
        assert_eq!(choose_key(30, 0), (PC_KEY, true));
        assert_eq!(choose_key(10, 10), (PC_KEY, true));
        assert_eq!(choose_key(0, 1), (ANDROID_KEY, true));
        assert_eq!(choose_key(30, 10), (ANDROID_KEY, true));
        assert_eq!(choose_key(-1, 40), (ANDROID_KEY, true));
    }

    #[test]
    fn a_tie_at_zero_uses_pc_and_stays_unconfirmed() {
        assert_eq!(choose_key(0, 0), (PC_KEY, false));
        assert_eq!(choose_key(-1, 0), (PC_KEY, false));
        assert_eq!(choose_key(-1, -1), (PC_KEY, false));
    }

    #[test]
    fn hca_signature_may_be_masked() {
        assert!(is_hca(b"HCA\0"));
        assert!(is_hca(&[b'H' | 0x80, b'C' | 0x80, b'A' | 0x80, 0x80]));
        assert!(!is_hca(b"RIFF"));
        assert!(!is_hca(b"HC"));
    }

    #[test]
    fn companion_names_follow_the_reference_order() {
        let names = |path: &str| -> Vec<String> {
            companion_candidates(Path::new(path))
                .iter()
                .map(|candidate| candidate.to_string_lossy().replace('\\', "/"))
                .collect()
        };
        assert_eq!(names("a/bank.awb"), ["a/bank.acb"]);
        assert_eq!(
            names("a/bank_streamfiles.awb"),
            ["a/bank_streamfiles.acb", "a/bank.acb"]
        );
        assert_eq!(names("a/bank_STR.awb"), ["a/bank_STR.acb", "a/bank.acb"]);
        assert_eq!(names("a/_STR.awb"), ["a/_STR.acb"]);
    }
}
