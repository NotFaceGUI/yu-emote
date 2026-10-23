//! Deterministic CPU rasterizer for previews, screenshots and pixel checks.
//!
//! The vertex math and blend equations match the reference `eluna_player`
//! WebGPU path, but evaluation is done in software so the CLI tools can emit
//! PNG sequences without a GPU or window:
//!
//! - triangles are rasterized with barycentric coverage and bilinear atlas
//!   sampling;
//! - the native texture stage (`MODULATE2X` high nibble, modes 3/4 alpha
//!   premultiply, mode 5 complement) runs after `texture * diffuse`;
//! - the D3D9 blend table from `MMotionDevice::SetBlendMode` is applied per
//!   pixel;
//! - inner stencil owners are approximated as coverage buffers (union of the
//!   owner's source sprites) multiplied into each dependent sprite.
//!
//! Not covered: outer-stencil reference counting, feedback history, type-4
//! particles and stereovision. Those are reported by [`SoftStats`] or as
//! skipped plan entries rather than silently invented.

use std::collections::BTreeMap;

use serde::Serialize;

use crate::error::EmoteError;
use crate::plan::{DrawItem, PlanVertex, RenderPlan};
use crate::texture::DecodedTexture;

/// Rendered RGBA8 image.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenderImage {
    /// Output width.
    pub width: u32,
    /// Output height.
    pub height: u32,
    /// Tightly packed RGBA8 pixels, `width * height * 4` bytes.
    pub rgba8: Vec<u8>,
}

impl RenderImage {
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

    /// Number of pixels with non-zero alpha.
    pub fn covered_pixels(&self) -> usize {
        self.rgba8.chunks_exact(4).filter(|p| p[3] > 0).count()
    }

    /// Bounding box of non-zero alpha pixels (`left, top, right, bottom`).
    pub fn coverage_bbox(&self) -> Option<(u32, u32, u32, u32)> {
        let mut min_x = self.width;
        let mut min_y = self.height;
        let mut max_x = 0u32;
        let mut max_y = 0u32;
        let mut any = false;
        for y in 0..self.height {
            for x in 0..self.width {
                let index = (y as usize * self.width as usize + x as usize) * 4;
                if self.rgba8[index + 3] > 0 {
                    any = true;
                    min_x = min_x.min(x);
                    min_y = min_y.min(y);
                    max_x = max_x.max(x);
                    max_y = max_y.max(y);
                }
            }
        }
        any.then_some((min_x, min_y, max_x + 1, max_y + 1))
    }
}

/// Counters describing what the rasterizer consumed.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct SoftStats {
    /// Color draw items visited.
    pub items: usize,
    /// Triangles rasterized.
    pub triangles: usize,
    /// Pixels touched by at least one triangle.
    pub covered: usize,
    /// Items with a non-default blend mode.
    pub non_default_blend: usize,
    /// Items with at least one stencil owner.
    pub masked_items: usize,
    /// Mask source items rasterized.
    pub mask_sources: usize,
    /// Items whose texture resource was unavailable.
    pub missing_textures: usize,
    /// Items skipped because they need feedback history.
    pub feedback: usize,
}

/// Presentation transform applied to plan vertices before rasterization.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct ViewTransform {
    /// Uniform scale.
    pub scale: f32,
    /// Translation in output pixels.
    pub offset: [f32; 2],
}

impl Default for ViewTransform {
    fn default() -> Self {
        Self::identity()
    }
}

impl ViewTransform {
    /// No transform.
    pub const fn identity() -> Self {
        Self {
            scale: 1.0,
            offset: [0.0, 0.0],
        }
    }

    /// Fit the plan bounds into the output with a pixel margin.
    pub fn fit(plan: &RenderPlan, margin: f32) -> Self {
        let Some((min, max)) = plan.bounds() else {
            return Self::identity();
        };
        let width = (max[0] - min[0]).max(1.0);
        let height = (max[1] - min[1]).max(1.0);
        let available_w = (plan.width.max(1) as f32 - margin * 2.0).max(1.0);
        let available_h = (plan.height.max(1) as f32 - margin * 2.0).max(1.0);
        let scale = (available_w / width).min(available_h / height);
        let center_x = (min[0] + max[0]) * 0.5;
        let center_y = (min[1] + max[1]) * 0.5;
        Self {
            scale,
            offset: [
                plan.width as f32 * 0.5 - center_x * scale,
                plan.height as f32 * 0.5 - center_y * scale,
            ],
        }
    }

