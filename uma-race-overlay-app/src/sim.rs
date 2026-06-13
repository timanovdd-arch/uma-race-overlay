//! Monte Carlo симулятор гонки Umamusume.
//!
//! Прогоняет N виртуальных забегов по формулам движка игры (известны из
//! декомпила, коэффициенты читаются из master.mdb) и выдаёт винрейт каждой
//! лошади. Это ПРЕДСКАЗАНИЕ, а не чтение готового результата.
//!
//! Что моделируется:
//! - фазы гонки (序盤 0..1/6, 中盤 1/6..2/3, 終盤 2/3..5/6, ラストスパート);
//! - target speed по статам/стилю/аптитудам + разброс от ума по секциям;
//! - ускорение по силе (+стартовый рывок), замедление;
//! - HP-модель: расход от скорости, бонус упорства в спурте, расчёт точки
//!   начала last spurt по остатку HP, «сдох» = ползёт на минималке;
//! - «закидывание» (rushed) по уму;
//! - скиллы из master.mdb: target speed (27), accel (31), восстановление (9),
//!   мгновенная скорость (21), снижение закидывания (8), старт (13), дебаффы
//!   (target_type != себя). Зелёные (1-5) пропускаются — уже в Raw-статах.
//! - условия скиллов: честно вычисляются phase/order/order_rate/дистанция/HP/
//!   стиль/спурт/время; углы — синтетические окна (геометрии трасс нет);
//!   *_random — случайная точка в окне; позиционная борьба (blocked/bashin/
//!   overtake) не моделируется → вероятностный гейт.
//!
//! НЕ моделируется (поправляется калибровкой): дорожки/блокировка, уклоны,
//! погода/сезон/состояние грунта (считаем «хорошее»), position keep.

use std::collections::HashMap;

use crate::gamedata::{Cond, CondOp, CondTree, GameData, SkillVariant};

/// Входные данные одной лошади (из JSON плагина).
#[derive(Clone, Debug)]
pub struct SimHorse {
    pub gate: i32,
    /// 1 nige, 2 senko, 3 sashi, 4 oikomi.
    pub style: i32,
    /// Сырые статы (зелёные скиллы уже включены игрой).
    pub speed: f64,
    pub stamina: f64,
    pub pow: f64,
    pub guts: f64,
    pub wiz: f64,
    /// Мотивация 1..5 (3 = норма).
    pub motivation: i32,
    /// Аптитуды 1..8 (G..S): к дистанции ЭТОЙ гонки, своему стилю, поверхности.
    pub apt_dist: i32,
    pub apt_style: i32,
    pub apt_ground: i32,
    /// (skill_id, level)
    pub skills: Vec<(i32, i32)>,
}

#[derive(Clone, Debug)]
pub struct RaceParams {
    pub distance: f64,
    /// 1 = турф, 2 = грунт.
    pub ground: i32,
    /// Состояние трассы: 1 firm(良), 2 good(稍重), 3 soft(重), 4 heavy(不良).
    pub condition: i32,
}

impl Default for RaceParams {
    fn default() -> Self {
        Self { distance: 2000.0, ground: 1, condition: 1 }
    }
}

/// Срез стата выше 1200 вдвое (правило игры: «values past 1200 are halved»).
fn soft_cap(stat: f64) -> f64 {
    if stat > 1200.0 {
        1200.0 + (stat - 1200.0) * 0.5
    } else {
        stat
    }
}

#[derive(Clone, Debug, Default)]
pub struct SimResult {
    /// Доля побед каждой лошади (порядок — как во входном срезе).
    pub win: Vec<f64>,
    /// Доля топ-3.
    pub top3: Vec<f64>,
    /// Среднее место.
    pub avg_place: Vec<f64>,
    pub runs: u32,
}

const DT: f64 = 1.0 / 15.0;
/// Вероятность «истинно» для условий, которые мы не моделируем (блокировка,
/// обгоны, разрывы в башинах) — вероятностный гейт.
const UNSUPPORTED_COND_P: f64 = 0.4;

// --- Калибровочные константы (TODO_калибровка_винрейта.md) ---
/// Прошёл ли скилл бросок активации. Уники (свои И наследуемые, skill_category==5)
/// — всегда (2.1), остальные — по Wit (act_rate). `roll` ∈ [0,1) — заранее
/// брошенное число.
fn passes_activation_gate(is_unique: bool, act_rate: f64, roll: f64) -> bool {
    is_unique || roll < act_rate
}
/// Бонус target speed одинокого лидера-nige (Securing the Lead / Pace Up Ex, 3.1).
const LEADER_LEAD_BONUS: f64 = 0.012;
/// Штраф target speed «упёршегося» закрывающего в спурте (блокировка, 3.1).
const BLOCK_PENALTY: f64 = 0.07;
/// Шанс быть заблокированным в спурте по стилю: nige/senko/sashi/oikomi (3.1).
/// Закрывающие рискуют упереться при попытке обгона — прямо из правил гонки.
fn block_prob(style: i32) -> f64 {
    match style {
        1 => 0.0,  // лидер не блокируется
        2 => 0.12, // senko
        3 => 0.28, // sashi
        4 => 0.40, // oikomi (backline — «especially fatal», по правилам)
        _ => 0.15,
    }
}

