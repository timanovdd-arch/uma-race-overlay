//! Окно-реплей завершённой гонки: стилизованный трек-овал, где и какой скилл
//! сработал у каждой девочки (цвет = девочка), ползунок времени, который двигает
//! лошадей по треку и показывает их статы в этот момент.
//!
//! Источник — последний архив гонки (`%LOCALAPPDATA%\uma_race_overlay_races\`),
//! который пишет внутриигровой плагин (см. uma-race-overlay/src/archive.rs):
//! покадровый таймлайн d/v/hp/lane/block/temp + надетые скиллы.
//!
//! Тайминг скиллов: в архиве есть НАДЕТЫЕ скиллы, но не «когда сработал». Поэтому
//! по реальным кривым v/hp детектируем события (рывок ускорения / скачок HP) и
//! привязываем к наиболее подходящему надетому скиллу по его условиям и типу
//! эффекта (master.mdb). Когда плагин начнёт писать настоящие события игры
//! (`_simEvDataList`, поле `act`) — берём их (real), иначе эвристику (≈ inferred).

use std::collections::HashMap;
use std::path::PathBuf;

use egui::{pos2, Align2, Color32, FontId, Pos2, Rect, Sense, Stroke};
use serde::Deserialize;

use crate::course::CourseGeom;
use crate::gamedata::{CondOp, CondTree, GameData, SkillDef};

/// Палитра стабильных цветов девочек (по месту финиша). До 12 различимых.
const PALETTE: [(u8, u8, u8); 12] = [
    (255, 215, 0),   // золото — 1 место
    (80, 200, 255),  // голубой — 2
    (255, 90, 160),  // розовый — 3
    (120, 230, 140), // зелёный
    (255, 150, 50),  // оранжевый
    (180, 130, 255), // фиолетовый
    (240, 240, 120), // лимонный
    (0, 200, 200),   // бирюзовый
    (255, 120, 120), // коралл
    (150, 200, 90),  // оливковый
    (200, 160, 255), // лавандовый
    (170, 170, 170), // серый
];

fn palette(i: usize) -> Color32 {
    let (r, g, b) = PALETTE[i % PALETTE.len()];
    Color32::from_rgb(r, g, b)
}

/// Стиль бега человекочитаемо.
fn style_name(style: i32) -> &'static str {
    match style {
        1 => "Runner (nige)",
        2 => "Leader (senko)",
        3 => "Betweener (sashi)",
        4 => "Chaser (oikomi)",
        _ => "?",
    }
}

// ---------------------------------------------------------------------------
// Spot Struggle (位置取り争い): фронт-раннеры рубятся за позицию в начале гонки.
// Правило движка (docs/правила_гонки.txt): контест активен, когда 2+ Front Runner
// (nige) идут в пределах 3.75 м друг от друга, в окне от 150 м после старта примерно
// до середины Middle leg (~5/12 дистанции). Runaway (大逃げ) — 5 м, но отдельным
// стилем в архиве не помечен, поэтому считаем по nige.
//
// ВАЖНО: движок НЕ отдаёт спот-страгл отдельным полем кадра — раскладка
// RaceSimulateHorseFrameData обрывается на BlockFrontHorseIndex(+0x21) (см.
// uma-race-overlay/src/frames.rs и docs/БЛОК-АНАЛИЗ). Поэтому, в отличие от блока
// (граунд-трус), спот-страгл ВЫВОДИТСЯ по правилам из style+дистанции (≈ derived).
// ---------------------------------------------------------------------------

/// Стиль «Front Runner» (逃げ, nige) — единственный, кто участвует в Spot Struggle.
const STYLE_NIGE: i32 = 1;
/// Макс. дистанция между фронт-раннерами для контеста (м).
const STRUGGLE_DIST: f32 = 3.75;
/// Контест не раньше этой дистанции от старта (м).
const STRUGGLE_START_M: f32 = 150.0;
/// Контест авто-гаснет около середины Middle leg (доля дистанции).
const STRUGGLE_END_FRAC: f32 = 5.0 / 12.0;

// ---------------------------------------------------------------------------
// Разбор архива гонки.
// ---------------------------------------------------------------------------

/// Настоящее событие активации скилла из игры (Phase B, поле `act` архива).
/// Пока плагин его не пишет — массив пуст и используется эвристика.
#[derive(Deserialize, Clone, Default)]
pub struct ActEvent {
    /// Индекс кадра в `frame_t`.
    #[serde(default)]
    pub f: usize,
    /// Дистанция (м) на момент срабатывания.
    #[serde(default)]
    pub d: f32,
    /// skill_id.
    #[serde(default)]
    pub id: i32,
}

#[derive(Deserialize, Clone, Default)]
pub struct ArchiveHorse {
    pub gate: i32,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub trainer: String,
    #[serde(default)]
    pub is_user: bool,
    #[serde(default)]
    pub style: i32,
    #[serde(default)]
    pub finish_order: i32,
    #[serde(default)]
    pub skills: Vec<(i32, i32)>,
    #[serde(default)]
    pub d: Vec<f32>,
    #[serde(default)]
    pub v: Vec<f32>,
    #[serde(default)]
    pub hp: Vec<f32>,
    /// Позиция поперёк поля (LanePosition) покадрово — рисуется в нижней полосе
    /// «Field» и используется для блока/спот-страгла.
    #[serde(default)]
    pub lane: Vec<f32>,
    /// `BlockFrontHorseIndex` покадрово: индекс лошади спереди, что блокирует
    /// (0xFF = свободна). Индекс совпадает с индексом в `horses` (оба по gate).
    #[serde(default)]
    pub block: Vec<u8>,
    /// Режим закидывания (掛かり) покадрово — часть схемы архива, пока не рисуется.
    #[serde(default)]
    #[allow(dead_code)]
    pub temp: Vec<u8>,
    /// Настоящие события активации (Phase B). Пусто → эвристика.
    #[serde(default)]
    pub act: Vec<ActEvent>,
    /// Старт-события Spot Struggle (位置取り争い) этой лошади — GROUND TRUTH из
    /// движка (event type 4). Пусто → нет данных (старый архив) → эвристика по
    /// правилам. `id` у ActEvent тут не используется.
    #[serde(default)]
    pub struggle: Vec<ActEvent>,
    /// Старт-события Dueling (追い比べ, дуэль на финишной прямой) — GROUND TRUTH из
    /// движка (event type 5). Только реальные события (правила-фолбэка нет).
    #[serde(default)]
    pub duel: Vec<ActEvent>,
}

#[derive(Deserialize, Clone, Default)]
pub struct ArchiveRace {
    #[serde(default)]
    pub course_id: i32,
    #[serde(default)]
    pub distance: f32,
    /// Поверхность (1 турф / 2 грунт) — часть схемы архива, пока не используется.
    #[serde(default)]
    #[allow(dead_code)]
    pub ground: i32,
    #[serde(default)]
    pub frame_t: Vec<f32>,
    #[serde(default)]
    pub horses: Vec<ArchiveHorse>,
}

impl ArchiveRace {
    /// Длина трассы (м): из метаданных, иначе максимум по пройденной дистанции.
    pub fn total_distance(&self) -> f32 {
        if self.distance > 1.0 {
            return self.distance;
        }
        self.horses
            .iter()
            .flat_map(|h| h.d.iter().copied())
            .filter(|x| x.is_finite())
            .fold(0.0_f32, f32::max)
            .max(1.0)
    }

}

fn races_dir() -> PathBuf {
    let base = std::env::var("LOCALAPPDATA").unwrap_or_else(|_| ".".into());
    PathBuf::from(base).join("uma_race_overlay_races")
}

/// Прочитать самый свежий архив гонки. None — папки/файлов нет или не распарсилось.
pub fn load_latest() -> Option<ArchiveRace> {
    let dir = races_dir();
    let mut newest: Option<(std::time::SystemTime, PathBuf)> = None;
    for entry in std::fs::read_dir(&dir).ok()? {
        let Ok(e) = entry else { continue };
        let p = e.path();
        let is_race = p
            .file_name()
            .and_then(|n| n.to_str())
            .map(|n| n.starts_with("race_") && n.ends_with(".json"))
            .unwrap_or(false);
        if !is_race {
            continue;
        }
        let Ok(meta) = e.metadata() else { continue };
        let Ok(mtime) = meta.modified() else { continue };
        if newest.as_ref().is_none_or(|(t, _)| mtime > *t) {
            newest = Some((mtime, p));
        }
    }
    let (_, path) = newest?;
    let text = std::fs::read_to_string(&path).ok()?;
    serde_json::from_str(&text).ok()
}