    /// Apply to a model-space point.
    pub fn apply(self, point: [f32; 2]) -> [f32; 2] {
        [
            point[0] * self.scale + self.offset[0],
            point[1] * self.scale + self.offset[1],
        ]
    }
}

fn screen_clip(rect: Option<[f32; 4]>, view: ViewTransform) -> Option<[f32; 4]> {
    let rect = rect?;
    let a = view.apply([rect[0], rect[1]]);
    let b = view.apply([rect[2], rect[3]]);
    Some([
        a[0].min(b[0]),
        a[1].min(b[1]),
        a[0].max(b[0]),
        a[1].max(b[1]),
    ])
}

fn color_item(item: &DrawItem) -> bool {
    !matches!(
        item.pass,
        crate::EmoteDrawPass::MaskGeneration | crate::EmoteDrawPass::StencilCompositeMask
    )
}

/// Render a plan with already decoded textures keyed by PSB resource index.
pub fn render_with_textures(
    plan: &RenderPlan,
    textures: &BTreeMap<u32, DecodedTexture>,
    view: ViewTransform,
) -> Result<(RenderImage, SoftStats), EmoteError> {
    let width = plan.width.max(1);
    let height = plan.height.max(1);
    let mut rgba8 = vec![0u8; width as usize * height as usize * 4];
    let mut stats = SoftStats::default();

    let mut masks: Vec<Option<Vec<f32>>> = Vec::with_capacity(plan.mask_groups.len());
    for group in &plan.mask_groups {
        let mut buffer = vec![0.0f32; width as usize * height as usize];
        let mut used = false;
        for source_index in &group.sources {
            let Some(item) = plan.items.get(*source_index) else {
                continue;
            };
            let Some(texture) = item_texture(item, textures) else {
                continue;
            };
            used = true;
            stats.mask_sources += 1;
            let clip = screen_clip(item.clip_rect, view);
            for triangle in item.indices.chunks_exact(3) {
                let (Some(v0), Some(v1), Some(v2)) = (
                    item.vertices.get(triangle[0] as usize),
                    item.vertices.get(triangle[1] as usize),
                    item.vertices.get(triangle[2] as usize),
                ) else {
                    continue;
                };
                rasterize_triangle_coverage(
                    &mut buffer,
                    width,
                    height,
                    texture,
                    v0,
                    v1,
                    v2,
                    view,
                    clip,
                );
            }
        }
        masks.push(used.then_some(buffer));
    }

    for item in &plan.items {
        if !color_item(item) {
            continue;
        }
        stats.items += 1;
        if (1..=5).contains(&(item.blend_mode & 0xFF0F)) {
            stats.non_default_blend += 1;
        }
        if !item.mask_groups.is_empty() {
            stats.masked_items += 1;
        }
        if item.resource.is_none() {
            stats.feedback += 1;
            continue;
        }
        let Some(texture) = item_texture(item, textures) else {
            stats.missing_textures += 1;
            continue;
        };
        let clip = screen_clip(item.clip_rect, view);
        for triangle in item.indices.chunks_exact(3) {
            let (Some(v0), Some(v1), Some(v2)) = (
                item.vertices.get(triangle[0] as usize),
                item.vertices.get(triangle[1] as usize),
                item.vertices.get(triangle[2] as usize),
            ) else {
                continue;
            };
            stats.triangles += 1;
            stats.covered += rasterize_triangle(
                &mut rgba8,
                width,
                height,
                texture,
                v0,
                v1,
                v2,
                view,
                &masks,
                &item.mask_groups,
                clip,
                item.blend_mode,
                item.wipe,
            );
        }
    }
    Ok((
        RenderImage {
            width,
            height,
            rgba8,
        },
        stats,
    ))
}

