//! Layer/frame pose evaluation (M2).
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
use crate::schema::{Content, Frame, IconRef, Layer, Model, Motion, ParameterRef};

/// Full transform-order mask (all linear channels inherited).
const INHERIT_LINEAR_ALL: i64 = 0x1FC;
/// Opacity inheritance bit.
const INHERIT_OPACITY: i64 = 0x400;
/// Recursion guard for type-3 nested motions.
const MAX_NESTED_MOTION_DEPTH: usize = 32;

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
            color: [0xFFFF_FFFF; 4],
            opacity: 255.0,
            mask: 0,
            src: None,
            icon: None,
            mesh_bp: None,
            blend_mode: 0x10,
            blend_parameter: 0.0,
            motion_time_offset: 0.0,
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
        }
        if let Some(mesh) = &content.mesh {
            if let Some(bp) = &mesh.bp {
                self.mesh_bp = Some(bp.clone());
            }
        }
        if let Some(time_offset) = content.motion_time_offset {
            self.motion_time_offset = time_offset;
        }
    }
}

/// Evaluate one layer's frame list at `time`.
///
/// `previous` is the state before this evaluation (used by `type == 0`).
pub fn evaluate_frames(frames: &[Frame], time: f64, previous: Option<&FrameState>) -> FrameState {
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
    interpolate_state(&mut state, &next_state, t);
    state
}

fn interpolate_state(state: &mut FrameState, next: &FrameState, t: f64) {
    for axis in 0..3 {
        state.coord[axis] = lerp(state.coord[axis], next.coord[axis], t);
    }
    for axis in 0..2 {
        state.origin[axis] = lerp(state.origin[axis], next.origin[axis], t);
        state.scale[axis] = lerp(state.scale[axis], next.scale[axis], t);
        state.shear[axis] = lerp(state.shear[axis], next.shear[axis], t);
    }
    state.rotation_deg = lerp_angle(state.rotation_deg, next.rotation_deg, t);
    state.opacity = (lerp(state.opacity, next.opacity, t) + 0.5).floor();
    for channel in 0..4 {
        state.color[channel] = lerp_color(state.color[channel], next.color[channel], t);
    }
    if let (Some(current), Some(next)) = (&state.mesh_bp, &next.mesh_bp) {
        if current.len() == next.len() {
            let mut mesh = current.clone();
            for (slot, target) in mesh.iter_mut().zip(next.iter()) {
                *slot = lerp(*slot, *target, t);
            }
            state.mesh_bp = Some(mesh);
        }
    }
    if state.src.is_none() {
        state.src = next.src.clone();
    }
    if state.icon.is_none() {
        state.icon = next.icon.clone();
    }
    if state.mesh_bp.is_none() {
        state.mesh_bp = next.mesh_bp.clone();
    }
}

fn lerp(a: f64, b: f64, t: f64) -> f64 {
    a + (b - a) * t
}

fn lerp_color(a: u32, b: u32, t: f64) -> u32 {
    // Native interpolation uses an 8-bit weighted average; round to nearest.
    let value = (a as f64) + ((b as f64) - (a as f64)) * t;
    value.round().clamp(0.0, 255.0) as u32
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
    /// `stencilType`.
    pub stencil_type: i64,
    /// Composite-mask group inherited from the nearest type-12 owner with
    /// `stencilType & 4` and a non-empty `stencilCompositeMaskLayerList`.
    pub mask_group: Option<usize>,
    /// Motion scope this layer was evaluated in (nested motions get their own
    /// scope, so equal labels in the left/right eye never alias).
    pub scope: usize,
    /// Ancestor deformer meshes that warp this layer's geometry, outermost
    /// first. Each entry is applied leaf-to-root by the plan builder.
    pub deformers: Vec<Deformer>,
    /// Draw-order key: rank path from `priority` frames.
    pub priority_path: Vec<i64>,
    /// Child layers.
    pub children: Vec<LayerPose>,
}

