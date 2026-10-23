//! Draw-plan generation from an evaluated `eluna` scene.
//!
//! The scene itself (motion selection, timeline state, mesh-chain deformation,
//! physics, anchors, clips, joins) is produced by the vendored `eluna` runtime.
//! This module only turns its sprites into textured triangles, using the same
//! vertex math as the reference `eluna_player` renderer:
//!
//! - local rectangle -> center-relative scale -> rotation -> world transform;
//! - 4x4 Bezier patch sampling for deformed meshes;
//! - four corner colors in `0xRRGGBBAA` order, alpha multiplied by opacity;
//! - native draw order (the scene already emits `DrawFrameInfo` order).
//!
//! Stencil/mask owners are resolved from eluna's structural
//! `native_draw_key` chain. Inner masks are unioned into per-owner coverage
//! buffers by [`crate::soft`]; outer masks are reported as skipped because the
//! CPU rasterizer has no stencil reference pass yet.

use std::collections::BTreeMap;

use serde::Serialize;

use eluna::{
    EmoteDrawFrameInfo, EmoteDrawPass, EmoteMeshPatch, EmoteStaticScene, EmoteStaticSprite,
};

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
    /// Blend mode (`bm`), including the MODULATE2X high nibble.
    pub blend_mode: u32,
    /// Native draw pass.
    #[serde(skip)]
    pub pass: EmoteDrawPass,
    /// Whether the item uses a deformed mesh.
    pub mesh: bool,
    /// Tessellation density (0 for a plain quad).
    pub mesh_division: u32,
    /// `stencilType` from the layer.
    pub stencil_type: i64,
    /// Type-7 clip rectangle in model space (`left, top, right, bottom`).
    pub clip_rect: Option<[f32; 4]>,
    /// Inner stencil groups that must be satisfied before this item is drawn.
    pub mask_groups: Vec<usize>,
    /// Type-12 wipe parameters (`enabled`, `scale`, `bias`).
    pub wipe: [f32; 3],
    /// Vertices.
    pub vertices: Vec<PlanVertex>,
    /// Triangle indices.
    pub indices: Vec<u32>,
    /// Model-space vertex bounds (`min`, `max`), if any.
    pub bounds: Option<([f32; 2], [f32; 2])>,
}

/// Coverage sources for one inner stencil owner.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct MaskGroup {
    /// Source item indices in [`RenderPlan::items`].
    pub sources: Vec<usize>,
    /// Structural owner key (diagnostics only).
    pub owner: Vec<u64>,
}

/// Deterministic draw plan for one scene.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct RenderPlan {
    /// Output canvas width.
    pub width: u32,
    /// Output canvas height.
    pub height: u32,
    /// Draw items in native emission order.
    pub items: Vec<DrawItem>,
    /// Inner stencil owners reachable from this scene.
    pub mask_groups: Vec<MaskGroup>,
    /// Sprites skipped, keyed by reason.
    pub skipped: BTreeMap<String, usize>,
}

