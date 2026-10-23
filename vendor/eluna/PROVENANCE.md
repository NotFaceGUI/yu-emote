# Vendored `eluna_rs`

This directory contains the `eluna_rs` crate copied from the upstream
`eluna` project.

- Upstream: <https://github.com/xmoezzz/eluna>
- Commit: `9a000293968392daa34c83f82b991f7ba25ccc29` (2026-09-26)
- License: Mozilla Public License 2.0 (see `LICENSE`)
- Package: `eluna_rs`, library name `eluna`

## Why it is vendored

`yu-emote` targets the E-mote feature as used by YU-RIS. The PSB data layer,
motion/timeline evaluation, mesh-chain deformation, physics and the SDK-facing
player state machine are taken directly from `eluna_rs` instead of being
reimplemented. `yu-emote` adds the YU-RIS/ERIS loading path, texture decoding,
the renderer bridge and the demo tooling on top.

Keeping the source in-tree is required by the MPL-2.0 so that the exact
covered source used by this project is available. The files remain under the
MPL-2.0; the surrounding `yu-emote` code is licensed separately.

## Local modifications

Only these changes are applied to the vendored source:

1. `src/psb/mod.rs`, `read_uint_array`: accept the bare `0x0c` integer-array
   prefix that PSB v4 (`spec: ems`) models use for an empty array, both as the
   length marker and as a zero-byte item type with length 0.

2. `src/sdk.rs`: add read-only `EmoteRuntime::timeline_loop_begin` and
   `timeline_loop_end` accessors. Hosts need the authored loop boundaries to
   start a preview clip at the steady-state loop point; no runtime behavior is
   changed.

Any further change must be recorded here. Debug-only instrumentation from
local analysis checkouts is intentionally not vendored.
