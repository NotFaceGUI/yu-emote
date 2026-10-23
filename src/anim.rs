//! Deterministic animation state (M2).
//!
//! This module mirrors the official timeline model rather than treating a
//! timeline as a set of independently sampled curves:
//!
//! - a timeline is a **command stream**. Crossing a frame boundary issues a
//!   variable write; the authored next frame determines the transition
//!   duration (`next.time - crossing_time - 1`) and easing exponent;
//! - `type == 0` frames are cursor-only hold markers and never issue a write;
//! - every write starts a timed transition from the variable's current value
//!   to the frame value (`pow(progress, exponent)`);
//! - authored loops advance to `loopEnd` non-inclusively, seek `loopBegin`,
//!   and keep the residual delta;
//! - the player runs at 60 ticks per second (official API constant).
//!
//! Evidence: `eluna` documents the recovered native timeline runtime
//! (`sub_1026A660` / `sub_1026B1F0` / `sub_10275350` / `sub_1026AD10`) and the
//! official SDK exposes `EMOTE_TICKS_PER_SECOND = 60`. The command-stream behavior and
//! constants are adapted from eluna (MPL-2.0); see NOTICE.
//!
//! Physics and graph controllers (eye/brow/mouth) live in
//! [`crate::control`] and consume the variables produced here.

use std::collections::{BTreeMap, BTreeSet};

use crate::error::EmoteError;
use crate::schema::{Model, Timeline, TimelineFrame};

/// Official SDK tick rate.
pub const TICKS_PER_SECOND: f64 = 60.0;

/// One active timeline.
#[derive(Debug, Clone, PartialEq)]
pub struct Playback {
    /// Timeline name.
    pub name: String,
    /// Position in ticks (frames).
    pub time_frames: f64,
    /// Playback speed multiplier (1.0 = normal).
    pub speed: f64,
    /// Whether the timeline loops between `loop_begin` and `loop_end`.
    pub looping: bool,
    /// Blend weight retained for difference playback (ordinary playback is 1).
    pub weight: f64,
    /// Difference timelines write deltas multiplied by `blend_ratio`.
    pub difference: bool,
    /// Current difference blend ratio (native Timeline runtime +36).
    pub blend_ratio: f64,
    /// Active blend-ratio transition.
    blend_target: Option<Fade>,
    /// Whether the timeline is fading out and should be removed when idle.
    stopping: bool,
    /// Per-track frame cursors; `usize::MAX` means "before the first frame".
    pub(crate) cursors: Vec<usize>,
}

/// One timed variable transition.
#[derive(Debug, Clone, PartialEq)]
struct Fade {
    from: f64,
    to: f64,
    value: f64,
    elapsed: f64,
    duration: f64,
    easing: f64,
}

/// Value plus optional transition, matching the native global variable state.
#[derive(Debug, Clone, PartialEq)]
struct VariableState {
    value: f64,
    target: Option<Fade>,
}

impl VariableState {
    fn new(value: f64) -> Self {
        Self {
            value,
            target: None,
        }
    }
}

/// Deterministic E-mote variable/timeline state.
#[derive(Debug, Clone)]
pub struct Animator {
    timelines: BTreeMap<String, Timeline>,
    variables: BTreeMap<String, VariableState>,
    /// Per-difference-timeline variable deltas (native `timeline_diff_variables`).
    diff_variables: BTreeMap<String, BTreeMap<String, VariableState>>,
    strings: BTreeMap<String, String>,
    playbacks: Vec<Playback>,
    frames_per_second: f64,
    unsupported_easing: BTreeSet<i64>,
    elapsed_frames: f64,
}

impl Animator {
    /// Create an animator from a model.
    ///
    /// `frames_per_second` is supplied by the caller. The official SDK
    /// constant is [`TICKS_PER_SECOND`]; a different value is only useful for
    /// slow-motion diagnostics.
    pub fn new(model: &Model, frames_per_second: f64) -> Self {
        let mut timelines = BTreeMap::new();
        for timeline in &model.timelines {
            timelines.insert(timeline.label.clone(), timeline.clone());
        }
        Self {
            timelines,
            variables: BTreeMap::new(),
            diff_variables: BTreeMap::new(),
            strings: BTreeMap::new(),
            playbacks: Vec::new(),
            frames_per_second,
            unsupported_easing: BTreeSet::new(),
            elapsed_frames: 0.0,
        }
    }