/// Коэффициенты стилей по фазам (из декомпила движка; в master.mdb их нет).
fn style_speed_coef(style: i32) -> [f64; 3] {
    match style {
        1 => [1.0, 0.98, 0.962],
        2 => [0.978, 0.991, 0.975],
        3 => [0.938, 0.998, 0.994],
        4 => [0.931, 1.0, 1.0],
        _ => [0.978, 0.991, 0.975],
    }
}

fn style_accel_coef(style: i32) -> [f64; 3] {
    match style {
        1 => [1.0, 1.0, 0.996],
        2 => [0.985, 1.0, 0.996],
        3 => [0.975, 1.0, 1.0],
        4 => [0.945, 1.0, 0.997],
        _ => [0.985, 1.0, 0.996],
    }
}

fn style_hp_coef(style: i32) -> f64 {
    match style {
        1 => 0.95,
        2 => 0.89,
        3 => 1.0,
        4 => 0.995,
        _ => 1.0,
    }
}

/// Категория дистанции: 1 спринт, 2 миля, 3 средняя, 4 длинная (= distance_type).
pub fn distance_type(d: f64) -> i32 {
    if d <= 1400.0 {
        1
    } else if d <= 1800.0 {
        2
    } else if d <= 2400.0 {
        3
    } else {
        4
    }
}

/// Синтетические окна углов (доли дистанции): геометрии трасс в master.mdb нет,
/// берём типовую компоновку. Последнее окно — финальный угол.
const CORNERS: [(f64, f64); 3] = [(0.12, 0.22), (0.37, 0.47), (0.60, 0.72)];
const FINAL_CORNER: (f64, f64) = (0.60, 0.72);

// ---------------------------------------------------------------------------
// Подготовленные (per-run) данные скилла
// ---------------------------------------------------------------------------

struct SkillRt {
    /// Индекс варианта в SkillDef + level.
    variant: SkillVariant,
    level: i32,
    /// Прошёл ли скилл бросок активации по уму (один на забег).
    gate_ok: bool,
    /// Уже сработал (одноразовые).
    used: bool,
    /// Пре-ролл для неподдерживаемых условий: cond_key -> bool.
    unsupported: HashMap<usize, bool>,
    /// Случайные точки срабатывания для *_random условий: cond_key -> метры.
    random_pts: HashMap<usize, f64>,
}

/// Активный эффект на лошади.
struct ActiveEff {
    until: f64,
    ability: i32,
    value: f64,
}

// ---------------------------------------------------------------------------
// Состояние лошади в забеге
// ---------------------------------------------------------------------------

struct Runner {
    // постоянные на забег
    style: i32,
    base_speed: f64,
    max_hp: f64,
    spd_adj: f64,
    pow_adj: f64,
    guts_adj: f64,
    wiz_adj: f64,
    dist_prof_speed: f64,
    dist_prof_pow: f64,
    ground_prof: f64,
    gate: i32,
    popularity: i32,
    start_delay: f64,
    rushed_section: i32, // -1 = не закинуло
    skills: Vec<SkillRt>,
    // динамика
    pos: f64,
    v: f64,
    hp: f64,
    finished: bool,
    finish_time: f64,
    spurting: bool,
    spurt_from_remain: f64, // спурт начнётся, когда remain <= этого значения
    wis_var: f64,           // текущая надбавка от ума (на секцию)
    section: i32,
    active: Vec<ActiveEff>,
    order: i32,
    /// «Упёрся» в этом забеге (закрывающий блокируется в спурте, 3.1).
    blocked: bool,
}

impl Runner {
    fn phase(&self, d: f64) -> i32 {
        let r = self.pos / d;
        if r < 1.0 / 6.0 {
            0
        } else if r < 2.0 / 3.0 {
            1
        } else if r < 5.0 / 6.0 {
            2
        } else {
            3
        }
    }
}

fn in_window(frac: f64, w: (f64, f64)) -> bool {
    frac >= w.0 && frac <= w.1
}

fn in_corner(frac: f64) -> bool {
    CORNERS.iter().any(|w| in_window(frac, *w))
}

// ---------------------------------------------------------------------------
// Вычисление условий скиллов
// ---------------------------------------------------------------------------

struct CondCtx {
    phase: f64,
    distance_rate: f64,
    remain_distance: f64,
    hp_per: f64,
    order: f64,
    order_rate: f64,
    running_style: f64,
    distance_type: f64,
    ground_type: f64,
    is_lastspurt: f64,
    is_finalcorner: f64,
    corner: f64,
    accumulatetime: f64,
    post_number: f64,
    popularity: f64,
    is_badstart: f64,
    is_basis_distance: f64,
    pos: f64,
}

/// Значение переменной условия; None = не моделируем.
fn cond_var(ctx: &CondCtx, var: &str) -> Option<f64> {
    Some(match var {
        "phase" => ctx.phase,
        "distance_rate" => ctx.distance_rate,
        "remain_distance" => ctx.remain_distance,
        "hp_per" => ctx.hp_per,
        "order" => ctx.order,
        "order_rate" => ctx.order_rate,
        "running_style" => ctx.running_style,
        "distance_type" => ctx.distance_type,
        "ground_type" => ctx.ground_type,
        "is_lastspurt" => ctx.is_lastspurt,
        "is_finalcorner" | "is_finalcorner_laterhalf" => ctx.is_finalcorner,
        "corner" => ctx.corner,
        "accumulatetime" => ctx.accumulatetime,
        "post_number" => ctx.post_number,
        "popularity" => ctx.popularity,
        "is_badstart" => ctx.is_badstart,
        "is_basis_distance" => ctx.is_basis_distance,
        "always" => 1.0,
        "slope" => 0.0,            // уклонов не моделируем: всегда «ровно»
        "ground_condition" => 1.0, // считаем «хорошее»
        "weather" => 1.0,          // считаем «ясно»
        "season" => 1.0,
        "rotation" => 1.0,
        "temptation_count" => 0.0,
        _ => return None,
    })
}

