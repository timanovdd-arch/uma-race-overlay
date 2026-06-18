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
//! - Position Keep (первая половина гонки): режимы front/pace/late/end по
//!   реальным разрывам между лошадьми — передние палят HP за позицию, задние
//!   экономят. Прогон распараллелен по ядрам (одно ядро оставляем игре).
//!
//! НЕ моделируется (поправляется калибровкой): дорожки/блокировка, уклоны,
//! погода/сезон/состояние грунта (считаем «хорошее»).

use std::collections::HashMap;

use crate::course::CourseGeom;
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
    /// Реальная геометрия трассы (углы/уклоны/прямые). None → синтетика.
    pub course: Option<CourseGeom>,
}

impl Default for RaceParams {
    fn default() -> Self {
        Self { distance: 2000.0, ground: 1, condition: 1, course: None }
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

/// Debug-тумблер: `UMA_SIM_NO_PK=1` отключает Position Keep (для A/B-калибровки).
/// Читается один раз на процесс.
fn pk_enabled() -> bool {
    static V: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *V.get_or_init(|| std::env::var("UMA_SIM_NO_PK").is_err())
}

/// Debug-тумблер: `UMA_SIM_NO_WISVAR=1` отключает по-секционный wit-random (для
/// проверки, шум ли держит слабые лошади в игре).
fn wisvar_enabled() -> bool {
    static V: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *V.get_or_init(|| std::env::var("UMA_SIM_NO_WISVAR").is_err())
}

/// Debug-тумблер: `UMA_SIM_NO_BLOCK=1` глобально выключает трафик-модель (поверх
/// параметра block_model) — для локализации компрессии поля.
fn block_env_enabled() -> bool {
    static V: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *V.get_or_init(|| std::env::var("UMA_SIM_NO_BLOCK").is_err())
}

/// Debug-калибровка: `SKILL_SCALE=<f>` глобально масштабирует величину гоночных
/// скилл-эффектов (деф. 1.0). Для проверки гипотезы «скиллы переоценены/переуверены»:
/// 0.0 ≈ без скиллов, <1.0 — слабее. Только для калибровочных прогонов.
fn skill_scale() -> f64 {
    static V: std::sync::OnceLock<f64> = std::sync::OnceLock::new();
    *V.get_or_init(|| {
        std::env::var("SKILL_SCALE").ok().and_then(|s| s.parse().ok()).unwrap_or(1.0)
    })
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

// --- Position Keep (модель UmaLator/RaceSolver.ts: только Pace Down) ---
// ВАЖНО: UmaLator НЕ даёт передовым разгон (+4% Speed Up/Pace Up Ex) — преимущество
// nige уже зашито в коэффициенте фазы (1.0 vs 0.93). Позиционка моделируется как
// Pace DOWN: лошадь, зажатая слишком близко к пейсмейкеру, ПРИТОРМАЖИВАЕТ (экономит
// HP, держит дистанцию). Разгоны делали front «непроигрываемыми» — убраны.
/// Конец окна позиционки (доля дистанции). UmaLator кап = 5 секций = 5/24.
const PK_END: f64 = 5.0 / 24.0;
/// Трафик: в пределах скольки метров за идущим впереди лошадь «в стенке».
const BLOCK_DIST: f64 = 4.0;
/// Порог «близко впереди» для infront_near_lane_time (м). ⚠ кандидат на калибровку (шаг 3).
const NEAR_LANE: f64 = 3.0;
/// Радиус для near_count (м). ⚠ кандидат на калибровку (шаг 3).
const NEAR_COUNT_RADIUS: f64 = 5.0;
/// Шанс найти просвет и пройти сквозь трафик за тик (растёт с силой).
fn pass_prob(pow_adj: f64) -> f64 {
    (0.08 + pow_adj / 3500.0).clamp(0.08, 0.45)
}
/// Доля преимущества скорости, которую зажатая лошадь всё же реализует (просветы).
/// 0 = полная стенка (липко к старт-позиции), выше = навыки/статы выражаются сильнее.
const BLOCK_LEAK: f64 = 0.1;
/// Множитель Pace Down: фаза 1 / фаза 2+ (UmaLator: 0.945 / 0.915).
const PACE_DOWN_P1: f64 = 0.945;
const PACE_DOWN_P2: f64 = 0.915;

/// Минимальный gap до пейсмейкера (м): ближе → Pace Down. UmaLator
/// BaseMinimumThreshold[strategy] × courseFactor (senko — без courseFactor).
fn pk_min_threshold(style: i32, d: f64) -> f64 {
    let base = match style {
        2 => 3.0, // senko (Pace Chaser)
        3 => 6.5, // sashi (Late Surger)
        4 => 7.5, // oikomi (End Closer)
        _ => 0.0, // nige не приторможивает (он и есть голова)
    };
    if style == 2 {
        base
    } else {
        base * (0.0008 * (d - 1000.0) + 1.0) // courseFactor
    }
}

/// Множитель target speed от Position Keep (только Pace Down). Возвращает <1.0,
/// если лошадь зажата у пейсмейкера, иначе 1.0. Отменяется speed-скиллом и
/// закидыванием (как в UmaLator).
fn position_keep_mult(
    style: i32,
    phase: i32,
    pacemaker_gap: f64,
    speed_skill_active: bool,
    rushed: bool,
    d: f64,
) -> f64 {
    if style == 1 || speed_skill_active || rushed {
        return 1.0;
    }
    if pacemaker_gap < pk_min_threshold(style, d) {
        if phase >= 2 {
            PACE_DOWN_P2
        } else {
            PACE_DOWN_P1
        }
    } else {
        1.0
    }
}

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
    /// Свой RNG для бросков ВНУТРИ забега (трафик/прорыв сквозь пелотон),
    /// независимый от других лошадей.
    race_rng: fastrand::Rng,
    // --- накопители позиционных условий (continue-time + история места) ---
    /// Время с кем-то близко впереди (сбрасывается, когда разрыв открывается).
    infront_near_time: f64,
    /// Время «в стенке» спереди (разрыв < BLOCK_DIST).
    blocked_front_time: f64,
    /// Место на прошлом тике (для детекта обгона/смены места).
    prev_order: i32,
    /// Хоть раз сменил место за забег (change_order_onetime).
    changed_order: bool,
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
    /// 0 ровно, 1 подъём, 2 спуск (по реальной геометрии трассы).
    slope: f64,
    /// 1 если на прямом участке (по реальной геометрии).
    straight: f64,
    /// id ипподрома (raceTrackId) — для track-зелёных скиллов; 0 если курс не задан.
    track_id: f64,
    accumulatetime: f64,
    post_number: f64,
    popularity: f64,
    is_badstart: f64,
    is_basis_distance: f64,
    pos: f64,
    // --- позиционные (из состояния симуляции, см. PosMode) ---
    bashin_diff_infront: f64,
    bashin_diff_behind: f64,
    infront_near_lane_time: f64,
    blocked_front_time: f64,
    blocked_side_time: f64,
    blocked_all_time: f64,
    is_overtake: f64,
    change_order: f64,
    /// Знак смены места за тик: <0 обогнал, >0 обошли, 0 без изменений.
    change_order_onetime: f64,
    near_count: f64,
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
        "slope" => ctx.slope,      // 0 ровно / 1 подъём / 2 спуск (реальная геометрия)
        "straight" => ctx.straight,
        "track_id" => ctx.track_id, // ипподром (track-зелёные скиллы)
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

/// Режим разрешения ПОЗИЦИОННЫХ условий (трафик/обгоны), которые модель знает лишь
/// приблизительно. Геометрические/статовые условия от режима НЕ зависят.
/// - Expected: считаем из состояния симуляции (главное число винрейта);
/// - Floor: позиционные условия = ложь (худший случай, «позиционка не сложилась»);
/// - Ceiling: позиционные условия = истина (лучший случай, «всё сложилось идеально»).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PosMode {
    Floor,
    Expected,
    Ceiling,
}

/// Это позиционное условие (зависит от взаимного положения лошадей, а не от трассы/статов)?
fn is_positional(var: &str) -> bool {
    matches!(
        var,
        "bashin_diff_infront"
            | "bashin_diff_behind"
            | "infront_near_lane_time"
            | "blocked_front_continuetime"
            | "blocked_side_continuetime"
            | "blocked_all_continuetime"
            | "is_overtake"
            | "change_order_onetime"
            | "change_order"
            | "overtake_target_time"
            | "near_count"
    )
}

/// Значение позиционной переменной, вычисленное ИЗ состояния симуляции.
/// None = такую позиционку пока не считаем (в Expected уйдёт в пре-ролл).
fn positional_value(ctx: &CondCtx, var: &str) -> Option<f64> {
    Some(match var {
        "bashin_diff_infront" => ctx.bashin_diff_infront,
        "bashin_diff_behind" => ctx.bashin_diff_behind,
        "infront_near_lane_time" => ctx.infront_near_lane_time,
        "blocked_front_continuetime" => ctx.blocked_front_time,
        "blocked_all_continuetime" => ctx.blocked_all_time,
        "blocked_side_continuetime" => ctx.blocked_side_time,
        "is_overtake" => ctx.is_overtake,
        "change_order_onetime" => ctx.change_order_onetime,
        "change_order" => ctx.change_order,
        "near_count" => ctx.near_count,
        _ => return None,
    })
}

fn eval_cond(
    tree: &CondTree,
    ctx: &CondCtx,
    rt_unsupported: &HashMap<usize, bool>,
    rt_random: &HashMap<usize, f64>,
    mode: PosMode,
) -> bool {
    if tree.is_empty() {
        return true;
    }
    'group: for (gi, group) in tree.iter().enumerate() {
        for (ci, c) in group.iter().enumerate() {
            let ok = eval_one(
                c,
                ctx,
                rt_unsupported.get(&cond_key(gi, ci)),
                rt_random.get(&cond_key(gi, ci)),
                mode,
            );
            if !ok {
                continue 'group;
            }
        }
        return true; // вся группа И-условий истинна
    }
    false
}

fn eval_one(
    c: &Cond,
    ctx: &CondCtx,
    unsupported: Option<&bool>,
    random_pt: Option<&f64>,
    mode: PosMode,
) -> bool {
    // *_random: срабатывают в заранее брошенной точке (окно ~60 м).
    if c.var.ends_with("_random") || c.var == "is_finalcorner_random" {
        if let Some(&pt) = random_pt {
            return ctx.pos >= pt && ctx.pos <= pt + 60.0;
        }
        return false;
    }
    // Позиционные условия: по режиму (floor=ложь, ceiling=истина, expected=из симуляции).
    if is_positional(&c.var) {
        return match mode {
            PosMode::Floor => false,
            PosMode::Ceiling => true,
            PosMode::Expected => match positional_value(ctx, &c.var) {
                Some(v) => cmp(c.op, v, c.val),
                None => unsupported.copied().unwrap_or(false),
            },
        };
    }
    match cond_var(ctx, &c.var) {
        Some(v) => cmp(c.op, v, c.val),
        None => unsupported.copied().unwrap_or(false),
    }
}

/// Окно дистанции (в метрах) для *_random переменной; None = вся гонка.
/// Углы/прямые берутся из реальной геометрии трассы, если она задана.
fn random_window(var: &str, val: f64, d: f64, course: Option<&CourseGeom>) -> (f64, f64) {
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
        "is_finalcorner_random" => match course.and_then(|c| c.corners.last()) {
            Some(&(s, e)) => (s, e),
            None => (d * FINAL_CORNER.0, d * FINAL_CORNER.1),
        },
        "corner_random" | "all_corner_random" => match course {
            Some(c) if !c.corners.is_empty() => (c.corners[0].0, c.corners.last().unwrap().1),
            _ => (d * CORNERS[0].0, d * CORNERS[2].1),
        },
        "straight_random" => match course.and_then(|c| c.straights.last()) {
            Some(&(s, e)) => (s, e),
            None => (d * 0.72, d * 0.95), // финишная прямая
        },
        "distance_rate_after_random" => (d * val / 100.0, d),
        _ => (0.0, d),
    }
}