    /// Frames per second supplied at construction.
    pub fn frames_per_second(&self) -> f64 {
        self.frames_per_second
    }

    /// Total advanced time in frames.
    pub fn elapsed_frames(&self) -> f64 {
        self.elapsed_frames
    }

    /// Timeline names known to the model.
    pub fn timeline_names(&self) -> impl Iterator<Item = &str> {
        self.timelines.keys().map(String::as_str)
    }

    /// Set a numeric variable immediately (native `SetVariable` with time 0).
    pub fn set_variable(&mut self, name: impl Into<String>, value: f64) {
        self.variables
            .insert(name.into(), VariableState::new(value));
    }

    /// Set a numeric variable with a linear fade over `duration_seconds`.
    ///
    /// A non-positive or non-finite duration sets the value immediately.
    pub fn set_variable_with_fade(
        &mut self,
        name: impl Into<String>,
        value: f64,
        duration_seconds: f64,
    ) {
        self.set_variable_timed(name, value, duration_seconds, 0.0);
    }

    /// Set a numeric variable with an official easing selector.
    ///
    /// `easing` is the SDK value converted by native `sub_1026AD10`:
    /// `0` is linear, positive values raise the exponent, negative values
    /// compress the start of the curve.
    pub fn set_variable_timed(
        &mut self,
        name: impl Into<String>,
        value: f64,
        duration_seconds: f64,
        easing: f64,
    ) {
        let name = name.into();
        let duration = duration_seconds * self.frames_per_second;
        if !duration.is_finite() || duration <= 0.0 {
            self.set_variable(name, value);
            return;
        }
        let state = self
            .variables
            .entry(name)
            .or_insert_with(|| VariableState::new(value));
        state.target = Some(Fade {
            from: state.value,
            to: value,
            value: state.value,
            elapsed: 0.0,
            duration,
            easing,
        });
    }

    /// Set a string variable (timeline-independent state, e.g. a part label).
    pub fn set_string(&mut self, name: impl Into<String>, value: impl Into<String>) {
        self.strings.insert(name.into(), value.into());
    }

    /// Current string variable.
    pub fn string(&self, name: &str) -> Option<&str> {
        self.strings.get(name).map(String::as_str)
    }

    /// Remove a variable override.
    pub fn clear_variable(&mut self, name: &str) {
        self.variables.remove(name);
    }

    /// Start a timeline.
    ///
    /// Playing an already active timeline restarts it. This is the native
    /// non-parallel mode: every other main timeline is stopped first.
    pub fn play(&mut self, name: &str) -> Result<(), EmoteError> {
        self.play_with(name, 1.0, false)
    }

    /// Start a timeline with a weight and optional compatibility loop flag.
    ///
    /// Authored `loopBegin`/`loopEnd` take precedence; `looping` is a fallback
    /// for synthetic timelines without authored loop bounds.
    pub fn play_with(&mut self, name: &str, weight: f64, looping: bool) -> Result<(), EmoteError> {
        let timeline = self
            .timelines
            .get(name)
            .ok_or_else(|| EmoteError::UnknownTimeline(name.to_string()))?;
        let looping = looping || timeline.loop_end > timeline.loop_begin.max(-1);
        self.playbacks.clear();
        self.diff_variables.clear();
        let cursors = vec![usize::MAX; timeline.tracks.len()];
        self.playbacks.push(Playback {
            name: name.to_string(),
            time_frames: 0.0,
            speed: 1.0,
            looping,
            weight,
            difference: false,
            blend_ratio: 1.0,
            blend_target: None,
            stopping: false,
            cursors,
        });
        self.seek(name, 0.0);
        Ok(())
    }

