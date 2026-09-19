"""Turn downloaded candidates into sky plates: verify with the app's sky
model, cut the pure-sky block, and classify by look."""
import sys, os, json; sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from lib import *
import colorsys

SK = sys.argv[1]
OUT = os.path.join(SK, "plates"); os.makedirs(OUT, exist_ok=True)
cands = json.load(open(os.path.join(SK, "manifest.json")))
seen_files = {}
for c in cands:                      # de-duplicate repeated search hits
    seen_files.setdefault(c["file"], c)
plates = []
for c in seen_files.values():
    path = os.path.join(SK, "raw", c["file"])
    if not os.path.exists(path): continue
    im = load(path, 2000)
    sky, cls, _ = upernet(im, "upernet_swin_large.onnx")
    rows = (sky > 0.85).mean(1)
    # tallest block of nearly-pure sky anchored at the top
    h = 0
    while h < len(rows) and rows[h] > 0.97: h += 1
    frac = h / len(rows)
    if frac < 0.45:
        print(f"skip {c['file'][:44]:<46} sky block {frac:.0%}"); continue
    if frac * im.size[1] < 0.28 * im.size[0]:
        print(f"skip {c['file'][:44]:<46} plate too shallow"); continue
    full = load(path)                                   # full resolution
    cut = int(full.size[1] * frac)
    plate = full.crop((0, 0, full.size[0], cut))
    a = np.asarray(plate.resize((256, max(1, int(256 * plate.size[1] / plate.size[0]))), Image.BILINEAR), np.float32) / 255
    hsv = np.array([colorsys.rgb_to_hsv(*px) for px in a.reshape(-1, 3)[::17]])
    hue, sat, val = hsv[:, 0].mean(), hsv[:, 1].mean(), hsv[:, 2].mean()
    texture = float(np.std(a.mean(-1) - cv2.GaussianBlur(a.mean(-1), (0, 0), 6)))
    warm_frac = float((((hsv[:, 0] < 0.11) | (hsv[:, 0] > 0.91)) & (hsv[:, 1] > 0.22)).mean())
    blue_frac = float(((hsv[:, 0] > 0.5) & (hsv[:, 0] < 0.72) & (hsv[:, 1] > 0.15)).mean())
    if warm_frac > 0.22 and val > 0.22: look = "sunset"
    elif val < 0.30: look = "twilight"
    elif sat < 0.13: look = "overcast"
    elif val < 0.5 and texture > 0.025: look = "stormy"
    elif blue_frac > 0.35 and texture < 0.012: look = "blue-clear"
    elif blue_frac > 0.25: look = "blue-clouds"
    else: look = "mixed"
    extra = {"warm_frac": round(warm_frac, 3), "blue_frac": round(blue_frac, 3)}
    name = f"{look}_{c['file']}"
    plate.save(os.path.join(OUT, name), quality=94)
    plates.append({**c, "plate": name, "look": look, "sky_block": round(frac, 3),
                   "plate_size": plate.size, "hue": round(float(hue), 3),
                   "sat": round(float(sat), 3), "val": round(float(val), 3),
                   "texture": round(texture, 4), **extra})
    print(f"keep {name[:58]:<60} {frac:.0%} {plate.size}", flush=True)
json.dump(plates, open(os.path.join(SK, "plates.json"), "w"), indent=1)
from collections import Counter
print("plates:", len(plates), Counter(p["look"] for p in plates))
