//! Layer/frame pose evaluation (M2).
//! Algorithms and native semantics adapted from eluna (MPL-2.0,
//! https://github.com/xmoezzz/eluna). See NOTICE and LICENSE-MPL-2.0.
//!
//! ## Evidence
//!
//! The model structure (layers, `frameList`, `content` channel names,
//! `inheritMask`, `transformOrder`, `parameterize`) is observed in official
//! PSB data and in the official DLL's schema keys. The evaluation rules below
//! follow the behavior recovered from the official runtime by public tooling
//! (notably eluna, which cites native `sub_1032FB00`, `sub_10334D40`,
//! `sub_10331060`, `sub_103D0E0` behavior):
//!
//! - a layer picks the last frame whose `time` is <= the layer's sample time;
//! - `type == 0` holds the previous decoded frame (no update);
//! - only `type == 3` interpolates toward its immediate successor;
//! - `ti` quantizes elapsed time before interpolation;
//! - `transformOrder` is a permutation of `0=flip, 1=rotation, 2=scale,
//!   3=shear`, left-multiplied stage by stage;
//! - `inheritMask` bits select channel-wise inheritance:
//!   `0x4/0x8` flips, `0x10` angle, `0x20/0x40` scales, `0x80/0x100` shears,
//!   `0x1FC` all linear channels, `0x400` opacity, `0x400000` transparent
//!   inheritance source;
//! - `coordinate == 0` maps XY, non-zero maps XZ.
//!
//! These rules are **not** yet confirmed by official pixel parity. Fields the
//! data does not provide (e.g. non-zero easing tables, stencil/mask ordering,
//! physics controllers) are left in their authored/held state and are called
//! out in [`FrameState`].

use std::collections::BTreeMap;

use serde::Serialize;

use crate::render::MeshPatch;
use crate::schema::{
    Content, EasingCurve, EasingPiece, Frame, IconRef, Layer, Model, Motion, MotionPath,
    ParameterRef,
};

/// Full transform-order mask (all linear channels inherited).
const INHERIT_LINEAR_ALL: i64 = 0x1FC;
/// Opacity inheritance bit.
const INHERIT_OPACITY: i64 = 0x400;
/// Recursion guard for type-3 nested motions.
const MAX_NESTED_MOTION_DEPTH: usize = 256;

/// Decoded state of one layer at a given time.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct FrameState {
    /// Selected frame index in `frameList`, if any.
    pub frame_index: Option<usize>,
    /// Start time of the selected frame.
    pub frame_start: f64,
    /// Serialized frame type.
    pub kind: i64,
    /// Interpolation factor toward the next frame (0 for held frames).
    pub interpolation_t: f64,
    /// Whether a frame was selected.
    pub valid: bool,
    /// Local translation (x, y, z).
    pub coord: [f64; 3],
    /// Sprite origin (ox, oy).
    pub origin: [f64; 2],
    /// Horizontal flip.
    pub flip_x: bool,
    /// Vertical flip.
    pub flip_y: bool,
    /// Local scale.
    pub scale: [f64; 2],
    /// Local rotation in degrees.
    pub rotation_deg: f64,
    /// Local shear.
    pub shear: [f64; 2],
    /// Four corner colors (each `0xAARRGGBB`).
    pub color: [u32; 4],
    /// Opacity as authored (0..255).
    pub opacity: f64,
    /// Mask bits.
    pub mask: i64,
    /// Texture page name.
    pub src: Option<String>,
    /// Atlas icon.
    pub icon: Option<IconRef>,
    /// Flat mesh control points (`mesh.bp`).
    pub mesh_bp: Option<Vec<f64>>,
    /// Blend mode (`bm`, default 0x10).
    pub blend_mode: u32,
    /// Blend parameter (`bp`).
    pub blend_parameter: f64,
    /// Sub-motion time offset.
    pub motion_time_offset: f64,
    /// Type-9 anchor target label.
    pub anchor_target: Option<String>,
}

impl Default for FrameState {
    fn default() -> Self {
        Self {
            frame_index: None,
            frame_start: 0.0,
            kind: 3,
            interpolation_t: 0.0,
            valid: false,
            coord: [0.0; 3],
            origin: [0.0; 2],
            flip_x: false,
            flip_y: false,
            scale: [1.0; 2],
            rotation_deg: 0.0,
            shear: [0.0; 2],
            color: [0x8080_80FF; 4],
            opacity: 255.0,
            mask: 0,
            src: None,
            icon: None,
            mesh_bp: None,
            blend_mode: 0x10,
            blend_parameter: 0.0,
            motion_time_offset: 0.0,
            anchor_target: None,
        }
    }
}

impl FrameState {
    /// Build a mesh patch from the current control points.
    pub fn mesh_patch(&self) -> MeshPatch {
        MeshPatch::from_bp(self.mesh_bp.as_deref())
    }

    fn apply_content(&mut self, content: &Content) {
        if let Some(coord) = content.coord {
            self.coord = coord;
        }
        if let Some(ox) = content.ox {
            self.origin[0] = ox;
        }
        if let Some(oy) = content.oy {
            self.origin[1] = oy;
        }
        if let Some(flip) = content.flip_x {
            self.flip_x = flip;
        }
        if let Some(flip) = content.flip_y {
            self.flip_y = flip;
        }
        if let Some(scale) = content.scale_x {
            self.scale[0] = scale;
        }
        if let Some(scale) = content.scale_y {
            self.scale[1] = scale;
        }
        if let Some(angle) = content.angle {
            self.rotation_deg = angle;
        }
        if let Some(shear) = content.shear_x {
            self.shear[0] = shear;
        }
        if let Some(shear) = content.shear_y {
            self.shear[1] = shear;
        }
        if let Some(src) = &content.src {
            self.src = Some(src.clone());
        }
        if let Some(icon) = &content.icon {
            self.icon = Some(icon.clone());
        }
        if let Some(opa) = content.opa {
            self.opacity = opa;
        }
        if let Some(mask) = content.mask {
            self.mask = mask;
        }
        if let Some(bm) = content.blend_mode {
            self.blend_mode = bm;
        }
        if let Some(bp) = content.blend_parameter {
            self.blend_parameter = bp;
        }
        if let Some(color) = content.color {
            self.color = color;
        } else if (self.blend_mode & 0xF0) == 0 {
            // sub_1033D0E0: legacy non-MODULATE2X frames default to white;
            // MODULATE2X frames use the neutral 0x80 gray that doubles to 1.
            self.color = [0xFFFF_FFFF; 4];
        } else {
            self.color = [0x8080_80FF; 4];
        }
        if let Some(mesh) = &content.mesh {
            if let Some(bp) = &mesh.bp {
                self.mesh_bp = Some(bp.clone());
            }
        }
        if let Some(time_offset) = content.motion_time_offset {
            self.motion_time_offset = time_offset;
        }
        if let Some(target) = &content.anchor_target {
            self.anchor_target = Some(target.clone());
        }
    }
}

/// Evaluate one layer's frame list at `time` without a model easing table.
pub fn evaluate_frames(frames: &[Frame], time: f64, previous: Option<&FrameState>) -> FrameState {
    evaluate_frames_with_easing(frames, time, previous, &[], 0)
}

/// Evaluate one layer's frame list with the model easing table and coordinate
/// plane. `coordinate_plane == 0` maps XY; any other value maps XZ.
pub fn evaluate_frames_with_easing(
    frames: &[Frame],
    time: f64,
    previous: Option<&FrameState>,
    easing_table: &[EasingCurve],
    coordinate_plane: i64,
) -> FrameState {
    let Some(current_index) = frames
        .iter()
        .enumerate()
        .take_while(|(_, frame)| frame.time as f64 <= time)
        .map(|(index, _)| index)
        .last()
    else {
        return FrameState::default();
    };
    let current = &frames[current_index];
    let mut state = FrameState {
        frame_index: Some(current_index),
        frame_start: current.time as f64,
        kind: current.kind,
        valid: true,
        ..FrameState::default()
    };

    if current.kind == 0 {
        // A type-0 terminal frame holds the last authored keyframe. The
        // previous state may live in an earlier frame of the same list (the
        // nested eye/mouth motions rely on this), so search backwards before
        // falling back to the caller-provided state.
        for previous_frame in frames[..current_index].iter().rev() {
            if let Some(content) = &previous_frame.content {
                let mut held = FrameState {
                    frame_index: Some(current_index),
                    frame_start: current.time as f64,
                    kind: 0,
                    valid: true,
                    ..FrameState::default()
                };
                held.apply_content(content);
                held.interpolation_t = 0.0;
                return held;
            }
        }
        if let Some(previous) = previous {
            let mut held = previous.clone();
            held.frame_index = Some(current_index);
            held.frame_start = current.time as f64;
            held.kind = 0;
            held.interpolation_t = 0.0;
            return held;
        }
        return state;
    }

    let Some(content) = &current.content else {
        return state;
    };
    state.apply_content(content);

    if current.kind != 3 {
        return state;
    }
    let Some(next) = frames.get(current_index + 1) else {
        return state;
    };
    if next.kind == 0 {
        return state;
    }
    let Some(next_content) = &next.content else {
        return state;
    };
    let span = next.time as f64 - current.time as f64;
    if !span.is_finite() || span <= f64::EPSILON {
        return state;
    }
    let mut elapsed = (time - current.time as f64).max(0.0);
    if let Some(ti) = current.time_quantization.filter(|ti| *ti > 0) {
        let ti = ti as f64;
        elapsed = ti * (elapsed / ti).trunc();
    }
    let t = (elapsed / span).clamp(0.0, 1.0);
    state.interpolation_t = t;

    let mut next_state = FrameState::default();
    next_state.apply_content(next_content);
    interpolate_state(
        &mut state,
        &next_state,
        content,
        t,
        easing_table,
        coordinate_plane,
    );
    state
}