    /// Start a differential timeline blended over `fade_seconds`.
    ///
    /// Difference timelines write deltas into a per-timeline map; the final
    /// variable value is `base + delta * blend_ratio`. Native `DPLAY` keeps
    /// the current main timeline running, so this does not clear other
    /// playbacks. A non-positive fade starts at the requested weight.
    pub fn play_diff(
        &mut self,
        name: &str,
        fade_seconds: f64,
        weight: f64,
    ) -> Result<(), EmoteError> {
        let timeline = self
            .timelines
            .get(name)
            .ok_or_else(|| EmoteError::UnknownTimeline(name.to_string()))?;
        let looping = timeline.loop_end > timeline.loop_begin.max(-1);
        self.playbacks.retain(|playback| playback.name != name);
        self.diff_variables.remove(name);
        let duration = fade_seconds * self.frames_per_second;
        let blend_target = if duration.is_finite() && duration > 0.0 {
            Some(Fade {
                from: 0.0,
                to: weight,
                value: 0.0,
                elapsed: 0.0,
                duration,
                easing: 0.0,
            })
        } else {
            None
        };
        let cursors = vec![usize::MAX; timeline.tracks.len()];
        self.playbacks.push(Playback {
            name: name.to_string(),
            time_frames: 0.0,
            speed: 1.0,
            looping,
            weight,
            difference: true,
            blend_ratio: if blend_target.is_some() { 0.0 } else { weight },
            blend_target,
            stopping: false,
            cursors,
        });
        self.seek(name, 0.0);
        Ok(())
    }

    /// Stop a timeline immediately.
    pub fn stop(&mut self, name: &str) {
        self.playbacks.retain(|playback| playback.name != name);
        self.diff_variables.remove(name);
    }

    /// Stop a timeline after fading its difference blend ratio to zero.
    pub fn stop_with_fade(&mut self, name: &str, fade_seconds: f64) {
        let duration = fade_seconds * self.frames_per_second;
        if !duration.is_finite() || duration <= 0.0 {
            self.stop(name);
            return;
        }
        let Some(playback) = self
            .playbacks
            .iter_mut()
            .find(|playback| playback.name == name)
        else {
            return;
        };
        if !playback.difference {
            self.stop(name);
            return;
        }
        playback.stopping = true;
        playback.blend_target = Some(Fade {
            from: playback.blend_ratio,
            to: 0.0,
            value: playback.blend_ratio,
            elapsed: 0.0,
            duration,
            easing: 0.0,
        });
    }

    /// Currently active playbacks.
    pub fn playbacks(&self) -> &[Playback] {
        &self.playbacks
    }

    /// Easing selectors seen but not implemented. The command-stream model
    /// implements every selector through the native exponent conversion.
    pub fn unsupported_easing(&self) -> &BTreeSet<i64> {
        &self.unsupported_easing
    }

    /// Advance the deterministic clock by `dt_seconds`.
    ///
    /// Returns `true` when any variable changed.
    pub fn tick(&mut self, dt_seconds: f64) -> bool {
        if !dt_seconds.is_finite() || dt_seconds <= 0.0 {
            return false;
        }
        let before = self.evaluated_variables();
        let frames = dt_seconds * self.frames_per_second;
        self.elapsed_frames += frames;

        self.progress_playbacks(frames);
        self.advance_blends(frames);
        self.advance_fades(frames);
        self.retain_active_playbacks();
        self.evaluated_variables() != before
    }

    /// Evaluate all variables at the current time.
    ///
    /// Difference timelines are added on top of the base variable state,
    /// scaled by their current blend ratio.
    pub fn evaluated_variables(&self) -> BTreeMap<String, f64> {
        let mut out: BTreeMap<String, f64> = self
            .variables
            .iter()
            .map(|(name, state)| (name.clone(), state.value))
            .collect();
        for playback in &self.playbacks {
            if !playback.difference || playback.blend_ratio.abs() <= f64::EPSILON {
                continue;
            }
            let Some(deltas) = self.diff_variables.get(&playback.name) else {
                continue;
            };
            for (name, state) in deltas {
                *out.entry(name.clone()).or_insert(0.0) += state.value * playback.blend_ratio;
            }
        }
        out
    }

    /// One evaluated variable.
    pub fn variable(&self, name: &str) -> Option<f64> {
        let mut value = self.variables.get(name).map(|state| state.value);
        for playback in &self.playbacks {
            if !playback.difference || playback.blend_ratio.abs() <= f64::EPSILON {
                continue;
            }
            if let Some(state) = self
                .diff_variables
                .get(&playback.name)
                .and_then(|deltas| deltas.get(name))
            {
                *value.get_or_insert(0.0) += state.value * playback.blend_ratio;
            }
        }
        value
    }

