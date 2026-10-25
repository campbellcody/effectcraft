//! Uncompressed QuickTime video (`raw `): 8-bit RGB ("Millions of Colors", depth 24) or ARGB
//! ("Millions of Colors+", depth 32), one frame per sample, muxed by FilmCraft's QuickTime writer
//! next to PCM audio like ProRes.
//!
//! Layout from the QuickTime File Format specification (Apple, "Video Sample Description" and
//! "Uncompressed RGB" formats): pixels are stored top to bottom, components big-endian in
//! A R G B / R G B order, and each row is padded to an even number of bytes (only depth 24 at odd
//! widths needs it).
//!
//! The 16-bpc variant is `b64a`: full-range, big-endian 16-bit A R G B samples, per
//! Apple Technical Q&A QA1114 (2002-02-07). It always stores four components; RGB and alpha-only
//! output have opaque alpha. See this crate's README for specification references.

use effectcraft_project::render_queue::{AlphaMode, Channels};
use effectcraft_raster::Image;
use filmcraft_isobmff::{CodecConfig, FourCc, SampleEntry, VideoParams};

use crate::encode::{Packet, VideoEncoder};
use crate::{ExportError, Result};

pub(crate) struct Raw {
    w: u32,
    h: u32,
    alpha: bool,
    sixteen: bool,
}

// Allows an 8K ARGB16 frame, while bounding allocations even for hostile frame dimensions.
const MAX_FRAME_BYTES: usize = 256 * 1024 * 1024;

fn frame_buffer(size: usize) -> Result<Vec<u8>> {
    if size > MAX_FRAME_BYTES {
        return Err(ExportError::Unsupported("uncompressed frames are limited to 256 MiB; reduce the output dimensions".into()));
    }
    let mut data = Vec::new();
    data.try_reserve_exact(size).map_err(|e| ExportError::Encode(format!("cannot allocate uncompressed frame ({size} bytes): {e}")))?;
    data.resize(size, 0);
    Ok(data)
}

/// Convert the rendered, premultiplied float image directly to big-endian RGBA16, without
/// passing through RGBA8. Color conversion has already happened in the renderer.
pub(crate) fn pixels16(img: &Image, background: [f32; 3], channels: Channels, mode: AlphaMode) -> Result<Vec<u8>> {
    let pixels = (img.width as usize).checked_mul(img.height as usize).ok_or_else(|| ExportError::Encode("frame too large".into()))?;
    let size = pixels.checked_mul(8).ok_or_else(|| ExportError::Encode("frame too large".into()))?;
    if img.data.len() != pixels || (channels == Channels::Rgb && !background.iter().all(|c| c.is_finite())) {
        return Err(ExportError::Encode("invalid uncompressed 16-bpc frame or background".into()));
    }
    let mut data = frame_buffer(size)?;
    for (p, out) in img.data.iter().zip(data.as_chunks_mut::<8>().0) {
        if !p.iter().all(|c| c.is_finite()) {
            return Err(ExportError::Encode("uncompressed 16-bpc frame contains non-finite color values".into()));
        }
        let [r, g, b, a] = *p;
        let alpha = a.clamp(0.0, 1.0);
        let rgba = match channels {
            Channels::Rgb => {
                let [br, bg, bb] = background;
                let k = 1.0 - alpha;
                [r + br * k, g + bg * k, b + bb * k, 1.0]
            }
            Channels::Rgba if mode == AlphaMode::Straight => {
                if a > 0.0 {
                    [r / a, g / a, b / a, alpha]
                } else {
                    [0.0; 4]
                }
            }
            Channels::Rgba => [r, g, b, alpha],
            Channels::Alpha => [alpha, alpha, alpha, 1.0],
        };
        for (c, bytes) in rgba.into_iter().zip(out.as_chunks_mut::<2>().0) {
            // Clamping precedes the integer cast, including any overflow from unpremultiplying.
            let v = (c.clamp(0.0, 1.0) * 65535.0 + 0.5) as u16;
            *bytes = v.to_be_bytes();
        }
    }
    Ok(data)
}

impl Raw {
    /// `alpha`: write ARGB (depth 32); otherwise RGB (depth 24).
    pub(crate) fn new(w: u32, h: u32, alpha: bool) -> Result<Raw> {
        if w == 0 || h == 0 || w > u16::MAX as u32 || h > u16::MAX as u32 {
            return Err(ExportError::Unsupported(format!("uncompressed QuickTime frames must be 1–65535 px, not {w}×{h}")));
        }
        let raw = Raw { w, h, alpha, sixteen: false };
        raw.frame_bytes()?;
        Ok(raw)
    }

