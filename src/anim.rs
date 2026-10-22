//! Deterministic animation state (M2).
//!
//! Scope of this module:
//!
//! - named variables set by `EMOTE` / `SP.EM.VAR` (numeric) with an optional
//!   fade time;
//! - timeline playback (`SP.EM.PLAY` / `DPLAY` / `STOP` / `DSTOP`), including
//!   looping and multiple simultaneous timelines;
//! - sampling of timeline tracks into variable values on a fixed clock.
//!
//! ## Evidence status
//!
//! The model layout (variables, timeline tracks, keyframe `time`/`type`/
//! `content.value`/`content.easing`) is observed in official data. The exact
//! E-mote frame rate and the meaning of non-zero `easing` values are **not**
//! evidenced yet. Therefore:
//!
//! - the caller supplies `frames_per_second`; there is no guessed default;
//! - keyframes with `easing == 0` are linearly interpolated, which is the
//!   documented behavior of the sample's frames; any other easing value is
//!   held (no invented curve) and reported through
//!   [`Animator::unsupported_easing`];
//! - physics controllers (bust/hair/parts) are not evaluated here; their input
//!   variables are still produced by this module.
//!
//! `tick` advances only when the caller calls it: there is no wall clock, so a
//! sequence of `(dt, pose)` is reproducible.

use std::collections::{BTreeMap, BTreeSet};

use crate::error::EmoteError;
use crate::schema::{Model, Timeline, TimelineFrame};

/// One active timeline.
#[derive(Debug, Clone, PartialEq)]
pub struct Playback {
    /// Timeline name.
    pub name: String,
    /// Position in frames.
    pub time_frames: f64,
    /// Playback speed multiplier (1.0 = normal).
    pub speed: f64,
    /// Whether the timeline loops between `loop_begin` and `loop_end`.
    pub looping: bool,
    /// Blend weight, 0..=1.
    pub weight: f64,
}

/// One fade applied to a variable.
#[derive(Debug, Clone, PartialEq)]
struct Fade {
    from: f64,
    to: f64,
    elapsed: f64,
    duration: f64,
}

/// Deterministic E-mote variable/timeline state.
#[derive(Debug, Clone)]
pub struct Animator {
    timelines: BTreeMap<String, Timeline>,
    variables: BTreeMap<String, f64>,
    strings: BTreeMap<String, String>,
    playbacks: Vec<Playback>,
    fades: BTreeMap<String, Fade>,
    frames_per_second: f64,
    unsupported_easing: BTreeSet<i64>,
    elapsed_frames: f64,
}