    /// Variables defined by the model's `metadata/variableList`.
    pub fn model_variable_names(&self) -> impl Iterator<Item = &str> {
        self.timelines
            .values()
            .flat_map(|timeline| timeline.tracks.iter().map(|track| track.label.as_str()))
    }

    fn progress_playbacks(&mut self, frames: f64) {
        let mut finished = Vec::new();
        let names: Vec<String> = self
            .playbacks
            .iter()
            .map(|playback| playback.name.clone())
            .collect();
        for name in names {
            let Some(timeline) = self.timelines.get(&name).cloned() else {
                finished.push(name);
                continue;
            };
            let Some(index) = self
                .playbacks
                .iter()
                .position(|playback| playback.name == name)
            else {
                continue;
            };
            let mut playback = self.playbacks.remove(index);
            let mut remaining = frames * playback.speed;
            let mut current = playback.time_frames;

            let authored_loop = timeline.loop_begin >= 0 && timeline.loop_end > timeline.loop_begin;
            let compat_loop = !authored_loop && playback.looping && timeline_end(&timeline) > 0.0;
            let loop_begin = if authored_loop {
                timeline.loop_begin as f64
            } else {
                0.0
            };
            let loop_end = if authored_loop {
                timeline.loop_end as f64
            } else {
                timeline_end(&timeline)
            };

            let mut natural_finished = false;
            if authored_loop || compat_loop {
                while remaining > 0.0 && loop_end > loop_begin {
                    let to_end = (loop_end - current).max(0.0);
                    if remaining < to_end {
                        current += remaining;
                        // Native crossing is inclusive during normal progress.
                        self.advance(&name, &timeline, &mut playback, current, true);
                        remaining = 0.0;
                    } else {
                        // sub_10275350: advance exactly to loopEnd with the
                        // non-inclusive boundary mode, seek loopBegin, then
                        // consume the residual delta.
                        self.advance(&name, &timeline, &mut playback, loop_end, false);
                        remaining -= to_end;
                        current = loop_begin;
                        self.seek_with(&name, &timeline, &mut playback, loop_begin);
                        if to_end <= f64::EPSILON && remaining > 0.0 {
                            remaining = 0.0;
                        }
                    }
                }
            } else {
                let end = timeline_end(&timeline);
                let target = (current + remaining).min(end);
                self.advance(&name, &timeline, &mut playback, target, true);
                current = target;
                natural_finished = target >= end - f64::EPSILON;
            }

            playback.time_frames = current;
            self.playbacks.insert(index, playback);
            if natural_finished {
                finished.push(name);
            }
        }
        self.playbacks
            .retain(|playback| !finished.contains(&playback.name));
    }

    fn advance_blends(&mut self, frames: f64) {
        for playback in &mut self.playbacks {
            if !playback.difference {
                continue;
            }
            let Some(mut fade) = playback.blend_target.take() else {
                continue;
            };
            advance_fade(&mut fade, frames);
            playback.blend_ratio = fade.value;
            if fade.elapsed < fade.duration {
                playback.blend_target = Some(fade);
            } else {
                playback.blend_ratio = fade.to;
            }
        }
    }

    fn advance_fades(&mut self, frames: f64) {
        for state in self.variables.values_mut() {
            advance_state(state, frames);
        }
        for deltas in self.diff_variables.values_mut() {
            for state in deltas.values_mut() {
                advance_state(state, frames);
            }
        }
    }

    fn retain_active_playbacks(&mut self) {
        self.playbacks.retain(|playback| {
            !(playback.stopping
                && playback.blend_target.is_none()
                && playback.blend_ratio.abs() <= f64::EPSILON)
        });
        let active: BTreeSet<String> = self
            .playbacks
            .iter()
            .map(|playback| playback.name.clone())
            .collect();
        self.diff_variables.retain(|name, _| active.contains(name));
    }

    fn seek(&mut self, name: &str, time: f64) {
        let Some(timeline) = self.timelines.get(name).cloned() else {
            return;
        };
        let Some(index) = self
            .playbacks
            .iter()
            .position(|playback| playback.name == name)
        else {
            return;
        };
        let mut playback = self.playbacks.remove(index);
        self.seek_with(name, &timeline, &mut playback, time);
        playback.time_frames = time;
        self.playbacks.insert(index, playback);
    }

