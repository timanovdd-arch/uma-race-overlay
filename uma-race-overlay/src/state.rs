use std::collections::HashMap;
use std::sync::{LazyLock, Mutex, MutexGuard};
use std::time::Instant;

#[derive(Clone)]
pub struct HorseState {
    pub gate_no: i32,
    pub chara_name: String,
    pub trainer_name: String,
    pub hp: f32,
    pub max_hp: f32,
    pub hp_pct: f32,
    pub speed: f32,
    /// Текущее ускорение (m/s^2), сглаженное.
    pub accel: f32,
    /// Максимальное ускорение, достигнутое во время last spurt.
    pub max_spurt_accel: f32,
    pub distance: f32,
    pub is_last_spurt: bool,
    pub finished: bool,
    pub finish_order: i32,
    pub last_update: Instant,
    // --- статы из HorseData (известны до старта, для расчёта шанса победы) ---
    /// Статы: скорость, выносливость, сила, упорство, ум. -1 = не прочитались.
    pub stat_speed: i32,
    pub stat_stamina: i32,
    pub stat_pow: i32,
    pub stat_guts: i32,
    pub stat_wiz: i32,
    /// Стиль бега: 1 nige, 2 senko, 3 sashi, 4 oikomi. -1 = неизвестен.
    pub running_style: i32,
    /// Аптитуды дистанций (1=G..8=S): short, mile, middle, long. -1 = неизвестно.
    pub apt_short: i32,
    pub apt_mile: i32,
    pub apt_middle: i32,
    pub apt_long: i32,
    /// Аптитуд к СВОЕМУ стилю бега (1=G..8=S).
    pub apt_style: i32,
    /// Аптитуды поверхности (1=G..8=S): трава, грунт.
    pub apt_turf: i32,
    pub apt_dirt: i32,
    /// Аптитуд (1=G..8=S) для ФАКТИЧЕСКОЙ поверхности/дистанции этой гонки.
    pub active_ground_apt: i32,
    pub active_dist_apt: i32,
    /// Тип трассы: 1 турф, 2 грунт, 0 неизвестно.
    pub ground_type: i32,
    /// Мотивация (やる気): 1..5, 5 — лучшая. -1 = неизвестно.
    pub motivation: i32,
    /// Популярность игры (фаворитизм): меньше = фаворит. -1 = неизвестно.
    pub popularity: i32,
    /// Скиллы лошади: (skill_id, level). Эффекты берутся из master.mdb в app.
    pub skills: Vec<(i32, i32)>,
}

impl HorseState {
    pub fn new(gate_no: i32, chara_name: String, trainer_name: String) -> Self {
        Self {
            gate_no,
            chara_name,
            trainer_name,
            hp: 0.0,
            max_hp: 0.0,
            hp_pct: 0.0,
            speed: 0.0,
            accel: 0.0,
            max_spurt_accel: 0.0,
            distance: 0.0,
            is_last_spurt: false,
            finished: false,
            finish_order: -1,
            last_update: Instant::now(),
            stat_speed: -1,
            stat_stamina: -1,
            stat_pow: -1,
            stat_guts: -1,
            stat_wiz: -1,
            running_style: -1,
            apt_short: -1,
            apt_mile: -1,
            apt_middle: -1,
            apt_long: -1,
            apt_style: -1,
            apt_turf: -1,
            apt_dirt: -1,
            active_ground_apt: -1,
            active_dist_apt: -1,
            ground_type: 0,
            motivation: -1,
            popularity: -1,
            skills: Vec::new(),
        }
    }
}

pub struct RaceState {
    /// Ключ — указатель на экземпляр HorseRaceInfoReplay.
    pub horses: HashMap<usize, HorseState>,
    pub last_ctor: Option<Instant>,
    pub last_update: Option<Instant>,
}

pub static RACE: LazyLock<Mutex<RaceState>> = LazyLock::new(|| {
    Mutex::new(RaceState {
        horses: HashMap::new(),
        last_ctor: None,
        last_update: None,
    })
});

pub fn lock_race() -> MutexGuard<'static, RaceState> {
    RACE.lock().unwrap_or_else(|e| e.into_inner())
}
