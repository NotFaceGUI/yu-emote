//! Bust/hair/parts physics controllers (M2).
//!
//! The official player runs three spring systems after motion evaluation:
//!
//! - `bustControl`: a single point spring (`EPBustControl`);
//! - `hairControl` / `partsControl`: a two-segment pendulum
//!   (`EPPendControl`) with a bend oscillator.
//!
//! This module reimplements the recovered integration order. Evidence is the
//! behavior documented by eluna from the official DLL (`sub_101D4300`,
//! `sub_10201AB0`, `sub_10202260`, `sub_1021A4F0`, `sub_10273DA0`,
//! `sub_10274A80`). The equations and integration order are adapted from
//! eluna (MPL-2.0); see NOTICE.
//!
//! Inputs are the `baseLayer` anchor from the evaluated pose, the player
//! rotation (zero in the CPU renderer), and optional outer force/wind
//! (not wired yet). Outputs are written to the animator variable map as
//! `bust_LR`, `hair_UD_front`, `quake_LR`, ... and sampled by the next pose.

use std::collections::BTreeMap;

use serde_json::Value;

use crate::pose::Pose;
use crate::schema::{Controller, Model};

const EPSILON: f64 = 1.192_092_9e-7;
const MAX_SUBSTEP: f64 = 1.0;
const PEND_BEND_TRIGGER: f64 = 28.0;
const PEND_BEND_STEP: f64 = 0.03125;

/// One bust spring configuration and persistent state.
#[derive(Debug, Clone, PartialEq)]
pub struct BustPhysics {
    /// Controller label.
    pub label: String,
    /// Whether the controller runs.
    pub enabled: bool,
    /// Pose layer used as the moving anchor.
    pub base_layer: Option<String>,
    /// Gravity coefficient.
    pub gravity: f64,
    /// Spring coefficient.
    pub spring: f64,
    /// Velocity damping.
    pub friction: f64,
    /// Horizontal output scale.
    pub scale_x: f64,
    /// Vertical output scale.
    pub scale_y: f64,
    /// Vertical rest bias.
    pub ofs: f64,
    /// Absolute root point.
    pub root: [f64; 3],
    /// Bob position.
    pub bob: [f64; 3],
    /// Bob velocity.
    pub vel: [f64; 3],
    /// Root-to-anchor XY delta captured on the first controller step.
    pub root_delta: [f64; 2],
    /// First group update uses dt = 0 (native `sub_10273DA0`).
    pub group_first_tick: bool,
    /// First controller step captures `root - anchor`.
    pub controller_first_tick: bool,
    /// Previous anchor without outer force.
    pub last_anchor: Option<[f64; 3]>,
    /// Output variable names.
    pub var_lr: Option<String>,
    /// Output variable name.
    pub var_ud: Option<String>,
}

/// One two-segment pendulum configuration and persistent state.
#[derive(Debug, Clone, PartialEq)]
pub struct PendPhysics {
    /// Controller label.
    pub label: String,
    /// Whether the controller runs.
    pub enabled: bool,
    /// Pose layer used as the moving anchor.
    pub base_layer: Option<String>,
    /// Segment lengths.
    pub length: [f64; 2],
    /// Gravity coefficient.
    pub gravity: f64,
    /// Horizontal damping.
    pub friction_x: f64,
    /// Vertical damping.
    pub friction_y: f64,
    /// Constraint response for the root segment.
    pub b_rate: f64,
    /// Constraint response for the second segment.
    pub v_bound: f64,
    /// Bend oscillator speed.
    pub bend_spd: f64,
    /// Bend oscillator volume.
    pub bend_vol: f64,
    /// Which segment drives `var_ud` (0 or 1).
    pub ud_eft: usize,
    /// Vertical rest bias.
    pub ofs: f64,
    /// Output scales per segment.
    pub scale_x: [f64; 2],
    /// Output scales per segment.
    pub scale_y: [f64; 2],
    /// Root offset from the anchor.
    pub root_offset: [f64; 3],
    /// Bob positions.
    pub bob: [[f64; 3]; 2],
    /// Bob velocities.
    pub vel: [[f64; 3]; 2],
    /// Bend oscillator phase.
    pub bend_phase: f64,
    /// Bend oscillator power.
    pub bend_power: f64,
    /// First group update.
    pub first_tick: bool,
    /// Previous anchor.
    pub last_anchor: Option<[f64; 3]>,
    /// Output variables.
    pub var_lr: Option<String>,
    /// Output variable.
    pub var_lrm: Option<String>,
    /// Output variable.
    pub var_ud: Option<String>,
}