    fn seek_with(
        &mut self,
        timeline_name: &str,
        timeline: &Timeline,
        playback: &mut Playback,
        time: f64,
    ) {
        if playback.cursors.len() != timeline.tracks.len() {
            playback.cursors = vec![usize::MAX; timeline.tracks.len()];
        }
        for (track_index, track) in timeline.tracks.iter().enumerate() {
            if track.frames.is_empty() {
                continue;
            }
            let mut cursor = usize::MAX;
            let mut latest_command = None;
            for (index, frame) in track.frames.iter().enumerate() {
                if frame.time as f64 > time {
                    break;
                }
                cursor = index;
                if frame.kind != 0 {
                    latest_command = Some(index);
                }
            }
            playback.cursors[track_index] = cursor;
            if let Some(index) = latest_command {
                self.issue_command(timeline_name, track, index, time, playback.difference);
            }
        }
    }

    fn advance(
        &mut self,
        timeline_name: &str,
        timeline: &Timeline,
        playback: &mut Playback,
        target_time: f64,
        inclusive: bool,
    ) {
        if playback.cursors.len() != timeline.tracks.len() {
            playback.cursors = vec![usize::MAX; timeline.tracks.len()];
        }
        for (track_index, track) in timeline.tracks.iter().enumerate() {
            if track.frames.is_empty() {
                continue;
            }
            let mut cursor = playback.cursors[track_index];
            loop {
                let next_index = if cursor == usize::MAX { 0 } else { cursor + 1 };
                let Some(next) = track.frames.get(next_index) else {
                    break;
                };
                let next_time = next.time as f64;
                let crossed = if inclusive {
                    target_time >= next_time
                } else {
                    target_time > next_time
                };
                if !crossed {
                    break;
                }
                cursor = next_index;
                if next.kind != 0 {
                    self.issue_command(
                        timeline_name,
                        track,
                        cursor,
                        target_time,
                        playback.difference,
                    );
                }
            }
            playback.cursors[track_index] = cursor;
        }
    }

    fn issue_command(
        &mut self,
        timeline_name: &str,
        track: &crate::schema::TimelineTrack,
        frame_index: usize,
        command_time: f64,
        difference: bool,
    ) {
        let Some(frame) = track.frames.get(frame_index) else {
            return;
        };
        // TimelineVariableFrame::type == 0 is a cursor-only marker; native
        // sub_1026A660/sub_1026B1F0 deliberately skip the variable setter.
        if frame.kind == 0 {
            return;
        }
        let duration = track
            .frames
            .get(frame_index + 1)
            .map(|next| (next.time as f64 - command_time - 1.0).max(0.0))
            .unwrap_or(0.0);
        let easing = frame.easing.unwrap_or(0) as f64;
        let name = track.label.clone();
        let state = if difference {
            self.diff_variables
                .entry(timeline_name.to_owned())
                .or_default()
                .entry(name)
                .or_insert_with(|| VariableState::new(0.0))
        } else {
            self.variables
                .entry(name)
                .or_insert_with(|| VariableState::new(0.0))
        };
        if duration <= 0.0 || !duration.is_finite() {
            state.value = frame.value;
            state.target = None;
        } else {
            state.target = Some(Fade {
                from: state.value,
                to: frame.value,
                value: state.value,
                elapsed: 0.0,
                duration,
                easing,
            });
        }
    }
}

/// Native `sub_1026AD10` easing conversion followed by `pow(progress, exp)`.
pub fn control_easing(progress: f64, easing: f64) -> f64 {
    let progress = progress.clamp(0.0, 1.0);
    if !progress.is_finite() || !easing.is_finite() || easing == 0.0 {
        return progress;
    }
    let exponent = if easing < 0.0 {
        1.0 / (1.0 - easing)
    } else {
        easing + 1.0
    };
    progress.powf(exponent)
}

fn advance_fade(fade: &mut Fade, frames: f64) {
    fade.elapsed = (fade.elapsed + frames).min(fade.duration.max(0.0));
    let t = if fade.duration <= 0.0 {
        1.0
    } else {
        (fade.elapsed / fade.duration).clamp(0.0, 1.0)
    };
    fade.value = fade.from + (fade.to - fade.from) * control_easing(t, fade.easing);
}

