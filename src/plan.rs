//! Render plan generation (M3 geometry bridge).
//! Algorithms and native semantics adapted from eluna (MPL-2.0,
//! https://github.com/xmoezzz/eluna). See NOTICE and LICENSE-MPL-2.0.
//!
//! Converts an evaluated [`Pose`] into textured mesh draw items that can be
//! uploaded to wgpu (or a software rasterizer) without exposing E-mote mesh
//! structures to `yu-gfx`.
//!
//! ## Evidence / open items
//!
//! - Atlas rectangles, pivots and page names come from official PSB data.
//! - Mesh tessellation uses the 4x4 cubic Bezier patch in [`crate::render`]
//!   (public-tooling behavior, pending official parity).
//! - `src == "blank"` layers are mask/deformation proxies, not color draws;
//!   they are skipped and counted.
//! - Composite masks are resolved by [`crate::soft`] from the plan's
//!   [`MaskGroup`] list; items carry `mask_group`, `stencil_type` and `mask`.
//! - Vertex color channel order is `0xRRGGBBAA` (low byte alpha), evidenced by
//!   the eyelid layer's opaque-black `0x000000FF`.

use std::collections::BTreeMap;

use serde::Serialize;

use crate::pose::{Affine2, LayerPose, MaskGroup, Pose};
use crate::render::MeshGrid;
use crate::schema::{AtlasIcon, IconRef, Model, TextureSource};

/// One vertex in model/pixel space.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct PlanVertex {
    /// Position in model pixels (already transformed by the layer world).
    pub position: [f32; 2],
    /// RGBA color (0..1), alpha already multiplied by layer opacity.
    pub color: [f32; 4],
    /// Normalized texture coordinates.
    pub uv: [f32; 2],
}

/// One textured mesh draw.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct DrawItem {
    /// Layer label.
    pub label: String,
    /// Structural path.
    pub path: String,
    /// Texture page name (`tex#0`, ...).
    pub page: String,
    /// Texture resource index.
    pub resource: Option<u32>,
    /// Page width in pixels.
    pub page_width: u32,
    /// Page height in pixels.
    pub page_height: u32,
    /// Blend mode (`bm`).
    pub blend_mode: u32,
    /// Whether the item uses a deformed mesh.
    pub mesh: bool,
    /// Tessellation density.
    pub mesh_division: u32,
    /// `stencilType` from the layer.
    pub stencil_type: i64,
    /// Type-7 clip rectangle in model space (`left, top, right, bottom`).
    pub clip_rect: Option<[f32; 4]>,
    /// Composite-mask group inherited from a type-12 owner.
    pub mask_group: Option<usize>,
    /// Motion scope used to resolve `stencilCompositeMaskLayerList` labels.
    pub scope: usize,
    /// Layer mask bits.
    pub mask: i64,
    /// Vertices.
    pub vertices: Vec<PlanVertex>,
    /// Triangle indices.
    pub indices: Vec<u32>,
}

/// Deterministic draw plan for one pose.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct RenderPlan {
    /// Model screen width.
    pub width: u32,
    /// Model screen height.
    pub height: u32,
    /// Draw items in traversal order.
    pub items: Vec<DrawItem>,
    /// Composite-mask owners reachable from this pose.
    pub mask_groups: Vec<MaskGroup>,
    /// Layers skipped, keyed by reason.
    pub skipped: BTreeMap<String, usize>,
}

impl RenderPlan {
    /// Build a plan from a model and pose.
    pub fn build(model: &Model, pose: &Pose) -> Self {
        let width = model.screen.width.max(1.0) as u32;
        let height = model.screen.height.max(1.0) as u32;
        let mut items = Vec::new();
        let mut skipped: BTreeMap<String, usize> = BTreeMap::new();

        for layer in pose.drawables() {
            match build_item(model, layer) {
                Ok(Some(item)) => items.push(item),
                Ok(None) => {}
                Err(reason) => *skipped.entry(reason.to_string()).or_default() += 1,
            }
        }

        Self {
            width,
            height,
            items,
            mask_groups: pose.mask_groups.clone(),
            skipped,
        }
    }

    /// Orthographic matrix mapping model pixels to clip space.
    ///
    /// Y is flipped so model Y grows downward while NDC Y grows upward.
    pub fn ortho(&self) -> [[f32; 4]; 4] {
        let w = self.width.max(1) as f32;
        let h = self.height.max(1) as f32;
        [
            [2.0 / w, 0.0, 0.0, 0.0],
            [0.0, -2.0 / h, 0.0, 0.0],
            [0.0, 0.0, 1.0, 0.0],
            [-1.0, 1.0, 0.0, 1.0],
        ]
    }