/// Persistent physics state for one model.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PhysicsState {
    /// Bust controllers.
    pub bust: Vec<BustPhysics>,
    /// Hair and parts controllers, in metadata order (hair first).
    pub pend: Vec<PendPhysics>,
}

impl PhysicsState {
    /// Parse every enabled/disabled controller from model metadata.
    pub fn new(model: &Model) -> Self {
        let mut bust = Vec::new();
        for controller in &model.controllers.bust {
            if let Some(config) = BustPhysics::from_controller(controller) {
                bust.push(config);
            }
        }
        let mut pend = Vec::new();
        for controller in model
            .controllers
            .hair
            .iter()
            .chain(model.controllers.parts.iter())
        {
            if let Some(config) = PendPhysics::from_controller(controller) {
                pend.push(config);
            }
        }
        Self { bust, pend }
    }

    /// Whether any controller is present.
    pub fn is_empty(&self) -> bool {
        self.bust.is_empty() && self.pend.is_empty()
    }

    /// Reset every controller to its authored initial state.
    pub fn reset(&mut self) {
        for control in &mut self.bust {
            control.group_first_tick = true;
            control.controller_first_tick = true;
            control.root_delta = [0.0, 0.0];
            control.last_anchor = None;
        }
        for control in &mut self.pend {
            control.first_tick = true;
            control.last_anchor = None;
        }
    }

    /// Advance all controllers by `delta_ticks` and publish their variables.
    ///
    /// `angle_radians` is the player rotation. The CPU renderer has no
    /// outer rotation, so callers normally pass zero.
    pub fn step(
        &mut self,
        pose: &Pose,
        delta_ticks: f64,
        angle_radians: f64,
        variables: &mut BTreeMap<String, f64>,
    ) {
        if !delta_ticks.is_finite() || delta_ticks <= 0.0 {
            return;
        }
        for control in &mut self.bust {
            if !control.enabled {
                continue;
            }
            let anchor = pose
                .layer_position(control.base_layer.as_deref())
                .unwrap_or([0.0; 3]);
            control.step(delta_ticks, anchor, angle_radians, variables);
        }
        for control in &mut self.pend {
            if !control.enabled {
                continue;
            }
            let anchor = pose
                .layer_position(control.base_layer.as_deref())
                .unwrap_or([0.0; 3]);
            control.step(delta_ticks, anchor, angle_radians, variables);
        }
    }
}

impl BustPhysics {
    fn from_controller(controller: &Controller) -> Option<Self> {
        let raw = &controller.raw;
        let param = raw.get("param");
        Some(Self {
            label: controller.label.clone().unwrap_or_default(),
            enabled: controller
                .enabled
                .unwrap_or_else(|| field_f64(raw, "enabled", 1.0) != 0.0),
            base_layer: field_str(raw, "baseLayer"),
            gravity: field_f64(raw, "gravity", 0.0),
            spring: field_f64(raw, "spring", 0.0),
            friction: field_f64(raw, "friction", 0.0),
            scale_x: field_f64(raw, "scale_x", 1.0),
            scale_y: field_f64(raw, "scale_y", 1.0),
            ofs: param.map_or(0.0, |param| field_f64(param, "ofs", 0.0)),
            root: param.map_or([0.0; 3], |param| vec3(param, "op")),
            bob: param.map_or([0.0; 3], |param| vec3(param, "p")),
            vel: param.map_or([0.0; 3], |param| vec3(param, "pv")),
            root_delta: [0.0; 2],
            group_first_tick: true,
            controller_first_tick: true,
            last_anchor: None,
            var_lr: controller.variable_lr.clone(),
            var_ud: controller.variable_ud.clone(),
        })
    }

    fn step(
        &mut self,
        delta_ticks: f64,
        target_anchor: [f64; 3],
        angle_radians: f64,
        variables: &mut BTreeMap<String, f64>,
    ) {
        if self.group_first_tick {
            self.group_first_tick = false;
            self.step_once(0.0, target_anchor, angle_radians, variables);
            self.last_anchor = Some(target_anchor);
            return;
        }
        let previous = self.last_anchor.unwrap_or(target_anchor);
        let mut elapsed = 0.0;
        while (delta_ticks - EPSILON) > elapsed {
            let step = (delta_ticks - elapsed).min(MAX_SUBSTEP);
            elapsed += step;
            let ratio = (elapsed / delta_ticks).clamp(0.0, 1.0);
            let anchor = lerp3(previous, target_anchor, ratio);
            self.step_once(step, anchor, angle_radians, variables);
        }
        self.last_anchor = Some(target_anchor);
    }

