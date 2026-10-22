#!/usr/bin/env python3
"""Render the real-game E-mote sample and build a self-contained demo page.

The official test model shipped with YU-RIS games is *not* redistributed here.
Point `--psb` at a decrypted `emote_test.psb` (or pass `--key` and let
`psb-render` decrypt it) and the script writes:

    target/emote-demo/index.html   single-file page (frames embedded as PNG)
    target/emote-demo/frames/*.png intermediate frames

Everything is produced by the clean-room CPU renderer in this repository.
"""

from __future__ import annotations

import argparse
import base64
import io
import json
import subprocess
import sys
from pathlib import Path

from PIL import Image

ROOT = Path(__file__).resolve().parents[1]
RENDER = ROOT / "target" / "debug" / ("psb-render.exe" if sys.platform == "win32" else "psb-render")
OUT = ROOT / "target" / "emote-demo"

# (id, button label, variables, accent)
POSES = [
    ("neutral", "通常", [], "#8aa4c8"),
    ("smile", "笑顔", ["face_mouth=30", "face_eyebrow=40", "face_eye_open=20"], "#e8b45a"),
    ("angry", "怒り", ["face_mouth=10", "face_eyebrow=20", "face_eye_open=-5"], "#d46a5a"),
    ("sad", "哀しみ", ["face_mouth=20", "face_eyebrow=30"], "#7d8fd6"),
    ("cry", "涙", ["face_mouth=20", "face_eyebrow=30", "face_tears=2", "face_eye_open=10"], "#6fa6d8"),
    ("surprised", "驚き", ["face_eye_open=-10", "face_mouth=40"], "#d8a45a"),
    ("blink", "半目", ["face_eye_open=10"], "#9a9a9a"),
    ("talk", "口パク", ["face_talk=5", "face_mouth=30"], "#64b5a0"),
    ("head_tilt", "首かしげ", ["head_slant=30"], "#b58ad4"),
    ("body_tilt", "体かしげ", ["body_slant=30"], "#8ab4d4"),
    ("move_left", "移動", ["move_LR=-30", "move_UD=1"], "#c88a8a"),
]


def run_render(psb: Path, key: str | None, variables: list[str], out: Path) -> dict:
    out.parent.mkdir(parents=True, exist_ok=True)
    cmd = [
        str(RENDER),
        str(psb),
        "--object",
        "all_parts",
        "--motion",
        "全体構造",
        "--time",
        "0",
        "--out",
        str(out),
    ]
    if key:
        cmd += ["--key", key]
    for spec in variables:
        cmd += ["--var", spec]
    proc = subprocess.run(cmd, capture_output=True, text=True)
    if proc.returncode != 0:
        raise SystemExit(f"render failed for {out.name}:\n{proc.stderr or proc.stdout}")
    try:
        return json.loads(proc.stdout)
    except json.JSONDecodeError:
        return {}


def trim_union(images: list[Image.Image], pad: int = 28) -> tuple[int, int, int, int]:
    boxes = [im.getchannel("A").getbbox() for im in images]
    boxes = [box for box in boxes if box]
    if not boxes:
        raise SystemExit("all rendered frames are empty")
    left = min(box[0] for box in boxes) - pad
    top = min(box[1] for box in boxes) - pad
    right = max(box[2] for box in boxes) + pad
    bottom = max(box[3] for box in boxes) + pad
    return (max(0, left), max(0, top), right, bottom)


