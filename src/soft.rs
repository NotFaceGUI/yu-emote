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
use std::sync::atomic::{AtomicUsize, Ordering};

use serde::Serialize;

use crate::error::EmoteError;
use crate::plan::{DrawItem, PlanVertex, RenderPlan};
use crate::texture::DecodedTexture;

/// Rasterizer worker count. `0` selects the available CPU parallelism.
static THREAD_COUNT: AtomicUsize = AtomicUsize::new(0);

/// Set the rasterizer worker count. `0` = auto.
pub fn set_thread_count(count: usize) {
    THREAD_COUNT.store(count, Ordering::Relaxed);
}

/// Configured rasterizer worker count (`0` = automatic).
pub fn thread_count() -> usize {
    THREAD_COUNT.load(Ordering::Relaxed)
}

/// Worker count actually used for a canvas.
///
/// Small canvases are dominated by per-worker setup, so keep the band count
/// low; larger canvases benefit from more bands.
pub fn effective_threads(_width: u32, _height: u32) -> usize {
    let configured = THREAD_COUNT.load(Ordering::Relaxed);
    if configured > 0 {
        return configured.clamp(1, 64);
    }
    std::thread::available_parallelism()
        .map(|count| count.get().clamp(1, 16))
        .unwrap_or(1)
}

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
///
/// Work is split into horizontal bands. Each band owns its pixels for the
/// whole frame, so draw order and blending stay identical to the single-thread
/// path; bands only change which pixels a worker touches.
pub fn render_with_textures(
    plan: &RenderPlan,
    textures: &BTreeMap<u32, DecodedTexture>,
    view: ViewTransform,
) -> Result<(RenderImage, SoftStats), EmoteError> {
    let width = plan.width.max(1);
    let height = plan.height.max(1);
    let mut rgba8 = vec![0u8; width as usize * height as usize * 4];
    let mut stats = collect_stats(plan, textures);

    let workers = effective_threads(width, height);
    let ranges = band_ranges(height, workers);
    let bands = build_band_plans(plan, textures, view, width, height, &ranges);
    let source_groups = build_source_groups(plan);
    let source_groups: &[Vec<usize>] = &source_groups;

    let mut slices: Vec<&mut [u8]> = Vec::with_capacity(ranges.len());
    let mut rest: &mut [u8] = &mut rgba8;
    for &(y0, y1) in &ranges {
        let len = (y1 - y0) as usize * width as usize * 4;
        let (head, tail) = rest.split_at_mut(len);
        slices.push(head);
        rest = tail;
    }

    let covered: Vec<usize> = if slices.len() == 1 {
        let band = bands.into_iter().next().expect("one band");
        vec![render_band(
            slices.into_iter().next().expect("one slice"),
            ranges[0].0,
            ranges[0].1,
            plan,
            textures,
            view,
            width,
            height,
            source_groups,
            &band,
        )]
    } else {
        std::thread::scope(|scope| {
            let mut handles = Vec::with_capacity(slices.len());
            for ((slice, band), &(y0, y1)) in slices.into_iter().zip(bands).zip(ranges.iter()) {
                handles.push(scope.spawn(move || {
                    render_band(
                        slice,
                        y0,
                        y1,
                        plan,
                        textures,
                        view,
                        width,
                        height,
                        source_groups,
                        &band,
                    )
                }));
            }
            handles
                .into_iter()
                .map(|handle| handle.join().expect("raster worker"))
                .collect()
        })
    };
    stats.covered = covered.into_iter().sum();

    Ok((
        RenderImage {
            width,
            height,
            rgba8,
        },
        stats,
    ))
}

fn collect_stats(plan: &RenderPlan, textures: &BTreeMap<u32, DecodedTexture>) -> SoftStats {
    let mut stats = SoftStats::default();
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
        match item.resource {
            None => stats.feedback += 1,
            Some(_) => {
                if item_texture(item, textures).is_some() {
                    stats.triangles += item.indices.len() / 3;
                } else {
                    stats.missing_textures += 1;
                }
            }
        }
    }
    for group in &plan.mask_groups {
        for &source in &group.sources {
            if let Some(item) = plan.items.get(source) {
                if item_texture(item, textures).is_some() {
                    stats.mask_sources += 1;
                }
            }
        }
    }
    stats
}

