//! Архив завершённой гонки для офлайн-реверса: meta + лошади (identity/stats/skills/
//! results) + покадровый таймлайн (d/v/hp/lane/block/temp). Пишется один раз на финише
//! в `%LOCALAPPDATA%\uma_race_overlay_races\race_<ts>_<room|bots>_<instance>.json`.
//!
//! По таймлайну видно, ЧТО решало гонку: рывки скорости (= сработал скилл), смены
//! места (обгоны), просадки HP. Разделение room match (PvP) от ботов — по числу
//! РАЗНЫХ непустых тренеров (у NPC тренер пустой) + по `race_type`.

use crate::frames;
use crate::il2cpp::RawPtr;
use crate::logger::logf;
use crate::state::{HorseState, RaceState};
use crate::writer::json_escape;
use std::path::PathBuf;

fn races_dir() -> PathBuf {
    let base = std::env::var("LOCALAPPDATA").unwrap_or_else(|_| ".".into());
    PathBuf::from(base).join("uma_race_overlay_races")
}

/// f32-массив в JSON: конечные — с 2 знаками, NaN/inf → null.
fn push_f32(s: &mut String, v: &[f32]) {
    for (i, x) in v.iter().enumerate() {
        if i > 0 {
            s.push(',');
        }
        if x.is_finite() {
            s.push_str(&format!("{:.2}", x));
        } else {
            s.push_str("null");
        }
    }
}

fn push_u8(s: &mut String, v: &[u8]) {
    for (i, x) in v.iter().enumerate() {
        if i > 0 {
            s.push(',');
        }
        s.push_str(&x.to_string());
    }
}

/// Индекс кадра, ближайшего по времени к `t` (для привязки событий к таймлайну).
fn nearest_frame(times: &[f32], t: f32) -> usize {
    let mut frame = 0usize;
    let mut best = f32::MAX;
    for (fi, &tm) in times.iter().enumerate() {
        let dd = (tm - t).abs();
        if dd < best {
            best = dd;
            frame = fi;
        }
    }
    frame
}

/// Записать архив завершённой гонки. Тихо выходит, если данных нет.
pub fn write_race(race: &RaceState, reader: RawPtr) {
    let Some(tl) = frames::collect_timeline(reader) else {
        logf!("archive: no timeline (reader {:p})", reader);
        return;
    };
    let dir = races_dir();
    if let Err(e) = std::fs::create_dir_all(&dir) {
        logf!("archive: mkdir failed: {}", e);
        return;
    }

    // room match (PvP) vs боты: число РАЗНЫХ непустых тренеров.
    let mut trainers: Vec<&str> = race
        .horses
        .values()
        .map(|h| h.trainer_name.as_str())
        .filter(|t| !t.is_empty())
        .collect();
    trainers.sort_unstable();
    trainers.dedup();
    let is_room = trainers.len() >= 2;
    let kind = if is_room { "room" } else { "bots" };

    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let path = dir.join(format!("race_{}_{}_{}.json", ts, kind, race.race_instance_id));

    let mut s = String::with_capacity(128 * 1024);
    s.push_str(&format!(
        "{{\"ts\":{},\"course_id\":{},\"track\":{},\"distance\":{},\"ground\":{},\"race_type\":{},\"instance\":{},\"is_room_match\":{},\"distinct_trainers\":{},",
        ts, race.course_id, race.race_track_id, race.course_distance, race.course_ground,
        race.race_type, race.race_instance_id, is_room, trainers.len()
    ));
    s.push_str("\"frame_t\":[");
    push_f32(&mut s, &tl.times);
    s.push_str("],\"horses\":[");

    let mut hs: Vec<&HorseState> = race.horses.values().collect();
    hs.sort_by_key(|h| h.gate_no);
    for (hi, h) in hs.iter().enumerate() {
        if hi > 0 {
            s.push(',');
        }
        let mut skills = String::from("[");
        for (i, (id, lvl)) in h.skills.iter().enumerate() {
            if i > 0 {
                skills.push(',');
            }
            skills.push_str(&format!("[{},{}]", id, lvl));
        }
        skills.push(']');
        s.push_str(&format!(
            "{{\"gate\":{},\"name\":\"{}\",\"trainer\":\"{}\",\"is_user\":{},\"style\":{},\"stats\":[{},{},{},{},{}],\"apt\":[{},{},{},{},{}],\"finish_order\":{},\"finish_time\":{:.3},\"finish_diff_time\":{:.3},\"motiv\":{},\"pop\":{},\"skills\":{}",
            h.gate_no,
            json_escape(&h.chara_name),
            json_escape(&h.trainer_name),
            h.is_user,
            h.running_style,
            h.stat_speed, h.stat_stamina, h.stat_pow, h.stat_guts, h.stat_wiz,
            h.apt_short, h.apt_mile, h.apt_middle, h.apt_long, h.apt_style,
            h.finish_order, h.finish_time, h.finish_diff_time, h.motivation, h.popularity,
            skills
        ));
        let ti = (h.gate_no - 1) as usize;
        if let Some(tr) = tl.horses.get(ti) {
            s.push_str(",\"d\":[");
            push_f32(&mut s, &tr.d);
            s.push_str("],\"v\":[");
            push_f32(&mut s, &tr.v);
            s.push_str("],\"hp\":[");
            push_f32(&mut s, &tr.hp);
            s.push_str("],\"lane\":[");
            push_f32(&mut s, &tr.lane);
            s.push_str("],\"block\":[");
            push_u8(&mut s, &tr.block);
            s.push_str("],\"temp\":[");
            push_u8(&mut s, &tr.temp);
            s.push(']');
            // Настоящие активации скиллов (ground truth) этой лошади: время события
            // → ближайший кадр → дистанция из таймлайна. {f:кадр, d:метры, id:skill}.
            s.push_str(",\"act\":[");
            let mut first = true;
            for ev in tl.events.iter().filter(|e| e.horse == ti) {
                let frame = nearest_frame(&tl.times, ev.time);
                let dist = tr.d.get(frame).copied().filter(|x| x.is_finite()).unwrap_or(0.0);
                if !first {
                    s.push(',');
                }
                first = false;
                s.push_str(&format!("{{\"f\":{},\"d\":{:.2},\"id\":{}}}", frame, dist, ev.skill_id));
            }
            s.push(']');
            // Контесты (ground truth): "struggle" = Spot Struggle (type 4), "duel" =
            // Dueling (type 5). Старт-события участника: {f:кадр, d:метры}. Партнёров
            // приложение группирует по совпадающему кадру старта.
            for (key, kind) in [(",\"struggle\":[", 4u8), (",\"duel\":[", 5u8)] {
                s.push_str(key);
                let mut first = true;
                for ev in tl.contests.iter().filter(|e| e.horse == ti && e.kind == kind) {
                    let frame = nearest_frame(&tl.times, ev.time);
                    let dist = tr.d.get(frame).copied().filter(|x| x.is_finite()).unwrap_or(0.0);
                    if !first {
                        s.push(',');
                    }
                    first = false;
                    s.push_str(&format!("{{\"f\":{},\"d\":{:.2}}}", frame, dist));
                }
                s.push(']');
            }
        }
        s.push('}');
    }
    s.push_str("]}");

    match std::fs::write(&path, &s) {
        Ok(_) => logf!("archive: wrote {} ({} KB, {})", path.display(), s.len() / 1024, kind),
        Err(e) => logf!("archive: write failed: {}", e),
    }
}