// ---------------------------------------------------------------------------
// Привязка скиллов: тип эффекта + проверка условий по фазе/остатку дистанции.
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ActKind {
    /// Скорость/ускорение (на себя).
    Speed,
    /// Восстановление HP (стамины).
    Recovery,
    /// Дебафф соперникам.
    Debuff,
}

#[derive(Clone)]
pub struct Activation {
    pub frame_idx: usize,
    pub distance: f32,
    pub skill_id: i32,
    pub kind: ActKind,
    /// Настоящее событие из игры (true) или эвристика по кривым (false).
    pub real: bool,
    /// 0..1 — уверенность привязки (для эвристики). У real = 1.
    pub confidence: f32,
}

/// На что способен скилл (по эффектам всех вариантов).
struct SkillCaps {
    speed: bool,
    recovery: bool,
}

fn skill_caps(def: &SkillDef) -> SkillCaps {
    let mut speed = false;
    let mut recovery = false;
    for v in &def.variants {
        for e in &v.effects {
            if e.target_type == 1 {
                if e.ability_type == 9 {
                    recovery = true;
                } else {
                    // 27 current speed, 31 accel и прочие позитивные на себя.
                    speed = true;
                }
            }
        }
    }
    SkillCaps { speed, recovery }
}

/// Фаза гонки по прогрессу (как в движке: границы 1/6, 2/3, 5/6).
fn phase_of(progress: f32) -> i32 {
    if progress < 1.0 / 6.0 {
        0
    } else if progress < 2.0 / 3.0 {
        1
    } else if progress < 5.0 / 6.0 {
        2
    } else {
        3
    }
}

/// Контекст состояния лошади на момент события (для проверки DSL-условий).
struct CondCtx {
    phase: i32,
    remain: f32,
    hp_per: f32,
}

/// Значение известной переменной DSL. None — переменную не умеем считать
/// (тогда условие игнорируется, чтобы не отбраковывать лишнего).
fn known_var(var: &str, ctx: &CondCtx) -> Option<f64> {
    match var {
        "phase" => Some(ctx.phase as f64),
        "remain_distance" => Some(ctx.remain as f64),
        "hp_per" => Some(ctx.hp_per as f64),
        _ => None,
    }
}

fn cmp_op(a: f64, op: CondOp, b: f64) -> bool {
    match op {
        CondOp::Eq => (a - b).abs() < 1e-6,
        CondOp::Ne => (a - b).abs() >= 1e-6,
        CondOp::Ge => a >= b,
        CondOp::Le => a <= b,
        CondOp::Gt => a > b,
        CondOp::Lt => a < b,
    }
}

/// Оценка дерева условий: None — ни одна ИЛИ-группа не прошла по ИЗВЕСТНЫМ
/// переменным; Some(score) — прошла, score = число совпавших известных условий
/// (выше = специфичнее совпадение). Пустое дерево = Some(0) (всегда истинно).
fn eval_tree(tree: &CondTree, ctx: &CondCtx) -> Option<u32> {
    if tree.is_empty() {
        return Some(0);
    }
    let mut best: Option<u32> = None;
    for group in tree {
        let mut ok = true;
        let mut score = 0u32;
        for c in group {
            if let Some(v) = known_var(&c.var, ctx) {
                if cmp_op(v, c.op, c.val) {
                    score += 1;
                } else {
                    ok = false;
                    break;
                }
            }
        }
        if ok {
            best = Some(best.map_or(score, |b| b.max(score)));
        }
    }
    best
}

/// Лучшая оценка соответствия скилла состоянию + минимальный кулдаун (сек).
/// None — скилл по условиям сейчас сработать не мог.
fn match_skill(def: &SkillDef, ctx: &CondCtx) -> Option<(u32, f64)> {
    let mut best: Option<(u32, f64)> = None;
    for v in &def.variants {
        let pre = eval_tree(&v.precondition, ctx);
        let cond = eval_tree(&v.condition, ctx);
        if let (Some(a), Some(b)) = (pre, cond) {
            let score = a + b;
            best = Some(match best {
                Some((bs, bcd)) if bs >= score => (bs, bcd),
                _ => (score, v.cooldown_s),
            });
        }
    }
    best
}

/// Детектирование событий + привязка к надетым скиллам по реальным кривым.
/// Возвращает по индексам `race.horses`.
pub fn infer(race: &ArchiveRace, gd: &GameData) -> Vec<Vec<Activation>> {
    let total = race.total_distance();
    race.horses.iter().map(|h| infer_horse(h, race, total, gd)).collect()
}

fn infer_horse(h: &ArchiveHorse, race: &ArchiveRace, total: f32, gd: &GameData) -> Vec<Activation> {
    // Если есть настоящие события (Phase B) — берём их, эвристику не зовём.
    if !h.act.is_empty() {
        return h
            .act
            .iter()
            .map(|e| {
                let kind = gd
                    .skills
                    .get(&e.id)
                    .map(|d| {
                        let c = skill_caps(d);
                        if c.recovery && !c.speed {
                            ActKind::Recovery
                        } else if c.speed {
                            ActKind::Speed
                        } else {
                            ActKind::Debuff
                        }
                    })
                    .unwrap_or(ActKind::Speed);
                Activation { frame_idx: e.f, distance: e.d, skill_id: e.id, kind, real: true, confidence: 1.0 }
            })
            .collect();
    }

    let n = h.d.len().min(h.v.len()).min(h.hp.len()).min(race.frame_t.len());
    if n < 3 {
        return Vec::new();
    }
    let t = &race.frame_t;
    let hp_max = h.hp.iter().copied().filter(|x| x.is_finite()).fold(1.0_f32, f32::max);

    // Ускорение по кадрам.
    let mut accel = vec![0.0_f32; n];
    for i in 1..n {
        let dt = (t[i] - t[i - 1]).max(1e-3);
        if h.v[i].is_finite() && h.v[i - 1].is_finite() {
            accel[i] = (h.v[i] - h.v[i - 1]) / dt;
        }
    }

    // Скиллы лошади с их возможностями (для фильтра по типу события).
    let equipped: Vec<(&SkillDef, SkillCaps)> = h
        .skills
        .iter()
        .filter_map(|(id, _)| gd.skills.get(id).map(|d| (d, skill_caps(d))))
        .collect();

    // Кулдаун-таймштампы по skill_id (последняя привязка).
    let mut last_used: HashMap<i32, f32> = HashMap::new();
    let mut out: Vec<Activation> = Vec::new();

    // --- события восстановления HP: HP обычно только падает, рост = recovery ---
    let mut i = 1;
    while i < n {
        let dhp = h.hp[i] - h.hp[i - 1];
        if dhp > 12.0 && h.hp[i].is_finite() && h.hp[i - 1].is_finite() {
            // слить подряд идущие растущие кадры в одно событие
            let mut j = i;
            while j + 1 < n && h.hp[j + 1] - h.hp[j] > 4.0 {
                j += 1;
            }
            let fi = i; // начало роста
            if let Some(a) = attribute(h, race, total, fi, ActKind::Recovery, &equipped, &mut last_used, hp_max) {
                out.push(a);
            }
            i = j + 1;
        } else {
            i += 1;
        }
    }

    // --- события скорости: рывок ускорения над локальной базой ---
    // База — медиана ускорения в окне ±W (убирает плавный профиль фазы).
    const W: usize = 4;
    let mut i = 2;
    while i < n - 1 {
        let progress = h.d[i] / total;
        // Пропускаем старт (мощный разгон с нуля) и финиш.
        if progress > 0.07 && progress < 0.995 && h.v[i] > 4.0 {
            let lo = i.saturating_sub(W);
            let hi = (i + W).min(n - 1);
            let mut win: Vec<f32> = accel[lo..=hi].to_vec();
            win.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
            let base = win[win.len() / 2];
            let excess = accel[i] - base;
            if excess > 0.22 && accel[i] > 0.12 {
                // локальный максимум рывка — берём вершину
                let mut peak = i;
                while peak + 1 < n - 1 && accel[peak + 1] > accel[peak] {
                    peak += 1;
                }
                if let Some(a) = attribute(h, race, total, peak, ActKind::Speed, &equipped, &mut last_used, hp_max) {
                    out.push(a);
                }
                i = peak + 2; // не дублировать тот же рывок
                continue;
            }
        }
        i += 1;
    }

    out.sort_by_key(|a| a.frame_idx);
    out
}