fn item_texture<'a>(
    item: &DrawItem,
    textures: &'a BTreeMap<u32, DecodedTexture>,
) -> Option<&'a DecodedTexture> {
    item.resource.and_then(|resource| textures.get(&resource))
}

#[allow(clippy::too_many_arguments)]
fn rasterize_triangle(
    target: &mut [u8],
    width: u32,
    height: u32,
    texture: &DecodedTexture,
    v0: &PlanVertex,
    v1: &PlanVertex,
    v2: &PlanVertex,
    view: ViewTransform,
    masks: &[Option<Vec<f32>>],
    mask_groups: &[usize],
    clip: Option<[f32; 4]>,
    blend_mode: u32,
    wipe: [f32; 3],
) -> usize {
    let p0 = view.apply(v0.position);
    let p1 = view.apply(v1.position);
    let p2 = view.apply(v2.position);
    let mut min_x = p0[0].min(p1[0]).min(p2[0]).floor();
    let mut max_x = p0[0].max(p1[0]).max(p2[0]).ceil();
    let mut min_y = p0[1].min(p1[1]).min(p2[1]).floor();
    let mut max_y = p0[1].max(p1[1]).max(p2[1]).ceil();
    if let Some(clip) = clip {
        min_x = min_x.max(clip[0]);
        max_x = max_x.min(clip[2]);
        min_y = min_y.max(clip[1]);
        max_y = max_y.min(clip[3]);
    }
    let min_x = min_x.max(0.0) as i64;
    let max_x = max_x.min(width as f32 - 1.0) as i64;
    let min_y = min_y.max(0.0) as i64;
    let max_y = max_y.min(height as f32 - 1.0) as i64;
    if max_x < min_x || max_y < min_y {
        return 0;
    }
    let area = edge(p0, p1, p2);
    if area.abs() < 1e-9 {
        return 0;
    }

    let mut covered = 0;
    for y in min_y..=max_y {
        for x in min_x..=max_x {
            let index = y as usize * width as usize + x as usize;
            let mut alpha_scale = 1.0f32;
            for group in mask_groups {
                let Some(Some(mask)) = masks.get(*group) else {
                    alpha_scale = 0.0;
                    break;
                };
                let coverage = mask[index];
                if coverage <= 0.003 {
                    alpha_scale = 0.0;
                    break;
                }
                alpha_scale *= coverage;
            }
            if alpha_scale <= 0.0 {
                continue;
            }

            let point = [x as f32 + 0.5, y as f32 + 0.5];
            let w0 = edge(p1, p2, point) / area;
            let w1 = edge(p2, p0, point) / area;
            let w2 = edge(p0, p1, point) / area;
            if w0 < -1e-6 || w1 < -1e-6 || w2 < -1e-6 {
                continue;
            }
            let uv = [
                v0.uv[0] * w0 + v1.uv[0] * w1 + v2.uv[0] * w2,
                v0.uv[1] * w0 + v1.uv[1] * w1 + v2.uv[1] * w2,
            ];
            let color = [
                v0.color[0] * w0 + v1.color[0] * w1 + v2.color[0] * w2,
                v0.color[1] * w0 + v1.color[1] * w1 + v2.color[1] * w2,
                v0.color[2] * w0 + v1.color[2] * w1 + v2.color[2] * w2,
                v0.color[3] * w0 + v1.color[3] * w1 + v2.color[3] * w2,
            ];
            let texel = sample_bilinear(texture, uv);
            let mut alpha = color[3] * alpha_scale;
            if wipe[0] > 0.5 {
                alpha = (alpha * wipe[1] + wipe[2]).clamp(0.0, 1.0);
            }
            let mut src = [
                texel[0] * color[0],
                texel[1] * color[1],
                texel[2] * color[2],
                texel[3] * alpha,
            ];
            if (blend_mode & 0xF0) == 0x10 {
                src[0] = (src[0] * 2.0).min(1.0);
                src[1] = (src[1] * 2.0).min(1.0);
                src[2] = (src[2] * 2.0).min(1.0);
            }
            let low = blend_mode & 0xFF0F;
            if low == 3 || low == 4 {
                src[0] *= src[3];
                src[1] *= src[3];
                src[2] *= src[3];
            } else if low == 5 {
                src[0] = 1.0 - src[0];
                src[1] = 1.0 - src[1];
                src[2] = 1.0 - src[2];
            }
            let pixel = &mut target[index * 4..index * 4 + 4];
            if low == 0 {
                blend_source_over(pixel, src);
            } else {
                blend_pixel(pixel, src, low);
            }
            covered += 1;
        }
    }
    covered
}

