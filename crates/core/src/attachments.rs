//! Bounded metadata and pure display/security helpers for message attachments.
use crate::MediaFile;
use std::path::PathBuf;
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Attachment {
    Voice(Voice),
    Document(Document),
    Sticker(Sticker),
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Voice {
    pub file: MediaFile,
    pub duration: u32,
    /// Unpacked samples in 0..=31; at most 409 (256 packed bytes).
    pub waveform: Vec<u8>,
    pub listened: bool,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Document {
    pub file: MediaFile,
    /// At most 256 UTF-8 bytes; display only, never used to construct a local path.
    pub name: String,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sticker {
    /// At most 64 UTF-8 bytes; animated stickers without a static thumbnail use this fallback.
    pub emoji: String,
    pub width: u16,
    pub height: u16,
    pub animated: bool,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileRequest {
    Open(PathBuf),
    ShowInFolder(PathBuf),
}
/// The Telegram waveform is packed low bit first, with five bits per sample.
pub fn decode_waveform(packed: &[u8]) -> Result<Vec<u8>, &'static str> {
    if packed.len() > 256 {
        return Err("Waveform exceeds 256 bytes.");
    }
    Ok((0..packed.len() * 8 / 5)
        .map(|sample| {
            let bit = sample * 5;
            let byte = bit / 8;
            let shift = bit % 8;
            let pair = u16::from(packed[byte])
                | (u16::from(packed.get(byte + 1).copied().unwrap_or(0)) << 8);
            ((pair >> shift) & 31) as u8
        })
        .collect())
}
/// Fixed display budget independent of the source length; missing samples draw flat bars.
pub fn waveform_bars(samples: &[u8]) -> [u8; 48] {
    let mut bars = [4; 48];
    if samples.is_empty() {
        return bars;
    }
    let samples = &samples[..samples.len().min(409)];
    for (i, bar) in bars.iter_mut().enumerate() {
        let start = i * samples.len() / 48;
        let end = ((i + 1) * samples.len() / 48)
            .max(start + 1)
            .min(samples.len());
        *bar = samples[start..end]
            .iter()
            .copied()
            .max()
            .unwrap_or(4)
            .min(31);
    }
    bars
}
/// Windows ignores trailing spaces/dots. Inspect the final extension, including double names.
pub fn is_executable_name(name: &str) -> bool {
    let name = name.trim_end_matches([' ', '.']);
    let extension = name.rsplit_once('.').map_or("", |(_, e)| e);
    [
        "exe",
        "msi",
        "bat",
        "cmd",
        "com",
        "scr",
        "pif",
        "ps1",
        "vbs",
        "js",
        "jse",
        "wsf",
        "hta",
        "lnk",
        "reg",
        "jar",
        "cpl",
        "vbe",
        "ws",
        "wsh",
        "psm1",
        "psd1",
        "msp",
        "mst",
        "application",
        "gadget",
        "chm",
        "scf",
        "url",
    ]
    .iter()
    .any(|candidate| extension.eq_ignore_ascii_case(candidate))
}
