//! Deterministic software rasterizer for the M3 draw plan.
//!
//! This is a reference path used to validate data/pose/plan without a GPU and
//! to produce deterministic screenshots for comparison. It is **not** the
//! final M3 renderer: wgpu is the delivery target and the official D3D9
//! blend/sampling state still needs M4 parity.
//!
//! Implemented:
//! - textured triangles with bilinear sampling (clamp-to-edge);
//! - vertex corner colors (`0xRRGGBBAA`) multiplied with the texel;
//! - straight-alpha source-over blending;
//! - mesh and quad items from [`RenderPlan`];
//! - composite alpha masks (`stencilType & 4`): sources are rasterized into
//!   per-group coverage buffers and multiply descendant alpha.
//!
//! Not implemented (counted in [`SoftStats`]):
//! - non-normal blend modes (`bm` other than the default 0x10);
//! - the type-7 clip rectangle;
//! - pixelate screen-space pass.

use std::collections::BTreeMap;

use serde::Serialize;

use crate::error::EmoteError;
use crate::plan::{PlanVertex, RenderPlan};
use crate::pose::Pose;
use crate::schema::Model;
use crate::texture::{self, DecodedTexture};

/// Rendered RGBA8 image, top-left origin.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenderImage {
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
    /// Tightly packed RGBA8 pixels.
    pub rgba8: Vec<u8>,
}

impl RenderImage {
    /// Pixel count.
    pub fn pixel_count(&self) -> usize {
        self.width as usize * self.height as usize
    }

    /// Borrow one pixel.
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
        self.rgba8.chunks_exact(4).filter(|px| px[3] != 0).count()
    }

    /// Bounding box of non-zero-alpha pixels, if any.
    pub fn coverage_bbox(&self) -> Option<(u32, u32, u32, u32)> {
        let mut bbox: Option<(u32, u32, u32, u32)> = None;
        for y in 0..self.height {
            for x in 0..self.width {
                if self.rgba8[(y as usize * self.width as usize + x as usize) * 4 + 3] != 0 {
                    bbox = Some(match bbox {
                        Some((x0, y0, x1, y1)) => (x0.min(x), y0.min(y), x1.max(x), y1.max(y)),
                        None => (x, y, x, y),
                    });
                }
            }
        }
        bbox
    }
}

/// Rasterizer counters.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct SoftStats {
    /// Draw items submitted.
    pub items: usize,
    /// Triangles rasterized.
    pub triangles: usize,
    /// Pixels covered by a triangle (before blending).
    pub covered: usize,
    /// Items skipped because their page had no decodable texture.
    pub missing_textures: usize,
    /// Items using a non-default blend mode (rendered with normal alpha).
    pub unsupported_blend: usize,
    /// Items carrying mask/stencil state.
    pub masked_items: usize,
    /// Source draw items rasterized into composite-mask buffers.
    pub mask_sources: usize,
}