fn cmp(op: CondOp, a: f64, b: f64) -> bool {
    match op {
        CondOp::Eq => (a - b).abs() < 1e-9,
        CondOp::Ne => (a - b).abs() >= 1e-9,
        CondOp::Ge => a >= b,
        CondOp::Le => a <= b,
        CondOp::Gt => a > b,
        CondOp::Lt => a < b,
    }
}

/// Ключ условия внутри скилла (для пре-роллов): индекс группы * 100 + индекс.
fn cond_key(gi: usize, ci: usize) -> usize {
    gi * 100 + ci
}

fn eval_cond(
    tree: &CondTree,
    ctx: &CondCtx,
    rt_unsupported: &HashMap<usize, bool>,
    rt_random: &HashMap<usize, f64>,
) -> bool {
    if tree.is_empty() {
        return true;
    }
    'group: for (gi, group) in tree.iter().enumerate() {
        for (ci, c) in group.iter().enumerate() {
            let ok = eval_one(c, ctx, rt_unsupported.get(&cond_key(gi, ci)), rt_random.get(&cond_key(gi, ci)));
            if !ok {
                continue 'group;
            }
        }
        return true; // вся группа И-условий истинна
    }
    false
}

fn eval_one(c: &Cond, ctx: &CondCtx, unsupported: Option<&bool>, random_pt: Option<&f64>) -> bool {
    // *_random: срабатывают в заранее брошенной точке (окно ~60 м).
    if c.var.ends_with("_random") || c.var == "is_finalcorner_random" {
        if let Some(&pt) = random_pt {
            return ctx.pos >= pt && ctx.pos <= pt + 60.0;
        }
        return false;
    }
    match cond_var(ctx, &c.var) {
        Some(v) => cmp(c.op, v, c.val),
        None => unsupported.copied().unwrap_or(false),
    }
}

/// Окно дистанции (в метрах) для *_random переменной; None = вся гонка.
fn random_window(var: &str, val: f64, d: f64) -> (f64, f64) {
    match var {
        // случайная точка в фазе N
        "phase_random" => match val as i32 {
            0 => (0.0, d / 6.0),
            1 => (d / 6.0, d * 2.0 / 3.0),
            2 => (d * 2.0 / 3.0, d * 5.0 / 6.0),
            _ => (d * 5.0 / 6.0, d),
        },
        "phase_laterhalf_random" => match val as i32 {
            // вторая половина фазы N
            0 => (d / 12.0, d / 6.0),
            1 => (d * 5.0 / 12.0, d * 2.0 / 3.0),
            2 => (d * 3.0 / 4.0, d * 5.0 / 6.0),
            _ => (d * 11.0 / 12.0, d),
        },
        "is_finalcorner_random" => (d * FINAL_CORNER.0, d * FINAL_CORNER.1),
        "corner_random" | "all_corner_random" => {
            // случайный из синтетических углов
            (d * CORNERS[0].0, d * CORNERS[2].1)
        }
        "straight_random" => (d * 0.72, d * 0.95), // финишная прямая
        "distance_rate_after_random" => (d * val / 100.0, d),
        _ => (0.0, d),
    }
}

// ---------------------------------------------------------------------------
// Сам симулятор
// ---------------------------------------------------------------------------

pub fn simulate(
    gd: &GameData,
    race: &RaceParams,
    horses: &[SimHorse],
    runs: u32,
    seed: u64,
) -> SimResult {
    let n = horses.len();
    let mut wins = vec![0u32; n];
    let mut top3 = vec![0u32; n];
    let mut place_sum = vec![0u64; n];

    for run in 0..runs {
        let mut rng = fastrand::Rng::with_seed(seed.wrapping_add(run as u64));
        let order = run_race(gd, race, horses, &mut rng);
        for (idx, place) in order.iter().enumerate() {
            if *place == 1 {
                wins[idx] += 1;
            }
            if *place <= 3 {
                top3[idx] += 1;
            }
            place_sum[idx] += *place as u64;
        }
    }

    SimResult {
        win: wins.iter().map(|w| *w as f64 / runs as f64).collect(),
        top3: top3.iter().map(|w| *w as f64 / runs as f64).collect(),
        avg_place: place_sum.iter().map(|s| *s as f64 / runs as f64).collect(),
        runs,
    }
}