    pub(crate) fn new16(w: u32, h: u32) -> Result<Raw> {
        let mut raw = Self::new(w, h, true)?;
        raw.sixteen = true;
        raw.frame_bytes()?;
        Ok(raw)
    }

    fn depth(&self) -> u16 {
        if self.sixteen {
            64
        } else if self.alpha {
            32
        } else {
            24
        }
    }

    /// Bytes per stored row (even).
    fn row_bytes(&self) -> usize {
        let n = self.w as usize
            * if self.sixteen {
                8
            } else if self.alpha {
                4
            } else {
                3
            };
        n + (n & 1)
    }

    fn frame_bytes(&self) -> Result<usize> {
        let size = self.row_bytes().checked_mul(self.h as usize).filter(|n| *n <= MAX_FRAME_BYTES);
        size.ok_or_else(|| ExportError::Unsupported("uncompressed frames are limited to 256 MiB; reduce the output dimensions".into()))
    }

    /// The `raw ` video sample description payload (after the 8-byte box header).
    fn description(&self) -> Vec<u8> {
        let mut b = Vec::with_capacity(78);
        b.extend_from_slice(&[0; 6]); // reserved
        b.extend_from_slice(&1u16.to_be_bytes()); // data reference index
        b.extend_from_slice(&[0; 4]); // version, revision level
        b.extend_from_slice(&[0; 4]); // vendor
        b.extend_from_slice(&0u32.to_be_bytes()); // temporal quality
        b.extend_from_slice(&1024u32.to_be_bytes()); // spatial quality: codecLosslessQuality
        b.extend_from_slice(&(self.w as u16).to_be_bytes());
        b.extend_from_slice(&(self.h as u16).to_be_bytes());
        b.extend_from_slice(&0x0048_0000u32.to_be_bytes()); // 72 dpi
        b.extend_from_slice(&0x0048_0000u32.to_be_bytes());
        b.extend_from_slice(&0u32.to_be_bytes()); // data size
        b.extend_from_slice(&1u16.to_be_bytes()); // frames per sample
        let name = b"None";
        let mut nb = [0u8; 32];
        nb[0] = name.len() as u8;
        nb[1..=name.len()].copy_from_slice(name);
        b.extend_from_slice(&nb);
        b.extend_from_slice(&self.depth().to_be_bytes());
        b.extend_from_slice(&(-1i16).to_be_bytes()); // no colour table
        b
    }
}

impl VideoEncoder for Raw {
    fn sample_entry(&self) -> SampleEntry {
        let fourcc = FourCc(if self.sixteen { *b"b64a" } else { *b"raw " });
        SampleEntry {
            format: fourcc,
            data_reference_index: 1,
            // Written verbatim; `video` makes the track a video track with the right size.
            codec: CodecConfig::Unknown { fourcc, raw: self.description() },
            video: Some(VideoParams { width: self.w as u16, height: self.h as u16, depth: self.depth(), ..Default::default() }),
            audio: None,
            bitrate: None,
        }
    }

    fn encode(&mut self, rgba: &[u8], _index: u64) -> Result<Vec<Packet>> {
        let (w, h) = (self.w as usize, self.h as usize);
        let row = self.row_bytes();
        let size = self.frame_bytes()?;
        let input_bpp = if self.sixteen { 8 } else { 4 };
        let expected = w.checked_mul(h).and_then(|n| n.checked_mul(input_bpp)).ok_or_else(|| ExportError::Encode("frame too large".into()))?;
        if rgba.len() != expected {
            return Err(ExportError::Encode(format!("uncompressed frame needs {expected} bytes, received {}", rgba.len())));
        }
        let mut data = frame_buffer(size)?;
        if self.sixteen {
            for (p, o) in rgba.as_chunks::<8>().0.iter().zip(data.as_chunks_mut::<8>().0) {
                *o = [p[6], p[7], p[0], p[1], p[2], p[3], p[4], p[5]];
            }
            return Ok(vec![Packet { data, key: true, cto: 0 }]);
        }
        for (y, out) in data.chunks_exact_mut(row).enumerate() {
            let src = rgba.get(y * w * 4..(y + 1) * w * 4).ok_or_else(|| ExportError::Encode("short frame".into()))?;
            for (p, o) in src.as_chunks::<4>().0.iter().zip(out.chunks_exact_mut(if self.alpha { 4 } else { 3 })) {
                if self.alpha {
                    o.copy_from_slice(&[p[3], p[0], p[1], p[2]]);
                } else {
                    o.copy_from_slice(&p[..3]);
                }
            }
        }
        Ok(vec![Packet { data, key: true, cto: 0 }])
    }