fn interpolate_state(
    state: &mut FrameState,
    next: &FrameState,
    current_content: &Content,
    t: f64,
    easing_table: &[EasingCurve],
    coordinate_plane: i64,
) {
    // Native per-property curves live on the CURRENT type-3 frame:
    // ccc = coordinate, acc = angle, zcc = zoom, scc = shear, occ = color.
    // Opacity uses the raw interpolation factor.
    let coord_t = frame_easing(t, current_content.coord_easing, easing_table);
    let angle_t = frame_easing(t, current_content.angle_easing, easing_table);
    let zoom_t = frame_easing(t, current_content.zoom_easing, easing_table);
    let shear_t = frame_easing(t, current_content.shear_easing, easing_table);

    let start = state.coord;
    let end = next.coord;
    state.coord = if start == end {
        start
    } else if let Some(path) = &current_content.path {
        evaluate_motion_path(path, coord_t).map_or_else(
            || {
                [
                    lerp(start[0], end[0], coord_t),
                    lerp(start[1], end[1], coord_t),
                    lerp(start[2], end[2], coord_t),
                ]
            },
            |[u, v]| {
                if coordinate_plane == 0 {
                    let dx = end[0] - start[0];
                    let dy = end[1] - start[1];
                    [
                        start[0] + dx * u - dy * v,
                        start[1] + dy * u + dx * v,
                        lerp(start[2], end[2], coord_t),
                    ]
                } else {
                    let dx = end[0] - start[0];
                    let dz = end[2] - start[2];
                    [
                        start[0] + dx * u - dz * v,
                        lerp(start[1], end[1], coord_t),
                        start[2] + dz * u + dx * v,
                    ]
                }
            },
        )
    } else {
        [
            lerp(start[0], end[0], coord_t),
            lerp(start[1], end[1], coord_t),
            lerp(start[2], end[2], coord_t),
        ]
    };

    // ox/oy are not part of the recovered StepFrame interpolation block.
    state.scale[0] = lerp(state.scale[0], next.scale[0], zoom_t);
    state.scale[1] = lerp(state.scale[1], next.scale[1], zoom_t);
    state.rotation_deg = lerp_angle(state.rotation_deg, next.rotation_deg, angle_t);
    state.shear[0] = lerp(state.shear[0], next.shear[0], shear_t);
    state.shear[1] = lerp(state.shear[1], next.shear[1], shear_t);
    state.opacity = (lerp(state.opacity, next.opacity, t) + 0.5).floor();
    let color_t = frame_easing(t, current_content.color_easing, easing_table);
    for channel in 0..4 {
        state.color[channel] = lerp_color(state.color[channel], next.color[channel], color_t);
    }
    // An omitted or empty `mesh.bp` decodes to the identity patch, not to
    // "keep the previous mesh". Interpolating frame 0's deformation to the
    // empty frame at 30 is therefore a smooth return to neutral; treating the
    // empty frame as absent made the parameter snap at the authored midpoint.
    if state.mesh_bp.is_some() || next.mesh_bp.is_some() {
        let current = MeshPatch::from_bp(state.mesh_bp.as_deref());
        let target = MeshPatch::from_bp(next.mesh_bp.as_deref());
        let blended = MeshPatch::interpolate(&current, &target, t as f32);
        let mut flat = Vec::with_capacity(32);
        for point in &blended.control_points {
            flat.push(point[0] as f64);
            flat.push(point[1] as f64);
        }
        state.mesh_bp = Some(flat);
    }
}

fn frame_easing(t: f64, reference: Option<i64>, easing_table: &[EasingCurve]) -> f64 {
    let t = t.clamp(0.0, 1.0);
    let Some(index) = reference.filter(|index| *index >= 0) else {
        return t;
    };
    easing_table
        .get(index as usize)
        .and_then(|curve| evaluate_easing_curve(curve, t))
        .unwrap_or(t)
}

fn evaluate_easing_curve(curve: &EasingCurve, x: f64) -> Option<f64> {
    if curve.pieces.is_empty() {
        return None;
    }
    for piece in &curve.pieces {
        if piece.x.len() >= 2 && x >= piece.x[0] && x <= *piece.x.last()? {
            return evaluate_easing_piece(piece, x);
        }
    }
    // Malformed/gapped tables use the closest authored endpoint rather than
    // an invented smoothstep fallback.
    let piece = curve.pieces.iter().min_by(|a, b| {
        let da = a.x.first().map_or(f64::INFINITY, |start| (x - start).abs());
        let db = b.x.first().map_or(f64::INFINITY, |start| (x - start).abs());
        da.total_cmp(&db)
    })?;
    evaluate_easing_piece(piece, x)
}

fn evaluate_easing_piece(piece: &EasingPiece, x: f64) -> Option<f64> {
    if piece.x.len() < 2 || piece.x.len() != piece.y.len() || piece.x.len() != piece.p.len() {
        return None;
    }
    let mut interval = 0usize;
    while interval + 1 < piece.x.len() - 1 && x > piece.x[interval + 1] {
        interval += 1;
    }
    while interval > 0 && x < piece.x[interval] {
        interval -= 1;
    }
    let x0 = piece.x[interval];
    let x1 = piece.x[interval + 1];
    let h = x1 - x0;
    if !h.is_finite() || h.abs() <= f64::EPSILON {
        return Some(piece.y[interval]);
    }
    let u = (x - x0) / h;
    let v = 1.0 - u;
    let cubic_u = u * u * u - u;
    let cubic_v = v * v * v - v;
    let linear = v * piece.y[interval] + u * piece.y[interval + 1];
    Some(linear + h * h * (cubic_u * piece.p[interval + 1] + cubic_v * piece.p[interval]) / 6.0)
}

fn evaluate_motion_path(path: &MotionPath, t: f64) -> Option<[f64; 2]> {
    if path.t.len() < 2 || path.x.len() < 4 || path.y.len() < 4 {
        return None;
    }
    let segments = path.t.len() - 1;
    let mut segment = 0usize;
    while segment + 1 < segments && t > path.t[segment + 1] {
        segment += 1;
    }
    while segment > 0 && path.t[segment] > t {
        segment -= 1;
    }
    let t0 = path.t[segment];
    let t1 = path.t[segment + 1];
    let span = t1 - t0;
    let local = if span.is_finite() && span.abs() > f64::EPSILON {
        (t - t0) / span
    } else {
        0.0
    };
    let u = path
        .s
        .get(segment)
        .and_then(|curve| evaluate_easing_curve(curve, local))
        .unwrap_or(local);
    let v = 1.0 - u;
    let b0 = v * v * v;
    let b1 = 3.0 * v * v * u;
    let b2 = 3.0 * v * u * u;
    let b3 = u * u * u;
    let base = segment * 3;
    if base + 3 >= path.x.len() || base + 3 >= path.y.len() {
        return None;
    }
    Some([
        b0 * path.x[base] + b1 * path.x[base + 1] + b2 * path.x[base + 2] + b3 * path.x[base + 3],
        b0 * path.y[base] + b1 * path.y[base + 1] + b2 * path.y[base + 2] + b3 * path.y[base + 3],
    ])
}

fn lerp(a: f64, b: f64, t: f64) -> f64 {
    a + (b - a) * t
}

fn lerp_color(a: u32, b: u32, t: f64) -> u32 {
    // Native `sub_103A4E90`: colors are 0xRRGGBBAA and interpolation happens
    // per byte lane with weight = floor(t * 256), not per 32-bit scalar.
    if a == b {
        return a;
    }
    let weight = (t.clamp(0.0, 1.0) * 256.0) as u32;
    let inverse = 256u32.saturating_sub(weight);
    let rb_a = a & 0x00FF_00FF;
    let rb_b = b & 0x00FF_00FF;
    let ga_a = (a >> 8) & 0x00FF_00FF;
    let ga_b = (b >> 8) & 0x00FF_00FF;
    let rb = ((weight * rb_b + inverse * rb_a) >> 8) & 0x00FF_00FF;
    let ga = (((weight * ga_b + inverse * ga_a) >> 8) & 0x00FF_00FF) << 8;
    rb | ga
}

fn lerp_angle(a: f64, b: f64, t: f64) -> f64 {
    let mut target = b;
    let delta = target - a;
    if delta > 180.0 {
        target -= 360.0;
    } else if delta < -180.0 {
        target += 360.0;
    }
    let value = lerp(a, target, t);
    if value.is_finite() {
        value.rem_euclid(360.0)
    } else {
        0.0
    }
}

/// 2D affine transform (row-vector convention: `[x y 1] * M`).
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct Affine2 {
    /// Row 1, column 1.
    pub m11: f64,
    /// Row 1, column 2.
    pub m12: f64,
    /// Row 2, column 1.
    pub m21: f64,
    /// Row 2, column 2.
    pub m22: f64,
    /// Translation X.
    pub tx: f64,
    /// Translation Y.
    pub ty: f64,
}

impl Default for Affine2 {
    fn default() -> Self {
        Self::identity()
    }
}

impl Affine2 {
    /// Identity transform.
    pub const fn identity() -> Self {
        Self {
            m11: 1.0,
            m12: 0.0,
            m21: 0.0,
            m22: 1.0,
            tx: 0.0,
            ty: 0.0,
        }
    }

    /// `self * next` (apply `self`, then `next`).
    pub fn then(self, next: Self) -> Self {
        Self {
            m11: self.m11 * next.m11 + self.m12 * next.m21,
            m12: self.m11 * next.m12 + self.m12 * next.m22,
            m21: self.m21 * next.m11 + self.m22 * next.m21,
            m22: self.m21 * next.m12 + self.m22 * next.m22,
            tx: self.tx * next.m11 + self.ty * next.m21 + next.tx,
            ty: self.tx * next.m12 + self.ty * next.m22 + next.ty,
        }
    }

