#!/usr/bin/env python3
"""Fit RapidRAW's lighting sliders (Blacks, Shadows, Highlights, Whites and
Contrast) to Lightroom's strength, measured by tools/adobe_lighting.py, and
write the engine's tables (src-tauri/src/color_engine/tone_zones_table.rs).

What is fitted is only how far each tone moves: Lightroom's change in L*
for a tone at a given lightness, at slider +-50 and +-100. How a move is made
stays RapidRAW's: one gain on all three channels for the zones (hues stay
put, Resolve's colour and texture finishes ride along) and a per-channel
curve in DaVinci Intermediate for Contrast, as Resolve applies it.

Each table is held as targets for greys: where a grey of a given lightness
should land. `seed` takes them straight from Lightroom; after rendering and
measuring RapidRAW with those tables, `correct` moves each target by what is
still missing, and the tables are rewritten. A target never crosses another,
so no slider can reverse tones.

    uv run --with numpy --with scipy tools/fit_lighting.py seed MEASURE.json
    uv run --with numpy --with scipy tools/fit_lighting.py correct MEASURE.json [DAMPING]

The targets are kept beside the table (tone_zones_targets.json), so each
correction builds on the last.
"""
import json
import os
import sys

import numpy as np

HERE = os.path.dirname(os.path.abspath(__file__))
TABLE = os.path.join(HERE, "..", "src-tauri/src/color_engine/tone_zones_table.rs")
TARGETS = os.path.join(HERE, "tone_zones_targets.json")
SUP = os.path.expanduser("~/Library/Application Support/io.github.CyberTimon.RapidRAW")
KNOTS = 65
MIN_SLOPE = 0.1
# Lightroom's curves are smooth; a table fitted knot by knot picks up the
# measurement's noise, and a kink in a tone curve shows as a band, or (on the
# regional key) as a puddle. Every curve is smoothed, and its slope (output
# key per input key) held between these, before it is written.
SMOOTHING = 100.0
SLOPE_RANGE = (0.3, 2.2)
# Exposure's shaping is steep near white: darkening, Lightroom holds the
# brightest tones back (at -1 stop white moves 3 L*, a tone at L* 90 14).
EXPOSURE_SLOPE_RANGE = (0.1, 8.0)
ZONES = ["blacks", "shadows", "highlights", "whites"]
SLIDERS = ZONES + ["contrast", "exposure"]
STRENGTHS = [("lift_half", 50), ("lift", 100), ("cut_half", -50), ("cut", -100)]
# Exposure's tables are at 1 and 2.5 stops, by the unexposed tone, like the
# zones' (its gain supplies only colour, and what lies beyond 2.5 stops).
EXPOSURE_STRENGTHS = [("lift_half", 1), ("lift", 2.5), ("cut_half", -1), ("cut", -2.5)]


def strengths(slider):
    return EXPOSURE_STRENGTHS if slider == "exposure" else STRENGTHS
# L* points the targets are held at.
GRID = np.linspace(0, 100, 101)


def read_cube(path):
    size, rows = None, []
    for line in open(path):
        t = line.strip()
        if not t or t.startswith("#"):
            continue
        if t.startswith("LUT_3D_SIZE"):
            size = int(t.split()[1])
            continue
        if t[0].isalpha():
            continue
        rows.append([float(x) for x in t.split()[:3]])
    return np.array(rows).reshape(size, size, size, 3)


# The output transform's grey axis: tonal key (Intermediate) -> display value.
_odt = read_cube(os.path.join(SUP, "output-transform.cube"))
_n = _odt.shape[0]
_diag = np.array([_odt[i, i, i].mean() for i in range(_n)])
KEYS = np.linspace(0, 1, 8193)
_display = np.interp(KEYS, np.linspace(0, 1, _n), _diag)


def encode_di(y):
    y = np.maximum(y, 0.0)
    return np.where(y <= 0.00262409, y * 10.44426855, (np.log2(y + 0.0075) + 7.0) * 0.07329248)


def decode_di(v):
    return np.where(v <= 0.02740668, v / 10.44426855, np.exp2(v / 0.07329248 - 7.0) - 0.0075)


def lightness(display):
    y = np.where(display <= 0.04045, display / 12.92, ((display + 0.055) / 1.055) ** 2.4)
    f = np.where(y > (6 / 29) ** 3, np.cbrt(np.clip(y, 0, None)), y / (3 * (6 / 29) ** 2) + 4 / 29)
    return 116 * f - 16


L_OF_KEY = lightness(_display)
TOP = float(L_OF_KEY.max())


def key_of(L):
    """The tonal key whose grey shows at lightness L (the grey axis is monotone)."""
    L = np.clip(L, 0, TOP)
    return np.interp(L, np.maximum.accumulate(L_OF_KEY), KEYS)


