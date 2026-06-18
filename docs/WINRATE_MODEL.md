# Umamusume Win-Rate Simulator — Model & Open Calibration Problem

Self-contained description of the Monte-Carlo win-rate predictor used in this project,
written for an external reviewer. It covers what the model computes, the exact formulas,
the field-level layer we added, the skill system, and the **open calibration problem**
(a strong front-runner is under-rated) with the evidence we gathered.

The code lives in `uma-race-overlay-app/src/sim.rs` (Rust). Race data (stats, aptitudes,
skills, course) comes from the game via a plugin; the simulator is a self-contained
physics + skill model. **The core physics is a faithful port of `uma-skill-tools`
(`RaceSolver.ts`, the engine behind the "umalator" web tool).**

---

## 1. Goal & output

Given a field of horses (each with 5 stats, running style, distance/surface aptitudes,
motivation, skills) and a course (distance, surface), run N≈3000 virtual races and report,
per horse: **win %**, top-3 %, average finishing place. Win % is "expected probability of
winning this exact field", one number, sums to ~100% over the field.

It is a **prediction**, not a replay of the game's predetermined result.

---

## 2. Race phases & base speed

- `baseSpeed(course) = 20.0 − (distance − 2000) / 1000`  (m/s, same for all horses).
- Distance category: ≤1400 sprint, ≤1800 mile, ≤2400 medium, else long.
- Phases by fraction of distance: phase 0 = opening (0…1/6), phase 1 = middle (1/6…2/3),
  phase 2 = final leg (2/3…end). "Last spurt" is a sub-state inside phase 2 where the horse
  commits to top speed (entry point planned from an HP budget, see §5).
- Tick `DT = 1/15 s`.

Per-style phase coefficients (from the game decompile; not in the DB):

```
speed coef [p0,p1,p2]:   nige 1.000/0.980/0.962   senko 0.978/0.991/0.975
                         sashi 0.938/0.998/0.994   oikomi 0.931/1.000/1.000
accel coef [p0,p1,p2]:   nige 1.000/1.000/0.996   senko 0.985/1.000/0.996
                         sashi 0.975/1.000/1.000   oikomi 0.945/1.000/0.997
```
Note: closers (sashi/oikomi) have a HIGHER last-leg speed coefficient than the leader
(nige). This is correct per the game (closers are built to surge at the end) and matters
for the calibration problem in §8.

---

## 3. Target speed (the speed a horse tries to reach) — ports umalator exactly

```
baseTargetSpeed(phase) = baseSpeed * speedCoef[style][phase]
                       + (phase == 2 ? sqrt(500 * speed) * DistApt * 0.002 : 0)

lastSpurtSpeed = (baseTargetSpeed(2) + 0.01 * baseSpeed) * 1.05
               + sqrt(500 * speed) * DistApt * 0.002
               + pow(450 * guts, 0.597) * 0.0001
```

- The **speed stat only enters the target in the final leg / spurt** (phases 0–1 have no
  speed term). This is faithful to the game: front-runners of a given style run the
  early/mid race at essentially the same speed regardless of their speed stat.
- `DistApt` = distance-aptitude multiplier `[G..S] = .1, .2, .4, .6, .8, .9, 1.0, 1.05`.
  So S vs A is only a 5% multiplier on a small additive term.
- Per-tick "wisdom random" adds noise to the target (re-rolled each of 24 sections):
  `wisVar = baseSpeed * (max − 0.65 + rand()*0.65) / 100`, where
  `max = wiz/5500 * log10(wiz*0.1)`. Magnitude ≈ ±0.05–0.07 m/s.

These formulas were verified line-by-line against `RaceSolver.ts` and match.

---

## 4. Acceleration (how fast a horse reaches its target) — ports umalator

```
accel = 0.0006 * sqrt(500 * power) * accelCoef[style][phase] * GroundApt * DistApt  (+ skill accel)
start dash: while v < 0.85*baseSpeed, accel += 24.0
```
- A horse accelerates while `v < target`; once it reaches `target` it is capped there
  (cruises). When `v > target` it decelerates (phase-dependent −0.8…−1.2 m/s²).
- **Key consequence (and a key real-game fact):** acceleration only matters to REACH top
  speed. The decisive lever in a real race is *how fast you reach max speed* — a strong
  accel (high power, or an accel skill timed at spurt entry) lets you hit top speed first
  and open a gap; a horse without that can't catch up before the line. Once everyone is at
  their (similar) top speed, the gap is roughly fixed.
- Uphill should use `0.0004` instead of `0.0006` (slope data is loaded from course geometry
  but this reduction is NOT yet applied — minor).

---

## 5. HP / stamina

- `maxHP = distance + 0.8 * styleHpCoef * stamina`  (styleHpCoef: nige .95, senko .89,
  sashi 1.0, oikomi .995).