    /// Apply the linear part only.
    pub fn apply_linear(self, point: [f64; 2]) -> [f64; 2] {
        [
            point[0] * self.m11 + point[1] * self.m21,
            point[0] * self.m12 + point[1] * self.m22,
        ]
    }

    /// Apply the full transform.
    pub fn apply(self, point: [f64; 2]) -> [f64; 2] {
        let linear = self.apply_linear(point);
        [linear[0] + self.tx, linear[1] + self.ty]
    }

    /// Inverse transform, or `None` for a singular matrix.
    pub fn inverse(self) -> Option<Self> {
        let determinant = self.m11 * self.m22 - self.m12 * self.m21;
        if !determinant.is_finite() || determinant.abs() < 1e-12 {
            return None;
        }
        let inv = 1.0 / determinant;
        let m11 = self.m22 * inv;
        let m12 = -self.m12 * inv;
        let m21 = -self.m21 * inv;
        let m22 = self.m11 * inv;
        Some(Self {
            m11,
            m12,
            m21,
            m22,
            tx: -(self.tx * m11 + self.ty * m21),
            ty: -(self.tx * m12 + self.ty * m22),
        })
    }

    /// Column-major 4x4 matrix for the vertex shader (`cMat`).
    pub fn to_mat4(self) -> [[f32; 4]; 4] {
        [
            [self.m11 as f32, self.m12 as f32, 0.0, 0.0],
            [self.m21 as f32, self.m22 as f32, 0.0, 0.0],
            [0.0, 0.0, 1.0, 0.0],
            [self.tx as f32, self.ty as f32, 0.0, 1.0],
        ]
    }
}

const DEFAULT_TRANSFORM_ORDER: [i64; 4] = [0, 3, 2, 1];

/// Channel-level linear state used for inheritance.
#[derive(Debug, Clone, Copy, PartialEq)]
struct LinearState {
    flip_x: bool,
    flip_y: bool,
    rotation_deg: f64,
    scale: [f64; 2],
    shear: [f64; 2],
}

impl Default for LinearState {
    fn default() -> Self {
        Self {
            flip_x: false,
            flip_y: false,
            rotation_deg: 0.0,
            scale: [1.0; 2],
            shear: [0.0; 2],
        }
    }
}

fn normalized_transform_order(order: &[i64]) -> [i64; 4] {
    if order.len() == 4 {
        let mut seen = [false; 4];
        let mut out = [0i64; 4];
        let mut valid = true;
        for (index, value) in order.iter().copied().enumerate() {
            if !(0..=3).contains(&value) || seen[value as usize] {
                valid = false;
                break;
            }
            seen[value as usize] = true;
            out[index] = value;
        }
        if valid {
            return out;
        }
    }
    DEFAULT_TRANSFORM_ORDER
}

fn build_linear(order: &[i64], state: LinearState) -> Affine2 {
    let mut linear = Affine2::identity();
    for stage in normalized_transform_order(order) {
        let op = match stage {
            0 => Affine2 {
                m11: if state.flip_x { -1.0 } else { 1.0 },
                m12: 0.0,
                m21: 0.0,
                m22: if state.flip_y { -1.0 } else { 1.0 },
                tx: 0.0,
                ty: 0.0,
            },
            1 => {
                let radians = state.rotation_deg.to_radians();
                let (sin, cos) = radians.sin_cos();
                Affine2 {
                    m11: cos,
                    m12: sin,
                    m21: -sin,
                    m22: cos,
                    tx: 0.0,
                    ty: 0.0,
                }
            }
            2 => Affine2 {
                m11: state.scale[0],
                m12: 0.0,
                m21: 0.0,
                m22: state.scale[1],
                tx: 0.0,
                ty: 0.0,
            },
            3 => Affine2 {
                m11: 1.0,
                m12: state.shear[1],
                m21: state.shear[0],
                m22: 1.0,
                tx: 0.0,
                ty: 0.0,
            },
            _ => unreachable!(),
        };
        linear = linear.then(op);
    }
    linear
}

fn inherit_state(own: LinearState, parent: LinearState, mask: i64) -> LinearState {
    LinearState {
        flip_x: own.flip_x ^ ((mask & 0x4) != 0 && parent.flip_x),
        flip_y: own.flip_y ^ ((mask & 0x8) != 0 && parent.flip_y),
        rotation_deg: own.rotation_deg
            + if (mask & 0x10) != 0 {
                parent.rotation_deg
            } else {
                0.0
            },
        scale: [
            own.scale[0]
                * if (mask & 0x20) != 0 {
                    parent.scale[0]
                } else {
                    1.0
                },
            own.scale[1]
                * if (mask & 0x40) != 0 {
                    parent.scale[1]
                } else {
                    1.0
                },
        ],
        shear: [
            own.shear[0]
                + if (mask & 0x80) != 0 {
                    parent.shear[0]
                } else {
                    0.0
                },
            own.shear[1]
                + if (mask & 0x100) != 0 {
                    parent.shear[1]
                } else {
                    0.0
                },
        ],
    }
}

/// Mesh-sync domain from the layer's first authored blank icon.
///
/// Blank icons are `width:height:originX:originY`; the native domain is
/// `[-originX, -originY, width, height]` (`parse_mesh_domain_icon`).
fn layer_mesh_domain(layer: &Layer) -> Option<[f32; 4]> {
    layer
        .frames
        .iter()
        .filter_map(|frame| frame.content.as_ref())
        .filter_map(|content| content.mesh.as_ref())
        .filter_map(|_| {
            layer
                .frames
                .iter()
                .filter_map(|frame| frame.content.as_ref())
                .filter_map(|content| content.icon.as_ref())
                .filter_map(|icon| icon.rect)
                .next()
        })
        .next()
        .and_then(|rect| {
            let [width, height, origin_x, origin_y] = rect;
            (width.is_finite() && height.is_finite() && width > 0.0 && height > 0.0).then_some([
                -origin_x as f32,
                -origin_y as f32,
                width as f32,
                height as f32,
            ])
        })
}

struct MeshCombinatorEval {
    self_patch: Option<MeshPatch>,
    child_patch: Option<MeshPatch>,
}

fn layer_parameter_key(
    layer: &Layer,
    parameters: &[crate::schema::ParameterDef],
) -> Option<String> {
    match layer.parameterize.as_ref()? {
        ParameterRef::Index(index) if *index >= 0 => parameters
            .get(*index as usize)
            .and_then(|parameter| parameter.id.clone()),
        ParameterRef::Inline(value) => value
            .get("id")
            .and_then(|id| id.as_str())
            .map(str::to_owned),
        _ => None,
    }
}

/// Evaluate `meshCombinator.combinatorList` for one layer.
///
/// The first entry is a self patch when it matches the layer's own
/// `parameterize` key (or when the layer has no parameter); every following
/// entry is a child patch and is authored as a displacement from identity.
fn evaluate_mesh_combinator(
    combinator: &crate::schema::MeshCombinator,
    layer: &Layer,
    parameters: &[crate::schema::ParameterDef],
    variables: &BTreeMap<String, f64>,
) -> Option<MeshCombinatorEval> {
    let first = combinator.entries.first()?;
    let parent_key = layer_parameter_key(layer, parameters);
    let restore_first_to_parent = match (parent_key.as_deref(), first.key.as_deref()) {
        (None, _) => true,
        (Some("param"), _) => true,
        (Some(parent), Some(first)) => parent == first,
        (Some(_), None) => false,
    };
    let domain = layer_mesh_domain(layer);
    let division = layer.mesh_division.unwrap_or(1).max(1) as u32;
    let mut self_patch = None;
    let mut child_patch = None;
    let mut start = 0usize;
    if restore_first_to_parent {
        if let Some(patch) = evaluate_one_mesh_combinator(first, false, variables, division, domain)
        {
            self_patch = Some(patch);
            start = 1;
        }
    }
    for entry in combinator.entries.iter().skip(start) {
        if let Some(patch) = evaluate_one_mesh_combinator(entry, true, variables, division, domain)
        {
            child_patch = combine_patch(child_patch, Some(patch));
        }
    }
    (self_patch.is_some() || child_patch.is_some()).then_some(MeshCombinatorEval {
        self_patch,
        child_patch,
    })
}

fn evaluate_one_mesh_combinator(
    entry: &crate::schema::MeshCombinatorEntry,
    delta: bool,
    variables: &BTreeMap<String, f64>,
    division: u32,
    domain: Option<[f32; 4]>,
) -> Option<MeshPatch> {
    let key = entry.key.as_deref()?;
    let mesh_count = entry.mesh_count.max(1);
    let begin = entry.range_begin;
    let end = entry.range_end;
    let neutral = if begin <= 0.0 && end >= 0.0 {
        0.0
    } else {
        (begin + end) * 0.5
    };
    let value = variables.get(key).copied().unwrap_or(neutral);
    let position = if mesh_count <= 1
        || !begin.is_finite()
        || !end.is_finite()
        || (end - begin).abs() <= f64::EPSILON
    {
        0.0
    } else {
        ((value - begin) / (end - begin)).clamp(0.0, 1.0) * (mesh_count as f64 - 1.0)
    };
    let i0 = position.floor() as usize;
    let i1 = (position.ceil() as usize).min(mesh_count - 1);
    let t = (position - i0 as f64) as f32;

    let patch_at = |index: usize| -> Option<MeshPatch> {
        if entry.neutral_index == index as i64 {
            return Some(
                MeshPatch::identity()
                    .with_division(division)
                    .with_domain(domain),
            );
        }
        let values = entry.values.get(index)?;
        let mut patch = MeshPatch::identity();
        for (slot, point) in patch.control_points.iter_mut().enumerate() {
            let mut x = values[slot * 2];
            let mut y = values[slot * 2 + 1];
            if delta {
                x += (slot % 4) as f32 / 3.0;
                y += (slot / 4) as f32 / 3.0;
            }
            *point = [x, y];
        }
        Some(patch.with_division(division).with_domain(domain))
    };
    let p0 = patch_at(i0)?;
    let p1 = patch_at(i1)?;
    Some(MeshPatch::interpolate(&p0, &p1, t).with_domain(domain))
}

