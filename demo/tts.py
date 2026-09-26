"""Narration for the demo video, voiced with the X1 Report podcast's Kokoro setup.

Run with the podcast virtualenv (read-only use of that repo: its model files and its
pronunciation lexicon are imported, nothing there is written):
  ~/x1report/podcast/.venv/bin/python tts.py
"""
import json
import re
import sys
from pathlib import Path

import numpy as np
import soundfile as sf

PODCAST = Path.home() / "x1report" / "podcast"
sys.path.insert(0, str(PODCAST))
from x1podcast.tts import spoken  # noqa: E402  (the podcast's pronunciation lexicon)
from kokoro_onnx import Kokoro  # noqa: E402

VOICE, SPEED = "am_eric", 1.05  # the podcast HOST voice the owner approved
EXTRA = [
    (r"\bPyth\b", "Pith"),
    (r"\bUSDC\b", "U S D C"),
    (r"\bperp\b", "perp"),
]

here = Path(__file__).resolve().parent
out = here / "build" / "audio"
out.mkdir(parents=True, exist_ok=True)
kok = Kokoro(str(PODCAST / "models/kokoro/kokoro-v1.0.onnx"), str(PODCAST / "models/kokoro/voices-v1.0.bin"))
durations = {}
for seg in json.loads((here / "narration.json").read_text()):
    text = seg["text"]
    for pat, rep in EXTRA:
        text = re.sub(pat, rep, text)
    audio, sr = kok.create(spoken(text), voice=VOICE, speed=SPEED, lang="en-us")
    a = np.asarray(audio, dtype=np.float32).squeeze()
    path = out / f"{seg['scene']}.wav"
    sf.write(str(path), a, sr)
    durations[seg["scene"]] = round(len(a) / sr, 2)
    print(f"{seg['scene']:12s} {durations[seg['scene']]:6.2f}s")
(out / "durations.json").write_text(json.dumps(durations, indent=1))