    fn step_once(
        &mut self,
        dt: f64,
        input_anchor: [f64; 3],
        angle_radians: f64,
        variables: &mut BTreeMap<String, f64>,
    ) {
        if self.controller_first_tick {
            self.root_delta = [
                self.root[0] - input_anchor[0],
                self.root[1] - input_anchor[1],
            ];
            self.controller_first_tick = false;
        } else {
            self.root[0] = input_anchor[0] + self.root_delta[0];
            self.root[1] = input_anchor[1] + self.root_delta[1];
        }

        let down = rotated_down(angle_radians);
        let displacement = sub3(self.root, self.bob);
        self.vel[0] += self.spring * dt * displacement[0];
        self.vel[1] += self.spring * dt * displacement[1];
        self.vel[2] += self.spring * dt * displacement[2];
        self.vel[0] += self.gravity * dt * down[0];
        self.vel[1] += self.gravity * dt * down[1];
        let damp = 1.0 - self.friction * dt;
        self.vel[0] *= damp;
        self.vel[1] *= damp;
        self.vel[2] *= damp;
        self.bob[0] += self.vel[0] * dt;
        self.bob[1] += self.vel[1] * dt;
        self.bob[2] += self.vel[2] * dt;

        let out = sub3(self.root, self.bob);
        let var_lr = response(-out[0] * self.scale_x);
        let var_ud = response((-out[1] - self.ofs) * self.scale_y);
        if let Some(name) = &self.var_lr {
            variables.insert(name.clone(), var_lr);
        }
        if let Some(name) = &self.var_ud {
            variables.insert(name.clone(), var_ud);
        }
    }
}

impl PendPhysics {
    fn from_controller(controller: &Controller) -> Option<Self> {
        let raw = &controller.raw;
        let param = raw.get("param");
        let length = field_pair(raw, "length");
        let root = param.map_or([0.0; 3], |param| vec3(param, "op"));
        let rest0 = [root[0], root[1] + length[0], root[2]];
        let rest1 = [rest0[0], rest0[1] + length[1], rest0[2]];
        let bob = param
            .map(|param| vec3_pair(param, "p", [rest0, rest1]))
            .unwrap_or([rest0, rest1]);
        let vel = param
            .map(|param| vec3_pair(param, "pv", [[0.0; 3]; 2]))
            .unwrap_or([[0.0; 3]; 2]);
        Some(Self {
            label: controller.label.clone().unwrap_or_default(),
            enabled: controller
                .enabled
                .unwrap_or_else(|| field_f64(raw, "enabled", 1.0) != 0.0),
            base_layer: field_str(raw, "baseLayer"),
            length,
            gravity: field_f64(raw, "gravity", 0.0),
            friction_x: field_f64(raw, "friction_x", 0.0),
            friction_y: field_f64(raw, "friction_y", 0.0),
            b_rate: field_f64(raw, "b_rate", 0.0),
            v_bound: field_f64(raw, "v_bound", 0.0),
            bend_spd: field_f64(raw, "bend_spd", 0.0),
            bend_vol: field_f64(raw, "bend_vol", 0.0),
            ud_eft: field_f64(raw, "ud_eft", 0.0).clamp(0.0, 1.0) as usize,
            ofs: param.map_or(0.0, |param| field_f64(param, "ofs", 0.0)),
            scale_x: field_pair(raw, "scale_x"),
            scale_y: field_pair(raw, "scale_y"),
            root_offset: root,
            bob,
            vel,
            bend_phase: param.map_or(0.0, |param| field_f64(param, "bendR", 0.0)),
            bend_power: param.map_or(0.0, |param| field_f64(param, "bendS", 0.0)),
            first_tick: true,
            last_anchor: None,
            var_lr: controller.variable_lr.clone(),
            var_lrm: controller.variable_lrm.clone(),
            var_ud: controller.variable_ud.clone(),
        })
    }

    fn step(
        &mut self,
        delta_ticks: f64,
        target_anchor: [f64; 3],
        angle_radians: f64,
        variables: &mut BTreeMap<String, f64>,
    ) {
        if self.first_tick {
            for bob in &mut self.bob {
                *bob = add3(*bob, target_anchor);
            }
            self.last_anchor = Some(target_anchor);
            self.first_tick = false;
            self.step_once(0.0, target_anchor, angle_radians, variables);
            return;
        }
        let previous = self.last_anchor.unwrap_or(target_anchor);
        let mut elapsed = 0.0;
        while (delta_ticks - EPSILON) > elapsed {
            let step = (delta_ticks - elapsed).min(MAX_SUBSTEP);
            elapsed += step;
            let ratio = (elapsed / delta_ticks).clamp(0.0, 1.0);
            let anchor = lerp3(previous, target_anchor, ratio);
            self.step_once(step, anchor, angle_radians, variables);
        }
        self.last_anchor = Some(target_anchor);
    }

