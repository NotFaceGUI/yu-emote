# E-mote shader variants (WGSL)

This directory contains one WGSL file per unique D3D9 program embedded in the
official `emotedriver.dll` (SDK 0.495). The programs were located through the
pointer table at `VA 0x100866C0..0x100867AC` and disassembled with
`fxc /dumpbin`; the WGSL is an independent implementation of the observed
semantics. No official shader source was copied.

## Layout

```text
shaders/
  ps_2_0/00.wgsl .. 26.wgsl   27 pixel shaders
  ps_3_0/00.wgsl .. 26.wgsl   27 pixel shaders
  vs_2_0/00.wgsl .. 02.wgsl    3 vertex shaders
  vs_3_0/00.wgsl               1 vertex shader (same program as vs_2_0/00)
```

The index is the position in the official pointer table, not a made-up order.

| Pixel indices | Group | Uniforms |
|---|---|---|
| 0,1,2,6,7,8,12,13,14,18,19,20,24 | plain | `cTex` |
| 3,4,5,9,10,11,15,16,17,21,22,23,25 | alpha mask | `cTex`, `cAlphaMaskTex` |
| 26 | pixelate | `cTex`, `cPixelateTex` |

`ps_3_0` masked/pixelate variants additionally use `float2 cScMul`; they
compute the mask/pixelate UV in the fragment shader as
`(vPos + 0.5) * cScMul` instead of using a vertex interpolant.

Vertex programs:

| File | Program |
|---|---|
| `vs_2_0/00.wgsl` | `cMat`, passthrough color/UV |
| `vs_2_0/01.wgsl` | `cMat` + `cScOffset`, screen UV from NDC |
| `vs_2_0/02.wgsl` | `cMat` + `cScOffset`, scaled screen UV |
| `vs_3_0/00.wgsl` | same as `vs_2_0/00` |

## Bindings

| Binding | Resource |
|---|---|
| 0 | `cTex` / `cMat` |
| 1 | sampler / `cScOffset` |
| 2 | `cAlphaMaskTex` or `cPixelateTex` |
| 3 | `cScMul` (`ps_3_0` masked/pixelate only) |

The bind-group layout is not final: the runtime may move the sampler to a
separate group. The uniform *semantics* are what this folder records.

## Regenerating

```bash
cargo run --bin gen-shaders
```

The generator lives in `src/render.rs`
(`generated_pixel_wgsl`/`generated_vertex_wgsl`). The
`shader_files_match_generator` test fails if a file drifts from it, and
`all_official_shader_files_are_present` checks all 58 entries.

## Evidence gaps

- The WGSL is a semantic transcription, not a bit-exact D3D9 emulation.
- `vs_2_0/02` uses the `c4.wzzw` form observed at `0x10032D...`; the exact
  scale meaning still needs M4 confirmation.
- Pixel parity against the official D3D9 renderer is M4 work.