impl RenderPlan {
    /// Build a plan from an evaluated `eluna` scene.
    ///
    /// `canvas` is the output size used by [`crate::soft::ViewTransform::fit`].
    pub fn from_scene(scene: &EmoteStaticScene, canvas: [u32; 2]) -> Self {
        let mut skipped: BTreeMap<String, usize> = BTreeMap::new();
        let visible: Vec<&EmoteStaticSprite> = scene
            .sprites
            .iter()
            .filter(|sprite| {
                if !sprite.visible {
                    *skipped.entry("invisible".to_owned()).or_default() += 1;
                    return false;
                }
                if sprite.opacity <= 0.0 {
                    *skipped.entry("zero-opacity".to_owned()).or_default() += 1;
                    return false;
                }
                if sprite.width <= 0.0 || sprite.height <= 0.0 {
                    *skipped.entry("zero-size".to_owned()).or_default() += 1;
                    return false;
                }
                true
            })
            .collect();

        let mut items = build_items(&visible);

        let mut key_to_item = BTreeMap::<Vec<u64>, usize>::new();
        for (index, sprite) in visible.iter().enumerate() {
            key_to_item.insert(sprite.draw_frame_info.native_draw_key.clone(), index);
        }
        let layer_infos: BTreeMap<Vec<u64>, &EmoteDrawFrameInfo> = scene
            .layer_states
            .iter()
            .map(|state| {
                (
                    state.draw_frame_info.native_draw_key.clone(),
                    &state.draw_frame_info,
                )
            })
            .collect();

        let mut mask_groups: Vec<MaskGroup> = Vec::new();
        let mut group_by_owner = BTreeMap::<Vec<u64>, usize>::new();
        for (index, sprite) in visible.iter().enumerate() {
            if matches!(
                sprite.draw_frame_info.pass,
                EmoteDrawPass::MaskGeneration | EmoteDrawPass::StencilCompositeMask
            ) {
                *skipped.entry("stencil-source".to_owned()).or_default() += 1;
                continue;
            }
            let mut cursor = sprite.draw_frame_info.stencil_parent_native_key.as_ref();
            let mut visited = std::collections::BTreeSet::<Vec<u64>>::new();
            while let Some(key) = cursor {
                if !visited.insert(key.clone()) {
                    break;
                }
                let Some(info) = layer_infos.get(key).copied() else {
                    break;
                };
                if info.stencil_phase == 1 {
                    let group = *group_by_owner.entry(key.clone()).or_insert_with(|| {
                        let source_keys: Vec<Vec<u64>> = if (info.stencil_type & 4) != 0 {
                            scene
                                .composite_mask_sources_by_key
                                .get(key)
                                .cloned()
                                .unwrap_or_default()
                        } else {
                            vec![key.clone()]
                        };
                        let sources = source_keys
                            .iter()
                            .filter_map(|source| key_to_item.get(source).copied())
                            .collect::<Vec<_>>();
                        mask_groups.push(MaskGroup {
                            sources,
                            owner: key.clone(),
                        });
                        mask_groups.len() - 1
                    });
                    if !items[index].mask_groups.contains(&group) {
                        items[index].mask_groups.push(group);
                    }
                } else if info.stencil_phase == 2 {
                    *skipped.entry("outer-stencil".to_owned()).or_default() += 1;
                }
                cursor = info.stencil_parent_native_key.as_ref();
            }
        }

        Self {
            width: canvas[0].max(1),
            height: canvas[1].max(1),
            items,
            mask_groups,
            skipped,
        }
    }

    /// Model-space bounds of all vertices, if any.
    pub fn bounds(&self) -> Option<([f32; 2], [f32; 2])> {
        let mut min = [f32::INFINITY; 2];
        let mut max = [f32::NEG_INFINITY; 2];
        let mut any = false;
        for item in &self.items {
            if matches!(
                item.pass,
                EmoteDrawPass::MaskGeneration | EmoteDrawPass::StencilCompositeMask
            ) {
                continue;
            }
            if let Some((item_min, item_max)) = item.bounds {
                any = true;
                min[0] = min[0].min(item_min[0]);
                min[1] = min[1].min(item_min[1]);
                max[0] = max[0].max(item_max[0]);
                max[1] = max[1].max(item_max[1]);
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
                    "resource": item.resource,
                    "mesh": item.mesh,
                    "division": item.mesh_division,
                    "vertices": item.vertices.len(),
                    "triangles": item.indices.len() / 3,
                    "blend_mode": item.blend_mode,
                    "stencil_type": item.stencil_type,
                    "pass": format!("{:?}", item.pass),
                    "masks": item.mask_groups.len(),
                    "bbox": item.bounds.map(|(min, max)| [min, max]),
                })
            })
            .collect();
        serde_json::json!({
            "width": self.width,
            "height": self.height,
            "items": items,
            "skipped": self.skipped,
            "mask_groups": self.mask_groups.len(),
            "vertex_count": self.vertex_count(),
            "triangle_count": self.triangle_count(),
        })
    }
}

