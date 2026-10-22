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
| PSB model normalization / part visibility | ⏳ M1 |
| Motion/timeline evaluation (VAR/PLAY/DPLAY/STOP/DSTOP/VO) | ⏳ M2 |
| wgpu / WebGPU mesh renderer (58 E-mote shader variants) | ⏳ M3 |
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
```

Verified against the YU-RIS ERIS sample `emote_test.psb` (PSB v2, body key
`0x175058c9`): 2 texture resources, `metadata` present, 5863 objects, max
depth 36. The key is data-specific and is **not** hardcoded in the library.

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
