# yu-emote

Clean-room E-mote (EMT/PSB) runtime for YU-RIS, written in Rust and designed for
native + `wasm32` use.

> E-mote is a 2D character-animation middleware by M2 Co., Ltd. This project is
> not affiliated with M2. It only reads/writes E-mote data files.

## Status

| Layer | State |
|---|---|
| PSB/MDF container parsing (via MIT `emote-psb`) | ✅ |
| Body decryption (E-mote key) + key search | ✅ (per-game key, not shipped) |
| Deterministic model summary (`psb-info`) | ✅ |
| YU-RIS adapter: `EMOTE` params, `EMOTEINFO` queries, capability, suspend/dirty | ✅ |
| Typed model schema: screen, atlas pages, objects/motions/layers/frames, variables, timelines, controllers (`psb-dump`) | ✅ M1 |
| Atlas decoding (`RGBA4444`, `A4R4G4B4` order evidenced from the DLL) | ✅ M1 |
| Deterministic variable/timeline state (`Animator`: PLAY/DPLAY/STOP, fades, looping) | ✅ M2 core (differential fade curve pending) |
| WGSL for all 58 official shader variants + cubic Bezier mesh tessellation | ✅ M3 core (wgpu pipeline pending) |
| Pixel parity vs. official `YSEmote.dll` | ⏳ M4 |

This repository intentionally does **not** claim rendering yet. The first
milestone is a trustworthy data layer; animation and rendering follow the task
book in the kernel repo (`docs/tasks/task-emote-runtime.md`).

## Usage

```bash
# Plain PSB
cargo run --bin psb-info -- /path/to/model.psb

# Body-encrypted PSB (recover the per-game key first)
cargo run --release --bin psb-key-search -- /path/to/model.psb
cargo run --bin psb-info -- /path/to/model.psb --key 0x175058c9

# Deterministic typed dump (+ optional raw tree, resources, decoded textures)
cargo run --bin psb-dump -- /path/to/model.psb --key 0x175058c9
cargo run --bin psb-dump -- model.psb --key 0x175058c9 --raw --resources out --decode
```

Verified against the YU-RIS ERIS sample `emote_test.psb` (PSB v2, body key
`0x175058c9`): 2 texture pages (`tex#0` 2048x2048 / `tex#1` 512x256, both
`RGBA4444`), 6 objects, 21 motions, 234 layers, 888 layer keyframes, 24
variables, 11 timelines. The key is data-specific and is **not** hardcoded in
the library.

## Evidence-backed facts

These come from the official SDK 0.495 binaries and the official sample; they
are recorded here so the implementation can be audited against them:

- **Key source.** `PEmotePlayer`'s constructor calls `strtol` on the decimal
  string `"391141577"` at `emotedriver.dll` VA `0x10071350` and uses the result
  as the fourth word of the PSB body cipher (`0x175058C9` for this build).
  Games may ship a different `emotedriver.dll`; the key is not a constant of
  the model format.
- **Texture order.** `RGBA4444` pixels use the `D3DFMT_A4R4G4B4` bit layout
  (alpha bits 15..12, red 11..8, green 7..4, blue 3..0), expanded with `n*17`.
  This matches the DLL's converter at `0x10054BA0` and reproduces the official
  atlas colors; the opposite nibble order swaps hair/skin colors.
- **Shader table.** `emotedriver.dll` holds 58 unique D3D9 programs in the
  pointer table at VA `0x100866c0..0x100867ac`: 27 `ps_2_0`, 27 `ps_3_0`,
  3 `vs_2_0`, 1 `vs_3_0`. The 27 pixel programs are 13 plain, 13 alpha-mask
  (`cAlphaMaskTex.g`), 1 pixelate (`cPixelateTex`); `ps_3_0` masked variants
  add `float2 cScMul`. [`src/render.rs`](src/render.rs) records each variant's
  feature set.
- **YSEmote dispatch.** `YSEmote.dll` exports ordinal 2 = `EmoteCreate`,
  ordinal 3 = `EmoteFilterTexture`; ordinals 101..121 call the device vtable
  slots `+0x00..+0x50`, ordinals 201..256 call the player vtable slots
  `+0x00..+0xdc`. The engine's loader stores them at `yu-ris.exe` `0x801384..`.
- **Contracts.** `EMOTE` resolves the character, then applies named string /
  flag / numeric variables; numeric fades are converted with `value * 0.06`
  before reaching the player. `EMOTEINFO` calls player slot 45. Details and
  remaining gaps are in the kernel's `docs/emote-parity.md`.

```rust
use yu_emote::model::ModelSummary;
use yu_emote::psb;

let document = psb::read_document("emote_test.psb")?;
let summary = ModelSummary::from_document(&document);
println!("{}", serde_json::to_string_pretty(&summary)?);
```

YU-RIS adapter:

```rust
use yu_emote::yuris::{Capability, EmoteArg, YurisEmote};

let mut emote = YurisEmote::new(Capability::DataOnly);
emote.apply_emote([("BUST", EmoteArg::Int(128)), ("SMOOTH", EmoteArg::Int(1))])?;
// SYSTEMINFO[EMOTE] must stay 0 until a renderer is ready.
assert_eq!(emote.capability(), Capability::DataOnly);
```

## Design boundaries

- **No fake semantics.** Unknown `EMOTE` parameters are recorded; unknown
  `EMOTEINFO` fields return an explicit error instead of a guessed value.
- **YU-RIS glue is separate** from the E-mote core: task suspension, capability
  reporting and dirty-frame tracking live in `src/yuris.rs`; the data layer is
  engine-neutral.
- **wasm-friendly.** The data layer is pure Rust with no OS dependencies. The
  renderer (M3) will use wgpu/WebGPU so the same code runs natively and on
  `wasm32-unknown-unknown`. The official Windows DLL cannot run in wasm and is
  only used as an oracle.

## Attribution and licensing

Dual-licensed under **MIT OR Apache-2.0**.

- PSB/MDF binary parsing: [`emote-psb`](https://github.com/storycraft/emote-psb-rs)
  by storycraft, MIT.
- PSB body stream cipher schedule and single-DWORD key search: documented by
  FreeMote and eluna; this repository contains an independent implementation
  (no code copied).
- Model normalization / part-table / wasm ABI concepts:
  [`amadeus-emote-loader`](https://github.com/Mieluoxxx/amadeus-emote-loader),
  Apache-2.0 (reference; no code copied in this milestone).
- Behavior/oracle references only (no code): [`eluna`](https://github.com/xmoezzz/eluna)
  (no license), [`FreeMote`](https://github.com/UlyssesWu/FreeMote) and its WebGL
  runtime (CC BY-NC-SA 4.0).

See `NOTICE` for details.

## Related

- YU-RIS kernel: <https://github.com/NotFaceGUI/yuris-kernel>
- E-mote audit/evidence ledger (in the kernel repo): `docs/emote-parity.md`
- Task book: `docs/tasks/task-emote-runtime.md`
