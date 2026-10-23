//! Texture atlas decoding for E-mote PSB resources.
//!
//! The payload rules match the reference `eluna_player` renderer:
//!
//! - `RGBA4444` is D3D `A4R4G4B4` (alpha in bits 15..12), expanded with the
//!   standard `n * 17` scaling;
//! - 32-bpp pages are channel-ordered from the authored format and the root
//!   `spec` string (`ems`/`common` families store RGBA, Windows DIBs BGRA);
//! - `compress: "RL"` uses the E-mote run-length block format;
//! - otherwise PNG payloads are decoded as a fallback.
//!
//! Coverage limits are explicit: unsupported format/bit-depth combinations
//! return [`EmoteError::UnsupportedTextureFormat`] instead of guessing.

use std::collections::BTreeMap;

use eluna::sdk::EmoteRuntime;
use eluna::{EmoteTextureSource, PsbFile, PsbValue};

use crate::error::EmoteError;

/// A decoded RGBA8 texture page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecodedTexture {
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
    /// Tightly packed RGBA8 pixels, `width * height * 4` bytes.
    pub rgba8: Vec<u8>,
}

impl DecodedTexture {
    /// Number of pixels.
    pub fn pixel_count(&self) -> usize {
        self.width as usize * self.height as usize
    }

    /// Borrow one pixel as RGBA.
    pub fn pixel(&self, x: u32, y: u32) -> Option<[u8; 4]> {
        if x >= self.width || y >= self.height {
            return None;
        }
        let index = (y as usize * self.width as usize + x as usize) * 4;
        Some([
            self.rgba8[index],
            self.rgba8[index + 1],
            self.rgba8[index + 2],
            self.rgba8[index + 3],
        ])
    }
}

/// Convert one `RGBA4444` word (D3D `A4R4G4B4` bit layout) to RGBA8.
pub fn rgba4444_to_rgba8(word: u16) -> [u8; 4] {
    let a = ((word >> 12) & 0xF) as u8;
    let r = ((word >> 8) & 0xF) as u8;
    let g = ((word >> 4) & 0xF) as u8;
    let b = (word & 0xF) as u8;
    [r * 17, g * 17, b * 17, a * 17]
}

/// Decode one atlas page using the authored metadata and root `spec`.
pub fn decode_source(
    bytes: &[u8],
    source: &EmoteTextureSource,
    spec: Option<&str>,
) -> Result<DecodedTexture, EmoteError> {
    let format = source.format.as_deref();
    let bit_count = source.bit_count.unwrap_or(32);
    // RGBA4444 is always 16 bpp even when the authored metadata omits or
    // misstates `bitCount`; deriving bpp from the format keeps RL alignment
    // and the length check correct for PSB v2 pages.
    let bpp = if format == Some("RGBA4444") {
        2
    } else {
        (bit_count as usize / 8).max(1)
    };
    let expected = (source.width as usize)
        .checked_mul(source.height as usize)
        .and_then(|pixels| pixels.checked_mul(bpp))
        .ok_or(EmoteError::TextureSizeOverflow {
            width: source.width,
            height: source.height,
        })?;

    let payload = if source
        .compress
        .as_deref()
        .is_some_and(|value| value.eq_ignore_ascii_case("RL"))
    {
        decompress_rl(bytes, bpp, expected)?
    } else {
        bytes.to_vec()
    };

    if format == Some("RGBA4444") && payload.len() == expected {
        let pixels = source.width as usize * source.height as usize;
        let mut rgba8 = Vec::with_capacity(pixels * 4);
        for pair in payload.chunks_exact(2) {
            rgba8.extend_from_slice(&rgba4444_to_rgba8(u16::from_le_bytes([pair[0], pair[1]])));
        }
        return Ok(DecodedTexture {
            width: source.width,
            height: source.height,
            rgba8,
        });
    }

    if bit_count == 32 && payload.len() == expected {
        let order = channel_order(format, spec);
        let mut rgba8 = Vec::with_capacity(payload.len());
        for pixel in payload.chunks_exact(4) {
            match order {
                ChannelOrder::Bgra => {
                    rgba8.extend_from_slice(&[pixel[2], pixel[1], pixel[0], pixel[3]]);
                }
                ChannelOrder::Rgba => rgba8.extend_from_slice(pixel),
                ChannelOrder::Bgrx => {
                    rgba8.extend_from_slice(&[pixel[2], pixel[1], pixel[0], 0xff]);
                }
                ChannelOrder::Rgbx => {
                    rgba8.extend_from_slice(&[pixel[0], pixel[1], pixel[2], 0xff]);
                }
            }
        }
        return Ok(DecodedTexture {
            width: source.width,
            height: source.height,
            rgba8,
        });
    }

    if payload.starts_with(&[0x89, b'P', b'N', b'G']) {
        return decode_png(&payload);
    }

    Err(EmoteError::UnsupportedTextureFormat {
        format: format.unwrap_or("<missing>").to_owned(),
    })
}

