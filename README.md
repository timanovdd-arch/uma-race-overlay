# Uma Race Overlay

A live overlay for **Umamusume Pretty Derby** (Steam) that runs on top of the
[Hachimi](https://hachimi.leadrdrk.com/) mod. During a PvP race it shows a table
over the game with each horse's **HP (stamina)**, **speed**, **acceleration**,
and an optional experimental **win-rate prediction** (Monte Carlo simulation).

> ⚠️ Unofficial fan-made tool. Not affiliated with or endorsed by Cygames.
> It only reads the game's data locally to draw an overlay. Use at your own risk.

## Features

- Live HP / speed / acceleration table for every horse, including other players' in PvP.
- "Mine vs rivals" filter (your horses highlighted), draggable window (any monitor).
- **%win rate** — experimental prediction simulating ~500 virtual races using the
  game's own formulas (stats, aptitudes, motivation, HP/spurt, skills from `master.mdb`).
  Off by default — it's a work in progress and not exact.

## Requirements

- Steam version of Umamusume Pretty Derby.
- The [Hachimi](https://hachimi.leadrdrk.com/) mod installed
  (also works with [Hachimi Edge](https://hachimi.noccu.art/)).

## Install (for users)

1. Download the latest `UmaRaceOverlay.zip` from the
   [**Releases**](../../releases) page and unzip it.
2. Follow `README.txt` inside (copy the `.dll` into the game folder, add it to
   Hachimi's `load_libraries`, run the `.exe`).

Hotkeys: **F8** hide · **F9** acceleration · **F6** rivals · **F10** %win rate · **F7** mouse mode · **F11** race map (replay window).

## Build (for developers)

Requires Rust (stable, MSVC toolchain) and CMake (for the GLFW dependency).

```sh
cargo build --release --manifest-path uma-race-overlay/Cargo.toml      # plugin (.dll)
cargo build --release --manifest-path uma-race-overlay-app/Cargo.toml  # overlay app (.exe)
cargo test  --release --manifest-path uma-race-overlay-app/Cargo.toml
```

## Architecture (short)

Two processes — rendering inside the game crashes it (conflicts with Hachimi's GUI
over the D3D swapchain), so graphics and the heavy simulation run separately:

- **`uma-race-overlay/`** — plugin (`.dll`), loaded by Hachimi via `load_libraries`.
  Hooks il2cpp, reads `HorseData`, writes a JSON snapshot to `%TEMP%`.
- **`uma-race-overlay-app/`** — standalone overlay window (egui/GLFW). Reads the
  JSON, draws the table, runs the win-rate Monte Carlo (reads the game's `master.mdb`).

Full documentation in [`docs/`](docs/) — start with `МАСТЕР-ДОКУМЕНТ.md` (RU):
architecture, data sources, the complete win-rate logic, build notes, and the
calibration TODO.

## Support

If this is useful, you can support development here:
**https://dalink.to/everlastingosu** — thank you! — *SupperMommy*

## License

MIT — see [LICENSE](LICENSE).
