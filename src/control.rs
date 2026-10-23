//! Graph/clamp controllers (M2).
//!
//! The official player routes controller-backed variables through dedicated
//! control objects before the pose is sampled. This module implements the two
//! controllers that change visible playback in the CPU renderer:
//!
//! - `clampControl` (`sub_10275CC0`): limits an LR/UD variable pair to a
//!   square-disc or radial domain;
//! - `eyeControl` (`sub_101E2970`): adds the automatic blink triangle on top
//!   of the authored `face_eye_open` graph value.
//!
//! `eyebrowControl`, `mouthControl`, `transitionControl`, and
//! `selectorControl` are parsed by [`crate::schema`]; their direct numeric
//! writes already behave like the native timed setters in the current
//! command-stream player. Cross-line graph routes and selector arm variants
//! remain explicitly unsupported (see [`ControlRuntime::unsupported`]).
//!
//! Evidence: the recovered native functions and constants are cited by eluna;
//! this implementation independently reproduces their documented behavior.

use std::collections::BTreeMap;

use serde_json::Value;

use crate::schema::Model;

/// One clamp controller.
#[derive(Debug, Clone, PartialEq)]
pub struct ClampControl {
    /// Display label.
    pub label: String,
    /// Whether the controller runs.
    pub enabled: bool,
    /// 0 = square-to-disc remap, 1 = radial clip.
    pub kind: i64,
    /// Horizontal variable.
    pub var_lr: String,
    /// Vertical variable.
    pub var_ud: String,
    /// Authored minimum (both axes).
    pub min: f64,
    /// Authored maximum (both axes).
    pub max: f64,
}

/// One automatic blink controller.
#[derive(Debug, Clone, PartialEq)]
pub struct EyeControl {
    /// Variable driven by the controller.
    pub label: String,
    /// Whether the controller runs.
    pub enabled: bool,
    /// Authored open frame (`beginFrame`).
    pub begin_frame: i64,
    /// Authored closed frame (`endFrame`).
    pub end_frame: i64,
    /// Whether blinking is enabled.
    pub blink_enabled: bool,
    /// Full close/open duration in ticks.
    pub blink_frame_count: f64,
    /// Minimum random interval.
    pub blink_interval_min: f64,
    /// Maximum random interval.
    pub blink_interval_max: f64,
    /// Current state.
    state: BlinkState,
    /// Current blink frame.
    blink_frame: f64,
    /// Timer for the current state.
    blink_timer: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BlinkState {
    Idle,
    Closing,
    ClosedHold,
    Opening,
}

/// Deterministic controller state for one model.
#[derive(Debug, Clone, PartialEq)]
pub struct ControlRuntime {
    /// Clamp controllers.
    pub clamps: Vec<ClampControl>,
    /// Eye controllers.
    pub eyes: Vec<EyeControl>,
    /// Labels whose non-trivial graph routing is not implemented yet.
    pub unsupported: Vec<String>,
    seed: u64,
}

impl ControlRuntime {
    /// Parse all controllers that affect rendering.
    pub fn new(model: &Model) -> Self {
        let clamps = model
            .controllers
            .clamp
            .iter()
            .filter_map(ClampControl::from_json)
            .collect();
        let eyes = model
            .controllers
            .eye
            .iter()
            .enumerate()
            .filter_map(|(index, control)| EyeControl::from_json(control.raw.as_object()?, index))
            .collect();
        let mut unsupported = Vec::new();
        for controller in &model.controllers.selector {
            if let Some(label) = &controller.label {
                unsupported.push(format!("selector routing: {label}"));
            }
        }
        for controller in &model.controllers.eyebrow {
            if let Some(label) = &controller.label {
                unsupported.push(format!("eyebrow graph route: {label}"));
            }
        }
        if !model.controllers.mouth.is_empty() {
            unsupported.push("mouth talk queue".to_string());
        }
        Self {
            clamps,
            eyes,
            unsupported,
            seed: 0x9E37_79B9_7F4A_7C15,
        }
    }

    /// Reset automatic blink timers deterministically.
    pub fn reset(&mut self) {
        self.seed = 0x9E37_79B9_7F4A_7C15;
        for control in &mut self.eyes {
            control.reset(&mut self.seed);
        }
    }