fn build_items(visible: &[&EmoteStaticSprite]) -> Vec<DrawItem> {
    let workers = std::thread::available_parallelism()
        .map(|count| count.get().clamp(1, 4))
        .unwrap_or(1);
    if workers <= 1 || visible.len() < 8 {
        return visible.iter().map(|sprite| build_item(sprite)).collect();
    }
    let chunk = visible.len().div_ceil(workers);
    std::thread::scope(|scope| {
        let mut handles = Vec::new();
        for slice in visible.chunks(chunk) {
            handles.push(scope.spawn(move || {
                slice
                    .iter()
                    .map(|sprite| build_item(sprite))
                    .collect::<Vec<_>>()
            }));
        }
        let mut items = Vec::with_capacity(visible.len());
        for handle in handles {
            items.extend(handle.join().expect("plan worker"));
        }
        items
    })
}

fn build_item(sprite: &EmoteStaticSprite) -> DrawItem {
    let (vertices, indices, mesh_division) = sprite_vertices(sprite);
    DrawItem {
        label: sprite
            .label
            .clone()
            .or_else(|| sprite.draw_frame_info.layer_label.clone())
            .unwrap_or_default(),
        path: sprite.draw_frame_info.path.clone(),
        page: if sprite.texture_name.is_empty() {
            format!("tex#{}", sprite.texture_resource_index)
        } else {
            sprite.texture_name.clone()
        },
        resource: (!sprite.feedback_history).then_some(sprite.texture_resource_index),
        page_width: sprite.texture_width.max(1),
        page_height: sprite.texture_height.max(1),
        blend_mode: sprite.blend_mode,
        pass: sprite.draw_frame_info.pass,
        mesh: sprite.mesh.is_some(),
        mesh_division,
        stencil_type: sprite.draw_frame_info.stencil_type,
        clip_rect: sprite.draw_frame_info.clip_rect,
        mask_groups: Vec::new(),
        wipe: [
            if sprite.draw_frame_info.stencil_wipe_enabled {
                1.0
            } else {
                0.0
            },
            sprite.draw_frame_info.stencil_wipe_scale,
            sprite.draw_frame_info.stencil_wipe_bias,
        ],
        bounds: vertex_bounds(&vertices),
        vertices,
        indices,
    }
}

fn vertex_bounds(vertices: &[PlanVertex]) -> Option<([f32; 2], [f32; 2])> {
    let mut min = [f32::INFINITY; 2];
    let mut max = [f32::NEG_INFINITY; 2];
    for vertex in vertices {
        min[0] = min[0].min(vertex.position[0]);
        min[1] = min[1].min(vertex.position[1]);
        max[0] = max[0].max(vertex.position[0]);
        max[1] = max[1].max(vertex.position[1]);
    }
    min[0].is_finite().then_some((min, max))
}

fn sprite_vertices(sprite: &EmoteStaticSprite) -> (Vec<PlanVertex>, Vec<u32>, u32) {
    let corner_colors = [
        native_corner_color(sprite, sprite.corner_colors[0]),
        native_corner_color(sprite, sprite.corner_colors[1]),
        native_corner_color(sprite, sprite.corner_colors[2]),
        native_corner_color(sprite, sprite.corner_colors[3]),
    ];
    let left = sprite.left();
    let top = sprite.top();
    let width = sprite.width;
    let height = sprite.height;

    if let Some(mesh) = &sprite.mesh {
        return mesh_vertices(sprite, mesh, left, top, width, height, corner_colors);
    }

    let right = sprite.right();
    let bottom = sprite.bottom();
    let make = |position: [f32; 2], uv: [f32; 2], color: [f32; 4]| PlanVertex {
        position: transform_sprite_point(sprite, position),
        color,
        uv,
    };
    let vertices = vec![
        make(
            [left, top],
            [sprite.uv_left, sprite.uv_top],
            corner_colors[0],
        ),
        make(
            [left, bottom],
            [sprite.uv_left, sprite.uv_bottom],
            corner_colors[2],
        ),
        make(
            [right, top],
            [sprite.uv_right, sprite.uv_top],
            corner_colors[1],
        ),
        make(
            [right, top],
            [sprite.uv_right, sprite.uv_top],
            corner_colors[1],
        ),
        make(
            [left, bottom],
            [sprite.uv_left, sprite.uv_bottom],
            corner_colors[2],
        ),
        make(
            [right, bottom],
            [sprite.uv_right, sprite.uv_bottom],
            corner_colors[3],
        ),
    ];
    (vertices, vec![0, 1, 2, 3, 4, 5], 0)
}