/// One ancestor 4x4 Bezier deformer (`mesh.bp` on a blank proxy layer).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Deformer {
    /// Deformer local space -> world.
    pub world: Affine2,
    /// World -> deformer local space.
    pub inverse: Affine2,
    /// Deformer box width in local pixels (`icon` rect width).
    pub width: f64,
    /// Deformer box height in local pixels.
    pub height: f64,
    /// Flat row-major 4x4 control points (32 floats).
    pub patch: Vec<f64>,
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
            world: Affine2::identity(),
            world_origin: [0.0; 3],
            linear_state: LinearState::default(),
            coordinate: 0,
            opacity: 1.0,
            visible: true,
            mask_group: None,
            deformers: Vec::new(),
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
        Self {
            object: object.to_string(),
            motion: motion_name.to_string(),
            time,
            variables: variables.clone(),
            mask_groups,
            layers,
        }
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

struct Context {
    world: Affine2,
    world_origin: [f64; 3],
    linear_state: LinearState,
    coordinate: i64,
    opacity: f64,
    visible: bool,
    mask_group: Option<usize>,
    deformers: Vec<Deformer>,
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
    let state = evaluate_frames(&layer.frames, sample_time, None);
    let inherit_mask = layer.inherit_mask.unwrap_or(0);
    let base_opacity = if (inherit_mask & INHERIT_OPACITY) != 0 {
        parent.opacity
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
    let inherited = inherit_state(own, parent.linear_state, inherit_mask);
    let linear = if (inherit_mask & INHERIT_LINEAR_ALL) == INHERIT_LINEAR_ALL {
        build_linear(&layer.transform_order, own)
    } else {
        build_linear(&layer.transform_order, inherited)
    };
    let local = linear;
    let (mapped, world_origin) = map_coordinate(parent, state.coord);
    let world_linear = if (inherit_mask & INHERIT_LINEAR_ALL) == INHERIT_LINEAR_ALL {
        parent.world.then(local)
    } else {
        local
    };
    let world = Affine2 {
        m11: world_linear.m11,
        m12: world_linear.m12,
        m21: world_linear.m21,
        m22: world_linear.m22,
        tx: mapped[0],
        ty: mapped[1],
    };

    let drawable = matches!(layer.kind, 0 | 10 | 12);
    let has_geometry = state.src.is_some() || state.icon.is_some() || state.mesh_bp.is_some();
    let visible_chain = parent.visible && opacity > 0.0;
    let visible = visible_chain && drawable && has_geometry;

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
        });
        child_mask_group = Some(id);
    }

    // TODO(M2): parameterized deformer-mesh inheritance. The native chain
    // applies `mesh.bp` in an absolute-mesh convention that is not yet
    // evidenced; treating every blank proxy as a world-space warp tears the
    // character apart. Keep the pose field but do not warp until the
    // convention is proven.
    let child_deformers = parent.deformers.clone();

    let context = Context {
        world,
        world_origin,
        linear_state: inherited,
        coordinate: layer.coordinate.unwrap_or(0),
        opacity,
        visible: visible_chain,
        mask_group: child_mask_group,
        deformers: child_deformers,
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
                let mut nested_counter = 0i64;
                for (index, nested_layer) in nested.layers.iter().enumerate() {
                    children.push(evaluate_layer(
                        model,
                        nested_layer,
                        &format!("{path}/@{}:{}/{}", object_name, icon.raw, index),
                        child_time,
                        variables,
                        &context,
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
        stencil_type: layer.stencil_type.unwrap_or(0),
        mask_group: inherited_mask_group,
        scope,
        deformers: parent.deformers.clone(),
        priority_path,
        children,
    }
}

fn map_coordinate(parent: &Context, coord: [f64; 3]) -> ([f64; 2], [f64; 3]) {
    if parent.coordinate != 0 {
        let mapped = parent.world.apply_linear([coord[0], coord[2]]);
        let origin = [
            parent.world_origin[0] + mapped[0],
            parent.world_origin[1] + coord[1],
            parent.world_origin[2] + mapped[1],
        ];
        ([origin[0], origin[1]], origin)
    } else {
        let mapped = parent.world.apply_linear([coord[0], coord[1]]);
        let origin = [
            parent.world_origin[0] + mapped[0],
            parent.world_origin[1] + mapped[1],
            parent.world_origin[2] + coord[2],
        ];
        ([origin[0], origin[1]], origin)
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
            mask_group: None,
            scope: 0,
            deformers: Vec::new(),
            priority_path: priority_path.to_vec(),
            children: Vec::new(),
        }
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