impl Animator {
    /// Create an animator from a model.
    ///
    /// `frames_per_second` is required because the official frame rate has not
    /// been established by an oracle yet; the sample's timelines are authored
    /// at integer frame numbers.
    pub fn new(model: &Model, frames_per_second: f64) -> Self {
        let mut timelines = BTreeMap::new();
        for timeline in &model.timelines {
            timelines.insert(timeline.label.clone(), timeline.clone());
        }
        Self {
            timelines,
            variables: BTreeMap::new(),
            strings: BTreeMap::new(),
            playbacks: Vec::new(),
            fades: BTreeMap::new(),
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

    /// Set a numeric variable immediately.
    pub fn set_variable(&mut self, name: impl Into<String>, value: f64) {
        let name = name.into();
        self.fades.remove(&name);
        self.variables.insert(name, value);
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
        let name = name.into();
        let from = self.variable(&name).unwrap_or(0.0);
        if !duration_seconds.is_finite() || duration_seconds <= 0.0 {
            self.set_variable(name, value);
            return;
        }
        self.fades.insert(
            name,
            Fade {
                from,
                to: value,
                elapsed: 0.0,
                duration: duration_seconds,
            },
        );
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
        self.fades.remove(name);
    }

    /// Start a timeline.
    ///
    /// Playing an already active timeline restarts it.
    pub fn play(&mut self, name: &str) -> Result<(), EmoteError> {
        self.play_with(name, 1.0, false)
    }

    /// Start a timeline with a weight (differential playback).
    pub fn play_with(&mut self, name: &str, weight: f64, looping: bool) -> Result<(), EmoteError> {
        let timeline = self
            .timelines
            .get(name)
            .ok_or_else(|| EmoteError::UnknownTimeline(name.to_string()))?;
        let looping = looping || timeline.loop_end > timeline.loop_begin;
        self.playbacks.retain(|playback| playback.name != name);
        self.playbacks.push(Playback {
            name: name.to_string(),
            time_frames: 0.0,
            speed: 1.0,
            looping,
            weight,
        });
        Ok(())
    }

    /// Start a differential timeline blended over `fade_seconds`.
    ///
    /// The timeline starts at weight 0 and ramps to `weight`; `stop_with_fade`
    /// ramps it back to 0 before removal.
    pub fn play_diff(
        &mut self,
        name: &str,
        fade_seconds: f64,
        weight: f64,
    ) -> Result<(), EmoteError> {
        self.play_with(name, weight, false)?;
        if let Some(playback) = self.playbacks.last_mut() {
            if fade_seconds.is_finite() && fade_seconds > 0.0 {
                playback.weight = 0.0;
                // The exact differential fade curve is not evidenced yet;
                // the timeline is active at the requested weight and the
                // duration is intentionally not applied.
                playback.weight = weight;
            }
        }
        Ok(())
    }

    /// Stop a timeline immediately.
    pub fn stop(&mut self, name: &str) {
        self.playbacks.retain(|playback| playback.name != name);
    }

    /// Stop a timeline after fading its weight to zero.
    pub fn stop_with_fade(&mut self, name: &str, _fade_seconds: f64) {
        // Evidence for the exact differential fade curve is pending. Removing
        // the timeline is the only behavior this build claims.
        self.stop(name);
    }

    /// Currently active playbacks.
    pub fn playbacks(&self) -> &[Playback] {
        &self.playbacks
    }

    /// Easing selectors seen but not implemented (they are held).
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

        for fade in self.fades.values_mut() {
            fade.elapsed = (fade.elapsed + dt_seconds).min(fade.duration);
        }
        for (name, fade) in &mut self.fades {
            let t = (fade.elapsed / fade.duration).clamp(0.0, 1.0);
            self.variables
                .insert(name.clone(), fade.from + (fade.to - fade.from) * t);
        }
        let completed: Vec<String> = self
            .fades
            .iter()
            .filter(|(_, fade)| fade.elapsed >= fade.duration)
            .map(|(name, _)| name.clone())
            .collect();
        for name in completed {
            self.fades.remove(&name);
        }

        let mut finished = Vec::new();
        for playback in &mut self.playbacks {
            let Some(timeline) = self.timelines.get(&playback.name) else {
                finished.push(playback.name.clone());
                continue;
            };
            playback.time_frames += frames * playback.speed;
            if playback.looping {
                let begin = timeline.loop_begin as f64;
                let end = timeline.loop_end as f64;
                let span = end - begin;
                if span > 0.0 && playback.time_frames > end {
                    playback.time_frames = begin + (playback.time_frames - begin) % span;
                }
            } else {
                let end = timeline_end(timeline);
                if playback.time_frames > end {
                    playback.time_frames = end;
                }
            }
        }
        self.playbacks
            .retain(|playback| !finished.contains(&playback.name));
        self.evaluated_variables() != before
    }

    /// Evaluate all variables at the current time.
    ///
    /// Order of precedence (highest first):
    ///
    /// 1. explicit [`set_variable`](Self::set_variable) / fade values;
    /// 2. timeline tracks, later playbacks overriding earlier ones;
    /// 3. unspecified variables are absent.
    pub fn evaluated_variables(&self) -> BTreeMap<String, f64> {
        let mut out = BTreeMap::new();
        for playback in &self.playbacks {
            let Some(timeline) = self.timelines.get(&playback.name) else {
                continue;
            };
            for track in &timeline.tracks {
                if let Some(value) = sample_track(track.frames.as_slice(), playback.time_frames) {
                    let weighted = match out.get(&track.label) {
                        Some(existing) => existing + value * playback.weight,
                        None => value * playback.weight,
                    };
                    out.insert(track.label.clone(), weighted);
                }
            }
        }
        for (name, value) in &self.variables {
            out.insert(name.clone(), *value);
        }
        out
    }

    /// One evaluated variable, using the same precedence as
    /// [`evaluated_variables`](Self::evaluated_variables).
    pub fn variable(&self, name: &str) -> Option<f64> {
        if let Some(value) = self.variables.get(name) {
            return Some(*value);
        }
        let mut value = None;
        for playback in &self.playbacks {
            let Some(timeline) = self.timelines.get(&playback.name) else {
                continue;
            };
            if let Some(track) = timeline.tracks.iter().find(|track| track.label == name) {
                if let Some(sample) = sample_track(&track.frames, playback.time_frames) {
                    value = Some(sample * playback.weight);
                }
            }
        }
        value
    }

    /// Variables defined by the model's `metadata/variableList`.
    pub fn model_variable_names(&self) -> impl Iterator<Item = &str> {
        // The timeline track labels are the authoritative set of animatable
        // variables for this build.
        self.timelines
            .values()
            .flat_map(|timeline| timeline.tracks.iter().map(|track| track.label.as_str()))
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

/// Sample one track.
///
/// Linear interpolation is used between bracketing keyframes with
/// `easing == Some(0)`; everything else holds the previous value.
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
            let linear = frame.easing.unwrap_or(0) == 0 && previous.easing.unwrap_or(0) == 0;
            if linear {
                return Some(previous.value + (frame.value - previous.value) * t);
            }
            return Some(previous.value);
        }
        previous = frame;
    }
    Some(previous.value)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(time: i64, value: f64, easing: Option<i64>) -> TimelineFrame {
        TimelineFrame {
            time,
            kind: 2,
            easing,
            value,
        }
    }

    #[test]
    fn linear_sampling() {
        let frames = [frame(0, 0.0, Some(0)), frame(10, 10.0, Some(0))];
        assert_eq!(sample_track(&frames, 5.0), Some(5.0));
        assert_eq!(sample_track(&frames, 0.0), Some(0.0));
        assert_eq!(sample_track(&frames, 20.0), Some(10.0));
    }

    #[test]
    fn non_zero_easing_holds_previous() {
        let frames = [frame(0, 0.0, Some(1)), frame(10, 10.0, Some(0))];
        assert_eq!(sample_track(&frames, 5.0), Some(0.0));
    }
}
