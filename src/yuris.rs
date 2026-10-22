//! YU-RIS adapter state for `EMOTE` / `EMOTEINFO`.
//!
//! This module is deliberately separate from the data layer. YU-RIS has its
//! own task scheduler, suspension rules and redraw policy; the adapter keeps
//! those concerns here so the E-mote core can stay renderer- and engine-neutral.
//!
//! Evidence status (see the kernel's `docs/emote-parity.md`):
//!
//! - `EMOTE` observed parameters in the ERIS corpus: `BUST`, `HAIR`, `PARTS`,
//!   `SMOOTH` (numeric). Unknown parameters are recorded, not guessed.
//! - `EMOTEINFO` field semantics are **not** evidenced yet; queries for
//!   anything outside the recorded numeric set return
//!   [`EmoteError::UnsupportedInfo`] instead of inventing a value.
//! - `SYSTEMINFO[EMOTE]` is a capability bit. Until a renderer exists, report
//!   [`Capability::Unavailable`] so scripts do not take the E-mote branch.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use serde::Serialize;

use crate::error::EmoteError;

/// Numeric parameter names observed in the ERIS corpus.
pub const KNOWN_NUMERIC: [&str; 4] = ["BUST", "HAIR", "PARTS", "SMOOTH"];

/// E-mote runtime capability, mirrored to `SYSTEMINFO[EMOTE]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum Capability {
    /// No E-mote plugin/runtime available (report `0` to scripts).
    Unavailable,
    /// PSB data can be parsed/inspected, but no character can be rendered.
    DataOnly,
    /// A renderer is available and animation may advance.
    Ready,
}

impl Capability {
    /// Map the official engine's `LoadLibrary("YSEmote.dll")` result to a
    /// capability. Loading the DLL proves a runtime exists; without our own
    /// renderer we still stop at [`Capability::DataOnly`].
    pub const fn from_plugin_loaded(loaded: bool) -> Self {
        if loaded {
            Self::DataOnly
        } else {
            Self::Unavailable
        }
    }

    /// Name used in diagnostics.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Unavailable => "unavailable",
            Self::DataOnly => "data_only",
            Self::Ready => "ready",
        }
    }
}

impl fmt::Display for Capability {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One evaluated `EMOTE` argument.
#[derive(Debug, Clone, PartialEq)]
pub enum EmoteArg {
    /// Integer value.
    Int(i64),
    /// Floating-point value.
    Float(f64),
    /// String value (e.g. a timeline name); stored but not interpreted yet.
    Str(String),
}

impl From<i64> for EmoteArg {
    fn from(value: i64) -> Self {
        Self::Int(value)
    }
}

impl From<f64> for EmoteArg {
    fn from(value: f64) -> Self {
        Self::Float(value)
    }
}

impl From<String> for EmoteArg {
    fn from(value: String) -> Self {
        Self::Str(value)
    }
}

impl From<&str> for EmoteArg {
    fn from(value: &str) -> Self {
        Self::Str(value.to_string())
    }
}

/// YU-RIS-side E-mote state: parameters, capability, suspension, dirty flag.
#[derive(Debug, Clone)]
pub struct YurisEmote {
    capability: Capability,
    variables: BTreeMap<String, f64>,
    strings: BTreeMap<String, String>,
    unknown: BTreeSet<String>,
    suspended: bool,
    dirty: bool,
    clock_ms: u64,
}

impl Default for YurisEmote {
    fn default() -> Self {
        Self::new(Capability::Unavailable)
    }
}

impl YurisEmote {
    /// Create an adapter with an explicit capability.
    pub fn new(capability: Capability) -> Self {
        Self {
            capability,
            variables: BTreeMap::new(),
            strings: BTreeMap::new(),
            unknown: BTreeSet::new(),
            suspended: false,
            dirty: true,
            clock_ms: 0,
        }
    }

    /// Current capability.
    pub const fn capability(&self) -> Capability {
        self.capability
    }

    /// Update the capability (e.g. when a plugin becomes available).
    pub fn set_capability(&mut self, capability: Capability) {
        self.capability = capability;
    }