fn edge(a: [f32; 2], b: [f32; 2], c: [f32; 2]) -> f32 {
    (b[0] - a[0]) * (c[1] - a[1]) - (b[1] - a[1]) * (c[0] - a[0])
}

fn sample_bilinear(texture: &DecodedTexture, uv: [f32; 2]) -> [f32; 4] {
    let x = uv[0] * texture.width as f32 - 0.5;
    let y = uv[1] * texture.height as f32 - 0.5;
    let x0 = x.floor();
    let y0 = y.floor();
    let fx = x - x0;
    let fy = y - y0;
    let px = |x: i64, y: i64| -> [f32; 4] {
        let x = x.clamp(0, texture.width as i64 - 1) as u32;
        let y = y.clamp(0, texture.height as i64 - 1) as u32;
        let pixel = texture.pixel(x, y).unwrap_or([0; 4]);
        [
            pixel[0] as f32 / 255.0,
            pixel[1] as f32 / 255.0,
            pixel[2] as f32 / 255.0,
            pixel[3] as f32 / 255.0,
        ]
    };
    let c00 = px(x0 as i64, y0 as i64);
    let c10 = px(x0 as i64 + 1, y0 as i64);
    let c01 = px(x0 as i64, y0 as i64 + 1);
    let c11 = px(x0 as i64 + 1, y0 as i64 + 1);
    let mut out = [0.0f32; 4];
    for channel in 0..4 {
        let top = c00[channel] + (c10[channel] - c00[channel]) * fx;
        let bottom = c01[channel] + (c11[channel] - c01[channel]) * fx;
        out[channel] = (top + (bottom - top) * fy).clamp(0.0, 1.0);
    }
    out
}

fn blend_pixel(dst: &mut [u8], src: [f32; 4], mode: u32) {
    let dst_color = [
        dst[0] as f32 / 255.0,
        dst[1] as f32 / 255.0,
        dst[2] as f32 / 255.0,
        dst[3] as f32 / 255.0,
    ];
    let alpha = src[3].clamp(0.0, 1.0);
    let (r, g, b, a) = match mode {
        1 => (
            src[0] * alpha + dst_color[0],
            src[1] * alpha + dst_color[1],
            src[2] * alpha + dst_color[2],
            dst_color[3],
        ),
        2 | 5 => (
            dst_color[0] - src[0] * alpha,
            dst_color[1] - src[1] * alpha,
            dst_color[2] - src[2] * alpha,
            dst_color[3],
        ),
        3 => (
            src[0] * dst_color[0] + dst_color[0] * (1.0 - alpha),
            src[1] * dst_color[1] + dst_color[1] * (1.0 - alpha),
            src[2] * dst_color[2] + dst_color[2] * (1.0 - alpha),
            dst_color[3],
        ),
        4 => (
            src[0] * (1.0 - dst_color[0]) + dst_color[0],
            src[1] * (1.0 - dst_color[1]) + dst_color[1],
            src[2] * (1.0 - dst_color[2]) + dst_color[2],
            dst_color[3],
        ),
        _ => {
            let inverse = 1.0 - alpha;
            (
                src[0] * alpha + dst_color[0] * inverse,
                src[1] * alpha + dst_color[1] * inverse,
                src[2] * alpha + dst_color[2] * inverse,
                alpha + dst_color[3] * inverse,
            )
        }
    };
    let quantize = |value: f32| (value.clamp(0.0, 1.0) * 255.0).round() as u8;
    dst[0] = quantize(r);
    dst[1] = quantize(g);
    dst[2] = quantize(b);
    dst[3] = quantize(a);
}