/// Decode every texture source referenced by the runtime.
///
/// The returned map is keyed by PSB resource index, matching
/// [`EmoteStaticSprite::texture_resource_index`](eluna::EmoteStaticSprite).
pub fn decode_runtime_textures(
    runtime: &EmoteRuntime,
    spec: Option<&str>,
) -> Result<BTreeMap<u32, DecodedTexture>, EmoteError> {
    let mut out = BTreeMap::new();
    for source in runtime.texture_sources().values() {
        let index = source.resource_index;
        if out.contains_key(&index) {
            continue;
        }
        let Some(bytes) = runtime.texture_bytes(index) else {
            return Err(EmoteError::TextureResourceMissing {
                page: source.name.clone(),
                index: Some(index as usize),
            });
        };
        out.insert(index, decode_source(bytes, source, spec)?);
    }
    Ok(out)
}

/// Read the root `spec` string (`ems`, `common`, ...), if present.
pub fn spec_of(root: &PsbValue) -> Option<&str> {
    match root {
        PsbValue::Object(fields) => fields
            .iter()
            .find(|(name, _)| name == "spec")
            .and_then(|(_, value)| value.as_str()),
        _ => None,
    }
}

/// Decode one resource by index without a source record.
///
/// Used by tooling that only has the PSB tree.
pub fn decode_resource(
    psb: &PsbFile,
    data: &[u8],
    index: u32,
    format: Option<&str>,
    width: u32,
    height: u32,
    spec: Option<&str>,
) -> Result<DecodedTexture, EmoteError> {
    let bytes = psb
        .resource_bytes(data, index as usize)
        .ok_or_else(|| EmoteError::TextureResourceMissing {
            page: format!("resource#{index}"),
            index: Some(index as usize),
        })?;
    let source = EmoteTextureSource {
        name: format!("resource#{index}"),
        resource_index: index,
        width,
        height,
        format: format.map(str::to_owned),
        compress: None,
        bit_count: None,
        icons: BTreeMap::new(),
    };
    decode_source(bytes, &source, spec)
}