/// Warp a child's authored coordinate/angle/scale through a parent's
/// shape-synced mesh (`sub_10335500`).
fn apply_mesh_sync_child_state(state: &mut FrameState, sync: MeshSyncState, inherit_mask: i64) {
    let Some([left, top, width, height]) = sync.patch.domain else {
        return;
    };
    if !width.is_finite()
        || !height.is_finite()
        || width.abs() <= f32::EPSILON
        || height.abs() <= f32::EPSILON
    {
        return;
    }
    let use_xz = sync.coordinate != 0;
    let point = if use_xz {
        [state.coord[0] as f32, state.coord[2] as f32]
    } else {
        [state.coord[0] as f32, state.coord[1] as f32]
    };
    let warp = |point: [f32; 2]| -> [f32; 2] {
        let u = (point[0] - left) / width;
        let v = (point[1] - top) / height;
        let mapped = sync.patch.sample(u, v);
        [left + mapped[0] * width, top + mapped[1] * height]
    };
    let mapped = warp(point);

    const EPS: f32 = 0.0001;
    let xm = warp([point[0] - EPS, point[1]]);
    let xp = warp([point[0] + EPS, point[1]]);
    let ym = warp([point[0], point[1] - EPS]);
    let yp = warp([point[0], point[1] + EPS]);

    if sync.mask & 0x2 != 0 && inherit_mask & 0x10 != 0 {
        let dx = [xp[0] - xm[0], xp[1] - xm[1]];
        let dy = [yp[0] - ym[0], yp[1] - ym[1]];
        if dx[0].is_finite() && dx[1].is_finite() && dy[0].is_finite() && dy[1].is_finite() {
            let ax = dx[1].atan2(dx[0]);
            let ay = (-dy[0]).atan2(dy[1]);
            state.rotation_deg += ((ax + ay) * 0.5).to_degrees() as f64;
        }
    }

    if sync.mask & 0x4 != 0 && inherit_mask & 0x60 != 0 {
        let triangle_area = |a: [f32; 2], b: [f32; 2], c: [f32; 2]| {
            (((b[0] - a[0]) * (c[1] - a[1]) - (b[1] - a[1]) * (c[0] - a[0])).abs()) * 0.5
        };
        let area = triangle_area(xm, xp, ym) + triangle_area(xm, xp, yp);
        let scale = (2.0 * area).sqrt() / (2.0 * EPS);
        if scale.is_finite() {
            if inherit_mask & 0x20 != 0 {
                state.scale[0] *= scale as f64;
            }
            if inherit_mask & 0x40 != 0 {
                state.scale[1] *= scale as f64;
            }
        }
    }

    if sync.mask & 0x1 != 0 {
        state.coord[0] = mapped[0] as f64;
        if use_xz {
            state.coord[2] = mapped[1] as f64;
        } else {
            state.coord[1] = mapped[1] as f64;
        }
    }
}

/// Remove the motion-root contribution from a partially-inherited state.
///
/// Native `sub_10331060` removes only the channels selected by `inheritMask`,
/// builds the relative matrix, and reapplies the root matrix outside it.
fn remove_motion_root_linear_state(
    mut state: LinearState,
    root: LinearState,
    mask: i64,
) -> LinearState {
    if (mask & 0x4) != 0 {
        state.flip_x ^= root.flip_x;
    }
    if (mask & 0x8) != 0 {
        state.flip_y ^= root.flip_y;
    }
    if (mask & 0x10) != 0 {
        state.rotation_deg -= root.rotation_deg;
    }
    if (mask & 0x20) != 0 && root.scale[0].abs() > f64::EPSILON {
        state.scale[0] /= root.scale[0];
    }
    if (mask & 0x40) != 0 && root.scale[1].abs() > f64::EPSILON {
        state.scale[1] /= root.scale[1];
    }
    if (mask & 0x80) != 0 {
        state.shear[0] -= root.shear[0];
    }
    if (mask & 0x100) != 0 {
        state.shear[1] -= root.shear[1];
    }
    state
}

/// One ancestor shape-sync patch with its world frame.
///
/// Native `StepFrameMeshChain` stores a patch and the owning layer transform;
/// type-1/type-5 layer positions are warped through this chain. Physics anchors
/// read the resulting position, not the un-warped world origin.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct MeshChainEntry {
    /// Patch in the owning layer's local sprite pixels.
    pub patch: MeshPatch,
    /// Owning layer world transform.
    pub world: Affine2,
}

/// One layer in the evaluated pose.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct LayerPose {
    /// Layer label.
    pub label: String,
    /// Structural path (`0/1/2` by sibling index).
    pub path: String,
    /// Layer kind.
    pub kind: i64,
    /// Time used to sample this layer.
    pub sample_time: f64,
    /// Decoded frame state.
    pub state: FrameState,
    /// Local linear transform (no translation).
    pub local: Affine2,
    /// Accumulated world transform.
    pub world: Affine2,
    /// Accumulated world origin.
    pub world_origin: [f64; 3],
    /// Accumulated opacity (0..1).
    pub opacity: f64,
    /// Whether this layer and its ancestors are visible.
    pub visible: bool,
    /// Whether this layer can emit geometry (types 0/10/12).
    pub drawable: bool,
    /// `meshDivision` (tessellation density, observed 20).
    pub mesh_division: u32,
    /// `meshTransform` flag.
    pub mesh_transform: i64,
    /// `meshCombine` flag.
    pub mesh_combine: bool,
    /// Type-7 clip rectangle in world space (`left, top, right, bottom`).
    pub clip_rect: Option<[f64; 4]>,
    /// Type-9 anchor mode (`anchor`).
    pub anchor_mode: i64,
    /// `stencilType`.
    pub stencil_type: i64,
    /// Composite-mask group inherited from the nearest type-12 owner with
    /// `stencilType & 4` and a non-empty `stencilCompositeMaskLayerList`.
    pub mask_group: Option<usize>,
    /// Motion scope this layer was evaluated in (nested motions get their own
    /// scope, so equal labels in the left/right eye never alias).
    pub scope: usize,
    /// Accumulated 4x4 Bezier sprite patch for this layer's own geometry.
    ///
    /// The native mesh chain (`sub_10353CF0`) folds every ancestor whose
    /// `meshTransform == 1` and `meshSyncChildMask & 8` is set (shape sync)
    /// into the child, combining patches by pointwise displacement addition
    /// (`sub_1038B340`). The patch is normalized to the drawn sprite's icon
    /// box; it is not a world-space warp.
    pub mesh: Option<MeshPatch>,
    /// Ancestor shape-sync mesh chain used to warp type-1/5 positions.
    #[serde(skip)]
    pub mesh_chain: Vec<MeshChainEntry>,
    /// Draw-order key: rank path from `priority` frames.
    pub priority_path: Vec<i64>,
    /// Child layers.
    pub children: Vec<LayerPose>,
}

/// One composite alpha-mask owner (`stencilType & 4`).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct MaskGroup {
    /// Owner layer path (diagnostics only).
    pub owner_path: String,
    /// Motion scope the owner belongs to; source labels resolve here.
    pub owner_scope: usize,
    /// `stencilCompositeMaskLayerList` labels.
    pub sources: Vec<String>,
    /// Resolved source layer paths.
    ///
    /// A type-0/10 source contributes itself when it has visible geometry.
    /// A type-3 source is not color geometry; the native resolver expands the
    /// referenced nested player and contributes every emitted sprite below it
    /// (`sub_10337560` + `sub_103390C0` second pass).
    pub source_paths: Vec<String>,
}

/// A full motion pose at one time.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Pose {
    /// Object name.
    pub object: String,
    /// Motion name.
    pub motion: String,
    /// Sample time.
    pub time: f64,
    /// Variables used for parameterized layers.
    pub variables: BTreeMap<String, f64>,
    /// Composite-mask owners encountered during traversal.
    pub mask_groups: Vec<MaskGroup>,
    /// Root layers.
    pub layers: Vec<LayerPose>,
}

impl Pose {
    /// Evaluate a motion from a model.
    pub fn evaluate(
        model: &Model,
        object: &str,
        motion_name: &str,
        time: f64,
        variables: &BTreeMap<String, f64>,
    ) -> Option<Self> {
        let motion = model.motion(object, motion_name)?;
        Some(Self::from_motion(
            model,
            object,
            motion_name,
            motion,
            time,
            variables,
        ))
    }

    /// Evaluate an already-resolved motion.
    pub fn from_motion(
        model: &Model,
        object: &str,
        motion_name: &str,
        motion: &Motion,
        time: f64,
        variables: &BTreeMap<String, f64>,
    ) -> Self {
        let root = Context {
            visible: true,
            mask_group: None,
            mesh: None,
            source: InheritSource::root(),
            motion_root: InheritSource::root(),
            motion_independent: false,
            clip: None,
            mesh_chain: Vec::new(),
        };
        let ranks = motion.priority_ranks(time);
        let mut counter = 0i64;
        let mut mask_groups = Vec::new();
        let mut next_scope = 1usize;
        let mut layers = Vec::with_capacity(motion.layers.len());
        for (index, layer) in motion.layers.iter().enumerate() {
            layers.push(evaluate_layer(
                model,
                layer,
                &index.to_string(),
                time,
                variables,
                &root,
                0,
                &ranks,
                &motion.parameters,
                &[],
                0,
                None,
                &mut mask_groups,
                &mut next_scope,
                &mut counter,
            ));
        }
        apply_anchor_pass(&mut layers);
        let mut pose = Self {
            object: object.to_string(),
            motion: motion_name.to_string(),
            time,
            variables: variables.clone(),
            mask_groups,
            layers,
        };
        resolve_mask_sources(&pose.layers, &mut pose.mask_groups);
        pose
    }

