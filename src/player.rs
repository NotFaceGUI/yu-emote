//! Deterministic player loop (M2).
//! Algorithms and native semantics adapted from eluna (MPL-2.0,
//! https://github.com/xmoezzz/eluna). See NOTICE and LICENSE-MPL-2.0.
//!
//! Ties together the command-stream timeline, the graph controllers, and the
//! physics solvers in the recovered native order:
//!
//! 1. advance timeline commands and timed variable targets;
//! 2. run eye/brow/mouth and clamp controls;
//! 3. evaluate the motion pose with the resulting variables;
//! 4. run bust/hair/parts physics against the pose anchors and publish their
//!    outputs for the next tick;
//! 5. evaluate the pose again for rendering.
//!
//! The official player's frame rate is one tick per 1/60 s; callers supply
//! wall-clock deltas and the player converts them to ticks.

use std::collections::BTreeMap;

use crate::anim::{Animator, TICKS_PER_SECOND};
use crate::control::ControlRuntime;
use crate::physics::PhysicsState;
use crate::pose::Pose;
use crate::schema::Model;

/// One deterministic character player.
#[derive(Debug, Clone)]
pub struct Player {
    /// Timeline/variable state.
    pub animator: Animator,
    /// Graph and clamp controllers.
    pub controls: ControlRuntime,
    /// Physics controllers.
    pub physics: PhysicsState,
    /// Variables after the most recent tick.
    pub variables: BTreeMap<String, f64>,
    /// Object name.
    pub object: String,
    /// Motion name.
    pub motion: String,
    /// Motion time in ticks.
    pub time_ticks: f64,
    /// Ticks per second.
    pub ticks_per_second: f64,
    /// Whether eye/clamp controllers run.
    pub controls_enabled: bool,
    /// Whether bust/hair/parts physics runs.
    pub physics_enabled: bool,
}

impl Player {
    /// Create a player for one object/motion.
    pub fn new(
        model: &Model,
        object: impl Into<String>,
        motion: impl Into<String>,
        ticks_per_second: f64,
    ) -> Self {
        let ticks_per_second = if ticks_per_second.is_finite() && ticks_per_second > 0.0 {
            ticks_per_second
        } else {
            TICKS_PER_SECOND
        };
        let animator = Animator::new(model, ticks_per_second);
        let variables = animator.evaluated_variables();
        Self {
            animator,
            controls: ControlRuntime::new(model),
            physics: PhysicsState::new(model),
            variables,
            object: object.into(),
            motion: motion.into(),
            time_ticks: 0.0,
            ticks_per_second,
            controls_enabled: true,
            physics_enabled: true,
        }
    }

    /// Set an explicit variable before playback.
    pub fn set_variable(&mut self, name: impl Into<String>, value: f64) {
        self.animator.set_variable(name, value);
        self.variables = self.animator.evaluated_variables();
    }

    /// Start a timeline.
    pub fn play(&mut self, name: &str, looping: bool) -> Result<(), crate::error::EmoteError> {
        self.animator.play_with(name, 1.0, looping)?;
        // `PlayTimeline` seeks frame 0 immediately. A zero-length first
        // command (frame 0 followed by frame 1) therefore becomes visible on
        // the very first rendered frame; do not keep the pre-play defaults.
        self.variables = self.animator.evaluated_variables();
        Ok(())
    }

    /// Advance by `dt_seconds` and update [`Player::variables`].
    pub fn tick(&mut self, model: &Model, dt_seconds: f64) -> bool {
        if !dt_seconds.is_finite() || dt_seconds <= 0.0 {
            return false;
        }
        let ticks = dt_seconds * self.ticks_per_second;
        self.time_ticks += ticks;
        self.animator.tick(dt_seconds);

        let mut variables = self.animator.evaluated_variables();
        if self.controls_enabled {
            self.controls.apply(&mut variables, ticks);
        }

        if let Some(pose) = Pose::evaluate(
            model,
            &self.object,
            &self.motion,
            self.time_ticks,
            &variables,
        ) {
            if self.physics_enabled {
                self.physics.step(&pose, ticks, 0.0, &mut variables);
            }
        }
        self.variables = variables;
        true
    }

    /// Advance multiple ticks while holding the motion at frame zero.
    ///
    /// Useful before a static render so physics starts from its equilibrium
    /// instead of the authored constructor state.
    pub fn settle(&mut self, model: &Model, ticks: usize) {
        let mut variables = self.animator.evaluated_variables();
        for _ in 0..ticks {
            if self.controls_enabled {
                self.controls.apply(&mut variables, 1.0);
            }
            if let Some(pose) = Pose::evaluate(
                model,
                &self.object,
                &self.motion,
                self.time_ticks,
                &variables,
            ) {
                if self.physics_enabled {
                    self.physics.step(&pose, 1.0, 0.0, &mut variables);
                }
            }
        }
        self.variables = variables;
    }

    /// Evaluate the current pose for rendering.
    pub fn pose(&self, model: &Model) -> Option<Pose> {
        Pose::evaluate(
            model,
            &self.object,
            &self.motion,
            self.time_ticks,
            &self.variables,
        )
    }

    /// Overlay variables that should win over the player state (`--var`).
    pub fn overlay_variables(&mut self, overlay: &BTreeMap<String, f64>) {
        for (name, value) in overlay {
            self.variables.insert(name.clone(), *value);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn player_without_model_data_is_stable() {
        let model = Model::default();
        let mut player = Player::new(&model, "all_parts", "motion", 60.0);
        player.set_variable("head_UD", 10.0);
        assert_eq!(player.variables.get("head_UD"), Some(&10.0));
        assert!(!player.tick(&model, 0.0));
        assert!(player.tick(&model, 1.0 / 60.0));
        assert_eq!(player.variables.get("head_UD"), Some(&10.0));
    }
}
