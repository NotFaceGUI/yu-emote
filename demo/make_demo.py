#!/usr/bin/env python3
"""Render real E-mote models and build a self-contained comparison page.

The official game assets are **not** redistributed here. Point `--model` at
local PSB files (Nekopara "pure.psb" files, the YU-RIS test model, ...) and the
script writes:

    target/emote-demo/index.html   single-file page (frames embedded as PNG)
    target/emote-demo/frames/*.png intermediate frames

All pixels come from the clean-room renderer in this repository.
"""

from __future__ import annotations

import argparse
import base64
import io
import json
import shutil
import subprocess
import sys
from pathlib import Path

from PIL import Image

ROOT = Path(__file__).resolve().parents[1]
RENDER = ROOT / "target" / "debug" / ("psb-render.exe" if sys.platform == "win32" else "psb-render")
ANIM = ROOT / "target" / "debug" / ("psb-anim.exe" if sys.platform == "win32" else "psb-anim")
OUT = ROOT / "target" / "emote-demo"

POSES = [
    ("neutral", "通常", [], "#8aa4c8"),
    ("smile", "笑顔", ["face_mouth=30", "face_eyebrow=40", "face_eye_open=25"], "#e8b45a"),
    ("angry", "怒り", ["face_mouth=10", "face_eyebrow=20", "face_eye_open=-5"], "#d46a5a"),
    ("tilt", "首かしげ", ["head_slant=30"], "#b58ad4"),
]


def run_render(psb: Path, key: str | None, extra: list[str], out: Path) -> dict:
    out.parent.mkdir(parents=True, exist_ok=True)
    cmd = [str(RENDER), str(psb), *extra, "--out", str(out)]
    if key:
        cmd += ["--key", key]
    proc = subprocess.run(cmd, capture_output=True, text=True)
    if proc.returncode != 0:
        raise SystemExit(f"render failed for {out.name}:\n{proc.stderr or proc.stdout}")
    try:
        return json.loads(proc.stdout)
    except json.JSONDecodeError:
        return {}


def run_anim(psb: Path, key: str | None, extra: list[str], out_dir: Path) -> dict | None:
    cmd = [str(ANIM), str(psb), *extra, "--out-dir", str(out_dir)]
    if key:
        cmd += ["--key", key]
    proc = subprocess.run(cmd, capture_output=True, text=True)
    if proc.returncode != 0:
        return None
    try:
        return json.loads(proc.stdout)
    except json.JSONDecodeError:
        return None


def encode_video(
    frames: list[Path],
    box: tuple[int, int, int, int],
    size: tuple[int, int],
    out: Path,
    fps: int,
) -> str | None:
    if not frames or shutil.which("ffmpeg") is None:
        return None
    temp = out.parent / f"{out.stem}-frames"
    if temp.exists():
        shutil.rmtree(temp)
    temp.mkdir(parents=True)
    for index, frame in enumerate(frames):
        image = Image.open(frame).convert("RGBA").crop(box).resize(size, Image.LANCZOS)
        image.save(temp / f"frame-{index:04}.png")
    cmd = [
        "ffmpeg", "-y", "-loglevel", "error",
        "-framerate", str(fps),
        "-i", str(temp / "frame-%04d.png"),
        "-c:v", "libvpx-vp9",
        "-pix_fmt", "yuva420p",
        "-b:v", "0",
        "-crf", "34",
        "-auto-alt-ref", "0",
        "-an",
        str(out),
    ]
    proc = subprocess.run(cmd, capture_output=True, text=True)
    shutil.rmtree(temp, ignore_errors=True)
    if proc.returncode != 0:
        print(proc.stderr, file=sys.stderr)
        return None
    return base64.b64encode(out.read_bytes()).decode("ascii")


def trim_union(images: list[Image.Image], pad: int = 26) -> tuple[int, int, int, int]:
    boxes = [im.getchannel("A").getbbox() for im in images]
    boxes = [box for box in boxes if box]
    if not boxes:
        raise SystemExit("all rendered frames are empty")
    return (
        max(0, min(box[0] for box in boxes) - pad),
        max(0, min(box[1] for box in boxes) - pad),
        max(box[2] for box in boxes) + pad,
        max(box[3] for box in boxes) + pad,
    )


def encode_webp(im: Image.Image) -> str:
    # WebP keeps the alpha channel and is ~5x smaller than the source PNG,
    # which matters because every frame is embedded in one HTML file.
    buf = io.BytesIO()
    im.save(buf, "WEBP", quality=90, method=6)
    return base64.b64encode(buf.getvalue()).decode("ascii")