def pooled(result, engine, control, value):
    rows = []
    for data in result[engine].values():
        m = data.get(control, {}).get(f"{value:g}")
        if m:
            rows.append([np.nan if x is None else x for x in m["pixel"]["median"]])
    if not rows:
        return None
    curve = np.nanmedian(np.array(rows), axis=0)
    bins = np.array(result["bins"])
    ok = ~np.isnan(curve)
    return np.interp(GRID, bins[ok], curve[ok])


# ---------------------------------------------------------------------------
# Sliders that adapt to the photo (see PhotoTones in plan.rs)
# ---------------------------------------------------------------------------
# Highlights reads its table shifted by (centre - the photo's median key);
# Whites' lift is scaled by clamp(a + b * the photo's brightest key). Their
# tables are fitted in those terms: each photo's change, as offsets on the
# tonal key, lined up (Highlights) or divided by its strength (Whites) before
# pooling.
KN = np.linspace(0, 1, KNOTS)
ADAPTIVE = {("highlights", n) for n, _ in STRENGTHS} | {("whites", "lift"), ("whites", "lift_half")}


def L_of_key(k):
    return np.interp(k, KEYS, L_OF_KEY)


def key_offsets(curve, bins):
    """A photo's measured change (L* by unedited lightness) as offsets on the
    tonal key, at the keys its tones sit at."""
    ok = ~np.isnan(curve)
    L = bins[ok]
    k = key_of(L)
    keep = np.concatenate([[True], np.diff(k) > 1e-6])
    return k[keep], (key_of(np.clip(L + curve[ok], 0, TOP)) - k)[keep]


def photo_curves(result, engine, s, v):
    """Per photo, for the photos both engines render alike: pictures already
    rendered (JPEGs, the chart). A RAW's unedited rendering differs between
    them (Lightroom's default puts its brightest tones near white; ours keeps
    the camera's exposure), and these sliders adapt to the photo as rendered,
    so their rules are learnt where the renderings agree and then applied to
    every photo as RapidRAW renders it."""
    bins = np.array(result["bins"])
    raw = set(result.get("raw", []))
    out = []
    for photo, data in result[engine].items():
        if photo in raw:
            continue
        m = data.get(s, {}).get(f"{v:g}")
        tones = result["ours"].get(photo, {}).get("_tones")
        if m and tones:
            c = np.array([np.nan if x is None else x for x in m["pixel"]["median"]])
            out.append((photo, key_offsets(c, bins), tones))
    return out


def whites_strength(adapt, tones):
    a, b, lo, hi = adapt["whites_lift"]
    return float(np.clip(a + b * tones["brightest"], lo, hi))


def adapted_pool(result, engine, s, name, v, adapt):
    """Pooled key offsets at the knots, in the slider's adapted terms."""
    rows = []
    for _, (k, d), tones in photo_curves(result, engine, s, v):
        if s == "highlights":
            shift = float(np.clip(adapt["highlights_centre"] - tones["median"], -0.3, 0.3))
            # Knot kn is read by a photo's key kn - shift.
            rows.append(np.interp(KN - shift, k, d, left=np.nan, right=np.nan))
        else:
            rows.append(np.interp(KN, k, d, left=np.nan, right=np.nan) / whites_strength(adapt, tones))
    pooled = np.nanmedian(np.array(rows), axis=0)
    ok = ~np.isnan(pooled)
    # Beyond the measured range, hold the nearest measured offset.
    return np.interp(KN, KN[ok], pooled[ok])


def fit_adaptation(result):
    """The centre for Highlights and Whites' strength law, from Lightroom."""
    raw = set(result.get("raw", []))
    tones = [d["_tones"] for p, d in result["ours"].items() if "_tones" in d and p not in raw]
    centre = float(np.median([t["median"] for t in tones]))
    # Whites +100's strength per photo relative to the typical photo, against
    # where its brightest tones sit.
    curves = photo_curves(result, "adobe", "whites", 100)
    typical = np.nanmedian(np.array([np.interp(KN, k, d, left=np.nan, right=np.nan) for _, (k, d), _ in curves]), axis=0)
    xs, amps = [], []
    for _, (k, d), t in curves:
        row = np.interp(KN, k, d, left=np.nan, right=np.nan)
        ok = ~np.isnan(row) & ~np.isnan(typical) & (np.abs(typical) > 1e-4)
        amps.append(float(np.sum(row[ok] * typical[ok]) / np.sum(typical[ok] ** 2)))
        xs.append(t["brightest"])
    b, a = np.polyfit(xs, amps, 1)
    lo, hi = max(0.3, min(amps)), min(3.0, max(amps))
    print(f"Highlights centre {centre:.3f}; Whites lift strength {a:+.3f} {b:+.3f}*brightest, "
          f"clamped {lo:.2f}..{hi:.2f} (per-photo {min(amps):.2f}..{max(amps):.2f}, corr {np.corrcoef(xs, amps)[0, 1]:+.2f})")
    return {"highlights_centre": centre, "whites_lift": [float(a), float(b), float(lo), float(hi)]}


