//! Разбор предрассчитанных покадровых данных гонки (RaceSimulateReader._simData)
//! для пост-разбора: сколько времени лошадь была в блоке, сколько эпизодов,
//! сколько в закидывании (掛かり), и финишный отрыв.
//!
//! Гонка в Umamusume предрассчитана при загрузке (ImportBase64), поэтому ВСЕ
//! кадры всей гонки уже лежат в _simData. Мы проходим их один раз на финише.
//!
//! Раскладка (подтверждена дампом классов в логе):
//!   RaceSimulateReader            +0x10 _simData
//!   RaceSimulateData              +0x18 _frameDataList(List<FrameData>)
//!                                 +0x20 _horseResultDataArray
//!   List<T>                       +0x10 _items (backing array)
//!   RaceSimulateFrameData (class) +0x10 Time(f32) +0x18 HorseDataArray
//!   RaceSimulateHorseFrameData    +0x10 Distance +0x14 LanePosition +0x18 Speed
//!                                 +0x1c Hp +0x20 TemptationMode(u8)
//!                                 +0x21 BlockFrontHorseIndex(u8, NULL = свободна)
//!   RaceSimulateHorseResultData   +0x10 FinishOrder +0x1c FinishDiffTime
//!
//! ВАЖНО: HorseFrameData может быть value-типом (struct) — тогда field offset
//! метаданных включает 0x10 заголовка, и при инлайн-чтении его надо вычитать.

use crate::il2cpp::{self, RawPtr};

#[derive(Clone, Default)]
pub struct Accum {
    /// Финишное место по данным симуляции (для валидации маппинга idx→gate).
    pub finish_order: i32,
    /// Отрыв до победителя (сек).
    pub finish_diff_time: f32,
    /// Суммарное время во фронт-блоке (сек).
    pub blocked_time: f32,
    /// Число отдельных эпизодов блока.
    pub episodes: i32,
    /// Время в закидывании (掛かり), сек.
    pub kakari_time: f32,
    /// Оценка потерянной из-за блока дистанции (м): ∫(v_до_блока − v_в_блоке)·dt.
    pub lost_dist: f32,
    /// Оценка потерянного из-за блока времени (сек): ∫(деф/v)·dt.
    pub lost_time: f32,
    // --- блок ВО ВРЕМЯ last spurt (только он реально гасит ускорение) ---
    /// Время во фронт-блоке в фазе спурта (сек).
    pub spurt_blocked_time: f32,
    /// Число эпизодов блока в спурте.
    pub spurt_episodes: i32,
    /// Потерянная дистанция из-за блока в спурте (м).
    pub spurt_lost_dist: f32,
    /// Потерянное время из-за блока в спурте (сек).
    pub spurt_lost_time: f32,
    /// Лошадь была в блоке В СПУРТЕ вплоть до финиша → её «свободную» скорость
    /// из этой гонки узнать нельзя, оценка потери занижена (помечаем неопределённой).
    pub spurt_unresolved: bool,
}