    /// Model-space bounds of all vertices, if any.
    pub fn bounds(&self) -> Option<([f32; 2], [f32; 2])> {
        let mut min = [f32::INFINITY; 2];
        let mut max = [f32::NEG_INFINITY; 2];
        let mut any = false;
        for item in &self.items {
            for vertex in &item.vertices {
                any = true;
                min[0] = min[0].min(vertex.position[0]);
                min[1] = min[1].min(vertex.position[1]);
                max[0] = max[0].max(vertex.position[0]);
                max[1] = max[1].max(vertex.position[1]);
            }
        }
        any.then_some((min, max))
    }

    /// Number of vertices across all items.
    pub fn vertex_count(&self) -> usize {
        self.items.iter().map(|item| item.vertices.len()).sum()
    }

    /// Number of triangles across all items.
    pub fn triangle_count(&self) -> usize {
        self.items.iter().map(|item| item.indices.len() / 3).sum()
    }

    /// Build a compact JSON summary (no vertex payloads).
    pub fn summary_json(&self) -> serde_json::Value {
        let items: Vec<_> = self
            .items
            .iter()
            .map(|item| {
                serde_json::json!({
                    "label": item.label,
                    "path": item.path,
                    "page": item.page,
                    "mesh": item.mesh,
                    "division": item.mesh_division,
                    "vertices": item.vertices.len(),
                    "triangles": item.indices.len() / 3,
                    "blend_mode": item.blend_mode,
                    "stencil_type": item.stencil_type,
                })
            })
            .collect();
        serde_json::json!({
            "width": self.width,
            "height": self.height,
            "items": items,
            "skipped": self.skipped,
            "vertex_count": self.vertex_count(),
            "triangle_count": self.triangle_count(),
        })
    }
}

fn build_item(model: &Model, layer: &LayerPose) -> Result<Option<DrawItem>, &'static str> {
    let src = layer.state.src.as_deref().ok_or("no-src")?;
    // `blank` layers are structural proxies (deformers, type-12 stencil
    // owners and mask placement nodes), not color geometry. Drawing them as
    // white quads covers the face and hair; the official renderer only uses
    // them while building stencil state.
    if src == "blank" {
        return Err("blank-proxy");
    }
    let page = model
        .sources
        .iter()
        .find(|page| page.name == src)
        .ok_or("missing-page")?;
    let icon_ref = layer.state.icon.as_ref().ok_or("no-icon")?;
    let (icon, _explicit) = resolve_icon(page, icon_ref).ok_or("missing-icon")?;
    if icon.width <= 0.0 || icon.height <= 0.0 {
        return Err("zero-size");
    }
    let width = icon.width as f32;
    let height = icon.height as f32;
    let origin_x = icon.origin_x as f32;
    let origin_y = icon.origin_y as f32;
    let page_width = page.width.max(1);
    let page_height = page.height.max(1);
    let world = layer.world;

    // The sprite patch is the accumulated ancestor shape-sync mesh plus the
    // layer's own `mesh.bp` (see `pose::combine_patch`). An identity patch is
    // just a quad, so skip tessellation to keep the triangle count bounded.
    let accumulated = layer.mesh.filter(|patch| !patch.is_identity(1.0e-6));
    let (vertices, indices, mesh) = if let Some(patch) = accumulated {
        let grid = MeshGrid::tessellate(
            &patch,
            width,
            height,
            patch.division.max(layer.mesh_division).max(1),
        );
        let vertices = grid
            .vertices
            .iter()
            .map(|vertex| PlanVertex {
                position: transform_point(world, [vertex.x - origin_x, vertex.y - origin_y]),
                color: mesh_color(layer, vertex.u, vertex.v),
                uv: [
                    (icon.left as f32 + vertex.u * width) / page_width as f32,
                    (icon.top as f32 + vertex.v * height) / page_height as f32,
                ],
            })
            .collect();
        (vertices, grid.indices, true)
    } else {
        let corners = [
            [0.0 - origin_x, 0.0 - origin_y],
            [width - origin_x, 0.0 - origin_y],
            [0.0 - origin_x, height - origin_y],
            [width - origin_x, height - origin_y],
        ];
        let uvs = [
            [
                icon.left as f32 / page_width as f32,
                icon.top as f32 / page_height as f32,
            ],
            [
                (icon.left as f32 + width) / page_width as f32,
                icon.top as f32 / page_height as f32,
            ],
            [
                icon.left as f32 / page_width as f32,
                (icon.top as f32 + height) / page_height as f32,
            ],
            [
                (icon.left as f32 + width) / page_width as f32,
                (icon.top as f32 + height) / page_height as f32,
            ],
        ];
        let vertices = corners
            .iter()
            .zip(uvs.iter())
            .enumerate()
            .map(|(index, (position, uv))| PlanVertex {
                position: transform_point(world, *position),
                color: corner_color(layer, index),
                uv: *uv,
            })
            .collect();
        (vertices, vec![0, 1, 2, 1, 3, 2], false)
    };

    Ok(Some(DrawItem {
        label: layer.label.clone(),
        path: layer.path.clone(),
        page: src.to_string(),
        resource: page.resource,
        page_width,
        page_height,
        blend_mode: layer.state.blend_mode,
        mesh,
        mesh_division: layer.mesh_division,
        stencil_type: layer.stencil_type,
        clip_rect: layer.clip_rect.map(|rect| {
            [
                rect[0] as f32,
                rect[1] as f32,
                rect[2] as f32,
                rect[3] as f32,
            ]
        }),
        mask_group: layer.mask_group,
        scope: layer.scope,
        mask: layer.state.mask,
        vertices,
        indices,
    }))
}