fn blend_source_over(dst: &mut [u8], src: [f32; 4]) {
    let sa = src[3].clamp(0.0, 1.0);
    if sa <= 0.0 {
        return;
    }
    let da = dst[3] as f32 / 255.0;
    let out_a = sa + da * (1.0 - sa);
    if out_a <= 0.0 {
        return;
    }
    for channel in 0..3 {
        let s = src[channel].clamp(0.0, 1.0);
        let d = dst[channel] as f32 / 255.0;
        dst[channel] = (((s * sa + d * da * (1.0 - sa)) / out_a) * 255.0)
            .round()
            .clamp(0.0, 255.0) as u8;
    }
    dst[3] = (out_a * 255.0).round().clamp(0.0, 255.0) as u8;
}

/// Rasterize one triangle into an alpha-coverage buffer.
///
/// Coverage is the union (max) of texture alpha times vertex alpha, matching
/// the native mask pass where overlapping source sprites increment the same
/// stencil reference only once.
#[allow(clippy::too_many_arguments)]
fn rasterize_triangle_coverage(
    target: &mut [f32],
    width: u32,
    height: u32,
    texture: &DecodedTexture,
    v0: &PlanVertex,
    v1: &PlanVertex,
    v2: &PlanVertex,
    view: ViewTransform,
    clip: Option<[f32; 4]>,
) -> usize {
    let p0 = view.apply(v0.position);
    let p1 = view.apply(v1.position);
    let p2 = view.apply(v2.position);
    let mut min_x = p0[0].min(p1[0]).min(p2[0]).floor();
    let mut max_x = p0[0].max(p1[0]).max(p2[0]).ceil();
    let mut min_y = p0[1].min(p1[1]).min(p2[1]).floor();
    let mut max_y = p0[1].max(p1[1]).max(p2[1]).ceil();
    if let Some(clip) = clip {
        min_x = min_x.max(clip[0]);
        max_x = max_x.min(clip[2]);
        min_y = min_y.max(clip[1]);
        max_y = max_y.min(clip[3]);
    }
    let min_x = min_x.max(0.0) as i64;
    let max_x = max_x.min(width as f32 - 1.0) as i64;
    let min_y = min_y.max(0.0) as i64;
    let max_y = max_y.min(height as f32 - 1.0) as i64;
    if max_x < min_x || max_y < min_y {
        return 0;
    }
    let area = edge(p0, p1, p2);
    if area.abs() < 1e-9 {
        return 0;
    }
    let mut covered = 0;
    for y in min_y..=max_y {
        for x in min_x..=max_x {
            let point = [x as f32 + 0.5, y as f32 + 0.5];
            let w0 = edge(p1, p2, point) / area;
            let w1 = edge(p2, p0, point) / area;
            let w2 = edge(p0, p1, point) / area;
            if w0 < -1e-6 || w1 < -1e-6 || w2 < -1e-6 {
                continue;
            }
            let uv = [
                v0.uv[0] * w0 + v1.uv[0] * w1 + v2.uv[0] * w2,
                v0.uv[1] * w0 + v1.uv[1] * w1 + v2.uv[1] * w2,
            ];
            let alpha = v0.color[3] * w0 + v1.color[3] * w1 + v2.color[3] * w2;
            let texel = sample_bilinear(texture, uv);
            let index = y as usize * width as usize + x as usize;
            let value = (alpha * texel[3]).clamp(0.0, 1.0);
            if value > target[index] {
                target[index] = value;
            }
            if value > 0.003 {
                covered += 1;
            }
        }
    }
    covered
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_plan_falls_back_to_identity_view() {
        let plan = RenderPlan {
            width: 100,
            height: 100,
            items: vec![],
            mask_groups: vec![],
            skipped: BTreeMap::new(),
        };
        assert_eq!(ViewTransform::fit(&plan, 10.0), ViewTransform::identity());
    }
}
