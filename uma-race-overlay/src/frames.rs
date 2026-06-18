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
    /// Абсолютное финишное время (сек) — для контрфактуального ранжирования.
    pub finish_time: f32,
    /// ВНИМАНИЕ: это `FinishDiffTime` = отрыв до лошади на ОДНО МЕСТО впереди
    /// (margin), а НЕ до победителя. Для ранжирования использовать `finish_time`.
    pub finish_diff_time: f32,
    /// Суммарное время во фронт-блоке (сек).
    pub blocked_time: f32,
    /// Суммарное время во фронт-блоке ДО начала спурта (сек), «mid leg block».
    pub pre_spurt_blocked_time: f32,
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
            acc[idx].finish_time = il2cpp::read_field(r, 0x14);
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
                    if !in_spurt {
                        acc[idx].pre_spurt_blocked_time += dt;
                    }
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

/// Покадровый таймлайн ВСЕЙ гонки (для архива/реверса): времена кадров + по каждой
/// лошади столбцы d/v/hp/lane/block/temp. Индекс лошади = индекс в кадровом массиве
/// (= gate-1). None — данные не прочитались.
pub struct HorseTrack {
    pub d: Vec<f32>,
    pub v: Vec<f32>,
    pub hp: Vec<f32>,
    pub lane: Vec<f32>,
    pub block: Vec<u8>,
    pub temp: Vec<u8>,
}

/// Настоящая активация скилла из игры (_simEvDataList). Индекс лошади — тот же
/// индекс кадрового массива, что и у таймлайна (= gate-1).
pub struct SkillEvent {
    pub horse: usize,
    pub time: f32,
    pub skill_id: i32,
}

/// Контест-событие из `_simEvDataList`: type==4 Spot Struggle (位置取り争い,
/// фронт-раннеры рубятся за позицию в начале), type==5 Dueling (追い比べ, дуэль
/// на финишной прямой). param[0] = индекс лошади-участника; событие на КАЖДОГО
/// участника в один и тот же момент → партнёров группируем по времени.
pub struct ContestEvent {
    pub horse: usize,
    pub time: f32,
    /// 4 = Spot Struggle, 5 = Dueling.
    pub kind: u8,
}

pub struct Timeline {
    pub times: Vec<f32>,
    pub horses: Vec<HorseTrack>,
    /// Настоящие активации скиллов (ground truth). Пусто — событий не было/не прочлось.
    pub events: Vec<SkillEvent>,
    /// Контесты (Spot Struggle / Dueling) — ground truth из событий движка.
    pub contests: Vec<ContestEvent>,
}

/// Читает реальные активации скиллов из `_simData._simEvDataList` (+0x28).
/// Раскладка события (RaceSimulateEventData): frameTime(f32)@0x10, type(i32)@0x14,
/// param(int[])@0x18. type==3 + param[2]!=-1 = активация: param=[horseIdx, skillId,
/// duration, 0, 1<<idx]. t=0 с param[2]==-1 — пред-стартовая регистрация скиллов
/// (НЕ активация), пропускаем.
fn collect_skill_events(sim: RawPtr, nh: usize) -> Vec<SkillEvent> {
    let mut out = Vec::new();
    unsafe {
        let ev_list: RawPtr = il2cpp::read_field(sim, 0x28);
        if ev_list.is_null() {
            return out;
        }
        let items: RawPtr = il2cpp::read_field(ev_list, 0x10);
        let n = il2cpp::array_length(items);
        for k in 0..n {
            let ev = il2cpp::array_get_ref(items, k);
            if ev.is_null() {
                continue;
            }
            let ty: i32 = il2cpp::read_field(ev, 0x14);
            if ty != 3 {
                continue;
            }
            let param: RawPtr = il2cpp::read_field(ev, 0x18);
            if il2cpp::array_length(param) < 3 {
                continue;
            }
            let horse: i32 = il2cpp::read_field(param, 0x20);
            let skill_id: i32 = il2cpp::read_field(param, 0x24);
            let dur: i32 = il2cpp::read_field(param, 0x28);
            // pre-race регистрация (dur==-1) — не активация.
            if dur < 0 {
                continue;
            }
            if horse < 0 || horse as usize >= nh || skill_id <= 0 {
                continue;
            }
            let time: f32 = il2cpp::read_field(ev, 0x10);
            out.push(SkillEvent { horse: horse as usize, time, skill_id });
        }
    }
    out
}