def adapt_seed(path):
    r = json.load(open(path))
    targets = json.load(open(TARGETS))
    targets["_adapt"] = fit_adaptation(r)
    targets["_keyed"] = {}
    for s, name in sorted(ADAPTIVE):
        v = dict(STRENGTHS)[name]
        targets["_keyed"][f"{s}/{name}"] = adapted_pool(r, "adobe", s, name, v, targets["_adapt"]).tolist()
    write(targets)


def smooth_curve(k_out, slope_range=SLOPE_RANGE):
    """The smooth curve nearest the fitted one (a Whittaker smoother: least
    squares plus a penalty on its bending), with its slope held in
    SLOPE_RANGE, kept as close to the fitted curve as that allows."""
    n = len(k_out)
    D = np.diff(np.eye(n), 2, axis=0)
    lam = SMOOTHING
    y = np.linalg.solve(np.eye(n) + lam * D.T @ D, np.asarray(k_out, float))
    x = np.linspace(0, 1, n)
    lo, hi = slope_range
    for _ in range(4):
        slope = np.clip(np.diff(y) / np.diff(x), lo, hi)
        z = np.concatenate([[0.0], np.cumsum(slope * np.diff(x))])
        # Anchored where it best matches the fitted curve.
        y = z + np.mean(np.asarray(k_out) - z)
        y = np.linalg.solve(np.eye(n) + 0.25 * lam * D.T @ D, y)
    slope = np.clip(np.diff(y) / np.diff(x), lo, hi)
    z = np.concatenate([[0.0], np.cumsum(slope * np.diff(x))])
    return z + np.mean(np.asarray(k_out) - z)


def keyed_table(offsets):
    k_out = np.maximum.accumulate(KN + np.array(offsets))
    for i in range(1, KNOTS):
        k_out[i] = max(k_out[i], k_out[i - 1] + MIN_SLOPE * (KN[i] - KN[i - 1]))
    return smooth_curve(k_out) - KN


def table_from_targets(targets, slope_range=SLOPE_RANGE):
    """Offsets per key knot from grey targets (L* in -> L* out)."""
    kn = np.linspace(0, 1, KNOTS)
    base = kn
    L_in = np.interp(kn, KEYS, L_OF_KEY)
    out = np.interp(L_in, GRID, targets)
    k_out = key_of(out)
    # Where the grey axis no longer gets lighter (display white and beyond),
    # hold the offset reached there.
    flat = np.nonzero(L_in >= TOP - 0.05)[0]
    if len(flat) and flat[0] > 0:
        w = flat[0]
        k_out[w:] = base[w:] + (k_out[w - 1] - base[w - 1])
    # Never reverse or merge tones: each knot lands above the one before by
    # at least a tenth of the step, so even every slider at its extreme at
    # once keeps neighbouring tones apart.
    k_out = np.maximum.accumulate(k_out)
    for i in range(1, KNOTS):
        k_out[i] = max(k_out[i], k_out[i - 1] + MIN_SLOPE * (kn[i] - kn[i - 1]))
    return smooth_curve(k_out, slope_range) - base