/// Привязать событие в кадре `fi` к лучшему подходящему надетому скиллу.
#[allow(clippy::too_many_arguments)]
fn attribute(
    h: &ArchiveHorse,
    race: &ArchiveRace,
    total: f32,
    fi: usize,
    kind: ActKind,
    equipped: &[(&SkillDef, SkillCaps)],
    last_used: &mut HashMap<i32, f32>,
    hp_max: f32,
) -> Option<Activation> {
    let dist = h.d[fi];
    let time = race.frame_t.get(fi).copied().unwrap_or(0.0);
    let progress = (dist / total).clamp(0.0, 1.0);
    let ctx = CondCtx {
        phase: phase_of(progress),
        remain: total - dist,
        hp_per: (h.hp.get(fi).copied().unwrap_or(0.0) / hp_max * 100.0).clamp(0.0, 100.0),
    };

    // Кандидаты нужного типа, не на кулдауне.
    let mut best: Option<(u32, bool, i32)> = None; // (score, is_unique, id)
    for (def, caps) in equipped {
        let type_ok = match kind {
            ActKind::Recovery => caps.recovery,
            ActKind::Speed => caps.speed,
            ActKind::Debuff => false,
        };
        if !type_ok {
            continue;
        }
        let Some((score, cd)) = match_skill(def, &ctx) else { continue };
        if let Some(&last) = last_used.get(&def.id) {
            if time - last < cd.max(2.0) as f32 {
                continue; // ещё на кулдауне
            }
        }
        let better = match best {
            None => true,
            Some((bs, bu, _)) => score > bs || (score == bs && def.is_unique && !bu),
        };
        if better {
            best = Some((score, def.is_unique, def.id));
        }
    }

    let (score, _, id) = best?;
    last_used.insert(id, time);
    // Уверенность: совпали известные условия → выше; пустое совпадение → ниже.
    let confidence = if score >= 2 {
        0.9
    } else if score == 1 {
        0.65
    } else {
        0.4
    };
    Some(Activation { frame_idx: fi, distance: dist, skill_id: id, kind, real: false, confidence })
}

// ---------------------------------------------------------------------------
// Состояние окна-реплея + рендер горизонтального графика (умалатор-стиль):
// X = дистанция, кривые скорости/HP, фоновые полосы фаз и рельефа, маркеры
// скиллов на их дистанции, точки лошадей в текущем кадре (плейхед).
// ---------------------------------------------------------------------------

/// Округление вверх до кратного `step` (для «красивого» максимума оси).
fn nice_ceil(v: f32, step: f32) -> f32 {
    if step <= 0.0 {
        return v;
    }
    (v / step).ceil() * step
}

/// Короткая подпись скилла (длинные имена режем, чтобы не наезжали).
fn short_name(name: &str) -> String {
    const MAX: usize = 14;
    if name.chars().count() <= MAX {
        name.to_string()
    } else {
        let cut: String = name.chars().take(MAX - 1).collect();
        format!("{cut}…")
    }
}

/// Ячейка контеста в таблице статов: «yes (with X)» (+N если соперников больше)
/// цветом `col`, либо тусклое «no». Имя соперника — ближайший партнёр (partners[0]).
fn contest_cell(ui: &mut egui::Ui, partners: &[usize], horses: &[ArchiveHorse], col: Color32) {
    match partners.first() {
        Some(&pj) => {
            let more = if partners.len() > 1 {
                format!(" +{}", partners.len() - 1)
            } else {
                String::new()
            };
            let txt = format!("yes (with {}{})", short_name(&horses[pj].name), more);
            ui.label(egui::RichText::new(txt).color(col).strong());
        }
        None => {
            ui.label(egui::RichText::new("no").weak());
        }
    }
}

/// Вертикальные зоны графика (px): полоса фаз сверху, зона кривых (фиксированная,
/// чтобы спайки скорости читались), затем зона подписей скиллов (высота = число
/// упакованных строк × ROW_H, поэтому скиллы 3 лошадей никогда не накладываются),
/// и ось дистанции снизу. Поля слева (ось скорости) / справа.
const PHASE_H: f32 = 26.0;
const CURVE_H: f32 = 190.0;
const ROW_H: f32 = 17.0;
const AXIS_H: f32 = 18.0;
const MARG_L: f32 = 34.0;
const MARG_R: f32 = 8.0;
/// Нижняя полоса «Field» (смещение по lane вдоль дистанции) и зазор над ней.
const FIELD_H: f32 = 88.0;
const FIELD_GAP: f32 = 8.0;

/// Одна подпись скилла на графике: дистанция (X), ширина, текст, цвет, лошадь и
/// кадр (для точки на кривой), назначенная строка упаковки.
struct LabelItem {
    dist: f32,
    w: f32,
    text: String,
    col: Color32,
    hi: usize,
    frame: usize,
    row: usize,
}

/// Эпизод контеста (ground truth): кадры [start..=end] и партнёры (Spot Struggle / Dueling).
struct ContestEp {
    start: usize,
    end: usize,
    partners: Vec<usize>,
}

/// Сепарация (м), при которой реальный контест считаем завершённым (немного больше
/// порога старта 3.75 — гистерезис, контест держится чуть после расхождения).
const CONTEST_SEP_DIST: f32 = 4.5;

/// По старт-событиям движка (`pick(h)` = `struggle` либо `duel`) строит эпизоды
/// контеста на лошадь: партнёры = участники со старт-событием в том же кадре (±1);
/// конец — пока лошадь держится в пределах CONTEST_SEP_DIST хотя бы от одного
/// партнёра. Пусто на всех → реальных событий нет (старый архив).
fn build_contest_eps(
    horses: &[ArchiveHorse],
    pick: impl Fn(&ArchiveHorse) -> &[ActEvent],
) -> Vec<Vec<ContestEp>> {
    // Все старты: (лошадь, кадр).
    let starts: Vec<(usize, usize)> = horses
        .iter()
        .enumerate()
        .flat_map(|(hi, h)| pick(h).iter().map(move |e| (hi, e.f)))
        .collect();
    let mut out: Vec<Vec<ContestEp>> = (0..horses.len()).map(|_| Vec::new()).collect();
    for &(hi, fs) in &starts {
        let partners: Vec<usize> = starts
            .iter()
            .filter(|&&(hj, fj)| hj != hi && fj.abs_diff(fs) <= 1)
            .map(|&(hj, _)| hj)
            .collect();
        if partners.is_empty() {
            continue;
        }
        // Конец эпизода: вперёд от старта, пока в пределах SEP от любого партнёра.
        let di = &horses[hi].d;
        let mut end = fs;
        let n = di.len();
        for f in fs..n {
            let Some(&dh) = di.get(f).filter(|x| x.is_finite()) else { break };
            let near = partners.iter().any(|&pj| {
                horses[pj].d.get(f).is_some_and(|&dp| dp.is_finite() && (dh - dp).abs() <= CONTEST_SEP_DIST)
            });
            if near {
                end = f;
            } else {
                break;
            }
        }
        out[hi].push(ContestEp { start: fs, end, partners });
    }
    out
}

/// Партнёры по контесту для `hi` в кадре `f` из заранее построенных эпизодов.
fn eps_partners_at(eps: &[Vec<ContestEp>], hi: usize, f: usize) -> Vec<usize> {
    for ep in &eps[hi] {
        if f >= ep.start && f <= ep.end {
            return ep.partners.clone();
        }
    }
    Vec::new()
}

