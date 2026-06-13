//! Загрузка данных из master.mdb игры (SQLite).
//!
//! Игра хранит ВСЕ числа баланса у себя в БД, мы читаем их же — поэтому
//! коэффициенты всегда в синхроне с версией игры, ничего не хардкодим:
//! - skill_data: эффекты скиллов (тип/значение/длительность/условия-DSL);
//! - skill_level_value: масштабирование значения эффекта по уровню скилла;
//! - race_motivation_rate: множитель статов от мотивации (1..5);
//! - race_proper_distance_rate: множители скорости/силы от аптитуда дистанции;
//! - race_proper_runningstyle_rate: множитель ума от аптитуда стиля;
//! - race_proper_ground_rate: множитель силы от аптитуда поверхности;
//! - race_course_set: дистанция и поверхность трасс по id.

use std::collections::HashMap;
use std::path::PathBuf;

use rusqlite::Connection;

/// Одно условие из DSL: `var op value` (например `phase>=2`).
#[derive(Debug, Clone)]
pub struct Cond {
    pub var: String,
    pub op: CondOp,
    pub val: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CondOp {
    Eq,
    Ne,
    Ge,
    Le,
    Gt,
    Lt,
}

/// Группы ИЛИ, внутри группы — И: `a&b@c&d` = (a И b) ИЛИ (c И d).
pub type CondTree = Vec<Vec<Cond>>;

/// Один эффект скилла (внутри варианта может быть до 3).
#[derive(Debug, Clone)]
pub struct SkillEffect {
    /// Тип эффекта (ability_type): 27 = current speed, 31 = accel, 9 = recovery...
    pub ability_type: i32,
    /// Значение эффекта, уже в долях (float_ability_value / 10000).
    pub value: f64,
    /// Кому: 1 = себе, прочее = другим (дебаффы).
    pub target_type: i32,
}

/// Вариант срабатывания скилла (skill_data имеет до 2 вариантов: _1 и _2).
#[derive(Debug, Clone)]
pub struct SkillVariant {
    pub precondition: CondTree,
    pub condition: CondTree,
    /// Длительность эффекта в секундах (база, до масштабирования по дистанции).
    pub duration_s: f64,
    pub cooldown_s: f64,
    pub effects: Vec<SkillEffect>,
}

#[derive(Debug, Clone)]
pub struct SkillDef {
    pub id: i32,
    pub rarity: i32,
    /// СВОЙ уник персонажа (固有): skill_category == 5 И id в 1xxxxx (pfx 10/11).
    /// Только такой скилл срабатывает БЕЗ Wit-гейта активации.
    /// Наследуемые уники (継承固有, id 9xxxxx) — НЕ сюда: у них Wit-чек ЕСТЬ
    /// (они слабее и проходят активацию как обычные скиллы).
    pub is_unique: bool,
    pub variants: Vec<SkillVariant>,
}

/// Все данные игры, нужные симулятору.
pub struct GameData {
    pub skills: HashMap<i32, SkillDef>,
    /// value-коэффициент по (ability_type, level): итог = value * coef.
    pub level_coef: HashMap<(i32, i32), f64>,
    /// Множитель статов от мотивации (индекс 1..5).
    pub motivation_rate: HashMap<i32, f64>,
    /// (множитель скорости, множитель силы) от аптитуда дистанции (1..8 = G..S).
    pub dist_rate: HashMap<i32, (f64, f64)>,
    /// Множитель ума от аптитуда стиля бега (1..8).
    pub style_rate: HashMap<i32, f64>,
    /// Множитель силы (ускорения) от аптитуда поверхности (1..8).
    pub ground_rate: HashMap<i32, f64>,
    /// Трассы: course_set_id -> (дистанция м, поверхность 1=турф 2=грунт).
    pub courses: HashMap<i32, (i32, i32)>,
}

/// Стандартный путь master.mdb (LocalLow Cygames).
pub fn master_mdb_path() -> Option<PathBuf> {
    let user = std::env::var("USERPROFILE").ok()?;
    for cy in ["Cygames", "cygames"] {
        for uma in ["umamusume", "Umamusume"] {
            let p = PathBuf::from(&user)
                .join("AppData/LocalLow")
                .join(cy)
                .join(uma)
                .join("master/master.mdb");
            if p.exists() {
                return Some(p);
            }
        }
    }
    None
}

/// `a&b@c` → [[a,b],[c]]. Пустая строка → пустое дерево (= всегда истинно).
pub fn parse_cond(s: &str) -> CondTree {
    if s.trim().is_empty() {
        return Vec::new();
    }
    s.split('@')
        .map(|grp| grp.split('&').filter_map(parse_one_cond).collect())
        .collect()
}

fn parse_one_cond(s: &str) -> Option<Cond> {
    let s = s.trim();
    // порядок важен: сначала двухсимвольные операторы
    for (txt, op) in [
        (">=", CondOp::Ge),
        ("<=", CondOp::Le),
        ("==", CondOp::Eq),
        ("!=", CondOp::Ne),
        (">", CondOp::Gt),
        ("<", CondOp::Lt),
    ] {
        if let Some(i) = s.find(txt) {
            let var = s[..i].trim().to_string();
            let val: f64 = s[i + txt.len()..].trim().parse().ok()?;
            return Some(Cond { var, op, val });
        }
    }
    None
}

impl GameData {
    pub fn load() -> Option<GameData> {
        let path = master_mdb_path()?;
        let conn = Connection::open_with_flags(
            &path,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .ok()?;

        let mut gd = GameData {
            skills: HashMap::new(),
            level_coef: HashMap::new(),
            motivation_rate: HashMap::new(),
            dist_rate: HashMap::new(),
            style_rate: HashMap::new(),
            ground_rate: HashMap::new(),
            courses: HashMap::new(),
        };

        // --- таблицы коэффициентов (значения в БД — десятитысячные доли) ---
        {
            let mut st = conn.prepare("select id, motivation_rate from race_motivation_rate").ok()?;
            let mut rows = st.query([]).ok()?;
            while let Ok(Some(r)) = rows.next() {
                let id: i32 = r.get(0).ok()?;
                let v: i64 = r.get(1).ok()?;
                gd.motivation_rate.insert(id, v as f64 / 10000.0);
            }
        }
        {
            let mut st = conn
                .prepare("select id, proper_rate_speed, proper_rate_power from race_proper_distance_rate")
                .ok()?;
            let mut rows = st.query([]).ok()?;
            while let Ok(Some(r)) = rows.next() {
                let id: i32 = r.get(0).ok()?;
                let sp: i64 = r.get(1).ok()?;
                let pw: i64 = r.get(2).ok()?;
                gd.dist_rate.insert(id, (sp as f64 / 10000.0, pw as f64 / 10000.0));
            }
        }
        {
            let mut st = conn.prepare("select id, proper_rate from race_proper_runningstyle_rate").ok()?;
            let mut rows = st.query([]).ok()?;
            while let Ok(Some(r)) = rows.next() {
                let id: i32 = r.get(0).ok()?;
                let v: i64 = r.get(1).ok()?;
                gd.style_rate.insert(id, v as f64 / 10000.0);
            }
        }
        {
            let mut st = conn.prepare("select id, proper_rate from race_proper_ground_rate").ok()?;
            let mut rows = st.query([]).ok()?;
            while let Ok(Some(r)) = rows.next() {
                let id: i32 = r.get(0).ok()?;
                let v: i64 = r.get(1).ok()?;
                gd.ground_rate.insert(id, v as f64 / 10000.0);
            }
        }
        {
            let mut st = conn.prepare("select id, distance, ground from race_course_set").ok()?;
            let mut rows = st.query([]).ok()?;
            while let Ok(Some(r)) = rows.next() {
                let id: i32 = r.get(0).ok()?;
                let d: i32 = r.get(1).ok()?;
                let g: i32 = r.get(2).ok()?;
                gd.courses.insert(id, (d, g));
            }
        }
        {
            let mut st = conn
                .prepare("select ability_type, level, float_ability_value_coef from skill_level_value")
                .ok()?;
            let mut rows = st.query([]).ok()?;
            while let Ok(Some(r)) = rows.next() {
                let at: i32 = r.get(0).ok()?;
                let lvl: i32 = r.get(1).ok()?;
                let c: i64 = r.get(2).ok()?;
                gd.level_coef.insert((at, lvl), c as f64 / 10000.0);
            }
        }

        // --- skill_data: 2 варианта × 3 эффекта ---
        {
            let mut st = conn
                .prepare(
                    "select id, rarity,
                        precondition_1, condition_1, float_ability_time_1, float_cooldown_time_1,
                        ability_type_1_1, float_ability_value_1_1, target_type_1_1,
                        ability_type_1_2, float_ability_value_1_2, target_type_1_2,
                        ability_type_1_3, float_ability_value_1_3, target_type_1_3,
                        precondition_2, condition_2, float_ability_time_2, float_cooldown_time_2,
                        ability_type_2_1, float_ability_value_2_1, target_type_2_1,
                        ability_type_2_2, float_ability_value_2_2, target_type_2_2,
                        ability_type_2_3, float_ability_value_2_3, target_type_2_3,
                        skill_category
                     from skill_data",
                )
                .ok()?;
            let mut rows = st.query([]).ok()?;
            while let Ok(Some(r)) = rows.next() {
                let id: i32 = r.get(0).ok()?;
                let rarity: i32 = r.get(1).ok()?;
                let skill_category: i32 = r.get(28).unwrap_or(0);
                // Свой уник (固有) = cat 5 И id в 1xxxxx (pfx 10/11). Только он без
                // Wit-гейта. Наследуемые уники (継承固有, id 9xxxxx, pfx 90/91) —
                // cat 5, но Wit-чек у них ЕСТЬ → НЕ помечаем как is_unique.
                let is_unique = skill_category == 5 && id < 200_000;
                let mut variants = Vec::new();
                for base in [2usize, 15usize] {
                    let pre: String = r.get(base).unwrap_or_default();
                    let cond: String = r.get(base + 1).unwrap_or_default();
                    let time_ms: i64 = r.get(base + 2).unwrap_or(0);
                    let cd_ms: i64 = r.get(base + 3).unwrap_or(0);
                    let mut effects = Vec::new();
                    for e in 0..3usize {
                        let at: i32 = r.get(base + 4 + e * 3).unwrap_or(0);
                        let val: i64 = r.get(base + 5 + e * 3).unwrap_or(0);
                        let tgt: i32 = r.get(base + 6 + e * 3).unwrap_or(0);
                        if at > 0 {
                            effects.push(SkillEffect {
                                ability_type: at,
                                value: val as f64 / 10000.0,
                                target_type: tgt,
                            });
                        }
                    }
                    if !effects.is_empty() || !cond.trim().is_empty() {
                        variants.push(SkillVariant {
                            precondition: parse_cond(&pre),
                            condition: parse_cond(&cond),
                            duration_s: time_ms as f64 / 10000.0,
                            cooldown_s: cd_ms as f64 / 10000.0,
                            effects,
                        });
                    }
                }
                gd.skills.insert(id, SkillDef { id, rarity, is_unique, variants });
            }
        }

        Some(gd)
    }

    /// Значение эффекта с учётом уровня скилла.
    pub fn effect_value(&self, eff: &SkillEffect, level: i32) -> f64 {
        let coef = self
            .level_coef
            .get(&(eff.ability_type, level))
            .copied()
            .unwrap_or(1.0);
        eff.value * coef
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_cond_groups() {
        let t = parse_cond("phase>=2&order==1@remain_distance<=200");
        assert_eq!(t.len(), 2);
        assert_eq!(t[0].len(), 2);
        assert_eq!(t[0][0].var, "phase");
        assert_eq!(t[0][0].op, CondOp::Ge);
        assert_eq!(t[0][0].val, 2.0);
        assert_eq!(t[1][0].var, "remain_distance");
    }

    #[test]
    fn parse_cond_empty() {
        assert!(parse_cond("").is_empty());
    }

    // Интеграционный тест: читает НАСТОЯЩУЮ master.mdb (есть на машине юзера).
    #[test]
    fn load_real_master_mdb() {
        let Some(gd) = GameData::load() else {
            eprintln!("master.mdb не найдена — пропуск");
            return;
        };
        assert!(gd.skills.len() > 300, "skills: {}", gd.skills.len());
        assert_eq!(gd.motivation_rate.get(&5), Some(&1.04));
        assert_eq!(gd.dist_rate.get(&8).map(|v| v.0), Some(1.05)); // S = +5% скорости
        assert_eq!(gd.style_rate.get(&8), Some(&1.1)); // S = +10% ума
        assert!(gd.courses.len() > 50);
        // скилл 10071 из выборки: current speed 0.15, 6 сек
        let s = gd.skills.get(&10071).expect("skill 10071");
        let v = &s.variants[0];
        assert!((v.duration_s - 6.0).abs() < 1e-9);
        assert_eq!(v.effects[0].ability_type, 27);
        assert!((v.effects[0].value - 0.15).abs() < 1e-9);
        assert!(!v.condition.is_empty());
    }
}
