# Managed LUTs & film simulations

RapidRAW+ scans `~/Documents/RapidRAW Models/luts/<pack>/*.cube` and lists
every cube in Effects → LUT → "Film simulations". Each pack folder should
carry a `SOURCES.md` recording provenance (origin, publisher, version,
input space, license notes) — LUT files are typically third-party
copyrighted material and are NEVER committed to this repository.

## Input spaces

- **display** (default): the classic path — the cube receives the final
  display-referred image.
- **flog2c**: film-simulation path for Fujifilm's official F-Log2C LUTs
  (filenames `FLog2C_to_*`, auto-detected). The engine feeds the cube the
  working image encoded exactly as a Fujifilm body would — linear sRGB →
  F-Gamut C (matrix in `src-tauri/src/flog2c.rs`, derived from the
  primaries in Fujifilm's F-Log2C Data Sheet Ver.1.0) → F-Log2 curve —
  and the cube's output is the finished BT.709 film-simulation image,
  replacing the app's tone mapping at full intensity. This is what makes
  the looks accurate from ANY camera's raws, not just Fujifilm's.

Constants are spec-pinned by unit tests (`flog2c.rs`: 0%→code 95,
18%→400, 90%→570 per the data sheet; matrix rows sum to 1 for D65→D65).

Currently installed pack: `fujifilm-flog2c-v110` (official GFX Eterna
pack v1.10, 65-grid, 12 looks — see its SOURCES.md).