    /// Advance controllers by `delta_ticks` and publish their outputs.
    pub fn apply(&mut self, variables: &mut BTreeMap<String, f64>, delta_ticks: f64) {
        if !delta_ticks.is_finite() || delta_ticks < 0.0 {
            return;
        }
        for control in &mut self.eyes {
            control.step(variables, delta_ticks, &mut self.seed);
        }
        for control in &self.clamps {
            control.apply(variables);
        }
    }
}

impl ClampControl {
    fn from_json(controller: &crate::schema::Controller) -> Option<Self> {
        let raw = &controller.raw;
        Some(Self {
            label: controller.label.clone().unwrap_or_default(),
            enabled: controller
                .enabled
                .unwrap_or_else(|| field_f64(raw, "enabled", 1.0) != 0.0),
            kind: field_f64(raw, "type", 0.0) as i64,
            var_lr: controller.variable_lr.clone().unwrap_or_default(),
            var_ud: controller.variable_ud.clone().unwrap_or_default(),
            min: field_f64(raw, "min", 0.0),
            max: field_f64(raw, "max", 0.0),
        })
    }

    fn apply(&self, variables: &mut BTreeMap<String, f64>) {
        if !self.enabled || self.var_lr.is_empty() || self.var_ud.is_empty() {
            return;
        }
        let lr = variables.get(&self.var_lr).copied().unwrap_or(0.0);
        let ud = variables.get(&self.var_ud).copied().unwrap_or(0.0);
        let span = self.max - self.min;
        if !span.is_finite() || span.abs() <= f64::EPSILON {
            return;
        }
        let mut x = ((lr - self.min) / span) * 2.0 - 1.0;
        let mut y = ((ud - self.min) / span) * 2.0 - 1.0;
        if x != 0.0 && y != 0.0 {
            match self.kind {
                1 => {
                    let radius = x.hypot(y);
                    if radius > 1.0 {
                        x /= radius;
                        y /= radius;
                    }
                }
                0 => {
                    // Native square-to-disc remap. The operation order is
                    // preserved to keep its edge behavior.
                    let mut q = (x / y).abs();
                    if q > 1.0 {
                        q = 1.0 / q;
                    }
                    let inv = 1.0 / (q * q + 1.0).sqrt();
                    x *= inv;
                    y *= inv;
                    let radius = x.hypot(y);
                    if radius > f64::EPSILON {
                        let radial = (radius * std::f64::consts::FRAC_PI_2).sin() / radius;
                        let axis_mix = 1.0 - (q * std::f64::consts::FRAC_PI_2).cos();
                        let scale = (radial - 1.0) * axis_mix + 1.0;
                        x *= scale;
                        y *= scale;
                    }
                }
                _ => {}
            }
        }
        let next_lr = ((x + 1.0) * 0.5) * span + self.min;
        let next_ud = ((y + 1.0) * 0.5) * span + self.min;
        variables.insert(self.var_lr.clone(), next_lr);
        variables.insert(self.var_ud.clone(), next_ud);
    }
}

impl EyeControl {
    fn from_json(raw: &serde_json::Map<String, Value>, index: usize) -> Option<Self> {
        let label = raw.get("label")?.as_str()?.to_owned();
        let mut control = Self {
            label,
            enabled: field_f64(&Value::Object(raw.clone()), "enabled", 1.0) != 0.0,
            begin_frame: field_f64(&Value::Object(raw.clone()), "beginFrame", 0.0) as i64,
            end_frame: field_f64(&Value::Object(raw.clone()), "endFrame", 0.0) as i64,
            blink_enabled: field_f64(&Value::Object(raw.clone()), "blinkEnabled", 0.0) != 0.0,
            blink_frame_count: field_f64(&Value::Object(raw.clone()), "blinkFrameCount", 0.0),
            blink_interval_min: field_f64(&Value::Object(raw.clone()), "blinkIntervalMin", 0.0),
            blink_interval_max: field_f64(&Value::Object(raw.clone()), "blinkIntervalMax", 0.0),
            state: BlinkState::Idle,
            blink_frame: 0.0,
            blink_timer: 0.0,
        };
        control.blink_frame = control.begin_frame as f64;
        let mut seed = 0x1234_5678_9ABC_DEF1 ^ index as u64;
        control.reset(&mut seed);
        control.blink_timer = control.blink_interval_min
            + (control.blink_interval_max - control.blink_interval_min)
                * next_random(&mut seed).clamp(0.0, 1.0);
        Some(control)
    }

    fn reset(&mut self, seed: &mut u64) {
        self.state = BlinkState::Idle;
        self.blink_frame = self.begin_frame as f64;
        self.blink_timer = self.blink_interval_min
            + (self.blink_interval_max - self.blink_interval_min)
                * next_random(seed).clamp(0.0, 1.0);
    }