/// Контесты из `_simEvDataList`: type==4 Spot Struggle, type==5 Dueling. У них
/// param длиной 1 — param[0] = индекс лошади-участника (тот же sim-индекс, что у
/// таймлайна = gate-1). На каждого участника эмитится отдельное событие в один и
/// тот же `frameTime`. Подтверждено реальной гонкой: type 4 @≈7.86s для двух nige
/// в 2.6 м друг от друга (Spot Struggle), type 5 на финишной прямой (Dueling).
fn collect_contest_events(sim: RawPtr, nh: usize) -> Vec<ContestEvent> {
    let mut out = Vec::new();
    unsafe {
        let ev_list: RawPtr = il2cpp::read_field(sim, 0x28);
        if ev_list.is_null() {
            return out;
        }
        let items: RawPtr = il2cpp::read_field(ev_list, 0x10);
        let n = il2cpp::array_length(items);
        for k in 0..n {
            let ev = il2cpp::array_get_ref(items, k);
            if ev.is_null() {
                continue;
            }
            let ty: i32 = il2cpp::read_field(ev, 0x14);
            if ty != 4 && ty != 5 {
                continue;
            }
            let param: RawPtr = il2cpp::read_field(ev, 0x18);
            if il2cpp::array_length(param) < 1 {
                continue;
            }
            let horse: i32 = il2cpp::read_field(param, 0x20);
            if horse < 0 || horse as usize >= nh {
                continue;
            }
            let time: f32 = il2cpp::read_field(ev, 0x10);
            out.push(ContestEvent { horse: horse as usize, time, kind: ty as u8 });
        }
    }
    out
}

pub fn collect_timeline(reader: RawPtr) -> Option<Timeline> {
    if reader.is_null() {
        return None;
    }
    unsafe {
        let sim: RawPtr = il2cpp::read_field(reader, 0x10);
        if sim.is_null() {
            return None;
        }
        let list: RawPtr = il2cpp::read_field(sim, 0x18);
        if list.is_null() {
            return None;
        }
        let frames: RawPtr = il2cpp::read_field(list, 0x10);
        let nframes = il2cpp::array_length(frames);
        if nframes == 0 {
            return None;
        }
        let frame0 = il2cpp::array_get_ref(frames, 0);
        if frame0.is_null() {
            return None;
        }
        let harr0: RawPtr = il2cpp::read_field(frame0, 0x18);
        let nh = il2cpp::array_length(harr0);
        if nh == 0 {
            return None;
        }
        let hcls = il2cpp::array_element_class(harr0);
        let is_vt = il2cpp::is_valuetype(hcls);
        let stride = il2cpp::array_element_size(harr0);
        let adj: usize = if is_vt { 0x10 } else { 0 };

        let mut times = Vec::with_capacity(nframes);
        let mut horses: Vec<HorseTrack> = (0..nh)
            .map(|_| HorseTrack {
                d: Vec::with_capacity(nframes),
                v: Vec::with_capacity(nframes),
                hp: Vec::with_capacity(nframes),
                lane: Vec::with_capacity(nframes),
                block: Vec::with_capacity(nframes),
                temp: Vec::with_capacity(nframes),
            })
            .collect();

        for f in 0..nframes {
            let frame = il2cpp::array_get_ref(frames, f);
            if frame.is_null() {
                continue;
            }
            let time: f32 = il2cpp::read_field(frame, 0x10);
            times.push(time);
            let harr: RawPtr = il2cpp::read_field(frame, 0x18);
            let cnt = if harr.is_null() { 0 } else { il2cpp::array_length(harr).min(nh) };
            for idx in 0..nh {
                let t = &mut horses[idx];
                let base = if idx < cnt {
                    il2cpp::array_elem_base(harr, idx, stride, is_vt)
                } else {
                    std::ptr::null_mut()
                };
                if base.is_null() {
                    // выравниваем длины столбцов с times
                    t.d.push(f32::NAN);
                    t.v.push(f32::NAN);
                    t.hp.push(f32::NAN);
                    t.lane.push(f32::NAN);
                    t.block.push(0xFF);
                    t.temp.push(0);
                    continue;
                }
                t.d.push(il2cpp::read_field(base, 0x10 - adj));
                t.lane.push(il2cpp::read_field(base, 0x14 - adj));
                t.v.push(il2cpp::read_field(base, 0x18 - adj));
                t.hp.push(il2cpp::read_field(base, 0x1c - adj));
                t.temp.push(il2cpp::read_field(base, 0x20 - adj));
                t.block.push(il2cpp::read_field(base, 0x21 - adj));
            }
        }
        let events = collect_skill_events(sim, nh);
        let contests = collect_contest_events(sim, nh);
        Some(Timeline { times, horses, events, contests })
    }
}