    /// World position of the first layer with `label`.
    ///
    /// Physics controllers anchor to the finalized StepFrame XYZ of their
    /// `baseLayer` (`center_bust`, `center_fronthair`, ...). The label is
    /// resolved over the whole evaluated tree, including nested motions.
    pub fn layer_position(&self, label: Option<&str>) -> Option<[f64; 3]> {
        let label = label.filter(|label| !label.is_empty())?;
        fn find(layers: &[LayerPose], label: &str) -> Option<[f64; 3]> {
            for layer in layers {
                if layer.label == label {
                    let mut position = layer.world_origin;
                    if matches!(layer.kind, 1 | 5) {
                        let mut xy = [position[0] as f32, position[1] as f32];
                        for entry in layer.mesh_chain.iter().rev() {
                            let Some(inverse) = entry.world.inverse() else {
                                continue;
                            };
                            let local = inverse.apply([xy[0] as f64, xy[1] as f64]);
                            let Some(warped) =
                                entry.patch.warp_local([local[0] as f32, local[1] as f32])
                            else {
                                continue;
                            };
                            let world = entry.world.apply([warped[0] as f64, warped[1] as f64]);
                            xy = [world[0] as f32, world[1] as f32];
                        }
                        position[0] = xy[0] as f64;
                        position[1] = xy[1] as f64;
                    }
                    return Some(position);
                }
                if let Some(position) = find(&layer.children, label) {
                    return Some(position);
                }
            }
            None
        }
        find(&self.layers, label)
    }

    /// Flatten all visible drawable layers.
    pub fn drawables(&self) -> Vec<&LayerPose> {
        fn walk<'a>(layers: &'a [LayerPose], out: &mut Vec<&'a LayerPose>) {
            for layer in layers {
                if layer.visible && layer.drawable {
                    out.push(layer);
                }
                walk(&layer.children, out);
            }
        }
        let mut out = Vec::new();
        walk(&self.layers, &mut out);
        // The native manager emits DrawFrameInfos in `priority` order and then
        // sorts them by resolved Z (`LayerInfo +620`, copied to DFI +36). Z is
        // the accumulated coordinate Z, so rear hair (-10) precedes the face
        // (3) and the front hair (4) precedes the eyes (5/6). Priority order
        // is only the deterministic tie-break for equal Z.
        out.sort_by(|a, b| {
            a.world_origin[2]
                .total_cmp(&b.world_origin[2])
                .then_with(|| a.priority_path.cmp(&b.priority_path))
                .then_with(|| a.path.cmp(&b.path))
        });
        out
    }
}

/// Ancestor state selected by the native `inheritParent` walk.
///
/// A layer whose `inheritMask` carries `0x400000` is transparent for
/// inheritance: its descendants keep using the first ancestor above that
/// transparent chain. The source stores the linear block, accumulated linear
/// channels, XYZ location and coordinate plane separately, exactly like the
/// native player (`sub_10331060`).
#[derive(Debug, Clone, Copy)]
struct MeshSyncState {
    patch: MeshPatch,
    mask: i64,
    coordinate: i64,
}

#[derive(Debug, Clone, Copy)]
struct InheritSource {
    linear: Affine2,
    linear_state: LinearState,
    location: [f64; 3],
    coordinate: i64,
    opacity: f64,
    mesh_sync: Option<MeshSyncState>,
}

impl InheritSource {
    fn root() -> Self {
        Self {
            linear: Affine2::identity(),
            linear_state: LinearState::default(),
            location: [0.0; 3],
            coordinate: 0,
            opacity: 1.0,
            mesh_sync: None,
        }
    }
}

#[derive(Clone)]
struct Context {
    visible: bool,
    mask_group: Option<usize>,
    /// Accumulated ancestor sprite patch inherited through shape sync.
    mesh: Option<MeshPatch>,
    /// Ancestor used by this layer's own inheritance.
    source: InheritSource,
    /// Synthetic root of the current motion player, used for partial-inherit
    /// root compensation.
    motion_root: InheritSource,
    /// `motionIndependentLayerInherit` of the nested player that owns this
    /// context.
    motion_independent: bool,
    /// Type-7 clip rectangle inherited from ancestors.
    clip: Option<[f64; 4]>,
    /// Ancestor shape-sync chain.
    mesh_chain: Vec<MeshChainEntry>,
}