/// Состояние окна-реплея: загруженная гонка + привязки + выбор девочек + кадр.
pub struct ReplayState {
    pub race: ArchiveRace,
    /// Привязки скиллов по индексам `race.horses`.
    pub activations: Vec<Vec<Activation>>,
    /// Порядок отображения (индексы horses), отсортированы по месту финиша.
    order: Vec<usize>,
    /// Цвет по индексу horses.
    color: Vec<Color32>,
    /// Какие девочки показаны (индексы horses).
    shown: Vec<bool>,
    pub frame: usize,
    /// Хотя бы у одной лошади есть настоящие события игры (Phase B).
    real_source: bool,
    /// Тоглы графика (как в умалаторе).
    show_hp: bool,
    show_gap: bool,
    show_labels: bool,
    /// Показывать нижнюю полосу «Field» (смещение лошадей по полю).
    show_field: bool,
    /// Эпизоды Spot Struggle по индексам `race.horses` (ground truth из событий).
    struggle_eps: Vec<Vec<ContestEp>>,
    /// Есть реальные старт-события спот-страгла → используем их, иначе правило (≈).
    real_struggle: bool,
    /// Эпизоды Dueling по индексам `race.horses` (только ground truth, фолбэка нет).
    duel_eps: Vec<Vec<ContestEp>>,
    /// Есть реальные старт-события дуэли.
    real_duel: bool,
}

impl ReplayState {
    pub fn new(race: ArchiveRace, gd: Option<&GameData>) -> Self {
        let activations = match gd {
            Some(g) => infer(&race, g),
            None => vec![Vec::new(); race.horses.len()],
        };
        let real_source = activations.iter().flatten().any(|a| a.real);
        let struggle_eps = build_contest_eps(&race.horses, |h| &h.struggle);
        let real_struggle = struggle_eps.iter().any(|v| !v.is_empty());
        let duel_eps = build_contest_eps(&race.horses, |h| &h.duel);
        let real_duel = duel_eps.iter().any(|v| !v.is_empty());

        // Порядок по месту финиша (0 = не финишировал → в конец).
        let mut order: Vec<usize> = (0..race.horses.len()).collect();
        order.sort_by_key(|&i| {
            let o = race.horses[i].finish_order;
            if o <= 0 {
                i32::MAX
            } else {
                o
            }
        });
        // Цвет назначаем по позиции в order (1 место = золото и т.д.).
        let mut color = vec![Color32::GRAY; race.horses.len()];
        for (rank, &hi) in order.iter().enumerate() {
            color[hi] = palette(rank);
        }
        // По умолчанию показаны топ-3.
        let mut shown = vec![false; race.horses.len()];
        for &hi in order.iter().take(3) {
            shown[hi] = true;
        }

        Self {
            race,
            activations,
            order,
            color,
            shown,
            frame: 0,
            real_source,
            show_hp: true,
            show_gap: false,
            show_labels: true,
            show_field: true,
            struggle_eps,
            real_struggle,
            duel_eps,
            real_duel,
        }
    }

    fn n_frames(&self) -> usize {
        self.race.frame_t.len().max(1)
    }

    /// Кто блокирует лошадь `hi` в кадре `f` (индекс в `race.horses`) — напрямую из
    /// движкового `BlockFrontHorseIndex`. None — свободна. Это ГРАУНД-ТРУС (не эвристика).
    fn blocker_at(&self, hi: usize, f: usize) -> Option<usize> {
        let b = *self.race.horses[hi].block.get(f)?;
        if b == 0xFF {
            return None;
        }
        let bi = b as usize;
        (bi < self.race.horses.len() && bi != hi).then_some(bi)
    }

    /// Соперники по Spot Struggle для `hi` в кадре `f`. Если в архиве есть РЕАЛЬНЫЕ
    /// старт-события движка (`real_struggle`) — берём их (ground truth), иначе
    /// выводим по правилам игры (≈, см. `struggle_partners_rule`). Пусто — не в контесте.
    fn struggle_partners(&self, hi: usize, f: usize, total: f32) -> Vec<usize> {
        if self.real_struggle {
            return eps_partners_at(&self.struggle_eps, hi, f);
        }
        self.struggle_partners_rule(hi, f, total)
    }

    /// Соперники по Dueling (追い比べ) для `hi` в кадре `f`. Только ground truth из
    /// событий движка — правила-фолбэка для дуэли нет (зависит от скоростей в финишной
    /// прямой, по правилам не воспроизводится).
    fn duel_partners(&self, hi: usize, f: usize) -> Vec<usize> {
        if self.real_duel {
            eps_partners_at(&self.duel_eps, hi, f)
        } else {
            Vec::new()
        }
    }

    /// Правило-фолбэк (≈) для архивов без реальных событий: спот-страгл — это контест
    /// ДВУХ ПЕРЕДНИХ фронт-раннеров (как и делает движок: триггерятся именно лидеры,
    /// а не любая пара рядом). Возвращает второго из пары, если `hi` — один из лидеров.
    fn struggle_partners_rule(&self, hi: usize, f: usize, total: f32) -> Vec<usize> {
        let h = &self.race.horses[hi];
        if h.style != STYLE_NIGE {
            return Vec::new();
        }
        let Some(&di) = h.d.get(f) else { return Vec::new() };
        if !di.is_finite() || di < STRUGGLE_START_M || di / total > STRUGGLE_END_FRAC {
            return Vec::new();
        }
        // Два передних nige по дистанции в этом кадре.
        let mut nige: Vec<(usize, f32)> = self
            .race
            .horses
            .iter()
            .enumerate()
            .filter(|(_, o)| o.style == STYLE_NIGE)
            .filter_map(|(j, o)| o.d.get(f).copied().filter(|x| x.is_finite()).map(|d| (j, d)))
            .collect();
        nige.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        if nige.len() < 2 || (nige[0].1 - nige[1].1).abs() >= STRUGGLE_DIST {
            return Vec::new();
        }
        let (a, b) = (nige[0].0, nige[1].0);
        if hi == a {
            vec![b]
        } else if hi == b {
            vec![a]
        } else {
            Vec::new()
        }
    }