    fn step_once(
        &mut self,
        dt: f64,
        target_anchor: [f64; 3],
        angle_radians: f64,
        variables: &mut BTreeMap<String, f64>,
    ) {
        let root = [
            target_anchor[0] + self.root_offset[0],
            target_anchor[1] + self.root_offset[1],
            target_anchor[2] + self.root_offset[2],
        ];
        let rest0 = [root[0], root[1] + self.length[0], root[2]];
        let rest1 = [rest0[0], rest0[1] + self.length[1], rest0[2]];
        let rest = [rest0, rest1];
        let down = rotated_down(angle_radians);
        let gravity = [down[0] * self.gravity, down[1] * self.gravity, 0.0];

        for index in 0..2 {
            let hinge = if index == 0 { root } else { self.bob[0] };
            let hinge_to_bob = sub3(self.bob[index], hinge);
            let distance = len3(hinge_to_bob);
            let segment = self.length[index].max(f64::EPSILON);
            if distance > segment && distance > f64::EPSILON {
                let outward = scale3(hinge_to_bob, 1.0 / distance);
                let inward = scale3(outward, -1.0);
                let excess = distance - segment;
                if excess > 0.015625 {
                    if index == 1 {
                        self.bob[index] = add3(self.bob[index], scale3(inward, excess));
                        let radial = dot3(self.vel[index], inward);
                        self.vel[index] =
                            add3(self.vel[index], scale3(inward, -radial * self.v_bound * dt));
                    } else {
                        self.vel[index] =
                            add3(self.vel[index], scale3(inward, excess * self.b_rate * dt));
                    }
                }
            }

            self.vel[index][0] += gravity[0] * dt;
            self.vel[index][1] += gravity[1] * dt;
            self.vel[index][2] += gravity[2] * dt;
            self.vel[index][0] -= self.vel[index][0] * self.friction_x * dt;
            self.vel[index][1] -= self.vel[index][1] * self.friction_y * dt;
            self.bob[index] = add3(self.bob[index], scale3(self.vel[index], dt));
        }

        let d0 = sub3(rest[0], self.bob[0]);
        let d1 = sub3(rest[1], self.bob[1]);
        let mut var_lr = response(-d0[0] * self.scale_x[0]);
        let mut var_lrm = response(-d1[0] * self.scale_x[1]);
        let displacement = if self.ud_eft == 0 { d0[1] } else { d1[1] };
        let var_ud = response((self.ofs - displacement) * self.scale_y[self.ud_eft]);

        if self.bend_spd != 0.0 && self.bend_vol != 0.0 {
            if var_lr.abs() <= PEND_BEND_TRIGGER {
                self.bend_power = (self.bend_power - PEND_BEND_STEP * dt).max(0.0);
            } else {
                self.bend_power = (self.bend_power + PEND_BEND_STEP * dt).min(1.0);
            }
            self.bend_phase = (self.bend_phase + self.bend_spd * self.bend_power * dt)
                .rem_euclid(std::f64::consts::TAU);
            let bend = self.bend_phase.sin() * self.bend_power * self.bend_vol;
            var_lrm += bend;
            var_lr -= bend;
        }

        if let Some(name) = &self.var_lr {
            variables.insert(name.clone(), var_lr);
        }
        if let Some(name) = &self.var_lrm {
            variables.insert(name.clone(), var_lrm);
        }
        if let Some(name) = &self.var_ud {
            variables.insert(name.clone(), var_ud);
        }
    }
}

/// Native `sub_1021A4F0` saturating response shared by both controllers.
fn response(value: f64) -> f64 {
    const K: f64 = std::f64::consts::PI / 80.0;
    ((1.15 * value) * K).atan() / K
}

fn rotated_down(angle_radians: f64) -> [f64; 3] {
    let c = (-angle_radians).cos();
    let s = (-angle_radians).sin();
    [-s, c, 0.0]
}

fn field_f64(value: &Value, key: &str, default: f64) -> f64 {
    value
        .get(key)
        .and_then(Value::as_f64)
        .filter(|value| value.is_finite())
        .unwrap_or(default)
}

fn field_str(value: &Value, key: &str) -> Option<String> {
    value.get(key).and_then(Value::as_str).map(str::to_owned)
}