#[allow(clippy::too_many_arguments)]
fn evaluate_layer(
    model: &Model,
    layer: &Layer,
    path: &str,
    fallback_time: f64,
    variables: &BTreeMap<String, f64>,
    parent: &Context,
    depth: usize,
    ranks: &BTreeMap<i64, i64>,
    motion_parameters: &[crate::schema::ParameterDef],
    priority_prefix: &[i64],
    scope: usize,
    inherited_mask_group: Option<usize>,
    mask_groups: &mut Vec<MaskGroup>,
    next_scope: &mut usize,
    counter: &mut i64,
) -> LayerPose {
    let layer_index = *counter;
    *counter += 1;
    let rank = ranks.get(&layer_index).copied().unwrap_or(layer_index);
    let mut priority_path = priority_prefix.to_vec();
    priority_path.push(rank);
    let sample_time = parameter_time(
        model,
        motion_parameters,
        layer.parameterize.as_ref(),
        variables,
    )
    .unwrap_or(fallback_time);
    let inherit_mask = layer.inherit_mask.unwrap_or(0);
    let mut state = evaluate_frames_with_easing(
        &layer.frames,
        sample_time,
        None,
        &model.easing,
        layer.coordinate.unwrap_or(0),
    );
    if let Some(sync) = parent.source.mesh_sync {
        apply_mesh_sync_child_state(&mut state, sync, inherit_mask);
    }
    // Opacity uses the selected inheritance source (`sub_10331060`), not the
    // direct parent. Transparent (0x400000) parents must not multiply their
    // own opacity into descendants.
    let base_opacity = if (inherit_mask & INHERIT_OPACITY) != 0 {
        parent.source.opacity
    } else if !parent.motion_independent {
        parent.motion_root.opacity
    } else {
        1.0
    };
    let opacity = (base_opacity * (state.opacity / 255.0)).clamp(0.0, 1.0);

    let own = LinearState {
        flip_x: state.flip_x,
        flip_y: state.flip_y,
        rotation_deg: state.rotation_deg,
        scale: state.scale,
        shear: state.shear,
    };
    let source = parent.source;
    let inherited = inherit_state(own, source.linear_state, inherit_mask);
    let own_linear = build_linear(&layer.transform_order, own);
    let linear = if (inherit_mask & INHERIT_LINEAR_ALL) == INHERIT_LINEAR_ALL {
        // Native fast path: ancestor matrix followed by the local matrix.
        source.linear.then(own_linear)
    } else if !parent.motion_independent {
        // Partial inheritance: remove the motion-root channels from the
        // composite state, build the relative matrix, then reapply the root
        // matrix as an outer transform.
        let relative = remove_motion_root_linear_state(
            inherited,
            parent.motion_root.linear_state,
            inherit_mask,
        );
        let relative_linear = build_linear(&layer.transform_order, relative);
        parent.motion_root.linear.then(relative_linear)
    } else {
        build_linear(&layer.transform_order, inherited)
    };
    let local = own_linear;
    let world_origin = map_coordinate(source, state.coord);
    let world = Affine2 {
        m11: linear.m11,
        m12: linear.m12,
        m21: linear.m21,
        m22: linear.m22,
        tx: world_origin[0],
        ty: world_origin[1],
    };

    let drawable = matches!(layer.kind, 0 | 10 | 12);
    let has_geometry = state.src.is_some() || state.icon.is_some() || state.mesh_bp.is_some();
    let visible_chain = parent.visible && opacity > 0.0;
    let visible = visible_chain && drawable && has_geometry;

    // Type-7 clip rectangle (`sub_10352360`). An active type-7 layer replaces
    // the inherited rect with its transformed +/-16 box intersected with the
    // inherited rectangle; inactive type-7 layers simply inherit.
    let clip_rect = if layer.kind == 7 && visible_chain {
        let own = type7_local_bounds(world, world_origin, state.origin);
        intersect_clip_rect(parent.clip, own)
    } else {
        parent.clip
    };

    // `stencilType & 4` marks a composite-mask owner. Its descendants are
    // clipped by the union of the named source layers, resolved in this
    // motion scope (native sub_103390C0 second pass).
    let mut child_mask_group = inherited_mask_group;
    if layer.stencil_type.unwrap_or(0) & 0x4 != 0
        && !layer.stencil_composite_mask_layer_list.is_empty()
    {
        let id = mask_groups.len();
        mask_groups.push(MaskGroup {
            owner_path: path.to_string(),
            owner_scope: scope,
            sources: layer.stencil_composite_mask_layer_list.clone(),
            source_paths: Vec::new(),
        });
        child_mask_group = Some(id);
    }

    // Native sprite-mesh inheritance (`sub_10353CF0` / `sub_1038B340`).
    //
    // A layer whose frame carries `mesh.bp` always draws its own sprite
    // through that patch. Descendants additionally inherit the accumulated
    // patch only when the layer is a shape-sync node
    // (`meshTransform == 1` and `meshSyncChildMask & 8`); other sync bits
    // only warp the child's coordinate/angle/scale (`sub_10335500`) and do
    // not propagate the patch. Patches are combined by adding control-point
    // displacement from identity, not by composing the Bezier mappings.
    let own_mesh = state.mesh_bp.as_deref().map(|bp| {
        MeshPatch::from_bp(Some(bp))
            .with_division(layer.mesh_division.unwrap_or(1) as u32)
            .with_domain(layer_mesh_domain(layer))
    });
    let combinator = layer.mesh_combinator.as_ref().and_then(|combinator| {
        evaluate_mesh_combinator(combinator, layer, motion_parameters, variables)
    });
    let self_mesh = combine_patch(
        combine_patch(parent.mesh, own_mesh),
        combinator.as_ref().and_then(|value| value.self_patch),
    );
    // Native `sub_10353CF0` activates a mesh-chain node only for
    // `meshTransform == 1`; bit 8 is the shape-sync half of the mask.
    let shape_sync = layer.mesh_transform.unwrap_or(0) == 1
        && layer.mesh_sync_child_mask.unwrap_or(0) & 0x8 != 0;
    let child_mesh = if shape_sync {
        combine_patch(
            self_mesh,
            combinator.as_ref().and_then(|value| value.child_patch),
        )
    } else {
        parent.mesh
    };

    let mut child_chain = parent.mesh_chain.clone();
    if shape_sync {
        if let Some(patch) = own_mesh.filter(|patch| patch.domain.is_some()) {
            child_chain.push(MeshChainEntry { patch, world });
        }
    }

    let current_coordinate = layer.coordinate.unwrap_or(0);
    // A layer with inheritMask bit 0x400000 is transparent: descendants keep
    // the source that this layer itself inherited from.
    let child_source = if inherit_mask & 0x400000 != 0 {
        parent.source
    } else {
        InheritSource {
            linear: Affine2 {
                m11: linear.m11,
                m12: linear.m12,
                m21: linear.m21,
                m22: linear.m22,
                tx: 0.0,
                ty: 0.0,
            },
            linear_state: inherited,
            location: world_origin,
            coordinate: current_coordinate,
            opacity,
            mesh_sync: if layer.mesh_transform.unwrap_or(0) == 1
                && layer.mesh_sync_child_mask.unwrap_or(0) & 0x7 != 0
            {
                self_mesh
                    .filter(|patch| patch.domain.is_some())
                    .map(|patch| MeshSyncState {
                        patch,
                        mask: layer.mesh_sync_child_mask.unwrap_or(0) & 0x7,
                        coordinate: current_coordinate,
                    })
            } else {
                None
            },
        }
    };
    let context = Context {
        visible: visible_chain,
        mask_group: child_mask_group,
        mesh: child_mesh,
        source: child_source,
        motion_root: parent.motion_root,
        motion_independent: parent.motion_independent,
        clip: clip_rect,
        mesh_chain: child_chain,
    };
    let mut children: Vec<LayerPose> = layer
        .children
        .iter()
        .enumerate()
        .map(|(index, child)| {
            evaluate_layer(
                model,
                child,
                &format!("{path}/{index}"),
                // Parameterization is local to the layer: children keep the
                // player/motion timeline for their own frames unless they are
                // parameterized themselves.
                fallback_time,
                variables,
                &context,
                depth + 1,
                ranks,
                motion_parameters,
                priority_prefix,
                scope,
                child_mask_group,
                mask_groups,
                next_scope,
                counter,
            )
        })
        .collect();

    // Type 3 layers reference a nested motion: `src` is the object name and
    // `icon` is the motion name. Child time = parent_time - frame_start +
    // motion timeOffset (native sub_10355BF0 behavior).
    if layer.kind == 3 && depth < MAX_NESTED_MOTION_DEPTH {
        if let (Some(object_name), Some(icon)) = (&state.src, &state.icon) {
            if let Some(nested) = model.motion(object_name, &icon.raw) {
                // A referenced motion may declare its own `parameterize` index
                // (eyes/mouth/brows/tears/cheeks). That variable drives the
                // nested motion's timeline even when the proxy layer has no
                // parameterize of its own.
                let nested_base = if let Some(index) = nested.parameterize {
                    parameter_time(
                        model,
                        &nested.parameters,
                        Some(&ParameterRef::Index(index)),
                        variables,
                    )
                    .unwrap_or(fallback_time)
                } else if layer.parameterize.is_some() {
                    sample_time
                } else {
                    fallback_time
                };
                let child_time =
                    (nested_base - state.frame_start + state.motion_time_offset).max(0.0);
                let nested_ranks = nested.priority_ranks(child_time);
                let nested_scope = *next_scope;
                *next_scope += 1;
                // A nested type-3 layer owns a fresh native MMotionPlayer. Its
                // synthetic root is the enclosing layer's current composite
                // state (`sub_1033ED90` / `apply_motion_layer_inherit`).
                let nested_root = InheritSource {
                    linear: Affine2 {
                        m11: world.m11,
                        m12: world.m12,
                        m21: world.m21,
                        m22: world.m22,
                        tx: 0.0,
                        ty: 0.0,
                    },
                    linear_state: inherited,
                    location: world_origin,
                    coordinate: current_coordinate,
                    opacity,
                    mesh_sync: None,
                };
                let mut nested_context = context.clone();
                nested_context.source = nested_root;
                nested_context.motion_root = nested_root;
                nested_context.motion_independent =
                    layer.motion_independent_layer_inherit.unwrap_or(0) != 0;
                let mut nested_counter = 0i64;
                for (index, nested_layer) in nested.layers.iter().enumerate() {
                    children.push(evaluate_layer(
                        model,
                        nested_layer,
                        &format!("{path}/@{}:{}/{}", object_name, icon.raw, index),
                        child_time,
                        variables,
                        &nested_context,
                        depth + 1,
                        &nested_ranks,
                        &nested.parameters,
                        &priority_path,
                        nested_scope,
                        context.mask_group,
                        mask_groups,
                        next_scope,
                        &mut nested_counter,
                    ));
                }
            }
        }
    }

    LayerPose {
        label: layer.label.clone(),
        path: path.to_string(),
        kind: layer.kind,
        sample_time,
        state,
        local,
        world,
        world_origin,
        opacity,
        visible,
        drawable,
        mesh_division: layer.mesh_division.unwrap_or(1).max(1) as u32,
        mesh_transform: layer.mesh_transform.unwrap_or(0),
        mesh_combine: layer.mesh_combine.unwrap_or(0) != 0,
        clip_rect,
        anchor_mode: layer.anchor.unwrap_or(0),
        stencil_type: layer.stencil_type.unwrap_or(0),
        mask_group: inherited_mask_group,
        scope,
        mesh: self_mesh,
        mesh_chain: parent.mesh_chain.clone(),
        priority_path,
        children,
    }
}

#[derive(Debug, Clone, Copy)]
struct AnchorRequest {
    target: [f64; 3],
    mode: i32,
    current: [f64; 3],
    flip_x: bool,
    flip_y: bool,
}

/// Native type-9 anchor pass (`sub_10351120`).
///
/// All anchors in the motion scope are evaluated first, then every authored
/// layer is shifted by one aggregate XYZ offset. Exact constraints win over
/// the positive-side constraint, which wins over the negative-side one.
fn apply_anchor_pass(layers: &mut [LayerPose]) {
    let mut requests = Vec::new();
    let fallback = layers.first().map(|layer| layer.world_origin);
    let positions = layer_positions(layers);
    collect_anchor_requests(layers, &positions, fallback, &mut requests);
    if requests.is_empty() {
        return;
    }

    let mut low_active = [false; 3];
    let mut exact_active = [false; 3];
    let mut high_active = [false; 3];
    let mut low_shift = [f64::INFINITY; 3];
    let mut exact_shift = [0.0; 3];
    let mut high_shift = [f64::NEG_INFINITY; 3];

    for request in requests {
        let mode = anchor_mode_after_flip(request.mode, request.flip_x, request.flip_y);
        if !(0..=8).contains(&mode) {
            continue;
        }
        let axis = (mode / 3) as usize;
        let kind = mode % 3;
        let delta = request.target[axis] - request.current[axis];
        match kind {
            0 => {
                if request.current[axis] > request.target[axis] {
                    low_active[axis] = true;
                    low_shift[axis] = low_shift[axis].min(delta);
                }
            }
            1 => {
                exact_active[axis] = true;
                exact_shift[axis] = delta;
            }
            2 => {
                if request.target[axis] > request.current[axis] {
                    high_active[axis] = true;
                    high_shift[axis] = high_shift[axis].max(delta);
                }
            }
            _ => unreachable!(),
        }
    }

    let shift = [0, 1, 2].map(|axis| {
        if exact_active[axis] {
            exact_shift[axis]
        } else if high_active[axis] {
            high_shift[axis]
        } else if low_active[axis] {
            low_shift[axis]
        } else {
            0.0
        }
    });
    if shift == [0.0; 3] {
        return;
    }
    fn walk(layers: &mut [LayerPose], shift: [f64; 3]) {
        for layer in layers {
            for (axis, value) in shift.iter().enumerate() {
                layer.world_origin[axis] += value;
            }
            layer.world.tx += shift[0];
            layer.world.ty += shift[1];
            if let Some(rect) = &mut layer.clip_rect {
                rect[0] += shift[0];
                rect[1] += shift[1];
                rect[2] += shift[0];
                rect[3] += shift[1];
            }
            walk(&mut layer.children, shift);
        }
    }
    walk(layers, shift);
}