#[allow(clippy::too_many_arguments)]
fn mesh_vertices(
    sprite: &EmoteStaticSprite,
    mesh: &EmoteMeshPatch,
    left: f32,
    top: f32,
    width: f32,
    height: f32,
    corner_colors: [[f32; 4]; 4],
) -> (Vec<PlanVertex>, Vec<u32>, u32) {
    let division_x = mesh.division_x.max(1);
    let division_y = mesh.division_y.max(1);
    let vertex_at = |ix: u32, iy: u32| -> PlanVertex {
        let u = ix as f32 / division_x as f32;
        let v = iy as f32 / division_y as f32;
        let sample = mesh.sample(u, v);
        let position = [left + sample[0] * width, top + sample[1] * height];
        PlanVertex {
            position: transform_sprite_point(sprite, position),
            color: bilerp_color(corner_colors, u, v),
            uv: [
                sprite.uv_left + (sprite.uv_right - sprite.uv_left) * u,
                sprite.uv_top + (sprite.uv_bottom - sprite.uv_top) * v,
            ],
        }
    };

    let mut vertices = Vec::with_capacity((division_x * division_y * 6) as usize);
    let mut indices = Vec::with_capacity((division_x * division_y * 6) as usize);
    for y in 0..division_y {
        for x in 0..division_x {
            indices.extend_from_slice(&[
                vertices.len() as u32,
                vertices.len() as u32 + 1,
                vertices.len() as u32 + 2,
                vertices.len() as u32 + 3,
                vertices.len() as u32 + 4,
                vertices.len() as u32 + 5,
            ]);
            vertices.push(vertex_at(x, y));
            vertices.push(vertex_at(x, y + 1));
            vertices.push(vertex_at(x + 1, y));
            vertices.push(vertex_at(x + 1, y));
            vertices.push(vertex_at(x, y + 1));
            vertices.push(vertex_at(x + 1, y + 1));
        }
    }
    (vertices, indices, division_x.max(division_y))
}

fn native_corner_color(sprite: &EmoteStaticSprite, packed: u32) -> [f32; 4] {
    // Frame colors are 0xRRGGBBAA. Only the alpha byte is multiplied by layer
    // opacity; the neutral RGB byte for MODULATE2X is 0x80.
    let r = ((packed >> 24) & 0xFF) as f32 / 255.0;
    let g = ((packed >> 16) & 0xFF) as f32 / 255.0;
    let b = ((packed >> 8) & 0xFF) as f32 / 255.0;
    let a = (packed & 0xFF) as f32 / 255.0;
    [r, g, b, (a * sprite.opacity).clamp(0.0, 1.0)]
}

fn bilerp_color(corners: [[f32; 4]; 4], u: f32, v: f32) -> [f32; 4] {
    // Native corner order follows the rectangle: TL, TR, BL, BR.
    let mut out = [0.0; 4];
    for channel in 0..4 {
        let top = corners[0][channel] + (corners[1][channel] - corners[0][channel]) * u;
        let bottom = corners[2][channel] + (corners[3][channel] - corners[2][channel]) * u;
        out[channel] = top + (bottom - top) * v;
    }
    out
}