def write(targets):
    tables = {}
    for s in SLIDERS:
        for name, v in strengths(s):
            if s == "exposure":
                tables[(s, name)] = table_from_targets(np.array(targets[s][name]), EXPOSURE_SLOPE_RANGE)
            else:
                tables[(s, name)] = table_from_targets(np.array(targets[s][name]))
    for slot, offsets in targets.get("_keyed", {}).items():
        s, name = slot.split("/")
        tables[(s, name)] = keyed_table(offsets)
    # A zone's lift only raises tones and its cut only lowers them, whatever
    # noise the fitting picked up near the ends. (Taking the larger, or the
    # smaller, of the table and "no change" keeps tones in order.)
    for z in ZONES:
        for name in ("lift", "lift_half"):
            tables[(z, name)] = np.maximum(tables[(z, name)], 0.0)
        for name in ("cut", "cut_half"):
            tables[(z, name)] = np.minimum(tables[(z, name)], 0.0)
    lines = [
        "//! RapidRAW's lighting tables: the tone zones (Blacks, Shadows, Highlights,",
        "//! Whites) and Contrast, fitted to Lightroom's strength by",
        "//! tools/fit_lighting.py from tools/adobe_lighting.py's measurements; do",
        "//! not edit.",
        "//!",
        "//! Zone offsets on the tonal key (DaVinci Intermediate luminance), 65 knots",
        "//! over 0..1, per zone [blacks, shadows, highlights, whites] at slider",
        "//! +100 (`LIFT`), -100 (`CUT`), +50 (`LIFT_HALF`) and -50 (`CUT_HALF`).",
        "//! The engine applies the zones in that order, each on the previous",
        "//! result. `CONTRAST`: offsets per knot of a channel's Intermediate value,",
        "//! [+50, +100, -50, -100].",
        "",
        f"pub const KNOTS: usize = {KNOTS};",
    ]
    for const, name in (("LIFT", "lift"), ("CUT", "cut"), ("LIFT_HALF", "lift_half"), ("CUT_HALF", "cut_half")):
        lines.append(f"pub const {const}: [[f32; 4]; KNOTS] = [")
        for i in range(KNOTS):
            lines.append("    [" + ", ".join(f"{tables[(z, name)][i]:.6f}" for z in ZONES) + "],")
        lines.append("];")
    lines.append("pub const CONTRAST: [[f32; 4]; KNOTS] = [")
    for i in range(KNOTS):
        lines.append("    [" + ", ".join(f"{tables[('contrast', n)][i]:.6f}" for n in ("lift_half", "lift", "cut_half", "cut")) + "],")
    lines.append("];")
    adapt = targets.get("_adapt", {"highlights_centre": 0.5, "whites_lift": [1.0, 0.0, 1.0, 1.0]})
    lines.append("/// Highlights reads its table relative to the photo's median tonal key,")
    lines.append("/// shifted so a photo whose median sits here reads it as fitted.")
    lines.append(f"pub const HIGHLIGHTS_CENTRE: f32 = {adapt['highlights_centre']:.6f};")
    lines.append("/// Whites' lift strength from the photo's brightest tonal key (99th")
    lines.append("/// percentile): [a, b, lowest, highest] for clamp(a + b * key).")
    lines.append("pub const WHITES_LIFT: [f32; 4] = [" + ", ".join(f"{v:.6f}" for v in adapt["whites_lift"]) + "];")
    lines.append("/// Exposure's shaping beyond its gain, per knot of the exposed tonal key:")
    lines.append("/// [+1, +2.5, -1, -2.5] stops.")
    lines.append("pub const EXPOSURE: [[f32; 4]; KNOTS] = [")
    for i in range(KNOTS):
        lines.append("    [" + ", ".join(f"{tables[('exposure', n)][i]:.6f}" for n in ("lift_half", "lift", "cut_half", "cut")) + "],")
    lines.append("];")
    open(TABLE, "w").write("\n".join(lines) + "\n")
    json.dump(targets, open(TARGETS, "w"), indent=1)
    print("wrote", os.path.normpath(TABLE))


def seed(path, only=None):
    r = json.load(open(path))
    targets = json.load(open(TARGETS)) if only else {}
    for s in SLIDERS:
        if only and s != only:
            continue
        targets[s] = {}
        for name, v in strengths(s):
            d = pooled(r, "adobe", s, v)
            if d is None:
                raise SystemExit(f"no Lightroom measurement for {s} {v}")
            targets[s][name] = np.clip(GRID + d, 0, TOP).tolist()
    targets.pop("_gain", None)
    write(targets)


def correct(path, damping=0.8):
    r = json.load(open(path))
    targets = json.load(open(TARGETS))
    for s in SLIDERS:
        for name, v in strengths(s):
            want = pooled(r, "adobe", s, v)
            got = pooled(r, "ours", s, v)
            if got is None:
                continue
            miss = want - got
            if f"{s}/{name}" in targets.get("_keyed", {}):
                # Adapted sliders: compare in their own terms, per photo.
                want_k = adapted_pool(r, "adobe", s, name, v, targets["_adapt"])
                got_k = adapted_pool(r, "ours", s, name, v, targets["_adapt"])
                targets["_keyed"][f"{s}/{name}"] = (np.array(targets["_keyed"][f"{s}/{name}"]) + damping * (want_k - got_k)).tolist()
                continue
            t = np.array(targets[s][name]) + damping * miss
            # Keep the targets ordered (no reversal) and inside the display range.
            t = np.clip(np.maximum.accumulate(t), 0, TOP)
            targets[s][name] = t.tolist()
            print(f"{s:10} {v:+4}: still missing {np.sqrt(np.mean(miss ** 2)):4.1f} L* rms, worst {np.abs(miss).max():4.1f}")
    write(targets)


if __name__ == "__main__":
    if len(sys.argv) >= 3 and sys.argv[1] == "seed":
        seed(sys.argv[2], sys.argv[3] if len(sys.argv) > 3 else None)
    elif len(sys.argv) >= 3 and sys.argv[1] == "adapt":
        adapt_seed(sys.argv[2])
    elif len(sys.argv) >= 3 and sys.argv[1] == "correct":
        correct(sys.argv[2], float(sys.argv[3]) if len(sys.argv) > 3 else 0.8)
    else:
        print(__doc__)