fn collect_anchor_requests(
    layers: &[LayerPose],
    positions: &BTreeMap<String, [f64; 3]>,
    fallback: Option<[f64; 3]>,
    out: &mut Vec<AnchorRequest>,
) {
    for layer in layers {
        if layer.kind == 9 {
            if let Some(target) = layer.state.anchor_target.as_deref() {
                if !target.is_empty() {
                    if let Some(target_position) = positions.get(target).copied().or(fallback) {
                        out.push(AnchorRequest {
                            target: target_position,
                            mode: layer.anchor_mode as i32,
                            current: layer.world_origin,
                            flip_x: layer.state.flip_x,
                            flip_y: layer.state.flip_y,
                        });
                    }
                }
            }
        }
        collect_anchor_requests(&layer.children, positions, fallback, out);
    }
}

fn layer_positions(layers: &[LayerPose]) -> BTreeMap<String, [f64; 3]> {
    fn walk(layers: &[LayerPose], out: &mut BTreeMap<String, [f64; 3]>) {
        for layer in layers {
            out.entry(layer.label.clone()).or_insert(layer.world_origin);
            walk(&layer.children, out);
        }
    }
    let mut out = BTreeMap::new();
    walk(layers, &mut out);
    out
}

fn anchor_mode_after_flip(mut mode: i32, flip_x: bool, flip_y: bool) -> i32 {
    if flip_x {
        if mode == 0 {
            mode = 2;
        } else if mode == 2 {
            mode = 3;
        }
    }
    if flip_y {
        if mode == 3 {
            mode = 5;
        } else if mode == 5 {
            mode = 3;
        }
    }
    mode
}

fn type7_local_bounds(world: Affine2, origin: [f64; 3], frame_offset: [f64; 2]) -> [f64; 4] {
    // Native `sub_10352360`:
    //   offset = M * (ox, oy)
    //   lo = M * (-16, -16), hi = M * (16, 16)
    //   p = raw_position + lo/hi - offset
    let transform = |x: f64, y: f64| world.apply_linear([x, y]);
    let offset = transform(frame_offset[0], frame_offset[1]);
    let lo = transform(-16.0, -16.0);
    let hi = transform(16.0, 16.0);
    let p0 = [origin[0] + lo[0] - offset[0], origin[1] + lo[1] - offset[1]];
    let p1 = [origin[0] + hi[0] - offset[0], origin[1] + hi[1] - offset[1]];
    [
        p0[0].min(p1[0]),
        p0[1].min(p1[1]),
        p0[0].max(p1[0]),
        p0[1].max(p1[1]),
    ]
}

fn intersect_clip_rect(inherited: Option<[f64; 4]>, own: [f64; 4]) -> Option<[f64; 4]> {
    let Some(parent) = inherited else {
        return Some(own);
    };
    let rect = [
        parent[0].max(own[0]),
        parent[1].max(own[1]),
        parent[2].min(own[2]),
        parent[3].min(own[3]),
    ];
    (rect[0] <= rect[2] && rect[1] <= rect[3]).then_some(rect)
}

fn map_coordinate(source: InheritSource, coord: [f64; 3]) -> [f64; 3] {
    // `sub_10331060` maps XY for coordinate==0 and XZ otherwise; the selected
    // source's location is added afterwards.
    if source.coordinate != 0 {
        let mapped = source.linear.apply_linear([coord[0], coord[2]]);
        [
            source.location[0] + mapped[0],
            source.location[1] + coord[1],
            source.location[2] + mapped[1],
        ]
    } else {
        let mapped = source.linear.apply_linear([coord[0], coord[1]]);
        [
            source.location[0] + mapped[0],
            source.location[1] + mapped[1],
            source.location[2] + coord[2],
        ]
    }
}

/// Fold an inherited sprite patch with a layer's own patch.
///
/// Native `meshCombine` (`sub_1038B340`) is pointwise displacement addition
/// from identity, not Bezier composition; see
/// [`MeshPatch::combined_with`].
fn combine_patch(base: Option<MeshPatch>, own: Option<MeshPatch>) -> Option<MeshPatch> {
    match (base, own) {
        (Some(base), Some(own)) => Some(base.combined_with(&own)),
        (None, own) => own,
        (base, None) => base,
    }
}

/// Resolve composite-mask source labels to concrete layer paths.
///
/// `stencilCompositeMaskLayerList` stores player-local layer labels. The
/// native resolver (`sub_10337560`) finds the label in the owner's own
/// motion scope and accepts only type-0/type-3 sources:
///
/// - a type-0 source contributes its own visible geometry;
/// - a type-3 source is a proxy for a nested player, so the resolver expands
///   the referenced motion and contributes every emitted sprite below it
///   (recursively, including deeper nested players).
///
/// Blank proxies (such as the optional `add_mask` helper) have no geometry
/// and therefore do not widen the mask.
fn resolve_mask_sources(layers: &[LayerPose], groups: &mut [MaskGroup]) {
    if groups.is_empty() {
        return;
    }
    struct Source<'a> {
        path: &'a str,
        scope: usize,
        label: &'a str,
        kind: i64,
        geometry: bool,
    }
    fn walk<'a>(layers: &'a [LayerPose], out: &mut Vec<Source<'a>>) {
        for layer in layers {
            let geometry = layer.visible
                && layer.opacity > 0.0
                && layer.drawable
                && layer
                    .state
                    .src
                    .as_deref()
                    .is_some_and(|src| !src.is_empty() && src != "blank");
            out.push(Source {
                path: &layer.path,
                scope: layer.scope,
                label: &layer.label,
                kind: layer.kind,
                geometry,
            });
            walk(&layer.children, out);
        }
    }
    let mut sources = Vec::new();
    walk(layers, &mut sources);
    for group in groups.iter_mut() {
        let mut paths = Vec::new();
        for label in &group.sources {
            for source in sources
                .iter()
                .filter(|source| source.scope == group.owner_scope && source.label == label)
            {
                if source.kind == 3 {
                    let prefix = format!("{}/@", source.path);
                    for descendant in &sources {
                        if descendant.geometry
                            && descendant.path.starts_with(&prefix)
                            && !paths
                                .iter()
                                .any(|path: &String| path.as_str() == descendant.path)
                        {
                            paths.push(descendant.path.to_string());
                        }
                    }
                } else if source.geometry
                    && !paths
                        .iter()
                        .any(|path: &String| path.as_str() == source.path)
                {
                    paths.push(source.path.to_string());
                }
            }
        }
        group.source_paths = paths;
    }
}