/// Считает per-horse статистику. Индекс результата = индекс лошади в кадровом
/// массиве (как правило gate-1). Пустой Vec — данные не прочитались.
pub fn compute(reader: RawPtr) -> Vec<Accum> {
    if reader.is_null() {
        return Vec::new();
    }
    unsafe {
        let sim: RawPtr = il2cpp::read_field(reader, 0x10);
        if sim.is_null() {
            return Vec::new();
        }
        let list: RawPtr = il2cpp::read_field(sim, 0x18);
        if list.is_null() {
            return Vec::new();
        }
        let frames: RawPtr = il2cpp::read_field(list, 0x10);
        let nframes = il2cpp::array_length(frames);
        if nframes == 0 {
            return Vec::new();
        }

        // Раскладку per-horse элемента определяем по первому кадру.
        let frame0 = il2cpp::array_get_ref(frames, 0);
        if frame0.is_null() {
            return Vec::new();
        }
        let harr0: RawPtr = il2cpp::read_field(frame0, 0x18);
        let nh = il2cpp::array_length(harr0);
        if nh == 0 {
            return Vec::new();
        }
        let hcls = il2cpp::array_element_class(harr0);
        let is_vt = il2cpp::is_valuetype(hcls);
        let stride = il2cpp::array_element_size(harr0);
        // Для value-типа поля читаем по (offset - 0x10) от инлайн-данных.
        let adj: usize = if is_vt { 0x10 } else { 0 };

        let mut acc = vec![Accum::default(); nh];

        // Итоги (финиш + точка старта спурта) — нужны ДО цикла, чтобы знать,
        // какой кадр уже в фазе спурта. _horseResultDataArray (+0x20).
        let res: RawPtr = il2cpp::read_field(sim, 0x20);
        let nr = il2cpp::array_length(res).min(nh);
        // Точка старта спурта по лошади (метры). MAX = неизвестна (см. фолбэк).
        let mut spurt_start = vec![f32::MAX; nh];
        for idx in 0..nr {
            let r = il2cpp::array_get_ref(res, idx);
            if r.is_null() {
                continue;
            }
            acc[idx].finish_order = il2cpp::read_field(r, 0x10);
            acc[idx].finish_diff_time = il2cpp::read_field(r, 0x1c);
            // LastSpurtStartDistance(+0x28): тип точно не известен — пробуем как
            // f32 и i32, берём то, что попадает в разумный диапазон дистанции.
            let lf: f32 = il2cpp::read_field(r, 0x28);
            let li: i32 = il2cpp::read_field(r, 0x28);
            spurt_start[idx] = if (200.0..=4000.0).contains(&lf) {
                lf
            } else if (200..=4000).contains(&li) {
                li as f32
            } else {
                f32::MAX
            };
        }
        // Фолбэк для неизвестной точки спурта: последняя 1/6 трассы (фаза 3).
        // Дистанцию трассы берём как макс. дистанцию в последнем кадре.
        if spurt_start.iter().any(|s| !s.is_finite() || *s == f32::MAX) {
            let last = il2cpp::array_get_ref(frames, nframes - 1);
            if !last.is_null() {
                let harr: RawPtr = il2cpp::read_field(last, 0x18);
                let cnt = il2cpp::array_length(harr).min(nh);
                let mut race_dist = 0.0f32;
                for idx in 0..cnt {
                    let base = il2cpp::array_elem_base(harr, idx, stride, is_vt);
                    if base.is_null() {
                        continue;
                    }
                    let d: f32 = il2cpp::read_field(base, 0x10 - adj);
                    race_dist = race_dist.max(d);
                }
                let thr = if race_dist > 0.0 { race_dist * 5.0 / 6.0 } else { f32::MAX };
                for s in spurt_start.iter_mut() {
                    if !s.is_finite() || *s == f32::MAX {
                        *s = thr;
                    }
                }
            }
        }

        let mut blocked_prev = vec![false; nh];
        let mut sblocked_prev = vec![false; nh];
        let mut prev_speed = vec![0.0f32; nh]; // скорость на прошлом кадре
        let mut vref = vec![0.0f32; nh]; // скорость на входе в блок (общий)
        // Буфер текущего эпизода спурт-блока (для оценки по «скорости освобождения»).
        let mut sb_entry = vec![0.0f32; nh]; // скорость на входе в блок
        let mut sb_dur = vec![0.0f32; nh]; // длительность эпизода
        let mut sb_dist = vec![0.0f32; nh]; // пройдено за эпизод (∫v·dt)
        let mut prev_time: f32 = 0.0;

        for f in 0..nframes {
            let frame = il2cpp::array_get_ref(frames, f);
            if frame.is_null() {
                continue;
            }
            let time: f32 = il2cpp::read_field(frame, 0x10);
            let dt = if f == 0 { 0.0 } else { (time - prev_time).max(0.0) };
            prev_time = time;

            let harr: RawPtr = il2cpp::read_field(frame, 0x18);
            if harr.is_null() {
                continue;
            }
            let cnt = il2cpp::array_length(harr).min(nh);
            for idx in 0..cnt {
                let base = il2cpp::array_elem_base(harr, idx, stride, is_vt);
                if base.is_null() {
                    continue;
                }
                let distance: f32 = il2cpp::read_field(base, 0x10 - adj);
                let speed: f32 = il2cpp::read_field(base, 0x18 - adj);
                let temptation: u8 = il2cpp::read_field(base, 0x20 - adj);
                let block: u8 = il2cpp::read_field(base, 0x21 - adj);
                // Блок = валидный индекс соперника спереди (NULL-сентинел = 0xFF).
                let is_blocked = block != 0xFF && (block as usize) < nh;
                let in_spurt = distance >= spurt_start[idx];

                // Полный блок (любая фаза) — для справки.
                if is_blocked {
                    acc[idx].blocked_time += dt;
                    if !blocked_prev[idx] {
                        acc[idx].episodes += 1;
                        vref[idx] = prev_speed[idx].max(speed);
                    }
                    let v = vref[idx];
                    if v > 0.1 {
                        let deficit = (v - speed).max(0.0);
                        acc[idx].lost_dist += deficit * dt;
                        acc[idx].lost_time += (deficit / v) * dt;
                    }
                }
                blocked_prev[idx] = is_blocked;

                // Блок В СПУРТЕ — главное: глушит ускорение на решающей стадии.
                // Потерю оцениваем по «скорости освобождения»: эпизод копим, а на
                // снятии блока опорная скорость = max(вход, скорость сразу после
                // освобождения) — лошадь там рвёт вперёд, это и есть «что задушили».
                let s_blocked = is_blocked && in_spurt;
                if s_blocked {
                    acc[idx].spurt_blocked_time += dt;
                    if !sblocked_prev[idx] {
                        acc[idx].spurt_episodes += 1;
                        sb_entry[idx] = prev_speed[idx].max(speed);
                        sb_dur[idx] = 0.0;
                        sb_dist[idx] = 0.0;
                    }
                    sb_dur[idx] += dt;
                    sb_dist[idx] += speed * dt;
                } else if sblocked_prev[idx] {
                    // Эпизод только что закончился — speed = скорость освобождения.
                    let refv = sb_entry[idx].max(speed);
                    if refv > 0.1 {
                        let ld = (refv * sb_dur[idx] - sb_dist[idx]).max(0.0);
                        acc[idx].spurt_lost_dist += ld;
                        acc[idx].spurt_lost_time += ld / refv;
                    }
                }
                sblocked_prev[idx] = s_blocked;

                prev_speed[idx] = speed;
                if temptation != 0 {
                    acc[idx].kakari_time += dt;
                }
            }
        }

        // Эпизоды спурт-блока, не снятые до финиша: «свободную» скорость узнать
        // нельзя → оцениваем по входной скорости (занижено) и помечаем неопределённым.
        for idx in 0..nh {
            if sblocked_prev[idx] {
                let refv = sb_entry[idx];
                if refv > 0.1 {
                    let ld = (refv * sb_dur[idx] - sb_dist[idx]).max(0.0);
                    acc[idx].spurt_lost_dist += ld;
                    acc[idx].spurt_lost_time += ld / refv;
                }
                acc[idx].spurt_unresolved = true;
            }
        }
        acc
    }
}
