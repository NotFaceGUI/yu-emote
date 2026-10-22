//! Texture atlas decoding.
//!
//! E-mote PSB stores atlas pages as raw pixel resources referenced by
//! `source/tex#N/texture/pixel`; `texture/type` names the format. The official
//! YU-RIS sample uses `RGBA4444`.
//!
//! ## `RGBA4444` byte order (evidenced)
//!
//! The official `emotedriver.dll` converts `RGBA4444` to a 32-bit D3D color in
//! `0x10054BA0`:
//!
//! ```text
//! word = u16 little-endian
//! a = word >> 12          r = (word >> 8) & 0xF
//! g = (word >> 4) & 0xF   b = word & 0xF
//! out = (a*17) << 24 | (r*17) << 16 | (g*17) << 8 | (b*17)
//! ```
//!
//! That is the `D3DFMT_A4R4G4B4` bit layout (alpha in bits 15..12). Decoding
//! the official atlas with this order produces the expected character colors;
//! the alternate nibble order swaps skin/hair colors, so it is not used.
//!
//! Converted pixels are returned as tightly packed RGBA8 in row-major order,
//! top-left origin (the same orientation as the atlas rectangles).

use crate::error::EmoteError;
use crate::psb::PsbDocument;
use crate::schema::TextureSource;

/// Bytes per pixel for the `RGBA4444` format.
pub const RGBA4444_BPP: usize = 2;

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

/// Decode an `RGBA4444` byte buffer into RGBA8.
///
/// # Errors
///
/// Returns [`EmoteError::TextureSizeMismatch`] when the buffer length is not
/// exactly `width * height * 2`.
pub fn decode_rgba4444(
    bytes: &[u8],
    width: u32,
    height: u32,
) -> Result<DecodedTexture, EmoteError> {
    let expected = (width as usize)
        .checked_mul(height as usize)
        .and_then(|pixels| pixels.checked_mul(RGBA4444_BPP))
        .ok_or(EmoteError::TextureSizeOverflow { width, height })?;
    if bytes.len() != expected {
        return Err(EmoteError::TextureSizeMismatch {
            width,
            height,
            expected,
            actual: bytes.len(),
        });
    }
    let mut rgba8 = Vec::with_capacity(expected * 2);
    for pair in bytes.chunks_exact(RGBA4444_BPP) {
        let word = u16::from_le_bytes([pair[0], pair[1]]);
        rgba8.extend_from_slice(&rgba4444_to_rgba8(word));
    }
    Ok(DecodedTexture {
        width,
        height,
        rgba8,
    })
}

/// Decode one atlas page from a parsed document.
///
/// # Errors
///
/// Returns [`EmoteError::TextureResourceMissing`] when the page has no resource
/// reference or the reference is out of range, and
/// [`EmoteError::UnsupportedTextureFormat`] for formats this build does not
/// decode.
pub fn decode_page(
    document: &PsbDocument,
    source: &TextureSource,
) -> Result<DecodedTexture, EmoteError> {
    let format = source
        .format
        .as_deref()
        .ok_or_else(|| EmoteError::UnsupportedTextureFormat {
            format: "<missing>".into(),
        })?;
    if format != "RGBA4444" {
        return Err(EmoteError::UnsupportedTextureFormat {
            format: format.to_string(),
        });
    }
    let index = source.resource.ok_or(EmoteError::TextureResourceMissing {
        page: source.name.clone(),
        index: None,
    })? as usize;
    let bytes =
        document
            .resource_data
            .get(index)
            .ok_or_else(|| EmoteError::TextureResourceMissing {
                page: source.name.clone(),
                index: Some(index),
            })?;
    decode_rgba4444(bytes, source.width, source.height)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn word_layout_is_a4r4g4b4() {
        // a=0xF r=0x8 g=0x4 b=0x2
        assert_eq!(rgba4444_to_rgba8(0xF842), [0x88, 0x44, 0x22, 0xFF]);
        assert_eq!(rgba4444_to_rgba8(0x0000), [0, 0, 0, 0]);
        assert_eq!(rgba4444_to_rgba8(0xFFFF), [255, 255, 255, 255]);
    }

    #[test]
    fn size_mismatch_is_explicit() {
        let error = decode_rgba4444(&[0; 3], 2, 2).unwrap_err();
        assert!(matches!(
            error,
            EmoteError::TextureSizeMismatch {
                expected: 8,
                actual: 3,
                ..
            }
        ));
    }
}