/// Parameter sample time for a layer.
///
/// `layer.parameterize` resolves to an index in the **owning motion's**
/// `parameter` table (or to an inline object). The value is optionally
/// truncated (`discretization`), clamped to the authored range, then mapped
/// with `division` into the layer-local frame time.
pub fn parameter_time(
    model: &Model,
    motion_parameters: &[crate::schema::ParameterDef],
    reference: Option<&ParameterRef>,
    variables: &BTreeMap<String, f64>,
) -> Option<f64> {
    let reference = reference?;
    let resolved = match reference {
        ParameterRef::Index(index) if *index >= 0 => {
            motion_parameters.get(*index as usize).cloned()
        }
        _ => model.parameter(reference),
    };
    let Some(parameter) = resolved else {
        // A parameterized layer with an unresolved index samples time 0 in the
        // official runtime (the parameter table is absent or shorter).
        return Some(0.0);
    };
    let Some(id) = parameter.id else {
        return Some(0.0);
    };
    // A variable that was never set has the E-mote neutral value 0, which is
    // then mapped through the authored range (for -30..30 that is frame 30,
    // not frame 0).
    let value = variables.get(&id).copied().unwrap_or(0.0);
    let mut sample = if parameter.discretization {
        value.trunc()
    } else {
        value
    };
    let low = parameter.range_begin.min(parameter.range_end);
    let high = parameter.range_begin.max(parameter.range_end);
    sample = sample.clamp(low, high);
    let span = parameter.range_end - parameter.range_begin;
    if !span.is_finite() || span.abs() <= f64::EPSILON || parameter.division <= 0.0 {
        return Some(0.0);
    }
    Some((sample - parameter.range_begin) * parameter.division / span)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transform_order_and_angle_are_deterministic() {
        let state = LinearState {
            rotation_deg: 90.0,
            scale: [2.0, 3.0],
            ..LinearState::default()
        };
        let transform = build_linear(&[0, 3, 2, 1], state);
        let point = transform.apply([1.0, 0.0]);
        assert!(point[0].abs() < 1e-9);
        assert!((point[1] - 2.0).abs() < 1e-9);
    }

    #[test]
    fn angle_interpolation_takes_shortest_path() {
        assert!((lerp_angle(350.0, 10.0, 0.5) - 0.0).abs() < 1e-9);
        assert!((lerp_angle(10.0, 350.0, 0.5) - 0.0).abs() < 1e-9);
    }

    #[test]
    fn frame_easing_table_reshapes_coordinate_interpolation() {
        let curve = EasingCurve {
            pieces: vec![EasingPiece {
                x: vec![0.0, 1.0],
                y: vec![0.0, 1.0],
                p: vec![0.0, 6.0],
            }],
        };
        let frames = vec![
            Frame {
                time: 0,
                kind: 3,
                time_quantization: None,
                content: Some(Content {
                    coord: Some([0.0, 0.0, 0.0]),
                    coord_easing: Some(0),
                    ..Content::default()
                }),
            },
            Frame {
                time: 10,
                kind: 2,
                time_quantization: None,
                content: Some(Content {
                    coord: Some([10.0, 0.0, 0.0]),
                    ..Content::default()
                }),
            },
        ];
        let state = evaluate_frames_with_easing(&frames, 5.0, None, &[curve], 0);
        // At t=0.5 the native spline returns 0.375.
        // At t=0.5 the native spline returns 0.125 for p=[0,6].
        assert!(
            (state.coord[0] - 1.25).abs() < 1.0e-9,
            "coord={:?}",
            state.coord
        );
    }

    #[test]
    fn native_color_lane_interpolation_matches_native() {
        assert_eq!(lerp_color(0x0000_0000, 0xFFFF_FFFF, 0.0), 0x0000_0000);
        assert_eq!(lerp_color(0x0000_0000, 0xFFFF_FFFF, 0.5), 0x7F7F_7F7F);
        assert_eq!(lerp_color(0x0000_0000, 0xFFFF_FFFF, 1.0), 0xFFFF_FFFF);
        assert_eq!(lerp_color(0x1234_5678, 0x1234_5678, 0.25), 0x1234_5678);
    }

    #[test]
    fn type7_local_bounds_uses_world_transform_and_frame_offset() {
        let world = Affine2 {
            m11: 2.0,
            m12: 0.0,
            m21: 0.0,
            m22: 2.0,
            tx: 100.0,
            ty: 100.0,
        };
        let rect = type7_local_bounds(world, [100.0, 100.0, 0.0], [4.0, -4.0]);
        // offset = (8,-8), lo = (-32,-32), hi = (32,32)
        assert_eq!(rect, [60.0, 76.0, 124.0, 140.0]);
        let clipped = intersect_clip_rect(Some([0.0, 0.0, 100.0, 100.0]), rect).unwrap();
        assert_eq!(clipped, [60.0, 76.0, 100.0, 100.0]);
        assert!(intersect_clip_rect(Some([0.0, 0.0, 10.0, 10.0]), rect).is_none());
    }

    #[test]
    fn anchor_mode_flip_mapping_matches_native() {
        assert_eq!(anchor_mode_after_flip(0, true, false), 2);
        assert_eq!(anchor_mode_after_flip(2, true, false), 3);
        assert_eq!(anchor_mode_after_flip(3, false, true), 5);
        assert_eq!(anchor_mode_after_flip(5, false, true), 3);
        assert_eq!(anchor_mode_after_flip(8, true, true), 8);
    }

    #[test]
    fn anchor_pass_shifts_every_layer_to_exact_target() {
        let mut target = drawable_layer("T", 0.0, &[]);
        target.world_origin = [10.0, 0.0, 0.0];
        let mut anchor = drawable_layer("A", 0.0, &[]);
        anchor.kind = 9;
        anchor.anchor_mode = 1;
        anchor.state.anchor_target = Some("T".to_string());
        anchor.world_origin = [50.0, 0.0, 0.0];
        let mut layers = vec![target, anchor];
        apply_anchor_pass(&mut layers);
        assert!((layers[0].world_origin[0] + 30.0).abs() < 1.0e-9);
        assert!((layers[1].world_origin[0] - 10.0).abs() < 1.0e-9);
    }

    #[test]
    fn mesh_chain_warps_type1_layer_position() {
        let mut patch = MeshPatch::identity();
        patch.domain = Some([-830.0, -900.0, 1660.0, 1800.0]);
        for point in &mut patch.control_points {
            point[1] += 0.005;
        }
        let mut layer = drawable_layer("center_fronthair", 0.0, &[]);
        layer.kind = 1;
        layer.world_origin = [6.0, -406.0, 0.0];
        layer.mesh_chain = vec![MeshChainEntry {
            patch,
            world: Affine2 {
                tx: 7.0,
                ty: 144.0,
                ..Affine2::identity()
            },
        }];
        let pose = Pose {
            object: String::new(),
            motion: String::new(),
            time: 0.0,
            variables: BTreeMap::new(),
            mask_groups: Vec::new(),
            layers: vec![layer],
        };
        let position = pose.layer_position(Some("center_fronthair")).unwrap();
        assert!((position[1] + 397.0).abs() < 0.5, "{position:?}");
    }

    #[test]
    fn type_zero_frame_holds_preceding_content() {
        let frames = [
            Frame {
                time: 0,
                kind: 2,
                time_quantization: None,
                content: Some(Content {
                    coord: Some([7.0, -3.0, 0.0]),
                    ..Content::default()
                }),
            },
            Frame {
                time: 10,
                kind: 0,
                time_quantization: None,
                content: None,
            },
        ];
        let state = evaluate_frames(&frames, 10.0, None);
        assert!(state.valid);
        assert_eq!(state.coord, [7.0, -3.0, 0.0]);
        assert_eq!(state.kind, 0);
    }

    fn drawable_layer(label: &str, z: f64, priority_path: &[i64]) -> LayerPose {
        LayerPose {
            label: label.to_string(),
            path: label.to_string(),
            kind: 0,
            sample_time: 0.0,
            state: FrameState::default(),
            local: Affine2::identity(),
            world: Affine2::identity(),
            world_origin: [0.0, 0.0, z],
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
            mesh_chain: Vec::new(),
            priority_path: priority_path.to_vec(),
            children: Vec::new(),
        }
    }

    #[test]
    fn mesh_combinator_interpolates_authored_patches() {
        fn flat(offset_x: f32) -> [f32; 32] {
            let mut out = [0.0f32; 32];
            for row in 0..4 {
                for col in 0..4 {
                    let index = row * 4 + col;
                    out[index * 2] = col as f32 / 3.0 + offset_x;
                    out[index * 2 + 1] = row as f32 / 3.0;
                }
            }
            out
        }
        let entry = crate::schema::MeshCombinatorEntry {
            key: Some("p".into()),
            range_begin: 0.0,
            range_end: 1.0,
            mesh_count: 2,
            neutral_index: -1,
            values: vec![flat(0.0), flat(0.1)],
        };
        let mut variables = BTreeMap::new();
        variables.insert("p".to_string(), 0.5);
        let patch = evaluate_one_mesh_combinator(&entry, false, &variables, 20, None).unwrap();
        let sample = patch.sample(0.5, 0.5);
        assert!((sample[0] - 0.55).abs() < 1e-5);
        assert!((sample[1] - 0.5).abs() < 1e-5);
    }

    #[test]
    fn mesh_sync_warps_child_coordinate() {
        let mut patch = MeshPatch::identity();
        for point in &mut patch.control_points {
            point[0] += 0.1;
        }
        patch.domain = Some([-50.0, -40.0, 100.0, 80.0]);
        let mut state = FrameState {
            coord: [10.0, 20.0, 0.0],
            ..FrameState::default()
        };
        apply_mesh_sync_child_state(
            &mut state,
            MeshSyncState {
                patch,
                mask: 0x1,
                coordinate: 0,
            },
            0x7FC,
        );
        assert!((state.coord[0] - 20.0).abs() < 1e-4);
        assert!((state.coord[1] - 20.0).abs() < 1e-4);
    }

    #[test]
    fn partial_inheritance_removes_motion_root_channels() {
        let state = LinearState {
            flip_x: true,
            flip_y: true,
            rotation_deg: 30.0,
            scale: [2.0, 3.0],
            shear: [5.0, 6.0],
        };
        let root = LinearState {
            flip_x: true,
            flip_y: true,
            rotation_deg: 10.0,
            scale: [2.0, 3.0],
            shear: [1.0, 2.0],
        };
        let relative = remove_motion_root_linear_state(state, root, 0x1FC);
        assert!(!relative.flip_x);
        assert!(!relative.flip_y);
        assert!((relative.rotation_deg - 20.0).abs() < 1e-9);
        assert!((relative.scale[0] - 1.0).abs() < 1e-9);
        assert!((relative.scale[1] - 1.0).abs() < 1e-9);
        assert!((relative.shear[0] - 4.0).abs() < 1e-9);
        assert!((relative.shear[1] - 4.0).abs() < 1e-9);
    }

    #[test]
    fn mask_source_type3_expands_nested_sprites() {
        let mut owner = drawable_layer("stencil", 0.0, &[0]);
        owner.kind = 12;
        owner.stencil_type = 5;

        let mut proxy = drawable_layer("eye_proxy", 0.0, &[1]);
        proxy.kind = 3;
        proxy.drawable = false;
        proxy.visible = false;

        let mut iris = drawable_layer("iris", 0.0, &[2]);
        iris.path = "eye_proxy/@face_parts:瞳孔/0".into();
        iris.state.src = Some("tex".into());

        let mut nested_blank = drawable_layer("blank_proxy", 0.0, &[3]);
        nested_blank.path = "eye_proxy/@face_parts:瞳孔/1".into();
        nested_blank.state.src = Some("blank".into());

        let mut direct = drawable_layer("shirome", 0.0, &[4]);
        direct.state.src = Some("tex".into());

        let mut groups = vec![MaskGroup {
            owner_path: "stencil".into(),
            owner_scope: 0,
            sources: vec!["eye_proxy".into(), "shirome".into()],
            source_paths: Vec::new(),
        }];
        resolve_mask_sources(&[owner, proxy, iris, nested_blank, direct], &mut groups);
        assert_eq!(
            groups[0].source_paths,
            vec![
                "eye_proxy/@face_parts:瞳孔/0".to_string(),
                "shirome".to_string(),
            ]
        );
    }

    #[test]
    fn drawables_sort_by_resolved_z() {
        let pose = Pose {
            object: "test".into(),
            motion: "m".into(),
            time: 0.0,
            variables: BTreeMap::new(),
            mask_groups: Vec::new(),
            layers: vec![
                drawable_layer("face", 3.0, &[0, 9]),
                drawable_layer("rear_hair", -10.0, &[0, 20]),
                drawable_layer("front_hair", 4.0, &[0, 1]),
                drawable_layer("eye", 5.0, &[0, 0]),
            ],
        };
        let labels: Vec<&str> = pose
            .drawables()
            .iter()
            .map(|layer| layer.label.as_str())
            .collect();
        assert_eq!(labels, ["rear_hair", "face", "front_hair", "eye"]);
    }
}
