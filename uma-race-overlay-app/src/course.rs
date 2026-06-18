//! Геометрия трасс (углы / уклоны / прямые) для accel-тайминга в симуляторе.
//!
//! КЛЮЧЕВО для винрейта: где скилл срабатывает по трассе решает всё — accel на
//! входе в спурт (угол перед финишной прямой) даёт отрыв, тот же accel в финальной
//! прямой уже бесполезен. Поэтому условия `corner / slope / straight / is_finalcorner`
//! считаются по РЕАЛЬНОЙ геометрии курса, а не синтетике.
//!
//! Формат повторяет UmaLator (`CourseData`): на курс — массивы corners{start,len},
//! slopes{start,len,slope}, straights{start,end}. Таблица грузится один раз и
//! индексируется по `course_id` гонки (все ~138 трасс сразу, без кода на трек).
//!
//! ИСТОЧНИК ДАННЫХ (dev): course_data.json из uma-skill-tools (GPLv3) — для
//! разработки/валидации, в MIT-релиз НЕ бандлится. Для раздачи — своя копия из игры.

use std::collections::HashMap;
use std::path::Path;

use serde::Deserialize;

#[derive(Deserialize)]
struct RawCorner {
    start: f64,
    length: f64,
}
#[derive(Deserialize)]
struct RawSlope {
    start: f64,
    length: f64,
    slope: f64,
}
#[derive(Deserialize)]
struct RawStraight {
    start: f64,
    end: f64,
}
#[derive(Deserialize)]
struct RawCourse {
    distance: f64,
    #[serde(rename = "raceTrackId")]
    race_track_id: i32,
    turn: i32,
    #[serde(default)]
    corners: Vec<RawCorner>,
    #[serde(default)]
    slopes: Vec<RawSlope>,
    #[serde(default)]
    straights: Vec<RawStraight>,
}

/// Геометрия одного курса (позиции в метрах от старта).
#[derive(Clone, Debug, Default)]
pub struct CourseGeom {
    pub distance: f64,
    pub race_track_id: i32,
    /// 1 правый, 2 левый поворот (как в игре).
    pub turn: i32,
    /// Углы: (начало, конец).
    pub corners: Vec<(f64, f64)>,
    /// Уклоны: (начало, конец, знаковое значение; >0 подъём, <0 спуск).
    pub slopes: Vec<(f64, f64, f64)>,
    /// Прямые: (начало, конец).
    pub straights: Vec<(f64, f64)>,
}

impl CourseGeom {
    pub fn in_corner(&self, pos: f64) -> bool {
        self.corners.iter().any(|&(s, e)| pos >= s && pos <= e)
    }
    /// В ПОСЛЕДНЕМ углу трассы (финальный угол перед домашней прямой).
    pub fn is_final_corner(&self, pos: f64) -> bool {
        self.corners.last().is_some_and(|&(s, e)| pos >= s && pos <= e)
    }
    pub fn in_straight(&self, pos: f64) -> bool {
        self.straights.iter().any(|&(s, e)| pos >= s && pos <= e)
    }
    /// DSL-вид уклона: 0 ровно, 1 подъём, 2 спуск.
    pub fn slope_kind(&self, pos: f64) -> f64 {
        for &(s, e, v) in &self.slopes {
            if pos >= s && pos <= e {
                return if v > 0.0 {
                    1.0
                } else if v < 0.0 {
                    2.0
                } else {
                    0.0
                };
            }
        }
        0.0
    }
}

/// Грузит таблицу всех курсов: course_id -> геометрия.
pub fn load_courses(path: &Path) -> Option<HashMap<i32, CourseGeom>> {
    let text = std::fs::read_to_string(path).ok()?;
    let raw: HashMap<String, RawCourse> = serde_json::from_str(&text).ok()?;
    let mut out = HashMap::new();
    for (id, c) in raw {
        let Ok(id) = id.parse::<i32>() else { continue };
        out.insert(
            id,
            CourseGeom {
                distance: c.distance,
                race_track_id: c.race_track_id,
                turn: c.turn,
                corners: c.corners.iter().map(|x| (x.start, x.start + x.length)).collect(),
                slopes: c.slopes.iter().map(|x| (x.start, x.start + x.length, x.slope)).collect(),
                straights: c.straights.iter().map(|x| (x.start, x.end)).collect(),
            },
        );
    }
    Some(out)
}