fn resolve_icon(page: &TextureSource, icon_ref: &IconRef) -> Option<(AtlasIcon, bool)> {
    if let Some(id) = icon_ref.id {
        return page.icons.get(&id).cloned().map(|icon| (icon, false));
    }
    let rect = icon_ref.rect?;
    Some((
        AtlasIcon {
            attr: None,
            left: rect[0],
            top: rect[1],
            width: rect[2],
            height: rect[3],
            origin_x: 0.0,
            origin_y: 0.0,
        },
        true,
    ))
}

fn corner_color(layer: &LayerPose, index: usize) -> [f32; 4] {
    // Official frame colors are four corner colors stored as 0xRRGGBBAA
    // (eluna's `corner_colors` byte order); alpha is the low byte and is
    // multiplied by the accumulated layer opacity.
    let rgba = layer.state.color[index.min(3)];
    let r = ((rgba >> 24) & 0xFF) as f32 / 255.0;
    let g = ((rgba >> 16) & 0xFF) as f32 / 255.0;
    let b = ((rgba >> 8) & 0xFF) as f32 / 255.0;
    let a = (rgba & 0xFF) as f32 / 255.0 * layer.opacity as f32;
    [r, g, b, a]
}

fn mesh_color(layer: &LayerPose, u: f32, v: f32) -> [f32; 4] {
    // Bilinear over the four authored corner colors (top-left, top-right,
    // bottom-left, bottom-right).
    let c00 = corner_color(layer, 0);
    let c10 = corner_color(layer, 1);
    let c01 = corner_color(layer, 2);
    let c11 = corner_color(layer, 3);
    let mut out = [0.0f32; 4];
    for channel in 0..4 {
        let top = c00[channel] + (c10[channel] - c00[channel]) * u;
        let bottom = c01[channel] + (c11[channel] - c01[channel]) * u;
        out[channel] = top + (bottom - top) * v;
    }
    out
}

fn transform_point(world: Affine2, point: [f32; 2]) -> [f32; 2] {
    let out = world.apply([point[0] as f64, point[1] as f64]);
    [out[0] as f32, out[1] as f32]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn color_is_rgba() {
        let layer = LayerPose {
            label: String::new(),
            path: String::new(),
            kind: 0,
            sample_time: 0.0,
            state: crate::pose::FrameState {
                color: [0x80FF_8040; 4],
                ..Default::default()
            },
            local: Affine2::identity(),
            world: Affine2::identity(),
            world_origin: [0.0; 3],
            opacity: 1.0,
            visible: true,
            drawable: true,
            mesh_division: 1,
            mesh_transform: 0,
            mesh_combine: false,
            stencil_type: 0,
            clip_rect: None,
            anchor_mode: 0,
            mask_group: None,
            scope: 0,
            mesh: None,
            priority_path: Vec::new(),
            children: Vec::new(),
        };
        let color = corner_color(&layer, 0);
        assert!((color[0] - (128.0 / 255.0)).abs() < 1e-6);
        assert!((color[1] - 1.0).abs() < 1e-6);
        assert!((color[2] - (128.0 / 255.0)).abs() < 1e-6);
        assert!((color[3] - (64.0 / 255.0)).abs() < 1e-6);
    }
}