def encode_png(im: Image.Image) -> str:
    buf = io.BytesIO()
    im.save(buf, "PNG", optimize=True)
    return base64.b64encode(buf.getvalue()).decode("ascii")


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--psb", required=True, help="path to emote_test.psb")
    parser.add_argument("--key", default=None, help="per-game PSB key, e.g. 0x175058c9")
    parser.add_argument("--max-height", type=int, default=920)
    args = parser.parse_args()

    psb = Path(args.psb)
    if not RENDER.exists():
        raise SystemExit(f"{RENDER} is missing; run `cargo build --bins` first")
    if not psb.exists():
        raise SystemExit(f"{psb} does not exist")

    frames: list[tuple[str, str, str, Image.Image, dict]] = []
    for pose_id, label, variables, accent in POSES:
        raw_path = OUT / "frames" / f"{pose_id}.png"
        stats = run_render(psb, args.key, variables, raw_path)
        frames.append((pose_id, label, accent, Image.open(raw_path).convert("RGBA"), stats))

    box = trim_union([frame[3] for frame in frames])
    height = min(args.max_height, box[3] - box[1])
    width = max(1, round((box[2] - box[0]) * height / (box[3] - box[1])))

    prepared = []
    for pose_id, label, accent, image, stats in frames:
        cropped = image.crop(box).resize((width, height), Image.LANCZOS)
        prepared.append((pose_id, label, accent, encode_png(cropped), stats))

    neutral_stats = next((stats for pid, _, _, _, stats in prepared if pid == "neutral"), {})
    stats_json = json.dumps(neutral_stats, ensure_ascii=False)
    data_json = json.dumps(
        [
            {"id": pid, "label": label, "accent": accent, "src": f"data:image/png;base64,{data}"}
            for pid, label, accent, data, _ in prepared
        ],
        ensure_ascii=False,
    )

    html = f"""<!doctype html>
<html lang="ja">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>yu-emote — E-mote 実機サンプル</title>
<style>
  :root {{
    color-scheme: dark;
    --bg: #0e1014;
    --panel: #171a21;
    --line: #2a2f3a;
    --ink: #e7e9ee;
    --muted: #8f96a3;
    --accent: #8aa4c8;
  }}
  * {{ box-sizing: border-box; }}
  html, body {{ margin: 0; min-height: 100%; background: var(--bg); color: var(--ink); }}
  body {{
    font: 14px/1.5 "Segoe UI", "Yu Gothic UI", "Hiragino Sans", system-ui, sans-serif;
    background:
      radial-gradient(900px 600px at 72% 8%, #8aa4c822, transparent 70%),
      radial-gradient(700px 500px at 20% 90%, #2a355044, transparent 70%),
      var(--bg);
  }}
  .wrap {{ max-width: 1180px; margin: 0 auto; padding: 34px 22px 56px; }}
  header {{ display: flex; align-items: baseline; justify-content: space-between; gap: 16px; flex-wrap: wrap; }}
  h1 {{ font-size: 21px; letter-spacing: .02em; margin: 0; font-weight: 650; }}
  h1 span {{ color: var(--accent); }}
  .sub {{ color: var(--muted); font-size: 12.5px; }}
  .layout {{ display: grid; grid-template-columns: minmax(0, 1fr) 300px; gap: 22px; margin-top: 22px; }}
  @media (max-width: 860px) {{ .layout {{ grid-template-columns: 1fr; }} }}
  .stage {{
    position: relative; height: min(70vh, 720px); min-height: 520px;
    border: 1px solid var(--line); border-radius: 18px;
    overflow: hidden; display: flex; align-items: flex-end; justify-content: center;
    background:
      linear-gradient(180deg, #1b2130 0%, #12151c 58%, #0d1015 100%);
  }}
  .stage::before {{
    content: ""; position: absolute; inset: 0;
    background:
      radial-gradient(ellipse at 50% 18%, #ffffff14, transparent 55%),
      repeating-linear-gradient(90deg, #ffffff05 0 1px, transparent 1px 44px),
      repeating-linear-gradient(0deg, #ffffff05 0 1px, transparent 1px 44px);
    pointer-events: none;
  }}
  .stage::after {{
    content: ""; position: absolute; left: 8%; right: 8%; bottom: 26px; height: 40px;
    background: radial-gradient(ellipse, #00000088, transparent 70%);
    filter: blur(6px); pointer-events: none;
  }}
  #view {{
    position: relative; z-index: 1; max-height: calc(100% - 8px); max-width: 94%; object-fit: contain;
    filter: drop-shadow(0 18px 26px #00000066) drop-shadow(0 2px 3px #00000088);
    transition: opacity .16s ease, transform .2s ease;
  }}
  #view.swap {{ opacity: .15; transform: translateY(3px) scale(.995); }}
  .panel {{ border: 1px solid var(--line); border-radius: 16px; background: var(--panel); padding: 16px; }}
  .panel h2 {{ margin: 0 0 10px; font-size: 12px; letter-spacing: .14em; text-transform: uppercase; color: var(--muted); font-weight: 650; }}
  .chips {{ display: flex; flex-wrap: wrap; gap: 8px; }}
  button {{
    font: inherit; color: var(--ink); background: #202530; border: 1px solid #303745;
    padding: 8px 12px; border-radius: 10px; cursor: pointer; transition: .15s ease;
  }}
  button:hover {{ transform: translateY(-1px); border-color: #4a5366; background: #262c39; }}
  button.active {{ background: color-mix(in srgb, var(--pill) 26%, #202530); border-color: var(--pill); box-shadow: 0 0 0 1px var(--pill) inset; }}
  .readout {{ margin-top: 15px; padding-top: 14px; border-top: 1px solid var(--line); }}
  .readout .name {{ font-size: 20px; font-weight: 650; }}
  .readout .vars {{ color: var(--muted); font-size: 12.5px; min-height: 19px; word-break: break-all; }}
  .stats {{ margin-top: 14px; display: grid; grid-template-columns: 1fr 1fr; gap: 7px 12px; font-size: 12px; color: var(--muted); }}
  .stats b {{ color: var(--ink); font-weight: 600; font-variant-numeric: tabular-nums; }}
  .note {{ margin-top: 16px; color: var(--muted); font-size: 12px; line-height: 1.65; }}
  .note code {{ color: #c7d0dd; background: #202530; padding: 1px 5px; border-radius: 5px; }}
  footer {{ margin-top: 24px; color: var(--muted); font-size: 12px; display: flex; justify-content: space-between; gap: 12px; flex-wrap: wrap; }}
  kbd {{ font: 11px ui-monospace, monospace; background: #202530; border: 1px solid #333b49; border-bottom-width: 2px; border-radius: 5px; padding: 1px 5px; }}
</style>
</head>
<body>
<div class="wrap">
  <header>
    <h1>yu-emote <span>/ E-mote 実機サンプル</span></h1>
    <div class="sub">all_parts · 全体構造 · 800×1080 · CPU rasterizer</div>
  </header>

  <div class="layout">
    <div class="stage"><img id="view" alt="E-mote render"></div>
    <aside>
      <div class="panel">
        <h2>Expressions</h2>
        <div class="chips" id="chips"></div>
        <div class="readout">
          <div class="name" id="pose-name"></div>
          <div class="vars" id="pose-vars"></div>
        </div>
        <div class="stats">
          <span>draw items</span><b id="st-items">—</b>
          <span>triangles</span><b id="st-tris">—</b>
          <span>atlas pages</span><b id="st-tex">2</b>
          <span>mask sources</span><b id="st-mask">—</b>
        </div>
        <div class="note">
          実ゲーム同梱の <code>emote_test.psb</code> を、このリポジトリの
          クリーンルーム実装（PSB 解析 → VAR/pose → 描画リスト → CPU ラスタ）で描画しています。
          左のボタンまたは <kbd>←</kbd><kbd>→</kbd> で変数を切り替え。
        </div>
      </div>
    </aside>
  </div>

  <footer>
    <span>frames: {len(prepared)} · timeline: 61 · variable-driven pose</span>
    <span>evidence: shipped PSB data · renderer: yu-emote</span>
  </footer>
</div>
<script>
const POSES = {data_json};
const STATS = {stats_json};
const chips = document.getElementById('chips');
const view = document.getElementById('view');
const nameEl = document.getElementById('pose-name');
const varsEl = document.getElementById('pose-vars');
let index = -1;

POSES.forEach((pose, i) => {{
  const button = document.createElement('button');
  button.textContent = pose.label;
  button.style.setProperty('--pill', pose.accent);
  button.addEventListener('click', () => select(i));
  chips.appendChild(button);
}});

function select(i) {{
  if (i === index && view.src) return;
  index = (i + POSES.length) % POSES.length;
  const pose = POSES[index];
  view.classList.add('swap');
  setTimeout(() => {{
    view.src = pose.src;
    view.classList.remove('swap');
  }}, 110);
  [...chips.children].forEach((el, j) => el.classList.toggle('active', j === index));
  nameEl.textContent = pose.label;
  varsEl.textContent = pose.id === 'neutral' ? 'base state' : pose.id.replaceAll('_', ' ');
  if (STATS.stats) {{
    document.getElementById('st-items').textContent = STATS.stats.items;
    document.getElementById('st-tris').textContent = STATS.stats.triangles.toLocaleString();
    document.getElementById('st-mask').textContent = STATS.stats.mask_sources;
  }}
}}

window.addEventListener('keydown', (event) => {{
  if (event.key === 'ArrowRight') select(index + 1);
  if (event.key === 'ArrowLeft') select(index - 1);
}});

view.src = POSES[0].src;
select(0);
</script>
</body>
</html>
"""

    OUT.mkdir(parents=True, exist_ok=True)
    page = OUT / "index.html"
    page.write_text(html, encoding="utf-8")
    print(json.dumps({"page": str(page), "frames": len(prepared), "size": page.stat().st_size}, ensure_ascii=False))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