fn advance_state(state: &mut VariableState, frames: f64) {
    let Some(mut fade) = state.target.take() else {
        return;
    };
    advance_fade(&mut fade, frames);
    state.value = fade.value;
    if fade.elapsed < fade.duration {
        state.target = Some(fade);
    } else {
        state.value = fade.to;
    }
}

fn timeline_end(timeline: &Timeline) -> f64 {
    if timeline.last_time > 0 {
        timeline.last_time as f64
    } else {
        timeline
            .tracks
            .iter()
            .flat_map(|track| track.frames.iter())
            .map(|frame| frame.time)
            .max()
            .unwrap_or(0) as f64
    }
}

/// Legacy stateless track sampling.
///
/// The player no longer uses this helper: timelines are command streams. It is
/// kept for diagnostics and for callers that explicitly want a static snapshot
/// of authored values without running the transition state machine.
pub fn sample_track(frames: &[TimelineFrame], time_frames: f64) -> Option<f64> {
    let first = frames.first()?;
    if time_frames <= first.time as f64 {
        return Some(first.value);
    }
    let mut previous: &TimelineFrame = first;
    for frame in &frames[1..] {
        if (frame.time as f64) >= time_frames {
            let span = (frame.time - previous.time) as f64;
            if span <= 0.0 {
                return Some(frame.value);
            }
            let t = (time_frames - previous.time as f64) / span;
            return Some(previous.value + (frame.value - previous.value) * t);
        }
        previous = frame;
    }
    Some(previous.value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::{Timeline, TimelineFrame, TimelineTrack};

    fn frame(time: i64, value: f64, kind: i64, easing: Option<i64>) -> TimelineFrame {
        TimelineFrame {
            time,
            kind,
            easing,
            value,
        }
    }

    fn model_with_timeline(timeline: Timeline) -> Model {
        let mut model = Model::default();
        model.timelines.push(timeline);
        model
    }

    #[test]
    fn command_stream_uses_next_frame_gap_minus_one() {
        // Command at 0 sets 10; the next command is at frame 10, so the
        // transition lasts 9 ticks (native duration = next.time - time - 1).
        let model = model_with_timeline(Timeline {
            label: "t".into(),
            last_time: 10,
            loop_begin: -1,
            loop_end: -1,
            tracks: vec![TimelineTrack {
                label: "v".into(),
                frames: vec![frame(0, 10.0, 2, Some(0)), frame(10, 0.0, 0, None)],
            }],
        });
        let mut animator = Animator::new(&model, 60.0);
        animator.play("t").unwrap();
        // The frame-0 command starts a transition from the variable default.
        assert_eq!(animator.variable("v"), Some(0.0));
        animator.tick(1.0 / 60.0);
        let value = animator.variable("v").unwrap();
        assert!((value - 10.0 / 9.0).abs() < 1.0e-9, "{value}");
    }

    #[test]
    fn easing_selector_changes_curve() {
        let model = model_with_timeline(Timeline {
            label: "t".into(),
            last_time: 10,
            loop_begin: -1,
            loop_end: -1,
            tracks: vec![TimelineTrack {
                label: "v".into(),
                frames: vec![frame(0, 10.0, 2, Some(0)), frame(10, 0.0, 0, None)],
            }],
        });
        let mut linear = Animator::new(&model, 60.0);
        linear.play("t").unwrap();
        for _ in 0..4 {
            linear.tick(1.0 / 60.0);
        }
        let linear_value = linear.variable("v").unwrap();

        let mut eased_model = model_with_timeline(Timeline {
            label: "t".into(),
            last_time: 10,
            loop_begin: -1,
            loop_end: -1,
            tracks: vec![TimelineTrack {
                label: "v".into(),
                frames: vec![frame(0, 10.0, 2, Some(1)), frame(10, 0.0, 0, None)],
            }],
        });
        let mut eased = Animator::new(&eased_model, 60.0);
        eased.play("t").unwrap();
        for _ in 0..4 {
            eased.tick(1.0 / 60.0);
        }
        let eased_value = eased.variable("v").unwrap();
        assert!(eased_value < linear_value);
        // 0 -> 10 over 9 ticks, exponent 2, at t=4/9.
        assert!((eased_value - 10.0 * (4.0f64 / 9.0).powi(2)).abs() < 1.0e-9);
        eased_model.timelines.clear();
    }

    #[test]
    fn hold_frames_are_cursor_only() {
        let model = model_with_timeline(Timeline {
            label: "t".into(),
            last_time: 20,
            loop_begin: -1,
            loop_end: -1,
            tracks: vec![TimelineTrack {
                label: "v".into(),
                frames: vec![
                    frame(0, 0.0, 2, Some(0)),
                    frame(5, 0.0, 0, None),
                    frame(10, 10.0, 2, Some(0)),
                    frame(20, 0.0, 0, None),
                ],
            }],
        });
        let mut animator = Animator::new(&model, 60.0);
        animator.play("t").unwrap();
        for _ in 0..10 {
            animator.tick(1.0 / 60.0);
        }
        let value = animator.variable("v").unwrap();
        assert!((value - 10.0 / 9.0).abs() < 1.0e-9, "{value}");
    }

    #[test]
    fn authored_loop_reseeks_loop_begin() {
        let model = model_with_timeline(Timeline {
            label: "t".into(),
            last_time: 21,
            loop_begin: 0,
            loop_end: 20,
            tracks: vec![TimelineTrack {
                label: "v".into(),
                frames: vec![
                    frame(0, 0.0, 2, Some(0)),
                    frame(10, 10.0, 2, Some(0)),
                    frame(20, 0.0, 0, None),
                ],
            }],
        });
        let mut animator = Animator::new(&model, 60.0);
        animator.play("t").unwrap();
        for _ in 0..20 {
            animator.tick(1.0 / 60.0);
        }
        let at_end = animator.variable("v").unwrap();
        assert!(at_end > 0.0);
        animator.tick(1.0 / 60.0);
        let after_loop = animator.variable("v").unwrap();
        // The loop seek reissues the frame-0 command at loopBegin; the loop
        // tick and this tick each advance the new transition by one tick.
        assert!(
            (after_loop - (10.0 - 20.0 / 9.0)).abs() < 1.0e-9,
            "{after_loop}"
        );
        assert_eq!(animator.playbacks()[0].time_frames, 1.0);
    }

    #[test]
    fn timed_api_write_uses_easing_exponent() {
        let model = Model::default();
        let mut animator = Animator::new(&model, 60.0);
        animator.set_variable("v", 0.0);
        animator.set_variable_timed("v", 10.0, 1.0, 1.0);
        for _ in 0..30 {
            animator.tick(1.0 / 60.0);
        }
        let value = animator.variable("v").unwrap();
        assert!((value - 10.0 * 0.25).abs() < 1.0e-9, "{value}");
    }

    #[test]
    fn difference_timeline_adds_scaled_delta() {
        let model = model_with_timeline(Timeline {
            label: "diff".into(),
            last_time: 60,
            loop_begin: 0,
            loop_end: 60,
            tracks: vec![TimelineTrack {
                label: "v".into(),
                // Authored delta of 4, held for the whole loop.
                frames: vec![
                    frame(0, 4.0, 2, Some(0)),
                    frame(1, 0.0, 0, None),
                    frame(60, 0.0, 0, None),
                ],
            }],
        });
        let mut animator = Animator::new(&model, 60.0);
        animator.set_variable("v", 5.0);
        animator.play_diff("diff", 1.0, 1.0).unwrap();
        // Fade starts at ratio 0.
        assert_eq!(animator.variable("v"), Some(5.0));
        for _ in 0..30 {
            animator.tick(1.0 / 60.0);
        }
        let half = animator.variable("v").unwrap();
        assert!((half - 7.0).abs() < 1.0e-9, "{half}");
        for _ in 0..30 {
            animator.tick(1.0 / 60.0);
        }
        let full = animator.variable("v").unwrap();
        assert!((full - 9.0).abs() < 1.0e-9, "{full}");
        animator.stop_with_fade("diff", 1.0);
        for _ in 0..61 {
            animator.tick(1.0 / 60.0);
        }
        assert_eq!(animator.variable("v"), Some(5.0));
        assert!(animator.playbacks().is_empty());
    }
}