fn field_pair(value: &Value, key: &str) -> [f64; 2] {
    let Some(items) = value.get(key).and_then(Value::as_array) else {
        return [0.0; 2];
    };
    [
        items.first().and_then(Value::as_f64).unwrap_or(0.0),
        items.get(1).and_then(Value::as_f64).unwrap_or(0.0),
    ]
}

fn vec3(value: &Value, key: &str) -> [f64; 3] {
    let Some(object) = value.get(key) else {
        return [0.0; 3];
    };
    [
        field_f64(object, "x", 0.0),
        field_f64(object, "y", 0.0),
        field_f64(object, "z", 0.0),
    ]
}

fn vec3_pair(value: &Value, key: &str, fallback: [[f64; 3]; 2]) -> [[f64; 3]; 2] {
    let Some(items) = value.get(key).and_then(Value::as_array) else {
        return fallback;
    };
    if items.len() != 2 {
        return fallback;
    }
    [
        [
            field_f64(&items[0], "x", fallback[0][0]),
            field_f64(&items[0], "y", fallback[0][1]),
            field_f64(&items[0], "z", fallback[0][2]),
        ],
        [
            field_f64(&items[1], "x", fallback[1][0]),
            field_f64(&items[1], "y", fallback[1][1]),
            field_f64(&items[1], "z", fallback[1][2]),
        ],
    ]
}

fn add3(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
}

fn sub3(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

fn scale3(a: [f64; 3], scale: f64) -> [f64; 3] {
    [a[0] * scale, a[1] * scale, a[2] * scale]
}

fn dot3(a: [f64; 3], b: [f64; 3]) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

fn len3(a: [f64; 3]) -> f64 {
    dot3(a, a).sqrt()
}

fn lerp3(a: [f64; 3], b: [f64; 3], t: f64) -> [f64; 3] {
    [
        a[0] + (b[0] - a[0]) * t,
        a[1] + (b[1] - a[1]) * t,
        a[2] + (b[2] - a[2]) * t,
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn output_response_is_odd_and_monotonic() {
        assert!(response(0.0).abs() < 1.0e-12);
        assert!(response(-30.0) < 0.0);
        assert!(response(30.0) > 0.0);
        assert!(response(30.0) < 30.0);
        assert!((response(10.0) + response(-10.0)).abs() < 1.0e-12);
    }

    #[test]
    fn bust_spring_reaches_equilibrium_under_gravity() {
        let mut control = BustPhysics {
            label: "bust".into(),
            enabled: true,
            base_layer: None,
            gravity: 0.3,
            spring: 0.015625,
            friction: 0.125,
            scale_x: 1.0,
            scale_y: 1.0,
            ofs: 0.0,
            root: [0.0; 3],
            bob: [0.0, 0.0, 0.0],
            vel: [0.0; 3],
            root_delta: [0.0; 2],
            group_first_tick: true,
            controller_first_tick: true,
            last_anchor: None,
            var_lr: Some("bust_LR".into()),
            var_ud: Some("bust_UD".into()),
        };
        let mut variables = BTreeMap::new();
        for _ in 0..600 {
            control.step(1.0, [0.0; 3], 0.0, &mut variables);
        }
        let ud = variables["bust_UD"];
        assert!(ud.is_finite());
        assert!(ud > 0.0);
    }

    #[test]
    fn pendulum_first_tick_attaches_to_anchor_without_jump() {
        let mut control = PendPhysics {
            label: "hair".into(),
            enabled: true,
            base_layer: None,
            length: [64.0, 48.0],
            gravity: 0.2,
            friction_x: 0.0625,
            friction_y: 0.0625,
            b_rate: 0.0037,
            v_bound: 2.0,
            bend_spd: 0.0,
            bend_vol: 0.0,
            ud_eft: 0,
            ofs: -53.9,
            scale_x: [0.75, 0.25],
            scale_y: [2.0, 3.0],
            root_offset: [0.0; 3],
            bob: [[0.0, 64.0, 0.0], [0.0, 112.0, 0.0]],
            vel: [[0.0; 3]; 2],
            bend_phase: 0.0,
            bend_power: 0.0,
            first_tick: true,
            last_anchor: None,
            var_lr: Some("hair_LR".into()),
            var_lrm: Some("hair_LR_M".into()),
            var_ud: Some("hair_UD".into()),
        };
        let mut variables = BTreeMap::new();
        control.step(1.0, [100.0, 200.0, 0.0], 0.0, &mut variables);
        // The first update only attaches the chain; the horizontal output is
        // zero, while UD publishes the authored rest bias through the native
        // saturating response.
        assert!((variables["hair_LR"]).abs() < 1.0e-9);
        assert!((variables["hair_UD"] - response(-53.9 * 2.0)).abs() < 1.0e-9);
    }
}