fn band_ranges(height: u32, workers: usize) -> Vec<(u32, u32)> {
    let height = height.max(1);
    let workers = workers.clamp(1, 64).min(height as usize);
    let base = height / workers as u32;
    let extra = height % workers as u32;
    let mut ranges = Vec::with_capacity(workers);
    let mut y = 0u32;
    for index in 0..workers as u32 {
        let band_height = base + u32::from(index < extra);
        ranges.push((y, y + band_height));
        y += band_height;
    }
    ranges
}

#[derive(Default, Clone)]
struct BandPlan {
    color: Vec<usize>,
    mask: Vec<usize>,
    groups: Vec<usize>,
}

fn build_band_plans(
    plan: &RenderPlan,
    textures: &BTreeMap<u32, DecodedTexture>,
    view: ViewTransform,
    width: u32,
    height: u32,
    ranges: &[(u32, u32)],
) -> Vec<BandPlan> {
    let mut bands = vec![BandPlan::default(); ranges.len()];
    for (index, item) in plan.items.iter().enumerate() {
        if item_texture(item, textures).is_none() {
            continue;
        }
        let Some((min_y, max_y)) = item_screen_y(item, view, width, height) else {
            continue;
        };
        let is_color = color_item(item);
        for (band_index, &(y0, y1)) in ranges.iter().enumerate() {
            if max_y < y0 as f32 || min_y >= y1 as f32 {
                continue;
            }
            if is_color {
                bands[band_index].color.push(index);
                for &group in &item.mask_groups {
                    if !bands[band_index].groups.contains(&group) {
                        bands[band_index].groups.push(group);
                    }
                }
            } else {
                bands[band_index].mask.push(index);
            }
        }
    }
    bands
}

fn build_source_groups(plan: &RenderPlan) -> Vec<Vec<usize>> {
    let mut source_groups = vec![Vec::new(); plan.items.len()];
    for (group_index, group) in plan.mask_groups.iter().enumerate() {
        for &source in &group.sources {
            if let Some(groups) = source_groups.get_mut(source) {
                groups.push(group_index);
            }
        }
    }
    source_groups
}

fn item_screen_y(
    item: &DrawItem,
    view: ViewTransform,
    width: u32,
    height: u32,
) -> Option<(f32, f32)> {
    let (local_min, local_max) = item.bounds?;
    let a = view.apply(local_min);
    let b = view.apply(local_max);
    let min_x = a[0].min(b[0]);
    let max_x = a[0].max(b[0]);
    let min_y = a[1].min(b[1]);
    let max_y = a[1].max(b[1]);
    let min_x = min_x.max(0.0);
    let max_x = max_x.min(width as f32 - 1.0);
    let min_y = min_y.max(0.0);
    let max_y = max_y.min(height as f32 - 1.0);
    (max_x >= min_x && max_y >= min_y).then_some((min_y, max_y))
}

