//! Пишет снимок состояния гонки в JSON-файл, который читает отдельное
//! приложение-оверлей. Атомарная запись (temp + rename), ~20 раз в секунду.

use std::io::Write;
use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::state::lock_race;

pub fn state_path() -> PathBuf {
    let base = std::env::var("TEMP").unwrap_or_else(|_| ".".into());
    PathBuf::from(base).join("uma_race_overlay_state.json")
}

pub(crate) fn json_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

fn build_json() -> String {
    let race = lock_race();
    let running = race
        .last_update
        .map_or(false, |t| t.elapsed() < Duration::from_secs(3));
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);

    // Возраст последней пачки конструкторов: app по нему показывает таблицу
    // (с шансами победы) ещё ДО старта, когда running ещё false.
    let ctor_age_ms = race
        .last_ctor
        .map(|t| t.elapsed().as_millis() as u64)
        .unwrap_or(u64::MAX);

    let mut s = String::with_capacity(2048);
    s.push_str(&format!(
        "{{\"ts\":{},\"running\":{},\"ctor_age_ms\":{},\"course_id\":{},\"race_track_id\":{},\"course_distance\":{},\"course_ground\":{},\"race_type\":{},\"race_instance_id\":{},\"horses\":[",
        ts, running, ctor_age_ms,
        race.course_id, race.race_track_id, race.course_distance,
        race.course_ground, race.race_type, race.race_instance_id
    ));
    let mut first = true;
    for h in race.horses.values() {
        if !first {
            s.push(',');
        }
        first = false;
        // Скиллы: компактный массив пар [id,level].
        let mut skills_json = String::from("[");
        for (i, (id, lvl)) in h.skills.iter().enumerate() {
            if i > 0 {
                skills_json.push(',');
            }
            skills_json.push_str(&format!("[{},{}]", id, lvl));
        }
        skills_json.push(']');

        s.push_str(&format!(
            "{{\"gate\":{},\"name\":\"{}\",\"trainer\":\"{}\",\"hp\":{:.1},\"max_hp\":{:.1},\"speed\":{:.2},\"accel\":{:.3},\"max_spurt_accel\":{:.3},\"max_spurt_speed\":{:.2},\"distance\":{:.1},\"spurt\":{},\"finished\":{},\"order\":{},\"style\":{},\"stats\":[{},{},{},{},{}],\"apt\":[{},{},{},{},{}],\"ground\":[{},{}],\"adist\":{},\"aground\":{},\"gtype\":{},\"is_user\":{},\"motiv\":{},\"pop\":{},\"stats_ready\":{},\"blocked_time\":{:.2},\"pre_spurt_blocked_time\":{:.2},\"blocked_episodes\":{},\"kakari_time\":{:.2},\"finish_time\":{:.3},\"finish_diff_time\":{:.3},\"blocked_lost_dist\":{:.1},\"blocked_lost_time\":{:.2},\"spurt_blocked_time\":{:.2},\"spurt_blocked_episodes\":{},\"spurt_lost_dist\":{:.1},\"spurt_lost_time\":{:.2},\"spurt_unresolved\":{},\"skills\":{}}}",
            h.gate_no,
            json_escape(&h.chara_name),
            json_escape(&h.trainer_name),
            h.hp,
            h.max_hp,
            h.speed,
            h.accel,
            h.max_spurt_accel,
            h.max_spurt_speed,
            h.distance,
            h.is_last_spurt,
            h.finished,
            h.finish_order,
            h.running_style,
            h.stat_speed,
            h.stat_stamina,
            h.stat_pow,
            h.stat_guts,
            h.stat_wiz,
            // apt: short, mile, middle, long, свой стиль (1=G..8=S)
            h.apt_short,
            h.apt_mile,
            h.apt_middle,
            h.apt_long,
            h.apt_style,
            // ground apt: turf, dirt
            h.apt_turf,
            h.apt_dirt,
            h.active_dist_apt,
            h.active_ground_apt,
            h.ground_type,
            h.is_user,
            h.motivation,
            h.popularity,
            h.stats_ready,
            h.blocked_time,
            h.pre_spurt_blocked_time,
            h.blocked_episodes,
            h.kakari_time,
            h.finish_time,
            h.finish_diff_time,
            h.blocked_lost_dist,
            h.blocked_lost_time,
            h.spurt_blocked_time,
            h.spurt_blocked_episodes,
            h.spurt_lost_dist,
            h.spurt_lost_time,
            h.spurt_unresolved,
            skills_json,
        ));
    }
    s.push_str("]}");
    s
}

fn write_atomic(path: &PathBuf, data: &str) -> std::io::Result<()> {
    let tmp = path.with_extension("json.tmp");
    {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(data.as_bytes())?;
        f.flush()?;
    }
    std::fs::rename(&tmp, path)
}

pub fn writer_thread() {
    let path = state_path();
    loop {
        let json = build_json();
        let _ = write_atomic(&path, &json);
        std::thread::sleep(Duration::from_millis(50));
    }
}