- Drain per tick: `20 * (v − baseSpeed + 12)^2 / 144`; in spurt `× (1 + 200/sqrt(600*guts))`;
  kakari (掛かり) `× 1.6`; soft/heavy ground `× 1.06`.
- When HP hits 0 the horse drops to a crawl: target `= 0.85*baseSpeed + sqrt(200*guts)*0.001`.
- Spurt entry is planned (`plan_spurt`) from the remaining HP budget so the horse spends
  stamina to finish without dying (re-evaluated each section in phase ≥ 2).

Other game rules applied: stat soft-cap (values > 1200 are halved above 1200), motivation
(±2%/level), surface/condition penalties, aptitude multipliers.

---

## 6. Skills

Effects come from the game DB (`master.mdb`). Each skill has up to 2 variants with
DSL conditions (`&`=AND between conditions, `@`=OR between groups) and up to 3 effects.

Modeled ability types:
- `27` target-speed buff, `31` acceleration buff → active effects with a duration; summed
  into the target / accel while active. Debuffs (target_type ≠ 1) apply to all opponents.
- `9` HP recovery, `21` instant current-speed bump, `8`/`13` start/kakari passives.
- `1–5` (raw stat buffs) are skipped (already in the horse's Raw stats).

Conditions are evaluated honestly from sim state each tick: `phase, order, order_rate,
distance_rate, remain_distance, hp_per, corner, slope, straight, is_lastspurt`, etc.
Course geometry (corners/slopes/straights) is real, loaded from `course_data.json`
(uma-skill-tools data, keyed by course_id). Positional conditions (`is_overtake`,
`change_order_onetime`, `blocked_*`, `bashin_diff_*`, `near_count`) are computed from the
field state (see §7).

Activation gate: most skills roll against a wisdom-based chance once per race; **unique
skills (skill_category 5, id < 200000) skip the gate** (always fire when their condition
is met). Each horse has its own RNG so one horse's skill count never shifts another's rolls.

---

## 7. Field layer (OUR addition — NOT in umalator)

umalator is a 1-v-1 time-trial: each horse runs a "ghost" independently and finish times
are compared. There is no field, no positions, no traffic. We added a field layer so the
sim handles a full 9–18 horse race. **This layer is where the calibration problem lives.**

- **Position Keep (Pace Down only).** A non-nige horse that is too close behind the
  pacemaker in the first `5/24` of the race slows down (×0.945 phase 1 / ×0.915 phase 2+)
  unless it has an active speed skill or is in kakari. Thresholds (gap to pacemaker, m):
  senko 3.0, sashi 6.5, oikomi 7.5, each × courseFactor `0.0008*(d−1000)+1`. This models
  conserving HP early. (We deliberately do NOT give the leader a speed boost — an earlier
  `PK_UP` boost made front-runners unbeatable and was removed; the front advantage is
  already in the phase coefficients.)
- **Traffic / "deny overtake".** In phase ≥ 2, if a horse is within `BLOCK_DIST = 4 m`
  behind the horse ahead AND wants to go faster (`ahead_speed < target`):
  - per-tick pass chance `p = clamp(pass_prob(power) + 1.6 * deficit, 0, 0.95)`,
    where `pass_prob(power) = clamp(0.08 + power/3500, 0.08, 0.45)` and
    `deficit = (target − ahead_speed) / target` (a RELATIVE deficit);
  - if the pass roll fails, speed is capped to `ahead_speed + (target − ahead_speed)*BLOCK_LEAK`
    with `BLOCK_LEAK = 0.1` (realises only 10% of the advantage that tick).
  - The leader (gap = ∞) is never blocked.
- **Lone-leader nige bonus:** an `order == 1` nige in `frac > 5/24` gets target `× 1.012`.
- **Position-change tracking:** `is_overtake` (passed someone this tick),
  `change_order_onetime` (sign of this-tick order change: **< 0 means overtook**, > 0 got
  passed — matches the game's convention), continue-timers for `blocked_front` etc.

Debug env toggles (read once per process): `UMA_SIM_NO_PK`, `UMA_SIM_NO_WISVAR`,
`UMA_SIM_NO_BLOCK` disable Position Keep / wit-random / traffic respectively.

---

## 8. THE OPEN PROBLEM — a strong front-runner may be under-rated

**Symptom (to be confirmed per real race, not assumed).** A clearly stronger horse of a
given running style (especially a strong front-runner / nige) can be under-rated by the
simulator: its win share leaks to **other, weaker horses of the same style** instead of
separating cleanly by stats/skills. The model can fail to separate same-style runners by
their stats. This is the hypothesis under investigation — every claim below must be
re-grounded against an archived real race, not against any remembered case.

**How to diagnose on a real race (debug toggles, per-race):**

| run | what it isolates |
|-----|------------------|
| baseline | full model |
| `UMA_SIM_NO_PK` | Position-Keep contribution |
| `UMA_SIM_NO_WISVAR` | wit-random noise contribution |
| `UMA_SIM_NO_BLOCK` | traffic / "deny overtake" contribution |

Compare each toggle's win% against the real finishing order to see which layer moves the
result. Record the actual numbers from the archived race — do not carry over numbers from
earlier sessions.

1. **The base physics is healthy.** A strong nige with NO skills beats a no-skill field 1-v-1
   at ~99.5 % (`scenario_nige_dominance_no_skills`); formulas match umalator. Confirmed on the
   real corpus too: with skills stripped, the sim's winner pick equals a naive stat-sort.
2. **CONFIRMED CULPRIT (corpus, 22 real races) — the SKILL layer, NOT traffic/block.** Measured
   with `analyze_archive_corpus`: sim baseline top-1 hit 13.6 % (vs naive stat-sort 27.3 %; in
   tight 9-horse PvP only 5.3 %, worse than the ~11 % random rate). Stripping all skills
   (`CORPUS_NO_SKILLS=1`) DOUBLES accuracy: top-1 27.3 %, PvP 5.3 %→21.1 %. The speed/accel
   skill effects (`ability_type∈{9,21,27,31}`, sim.rs:1189-1201) inject enough wrong signal to
   bury the (correct) stat ordering. Green stat-skills are already filtered out, so this is NOT
   double-counting — it is magnitudes / activation conditions / GameData coverage of the active
   effects. **Open sub-question (H2): which of those three.**
3. **Block is REFUTED as the culprit.** `UMA_SIM_NO_BLOCK` on the corpus does not improve top-1
   (13.6 %) and worsens Brier/place-MAE — the traffic layer helps on average. The earlier
   "block is the villain" conclusion was an artifact of a single race and has been discarded.

**Real-game mechanics the model must respect (from the domain expert):**
- Acceleration matters because it gets you to max speed *fast*; max speed in the last stage
  decides a lot. A horse without an accel burst cannot catch a strong front-runner.
- An accel skill firing exactly at spurt entry is very strong (you reach top speed almost
  instantly while others are still accelerating → durable gap).
- Among near-identical front-runners, randomness over who leads is realistic. But weaker,
  skill-less bots are NOT identical and should not be competitive.
- A leader-type unique (e.g. Seiun Sky's) requires `order == 1`, so only the actual leader
  gets it — reinforcing that the true leader pulls away.

---

## 9. Fixes made / reverted, and the calibration direction

**Calibration direction: only from real archived races.** Speculative cases and remembered
percentages have been removed. Every tuning decision must be backed by a "sim vs actual"
comparison on an archived race, scored with a fixed error metric — not adjusted by feel.

- **FIXED — dead overtake skills.** Skills with condition `change_order_onetime < 0`
  (e.g. "Ramp Up", id 200462: +0.15 target speed in phase 1 on overtake) never fired,
  because we stored `change_order` as a 0/1 flag (never < 0). The game value is the SIGN of
  the order change (overtake = order number decreased = negative). Now `change_order_onetime`
  carries that sign and these skills fire correctly.
- **REVERTED — strength-aware overtake attempt.** Replacing the pass chance with
  `pass_prob + 0.7*(target − ahead) + accel-skill bonus` over-empowered closers (whose high
  target comes from the style's spurt coefficient, not from stats), breaking a regression
  test (`accel_skill_in_spurt_helps`). Lesson: the pass chance must reward a real STAT/POWER
  advantage over the blocker, not raw target-speed difference. **Candidate fix:** make the
  pass chance scale with the passer's **power advantage over the horse directly ahead**
  (precompute `ahead_power` like `ahead_speed`). Not yet implemented.

**Open question for the reviewer:** how to let a genuinely stronger horse pass a weaker one
reliably, while still throttling closers (whose speed edge comes from the style coefficient,
not from being stronger), and keeping near-identical front-runners appropriately random?

---

## 10. Tools

- **Analyzer** (reads a real race dump, runs the sim, prints per-horse win%/avgPlace vs the
  actual result): `RACE_JSON=<file> RACE_COURSE=10906 cargo test --release analyze_race_json -- --ignored --nocapture`.
- Real races are archived to disk (full per-frame timeline: speed/distance/HP/lane/block per
  horse) for offline analysis, so calibration can be driven by real data rather than guesses.
- Reference engine: `github.com/alpha123/uma-skill-tools` (`RaceSolver.ts`, `RaceParameters.ts`).
  Our `course_data.json` comes from there (GPL; dev-only, not shipped).