    /// Главный UI окна: горизонтальный график (X = дистанция) + тоглы, ползунок
    /// времени, легенда и живые статы. `geom` — геометрия курса (полосы рельефа).
    pub fn ui(&mut self, ui: &mut egui::Ui, gd: Option<&GameData>, geom: Option<&CourseGeom>) {
        let nf = self.n_frames();
        if self.frame >= nf {
            self.frame = nf - 1;
        }

        let t_now = self.race.frame_t.get(self.frame).copied().unwrap_or(0.0);
        let t_end = self.race.frame_t.last().copied().unwrap_or(0.0);

        // --- шапка: заголовок + источник + тоглы ---
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("Race map").strong());
            if self.real_source {
                ui.label(egui::RichText::new("● real events").color(crate::theme::C_GOOD).small());
            } else {
                ui.label(egui::RichText::new("≈ inferred from curves").weak().small())
                    .on_hover_text(
                        "Skill markers are estimated from the recorded speed/HP curves\n\
                         and matched to equipped skills. Approximate until the plugin\n\
                         records real activation events.",
                    );
            }
            ui.separator();
            ui.checkbox(&mut self.show_hp, "Show HP");
            ui.checkbox(&mut self.show_gap, "Show Poskeep Gap")
                .on_hover_text("Gap behind the leader (m), dashed.");
            ui.checkbox(&mut self.show_labels, "Show Labels");
            ui.checkbox(&mut self.show_field, "Show Field")
                .on_hover_text(
                    "Bottom strip: how horses move across the field (lane) over distance.\n\
                     Red = blocked by the horse in front, orange = Spot Struggle,\n\
                     purple = Dueling (final straight).",
                );
            // Бейдж «реального» Spot Struggle показываем только когда он реально
            // из событий движка; «≈ derived» индикатор убран по просьбе — правило
            // всё равно работает в таблице/поле, просто без шумной метки сверху.
            if self.real_struggle {
                ui.separator();
                ui.label(egui::RichText::new("⚔ real struggle").color(crate::theme::C_SPURT).small())
                    .on_hover_text("Spot Struggle is read from real engine events (type 4) recorded by the plugin.");
            }
            if self.real_duel {
                ui.label(egui::RichText::new("⚔ real duel").color(crate::theme::C_DUEL).small())
                    .on_hover_text("Dueling (追い比べ) on the final straight, from real engine events (type 5).");
            }
        });

        // --- инфо НАВЕРХУ (компактно): слева список девочек (3 колонки по 3,
        // начиная с победителей), справа живые статы в текущем кадре ---
        // Легенда и статы — в ТЁМНЫХ обведённых панелях (как фон графика), чтобы
        // параметры читались и колонки были выровнены/«обведены».
        ui.horizontal_top(|ui| {
            crate::theme::dark_panel().show(ui, |ui| {
                ui.vertical(|ui| {
                    ui.label(egui::RichText::new("Horses").color(crate::theme::PINK_SOFT).strong());
                    self.legend(ui, gd);
                });
            });
            ui.add_space(10.0);
            crate::theme::dark_panel().show(ui, |ui| {
                ui.vertical(|ui| {
                    ui.label(
                        egui::RichText::new(format!("Live stats @ {:.1}s", t_now))
                            .color(crate::theme::PINK_SOFT)
                            .strong(),
                    );
                    self.stats_panel(ui);
                });
            });
        });
        ui.add_space(4.0);
        ui.separator();

        // --- график: ширина по доступной, ВЫСОТА по содержимому (зоны фаз/кривых
        // + ровно столько строк подписей, сколько нужно) — без пустот и наложений ---
        let chart_w = ui.available_width().clamp(720.0, 1200.0);
        let plot_width = (chart_w - MARG_L - MARG_R).max(50.0);
        let total = self.race.total_distance().max(1.0);
        let (labels, n_rows) = self.build_labels(plot_width, total, gd);
        let n_rows = n_rows.min(10); // разумный предел, чтобы не было абсурдно высоко
        let label_h = if self.show_labels { n_rows as f32 * ROW_H } else { 0.0 };
        let field_h = if self.show_field { FIELD_GAP + FIELD_H } else { 0.0 };
        let chart_h = PHASE_H + CURVE_H + 4.0 + label_h + 4.0 + AXIS_H + field_h;
        let (resp, painter) = ui.allocate_painter(egui::vec2(chart_w, chart_h), Sense::hover());
        self.paint_chart(&painter, resp.rect, geom, &labels, if self.show_labels { n_rows } else { 0 });

        // ползунок во всю ширину графика — игрок сам тащит его. В тёмной панели
        // с розовым временем — чтобы был заметнее («более чётко»).
        ui.add_space(2.0);
        let mut f = self.frame;
        let mut changed = false;
        crate::theme::dark_panel().show(ui, |ui| {
            ui.spacing_mut().slider_width = (chart_w - 40.0).max(120.0);
            ui.horizontal(|ui| {
                let sresp = ui.add(
                    egui::Slider::new(&mut f, 0..=(nf - 1))
                        .show_value(false)
                        .text(
                            egui::RichText::new(format!("{:.1}s / {:.1}s", t_now, t_end))
                                .color(crate::theme::PINK_SOFT)
                                .strong(),
                        ),
                );
                changed = sresp.changed();
            });
        });
        if changed {
            self.frame = f;
        }
    }

    /// Легенда: девочки в 3 колонки по 3, начиная с победителей (порядок финиша).
    fn legend(&mut self, ui: &mut egui::Ui, _gd: Option<&GameData>) {
        let order = self.order.clone();
        egui::Grid::new("replay_legend")
            .num_columns(3)
            .spacing([18.0, 3.0])
            .show(ui, |ui| {
                for (rank, hi) in order.into_iter().enumerate() {
                    let col = self.color[hi];
                    let h = &self.race.horses[hi];
                    let place = if h.finish_order > 0 {
                        format!("P{}", h.finish_order)
                    } else {
                        format!("#{}", rank + 1)
                    };
                    ui.horizontal(|ui| {
                        let mut on = self.shown[hi];
                        if ui.checkbox(&mut on, "").changed() {
                            self.shown[hi] = on;
                        }
                        let (rect, _) = ui.allocate_exact_size(egui::vec2(12.0, 12.0), Sense::hover());
                        ui.painter().rect_filled(rect, 2.0, col);
                        let label = format!("{} {}", place, h.name);
                        let txt = if h.is_user {
                            egui::RichText::new(label).color(crate::theme::C_MINE)
                        } else {
                            egui::RichText::new(label)
                        };
                        let tip = if h.trainer.is_empty() {
                            format!("Gate {} · {}", h.gate, style_name(h.style))
                        } else {
                            format!("Gate {} · {} · trainer {}", h.gate, style_name(h.style), h.trainer)
                        };
                        ui.label(txt).on_hover_text(tip);
                    });
                    if rank % 3 == 2 {
                        ui.end_row();
                    }
                }
            });
    }

    /// Живые статы выбранных девочек в текущем кадре: скорость, HP, ускорение,
    /// блок (кем) и Spot Struggle (с кем).
    fn stats_panel(&self, ui: &mut egui::Ui) {
        let f = self.frame;
        let total = self.race.total_distance().max(1.0);
        egui::Grid::new("replay_stats")
            .num_columns(7)
            .striped(true)
            .spacing([12.0, 4.0])
            .show(ui, |ui| {
                ui.label(egui::RichText::new("Horse").strong());
                ui.label(egui::RichText::new("Speed").strong());
                ui.label(egui::RichText::new("HP").strong());
                ui.label(egui::RichText::new("Accel").strong())
                    .on_hover_text("Acceleration (m/s²) at this moment, from the speed curve.");
                ui.label(egui::RichText::new("Block").strong())
                    .on_hover_text("Blocked by the horse in front at this moment (engine truth).");
                ui.label(egui::RichText::new("Struggle").strong())
                    .on_hover_text(
                        "Spot Struggle (位置取り争い): front-runners contesting position early in\n\
                         the race. Real engine events (type 4) when available, else a rules\n\
                         approximation (two front-runners contesting).",
                    );
                ui.label(egui::RichText::new("Duel").strong())
                    .on_hover_text(
                        "Dueling (追い比べ): side-by-side battle on the final straight.\n\
                         Real engine events (type 5) only — blank for older archives.",
                    );
                ui.end_row();

                for &hi in &self.order {
                    if !self.shown[hi] {
                        continue;
                    }
                    let h = &self.race.horses[hi];
                    let col = self.color[hi];
                    let v = h.v.get(f).copied().unwrap_or(f32::NAN);
                    let hp = h.hp.get(f).copied().unwrap_or(f32::NAN);
                    // Ускорение из кривой скорости: Δv/Δt от предыдущего кадра.
                    let accel = if f > 0 {
                        let dt = (self.race.frame_t[f] - self.race.frame_t[f - 1]).max(1e-3);
                        match (h.v.get(f), h.v.get(f - 1)) {
                            (Some(&a), Some(&b)) if a.is_finite() && b.is_finite() => Some((a - b) / dt),
                            _ => None,
                        }
                    } else {
                        Some(0.0)
                    };
                    ui.label(egui::RichText::new(h.name.clone()).color(col));
                    ui.label(if v.is_finite() { format!("{:.1}", v) } else { "—".into() });
                    ui.label(if hp.is_finite() { format!("{:.0}", hp) } else { "—".into() });
                    match accel {
                        Some(a) => {
                            let c = if a > 0.05 {
                                crate::theme::C_GOOD
                            } else if a < -0.05 {
                                Color32::from_rgb(230, 57, 53)
                            } else {
                                Color32::GRAY
                            };
                            ui.label(egui::RichText::new(format!("{:+.2}", a)).color(c).monospace());
                        }
                        None => {
                            ui.label(egui::RichText::new("—").weak());
                        }
                    }
                    // Block (граунд-трус): «yes (by X)» с именем блокирующей спереди.
                    match self.blocker_at(hi, f) {
                        Some(bi) => {
                            let txt = format!("yes (by {})", short_name(&self.race.horses[bi].name));
                            ui.label(egui::RichText::new(txt).color(Color32::from_rgb(230, 57, 53)).strong());
                        }
                        None => {
                            ui.label(egui::RichText::new("no").weak());
                        }
                    }
                    // Spot Struggle (ground truth / ≈ правило): «yes (with X)», +N если больше.
                    contest_cell(ui, &self.struggle_partners(hi, f, total), &self.race.horses, crate::theme::C_SPURT);
                    // Dueling (только ground truth): «yes (with X)».
                    contest_cell(ui, &self.duel_partners(hi, f), &self.race.horses, crate::theme::C_DUEL);
                    ui.end_row();
                }
            });
    }

    /// Собирает подписи скиллов показанных девочек и упаковывает их по строкам
    /// (greedy слева направо), чтобы они не накладывались. Возвращает (подписи,
    /// число строк). `plot_width` — ширина зоны графика в px (для раскладки).
    fn build_labels(
        &self,
        plot_width: f32,
        total: f32,
        gd: Option<&GameData>,
    ) -> (Vec<LabelItem>, usize) {
        let mut items: Vec<LabelItem> = Vec::new();
        for &hi in &self.order {
            if !self.shown[hi] {
                continue;
            }
            let col = self.color[hi];
            for a in &self.activations[hi] {
                let sym = match a.kind {
                    ActKind::Recovery => "♥ ",
                    ActKind::Debuff => "▽ ",
                    ActKind::Speed => "",
                };
                let name = gd.map(|g| g.skill_name(a.skill_id)).unwrap_or_else(|| format!("#{}", a.skill_id));
                let text = format!("{}{}", sym, short_name(&name));
                let w = text.chars().count() as f32 * 6.8 + 10.0;
                let alpha = if a.confidence >= 0.85 {
                    255
                } else if a.confidence >= 0.6 {
                    215
                } else {
                    150
                };
                let col = Color32::from_rgba_unmultiplied(col.r(), col.g(), col.b(), alpha);
                items.push(LabelItem { dist: a.distance, w, text, col, hi, frame: a.frame_idx, row: 0 });
            }
        }
        items.sort_by(|a, b| a.dist.partial_cmp(&b.dist).unwrap_or(std::cmp::Ordering::Equal));
        let gap = 6.0;
        let mut row_end: Vec<f32> = Vec::new();
        for it in &mut items {
            let x = (it.dist / total).clamp(0.0, 1.0) * plot_width;
            let x0 = x - it.w * 0.5;
            let mut ri = row_end.len();
            for (i, end) in row_end.iter_mut().enumerate() {
                if x0 > *end + gap {
                    *end = x0 + it.w;
                    ri = i;
                    break;
                }
            }
            if ri == row_end.len() {
                row_end.push(x0 + it.w);
            }
            it.row = ri;
        }
        (items, row_end.len().max(1))
    }

    /// Рисует горизонтальный график гонки (умалатор-стиль) в трёх зонах по высоте:
    /// полоса фаз, зона кривых (скорость/HP/gap), зона подписей скиллов. `labels`
    /// и `n_rows` приходят из build_labels (та же раскладка, что задала высоту).
    fn paint_chart(
        &self,
        painter: &egui::Painter,
        rect: Rect,
        geom: Option<&CourseGeom>,
        labels: &[LabelItem],
        n_rows: usize,
    ) {
        let total = self.race.total_distance().max(1.0);
        painter.rect_filled(rect, 4.0, Color32::from_rgb(26, 28, 34));
        // Зоны по высоте: полоса фаз сверху, зона КРИВЫХ (plot), зона подписей,
        // ось дистанции снизу. `plot` = только зона кривых (скорость/HP).
        let plot = Rect::from_min_max(
            pos2(rect.left() + MARG_L, rect.top() + PHASE_H),
            pos2(rect.right() - MARG_R, rect.top() + PHASE_H + CURVE_H),
        );
        let label_top = plot.bottom() + 4.0;
        let label_bottom = label_top + n_rows as f32 * ROW_H;
        let axis_y = label_bottom + 4.0;
        let x_of = |d: f32| plot.left() + (d / total).clamp(0.0, 1.0) * plot.width();

        // Шкалы. Скорость — общий максимум по показанным (округлён вверх).
        let mut vmax = 1.0_f32;
        let mut hpmax = 1.0_f32;
        for &hi in &self.order {
            if !self.shown[hi] {
                continue;
            }
            for &v in &self.race.horses[hi].v {
                if v.is_finite() {
                    vmax = vmax.max(v);
                }
            }
            for &hp in &self.race.horses[hi].hp {
                if hp.is_finite() {
                    hpmax = hpmax.max(hp);
                }
            }
        }
        vmax = nice_ceil(vmax, 4.0).max(8.0);
        let y_v = |v: f32| plot.bottom() - (v / vmax).clamp(0.0, 1.2) * plot.height();
        let y_hp = |hp: f32| plot.bottom() - (hp / hpmax).clamp(0.0, 1.0) * plot.height();

        // --- полосы фаз: подписи и границы НАД треком (без заливки, чтобы не
        // налезали на скиллы и не красили поле) ---
        let phases = [
            (0.0_f32, 1.0 / 6.0, "Opening leg"),
            (1.0 / 6.0, 2.0 / 3.0, "Middle leg"),
            (2.0 / 3.0, 1.0, "Last spurt"),
        ];
        for (p0, p1, name) in phases {
            let xa = x_of(p0 * total);
            let xb = x_of(p1 * total);
            painter.text(
                pos2((xa + xb) * 0.5, rect.top() + 12.0),
                Align2::CENTER_CENTER,
                name,
                FontId::proportional(11.0),
                Color32::from_gray(195),
            );
            if p0 > 0.0 {
                // граница фазы — тонкая вертикальная линия через кривые и подписи
                painter.line_segment(
                    [pos2(xa, plot.top()), pos2(xa, label_bottom)],
                    Stroke::new(1.0, Color32::from_gray(90)),
                );
                painter.text(
                    pos2(xa + 2.0, plot.top() + 1.0),
                    Align2::LEFT_TOP,
                    format!("{}m", (p0 * total) as i32),
                    FontId::proportional(8.5),
                    Color32::from_gray(140),
                );
            }
        }

        // --- рельеф БЕЗ заливки: границы углов тонкими линиями + серые подписи
        // (углы / прямые / уклоны) у низа поля ---
        if let Some(g) = geom {
            let label_y = plot.bottom() - 11.0;
            for (i, &(s, e)) in g.corners.iter().enumerate() {
                let xa = x_of(s as f32);
                let xb = x_of(e as f32);
                for x in [xa, xb] {
                    painter.line_segment(
                        [pos2(x, plot.top()), pos2(x, plot.bottom())],
                        Stroke::new(1.0, Color32::from_gray(72)),
                    );
                }
                if xb - xa > 26.0 {
                    painter.text(pos2((xa + xb) * 0.5, label_y), Align2::CENTER_CENTER, format!("Corner {}", i + 1), FontId::proportional(9.5), Color32::from_gray(150));
                }
            }
            for &(s, e) in &g.straights {
                let xa = x_of(s as f32);
                let xb = x_of(e as f32);
                if xb - xa > 30.0 {
                    painter.text(pos2((xa + xb) * 0.5, label_y), Align2::CENTER_CENTER, "Straight", FontId::proportional(9.5), Color32::from_gray(140));
                }
            }
            for &(s, e, v) in &g.slopes {
                if v == 0.0 {
                    continue;
                }
                let xa = x_of(s as f32);
                let xb = x_of(e as f32);
                if xb - xa > 24.0 {
                    let sym = if v > 0.0 { "↗ up" } else { "↘ down" };
                    painter.text(pos2((xa + xb) * 0.5, label_y - 12.0), Align2::CENTER_CENTER, sym, FontId::proportional(9.0), Color32::from_gray(135));
                }
            }
        }

        // --- сетка + оси ---
        let grid = Color32::from_gray(52);
        let mut d = 0.0;
        while d <= total + 1.0 {
            let x = x_of(d);
            painter.line_segment([pos2(x, plot.top()), pos2(x, label_bottom)], Stroke::new(1.0, grid));
            painter.text(pos2(x, axis_y), Align2::CENTER_TOP, format!("{}", d as i32), FontId::proportional(9.0), Color32::from_gray(150));
            d += 200.0;
        }
        let mut s = 0.0;
        while s <= vmax + 0.1 {
            let y = y_v(s);
            painter.line_segment([pos2(plot.left(), y), pos2(plot.right(), y)], Stroke::new(1.0, grid));
            painter.text(pos2(plot.left() - 3.0, y), Align2::RIGHT_CENTER, format!("{}", s as i32), FontId::proportional(9.0), Color32::from_gray(150));
            s += 4.0;
        }

        let nframes = self.race.frame_t.len();

        // --- poskeep gap (отставание от лидера), пунктир в нижней части поля ---
        if self.show_gap {
            // лидер по дистанции в каждом кадре (по ВСЕМ лошадям).
            let mut leader = vec![0.0_f32; nframes];
            for h in &self.race.horses {
                for (i, &d) in h.d.iter().take(nframes).enumerate() {
                    if d.is_finite() {
                        leader[i] = leader[i].max(d);
                    }
                }
            }
            let mut gapmax = 1.0_f32;
            for &hi in &self.order {
                if !self.shown[hi] {
                    continue;
                }
                let h = &self.race.horses[hi];
                for (i, &d) in h.d.iter().take(nframes).enumerate() {
                    if d.is_finite() {
                        gapmax = gapmax.max(leader[i] - d);
                    }
                }
            }
            let y_gap = |g: f32| plot.bottom() - (g / gapmax).clamp(0.0, 1.0) * plot.height() * 0.45;
            for &hi in &self.order {
                if !self.shown[hi] {
                    continue;
                }
                let h = &self.race.horses[hi];
                let col = self.color[hi];
                let pts: Vec<Pos2> = (0..nframes.min(h.d.len()))
                    .filter(|&i| h.d[i].is_finite())
                    .map(|i| pos2(x_of(h.d[i]), y_gap((leader[i] - h.d[i]).max(0.0))))
                    .collect();
                if pts.len() > 1 {
                    painter.extend(egui::Shape::dashed_line(&pts, Stroke::new(1.3, col), 5.0, 4.0));
                }
            }
        }

        // --- кривые: HP (тонкие, полупрозрачные) и скорость (жирные) ---
        for &hi in &self.order {
            if !self.shown[hi] {
                continue;
            }
            let h = &self.race.horses[hi];
            let col = self.color[hi];
            let m = nframes.min(h.d.len());
            if self.show_hp {
                let hp_col = Color32::from_rgba_unmultiplied(col.r(), col.g(), col.b(), 95);
                let pts: Vec<Pos2> = (0..m.min(h.hp.len()))
                    .filter(|&i| h.d[i].is_finite() && h.hp[i].is_finite())
                    .map(|i| pos2(x_of(h.d[i]), y_hp(h.hp[i])))
                    .collect();
                if pts.len() > 1 {
                    painter.add(egui::Shape::line(pts, Stroke::new(1.2, hp_col)));
                }
            }
            let pts: Vec<Pos2> = (0..m.min(h.v.len()))
                .filter(|&i| h.d[i].is_finite() && h.v[i].is_finite())
                .map(|i| pos2(x_of(h.d[i]), y_v(h.v[i])))
                .collect();
            if pts.len() > 1 {
                painter.add(egui::Shape::line(pts, Stroke::new(2.0, col)));
            }
        }

        // --- подписи скиллов в СВОЕЙ зоне (высота зоны = n_rows строк, поэтому
        // подписи никогда не накладываются). Точка на кривой + выноска вниз. ---
        if self.show_labels {
            for it in labels {
                let x = x_of(it.dist);
                let v = self.race.horses[it.hi]
                    .v
                    .get(it.frame)
                    .copied()
                    .filter(|x| x.is_finite())
                    .unwrap_or(0.0);
                let cy = y_v(v);
                let row = it.row.min(n_rows.saturating_sub(1));
                let ly = label_top + row as f32 * ROW_H + ROW_H * 0.5;
                let center = pos2(x, ly);
                let faint = Color32::from_rgba_unmultiplied(it.col.r(), it.col.g(), it.col.b(), 80);
                painter.circle_filled(pos2(x, cy), 3.0, it.col);
                painter.line_segment([pos2(x, cy + 3.0), pos2(x, ly - ROW_H * 0.5)], Stroke::new(1.0, faint));
                let bg = Rect::from_center_size(center, egui::vec2(it.w, ROW_H - 2.0));
                painter.rect_filled(bg, 3.0, Color32::from_rgba_unmultiplied(18, 20, 26, 220));
                painter.text(center, Align2::CENTER_CENTER, &it.text, FontId::proportional(11.5), it.col);
            }
        }

        // --- позиции лошадей в ТЕКУЩЕМ кадре (плейхед по времени из ползунка) ---
        let f = self.frame;
        for &hi in &self.order {
            if !self.shown[hi] {
                continue;
            }
            let h = &self.race.horses[hi];
            let (Some(&d), Some(&v)) = (h.d.get(f), h.v.get(f)) else { continue };
            if !d.is_finite() || !v.is_finite() {
                continue;
            }
            let x = x_of(d);
            let col = self.color[hi];
            painter.line_segment(
                [pos2(x, plot.top()), pos2(x, label_bottom)],
                Stroke::new(1.0, Color32::from_rgba_unmultiplied(255, 255, 255, 32)),
            );
            let p = pos2(x, y_v(v));
            painter.circle_filled(p, 4.5, col);
            painter.circle_stroke(p, 4.5, Stroke::new(1.5, Color32::BLACK));
        }

        // --- НИЖНЯЯ ПОЛОСА «Field»: смещение лошадей поперёк поля (lane) вдоль
        // дистанции. Поверх кривой lane — КРАСНЫЕ сегменты блока (кто спереди душит)
        // и ОРАНЖЕВЫЕ сегменты Spot Struggle (фронт-раннеры рубятся за позицию).
        // Прямо видно, где была давка/блок и где спот-страгл. ---
        if self.show_field {
            let field_top = axis_y + AXIS_H + FIELD_GAP;
            let field = Rect::from_min_max(
                pos2(plot.left(), field_top),
                pos2(plot.right(), field_top + FIELD_H),
            );
            painter.rect_filled(field, 3.0, Color32::from_rgb(20, 22, 28));

            // Шкала lane: авто по min/max показанных (единицы движка не важны).
            let mut lmin = f32::MAX;
            let mut lmax = f32::MIN;
            for &hi in &self.order {
                if !self.shown[hi] {
                    continue;
                }
                for &l in &self.race.horses[hi].lane {
                    if l.is_finite() {
                        lmin = lmin.min(l);
                        lmax = lmax.max(l);
                    }
                }
            }
            if !(lmin.is_finite() && lmax.is_finite() && lmax > lmin) {
                lmin = 0.0;
                lmax = 1.0;
            }
            let pad = 9.0;
            let span = (lmax - lmin).max(1e-3);
            // Меньший lane (ближе к внутренней бровке) — внизу полосы.
            let y_lane = |l: f32| field.bottom() - pad - (l - lmin) / span * (field.height() - pad * 2.0);

            painter.text(
                pos2(field.left() + 3.0, field.top() + 1.0),
                Align2::LEFT_TOP,
                "Field (lane shift)",
                FontId::proportional(9.5),
                Color32::from_gray(150),
            );
            painter.text(
                pos2(field.left() + 3.0, field.bottom() - 1.0),
                Align2::LEFT_BOTTOM,
                "inner rail",
                FontId::proportional(8.5),
                Color32::from_gray(105),
            );
            painter.text(
                pos2(field.left() + 3.0, field.top() + 13.0),
                Align2::LEFT_TOP,
                "outer",
                FontId::proportional(8.5),
                Color32::from_gray(105),
            );

            // Лёгкая X-сетка (та же шкала дистанции), для совмещения с верхним графиком.
            let mut d = 0.0;
            while d <= total + 1.0 {
                let x = x_of(d);
                painter.line_segment(
                    [pos2(x, field.top()), pos2(x, field.bottom())],
                    Stroke::new(1.0, Color32::from_gray(40)),
                );
                d += 200.0;
            }

            for &hi in &self.order {
                if !self.shown[hi] {
                    continue;
                }
                let h = &self.race.horses[hi];
                let col = self.color[hi];
                let m = nframes.min(h.d.len()).min(h.lane.len());
                let pos_at = |i: usize| pos2(x_of(h.d[i]), y_lane(h.lane[i]));
                let ok = |i: usize| h.d[i].is_finite() && h.lane[i].is_finite();

                // базовая кривая lane (полупрозрачный цвет лошади)
                let base: Vec<Pos2> = (0..m).filter(|&i| ok(i)).map(&pos_at).collect();
                if base.len() > 1 {
                    let c = Color32::from_rgba_unmultiplied(col.r(), col.g(), col.b(), 170);
                    painter.add(egui::Shape::line(base, Stroke::new(1.5, c)));
                }

                // оверлеи состояний: оранжевый (struggle) под красным (block)
                let draw_runs = |active: &dyn Fn(usize) -> bool, color: Color32, width: f32| {
                    let mut seg: Vec<Pos2> = Vec::new();
                    for i in 0..m {
                        if ok(i) && active(i) {
                            seg.push(pos_at(i));
                        } else if seg.len() > 1 {
                            painter.add(egui::Shape::line(std::mem::take(&mut seg), Stroke::new(width, color)));
                        } else {
                            seg.clear();
                        }
                    }
                    if seg.len() > 1 {
                        painter.add(egui::Shape::line(seg, Stroke::new(width, color)));
                    }
                };
                draw_runs(
                    &|i| !self.duel_partners(hi, i).is_empty(),
                    Color32::from_rgb(200, 130, 255),
                    3.0,
                );
                draw_runs(
                    &|i| !self.struggle_partners(hi, i, total).is_empty(),
                    Color32::from_rgb(255, 160, 50),
                    3.0,
                );
                draw_runs(&|i| self.blocker_at(hi, i).is_some(), Color32::from_rgb(230, 57, 53), 2.5);

                // плейхед-точка на полосе
                if self.frame < m && ok(self.frame) {
                    let p = pos_at(self.frame);
                    painter.circle_filled(p, 3.5, col);
                    painter.circle_stroke(p, 3.5, Stroke::new(1.0, Color32::BLACK));
                }
            }

            // вертикаль плейхеда через всю полосу
            if let Some(h) = self.order.iter().copied().find(|&hi| self.shown[hi]) {
                if let Some(&d) = self.race.horses[h].d.get(self.frame).filter(|x| x.is_finite()) {
                    let x = x_of(d);
                    painter.line_segment(
                        [pos2(x, field.top()), pos2(x, field.bottom())],
                        Stroke::new(1.0, Color32::from_rgba_unmultiplied(255, 255, 255, 28)),
                    );
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gamedata::Cond;

    #[test]
    fn short_name_truncates() {
        assert_eq!(short_name("Triple 7"), "Triple 7");
        let long = short_name("Professor of Curvature");
        assert!(long.chars().count() <= 14, "слишком длинно: {long}");
        assert!(long.ends_with('…'));
    }

    #[test]
    fn phases_split_by_progress() {
        assert_eq!(phase_of(0.0), 0);
        assert_eq!(phase_of(0.5), 1);
        assert_eq!(phase_of(0.7), 2);
        assert_eq!(phase_of(0.95), 3);
    }

    #[test]
    fn cond_op_works() {
        assert!(cmp_op(2.0, CondOp::Ge, 2.0));
        assert!(!cmp_op(1.0, CondOp::Gt, 2.0));
        assert!(cmp_op(3.0, CondOp::Ne, 2.0));
    }

    fn ctx(phase: i32) -> CondCtx {
        CondCtx { phase, remain: 500.0, hp_per: 80.0 }
    }

    #[test]
    fn empty_tree_always_true() {
        let t: CondTree = Vec::new();
        assert_eq!(eval_tree(&t, &ctx(2)), Some(0));
    }

    #[test]
    fn phase_gate_filters() {
        // phase>=2 проходит в фазе 2 и блокирует в фазе 1.
        let tree = vec![vec![Cond { var: "phase".into(), op: CondOp::Ge, val: 2.0 }]];
        assert_eq!(eval_tree(&tree, &ctx(2)), Some(1));
        assert_eq!(eval_tree(&tree, &ctx(1)), None);
    }

    #[test]
    fn unknown_var_is_ignored() {
        // незнакомую переменную не отбраковываем (условие игнорируется).
        let tree = vec![vec![Cond { var: "running_style".into(), op: CondOp::Eq, val: 1.0 }]];
        assert_eq!(eval_tree(&tree, &ctx(1)), Some(0));
    }

    // Интеграционный тест: грузит САМЫЙ свежий реальный архив гонки + master.mdb
    // и прогоняет привязку скиллов. Тихо выходит, если данных на машине нет
    // (как load_real_master_mdb). Проверяет, что путь не паникует и даёт маркеры
    // у победителя, скучкованные ближе к спурту.
    // Эмпирическая проверка: сканирует ВСЕ архивы гонок и применяет правило
    // Spot Struggle (struggle_partners) ко всем фронт-раннерам по всем кадрам.
    // Печатает по гонке: дистанцию, число nige, эпизоды/кадры спот-страгла и пример
    // «кто-с-кем-где». Тихо выходит, если архивов нет. Запуск:
    //   cargo test --release scan_all_archives -- --ignored --nocapture
    #[test]
    #[ignore]
    fn scan_all_archives_for_struggle() {
        let dir = races_dir();
        let Ok(rd) = std::fs::read_dir(&dir) else {
            eprintln!("нет папки архивов — пропуск");
            return;
        };
        let mut files: Vec<std::path::PathBuf> = rd
            .flatten()
            .map(|e| e.path())
            .filter(|p| {
                p.file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.starts_with("race_") && n.ends_with(".json"))
            })
            .collect();
        files.sort();
        eprintln!("=== сканирую {} архивов на Spot Struggle ===", files.len());
        let (mut races_with, mut total_races) = (0u32, 0u32);
        for p in &files {
            let Ok(text) = std::fs::read_to_string(p) else { continue };
            let race: ArchiveRace = match serde_json::from_str(&text) {
                Ok(r) => r,
                Err(e) => {
                    eprintln!("{:?}: parse fail {e}", p.file_name().unwrap());
                    continue;
                }
            };
            total_races += 1;
            let total = race.total_distance().max(1.0);
            let nige: Vec<usize> = race
                .horses
                .iter()
                .enumerate()
                .filter(|(_, h)| h.style == STYLE_NIGE)
                .map(|(i, _)| i)
                .collect();
            let r = ReplayState::new(race, None);
            let nf = r.n_frames();
            let (mut episodes, mut frames) = (0u32, 0u32);
            let mut sample = String::new();
            for &hi in &nige {
                let mut prev = false;
                for f in 0..nf {
                    let parts = r.struggle_partners(hi, f, total);
                    let on = !parts.is_empty();
                    if on {
                        frames += 1;
                        if !prev {
                            episodes += 1;
                            if sample.len() < 180 {
                                let d = r.race.horses[hi].d.get(f).copied().unwrap_or(0.0);
                                sample.push_str(&format!(
                                    "[{} ↔ {} @{:.0}m] ",
                                    r.race.horses[hi].name, r.race.horses[parts[0]].name, d
                                ));
                            }
                        }
                    }
                    prev = on;
                }
            }
            if episodes > 0 {
                races_with += 1;
            }
            let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("?");
            eprintln!(
                "{:<42} d={:<5.0} nige={} | episodes={} frames={} {}",
                name, total, nige.len(), episodes, frames, sample
            );
        }
        eprintln!(
            "=== Spot Struggle сработал в {}/{} гонках ===",
            races_with, total_races
        );
    }

    #[test]
    fn infer_on_real_archive() {
        let Some(race) = load_latest() else {
            eprintln!("нет архивов гонок — пропуск");
            return;
        };
        let Some(gd) = GameData::load() else {
            eprintln!("master.mdb не найдена — пропуск");
            return;
        };
        assert!(!race.frame_t.is_empty(), "пустой таймлайн");
        let acts = infer(&race, &gd);
        assert_eq!(acts.len(), race.horses.len());
        let total = race.total_distance();
        // Победитель (finish_order == 1), если есть.
        if let Some(wi) = race.horses.iter().position(|h| h.finish_order == 1) {
            let n = acts[wi].len();
            eprintln!("winner {} activations: {}", race.horses[wi].name, n);
            for a in &acts[wi] {
                let p = a.distance / total;
                assert!((0.0..=1.0).contains(&p), "маркер вне трассы: {p}");
                eprintln!("  {:?} {} @ {:.0}m (p={:.2}) conf {:.2}", a.kind, gd.skill_name(a.skill_id), a.distance, p, a.confidence);
            }
        }
    }

}