/// Presentation transform applied to plan vertices before rasterization.
///
/// E-mote model coordinates are not screen coordinates: the YU-RIS host
/// composites the character with a scene-specific position/scale. The plan
/// stays in model space; this transform is supplied by the caller (the CLI
/// uses `fit` for previews, the kernel will use the CG layer transform).
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
    mask: Option<&[f32]>,
) -> usize {
    let p0 = view.apply(v0.position);
    let p1 = view.apply(v1.position);
    let p2 = view.apply(v2.position);
    let min_x = p0[0].min(p1[0]).min(p2[0]).floor();
    let max_x = p0[0].max(p1[0]).max(p2[0]).ceil();
    let min_y = p0[1].min(p1[1]).min(p2[1]).floor();
    let max_y = p0[1].max(p1[1]).max(p2[1]).ceil();
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
            let color = [
                v0.color[0] * w0 + v1.color[0] * w1 + v2.color[0] * w2,
                v0.color[1] * w0 + v1.color[1] * w1 + v2.color[1] * w2,
                v0.color[2] * w0 + v1.color[2] * w1 + v2.color[2] * w2,
                v0.color[3] * w0 + v1.color[3] * w1 + v2.color[3] * w2,
            ];
            let texel = sample_bilinear(texture, uv);
            let index = (y as usize * width as usize + x as usize) * 4;
            let mut alpha = color[3];
            if let Some(mask) = mask {
                let coverage = mask[y as usize * width as usize + x as usize];
                if coverage <= 0.003 {
                    continue;
                }
                alpha *= coverage;
            }
            let src = [
                texel[0] * color[0],
                texel[1] * color[1],
                texel[2] * color[2],
                texel[3] * alpha,
            ];
            blend_source_over(&mut target[index..index + 4], src);
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

/// Render helper that decodes pages from a parsed document.
///
/// [`render_with_document`] uses this path; [`render_plan_with_document`] is
/// kept for callers that already built a plan (the wgpu backend will supply
/// its own textures).
pub fn render_with_document(
    document: &crate::psb::PsbDocument,
    model: &Model,
    pose: &Pose,
    view: ViewTransform,
) -> Result<(RenderImage, SoftStats), EmoteError> {
    let plan = RenderPlan::build(model, pose);
    render_plan_with_document(document, model, &plan, view)
}

/// Render an existing plan, decoding atlas pages from `document`.
pub fn render_plan_with_document(
    document: &crate::psb::PsbDocument,
    model: &Model,
    plan: &RenderPlan,
    view: ViewTransform,
) -> Result<(RenderImage, SoftStats), EmoteError> {
    let mut textures: BTreeMap<String, DecodedTexture> = BTreeMap::new();
    for item in &plan.items {
        if textures.contains_key(&item.page) {
            continue;
        }
        if item.page == "blank" {
            textures.insert(
                "blank".to_string(),
                DecodedTexture {
                    width: 1,
                    height: 1,
                    rgba8: vec![255, 255, 255, 255],
                },
            );
            continue;
        }
        let Some(page) = model.sources.iter().find(|page| page.name == item.page) else {
            continue;
        };
        textures.insert(item.page.clone(), texture::decode_page(document, page)?);
    }
    render_with_textures(plan, &textures, view)
}

fn render_with_textures(
    plan: &RenderPlan,
    textures: &BTreeMap<String, DecodedTexture>,
    view: ViewTransform,
) -> Result<(RenderImage, SoftStats), EmoteError> {
    let width = plan.width.max(1);
    let height = plan.height.max(1);
    let mut rgba8 = vec![0u8; width as usize * height as usize * 4];
    let mut stats = SoftStats::default();

    // Composite alpha masks (`stencilType & 4`). Each owner's named source
    // layers are rasterized into a coverage buffer in the same motion scope;
    // descendants are then multiplied by that coverage. This mirrors the
    // native inner-stencil union plus the shader's alpha discard.
    let mut masks: Vec<Option<Vec<f32>>> = Vec::with_capacity(plan.mask_groups.len());
    for (group_id, group) in plan.mask_groups.iter().enumerate() {
        let used = plan
            .items
            .iter()
            .any(|item| item.mask_group == Some(group_id));
        if !used {
            masks.push(None);
            continue;
        }
        let mut buffer = vec![0.0f32; width as usize * height as usize];
        let mut source_count = 0usize;
        for item in &plan.items {
            // Source paths were resolved in the pose (own scope; type-3
            // proxies expand to their nested player's emitted sprites).
            if !group.source_paths.iter().any(|path| path == &item.path) {
                continue;
            }
            let Some(texture) = textures.get(&item.page) else {
                continue;
            };
            source_count += 1;
            for triangle in item.indices.chunks_exact(3) {
                let (Some(v0), Some(v1), Some(v2)) = (
                    item.vertices.get(triangle[0] as usize),
                    item.vertices.get(triangle[1] as usize),
                    item.vertices.get(triangle[2] as usize),
                ) else {
                    continue;
                };
                rasterize_triangle_coverage(&mut buffer, width, height, texture, v0, v1, v2, view);
            }
        }
        stats.mask_sources += source_count;
        if let Ok(dir) = std::env::var("YU_EMOTE_DUMP_MASKS") {
            let mut pgm = format!(
                "P5
{} {}
255
",
                width, height
            )
            .into_bytes();
            pgm.extend(
                buffer
                    .iter()
                    .map(|value| (value.clamp(0.0, 1.0) * 255.0) as u8),
            );
            let _ = std::fs::create_dir_all(&dir);
            let _ = std::fs::write(format!("{dir}/mask-{group_id}.pgm"), pgm);
        }
        masks.push(Some(buffer));
    }

    for item in &plan.items {
        stats.items += 1;
        if (item.blend_mode & 0xF0) != 0x10 {
            stats.unsupported_blend += 1;
        }
        if item.mask != 0 || item.stencil_type != 0 || item.mask_group.is_some() {
            stats.masked_items += 1;
        }
        let Some(texture) = textures.get(&item.page) else {
            stats.missing_textures += 1;
            continue;
        };
        let mask = item
            .mask_group
            .and_then(|group| masks.get(group))
            .and_then(|mask| mask.as_deref());
        for triangle in item.indices.chunks_exact(3) {
            let (Some(v0), Some(v1), Some(v2)) = (
                item.vertices.get(triangle[0] as usize),
                item.vertices.get(triangle[1] as usize),
                item.vertices.get(triangle[2] as usize),
            ) else {
                continue;
            };
            stats.triangles += 1;
            stats.covered +=
                rasterize_triangle(&mut rgba8, width, height, texture, v0, v1, v2, view, mask);
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
) -> usize {
    let p0 = view.apply(v0.position);
    let p1 = view.apply(v1.position);
    let p2 = view.apply(v2.position);
    let min_x = p0[0].min(p1[0]).min(p2[0]).floor().max(0.0) as i64;
    let max_x = p0[0].max(p1[0]).max(p2[0]).ceil().min(width as f32 - 1.0) as i64;
    let min_y = p0[1].min(p1[1]).min(p2[1]).floor().max(0.0) as i64;
    let max_y = p0[1].max(p1[1]).max(p2[1]).ceil().min(height as f32 - 1.0) as i64;
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
            let alpha = (v0.color[3] * w0 + v1.color[3] * w1 + v2.color[3] * w2)
                * sample_bilinear(texture, uv)[3];
            let index = y as usize * width as usize + x as usize;
            target[index] = target[index].max(alpha.clamp(0.0, 1.0));
            covered += 1;
        }
    }
    covered
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_over_is_stable() {
        let mut pixel = [0u8, 0, 0, 0];
        blend_source_over(&mut pixel, [1.0, 0.0, 0.0, 0.5]);
        assert_eq!(pixel, [255, 0, 0, 128]);
        blend_source_over(&mut pixel, [0.0, 0.0, 1.0, 0.5]);
        assert_eq!(pixel, [85, 0, 170, 192]);
    }
}