    fn flush(&mut self) -> Result<Vec<Packet>> {
        Ok(vec![])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rgb_rows_are_padded_to_even_bytes() {
        let mut r = Raw::new(3, 2, false).unwrap();
        let rgba: Vec<u8> = (0..24).collect();
        let p = r.encode(&rgba, 0).unwrap();
        assert_eq!(p[0].data, [0, 1, 2, 4, 5, 6, 8, 9, 10, 0, 12, 13, 14, 16, 17, 18, 20, 21, 22, 0]);
        assert_eq!(r.sample_entry().video.unwrap().depth, 24);
    }

    #[test]
    fn argb_moves_alpha_first() {
        let mut r = Raw::new(1, 1, true).unwrap();
        assert_eq!(r.encode(&[10, 20, 30, 40], 0).unwrap()[0].data, [40, 10, 20, 30]);
        let e = r.sample_entry();
        let CodecConfig::Unknown { raw, .. } = e.codec else { panic!("raw entry") };
        assert_eq!(raw.len(), 78);
        assert_eq!(&raw[74..76], &32u16.to_be_bytes());
    }

    #[test]
    fn hostile_sizes_are_errors() {
        assert!(Raw::new(0, 1, false).is_err());
        assert!(Raw::new(70_000, 1, true).is_err());
        assert!(Raw::new(2, 2, true).unwrap().encode(&[0; 4], 0).is_err());
        assert!(Raw::new16(65_535, 65_535).is_err());
        assert!(Raw::new16(2, 2).unwrap().encode(&[0; 4], 0).is_err());
    }

    #[test]
    fn argb16_preserves_low_bytes_and_places_alpha_first() {
        let mut r = Raw::new16(1, 1).unwrap();
        let rgba = [0x12, 0x34, 0xab, 0xcd, 0x01, 0x02, 0x89, 0xab];
        assert_eq!(r.encode(&rgba, 0).unwrap()[0].data, [0x89, 0xab, 0x12, 0x34, 0xab, 0xcd, 0x01, 0x02]);
        let e = r.sample_entry();
        assert_eq!(e.format.0, *b"b64a");
        assert_eq!(e.video.unwrap().depth, 64);
    }

    #[test]
    fn float_conversion_keeps_precision_and_channel_semantics() {
        let img = Image::filled(1, 1, [0.125, 0.0625, 0.03125, 0.5]);
        let values = |channels, mode| -> Vec<u16> {
            pixels16(&img, [0.25; 3], channels, mode).unwrap().as_chunks::<2>().0.iter().map(|b| u16::from_be_bytes(*b)).collect()
        };
        assert_eq!(values(Channels::Rgba, AlphaMode::Straight), [16384, 8192, 4096, 32768]);
        assert_eq!(values(Channels::Rgba, AlphaMode::Premultiplied), [8192, 4096, 2048, 32768]);
        assert_eq!(values(Channels::Rgb, AlphaMode::Straight), [16384, 12288, 10240, 65535]);
        assert_eq!(values(Channels::Alpha, AlphaMode::Straight), [32768, 32768, 32768, 65535]);
        let transparent = Image::filled(1, 1, [0.0; 4]);
        assert_eq!(pixels16(&transparent, [0.0; 3], Channels::Rgba, AlphaMode::Straight).unwrap(), [0; 8]);
        let hdr = Image::filled(1, 1, [-1.0, 2.0, 0.5, 1.0]);
        assert_eq!(pixels16(&hdr, [0.0; 3], Channels::Rgba, AlphaMode::Straight).unwrap(), [0, 0, 255, 255, 128, 0, 255, 255]);
        let bad = Image::filled(1, 1, [f32::NAN, 0.0, 0.0, 1.0]);
        assert!(pixels16(&bad, [0.0; 3], Channels::Rgb, AlphaMode::Straight).is_err());
    }
}