    /// Apply one `EMOTE` call.
    ///
    /// Known numeric parameters are stored; unknown names are recorded in
    /// [`Self::unknown_parameters`] and ignored (the YU-RIS contract consumes
    /// by name). A known numeric parameter passed as a string is a conformance
    /// error rather than a silent coercion.
    pub fn apply_emote<'a, I>(&mut self, params: I) -> Result<(), EmoteError>
    where
        I: IntoIterator<Item = (&'a str, EmoteArg)>,
    {
        for (name, arg) in params {
            if !KNOWN_NUMERIC.contains(&name) {
                if let EmoteArg::Str(value) = &arg {
                    self.strings.insert(name.to_string(), value.clone());
                }
                self.unknown.insert(name.to_string());
                continue;
            }
            match arg {
                EmoteArg::Int(value) => {
                    self.variables.insert(name.to_string(), value as f64);
                }
                EmoteArg::Float(value) => {
                    self.variables.insert(name.to_string(), value);
                }
                EmoteArg::Str(_) => {
                    return Err(EmoteError::UnsupportedParameterType {
                        name: name.to_string(),
                    });
                }
            }
            self.dirty = true;
        }
        Ok(())
    }

    /// Query `EMOTEINFO` fields.
    ///
    /// Only the observed numeric parameter set is supported. Missing or
    /// unverified fields return [`EmoteError::UnsupportedInfo`]; the adapter
    /// does not invent defaults.
    pub fn query_info(&self, fields: &[&str]) -> Result<BTreeMap<String, EmoteArg>, EmoteError> {
        let mut out = BTreeMap::new();
        for field in fields {
            if !KNOWN_NUMERIC.contains(field) {
                return Err(EmoteError::UnsupportedInfo((*field).to_string()));
            }
            let Some(value) = self.variables.get(*field) else {
                return Err(EmoteError::UnsupportedInfo((*field).to_string()));
            };
            out.insert((*field).to_string(), number_arg(*value));
        }
        Ok(out)
    }

    /// Freeze the animation clock (YU-RIS task suspension).
    pub fn suspend(&mut self) {
        self.suspended = true;
    }

    /// Resume the animation clock.
    pub fn resume(&mut self) {
        self.suspended = false;
    }

    /// Whether the animation clock is suspended.
    pub const fn is_suspended(&self) -> bool {
        self.suspended
    }

    /// Advance the deterministic clock by `dt_ms`.
    ///
    /// Returns `true` when the character state advanced and a redraw is
    /// required. Suspended, non-ready, or zero-length ticks do nothing; the
    /// host can skip E-mote work when this returns `false`.
    pub fn tick(&mut self, dt_ms: u32) -> bool {
        if self.capability != Capability::Ready || self.suspended || dt_ms == 0 {
            return false;
        }
        self.clock_ms = self.clock_ms.saturating_add(u64::from(dt_ms));
        self.dirty = true;
        true
    }

    /// Take the pending redraw flag (clears it).
    pub fn take_dirty(&mut self) -> bool {
        std::mem::take(&mut self.dirty)
    }

    /// Recorded numeric variables.
    pub fn variables(&self) -> &BTreeMap<String, f64> {
        &self.variables
    }

    /// Recorded string parameters (timeline names etc.), not interpreted yet.
    pub fn strings(&self) -> &BTreeMap<String, String> {
        &self.strings
    }

    /// Names seen in `EMOTE` that are not in [`KNOWN_NUMERIC`].
    pub fn unknown_parameters(&self) -> &BTreeSet<String> {
        &self.unknown
    }

    /// Current animation clock in milliseconds.
    pub const fn clock_ms(&self) -> u64 {
        self.clock_ms
    }

    /// Reset the clock and dirty flag (not the recorded parameters).
    pub fn reset_clock(&mut self) {
        self.clock_ms = 0;
        self.dirty = true;
    }
}

fn number_arg(value: f64) -> EmoteArg {
    if value.is_finite()
        && value.fract() == 0.0
        && value >= i64::MIN as f64
        && value <= i64::MAX as f64
    {
        EmoteArg::Int(value as i64)
    } else {
        EmoteArg::Float(value)
    }
}