fn transform_sprite_point(sprite: &EmoteStaticSprite, point: [f32; 2]) -> [f32; 2] {
    let sx = if sprite.scale_x.is_finite() {
        sprite.scale_x
    } else {
        1.0
    };
    let sy = if sprite.scale_y.is_finite() {
        sprite.scale_y
    } else {
        1.0
    };
    let angle = if sprite.rotation_degrees.is_finite() {
        sprite.rotation_degrees.to_radians()
    } else {
        0.0
    };
    let cos = angle.cos();
    let sin = angle.sin();
    let dx = (point[0] - sprite.center_x) * sx;
    let dy = (point[1] - sprite.center_y) * sy;
    let local = [
        sprite.center_x + dx * cos - dy * sin,
        sprite.center_y + dx * sin + dy * cos,
    ];
    let m = sprite.world_transform;
    [
        m[0] * local[0] + m[1] * local[1] + m[4],
        m[2] * local[0] + m[3] * local[1] + m[5],
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn corner_color_is_rrggbbaa() {
        let mut sprite = EmoteStaticSprite {
            label: None,
            motion_name: String::new(),
            texture_name: String::new(),
            texture_resource_index: 0,
            texture_width: 1,
            texture_height: 1,
            texture_format: None,
            icon_name: String::new(),
            feedback_history: false,
            z: 0.0,
            opacity: 0.5,
            blend_mode: 0,
            blend_parameter: 0.0,
            corner_colors: [0x80FF_8040; 4],
            visible: true,
            center_x: 0.0,
            center_y: 0.0,
            width: 1.0,
            height: 1.0,
            scale_x: 1.0,
            scale_y: 1.0,
            rotation_degrees: 0.0,
            world_transform: [1.0, 0.0, 0.0, 1.0, 0.0, 0.0],
            uv_left: 0.0,
            uv_top: 0.0,
            uv_right: 1.0,
            uv_bottom: 1.0,
            mesh: None,
            draw_frame_info: EmoteDrawFrameInfo {
                layer_label: None,
                native_draw_key: Vec::new(),
                draw_index: 0,
                path: String::new(),
                layer_type: 0,
                ready_to_draw: true,
                submitted_to_draw_frame: true,
                mesh_transform: 0,
                mesh_combine: false,
                mesh_sync_child_mask: 0,
                mesh_sync_child_coord: false,
                mesh_sync_child_angle: false,
                mesh_sync_child_zoom: false,
                mesh_sync_child_shape: false,
                join_target: false,
                inherit_mask: None,
                inherit_parent: false,
                inherit_opacity: false,
                inherit_shape: false,
                inherit_angle: false,
                transform_order: Vec::new(),
                coordinate: None,
                ground_correction: false,
                obj_tri_priority: 0,
                clip_rect: None,
                stencil_type: 0,
                stencil_phase: 0,
                stencil_composite_item: false,
                stencil_wipe_enabled: false,
                stencil_wipe_reverse: false,
                stencil_wipe_scale: 0.0,
                stencil_wipe_bias: 0.0,
                stencil_composite_mask_layer_list: Vec::new(),
                stencil_composite_target_paths: Vec::new(),
                parent_mask_path: None,
                stencil_parent_path: None,
                stencil_parent_native_key: None,
                control_parameter: None,
                control_value: None,
                local_time_ticks: None,
                frame_index: None,
                next_frame_index: None,
                interpolation_t: 0.0,
                pass: EmoteDrawPass::Normal,
            },
        };
        let color = native_corner_color(&sprite, sprite.corner_colors[0]);
        assert!((color[0] - (128.0 / 255.0)).abs() < 1e-6);
        assert!((color[1] - 1.0).abs() < 1e-6);
        assert!((color[2] - (128.0 / 255.0)).abs() < 1e-6);
        assert!((color[3] - (32.0 / 255.0)).abs() < 1e-6);
        sprite.opacity = 1.0;
        assert!((native_corner_color(&sprite, 0x0000_00FF)[3] - 1.0).abs() < 1e-6);
    }
}
