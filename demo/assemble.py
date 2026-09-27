"""Pairs each recorded scene with its narration and joins them into demo.mp4 with burned-in
subtitles (plus a separate demo.srt)."""
import json
import os
import subprocess
from pathlib import Path

HERE = Path(__file__).resolve().parent
B = HERE / "build"
ORDER = ["title", "markets", "open", "trade", "liquidity", "leaderboard", "docs", "outro"]
# SKIP=open,... leaves scenes out (e.g. while the devnet market has no free slot to open one).
ORDER = [s for s in ORDER if s not in os.environ.get("SKIP", "").split(",")]
TEXT = {s["scene"]: s["text"] for s in json.loads((HERE / "narration.json").read_text())}
DUR = json.loads((B / "audio" / "durations.json").read_text())


def run(cmd):
    subprocess.run(cmd, check=True, stdout=subprocess.DEVNULL, stderr=subprocess.PIPE)


def probe(path):
    out = subprocess.run(["ffprobe", "-v", "error", "-show_entries", "format=duration", "-of", "csv=p=0", str(path)],
                         capture_output=True, text=True, check=True).stdout
    return float(out.strip())


def srt_time(t):
    h, m = int(t // 3600), int(t % 3600 // 60)
    s = t % 60
    return f"{h:02d}:{m:02d}:{int(s):02d},{int(round((s - int(s)) * 1000)):03d}"


parts, cues, t = [], [], 0.0
for name in ORDER:
    raw = B / "raw" / f"{name}.webm"
    lead = float((B / "raw" / f"{name}.start").read_text())
    audio = B / "audio" / f"{name}.wav"
    length = probe(raw) - lead
    out = B / f"part-{name}.mp4"
    # Trim the page load, hold the last frame if narration runs longer, start narration after 0.3 s.
    run(["ffmpeg", "-y", "-ss", f"{lead:.2f}", "-i", str(raw), "-i", str(audio),
         "-filter_complex",
         f"[0:v]fps=30,scale=1280:720,tpad=stop_mode=clone:stop_duration=5[v];[1:a]adelay=300|300,apad[a]",
         "-map", "[v]", "-map", "[a]", "-t", f"{max(length, DUR[name] + 1.0):.2f}",
         "-c:v", "libx264", "-preset", "medium", "-crf", "20", "-pix_fmt", "yuv420p",
         "-c:a", "aac", "-b:a", "160k", "-ar", "48000", "-ac", "2", str(out)])
    d = probe(out)
    # Subtitle cues: split the narration into sentences, timed by length within the narration.
    sentences = [x.strip() + "." for x in TEXT[name].replace("?", ".").split(". ") if x.strip()]
    sentences = [s[:-1] if s.endswith("..") else s for s in sentences]
    total = sum(len(s) for s in sentences)
    c = t + 0.3
    for s in sentences:
        dd = DUR[name] * len(s) / total
        cues.append((c, c + dd, s))
        c += dd
    parts.append(out)
    t += d
    print(f"{name:12s} {d:6.2f}s")

srt = "\n".join(f"{i + 1}\n{srt_time(a)} --> {srt_time(b)}\n{s}\n" for i, (a, b, s) in enumerate(cues))
(B / "demo.srt").write_text(srt)
(B / "list.txt").write_text("".join(f"file '{p}'\n" for p in parts))
run(["ffmpeg", "-y", "-f", "concat", "-safe", "0", "-i", str(B / "list.txt"), "-c", "copy", str(B / "joined.mp4")])
style = "FontName=Inter,FontSize=18,PrimaryColour=&H00FFFFFF,OutlineColour=&H80000000,BorderStyle=3,Outline=1,Shadow=0,MarginV=28"
run(["ffmpeg", "-y", "-i", str(B / "joined.mp4"), "-vf", f"subtitles={B / 'demo.srt'}:force_style='{style}'",
     "-c:v", "libx264", "-preset", "medium", "-crf", "20", "-pix_fmt", "yuv420p", "-c:a", "copy",
     "-movflags", "+faststart", str(HERE.parent / "docs" / "demo.mp4")])
print(f"total {t:.1f}s -> docs/demo.mp4")