fn decode_png(bytes: &[u8]) -> Result<DecodedTexture, EmoteError> {
    let decoder = png::Decoder::new(bytes);
    let mut reader = decoder
        .read_info()
        .map_err(|_| EmoteError::UnsupportedTextureFormat {
            format: "png".to_owned(),
        })?;
    let mut buffer = vec![0; reader.output_buffer_size()];
    let info = reader
        .next_frame(&mut buffer)
        .map_err(|_| EmoteError::UnsupportedTextureFormat {
            format: "png".to_owned(),
        })?;
    let rgba8 = match info.color_type {
        png::ColorType::Rgba => buffer[..info.buffer_size()].to_vec(),
        png::ColorType::Rgb => {
            let mut out = Vec::with_capacity(info.width as usize * info.height as usize * 4);
            for pixel in buffer[..info.buffer_size()].chunks_exact(3) {
                out.extend_from_slice(&[pixel[0], pixel[1], pixel[2], 0xff]);
            }
            out
        }
        _ => {
            return Err(EmoteError::UnsupportedTextureFormat {
                format: "png".to_owned(),
            });
        }
    };
    Ok(DecodedTexture {
        width: info.width,
        height: info.height,
        rgba8,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ChannelOrder {
    Bgra,
    Rgba,
    Bgrx,
    Rgbx,
}

fn channel_order(format: Option<&str>, spec: Option<&str>) -> ChannelOrder {
    let Some(format) = format else {
        return if spec_uses_rgba(spec) {
            ChannelOrder::Rgba
        } else {
            ChannelOrder::Bgra
        };
    };
    let normalized: String = format
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .map(|c| c.to_ascii_uppercase())
        .collect();
    match normalized.as_str() {
        "RGBA" | "RGBA8" => {
            if spec_uses_rgba(spec) {
                ChannelOrder::Rgba
            } else {
                ChannelOrder::Bgra
            }
        }
        "BERGBA8" => ChannelOrder::Rgba,
        "LERGBA8" | "BGRA8" | "ARGB8" | "A8R8G8B8" | "D3DFMTA8R8G8B8" => ChannelOrder::Bgra,
        "BGRX8" | "X8R8G8B8" | "D3DFMTX8R8G8B8" => ChannelOrder::Bgrx,
        "RGBX8" | "RGBX" => ChannelOrder::Rgbx,
        _ => {
            if spec_uses_rgba(spec) {
                ChannelOrder::Rgba
            } else {
                ChannelOrder::Bgra
            }
        }
    }
}

fn spec_uses_rgba(spec: Option<&str>) -> bool {
    spec.is_some_and(|value| {
        matches!(
            value.to_ascii_lowercase().as_str(),
            "common" | "ems" | "vita" | "psp" | "ps3"
        )
    })
}

fn decompress_rl(bytes: &[u8], align: usize, expected: usize) -> Result<Vec<u8>, EmoteError> {
    if align == 0 {
        return Err(EmoteError::UnsupportedTextureFormat {
            format: "RL/align=0".to_owned(),
        });
    }
    let mut out = Vec::with_capacity(expected);
    let mut pos = 0usize;
    while pos < bytes.len() {
        let command = bytes[pos];
        pos += 1;
        if (command & 0x80) != 0 {
            let count = ((command ^ 0x80) as usize) + 3;
            let end = pos
                .checked_add(align)
                .ok_or_else(|| EmoteError::UnsupportedTextureFormat {
                    format: "RL/overflow".to_owned(),
                })?;
            let pattern = bytes.get(pos..end).ok_or_else(|| {
                EmoteError::UnsupportedTextureFormat {
                    format: "RL/truncated".to_owned(),
                }
            })?;
            for _ in 0..count {
                out.extend_from_slice(pattern);
            }
            pos = end;
        } else {
            let count = ((command as usize) + 1) * align;
            let end = pos
                .checked_add(count)
                .ok_or_else(|| EmoteError::UnsupportedTextureFormat {
                    format: "RL/overflow".to_owned(),
                })?;
            let block = bytes.get(pos..end).ok_or_else(|| {
                EmoteError::UnsupportedTextureFormat {
                    format: "RL/truncated".to_owned(),
                }
            })?;
            out.extend_from_slice(block);
            pos = end;
        }
        if out.len() > expected {
            return Err(EmoteError::UnsupportedTextureFormat {
                format: "RL/output-too-long".to_owned(),
            });
        }
    }
    if out.len() != expected {
        return Err(EmoteError::TextureSizeMismatch {
            width: 0,
            height: 0,
            expected,
            actual: out.len(),
        });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn word_layout_is_a4r4g4b4() {
        assert_eq!(rgba4444_to_rgba8(0xF842), [0x88, 0x44, 0x22, 0xFF]);
        assert_eq!(rgba4444_to_rgba8(0x0000), [0, 0, 0, 0]);
        assert_eq!(rgba4444_to_rgba8(0xFFFF), [255, 255, 255, 255]);
    }

    #[test]
    fn ems_spec_keeps_rgba_order() {
        assert_eq!(channel_order(Some("RGBA8"), Some("ems")), ChannelOrder::Rgba);
        assert_eq!(channel_order(Some("RGBA8"), None), ChannelOrder::Bgra);
    }

    #[test]
    fn rgba4444_uses_16bpp_without_bit_count() {
        let source = EmoteTextureSource {
            name: "tex".to_owned(),
            resource_index: 0,
            width: 2,
            height: 1,
            format: Some("RGBA4444".to_owned()),
            compress: None,
            bit_count: None,
            icons: BTreeMap::new(),
        };
        let decoded = decode_source(&[0x42, 0xF8, 0xFF, 0xFF], &source, None).unwrap();
        assert_eq!(decoded.width, 2);
        assert_eq!(decoded.height, 1);
        assert_eq!(decoded.pixel(0, 0), Some([0x88, 0x44, 0x22, 0xFF]));
        assert_eq!(decoded.pixel(1, 0), Some([0xFF, 0xFF, 0xFF, 0xFF]));
    }
}