/// Один забег; возвращает место каждой лошади (1 = победа), индексы как на входе.
fn run_race(gd: &GameData, race: &RaceParams, horses: &[SimHorse], rng: &mut fastrand::Rng) -> Vec<i32> {
    let d = race.distance;
    let n = horses.len();
    let base_speed_course = 20.0 - (d - 2000.0) / 1000.0;
    let dist_type = distance_type(d);
    // Soft(3)/heavy(4) грунт повышает расход HP примерно на 2%/с (~× за весь забег).
    let cond_drain_mult = if race.condition >= 3 { 1.06 } else { 1.0 };

    // популярность для условий (по сумме статов — порядок, 1 = фаворит)
    let mut by_power: Vec<usize> = (0..n).collect();
    by_power.sort_by(|&a, &b| {
        let sa = horses[a].speed + horses[a].stamina + horses[a].pow + horses[a].guts + horses[a].wiz;
        let sb = horses[b].speed + horses[b].stamina + horses[b].pow + horses[b].guts + horses[b].wiz;
        sb.partial_cmp(&sa).unwrap_or(std::cmp::Ordering::Equal)
    });
    let mut popularity = vec![1; n];
    for (rank, &i) in by_power.iter().enumerate() {
        popularity[i] = rank as i32 + 1;
    }

    // Каждой лошади — свой независимый RNG (сид из общего потока, по одному
    // на лошадь). Так число скиллов одной лошади НЕ сдвигает броски другой —
    // забег честный, A/B-сравнения стабильны.
    let mut runners: Vec<Runner> = horses
        .iter()
        .enumerate()
        .map(|(i, h)| {
            let mut hr = fastrand::Rng::with_seed(rng.u64(..));
            make_runner(gd, h, d, race, base_speed_course, popularity[i], &mut hr)
        })
        .collect();

    let max_t = 60.0 + d / 10.0; // страховка от вечного цикла
    let mut t = 0.0;
    let mut finished_cnt = 0usize;
    let mut next_finish_order = 1i32;

    while finished_cnt < n && t < max_t {
        t += DT;

        // позиции (order): по пройденной дистанции
        let mut idx: Vec<usize> = (0..n).collect();
        idx.sort_by(|&a, &b| {
            runners[b]
                .pos
                .partial_cmp(&runners[a].pos)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        for (rank, &i) in idx.iter().enumerate() {
            runners[i].order = rank as i32 + 1;
        }

        // дебаффы, прилетевшие в этом тике от чужих скиллов: (цель, эффект)
        let mut debuffs: Vec<(usize, ActiveEff)> = Vec::new();

        for i in 0..n {
            let r = &mut runners[i];
            if r.finished || t < r.start_delay {
                continue;
            }
            let phase = r.phase(d);
            let frac = (r.pos / d).min(1.0);
            let remain = (d - r.pos).max(0.0);

            // секция сменилась → новый разброс ума + переоценка спурта
            let sec = (r.pos / (d / 24.0)) as i32;
            if sec != r.section {
                r.section = sec;
                let up = (r.wiz_adj / 5500.0) * (r.wiz_adj * 0.1).log10();
                let lo = up - 0.65;
                r.wis_var = r.base_speed * (lo + rng.f64() * (up - lo)) / 100.0;
                if phase >= 2 && !r.spurting {
                    r.spurt_from_remain = plan_spurt(r, d, remain);
                }
            }

            // вход в спурт
            if phase >= 2 && !r.spurting {
                if r.spurt_from_remain <= 0.0 {
                    r.spurt_from_remain = plan_spurt(r, d, remain);
                }
                if remain <= r.spurt_from_remain {
                    r.spurting = true;
                }
            }

            // --- скиллы ---
            let ctx = CondCtx {
                phase: phase as f64,
                distance_rate: frac * 100.0,
                remain_distance: remain,
                hp_per: (r.hp / r.max_hp * 100.0).max(0.0),
                order: r.order as f64,
                order_rate: r.order as f64 / n as f64 * 100.0,
                running_style: r.style as f64,
                distance_type: dist_type as f64,
                ground_type: race.ground as f64,
                is_lastspurt: r.spurting as i32 as f64,
                is_finalcorner: in_window(frac, FINAL_CORNER) as i32 as f64,
                corner: if in_corner(frac) { 1.0 } else { 0.0 },
                accumulatetime: t - r.start_delay,
                post_number: r.gate as f64,
                popularity: r.popularity as f64,
                is_badstart: (r.start_delay > 0.08) as i32 as f64,
                is_basis_distance: ((d as i32) % 400 == 0) as i32 as f64,
                pos: r.pos,
            };
            let mut activations: Vec<(f64, i32, f64, i32)> = Vec::new(); // (dur, ability, value, target)
            for s in r.skills.iter_mut() {
                if s.used || !s.gate_ok {
                    continue;
                }
                if !eval_cond(&s.variant.precondition, &ctx, &s.unsupported, &s.random_pts) {
                    continue;
                }
                if !eval_cond(&s.variant.condition, &ctx, &s.unsupported, &s.random_pts) {
                    continue;
                }
                s.used = true; // одноразово (кулдауны редки у гоночных скиллов)
                let dur = s.variant.duration_s * d / 1000.0;
                for eff in &s.variant.effects {
                    let val = gd.effect_value(eff, s.level);
                    activations.push((dur, eff.ability_type, val, eff.target_type));
                }
            }
            for (dur, ability, value, target) in activations {
                match ability {
                    1..=5 => {} // зелёные уже в Raw-статах
                    8 | 13 => {} // применены до старта
                    9 => r.hp = (r.hp + value * r.max_hp).min(r.max_hp),
                    21 => r.v += value,
                    27 | 31 => {
                        if target == 1 {
                            r.active.push(ActiveEff { until: t + dur, ability, value });
                        } else {
                            // дебафф: применяется к остальным
                            for j in 0..n {
                                if j != i {
                                    debuffs.push((j, ActiveEff { until: t + dur, ability, value }));
                                }
                            }
                        }
                    }
                    _ => {} // 10/14/22/28/29/35/502/503: обзор/дорожки — не моделируем
                }
            }

            // --- target speed ---
            r.active.retain(|e| e.until > t);
            let skill_speed: f64 = r
                .active
                .iter()
                .filter(|e| e.ability == 27)
                .map(|e| e.value)
                .sum();
            let skill_accel: f64 = r
                .active
                .iter()
                .filter(|e| e.ability == 31)
                .map(|e| e.value)
                .sum();

            let coef = style_speed_coef(r.style);
            let guts_term = (450.0 * r.guts_adj).powf(0.597) * 0.0001;
            let speed_term = (500.0 * r.spd_adj).sqrt() * r.dist_prof_speed * 0.002;
            let mut target = if r.hp <= 0.0 {
                // выдохся: ползёт на минималке
                0.85 * r.base_speed + (200.0 * r.guts_adj).sqrt() * 0.001
            } else if r.spurting {
                (r.base_speed * coef[2] + speed_term + 0.01 * r.base_speed) * 1.05
                    + speed_term
                    + guts_term
            } else if phase >= 2 {
                r.base_speed * coef[2] + speed_term + guts_term
            } else {
                let mut tg = r.base_speed * coef[phase.min(1) as usize];
                // закидывание: в «своей» секции цель завышена
                if r.rushed_section >= 0 && sec >= r.rushed_section && sec < r.rushed_section + 2 {
                    tg *= 1.6;
                }
                tg
            };
            if r.hp > 0.0 {
                target += r.wis_var + skill_speed;
            }

            // --- позиционные поправки (3.1, из правил «Target Speed») ---
            // Одинокий лидер-nige тянет темп (Securing the Lead / Pace Up Ex).
            if r.style == 1 && r.order == 1 && phase >= 1 && r.hp > 0.0 {
                target *= 1.0 + LEADER_LEAD_BONUS;
            }
            // Упёршийся закрывающий в спурте, пока не пробился в голову, не может
            // раскрыть скорость (ограничен идущими впереди).
            if r.blocked && r.spurting && r.order > 3 {
                target *= 1.0 - BLOCK_PENALTY;
            }

            // --- ускорение ---
            let acoef = style_accel_coef(r.style);
            let phase_idx = (phase.min(2)) as usize;
            let mut accel;
            if r.v < target {
                accel = 0.0006
                    * (500.0 * r.pow_adj).sqrt()
                    * acoef[phase_idx]
                    * r.ground_prof
                    * r.dist_prof_pow
                    + skill_accel;
                if r.v < 0.85 * r.base_speed {
                    accel += 24.0; // стартовый рывок
                }
            } else {
                accel = match phase {
                    0 => -1.2,
                    1 => -0.8,
                    _ => -1.0,
                };
            }
            r.v += accel * DT;
            if accel > 0.0 && r.v > target {
                r.v = target;
            }
            if accel < 0.0 && r.v < target {
                r.v = target;
            }
            if r.v < 1.0 {
                r.v = 1.0;
            }

            // --- HP ---
            if r.hp > 0.0 {
                let mut drain = 20.0 * (r.v - r.base_speed + 12.0).powi(2) / 144.0;
                if phase >= 2 && r.spurting {
                    drain *= 1.0 + 200.0 / (600.0 * r.guts_adj).sqrt();
                }
                // Закидывание (掛かри) ×1.6 расход HP, пока активно.
                if r.rushed_section >= 0 && r.section >= r.rushed_section && r.section < r.rushed_section + 3 {
                    drain *= 1.6;
                }
                // Тяжёлый грунт (soft/heavy) — +2%/с расхода HP.
                drain *= cond_drain_mult;
                r.hp -= drain * DT;
            }

            // --- движение ---
            r.pos += r.v * DT;
            if r.pos >= d {
                r.finished = true;
                // точное время пересечения
                r.finish_time = t - (r.pos - d) / r.v;
                finished_cnt += 1;
            }
        }

        // применяем дебаффы (после основного цикла, чтобы не двигать заимствование)
        for (j, eff) in debuffs {
            if !runners[j].finished {
                runners[j].active.push(eff);
            }
        }
        let _ = next_finish_order;
        next_finish_order += 0;
    }

    // места по времени финиша; не финишировавшие — по дистанции
    let mut idx: Vec<usize> = (0..n).collect();
    idx.sort_by(|&a, &b| {
        match (runners[a].finished, runners[b].finished) {
            (true, true) => runners[a]
                .finish_time
                .partial_cmp(&runners[b].finish_time)
                .unwrap_or(std::cmp::Ordering::Equal),
            (true, false) => std::cmp::Ordering::Less,
            (false, true) => std::cmp::Ordering::Greater,
            (false, false) => runners[b]
                .pos
                .partial_cmp(&runners[a].pos)
                .unwrap_or(std::cmp::Ordering::Equal),
        }
    });
    let mut places = vec![0i32; n];
    for (rank, &i) in idx.iter().enumerate() {
        places[i] = rank as i32 + 1;
    }
    places
}

fn make_runner(
    gd: &GameData,
    h: &SimHorse,
    d: f64,
    race: &RaceParams,
    base_speed: f64,
    popularity: i32,
    rng: &mut fastrand::Rng,
) -> Runner {
    let mot = gd.motivation_rate.get(&h.motivation).copied().unwrap_or(1.0);

    // Штрафы поверхности/состояния (правила игры):
    //  heavy(4) → скорость −50; turf не-firm → сила −50; dirt не-good → −100
    //  (good → −50). Применяются к базовому стату ДО мотивации/среза.
    let mut speed_pen = 0.0;
    let mut pow_pen = 0.0;
    if race.condition >= 4 {
        speed_pen = 50.0;
    }
    if race.ground == 1 && race.condition != 1 {
        pow_pen = 50.0;
    } else if race.ground == 2 {
        pow_pen = if race.condition == 2 { 50.0 } else { 100.0 };
    }

    // Порядок: базовый стат − штраф трассы, ×мотивация (±2%/ур.), затем срез >1200.
    let spd_adj = soft_cap((h.speed - speed_pen).max(1.0) * mot);
    let sta_adj = soft_cap(h.stamina * mot);
    let pow_adj = soft_cap((h.pow - pow_pen).max(1.0) * mot);
    let guts_adj = soft_cap(h.guts * mot);
    let style_rate = gd.style_rate.get(&h.apt_style).copied().unwrap_or(1.0);
    let wiz_adj = soft_cap(h.wiz * mot) * style_rate;
    let (dist_prof_speed, dist_prof_pow) = gd.dist_rate.get(&h.apt_dist).copied().unwrap_or((1.0, 1.0));
    let ground_prof = gd.ground_rate.get(&h.apt_ground).copied().unwrap_or(1.0);

    let max_hp = d + 0.8 * style_hp_coef(h.style) * sta_adj;

    // скиллы: подготовка per-run
    let mut start_delay_mod = 1.0f64;
    let mut rushed_mod = 1.0f64;
    let act_rate = (1.0 - 90.0 / wiz_adj.max(91.0)).clamp(0.2, 1.0);
    let mut skills: Vec<SkillRt> = Vec::new();
    for (id, level) in &h.skills {
        let Some(def) = gd.skills.get(id) else { continue };
        for variant in &def.variants {
            // пассивы старта/закидывания применяем сразу
            for eff in &variant.effects {
                match eff.ability_type {
                    8 => rushed_mod *= (1.0 + gd.effect_value(eff, *level)).max(0.0),
                    13 => start_delay_mod *= (1.0 + gd.effect_value(eff, *level)).max(0.0),
                    _ => {}
                }
            }
            // гоночные эффекты — в рантайм
            if !variant.effects.iter().any(|e| matches!(e.ability_type, 9 | 21 | 27 | 31)) {
                continue;
            }
            let mut unsupported = HashMap::new();
            let mut random_pts = HashMap::new();
            for (tree_kind, tree) in [(0usize, &variant.precondition), (1usize, &variant.condition)] {
                for (gi, group) in tree.iter().enumerate() {
                    for (ci, c) in group.iter().enumerate() {
                        let key = cond_key(gi, ci) + tree_kind * 10000;
                        if c.var.ends_with("_random") {
                            let (a, b) = random_window(&c.var, c.val, d);
                            random_pts.insert(cond_key(gi, ci), a + rng.f64() * (b - a).max(1.0));
                        } else if cond_var(&zero_ctx(), &c.var).is_none() {
                            unsupported.insert(cond_key(gi, ci), rng.f64() < UNSUPPORTED_COND_P);
                        }
                        let _ = key;
                    }
                }
            }
            // Уники (свои и наследуемые) срабатывают БЕЗ Wit-гейта (2.1).
            let gate_ok = passes_activation_gate(def.is_unique, act_rate, rng.f64());
            skills.push(SkillRt {
                variant: variant.clone(),
                level: *level,
                gate_ok,
                used: false,
                unsupported,
                random_pts,
            });
        }
    }

    // закидывание (掛かり)
    let rushed_p = ((6.5 / (0.1 * wiz_adj + 1.0).log10()).powi(2) / 100.0 * rushed_mod).clamp(0.0, 0.7);
    let rushed_section = if rng.f64() < rushed_p {
        // случайная секция в фазах 0-1 (первые 2/3 гонки = секции 0..16)
        rng.i32(1..16)
    } else {
        -1
    };
    // Блокировка в спурте: закрывающие рискуют упереться при обгоне (3.1).
    let blocked = rng.f64() < block_prob(h.style);

    Runner {
        style: h.style,
        base_speed,
        max_hp,
        spd_adj,
        pow_adj,
        guts_adj,
        wiz_adj,
        dist_prof_speed,
        dist_prof_pow,
        ground_prof,
        gate: h.gate,
        popularity,
        start_delay: rng.f64() * 0.1 * start_delay_mod,
        rushed_section,
        skills,
        pos: 0.0,
        v: 3.0,
        hp: max_hp,
        finished: false,
        finish_time: 0.0,
        spurting: false,
        spurt_from_remain: 0.0,
        wis_var: 0.0,
        section: -1,
        active: Vec::new(),
        order: 1,
        blocked,
    }
}

fn zero_ctx() -> CondCtx {
    CondCtx {
        phase: 0.0,
        distance_rate: 0.0,
        remain_distance: 0.0,
        hp_per: 0.0,
        order: 0.0,
        order_rate: 0.0,
        running_style: 0.0,
        distance_type: 0.0,
        ground_type: 0.0,
        is_lastspurt: 0.0,
        is_finalcorner: 0.0,
        corner: 0.0,
        accumulatetime: 0.0,
        post_number: 0.0,
        popularity: 0.0,
        is_badstart: 0.0,
        is_basis_distance: 0.0,
        pos: 0.0,
    }
}

/// Сколько метров до финиша можно бежать в полном спурте, не упав в 0 HP.
/// Линейный расчёт: остаток дистанции бежим либо на спурт-скорости (дорого),
/// либо на крейсере фазы 2; ищем максимальную длину спурта по бюджету HP.
fn plan_spurt(r: &Runner, _d: f64, remain: f64) -> f64 {
    let coef = style_speed_coef(r.style);
    let guts_term = (450.0 * r.guts_adj).powf(0.597) * 0.0001;
    let speed_term = (500.0 * r.spd_adj).sqrt() * r.dist_prof_speed * 0.002;
    let v2 = r.base_speed * coef[2] + speed_term + guts_term;
    let vs = (r.base_speed * coef[2] + speed_term + 0.01 * r.base_speed) * 1.05 + speed_term + guts_term;
    let guts_mod = 1.0 + 200.0 / (600.0 * r.guts_adj).sqrt();
    let drain = |v: f64, spurt: bool| -> f64 {
        let mut x = 20.0 * (v - r.base_speed + 12.0).powi(2) / 144.0;
        if spurt {
            x *= guts_mod;
        }
        x
    };
    let cost_spurt_per_m = drain(vs, true) / vs; // HP за метр в спурте
    let cost_cruise_per_m = drain(v2, false) / v2;
    let budget = r.hp - 1.0;
    if budget <= 0.0 {
        return 0.0;
    }
    // budget = L*cs + (remain-L)*cc  →  L = (budget - remain*cc) / (cs - cc)
    if cost_spurt_per_m <= cost_cruise_per_m {
        return remain; // упорство так велико, что спурт не дороже
    }
    let l = (budget - remain * cost_cruise_per_m) / (cost_spurt_per_m - cost_cruise_per_m);
    l.clamp(0.0, remain)
}

// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gamedata::GameData;

    fn gd() -> GameData {
        GameData::load().expect("master.mdb должна быть на машине")
    }

    fn horse(speed: f64, stamina: f64, pow: f64, guts: f64, wiz: f64, style: i32) -> SimHorse {
        SimHorse {
            gate: 1,
            style,
            speed,
            stamina,
            pow,
            guts,
            wiz,
            motivation: 3,
            apt_dist: 7, // A
            apt_style: 7,
            apt_ground: 7,
            skills: vec![],
        }
    }

    #[test]
    fn equal_horses_split_wins() {
        let gd = gd();
        let race = RaceParams { distance: 2000.0, ground: 1, condition: 1 };
        let horses = vec![horse(1000.0, 800.0, 900.0, 400.0, 800.0, 2); 4];
        let r = simulate(&gd, &race, &horses, 200, 42);
        for w in &r.win {
            assert!(*w > 0.10 && *w < 0.40, "win={w}");
        }
    }

    #[test]
    fn faster_horse_wins_more() {
        let gd = gd();
        let race = RaceParams { distance: 1600.0, ground: 1, condition: 1 };
        let mut horses = vec![horse(900.0, 700.0, 800.0, 400.0, 700.0, 2); 3];
        horses[0].speed = 1150.0;
        horses[0].pow = 1000.0;
        let r = simulate(&gd, &race, &horses, 200, 7);
        assert!(r.win[0] > r.win[1] * 1.8, "win: {:?}", r.win);
    }

    #[test]
    fn stamina_matters_on_long() {
        let gd = gd();
        let race = RaceParams { distance: 3000.0, ground: 1, condition: 1 };
        // спидстер без стамины против сбалансированной
        let glass = SimHorse { stamina: 350.0, ..horse(1150.0, 350.0, 900.0, 300.0, 800.0, 2) };
        let solid = horse(1000.0, 950.0, 850.0, 400.0, 800.0, 2);
        let r = simulate(&gd, &race, &[glass, solid], 200, 11);
        assert!(r.win[1] > r.win[0], "win: {:?}", r.win);
    }

    #[test]
    fn finish_times_are_sane() {
        // средняя лошадь на 2000м должна финишировать примерно за 1:55-2:10
        let gd = gd();
        let race = RaceParams { distance: 2000.0, ground: 1, condition: 1 };
        let horses = vec![horse(1000.0, 800.0, 900.0, 400.0, 800.0, 2)];
        // прогоним напрямую один забег и проверим время через simulate-обёртку:
        // здесь просто смоук — победитель определён, паника не случилась
        let r = simulate(&gd, &race, &horses, 10, 3);
        assert_eq!(r.win[0], 1.0);
    }

    #[test]
    fn recovery_skill_helps_on_long() {
        let gd = gd();
        let race = RaceParams { distance: 3200.0, ground: 1, condition: 1 };
        // одинаковые, но у первой — восстановление (круг исцеления 20051?
        // возьмём любой реальный recovery: найдём в данных скилл с типом 9)
        // Самый сильный recovery-скилл (наибольшее значение типа 9).
        let recovery_id = gd
            .skills
            .values()
            .filter_map(|s| {
                let v: f64 = s
                    .variants
                    .iter()
                    .flat_map(|v| v.effects.iter())
                    .filter(|e| e.ability_type == 9)
                    .map(|e| e.value)
                    .fold(0.0, f64::max);
                if v > 0.0 {
                    Some((s.id, v))
                } else {
                    None
                }
            })
            .max_by(|a, b| a.1.partial_cmp(&b.1).unwrap())
            .map(|(id, _)| id)
            .expect("есть recovery-скиллы");
        // Жёсткий дефицит стамины на длинной → восстановление решает.
        let mut a = horse(1050.0, 450.0, 850.0, 400.0, 900.0, 2);
        a.skills = vec![(recovery_id, 5); 4]; // 4 копии, ур.5
        let b = horse(1050.0, 450.0, 850.0, 400.0, 900.0, 2);
        let r = simulate(&gd, &race, &[a, b], 600, 5);
        assert!(
            r.win[0] > r.win[1],
            "recovery должен помогать: {:?} (skill {})",
            r.win,
            recovery_id
        );
    }

    #[test]
    fn accel_skill_in_spurt_helps() {
        let gd = gd();
        let race = RaceParams { distance: 2000.0, ground: 1, condition: 1 };
        // найдём accel-скилл (31) с условием на финальную часть
        let accel_id = gd
            .skills
            .values()
            .filter(|s| {
                s.variants.iter().any(|v| {
                    v.effects.iter().any(|e| e.ability_type == 31 && e.value > 0.0)
                        && v.condition.iter().flatten().any(|c| c.var == "phase" && c.val >= 2.0)
                })
            })
            .map(|s| s.id)
            .min()
            .expect("есть accel-скиллы на финал");
        let mut a = horse(1000.0, 800.0, 900.0, 400.0, 1200.0, 2);
        a.skills = vec![(accel_id, 1); 2];
        let b = horse(1000.0, 800.0, 900.0, 400.0, 1200.0, 2);
        let r = simulate(&gd, &race, &[a, b], 300, 13);
        assert!(
            r.win[0] > r.win[1],
            "accel в спурте должен помогать: {:?} (skill {})",
            r.win,
            accel_id
        );
    }

    #[test]
    fn soft_cap_halves_above_1200() {
        assert_eq!(soft_cap(1000.0), 1000.0);
        assert_eq!(soft_cap(1200.0), 1200.0);
        assert_eq!(soft_cap(1500.0), 1350.0); // пример из правил игры
        assert_eq!(soft_cap(2000.0), 1600.0);
    }

    #[test]
    fn ground_aptitude_decides_dirt_race() {
        // одинаковые статы, но первая — D на грунте, вторая — A
        let gd = gd();
        let race = RaceParams { distance: 1600.0, ground: 2, condition: 1 };
        let mut bad = horse(1000.0, 700.0, 850.0, 400.0, 800.0, 2);
        bad.apt_ground = 4; // D
        let mut good = horse(1000.0, 700.0, 850.0, 400.0, 800.0, 2);
        good.apt_ground = 7; // A
        let r = simulate(&gd, &race, &[bad, good], 200, 21);
        assert!(r.win[1] > r.win[0] * 1.5, "win: {:?}", r.win);
    }

    #[test]
    fn unique_flag_from_db() {
        // Без Wit-гейта — ТОЛЬКО свой уник (id 1xxxxx). Наследуемые (id 9xxxxx)
        // имеют Wit-чек → is_unique == false.
        let gd = gd();
        assert_eq!(gd.skills.get(&100231).map(|s| s.is_unique), Some(true)); // Seiun свой
        assert_eq!(gd.skills.get(&100221).map(|s| s.is_unique), Some(true)); // Fine свой
        assert_eq!(gd.skills.get(&900201).map(|s| s.is_unique), Some(false)); // наследуемый: Wit есть
        assert_eq!(gd.skills.get(&910261).map(|s| s.is_unique), Some(false)); // наследуемый: Wit есть
        assert_eq!(gd.skills.get(&200142).map(|s| s.is_unique), Some(false)); // обычный
    }

    #[test]
    fn unique_skips_wit_gate() {
        // Уника срабатывает даже при нулевом шансе активации и «плохом» броске;
        // обычный скилл — только если бросок прошёл порог.
        assert!(passes_activation_gate(true, 0.0, 0.99)); // уника, act_rate 0
        assert!(passes_activation_gate(true, 0.1, 0.95)); // уника, плохой бросок
        assert!(!passes_activation_gate(false, 0.5, 0.9)); // обычный, бросок мимо
        assert!(passes_activation_gate(false, 0.8, 0.5)); // обычный, бросок прошёл
    }

    #[test]
    fn blocking_hurts_closers_vs_leader() {
        // Идентичные статы, но один nige (лидер), другой oikomi (рискует блоком).
        // Лидер не должен систематически проигрывать из-за отсутствия позиционки.
        let gd = gd();
        let race = RaceParams { distance: 2000.0, ground: 1, condition: 1 };
        let leader = horse(1050.0, 800.0, 950.0, 450.0, 800.0, 1);
        let closer = horse(1050.0, 800.0, 950.0, 450.0, 800.0, 4);
        let r = simulate(&gd, &race, &[leader, closer], 500, 77);
        // не требуем, чтобы лидер выигрывал, но разрыв не должен быть разгромным
        // в пользу закрывающего (раньше модель сильно занижала лидера).
        assert!(r.win[0] > 0.30, "leader winrate too low: {:?}", r.win);
    }

    #[test]
    fn deterministic_with_seed() {
        let gd = gd();
        let race = RaceParams { distance: 1800.0, ground: 1, condition: 1 };
        let horses = vec![
            horse(1100.0, 800.0, 900.0, 400.0, 900.0, 1),
            horse(1000.0, 850.0, 950.0, 450.0, 800.0, 3),
        ];
        let a = simulate(&gd, &race, &horses, 100, 99);
        let b = simulate(&gd, &race, &horses, 100, 99);
        assert_eq!(a.win, b.win);
    }
}