#[allow(clippy::too_many_arguments)]
fn render_band(
    target: &mut [u8],
    y0: u32,
    y1: u32,
    plan: &RenderPlan,
    textures: &BTreeMap<u32, DecodedTexture>,
    view: ViewTransform,
    width: u32,
    height: u32,
    source_groups: &[Vec<usize>],
    band: &BandPlan,
) -> usize {
    let band_height = (y1 - y0) as usize;
    let mut masks: Vec<Option<Vec<f32>>> = vec![None; plan.mask_groups.len()];
    for &group in &band.groups {
        let mut buffer = vec![0.0f32; width as usize * band_height];
        for &item_index in &band.mask {
            if !source_groups
                .get(item_index)
                .is_some_and(|groups| groups.contains(&group))
            {
                continue;
            }
            let item = &plan.items[item_index];
            let Some(texture) = item_texture(item, textures) else {
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
                rasterize_coverage_band(
                    &mut buffer,
                    y0,
                    y1,
                    width,
                    texture,
                    v0,
                    v1,
                    v2,
                    view,
                    clip,
                );
            }
        }
        masks[group] = Some(buffer);
    }

    let mut covered = 0usize;
    for &item_index in &band.color {
        let item = &plan.items[item_index];
        let Some(texture) = item_texture(item, textures) else {
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
            covered += rasterize_triangle_band(
                target,
                y0,
                y1,
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
    covered
}

fn item_texture<'a>(
    item: &DrawItem,
    textures: &'a BTreeMap<u32, DecodedTexture>,
) -> Option<&'a DecodedTexture> {
    item.resource.and_then(|resource| textures.get(&resource))
}

/// Edge coefficients for `edge(a, b, point)`: `A*x + B*y + C`.
#[inline]
fn edge_plane(a: [f32; 2], b: [f32; 2]) -> (f32, f32, f32) {
    let bx = b[0] - a[0];
    let by = b[1] - a[1];
    (-by, bx, by * a[0] - bx * a[1])
}

#[allow(clippy::too_many_arguments)]
fn rasterize_triangle_band(
    target: &mut [u8],
    band_y0: u32,
    band_y1: u32,
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
    let min_y = min_y.max(band_y0 as f32) as i64;
    let max_y = max_y.min(band_y1 as f32 - 1.0).min(height as f32 - 1.0) as i64;
    if max_x < min_x || max_y < min_y {
        return 0;
    }
    let area = edge(p0, p1, p2);
    if area.abs() < 1e-9 {
        return 0;
    }
    let inv_area = 1.0 / area;
    let (a0, b0, c0) = edge_plane(p1, p2);
    let (a1, b1, c1) = edge_plane(p2, p0);
    let (a2, b2, c2) = edge_plane(p0, p1);
    let a0 = a0 * inv_area;
    let b0 = b0 * inv_area;
    let c0 = c0 * inv_area;
    let a1 = a1 * inv_area;
    let b1 = b1 * inv_area;
    let c1 = c1 * inv_area;
    let a2 = a2 * inv_area;
    let b2 = b2 * inv_area;
    let c2 = c2 * inv_area;

    let x_start = min_x as f32 + 0.5;
    let du_dx = v0.uv[0] * a0 + v1.uv[0] * a1 + v2.uv[0] * a2;
    let dv_dx = v0.uv[1] * a0 + v1.uv[1] * a1 + v2.uv[1] * a2;
    let dr_dx = v0.color[0] * a0 + v1.color[0] * a1 + v2.color[0] * a2;
    let dg_dx = v0.color[1] * a0 + v1.color[1] * a1 + v2.color[1] * a2;
    let db_dx = v0.color[2] * a0 + v1.color[2] * a1 + v2.color[2] * a2;
    let da_dx = v0.color[3] * a0 + v1.color[3] * a1 + v2.color[3] * a2;

    let low = blend_mode & 0xFF0F;
    let modulate2x = (blend_mode & 0xF0) == 0x10;
    let mut covered = 0usize;
    for y in min_y..=max_y {
        let yf = y as f32 + 0.5;
        let mut row_w0 = a0 * x_start + b0 * yf + c0;
        let mut row_w1 = a1 * x_start + b1 * yf + c1;
        let mut row_w2 = a2 * x_start + b2 * yf + c2;
        let mut row_u = v0.uv[0] * row_w0 + v1.uv[0] * row_w1 + v2.uv[0] * row_w2;
        let mut row_v = v0.uv[1] * row_w0 + v1.uv[1] * row_w1 + v2.uv[1] * row_w2;
        let mut row_r = v0.color[0] * row_w0 + v1.color[0] * row_w1 + v2.color[0] * row_w2;
        let mut row_g = v0.color[1] * row_w0 + v1.color[1] * row_w1 + v2.color[1] * row_w2;
        let mut row_b = v0.color[2] * row_w0 + v1.color[2] * row_w1 + v2.color[2] * row_w2;
        let mut row_a = v0.color[3] * row_w0 + v1.color[3] * row_w1 + v2.color[3] * row_w2;
        for x in min_x..=max_x {
            let ww0 = row_w0;
            let ww1 = row_w1;
            let ww2 = row_w2;
            row_w0 += a0;
            row_w1 += a1;
            row_w2 += a2;
            if ww0 < -1e-6 || ww1 < -1e-6 || ww2 < -1e-6 {
                row_u += du_dx;
                row_v += dv_dx;
                row_r += dr_dx;
                row_g += dg_dx;
                row_b += db_dx;
                row_a += da_dx;
                continue;
            }
            let local = (y - band_y0 as i64) as usize * width as usize + x as usize;
            let mut alpha_scale = 1.0f32;
            for group in mask_groups {
                let Some(Some(mask)) = masks.get(*group) else {
                    alpha_scale = 0.0;
                    break;
                };
                let coverage = mask[local];
                if coverage <= 0.003 {
                    alpha_scale = 0.0;
                    break;
                }
                alpha_scale *= coverage;
            }
            if alpha_scale <= 0.0 {
                row_u += du_dx;
                row_v += dv_dx;
                row_r += dr_dx;
                row_g += dg_dx;
                row_b += db_dx;
                row_a += da_dx;
                continue;
            }
            let texel = sample_bilinear(texture, [row_u, row_v]);
            let mut alpha = row_a * alpha_scale;
            if wipe[0] > 0.5 {
                alpha = (alpha * wipe[1] + wipe[2]).clamp(0.0, 1.0);
            }
            let mut src = [
                texel[0] * row_r,
                texel[1] * row_g,
                texel[2] * row_b,
                texel[3] * alpha,
            ];
            if modulate2x {
                src[0] = (src[0] * 2.0).min(1.0);
                src[1] = (src[1] * 2.0).min(1.0);
                src[2] = (src[2] * 2.0).min(1.0);
            }
            if low == 3 || low == 4 {
                src[0] *= src[3];
                src[1] *= src[3];
                src[2] *= src[3];
            } else if low == 5 {
                src[0] = 1.0 - src[0];
                src[1] = 1.0 - src[1];
                src[2] = 1.0 - src[2];
            }
            let pixel = &mut target[local * 4..local * 4 + 4];
            if low == 0 {
                blend_source_over(pixel, src);
            } else {
                blend_pixel(pixel, src, low);
            }
            covered += 1;
            row_u += du_dx;
            row_v += dv_dx;
            row_r += dr_dx;
            row_g += dg_dx;
            row_b += db_dx;
            row_a += da_dx;
        }
    }
    covered
}

#[allow(clippy::too_many_arguments)]
fn rasterize_coverage_band(
    target: &mut [f32],
    band_y0: u32,
    band_y1: u32,
    width: u32,
    texture: &DecodedTexture,
    v0: &PlanVertex,
    v1: &PlanVertex,
    v2: &PlanVertex,
    view: ViewTransform,
    clip: Option<[f32; 4]>,
) {
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
    let min_y = min_y.max(band_y0 as f32) as i64;
    let max_y = max_y.min(band_y1 as f32 - 1.0) as i64;
    if max_x < min_x || max_y < min_y {
        return;
    }
    let area = edge(p0, p1, p2);
    if area.abs() < 1e-9 {
        return;
    }
    let inv_area = 1.0 / area;
    let (a0, b0, c0) = edge_plane(p1, p2);
    let (a1, b1, c1) = edge_plane(p2, p0);
    let (a2, b2, c2) = edge_plane(p0, p1);
    let a0 = a0 * inv_area;
    let b0 = b0 * inv_area;
    let c0 = c0 * inv_area;
    let a1 = a1 * inv_area;
    let b1 = b1 * inv_area;
    let c1 = c1 * inv_area;
    let a2 = a2 * inv_area;
    let b2 = b2 * inv_area;
    let c2 = c2 * inv_area;

    let x_start = min_x as f32 + 0.5;
    let du_dx = v0.uv[0] * a0 + v1.uv[0] * a1 + v2.uv[0] * a2;
    let dv_dx = v0.uv[1] * a0 + v1.uv[1] * a1 + v2.uv[1] * a2;
    let da_dx = v0.color[3] * a0 + v1.color[3] * a1 + v2.color[3] * a2;

    for y in min_y..=max_y {
        let yf = y as f32 + 0.5;
        let mut row_w0 = a0 * x_start + b0 * yf + c0;
        let mut row_w1 = a1 * x_start + b1 * yf + c1;
        let mut row_w2 = a2 * x_start + b2 * yf + c2;
        let mut row_u = v0.uv[0] * row_w0 + v1.uv[0] * row_w1 + v2.uv[0] * row_w2;
        let mut row_v = v0.uv[1] * row_w0 + v1.uv[1] * row_w1 + v2.uv[1] * row_w2;
        let mut row_a = v0.color[3] * row_w0 + v1.color[3] * row_w1 + v2.color[3] * row_w2;
        for x in min_x..=max_x {
            let ww0 = row_w0;
            let ww1 = row_w1;
            let ww2 = row_w2;
            row_w0 += a0;
            row_w1 += a1;
            row_w2 += a2;
            if ww0 < -1e-6 || ww1 < -1e-6 || ww2 < -1e-6 {
                row_u += du_dx;
                row_v += dv_dx;
                row_a += da_dx;
                continue;
            }
            let texel = sample_bilinear_alpha(texture, [row_u, row_v]);
            let value = (row_a * texel).clamp(0.0, 1.0);
            let local = (y - band_y0 as i64) as usize * width as usize + x as usize;
            if value > target[local] {
                target[local] = value;
            }
            row_u += du_dx;
            row_v += dv_dx;
            row_a += da_dx;
        }
    }
}

fn edge(a: [f32; 2], b: [f32; 2], c: [f32; 2]) -> f32 {
    (b[0] - a[0]) * (c[1] - a[1]) - (b[1] - a[1]) * (c[0] - a[0])
}

#[inline]
fn sample_bilinear(texture: &DecodedTexture, uv: [f32; 2]) -> [f32; 4] {
    let width = texture.width as i64;
    let height = texture.height as i64;
    let x = uv[0] * width as f32 - 0.5;
    let y = uv[1] * height as f32 - 0.5;
    let x0 = x.floor();
    let y0 = y.floor();
    let fx = x - x0;
    let fy = y - y0;
    let xi = x0 as i64;
    let yi = y0 as i64;
    let x0c = xi.clamp(0, width - 1) as usize;
    let x1c = (xi + 1).clamp(0, width - 1) as usize;
    let y0c = yi.clamp(0, height - 1) as usize;
    let y1c = (yi + 1).clamp(0, height - 1) as usize;
    let pitch = width as usize * 4;
    let p00 = &texture.rgba8[y0c * pitch + x0c * 4..][..4];
    let p10 = &texture.rgba8[y0c * pitch + x1c * 4..][..4];
    let p01 = &texture.rgba8[y1c * pitch + x0c * 4..][..4];
    let p11 = &texture.rgba8[y1c * pitch + x1c * 4..][..4];
    let mut out = [0.0f32; 4];
    for channel in 0..4 {
        let c00 = p00[channel] as f32;
        let c10 = p10[channel] as f32;
        let c01 = p01[channel] as f32;
        let c11 = p11[channel] as f32;
        let top = c00 + (c10 - c00) * fx;
        let bottom = c01 + (c11 - c01) * fx;
        out[channel] = (top + (bottom - top) * fy) * (1.0 / 255.0);
    }
    out
}

#[inline]
fn sample_bilinear_alpha(texture: &DecodedTexture, uv: [f32; 2]) -> f32 {
    let width = texture.width as i64;
    let height = texture.height as i64;
    let x = uv[0] * width as f32 - 0.5;
    let y = uv[1] * height as f32 - 0.5;
    let x0 = x.floor();
    let y0 = y.floor();
    let fx = x - x0;
    let fy = y - y0;
    let xi = x0 as i64;
    let yi = y0 as i64;
    let x0c = xi.clamp(0, width - 1) as usize;
    let x1c = (xi + 1).clamp(0, width - 1) as usize;
    let y0c = yi.clamp(0, height - 1) as usize;
    let y1c = (yi + 1).clamp(0, height - 1) as usize;
    let pitch = width as usize * 4;
    let a00 = texture.rgba8[y0c * pitch + x0c * 4 + 3] as f32;
    let a10 = texture.rgba8[y0c * pitch + x1c * 4 + 3] as f32;
    let a01 = texture.rgba8[y1c * pitch + x0c * 4 + 3] as f32;
    let a11 = texture.rgba8[y1c * pitch + x1c * 4 + 3] as f32;
    let top = a00 + (a10 - a00) * fx;
    let bottom = a01 + (a11 - a01) * fx;
    (top + (bottom - top) * fy) * (1.0 / 255.0)
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

#[inline]
fn blend_source_over(dst: &mut [u8], src: [f32; 4]) {
    let sa = src[3].clamp(0.0, 1.0);
    if sa <= 0.0 {
        return;
    }
    let da = dst[3];
    if da == 0 {
        dst[0] = quantize(src[0]);
        dst[1] = quantize(src[1]);
        dst[2] = quantize(src[2]);
        dst[3] = quantize(sa);
        return;
    }
    if da == 255 {
        let inv = 1.0 - sa;
        dst[0] = quantize(src[0] * sa + dst[0] as f32 * (1.0 / 255.0) * inv);
        dst[1] = quantize(src[1] * sa + dst[1] as f32 * (1.0 / 255.0) * inv);
        dst[2] = quantize(src[2] * sa + dst[2] as f32 * (1.0 / 255.0) * inv);
        dst[3] = 255;
        return;
    }
    let daf = da as f32 * (1.0 / 255.0);
    let out_a = sa + daf * (1.0 - sa);
    let inv_out = 1.0 / out_a;
    for channel in 0..3 {
        let s = src[channel].clamp(0.0, 1.0);
        let d = dst[channel] as f32 * (1.0 / 255.0);
        dst[channel] = quantize((s * sa + d * daf * (1.0 - sa)) * inv_out);
    }
    dst[3] = quantize(out_a);
}

#[inline]
fn quantize(value: f32) -> u8 {
    (value.clamp(0.0, 1.0) * 255.0).round() as u8
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