def parse_model(spec: str) -> tuple[str, Path]:
    if "=" not in spec:
        raise SystemExit(f"--model needs LABEL=PATH, got {spec!r}")
    label, path = spec.split("=", 1)
    path = Path(path)
    if not path.exists():
        raise SystemExit(f"model does not exist: {path}")
    return label, path


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--model", action="append", required=True, help="LABEL=PATH to a PSB")
    parser.add_argument("--key", default=None, help="per-game PSB key for encrypted models")
    parser.add_argument("--max-height", type=int, default=860)
    parser.add_argument("--force", action="store_true", help="re-render cached clips")
    args = parser.parse_args()

    if not RENDER.exists():
        raise SystemExit(f"{RENDER} is missing; run `cargo build --bins` first")

    models = []
    for spec in args.model:
        label, path = parse_model(spec)
        stem = path.stem

        # One camera per model: derive it from the neutral pose, then render
        # every expression and animation frame with the same scale/offset so
        # tilting or moving never changes the apparent size.
        neutral_raw = OUT / "frames" / f"{stem}-neutral-fit.png"
        base_stats = run_render(path, args.key, ["--time", "0"], neutral_raw)
        view = base_stats.get("view") or {}
        if view:
            fixed = ["--view", f"{view['scale']},{view['offset'][0]},{view['offset'][1]}"]
        else:
            fixed = []

        rendered = []
        for pose_id, pose_label, variables, accent in POSES:
            raw_path = OUT / "frames" / f"{stem}-{pose_id}.png"
            extra = ["--time", "0", *fixed]
            for spec_value in variables:
                extra += ["--var", spec_value]
            stats = run_render(path, args.key, extra, raw_path)
            rendered.append(
                (pose_id, pose_label, accent, Image.open(raw_path).convert("RGBA"), stats)
            )

        # Full idle loop as a VP9/WebM clip. The native timeline is 421 ticks
        # at 60 Hz (7.0167 s); 15 fps keeps the preview smooth while the
        # renderer advances four engine ticks per output frame.
        anim_dir = OUT / "frames" / f"{stem}-anim"
        webm_path = OUT / "frames" / f"{stem}-idle.webm"
        expected_frames = 210
        anim_frames = sorted(anim_dir.glob("frame-*.png")) if anim_dir.exists() else []
        if args.force or len(anim_frames) < expected_frames:
            anim_info = run_anim(
                path,
                args.key,
                [
                    "--timeline",
                    "待機ループ00",
                    "--fps",
                    "30",
                    "--seconds",
                    "7.0",
                    *fixed,
                ],
                anim_dir,
            )
            anim_frames = sorted(anim_dir.glob("frame-*.png")) if anim_info else []
        else:
            anim_frames = anim_frames[:expected_frames]

        # A short one-shot gesture (挨拶) proves timeline transitions beyond
        # the subtle idle loop. It is cached separately from the idle clip.
        gesture_dir = OUT / "frames" / f"{stem}-gesture"
        gesture_webm = OUT / "frames" / f"{stem}-gesture.webm"
        gesture_count = 45
        gesture_frames = sorted(gesture_dir.glob("frame-*.png")) if gesture_dir.exists() else []
        if args.force or len(gesture_frames) < gesture_count:
            run_anim(
                path,
                args.key,
                [
                    "--timeline",
                    "挨拶",
                    "--fps",
                    "30",
                    "--seconds",
                    "1.5",
                    "--no-loop",
                    *fixed,
                ],
                gesture_dir,
            )
            gesture_frames = sorted(gesture_dir.glob("frame-*.png")) if gesture_dir.exists() else []
        else:
            gesture_frames = gesture_frames[:gesture_count]

        stills = [frame[3] for frame in rendered]
        if anim_frames:
            stills.append(Image.open(anim_frames[0]).convert("RGBA"))
        if gesture_frames:
            stills.append(Image.open(gesture_frames[0]).convert("RGBA"))
            stills.append(Image.open(gesture_frames[-1]).convert("RGBA"))
        box = trim_union(stills)
        height = min(args.max_height, box[3] - box[1])
        width = max(1, round((box[2] - box[0]) * height / (box[3] - box[1])))

        def encode(image: Image.Image) -> str:
            return encode_webp(image.crop(box).resize((width, height), Image.LANCZOS))

        video = None
        if anim_frames and (args.force or not webm_path.exists()):
            encoded = encode_video(
                anim_frames,
                box,
                (width, height),
                webm_path,
                30,
            )
            if encoded:
                video = f"data:video/webm;base64,{encoded}"
        elif anim_frames and webm_path.exists():
            video = (
                "data:video/webm;base64,"
                + base64.b64encode(webm_path.read_bytes()).decode("ascii")
            )

        gesture = None
        if gesture_frames and (args.force or not gesture_webm.exists()):
            encoded = encode_video(
                gesture_frames,
                box,
                (width, height),
                gesture_webm,
                30,
            )
            if encoded:
                gesture = f"data:video/webm;base64,{encoded}"
        elif gesture_frames and gesture_webm.exists():
            gesture = (
                "data:video/webm;base64,"
                + base64.b64encode(gesture_webm.read_bytes()).decode("ascii")
            )

        models.append(
            {
                "label": label,
                "stem": stem,
                "poses": {
                    pid: f"data:image/webp;base64,{encode(image)}"
                    for pid, _, _, image, _ in rendered
                },
                "video": video,
                "gesture": gesture,
                "poseMeta": [
                    {"id": pid, "label": plabel, "accent": accent}
                    for pid, plabel, accent, _, _ in rendered
                ],
                "stats": next(
                    (stats for pid, _, _, _, stats in rendered if pid == "neutral"), {}
                ),
            }
        )

    data_json = json.dumps(models, ensure_ascii=False)
    html = f"""<!doctype html>
<html lang="ja">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>yu-emote — 実機 E-mote モデル</title>
<style>
  :root {{
    color-scheme: dark;
    --bg: #0e1014; --panel: #171a21; --line: #2a2f3a;
    --ink: #e7e9ee; --muted: #8f96a3; --accent: #8aa4c8;
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
  .wrap {{ max-width: 1240px; margin: 0 auto; padding: 30px 22px 48px; }}
  header {{ display: flex; align-items: baseline; justify-content: space-between; gap: 16px; flex-wrap: wrap; }}
  h1 {{ font-size: 21px; letter-spacing: .02em; margin: 0; font-weight: 650; }}
  h1 span {{ color: var(--accent); }}
  .sub {{ color: var(--muted); font-size: 12.5px; }}
  .layout {{ display: grid; grid-template-columns: minmax(0, 1fr) 320px; gap: 22px; margin-top: 20px; }}
  @media (max-width: 900px) {{ .layout {{ grid-template-columns: 1fr; }} }}
  .stage {{
    position: relative; height: min(72vh, 780px); min-height: 520px;
    border: 1px solid var(--line); border-radius: 18px; overflow: hidden;
    display: flex; align-items: flex-end; justify-content: center;
    background: linear-gradient(180deg, #1b2130 0%, #12151c 58%, #0d1015 100%);
  }}
  .stage::before {{
    content: ""; position: absolute; inset: 0; pointer-events: none;
    background:
      radial-gradient(ellipse at 50% 18%, #ffffff12, transparent 55%),
      repeating-linear-gradient(90deg, #ffffff05 0 1px, transparent 1px 44px),
      repeating-linear-gradient(0deg, #ffffff05 0 1px, transparent 1px 44px);
  }}
  .stage::after {{
    content: ""; position: absolute; left: 10%; right: 10%; bottom: 18px; height: 34px;
    background: radial-gradient(ellipse, #00000088, transparent 70%); filter: blur(7px);
    pointer-events: none;
  }}
  #view, #view-next, #anim {{
    position: absolute; inset: 0; margin: auto; z-index: 1;
    max-height: calc(100% - 6px); max-width: 94%; object-fit: contain;
    filter: drop-shadow(0 18px 26px #00000066) drop-shadow(0 2px 3px #00000088);
    transition: opacity .26s ease;
  }}
  #view-next {{ opacity: 0; }}
  #anim {{ display: none; }}
  .panel {{ border: 1px solid var(--line); border-radius: 16px; background: var(--panel); padding: 16px; }}
  .panel h2 {{ margin: 0 0 9px; font-size: 11.5px; letter-spacing: .14em; text-transform: uppercase; color: var(--muted); font-weight: 650; }}
  .chips {{ display: flex; flex-wrap: wrap; gap: 7px; }}
  .chips + h2 {{ margin-top: 18px; }}
  button {{
    font: inherit; color: var(--ink); background: #202530; border: 1px solid #303745;
    padding: 7px 11px; border-radius: 10px; cursor: pointer; transition: .15s ease;
  }}
  button:hover {{ transform: translateY(-1px); border-color: #4a5366; background: #262c39; }}
  button.active {{ background: color-mix(in srgb, var(--pill) 26%, #202530); border-color: var(--pill); box-shadow: 0 0 0 1px var(--pill) inset; }}
  .readout {{ margin-top: 16px; padding-top: 14px; border-top: 1px solid var(--line); }}
  .readout .name {{ font-size: 20px; font-weight: 650; }}
  .readout .vars {{ color: var(--muted); font-size: 12.5px; min-height: 19px; }}
  .stats {{ margin-top: 13px; display: grid; grid-template-columns: 1fr 1fr; gap: 6px 12px; font-size: 12px; color: var(--muted); }}
  .stats b {{ color: var(--ink); font-weight: 600; font-variant-numeric: tabular-nums; }}
  .note {{ margin-top: 15px; color: var(--muted); font-size: 12px; line-height: 1.65; }}
  .note code {{ color: #c7d0dd; background: #202530; padding: 1px 5px; border-radius: 5px; }}
  footer {{ margin-top: 22px; color: var(--muted); font-size: 12px; display: flex; justify-content: space-between; gap: 12px; flex-wrap: wrap; }}
  kbd {{ font: 11px ui-monospace, monospace; background: #202530; border: 1px solid #333b49; border-bottom-width: 2px; border-radius: 5px; padding: 1px 5px; }}
</style>
</head>
<body>
<div class="wrap">
  <header>
    <h1>yu-emote <span>/ 実機 E-mote モデル</span></h1>
    <div class="sub">all_parts · timeline structure · CPU rasterizer</div>
  </header>

  <div class="layout">
    <div class="stage">
      <img id="view" alt="E-mote render">
      <img id="view-next" alt="">
      <video id="anim" muted loop playsinline autoplay preload="auto"></video>
    </div>
    <aside>
      <div class="panel">
        <h2>Characters</h2>
        <div class="chips" id="models"></div>
        <h2>Expressions</h2>
        <div class="chips" id="poses"></div>
        <div class="readout">
          <div class="name" id="pose-name"></div>
          <div class="vars" id="pose-vars"></div>
        </div>
        <div class="stats">
          <span>draw items</span><b id="st-items">—</b>
          <span>triangles</span><b id="st-tris">—</b>
          <span>mask sources</span><b id="st-mask">—</b>
          <span>canvas</span><b id="st-tex">—</b>
        </div>
        <div class="note">
          実ゲームの <code>.psb</code> を、公開実装（eluna / FreeMote など）を
          参考にしたこのリポジトリの Rust 実装（PSB → VAR/pose → draw list →
          CPU raster）で描画しています。
          <kbd>←</kbd><kbd>→</kbd> 表情、<kbd>↑</kbd><kbd>↓</kbd> キャラクター。
        </div>
      </div>
    </aside>
  </div>

  <footer>
    <span>models: {len(models)} · poses: {len(POSES)} · rendered at 800×1080</span>
    <span>assets stay local; not redistributed</span>
  </footer>
</div>
<script>
const MODELS = {data_json};
const modelsEl = document.getElementById('models');
const posesEl = document.getElementById('poses');
const view = document.getElementById('view');
const viewNext = document.getElementById('view-next');
const animEl = document.getElementById('anim');
let front = view;
const nameEl = document.getElementById('pose-name');
const varsEl = document.getElementById('pose-vars');
let mi = 0, pi = 0, playing = false, timer = null;

MODELS.forEach((model, i) => {{
  const button = document.createElement('button');
  button.textContent = model.label;
  button.style.setProperty('--pill', '#8aa4c8');
  button.addEventListener('click', () => {{
    mi = i;
    if (playing) startLive(); else select(mi, pi);
  }});
  modelsEl.appendChild(button);
}});

MODELS[0].poseMeta.forEach((pose, i) => {{
  const button = document.createElement('button');
  button.textContent = pose.label;
  button.style.setProperty('--pill', pose.accent);
  button.addEventListener('click', () => select(mi, i));
  posesEl.appendChild(button);
}});

const liveButton = document.createElement('button');
liveButton.textContent = '▶ 待機ループ';
liveButton.style.setProperty('--pill', '#64b5a0');
liveButton.addEventListener('click', () => startLive());
posesEl.appendChild(liveButton);

const gestureButton = document.createElement('button');
gestureButton.textContent = '▶ しぐさ（挨拶）';
gestureButton.style.setProperty('--pill', '#d7a35a');
gestureButton.addEventListener('click', () => startClip('gesture'));
posesEl.appendChild(gestureButton);

function updateStats(model) {{
  const stats = model.stats?.stats;
  if (!stats) return;
  document.getElementById('st-items').textContent = stats.items;
  document.getElementById('st-tris').textContent = stats.triangles.toLocaleString();
  document.getElementById('st-mask').textContent = stats.mask_sources;
  document.getElementById('st-tex').textContent = model.stats.width + '×' + model.stats.height;
}}

function setImage(src, label, vars) {{
  view.style.display = 'block';
  viewNext.style.display = 'block';
  animEl.style.display = 'none';
  const incoming = front === view ? viewNext : view;
  const outgoing = front === view ? view : viewNext;
  incoming.onload = () => {{
    incoming.style.opacity = 1;
    outgoing.style.opacity = 0;
    front = incoming;
    incoming.onload = null;
  }};
  incoming.src = src;
  nameEl.textContent = label;
  varsEl.textContent = vars;
}}

function select(modelIndex, poseIndex) {{
  stopLive();
  mi = (modelIndex + MODELS.length) % MODELS.length;
  const model = MODELS[mi];
  pi = (poseIndex + model.poseMeta.length) % model.poseMeta.length;
  const pose = model.poseMeta[pi];
  setImage(model.poses[pose.id], model.label + ' · ' + pose.label,
           pose.id === 'neutral' ? 'base state' : pose.id);
  [...modelsEl.children].forEach((el, i) => el.classList.toggle('active', i === mi));
  [...posesEl.children].forEach((el, i) => el.classList.toggle('active', i === pi));
  liveButton.classList.remove('active');
  updateStats(model);
}}

function startLive() {{
  startClip('idle');
}}

function startClip(kind) {{
  const model = MODELS[mi];
  const source = kind === 'gesture' ? model.gesture : model.video;
  if (!source) {{
    varsEl.textContent = 'このモデルにはクリップがありません';
    return;
  }}
  stopLive();
  playing = true;
  [...modelsEl.children].forEach((el, i) => el.classList.toggle('active', i === mi));
  [...posesEl.children].forEach((el, i) => el.classList.toggle('active', false));
  liveButton.classList.add('active');
  view.style.display = 'none';
  viewNext.style.display = 'none';
  animEl.style.display = 'block';
  animEl.loop = kind === 'idle';
  animEl.src = source;
  animEl.currentTime = 0;
  animEl.play().catch(() => {{
    animEl.addEventListener('canplay', () => animEl.play().catch(() => {{}}), {{ once: true }});
  }});
  nameEl.textContent = model.label + (kind === 'gesture' ? ' · 挨拶' : ' · 待機ループ');
  varsEl.textContent = kind === 'gesture'
    ? 'timeline: 挨拶 · one-shot'
    : 'timeline: 待機ループ00 · 60Hz engine / 30fps preview';
  updateStats(model);
}}

function stopLive() {{
  playing = false;
  animEl.pause();
  animEl.loop = true;
  animEl.style.display = 'none';
  view.style.display = 'block';
  viewNext.style.display = 'block';
  if (timer !== null) {{
    clearInterval(timer);
    timer = null;
  }}
}}

window.addEventListener('keydown', (event) => {{
  if (event.key === 'ArrowRight') select(mi, pi + 1);
  if (event.key === 'ArrowLeft') select(mi, pi - 1);
  if (event.key === 'ArrowDown') {{
    mi = (mi + 1) % MODELS.length;
    if (playing) startLive(); else select(mi, pi);
  }}
  if (event.key === 'ArrowUp') {{
    mi = (mi - 1 + MODELS.length) % MODELS.length;
    if (playing) startLive(); else select(mi, pi);
  }}
  if (event.key === ' ') {{
    event.preventDefault();
    if (playing) select(mi, pi); else startLive();
  }}
}});

select(0, 0);
</script>
</body>
</html>
"""

    OUT.mkdir(parents=True, exist_ok=True)
    page = OUT / "index.html"
    page.write_text(html, encoding="utf-8")
    print(json.dumps({"page": str(page), "models": len(models), "size": page.stat().st_size}, ensure_ascii=False))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