// ---------------------------------------------------------------------------
// Сам симулятор
// ---------------------------------------------------------------------------

/// Сколько потоков отдать симуляции. Оставляем ОДНО ядро игре/ОС, чтобы на
/// слабых ПК (2–4 ядра) оверлей не лагал; верхний предел — чтобы не плодить
/// потоки на сильных машинах (выгода всё равно убывает).
fn recommended_workers() -> usize {
    std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1)
        .saturating_sub(1)
        .clamp(1, 6)
}

/// Прогоняет забеги `range` и копит счётчики (wins, top3, sum мест).
fn accumulate(
    gd: &GameData,
    race: &RaceParams,
    horses: &[SimHorse],
    seed: u64,
    range: std::ops::Range<u64>,
    mode: PosMode,
    block_model: bool,
) -> (Vec<u32>, Vec<u32>, Vec<u64>) {
    let n = horses.len();
    let mut wins = vec![0u32; n];
    let mut top3 = vec![0u32; n];
    let mut place_sum = vec![0u64; n];
    for run in range {
        // Сид зависит ТОЛЬКО от индекса забега → результат не зависит от того,
        // как прогоны разбиты по потокам (детерминизм + честный параллелизм).
        let mut rng = fastrand::Rng::with_seed(seed.wrapping_add(run));
        let order = run_race(gd, race, horses, &mut rng, mode, block_model);
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
    (wins, top3, place_sum)
}

pub fn simulate(
    gd: &GameData,
    race: &RaceParams,
    horses: &[SimHorse],
    runs: u32,
    seed: u64,
) -> SimResult {
    simulate_with_workers(gd, race, horses, runs, seed, recommended_workers(), PosMode::Expected, true)
}

/// Как `simulate`, но с явным режимом позиционки (floor/expected/ceiling).
pub fn simulate_mode(
    gd: &GameData,
    race: &RaceParams,
    horses: &[SimHorse],
    runs: u32,
    seed: u64,
    mode: PosMode,
) -> SimResult {
    simulate_with_workers(gd, race, horses, runs, seed, recommended_workers(), mode, true)
}

/// Контрфактуал «без блока/трафика»: тот же забег (тот же сид → честное A/B), но
/// модель трафика выключена — лошади едут на своей target speed, не упираясь в
/// идущего впереди. Разница `avg_place` с обычным `simulate` = цена блока («block kill»).
pub fn simulate_no_block(
    gd: &GameData,
    race: &RaceParams,
    horses: &[SimHorse],
    runs: u32,
    seed: u64,
) -> SimResult {
    simulate_with_workers(gd, race, horses, runs, seed, recommended_workers(), PosMode::Expected, false)
}

/// Как `simulate`, но с явным числом потоков (для тестов параллелизма) и режимом.
/// Результат идентичен при любом `workers` — каждый забег детерминирован сидом,
/// а счётчики суммируются (порядок не важен).
pub fn simulate_with_workers(
    gd: &GameData,
    race: &RaceParams,
    horses: &[SimHorse],
    runs: u32,
    seed: u64,
    workers: usize,
    mode: PosMode,
    block_model: bool,
) -> SimResult {
    let n = horses.len();
    let runs64 = runs as u64;
    let (mut wins, mut top3, mut place_sum) = (vec![0u32; n], vec![0u32; n], vec![0u64; n]);

    // Мелкие прогоны (тесты) или одно ядро — без накладных расходов на потоки.
    if workers <= 1 || runs < 64 {
        let (w, t, p) = accumulate(gd, race, horses, seed, 0..runs64, mode, block_model);
        wins = w;
        top3 = t;
        place_sum = p;
    } else {
        let w = workers.min(runs as usize);
        std::thread::scope(|scope| {
            let mut handles = Vec::with_capacity(w);
            for k in 0..w as u64 {
                // равные чанки диапазона 0..runs (целочисленное разбиение)
                let lo = runs64 * k / w as u64;
                let hi = runs64 * (k + 1) / w as u64;
                handles.push(scope.spawn(move || accumulate(gd, race, horses, seed, lo..hi, mode, block_model)));
            }
            for h in handles {
                let (w2, t2, p2) = h.join().unwrap();
                for i in 0..n {
                    wins[i] += w2[i];
                    top3[i] += t2[i];
                    place_sum[i] += p2[i];
                }
            }
        });
    }

    SimResult {
        win: wins.iter().map(|w| *w as f64 / runs as f64).collect(),
        top3: top3.iter().map(|w| *w as f64 / runs as f64).collect(),
        avg_place: place_sum.iter().map(|s| *s as f64 / runs as f64).collect(),
        runs,
    }
}

/// Один забег; возвращает место каждой лошади (1 = победа), индексы как на входе.
fn run_race(
    gd: &GameData,
    race: &RaceParams,
    horses: &[SimHorse],
    rng: &mut fastrand::Rng,
    mode: PosMode,
    block_model: bool,
) -> Vec<i32> {
    let d = race.distance;
    let n = horses.len();
    let base_speed_course = 20.0 - (d - 2000.0) / 1000.0;
    let dist_type = distance_type(d);
    let track_id = race.course.as_ref().map(|c| c.race_track_id as f64).unwrap_or(0.0);
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

        // --- Position Keep: пейсмейкер на тик (для Pace Down) ---
        // Пейсмейкер = лидер самого переднего присутствующего стиля (UmaLator).
        let mut min_style = 5i32;
        let mut style_max_pos = [f64::MIN; 5];
        for rr in runners.iter() {
            if (1..=4).contains(&rr.style) {
                let s = rr.style as usize;
                if rr.pos > style_max_pos[s] {
                    style_max_pos[s] = rr.pos;
                }
                min_style = min_style.min(rr.style);
            }
        }
        let pacemaker_pos = if (1..=4).contains(&min_style) {
            style_max_pos[min_style as usize]
        } else {
            runners[idx[0]].pos
        };

        // Трафик: для каждой лошади — разрыв/скорость идущего ПРЯМО впереди и разрыв
        // до идущего сзади (по отсортированному idx). Лидер свободен (gap = ∞).
        let mut ahead_gap = vec![f64::INFINITY; n];
        let mut ahead_speed = vec![0.0f64; n];
        let mut behind_gap = vec![f64::INFINITY; n];
        for k in 0..n {
            let i = idx[k];
            if k > 0 {
                let j = idx[k - 1];
                ahead_gap[i] = runners[j].pos - runners[i].pos;
                ahead_speed[i] = runners[j].v;
            }
            if k + 1 < n {
                let j = idx[k + 1];
                behind_gap[i] = runners[i].pos - runners[j].pos;
            }
        }
        // near_count: соседи в радиусе NEAR_COUNT_RADIUS (локальный скан по сортировке).
        let mut near_count = vec![0i32; n];
        for k in 0..n {
            let i = idx[k];
            let mut cnt = 0;
            for m in (k + 1)..n {
                if runners[idx[m]].pos < runners[i].pos - NEAR_COUNT_RADIUS {
                    break;
                }
                cnt += 1;
            }
            for m in (0..k).rev() {
                if runners[idx[m]].pos > runners[i].pos + NEAR_COUNT_RADIUS {
                    break;
                }
                cnt += 1;
            }
            near_count[i] = cnt;
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

            // --- накопители позиционных условий (continue-time + смена места) ---
            if ahead_gap[i] < NEAR_LANE {
                r.infront_near_time += DT;
            } else {
                r.infront_near_time = 0.0;
            }
            if ahead_gap[i] < BLOCK_DIST {
                r.blocked_front_time += DT;
            } else {
                r.blocked_front_time = 0.0;
            }
            let overtook = r.order < r.prev_order; // место улучшилось → обгон
            // Знак смены места за тик: обгон = номер места УМЕНЬШИЛСЯ = ОТРИЦАТЕЛЬНО
            // (как в игре). Скиллы-обгона имеют условие `change_order_onetime < 0`.
            let order_change_sign: f64 = if overtook {
                -1.0
            } else if r.order > r.prev_order {
                1.0
            } else {
                0.0
            };
            if overtook {
                r.changed_order = true;
            }
            r.prev_order = r.order;

            // секция сменилась → новый разброс ума + переоценка спурта
            let sec = (r.pos / (d / 24.0)) as i32;
            if sec != r.section {
                r.section = sec;
                let up = (r.wiz_adj / 5500.0) * (r.wiz_adj * 0.1).log10();
                let lo = up - 0.65;
                r.wis_var = if wisvar_enabled() {
                    r.base_speed * (lo + rng.f64() * (up - lo)) / 100.0
                } else {
                    0.0
                };
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

            // Геометрия: реальные углы/уклоны/прямые по позиции, иначе синтетика.
            let (corner_v, finalcorner_v, slope_v, straight_v) = match &race.course {
                Some(cg) => (
                    cg.in_corner(r.pos) as i32 as f64,
                    cg.is_final_corner(r.pos) as i32 as f64,
                    cg.slope_kind(r.pos),
                    cg.in_straight(r.pos) as i32 as f64,
                ),
                None => (
                    if in_corner(frac) { 1.0 } else { 0.0 },
                    in_window(frac, FINAL_CORNER) as i32 as f64,
                    0.0,
                    0.0,
                ),
            };

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
                is_finalcorner: finalcorner_v,
                corner: corner_v,
                slope: slope_v,
                straight: straight_v,
                track_id,
                accumulatetime: t - r.start_delay,
                post_number: r.gate as f64,
                popularity: r.popularity as f64,
                is_badstart: (r.start_delay > 0.08) as i32 as f64,
                is_basis_distance: ((d as i32) % 400 == 0) as i32 as f64,
                pos: r.pos,
                bashin_diff_infront: ahead_gap[i] / 2.5,
                bashin_diff_behind: behind_gap[i] / 2.5,
                infront_near_lane_time: r.infront_near_time,
                blocked_front_time: r.blocked_front_time,
                blocked_all_time: r.blocked_front_time, // front/all не различаем
                blocked_side_time: 0.0,                 // боковую (полосы) не моделируем
                is_overtake: overtook as i32 as f64,
                change_order: r.changed_order as i32 as f64,
                change_order_onetime: order_change_sign,
                near_count: near_count[i] as f64,
            };
            let mut activations: Vec<(f64, i32, f64, i32)> = Vec::new(); // (dur, ability, value, target)
            for s in r.skills.iter_mut() {
                if s.used || !s.gate_ok {
                    continue;
                }
                if !eval_cond(&s.variant.precondition, &ctx, &s.unsupported, &s.random_pts, mode) {
                    continue;
                }
                if !eval_cond(&s.variant.condition, &ctx, &s.unsupported, &s.random_pts, mode) {
                    continue;
                }
                s.used = true; // одноразово (кулдауны редки у гоночных скиллов)
                let dur = s.variant.duration_s * d / 1000.0;
                for eff in &s.variant.effects {
                    let val = gd.effect_value(eff, s.level) * skill_scale();
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

            // --- Position Keep (Pace Down, модель UmaLator) ---
            // Закрывающий, зажатый слишком близко к пейсмейкеру, приторможивает
            // (экономит HP, держит дистанцию). Передовым разгона НЕ даём. Замедление
            // само снижает расход HP. Отменяется speed-скиллом и закидыванием.
            if r.hp > 0.0 && !r.spurting && frac <= PK_END && pk_enabled() {
                let speed_skill_active =
                    r.active.iter().any(|e| e.ability == 27 && e.value > 0.0);
                let rushed = r.rushed_section >= 0
                    && sec >= r.rushed_section
                    && sec < r.rushed_section + 2;
                target *= position_keep_mult(
                    r.style,
                    phase,
                    pacemaker_pos - r.pos,
                    speed_skill_active,
                    rushed,
                    d,
                );
            }

            // --- позиционные поправки (3.1, из правил «Target Speed») ---
            // Одинокий лидер-nige тянет темп ПОСЛЕ позиционки (Securing the Lead).
            if r.style == 1 && r.order == 1 && phase >= 1 && r.hp > 0.0 && frac > PK_END {
                target *= 1.0 + LEADER_LEAD_BONUS;
            }
            // Трафик в финале/спурте: зажат за идущим впереди (gap < BLOCK_DIST) и
            // хочет ехать быстрее него → ограничен его скоростью, если не нашёл просвет
            // (шанс прорыва растёт с силой). Лидер (gap=∞) свободен → ушедшие рано в
            // отрыв удерживают позицию; закрывающие пробиваются сквозь пелотон не всегда.
            if block_model && block_env_enabled() && phase >= 2 && ahead_gap[i] < BLOCK_DIST && ahead_speed[i] < target {
                // Чем сильнее хочешь ехать быстрее зажавшего, тем легче объехать.
                let deficit = (target - ahead_speed[i]) / target;
                let p = (pass_prob(r.pow_adj) + 1.6 * deficit).clamp(0.0, 0.95);
                if r.race_rng.f64() > p {
                    // не полная остановка: реализуем малую долю преимущества (просветы
                    // в стенке) — иначе забег «прилипает» к старт-позиции и навыки/статы
                    // не выражаются.
                    let leaked = ahead_speed[i] + (target - ahead_speed[i]) * BLOCK_LEAK;
                    target = target.min(leaked);
                }
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
                            let (a, b) = random_window(&c.var, c.val, d, race.course.as_ref());
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
        race_rng: fastrand::Rng::with_seed(rng.u64(..)),
        infront_near_time: 0.0,
        blocked_front_time: 0.0,
        prev_order: 1,
        changed_order: false,
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
        slope: 0.0,
        straight: 0.0,
        track_id: 0.0,
        accumulatetime: 0.0,
        post_number: 0.0,
        popularity: 0.0,
        is_badstart: 0.0,
        is_basis_distance: 0.0,
        pos: 0.0,
        bashin_diff_infront: 0.0,
        bashin_diff_behind: 0.0,
        infront_near_lane_time: 0.0,
        blocked_front_time: 0.0,
        blocked_side_time: 0.0,
        blocked_all_time: 0.0,
        is_overtake: 0.0,
        change_order: 0.0,
        change_order_onetime: 0.0,
        near_count: 0.0,
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
        let race = RaceParams { distance: 2000.0, ground: 1, condition: 1, course: None };
        let horses = vec![horse(1000.0, 800.0, 900.0, 400.0, 800.0, 2); 4];
        let r = simulate(&gd, &race, &horses, 200, 42);
        for w in &r.win {
            assert!(*w > 0.10 && *w < 0.40, "win={w}");
        }
    }

    // Диагностика калибровки (юзер): сильный nige без скиллов должен громить
    // безскилловое поле на 2200 (накопление через accel→топ, закрывающим нечем
    // создать бурст). Если closer тут берёт заметный %, базовая модель занижает nige.
    #[test]
    #[ignore]
    fn scenario_nige_dominance_no_skills() {
        let gd = gd();
        let race = RaceParams { distance: 2200.0, ground: 1, condition: 1, course: None };
        // Seiun Sky: nige, реальные статы, middle S, мотивация 5
        let mut seiun = horse(1200.0, 805.0, 1200.0, 528.0, 930.0, 1);
        seiun.apt_dist = 8; // S на средней
        seiun.motivation = 5;

        // 1) vs сильный закрывающий (oikomi), схожие статы, БЕЗ скиллов
        let mut closer = horse(1100.0, 1000.0, 900.0, 450.0, 850.0, 4);
        closer.apt_dist = 8;
        closer.motivation = 5;
        let r = simulate(&gd, &race, &[seiun_c(&seiun), closer], 3000, 42);
        println!(
            "NO-SKILL 2200  Seiun(nige) {:.1}%  vs  Closer(oikomi) {:.1}%   avgPlace {:?}",
            r.win[0] * 100.0, r.win[1] * 100.0, r.avg_place
        );

        // 2) vs средний senko, БЕЗ скиллов
        let mut mid = horse(1000.0, 900.0, 750.0, 420.0, 820.0, 2);
        mid.motivation = 5;
        let r2 = simulate(&gd, &race, &[seiun_c(&seiun), mid], 3000, 42);
        println!(
            "NO-SKILL 2200  Seiun(nige) {:.1}%  vs  Mid(senko)  {:.1}%",
            r2.win[0] * 100.0, r2.win[1] * 100.0
        );
    }
    fn seiun_c(s: &SimHorse) -> SimHorse {
        SimHorse {
            gate: 1,
            style: s.style,
            speed: s.speed,
            stamina: s.stamina,
            pow: s.pow,
            guts: s.guts,
            wiz: s.wiz,
            motivation: s.motivation,
            apt_dist: s.apt_dist,
            apt_style: s.apt_style,
            apt_ground: s.apt_ground,
            skills: vec![],
        }
    }

    #[test]
    fn faster_horse_wins_more() {
        let gd = gd();
        let race = RaceParams { distance: 1600.0, ground: 1, condition: 1, course: None };
        let mut horses = vec![horse(900.0, 700.0, 800.0, 400.0, 700.0, 2); 3];
        horses[0].speed = 1150.0;
        horses[0].pow = 1000.0;
        let r = simulate(&gd, &race, &horses, 200, 7);
        assert!(r.win[0] > r.win[1] * 1.8, "win: {:?}", r.win);
    }

    #[test]
    fn stamina_matters_on_long() {
        let gd = gd();
        let race = RaceParams { distance: 3000.0, ground: 1, condition: 1, course: None };
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
        let race = RaceParams { distance: 2000.0, ground: 1, condition: 1, course: None };
        let horses = vec![horse(1000.0, 800.0, 900.0, 400.0, 800.0, 2)];
        // прогоним напрямую один забег и проверим время через simulate-обёртку:
        // здесь просто смоук — победитель определён, паника не случилась
        let r = simulate(&gd, &race, &horses, 10, 3);
        assert_eq!(r.win[0], 1.0);
    }

    #[test]
    fn recovery_skill_helps_on_long() {
        let gd = gd();
        let race = RaceParams { distance: 3200.0, ground: 1, condition: 1, course: None };
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
        let race = RaceParams { distance: 2000.0, ground: 1, condition: 1, course: None };
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
        // A/B-изоляция скилла: одна и та же лошадь СО скиллом и БЕЗ него гоняется
        // против ОДНОГО И ТОГО ЖЕ поля и сида. Так трафик/RNG одинаковы у обоих
        // прогонов, и разница в победах горючего[0] = чистый эффект accel-скилла.
        // (Раньше тест ставил две лошади в один забег — с моделью трафика они
        // блокируют друг друга, и эффект скилла маскируется.)
        let field = || {
            vec![
                horse(980.0, 820.0, 880.0, 410.0, 900.0, 1),
                horse(1010.0, 790.0, 910.0, 420.0, 850.0, 3),
                horse(990.0, 810.0, 890.0, 400.0, 880.0, 4),
            ]
        };
        let with = {
            let mut a = horse(1000.0, 800.0, 900.0, 400.0, 1200.0, 2);
            a.skills = vec![(accel_id, 1); 2];
            let mut f = vec![a];
            f.extend(field());
            simulate(&gd, &race, &f, 600, 13)
        };
        let without = {
            let mut f = vec![horse(1000.0, 800.0, 900.0, 400.0, 1200.0, 2)];
            f.extend(field());
            simulate(&gd, &race, &f, 600, 13)
        };
        assert!(
            with.win[0] > without.win[0],
            "accel должен помогать: со скиллом {:.3} vs без {:.3} (skill {})",
            with.win[0],
            without.win[0],
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
        let race = RaceParams { distance: 1600.0, ground: 2, condition: 1, course: None };
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
        let race = RaceParams { distance: 2000.0, ground: 1, condition: 1, course: None };
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
        let race = RaceParams { distance: 1800.0, ground: 1, condition: 1, course: None };
        let horses = vec![
            horse(1100.0, 800.0, 900.0, 400.0, 900.0, 1),
            horse(1000.0, 850.0, 950.0, 450.0, 800.0, 3),
        ];
        let a = simulate(&gd, &race, &horses, 100, 99);
        let b = simulate(&gd, &race, &horses, 100, 99);
        assert_eq!(a.win, b.win);
    }

    #[test]
    fn parallel_equals_serial() {
        // Параллельный прогон обязан давать ТЕ ЖЕ числа, что и однопоточный:
        // каждый забег детерминирован сидом, счётчики суммируются.
        let gd = gd();
        let race = RaceParams { distance: 2000.0, ground: 1, condition: 1, course: None };
        let horses = vec![
            horse(1100.0, 800.0, 950.0, 450.0, 800.0, 1),
            horse(1000.0, 850.0, 900.0, 400.0, 900.0, 2),
            horse(1050.0, 800.0, 920.0, 420.0, 850.0, 3),
            horse(980.0, 820.0, 880.0, 430.0, 820.0, 4),
        ];
        let s = simulate_with_workers(&gd, &race, &horses, 512, 2024, 1, PosMode::Expected, true);
        let p = simulate_with_workers(&gd, &race, &horses, 512, 2024, 4, PosMode::Expected, true);
        assert_eq!(s.win, p.win);
        assert_eq!(s.avg_place, p.avg_place);
        assert_eq!(s.top3, p.top3);
    }

    // Анализ реальной гонки: читает JSON-дамп плагина (как apply_sim), гоняет
    // симулятор и печатает win% против фактического финиша. Запуск:
    //   RACE_JSON="C:\путь\race.json" cargo test --release analyze_race_json -- --ignored --nocapture
    #[test]
    #[ignore]
    fn analyze_race_json() {
        let path = std::env::var("RACE_JSON").expect("set RACE_JSON=<path to race.json>");
        let text = std::fs::read_to_string(&path).expect("read RACE_JSON");
        let snap: crate::RaceSnapshot = serde_json::from_str(&text).expect("parse race json");
        let horses = &snap.horses;
        let gd = gd();

        // Debug-флаги эксперимента: RACE_DIST форсит дистанцию; RACE_STRIP_UNIQUES=1
        // убирает уники; RACE_COURSE=<id> выбирает курс (деф. 10906 = Hanshin 2200m).
        let strip_uniques = std::env::var("RACE_STRIP_UNIQUES").is_ok();
        let dist_override: Option<f64> = std::env::var("RACE_DIST").ok().and_then(|s| s.parse().ok());
        let course_id: i32 = std::env::var("RACE_COURSE")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(10906);

        let courses = crate::course::load_courses(std::path::Path::new("data/course_data.json"));
        let geom = courses.as_ref().and_then(|m| m.get(&course_id).cloned());
        match &geom {
            Some(g) => eprintln!(
                "course {course_id}: dist {} углы {:?} уклоны {:?}",
                g.distance, g.corners, g.slopes
            ),
            None => eprintln!("course {course_id}: НЕ найден (геометрия = синтетика)"),
        }

        let distance = dist_override
            .or(geom.as_ref().map(|g| g.distance))
            .unwrap_or_else(|| crate::deduce_course_distance(horses).unwrap_or(2000.0) as f64);
        let dirt = horses.iter().filter(|h| h.gtype == 2).count();
        let turf = horses.iter().filter(|h| h.gtype == 1).count();
        let ground = if dirt > turf { 2 } else { 1 };
        let cat = match distance as i32 {
            ..=1400 => 0,
            1401..=1800 => 1,
            1801..=2400 => 2,
            _ => 3,
        };

        // Маппинг 1-в-1 как в apply_sim.
        let sim_horses: Vec<SimHorse> = horses
            .iter()
            .filter(|h| h.stat(0).is_some())
            .map(|h| SimHorse {
                gate: h.gate,
                style: if (1..=4).contains(&h.style) { h.style } else { 2 },
                speed: h.stat(0).unwrap_or(800.0) as f64,
                stamina: h.stat(1).unwrap_or(600.0) as f64,
                pow: h.stat(2).unwrap_or(600.0) as f64,
                guts: h.stat(3).unwrap_or(400.0) as f64,
                wiz: h.stat(4).unwrap_or(500.0) as f64,
                motivation: if (1..=5).contains(&h.motiv) { h.motiv } else { 3 },
                apt_dist: if (1..=8).contains(&h.adist) { h.adist } else { h.aptitude(cat).unwrap_or(7) },
                apt_style: h.aptitude(4).unwrap_or(7),
                apt_ground: if (1..=8).contains(&h.aground) { h.aground } else { 7 },
                skills: if strip_uniques {
                    h.skills.iter().copied().filter(|(id, _)| *id >= 200_000).collect()
                } else {
                    h.skills.clone()
                },
            })
            .collect();

        let race = RaceParams { distance, ground, condition: 1, course: geom };
        // Три режима: expected (главное число, 3000), floor/ceiling (диапазон, 1500).
        let exp = simulate_mode(&gd, &race, &sim_horses, 3000, 12345, PosMode::Expected);
        let flo = simulate_mode(&gd, &race, &sim_horses, 1500, 12345, PosMode::Floor);
        let cei = simulate_mode(&gd, &race, &sim_horses, 1500, 12345, PosMode::Ceiling);

        // имя/факт-место по gate
        let info: std::collections::HashMap<i32, (&str, i32, i32, i32)> = horses
            .iter()
            .map(|h| (h.gate, (h.name.as_str(), h.order, h.style, h.pop)))
            .collect();

        let mut rows: Vec<(usize, f64)> =
            (0..sim_horses.len()).map(|i| (i, exp.win[i])).collect();
        rows.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap());

        eprintln!("\n=== РАЗБОР ГОНКИ: dist≈{distance:.0}м ground={ground} лошадей={} ===", sim_horses.len());
        eprintln!(
            "{:>4} {:<16} {:>2} {:>4} | {:>6} {:>6} {:>6} | {:>5}",
            "sim#", "name", "st", "fact", "floor", "EXP", "ceil", "Δпоз"
        );
        for (rank, (i, win)) in rows.iter().enumerate() {
            let g = sim_horses[*i].gate;
            let (name, order, style, _pop) = info[&g];
            let f = flo.win[*i] * 100.0;
            let e = win * 100.0;
            let c = cei.win[*i] * 100.0;
            eprintln!(
                "{:>4} {:<16} {:>2} {:>4} | {:>5.1}% {:>5.1}% {:>5.1}% | {:>+5.1}",
                rank + 1,
                &name[..name.len().min(16)],
                style,
                order,
                f,
                e,
                c,
                e - f, // зависимость от позиционки (expected − floor)
            );
        }
        // Винрейт-фавориты vs фактический топ-3
        let mut fact: Vec<(&str, i32)> = horses.iter().map(|h| (h.name.as_str(), h.order)).collect();
        fact.sort_by_key(|x| x.1);
        eprintln!("\nфактический топ-5: {:?}", &fact[..5.min(fact.len())]);
    }

    // ----- Корпусная калибровка: sim vs РЕАЛЬНЫЕ архивы гонок -----
    // Читает каталог архивов плагина (формат ArchiveRace: stats/apt/motiv/skills +
    // finish_order/finish_time + покадровые кривые), пере-симулирует каждую гонку и
    // считает СКАЛЯРНЫЕ метрики точности агрегатно по корпусу. Это измеримая база
    // калибровки — без неё «стало лучше/хуже» нельзя доказать.
    //
    // Запуск:
    //   cargo test --release analyze_archive_corpus -- --ignored --nocapture
    // Каталог по умолчанию: %LOCALAPPDATA%\uma_race_overlay_races (env RACE_DIR переопределяет).
    // Прогонов на гонку: env RUNS (деф. 2000). A/B по слоям — обычными тумблерами:
    //   UMA_SIM_NO_BLOCK=1 / UMA_SIM_NO_PK=1 / UMA_SIM_NO_WISVAR=1.

    #[derive(serde::Deserialize)]
    struct ArcHorse {
        gate: i32,
        #[serde(default)]
        name: String,
        #[serde(default)]
        style: i32,
        #[serde(default)]
        is_user: bool,
        #[serde(default)]
        stats: Vec<i32>,
        #[serde(default)]
        apt: Vec<i32>,
        #[serde(default)]
        finish_order: i32,
        #[serde(default)]
        motiv: i32,
        #[serde(default)]
        skills: Vec<(i32, i32)>,
    }

    #[derive(serde::Deserialize)]
    struct ArcRace {
        #[serde(default)]
        course_id: i32,
        #[serde(default)]
        distance: f32,
        #[serde(default)]
        ground: i32,
        #[serde(default)]
        race_type: i32,
        #[serde(default)]
        is_room_match: bool,
        #[serde(default)]
        horses: Vec<ArcHorse>,
    }

    /// Spearman ранговая корреляция между предсказанным и фактическим порядком.
    /// Оба входа — ранги 1..n (для предсказанного — место по убыванию win%).
    fn spearman(pred_rank: &[f64], fact_rank: &[f64]) -> f64 {
        let n = pred_rank.len();
        if n < 2 {
            return 1.0;
        }
        let d2: f64 = pred_rank
            .iter()
            .zip(fact_rank)
            .map(|(p, f)| (p - f) * (p - f))
            .sum();
        1.0 - 6.0 * d2 / (n as f64 * ((n * n - 1) as f64))
    }

    #[test]
    #[ignore]
    fn analyze_archive_corpus() {
        let dir = std::env::var("RACE_DIR").unwrap_or_else(|_| {
            let base = std::env::var("LOCALAPPDATA").unwrap_or_default();
            format!("{base}/uma_race_overlay_races")
        });
        let runs: u32 = std::env::var("RUNS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(2000);
        // CORPUS_NO_SKILLS=1 — выкинуть ВСЕ скиллы (изолировать ядро стат→скорость).
        let no_skills = std::env::var("CORPUS_NO_SKILLS").is_ok();
        let courses = crate::course::load_courses(std::path::Path::new("data/course_data.json"));
        let gd = gd();

        let mut files: Vec<std::path::PathBuf> = std::fs::read_dir(&dir)
            .unwrap_or_else(|e| panic!("read RACE_DIR={dir}: {e}"))
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().and_then(|s| s.to_str()) == Some("json"))
            .collect();
        files.sort();
        assert!(!files.is_empty(), "нет .json в {dir}");

        eprintln!("\n=== КОРПУСНАЯ КАЛИБРОВКА: {} файлов, runs={runs} ===", files.len());
        eprintln!(
            "block={} pk={} wisvar={}",
            block_env_enabled(),
            pk_enabled(),
            wisvar_enabled()
        );
        eprintln!(
            "{:<30} {:>3} {:>5} {:>6} {:>6} {:>6} | {:<14}",
            "file", "n", "top1", "spear", "brier", "plMAE", "actual winner"
        );

        let (mut sum_top1, mut sum_spear, mut sum_brier, mut sum_plmae) = (0.0, 0.0, 0.0, 0.0);
        let mut used = 0usize;
        // Brier по СВОЕЙ лошади (выиграет ли) — отдельный агрегат.
        let (mut sum_user_brier, mut user_n) = (0.0, 0usize);
        // Наивная база: ранг по сумме статов (без симуляции вообще).
        let (mut sum_naive_top1, mut sum_naive_spear) = (0.0, 0.0);
        // Разрез room (PvP) vs bots (доминирующая лошадь).
        let (mut room_top1, mut room_spear, mut room_n) = (0.0, 0.0, 0usize);
        let (mut bots_top1, mut bots_spear, mut bots_n) = (0.0, 0.0, 0usize);

        for path in &files {
            let text = match std::fs::read_to_string(path) {
                Ok(t) => t,
                Err(_) => continue,
            };
            let race: ArcRace = match serde_json::from_str(&text) {
                Ok(r) => r,
                Err(e) => {
                    eprintln!("{:<30} SKIP parse: {e}", short(path));
                    continue;
                }
            };
            // Только лошади с валидными статами.
            let valid: Vec<&ArcHorse> = race
                .horses
                .iter()
                .filter(|h| h.stats.len() >= 5 && h.finish_order >= 1)
                .collect();
            let n = valid.len();
            // Финиш-порядок должен быть полной перестановкой 1..=n (иначе DNF/битый дамп).
            let mut orders: Vec<i32> = valid.iter().map(|h| h.finish_order).collect();
            orders.sort();
            let full_perm = n >= 2 && orders == (1..=n as i32).collect::<Vec<_>>();
            if !full_perm {
                eprintln!("{:<30} SKIP: финиш-порядок не 1..{n} ({:?})", short(path), orders);
                continue;
            }

            let distance = courses
                .as_ref()
                .and_then(|m| m.get(&race.course_id))
                .map(|g| g.distance)
                .unwrap_or(race.distance as f64);
            let geom = courses.as_ref().and_then(|m| m.get(&race.course_id).cloned());
            let ground = if race.ground == 2 { 2 } else { 1 };
            let cat = match distance as i32 {
                ..=1400 => 0,
                1401..=1800 => 1,
                1801..=2400 => 2,
                _ => 3,
            };

            let sim_horses: Vec<SimHorse> = valid
                .iter()
                .map(|h| SimHorse {
                    gate: h.gate,
                    style: if (1..=4).contains(&h.style) { h.style } else { 2 },
                    speed: h.stats[0] as f64,
                    stamina: h.stats[1] as f64,
                    pow: h.stats[2] as f64,
                    guts: h.stats[3] as f64,
                    wiz: h.stats[4] as f64,
                    motivation: if (1..=5).contains(&h.motiv) { h.motiv } else { 3 },
                    // apt = [short, mile, middle, long, style]; surface-апт в архиве нет → A(7).
                    apt_dist: *h.apt.get(cat).filter(|&&v| (1..=8).contains(&v)).unwrap_or(&7),
                    apt_style: *h.apt.get(4).filter(|&&v| (1..=8).contains(&v)).unwrap_or(&7),
                    apt_ground: 7,
                    skills: if no_skills { Vec::new() } else { h.skills.clone() },
                })
                .collect();

            let race_params = RaceParams { distance, ground, condition: 1, course: geom };
            let r = simulate_mode(&gd, &race_params, &sim_horses, runs, 12345, PosMode::Expected);

            // Предсказанный ранг по убыванию win%.
            let mut by_win: Vec<usize> = (0..n).collect();
            by_win.sort_by(|&a, &b| r.win[b].partial_cmp(&r.win[a]).unwrap());
            let mut pred_rank = vec![0.0; n];
            for (rank, &i) in by_win.iter().enumerate() {
                pred_rank[i] = (rank + 1) as f64;
            }
            let fact_rank: Vec<f64> = valid.iter().map(|h| h.finish_order as f64).collect();

            // top-1: предсказанный фаворит == фактический победитель.
            let pred_winner = by_win[0];
            let top1 = if valid[pred_winner].finish_order == 1 { 1.0 } else { 0.0 };
            let spear = spearman(&pred_rank, &fact_rank);
            // Brier победы: y_i = (finish_order==1).
            let brier: f64 = (0..n)
                .map(|i| {
                    let y = if valid[i].finish_order == 1 { 1.0 } else { 0.0 };
                    (r.win[i] - y).powi(2)
                })
                .sum::<f64>()
                / n as f64;
            // Ошибка предсказанного места: |avg_place − finish_order|.
            let plmae: f64 = (0..n)
                .map(|i| (r.avg_place[i] - valid[i].finish_order as f64).abs())
                .sum::<f64>()
                / n as f64;

            // Наивная база: ранг по сумме 5 статов (никакой симуляции).
            let stat_sum: Vec<f64> = valid.iter().map(|h| h.stats[..5].iter().map(|&x| x as f64).sum()).collect();
            let mut by_stat: Vec<usize> = (0..n).collect();
            by_stat.sort_by(|&a, &b| stat_sum[b].partial_cmp(&stat_sum[a]).unwrap());
            let mut naive_rank = vec![0.0; n];
            for (rank, &i) in by_stat.iter().enumerate() {
                naive_rank[i] = (rank + 1) as f64;
            }
            let naive_top1 = if valid[by_stat[0]].finish_order == 1 { 1.0 } else { 0.0 };
            let naive_spear = spearman(&naive_rank, &fact_rank);

            sum_top1 += top1;
            sum_spear += spear;
            sum_brier += brier;
            sum_plmae += plmae;
            sum_naive_top1 += naive_top1;
            sum_naive_spear += naive_spear;
            used += 1;

            if race.is_room_match {
                room_top1 += top1;
                room_spear += spear;
                room_n += 1;
            } else {
                bots_top1 += top1;
                bots_spear += spear;
                bots_n += 1;
            }

            if let Some(ui) = (0..n).find(|&i| valid[i].is_user) {
                let y = if valid[ui].finish_order == 1 { 1.0 } else { 0.0 };
                sum_user_brier += (r.win[ui] - y).powi(2);
                user_n += 1;
            }

            let winner = valid.iter().find(|h| h.finish_order == 1).unwrap();
            let tag = if race.is_room_match { "room" } else { "bots" };
            eprintln!(
                "{:<30} {:>3} {:>5.0} {:>6.2} {:>6.3} {:>6.2} | {:<14} [{tag} rt{}]",
                short(path),
                n,
                top1,
                spear,
                brier,
                plmae,
                &winner.name[..winner.name.len().min(14)],
                race.race_type
            );
        }

        assert!(used > 0, "ни одной валидной гонки в корпусе");
        let u = used as f64;
        eprintln!("\n--- АГРЕГАТ по {used} гонкам ---");
        eprintln!("top-1 hit rate : {:.1}%  (фаворит sim = реальный победитель)", 100.0 * sum_top1 / u);
        eprintln!("Spearman ранга : {:.3}   (1.0 = идеальный порядок, 0 = шум)", sum_spear / u);
        eprintln!("Brier победы   : {:.4}  (ниже = точнее; меньше — лучше)", sum_brier / u);
        eprintln!("place MAE      : {:.3}  (ошибка предсказанного места, мест)", sum_plmae / u);
        if user_n > 0 {
            eprintln!("Brier СВОЕЙ    : {:.4}  ({user_n} гонок с твоей лошадью)", sum_user_brier / user_n as f64);
        }
        eprintln!("\n--- НАИВНАЯ БАЗА (ранг по сумме статов, без sim) ---");
        eprintln!("top-1 hit rate : {:.1}%   Spearman : {:.3}", 100.0 * sum_naive_top1 / u, sum_naive_spear / u);
        eprintln!("(если наивная Spearman ≥ sim — Monte-Carlo не добавляет ценности над статами)");
        eprintln!("\n--- РАЗРЕЗ ---");
        if bots_n > 0 {
            eprintln!("bots ({bots_n:>2}): top-1 {:>5.1}%  Spearman {:.3}", 100.0 * bots_top1 / bots_n as f64, bots_spear / bots_n as f64);
        }
        if room_n > 0 {
            eprintln!("room ({room_n:>2}): top-1 {:>5.1}%  Spearman {:.3}  (тесный PvP)", 100.0 * room_top1 / room_n as f64, room_spear / room_n as f64);
        }
    }

    fn short(p: &std::path::Path) -> String {
        p.file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("?")
            .replace("race_", "")
            .replace(".json", "")
    }

    // Детальный разбор ОДНОЙ гонки: sim vs факт по лошадям + сколько скилл-условий
    // решается «монеткой» (UNSUPPORTED_COND_P), а не реальным состоянием гонки.
    //   RACE_ONE=<подстрока имени> cargo test --release inspect_one_race -- --ignored --nocapture
    // Без RACE_ONE берётся первая room-гонка из RACE_DIR.
    #[test]
    #[ignore]
    fn inspect_one_race() {
        let dir = std::env::var("RACE_DIR").unwrap_or_else(|_| {
            let base = std::env::var("LOCALAPPDATA").unwrap_or_default();
            format!("{base}/uma_race_overlay_races")
        });
        let runs: u32 = std::env::var("RUNS").ok().and_then(|s| s.parse().ok()).unwrap_or(2000);
        let want = std::env::var("RACE_ONE").ok();
        let mut files: Vec<std::path::PathBuf> = std::fs::read_dir(&dir)
            .unwrap_or_else(|e| panic!("read RACE_DIR={dir}: {e}"))
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().and_then(|s| s.to_str()) == Some("json"))
            .collect();
        files.sort();
        let path = match &want {
            Some(w) => files.iter().find(|p| p.to_string_lossy().contains(w.as_str())).cloned().expect("RACE_ONE не найден"),
            None => files.iter().find(|p| p.to_string_lossy().contains("room")).cloned().unwrap_or_else(|| files[0].clone()),
        };
        let text = std::fs::read_to_string(&path).expect("read race");
        let race: ArcRace = serde_json::from_str(&text).expect("parse race");
        let courses = crate::course::load_courses(std::path::Path::new("data/course_data.json"));
        let gd = gd();

        let valid: Vec<&ArcHorse> = race.horses.iter().filter(|h| h.stats.len() >= 5 && h.finish_order >= 1).collect();
        let n = valid.len();
        let distance = courses.as_ref().and_then(|m| m.get(&race.course_id)).map(|g| g.distance).unwrap_or(race.distance as f64);
        let geom = courses.as_ref().and_then(|m| m.get(&race.course_id).cloned());
        let ground = if race.ground == 2 { 2 } else { 1 };
        let cat = match distance as i32 { ..=1400 => 0, 1401..=1800 => 1, 1801..=2400 => 2, _ => 3 };
        let sim_horses: Vec<SimHorse> = valid.iter().map(|h| SimHorse {
            gate: h.gate,
            style: if (1..=4).contains(&h.style) { h.style } else { 2 },
            speed: h.stats[0] as f64, stamina: h.stats[1] as f64, pow: h.stats[2] as f64,
            guts: h.stats[3] as f64, wiz: h.stats[4] as f64,
            motivation: if (1..=5).contains(&h.motiv) { h.motiv } else { 3 },
            apt_dist: *h.apt.get(cat).filter(|&&v| (1..=8).contains(&v)).unwrap_or(&7),
            apt_style: *h.apt.get(4).filter(|&&v| (1..=8).contains(&v)).unwrap_or(&7),
            apt_ground: 7,
            skills: h.skills.clone(),
        }).collect();
        let race_params = RaceParams { distance, ground, condition: 1, course: geom };
        let r = simulate_mode(&gd, &race_params, &sim_horses, runs, 12345, PosMode::Expected);

        // классификация условия: 0 supported, 1 random-window, 2 «монетка» (неподдержано/немоделируемая позиционка)
        let z = zero_ctx();
        let classify = |c: &Cond| -> u8 {
            if c.var.ends_with("_random") { return 1; }
            if is_positional(&c.var) {
                return if positional_value(&z, &c.var).is_some() { 0 } else { 2 };
            }
            if cond_var(&z, &c.var).is_some() { 0 } else { 2 }
        };

        eprintln!("\n=== РАЗБОР ОДНОЙ ГОНКИ: {} (dist≈{distance:.0}, n={n}, runs={runs}) ===", short(&path));
        eprintln!("{:<14} {:>4} {:>6} {:>6} {:>6} | {:>5} {:>6} {:>5} {:>5} {:>10}",
            "name", "fact", "win%", "avgpl", "Σstat", "sk", "nodef", "rEff", "cond", "coin%");
        let (mut tot_cond, mut tot_coin) = (0usize, 0usize);
        // ранжировать вывод по факт-месту
        let mut idx: Vec<usize> = (0..n).collect();
        idx.sort_by_key(|&i| valid[i].finish_order);
        for &i in &idx {
            let h = valid[i];
            let statsum: i32 = h.stats[..5].iter().sum();
            let (mut n_reff, mut conds, mut coin, mut nodef) = (0usize, 0usize, 0usize, 0usize);
            for (id, _lvl) in &h.skills {
                let Some(def) = gd.skills.get(id) else { nodef += 1; continue };
                for v in &def.variants {
                    if !v.effects.iter().any(|e| matches!(e.ability_type, 9 | 21 | 27 | 31)) { continue; }
                    n_reff += 1;
                    for tree in [&v.precondition, &v.condition] {
                        for group in tree.iter() {
                            for c in group.iter() {
                                conds += 1;
                                if classify(c) == 2 { coin += 1; }
                            }
                        }
                    }
                }
            }
            tot_cond += conds; tot_coin += coin;
            let coin_pct = if conds > 0 { 100.0 * coin as f64 / conds as f64 } else { 0.0 };
            eprintln!("{:<14} {:>4} {:>5.1}% {:>6.2} {:>6} | {:>5} {:>6} {:>5} {:>5} {:>9.0}%",
                &h.name[..h.name.len().min(14)], h.finish_order, r.win[i] * 100.0, r.avg_place[i],
                statsum, h.skills.len(), nodef, n_reff, conds, coin_pct);
        }
        let coin_pct = if tot_cond > 0 { 100.0 * tot_coin as f64 / tot_cond as f64 } else { 0.0 };
        eprintln!("\nИТОГО race-effect условий: {tot_cond}, из них «монеткой» {tot_coin} ({coin_pct:.0}%)");
        eprintln!("(высокая доля coin% = скиллы палят случайно, отвязанно от реального хода гонки → H2)");
    }

    // Бенч (по умолчанию пропущен): `cargo test --release bench_3000 -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn bench_3000() {
        let gd = gd();
        let race = RaceParams { distance: 2000.0, ground: 1, condition: 1, course: None };
        let horses: Vec<SimHorse> = (1..=9)
            .map(|g| {
                let mut h = horse(1000.0, 800.0, 900.0, 400.0, 800.0, ((g % 4) + 1) as i32);
                h.gate = g as i32;
                h.skills = vec![(100231, 3), (200142, 5), (202051, 5)];
                h
            })
            .collect();
        for w in [1usize, recommended_workers()] {
            let t = std::time::Instant::now();
            let _ = simulate_with_workers(&gd, &race, &horses, 3000, 42, w, PosMode::Expected, true);
            eprintln!("workers={w}: 3000 прогонов 9 лошадей за {:?}", t.elapsed());
        }
    }

    #[test]
    fn position_keep_runs_and_sums() {
        // Поле с несколькими передовыми: позиционка не ломает симуляцию, сумма
        // винрейтов ≈ 1 (ровно одна победа на забег).
        let gd = gd();
        let race = RaceParams { distance: 2400.0, ground: 1, condition: 1, course: None };
        let horses = vec![
            horse(1050.0, 700.0, 950.0, 400.0, 800.0, 1),
            horse(1050.0, 750.0, 920.0, 400.0, 800.0, 1),
            horse(1000.0, 850.0, 900.0, 420.0, 850.0, 2),
            horse(1000.0, 900.0, 880.0, 450.0, 850.0, 4),
        ];
        let r = simulate(&gd, &race, &horses, 300, 5);
        let sum: f64 = r.win.iter().sum();
        assert!((sum - 1.0).abs() < 1e-6, "sum={sum}");
    }
}