    fn step(&mut self, variables: &mut BTreeMap<String, f64>, delta_ticks: f64, seed: &mut u64) {
        if !self.enabled {
            return;
        }
        let begin = self.begin_frame as f64;
        let end = self.end_frame as f64;

        // Native chains state transitions (`continue`) up to eight times so a
        // long tick can enter closing/hold/opening immediately.
        for _ in 0..8 {
            match self.state {
                BlinkState::Idle => {
                    if self.blink_enabled && self.blink_frame as i64 == self.begin_frame {
                        self.blink_timer -= delta_ticks;
                        if self.blink_timer <= 0.0 {
                            self.state = BlinkState::Closing;
                            continue;
                        }
                    }
                }
                BlinkState::Closing => {
                    if self.blink_frame_count.abs() > f64::EPSILON {
                        self.blink_frame +=
                            (end - begin) * (2.5 * delta_ticks / self.blink_frame_count);
                    } else {
                        self.blink_frame = end;
                    }
                    if self.blink_frame >= end {
                        self.blink_frame = end;
                        self.blink_timer = self.blink_frame_count / 5.0;
                        self.state = BlinkState::ClosedHold;
                        continue;
                    }
                }
                BlinkState::ClosedHold => {
                    self.blink_timer -= delta_ticks;
                    if self.blink_timer <= 0.0 {
                        self.state = BlinkState::Opening;
                        self.blink_timer = self.blink_interval_min
                            + (self.blink_interval_max - self.blink_interval_min)
                                * next_random(seed).clamp(0.0, 1.0);
                        continue;
                    }
                }
                BlinkState::Opening => {
                    if self.blink_frame_count.abs() > f64::EPSILON {
                        self.blink_frame -=
                            (end - begin) * (2.5 * delta_ticks / self.blink_frame_count);
                    } else {
                        self.blink_frame = begin;
                    }
                    if self.blink_frame <= begin {
                        self.blink_frame = begin;
                        self.state = BlinkState::Idle;
                        continue;
                    }
                }
            }
            break;
        }

        let graph_value = variables.get(&self.label).copied().unwrap_or(0.0);
        let output = if graph_value < begin
            || end < graph_value
            || (end - begin).abs() <= f64::EPSILON
            || self.state == BlinkState::Idle
        {
            graph_value
        } else {
            graph_value + (self.blink_frame - begin) * (end - graph_value) / (end - begin)
        };
        variables.insert(self.label.clone(), output);
    }
}

fn field_f64(value: &Value, key: &str, default: f64) -> f64 {
    value
        .get(key)
        .and_then(Value::as_f64)
        .filter(|value| value.is_finite())
        .unwrap_or(default)
}

/// Deterministic xorshift64* in `[0, 1)`.
fn next_random(seed: &mut u64) -> f64 {
    let mut x = *seed;
    x ^= x >> 12;
    x ^= x << 25;
    x ^= x >> 27;
    *seed = x;
    let value = x.wrapping_mul(0x2545_F491_4F6C_DD1D);
    (value >> 11) as f64 / (1u64 << 53) as f64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn radial_clamp_limits_corner_to_unit_circle() {
        let control = ClampControl {
            label: "head".into(),
            enabled: true,
            kind: 1,
            var_lr: "head_LR".into(),
            var_ud: "head_UD".into(),
            min: -30.0,
            max: 30.0,
        };
        let mut variables =
            BTreeMap::from([("head_LR".to_string(), 30.0), ("head_UD".to_string(), 30.0)]);
        control.apply(&mut variables);
        let lr = variables["head_LR"];
        let ud = variables["head_UD"];
        assert!((lr.hypot(ud) - 30.0).abs() < 1.0e-9);
        assert!(lr < 30.0 && ud < 30.0);
    }

    #[test]
    fn eye_blink_reaches_closed_frame_and_returns() {
        let mut control = EyeControl {
            label: "face_eye_open".into(),
            enabled: true,
            begin_frame: 0,
            end_frame: 10,
            blink_enabled: true,
            blink_frame_count: 16.0,
            blink_interval_min: 5.0,
            blink_interval_max: 5.0,
            state: BlinkState::Idle,
            blink_frame: 0.0,
            blink_timer: 5.0,
        };
        let mut variables = BTreeMap::from([("face_eye_open".to_string(), 0.0)]);
        let mut seed = 1u64;
        let mut maximum = f64::NEG_INFINITY;
        let mut minimum = f64::INFINITY;
        for _ in 0..200 {
            control.step(&mut variables, 1.0, &mut seed);
            maximum = maximum.max(variables["face_eye_open"]);
            minimum = minimum.min(variables["face_eye_open"]);
        }
        assert!(maximum >= 9.0, "blink should reach the closed frame");
        assert!(minimum <= 1.0, "the eye must reopen between blinks");
    }
}
