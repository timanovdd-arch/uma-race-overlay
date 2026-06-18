//! Хуки на классы гонки.
//!
//! Подход подсмотрен у Trainers-Legend-G:
//! - ctor `Gallop.HorseRaceInfoReplay(HorseData, RaceSimulateReader)` — вызывается
//!   для каждой лошади при загрузке гонки (включая чужих в PvP), даёт identity.
//! - `Gallop.HorseRaceInfoReplay.get_RunMotionSpeed()` — вызывается каждый кадр
//!   для каждой лошади, отсюда читаем HP/скорость.

use std::ffi::c_void;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use crate::il2cpp::{self, RawPtr};
use crate::logger::logf;
use crate::state::{lock_race, HorseState};

type FnThisF32 = unsafe extern "C" fn(RawPtr) -> f32;
type FnThisI32 = unsafe extern "C" fn(RawPtr) -> i32;
type FnThisBool = unsafe extern "C" fn(RawPtr) -> bool;
type FnThisPtr = unsafe extern "C" fn(RawPtr) -> RawPtr;
type FnThisVoid = unsafe extern "C" fn(RawPtr);

#[allow(non_snake_case)]
struct RaceApi {
    GetHp: FnThisF32,
    GetMaxHp: FnThisF32,
    GetHpPer: FnThisF32,
    get_IsLastSpurt: FnThisBool,
    get_IsOverRun: FnThisBool,
    get_FinishOrder: FnThisI32,
    get_GateNo: FnThisI32,
    get_charaName: FnThisPtr,
    InitTrainerName: FnThisVoid,
    get_TrainerName: FnThisPtr,
    distance_offset: usize,
    last_speed_offset: usize,
    stats: StatApi,
}

/// Геттеры статов/аптитудов HorseData — все опциональные: если в новой версии
/// игры имена изменятся, оверлей продолжит работать, просто без шанса победы
/// (а в лог упадёт дамп методов класса, чтобы поправить имена).
#[allow(non_snake_case)]
#[derive(Default)]
struct StatApi {
    get_Speed: Option<FnThisI32>,
    get_Stamina: Option<FnThisI32>,
    get_Pow: Option<FnThisI32>,
    get_Guts: Option<FnThisI32>,
    get_Wiz: Option<FnThisI32>,
    get_RunningStyle: Option<FnThisI32>,
    get_ProperDistanceShort: Option<FnThisI32>,
    get_ProperDistanceMile: Option<FnThisI32>,
    get_ProperDistanceMiddle: Option<FnThisI32>,
    get_ProperDistanceLong: Option<FnThisI32>,
    get_ProperRunningStyleNige: Option<FnThisI32>,
    get_ProperRunningStyleSenko: Option<FnThisI32>,
    get_ProperRunningStyleSashi: Option<FnThisI32>,
    get_ProperRunningStyleOikomi: Option<FnThisI32>,
    // --- для симулятора винрейта (читаются ниже) ---
    get_ProperGroundTurf: Option<FnThisI32>,
    get_ProperGroundDirt: Option<FnThisI32>,
    get_Motivation: Option<FnThisI32>,
    get_Popularity: Option<FnThisI32>,
    get_SkillDataArray: Option<FnThisPtr>,
    // Аптитуд для ФАКТИЧЕСКОЙ поверхности/дистанции этой гонки (игра считает сама).
    get_ActiveProperGroundType: Option<FnThisI32>,
    get_ActiveProperDistance: Option<FnThisI32>,
    /// «Эта лошадь принадлежит локальному игроку» — стабильный признак «своих»,
    /// не зависит от ника тренера. Главный сигнал для фильтра свои/чужие.
    get_IsUser: Option<FnThisBool>,
}

unsafe impl Send for RaceApi {}
unsafe impl Sync for RaceApi {}

static RACE_API: OnceLock<RaceApi> = OnceLock::new();
static CTOR_ORIG: AtomicUsize = AtomicUsize::new(0);
static RUN_MOTION_ORIG: AtomicUsize = AtomicUsize::new(0);
/// Одноразовый дамп класса RaceSimulateReader (поиск дистанции/поверхности трассы).
static SKILL_DUMPED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
/// Reader текущей гонки (для пост-разбора кадров на финише). 0 = нет.
static RACE_READER: AtomicUsize = AtomicUsize::new(0);
/// Статистика блока/закидывания за текущую гонку уже посчитана.
static STATS_DONE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

unsafe extern "C" fn ctor_hook(this: RawPtr, data: RawPtr, reader: RawPtr) {
    let orig: unsafe extern "C" fn(RawPtr, RawPtr, RawPtr) =
        std::mem::transmute(CTOR_ORIG.load(Ordering::Relaxed));
    orig(this, data, reader);

    let Some(api) = RACE_API.get() else { return };

    let gate_no = (api.get_GateNo)(data);
    let chara_name = il2cpp::read_string((api.get_charaName)(data));
    (api.InitTrainerName)(data);
    let trainer_name = il2cpp::read_string((api.get_TrainerName)(data));

    // Статы/аптитуды для расчёта шанса победы. Доступны уже при загрузке гонки.
    let geti = |f: Option<FnThisI32>| f.map(|f| f(data)).unwrap_or(-1);
    let stat_speed = geti(api.stats.get_Speed);
    let stat_stamina = geti(api.stats.get_Stamina);
    let stat_pow = geti(api.stats.get_Pow);
    let stat_guts = geti(api.stats.get_Guts);
    let stat_wiz = geti(api.stats.get_Wiz);
    let running_style = geti(api.stats.get_RunningStyle);
    let apt_short = geti(api.stats.get_ProperDistanceShort);
    let apt_mile = geti(api.stats.get_ProperDistanceMile);
    let apt_middle = geti(api.stats.get_ProperDistanceMiddle);
    let apt_long = geti(api.stats.get_ProperDistanceLong);
    let apt_style = match running_style {
        1 => geti(api.stats.get_ProperRunningStyleNige),
        2 => geti(api.stats.get_ProperRunningStyleSenko),
        3 => geti(api.stats.get_ProperRunningStyleSashi),
        4 => geti(api.stats.get_ProperRunningStyleOikomi),
        _ => -1,
    };
    // HP в конструкторе ещё НЕ посчитан (GetHp/GetMaxHp возвращают -1 — проверено
    // на живой игре), поэтому HP заполняется позже, в кадровом хуке. До старта
    // дистанция трассы в app берётся из дефолта (maxHP неизвестен).

    // --- Данные для симулятора винрейта ---
    let apt_turf = geti(api.stats.get_ProperGroundTurf);
    let apt_dirt = geti(api.stats.get_ProperGroundDirt);
    let motivation = geti(api.stats.get_Motivation);
    let popularity = geti(api.stats.get_Popularity);
    let skill_arr = api.stats.get_SkillDataArray.map(|f| f(data)).unwrap_or(std::ptr::null_mut());
    // SkillData: поля skill_id (+0x10), level (+0x14) — подтверждено дампом.
    let skill_count = unsafe { il2cpp::array_length(skill_arr) };
    let mut skills: Vec<(i32, i32)> = Vec::with_capacity(skill_count);
    for i in 0..skill_count {
        let obj = unsafe { il2cpp::array_get_ref(skill_arr, i) };
        if obj.is_null() {
            continue;
        }
        let id: i32 = unsafe { il2cpp::read_field(obj, 0x10) };
        let lvl: i32 = unsafe { il2cpp::read_field(obj, 0x14) };
        if id > 0 {
            skills.push((id, lvl));
        }
    }
    // Аптитуд для ФАКТИЧЕСКОЙ поверхности/дистанции гонки (игра считает сама).
    let active_ground_apt = geti(api.stats.get_ActiveProperGroundType);
    let active_dist_apt = geti(api.stats.get_ActiveProperDistance);
    // Дедукция типа трассы: к какой поверхности относится активный аптитуд.
    // 1 = турф, 2 = грунт, 0 = не удалось (турф==грунт по аптитуду).
    let ground_type = if active_ground_apt == apt_turf && apt_turf != apt_dirt {
        1
    } else if active_ground_apt == apt_dirt && apt_dirt != apt_turf {
        2
    } else {
        0
    };
    // Стабильный признак «моя лошадь» — прямо от игры, не зависит от ника.
    let is_user = api.stats.get_IsUser.map(|f| f(data)).unwrap_or(false);
    logf!(
        "  scout: isUser {} turf {} dirt {} motiv {} pop {} skills {} aGround {} aDist {} gtype {} ids {:?}",
        is_user, apt_turf, apt_dirt, motivation, popularity, skills.len(),
        active_ground_apt, active_dist_apt, ground_type,
        skills.iter().map(|s| s.0).collect::<Vec<_>>()
    );

    // Одноразовый дамп RaceSimulateReader и его _simData (+0x10): ищем точную
    // дистанцию, тип/состояние грунта и погоду, чтобы знать их ДО старта.
    if !SKILL_DUMPED.swap(true, Ordering::Relaxed) {
        // Поиск track_id/курса: игра знает track_id (track-зелёные скиллы по нему
        // активируются), но в данных симуляции его нет. Кандидаты — HorseData
        // (ctor arg) и хукнутый объект this (HorseRaceInfoReplay).
        let dk = il2cpp::class_of(data);
        il2cpp::dump_class_methods(dk, "HorseData(full)");
        let tk = il2cpp::class_of(this);
        il2cpp::dump_class_fields(tk, "HorseRaceInfoReplay");
        il2cpp::dump_class_methods(tk, "HorseRaceInfoReplay");

        let rk = il2cpp::class_of(reader);
        il2cpp::dump_class_methods(rk, "RaceSimulateReader");
        il2cpp::dump_class_fields(rk, "RaceSimulateReader");
        let sim_data: RawPtr = unsafe { il2cpp::read_field(reader, 0x10) };
        if !sim_data.is_null() {
            let sk = il2cpp::class_of(sim_data);
            il2cpp::dump_class_fields(sk, "RaceSimulateData(_simData)");

            // ПОИСК course_id (цикл дампа): int-скан полей reader и _simData.
            // Курс в course_data имеет id вида 10xxx (Hanshin 2200 = 10906),
            // дистанция ~1000-4000. Ищем offset, где лежит такое значение.
            let scan = |obj: RawPtr, label: &str, upto: usize| {
                if obj.is_null() {
                    return;
                }
                logf!("--- int-scan {} (course_id ~10xxx / distance) ---", label);
                let mut off = 0x10usize;
                while off < upto {
                    let v: i32 = unsafe { il2cpp::read_field(obj, off) };
                    if (1000..=300000).contains(&v) {
                        logf!("  +{:#05x} = {}", off, v);
                    }
                    off += 4;
                }
            };
            scan(reader, "RaceSimulateReader", 0x60);
            scan(sim_data, "RaceSimulateData", 0xC0);

            // Header (+0x10) — метаданные гонки: вероятно курс/дистанция/условия.
            let header: RawPtr = unsafe { il2cpp::read_field(sim_data, 0x10) };
            if !header.is_null() {
                let hk = il2cpp::class_of(header);
                il2cpp::dump_class_fields(hk, "RaceSimulateData.Header");
                il2cpp::dump_class_methods(hk, "RaceSimulateData.Header");
                scan(header, "Header", 0x120);
            }

            // _frameDataList (+0x18) — List<FrameData>. Гонка предрассчитана при
            // загрузке, поэтому покадровые данные (скорость, лейн, таймеры блока)
            // ВСЕЙ гонки уже здесь. Backing array List<T> лежит в _items (+0x10).
            let frame_list: RawPtr = unsafe { il2cpp::read_field(sim_data, 0x18) };
            if !frame_list.is_null() {
                let items: RawPtr = unsafe { il2cpp::read_field(frame_list, 0x10) };
                let n = unsafe { il2cpp::array_length(items) };
                logf!("_frameDataList: items {:p} len {}", items, n);
                let ec = il2cpp::array_element_class(items);
                if !ec.is_null() {
                    il2cpp::dump_class_fields(ec, "FrameData(elem)");
                    il2cpp::dump_class_methods(ec, "FrameData(elem)");
                }
                // Уровень глубже: FrameData.HorseDataArray (+0x18) — per-horse
                // данные кадра (скорость, лейн, дистанция, ТАЙМЕРЫ БЛОКА). Дереф
                // кадра[0] безопасен: RaceSimulateFrameData — ссылочный тип.
                if n > 0 {
                    let frame0 = unsafe { il2cpp::array_get_ref(items, 0) };
                    if !frame0.is_null() {
                        let horse_arr: RawPtr = unsafe { il2cpp::read_field(frame0, 0x18) };
                        let hn = unsafe { il2cpp::array_length(horse_arr) };
                        logf!("FrameData.HorseDataArray: len {}", hn);
                        let hec = il2cpp::array_element_class(horse_arr);
                        if !hec.is_null() {
                            il2cpp::dump_class_fields(hec, "HorseFrame(elem)");
                            il2cpp::dump_class_methods(hec, "HorseFrame(elem)");
                        }
                    }
                }
            }

            // _horseResultDataArray (+0x20) — T[] итогов по каждой лошади
            // (вероятное место суммарного времени в блоке / причины поражения).
            let res_arr: RawPtr = unsafe { il2cpp::read_field(sim_data, 0x20) };
            if !res_arr.is_null() {
                let n = unsafe { il2cpp::array_length(res_arr) };
                logf!("_horseResultDataArray: len {}", n);
                let ec = il2cpp::array_element_class(res_arr);
                if !ec.is_null() {
                    il2cpp::dump_class_fields(ec, "HorseResultData(elem)");
                    il2cpp::dump_class_methods(ec, "HorseResultData(elem)");
                }
            }

            // _simEvDataList (+0x28) — список ИГРОВЫХ событий гонки (List<T>): среди
            // них активации скиллов (кто/когда/какой скилл) — ГРАУНД-ТРУС тайминга
            // скиллов для окна-реплея. Backing array List<T> в _items (+0x10).
            // Дамп раскладки элемента (имена полей подскажут frame/horse/skill_id),
            // чтобы затем читать события в frames.rs и писать их в архив (поле `act`).
            let ev_list: RawPtr = unsafe { il2cpp::read_field(sim_data, 0x28) };
            if !ev_list.is_null() {
                let items: RawPtr = unsafe { il2cpp::read_field(ev_list, 0x10) };
                let n = unsafe { il2cpp::array_length(items) };
                logf!("_simEvDataList: items {:p} len {}", items, n);
                let ec = il2cpp::array_element_class(items);
                if !ec.is_null() {
                    il2cpp::dump_class_fields(ec, "SimEvData(elem)");
                    il2cpp::dump_class_methods(ec, "SimEvData(elem)");
                    // Раскладка подтверждена: frameTime(f32)@0x10, type(i32)@0x14,
                    // param(int[] ref)@0x18. Печатаем ВСЕ события с содержимым param,
                    // чтобы сопоставить skill_id (из "scout:" строк) и определить, какой
                    // type = активация скилла и где в param лежит skill_id/horse.
                    let is_vt = il2cpp::is_valuetype(ec);
                    let stride = il2cpp::array_element_size(items);
                    logf!("SimEvData: is_valuetype {} stride {}", is_vt, stride);
                    for k in 0..n.min(160) {
                        let base = unsafe { il2cpp::array_elem_base(items, k, stride, is_vt) };
                        if base.is_null() {
                            continue;
                        }
                        let ft: f32 = unsafe { il2cpp::read_field(base, 0x10) };
                        let ty: i32 = unsafe { il2cpp::read_field(base, 0x14) };
                        let param: RawPtr = unsafe { il2cpp::read_field(base, 0x18) };
                        let plen = unsafe { il2cpp::array_length(param) };
                        let mut ps = String::new();
                        for j in 0..plen.min(12) {
                            let v: i32 = unsafe { il2cpp::read_field(param, 0x20 + j * 4) };
                            if j > 0 {
                                ps.push(',');
                            }
                            ps.push_str(&v.to_string());
                        }
                        logf!("ev[{}] t={:.3} type={} param[{}]=[{}]", k, ft, ty, plen, ps);
                    }
                }
            }
        }
    }

    let mut race = lock_race();
    // Конструкторы приходят пачкой при загрузке гонки. Если с прошлой пачки
    // прошло больше 10 секунд — это новая гонка, сбрасываем старых лошадей.
    let now = Instant::now();
    if race
        .last_ctor
        .map_or(false, |t| now.duration_since(t) > Duration::from_secs(10))
    {
        race.horses.clear();
        // Новая гонка — сбрасываем флаг пост-разбора кадров и метаданные курса
        // (course_id перечитается ниже для новой гонки).
        STATS_DONE.store(false, Ordering::Relaxed);
        race.course_id = 0;
    }
    race.last_ctor = Some(now);
    // Reader один на всю гонку — запоминаем для разбора кадров на финише.
    if !reader.is_null() {
        RACE_READER.store(reader as usize, Ordering::Relaxed);
    }

    // Метаданные уровня гонки (курс/трек/тип) читаем один раз за гонку, когда
    // course_id ещё не заполнен. Источник — RaceManager.RaceInfo (см. read_race_meta).
    if race.course_id == 0 {
        if let Some(m) = read_race_meta() {
            race.course_id = m.course_id;
            race.race_track_id = m.race_track_id;
            race.course_distance = m.course_distance;
            race.course_ground = m.course_ground;
            race.race_type = m.race_type;
            race.race_instance_id = m.race_instance_id;
            logf!(
                "race meta: course_id {} track {} dist {} ground {} type {} instance {}",
                m.course_id, m.race_track_id, m.course_distance, m.course_ground,
                m.race_type, m.race_instance_id
            );
        }
    }
    logf!(
        "horse ctor: gate {} name '{}' trainer '{}' stats {}/{}/{}/{}/{} style {} apt d[{},{},{},{}] s[{}]",
        gate_no, chara_name, trainer_name,
        stat_speed, stat_stamina, stat_pow, stat_guts, stat_wiz,
        running_style, apt_short, apt_mile, apt_middle, apt_long, apt_style
    );
    let mut horse = HorseState::new(gate_no, chara_name, trainer_name);
    horse.stat_speed = stat_speed;
    horse.stat_stamina = stat_stamina;
    horse.stat_pow = stat_pow;
    horse.stat_guts = stat_guts;
    horse.stat_wiz = stat_wiz;
    horse.running_style = running_style;
    horse.apt_short = apt_short;
    horse.apt_mile = apt_mile;
    horse.apt_middle = apt_middle;
    horse.apt_long = apt_long;
    horse.apt_style = apt_style;
    horse.apt_turf = apt_turf;
    horse.apt_dirt = apt_dirt;
    horse.active_ground_apt = active_ground_apt;
    horse.active_dist_apt = active_dist_apt;
    horse.ground_type = ground_type;
    horse.is_user = is_user;
    horse.motivation = motivation;
    horse.popularity = popularity;
    horse.skills = skills;
    race.horses.insert(this as usize, horse);
}

/// Метаданные уровня гонки, прочитанные из `RaceManager.RaceInfo`.
struct RaceMeta {
    course_id: i32,
    race_track_id: i32,
    course_distance: i32,
    course_ground: i32,
    race_type: i32,
    race_instance_id: i32,
}

/// Закэшированные MethodInfo* геттеров (резолвятся один раз по именам).
#[allow(non_snake_case)]
struct MetaApi {
    get_Instance: RawPtr,
    get_RaceInfo: RawPtr,
    get_RaceType: RawPtr,
    get_RaceInstanceId: RawPtr,
    get_RaceCourseSet: RawPtr,
}
unsafe impl Send for MetaApi {}
unsafe impl Sync for MetaApi {}
static META_API: OnceLock<Option<MetaApi>> = OnceLock::new();

/// Разрешить (один раз) геттеры уровня гонки. None, если имена не совпали —
/// тогда авто-курс просто не работает (фолбэк — ручной выбор курса в app).
fn meta_api() -> Option<&'static MetaApi> {
    META_API
        .get_or_init(|| {
            let image = il2cpp::find_image("umamusume.dll")?;
            let rm = il2cpp::find_class(image, "Gallop", "RaceManager")?;
            let ri = il2cpp::find_class(image, "Gallop", "RaceInfo")?;
            Some(MetaApi {
                get_Instance: il2cpp::find_method(rm, "get_Instance", 0)?,
                get_RaceInfo: il2cpp::find_method(rm, "get_RaceInfo", 0)?,
                get_RaceType: il2cpp::find_method(ri, "get_RaceType", 0)?,
                get_RaceInstanceId: il2cpp::find_method(ri, "get_RaceInstanceId", 0)?,
                get_RaceCourseSet: il2cpp::find_method(ri, "get_RaceCourseSet", 0)?,
            })
        })
        .as_ref()
}

/// Прочитать метаданные текущей гонки: `RaceManager.Instance` → `RaceInfo` →
/// тип/инстанс + `RaceCourseSet` (Id = course_id, ключ course_data.json).
///
/// Всё через `il2cpp_runtime_invoke` (геттеры), БЕЗ сырых оффсетов на RaceInfo:
/// прямой вызов generic `get_Instance`/чтение чужих оффсетов крашили. Единственное
/// чтение по оффсету — поля самой мастер-строки `RaceCourseSet`
/// (Id@0x10/RaceTrackId@0x14/Distance@0x18/Ground@0x1c), на объекте, который
/// вернул штатный геттер (валиден — игра сама им пользуется).
fn read_race_meta() -> Option<RaceMeta> {
    let a = meta_api()?;
    let mgr = il2cpp::invoke0(a.get_Instance, std::ptr::null_mut());
    if mgr.is_null() {
        return None;
    }
    let ri = il2cpp::invoke0(a.get_RaceInfo, mgr);
    if ri.is_null() {
        return None;
    }
    let race_type = il2cpp::invoke_i32(a.get_RaceType, ri).unwrap_or(-1);
    let race_instance_id = il2cpp::invoke_i32(a.get_RaceInstanceId, ri).unwrap_or(-1);

    let cs = il2cpp::invoke0(a.get_RaceCourseSet, ri);
    let p = cs as usize;
    let (course_id, race_track_id, course_distance, course_ground) =
        if p >= 0x10000 && p < 0x7fff_ffff_0000 && p % 8 == 0 {
            unsafe {
                (
                    il2cpp::read_field::<i32>(cs, 0x10),
                    il2cpp::read_field::<i32>(cs, 0x14),
                    il2cpp::read_field::<i32>(cs, 0x18),
                    il2cpp::read_field::<i32>(cs, 0x1c),
                )
            }
        } else {
            (0, 0, 0, 0)
        };

    Some(RaceMeta {
        course_id,
        race_track_id,
        course_distance,
        course_ground,
        race_type,
        race_instance_id,
    })
}

unsafe extern "C" fn run_motion_speed_hook(this: RawPtr) -> f32 {
    let orig: FnThisF32 = std::mem::transmute(RUN_MOTION_ORIG.load(Ordering::Relaxed));
    let ret = orig(this);

    if let Some(api) = RACE_API.get() {
        let hp = (api.GetHp)(this);
        let max_hp = (api.GetMaxHp)(this);
        let hp_pct = (api.GetHpPer)(this);
        // Текущая скорость движения (m/s) — поле _lastSpeed, а НЕ get_Speed (это стат скорости).
        let speed: f32 = il2cpp::read_field(this, api.last_speed_offset);
        let is_last_spurt = (api.get_IsLastSpurt)(this);
        let finished = (api.get_IsOverRun)(this);
        let finish_order = if finished { (api.get_FinishOrder)(this) + 1 } else { -1 };
        let distance: f32 = il2cpp::read_field(this, api.distance_offset);

        let now = Instant::now();
        let mut race = lock_race();
        race.last_update = Some(now);
        let horse = race
            .horses
            .entry(this as usize)
            .or_insert_with(|| HorseState::new(-1, String::new(), String::new()));

        // Ускорение = Δскорости / Δt, со сглаживанием (экспоненциальное среднее),
        // чтобы убрать покадровый шум. dt берём от прошлого кадра этой лошади.
        // Ускорение считаем по ФИКСИРОВАННОМУ окну (~50мс) через якорь, а НЕ по
        // гэпу между вызовами хука: тот бывает <1мс (двойные вызовы/джиттер), и тогда
        // Δскорости/Δt улетал в десятки-сотни м/с² — единичный спайк навсегда застревал
        // в пике max_spurt_accel (видели 150). Окно делает Δt всегда осмысленным и не
        // зависит от частоты вызова хука; установившееся значение (напр. 0.4 от ульты)
        // сохраняется. Кламп — лишь страховка от телепортов скорости.
        let win = now.duration_since(horse.accel_anchor_time).as_secs_f32();
        if win >= 0.05 && win < 1.0 {
            let raw_accel = ((speed - horse.accel_anchor_speed) / win).clamp(-15.0, 15.0);
            horse.accel = horse.accel * 0.7 + raw_accel * 0.3;
            horse.accel_anchor_speed = speed;
            horse.accel_anchor_time = now;
        } else if win >= 1.0 {
            // Долго не обновлялись (новая лошадь/пауза) — переякориваемся без выброса.
            horse.accel_anchor_speed = speed;
            horse.accel_anchor_time = now;
        }
        // Пик ускорения во время last spurt (для финишного рывка).
        if is_last_spurt && horse.accel > horse.max_spurt_accel {
            horse.max_spurt_accel = horse.accel;
        }
        // Пик скорости во время last spurt.
        if is_last_spurt && speed > horse.max_spurt_speed {
            horse.max_spurt_speed = speed;
        }

        horse.hp = hp;
        horse.max_hp = max_hp;
        horse.hp_pct = hp_pct;
        horse.speed = speed;
        horse.distance = distance;
        horse.is_last_spurt = is_last_spurt;
        horse.finished = finished;
        horse.finish_order = finish_order;
        horse.last_update = now;

        // Пост-разбор кадров: когда ВСЕ лошади финишировали — один раз проходим
        // предрассчитанные кадры и считаем блок/закидывание/отрыв по каждой.
        let all_finished = race.horses.values().all(|h| h.finished);
        if all_finished && !STATS_DONE.swap(true, Ordering::Relaxed) {
            let reader = RACE_READER.load(Ordering::Relaxed) as RawPtr;
            let acc = crate::frames::compute(reader);
            if acc.is_empty() {
                logf!("frame stats: no data (reader {:p})", reader);
                STATS_DONE.store(false, Ordering::Relaxed); // попробуем на след. кадре
            } else {
                for h in race.horses.values_mut() {
                    let idx = (h.gate_no - 1) as usize;
                    if let Some(a) = acc.get(idx) {
                        h.blocked_time = a.blocked_time;
                        h.pre_spurt_blocked_time = a.pre_spurt_blocked_time;
                        h.blocked_episodes = a.episodes;
                        h.kakari_time = a.kakari_time;
                        h.finish_time = a.finish_time;
                        h.finish_diff_time = a.finish_diff_time;
                        h.blocked_lost_dist = a.lost_dist;
                        h.blocked_lost_time = a.lost_time;
                        h.spurt_blocked_time = a.spurt_blocked_time;
                        h.spurt_blocked_episodes = a.spurt_episodes;
                        h.spurt_lost_dist = a.spurt_lost_dist;
                        h.spurt_lost_time = a.spurt_lost_time;
                        h.spurt_unresolved = a.spurt_unresolved;
                        h.stats_ready = true;
                        // Валидация маппинга idx=gate-1 по финишному месту.
                        logf!(
                            "frame stats: gate {} idx {} blocked {:.2}s x{} preSpurtBlk {:.2}s spurtBlk {:.2}s x{} spurtLost {:.1}m/{:.2}s finishT {:.3} diff {:.3} simFO {} liveFO {}",
                            h.gate_no, idx, a.blocked_time, a.episodes, a.pre_spurt_blocked_time,
                            a.spurt_blocked_time, a.spurt_episodes, a.spurt_lost_dist, a.spurt_lost_time,
                            a.finish_time, a.finish_diff_time, a.finish_order, h.finish_order
                        );
                    }
                }
                // Полный архив гонки (meta + лошади + покадровый таймлайн) для
                // офлайн-реверса: %LOCALAPPDATA%\uma_race_overlay_races\.
                crate::archive::write_race(&race, reader);
            }
        }
    }

    ret
}

fn resolve_api() -> Option<(RaceApi, RawPtr, RawPtr)> {
    let image = il2cpp::find_image("umamusume.dll")?;
    let replay_klass = il2cpp::find_class(image, "Gallop", "HorseRaceInfoReplay")?;
    let info_klass = il2cpp::find_class(image, "Gallop", "HorseRaceInfo")?;
    let horse_data_klass = il2cpp::find_class(image, "Gallop", "HorseData")?;

    macro_rules! mptr {
        ($klass:expr, $name:literal, $argc:literal) => {{
            let m = il2cpp::find_method($klass, $name, $argc);
            if m.is_none() {
                il2cpp::dump_class_methods($klass, stringify!($klass));
            }
            unsafe { std::mem::transmute(il2cpp::method_pointer(m?)) }
        }};
    }

    let ctor_target = il2cpp::method_pointer(il2cpp::find_method(replay_klass, ".ctor", 2)?);
    let run_motion_target =
        il2cpp::method_pointer(il2cpp::find_method(replay_klass, "get_RunMotionSpeed", 0)?);

    // Опциональные геттеры статов: отсутствие любого из них НЕ валит оверлей.
    macro_rules! mopt {
        ($klass:expr, $name:literal) => {
            il2cpp::find_method($klass, $name, 0).map(|m| unsafe {
                std::mem::transmute::<RawPtr, FnThisI32>(il2cpp::method_pointer(m))
            })
        };
    }
    // Имена статов на HorseData: get_RawSpeed/RawStamina/RawPow/RawGuts/RawWiz
    // (get_Speed и т.п. в этом классе НЕТ — подтверждено дампом живой игры).
    let stats = StatApi {
        get_Speed: mopt!(horse_data_klass, "get_RawSpeed"),
        get_Stamina: mopt!(horse_data_klass, "get_RawStamina"),
        get_Pow: mopt!(horse_data_klass, "get_RawPow"),
        get_Guts: mopt!(horse_data_klass, "get_RawGuts"),
        get_Wiz: mopt!(horse_data_klass, "get_RawWiz"),
        get_RunningStyle: mopt!(horse_data_klass, "get_RunningStyle"),
        get_ProperDistanceShort: mopt!(horse_data_klass, "get_ProperDistanceShort"),
        get_ProperDistanceMile: mopt!(horse_data_klass, "get_ProperDistanceMile"),
        get_ProperDistanceMiddle: mopt!(horse_data_klass, "get_ProperDistanceMiddle"),
        get_ProperDistanceLong: mopt!(horse_data_klass, "get_ProperDistanceLong"),
        get_ProperRunningStyleNige: mopt!(horse_data_klass, "get_ProperRunningStyleNige"),
        get_ProperRunningStyleSenko: mopt!(horse_data_klass, "get_ProperRunningStyleSenko"),
        get_ProperRunningStyleSashi: mopt!(horse_data_klass, "get_ProperRunningStyleSashi"),
        get_ProperRunningStyleOikomi: mopt!(horse_data_klass, "get_ProperRunningStyleOikomi"),
        get_ProperGroundTurf: mopt!(horse_data_klass, "get_ProperGroundTurf"),
        get_ProperGroundDirt: mopt!(horse_data_klass, "get_ProperGroundDirt"),
        get_Motivation: mopt!(horse_data_klass, "get_Motivation"),
        get_Popularity: mopt!(horse_data_klass, "get_Popularity"),
        get_SkillDataArray: il2cpp::find_method(horse_data_klass, "get_SkillDataArray", 0)
            .map(|m| unsafe {
                std::mem::transmute::<RawPtr, FnThisPtr>(il2cpp::method_pointer(m))
            }),
        get_ActiveProperGroundType: mopt!(horse_data_klass, "get_ActiveProperGroundType"),
        get_ActiveProperDistance: mopt!(horse_data_klass, "get_ActiveProperDistance"),
        get_IsUser: il2cpp::find_method(horse_data_klass, "get_IsUser", 0)
            .map(|m| unsafe { std::mem::transmute::<RawPtr, FnThisBool>(il2cpp::method_pointer(m)) }),
    };
    if stats.get_Speed.is_none()
        || stats.get_Stamina.is_none()
        || stats.get_RunningStyle.is_none()
        || stats.get_ProperDistanceShort.is_none()
    {
        // Имена не совпали — выписываем все методы класса, чтобы поправить.
        il2cpp::dump_class_methods(horse_data_klass, "HorseData (stat getters missing)");
    }

    let api = RaceApi {
        GetHp: mptr!(info_klass, "GetHp", 0),
        GetMaxHp: mptr!(info_klass, "GetMaxHp", 0),
        GetHpPer: mptr!(info_klass, "GetHpPer", 0),
        get_IsLastSpurt: mptr!(replay_klass, "get_IsLastSpurt", 0),
        get_IsOverRun: mptr!(info_klass, "get_IsOverRun", 0),
        get_FinishOrder: mptr!(replay_klass, "get_FinishOrder", 0),
        get_GateNo: mptr!(horse_data_klass, "get_GateNo", 0),
        get_charaName: mptr!(horse_data_klass, "get_charaName", 0),
        InitTrainerName: mptr!(horse_data_klass, "InitTrainerName", 0),
        get_TrainerName: mptr!(horse_data_klass, "get_TrainerName", 0),
        distance_offset: il2cpp::field_offset(info_klass, "_distance")?,
        last_speed_offset: il2cpp::field_offset(info_klass, "_lastSpeed")?,
        stats,
    };
    Some((api, ctor_target, run_motion_target))
}

/// Фоновый поток: ждём инициализации il2cpp, разрешаем имена, ставим хуки.
pub fn init_thread() {
    logf!("init_thread: start, waiting for cri_ware_unity.dll (il2cpp fully initialized)");
    // Ждём загрузки CriWare — к этому моменту il2cpp полностью инициализирован
    // и трогать рантайм безопасно (тот же сигнал использует Hachimi).
    let mut ticks = 0u32;
    loop {
        if il2cpp::module_loaded(b"cri_ware_unity.dll\0") && il2cpp::domain_ready() {
            break;
        }
        ticks += 1;
        if ticks % 20 == 0 {
            logf!("init_thread: still waiting (tick {})", ticks);
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    // дополнительный запас, чтобы метаданные точно достроились
    std::thread::sleep(Duration::from_secs(3));
    logf!("init_thread: cri_ware loaded, attaching thread");
    il2cpp::attach_current_thread();

    if il2cpp::find_image("umamusume.dll").is_none() {
        logf!("init_thread: umamusume.dll image not found, aborting");
        return;
    }
    logf!("il2cpp ready, resolving race classes");

    let Some((api, ctor_target, run_motion_target)) = resolve_api() else {
        logf!("FAILED to resolve race api, overlay will be inactive");
        return;
    };
    let _ = RACE_API.set(api);

    unsafe {
        match minhook::MinHook::create_hook(ctor_target, ctor_hook as *mut c_void) {
            Ok(orig) => CTOR_ORIG.store(orig as usize, Ordering::Relaxed),
            Err(e) => {
                logf!("create_hook ctor failed: {:?}", e);
                return;
            }
        }
        match minhook::MinHook::create_hook(run_motion_target, run_motion_speed_hook as *mut c_void)
        {
            Ok(orig) => RUN_MOTION_ORIG.store(orig as usize, Ordering::Relaxed),
            Err(e) => {
                logf!("create_hook run_motion failed: {:?}", e);
                return;
            }
        }
        if let Err(e) = minhook::MinHook::enable_all_hooks() {
            logf!("enable_all_hooks failed: {:?}", e);
            return;
        }
    }
    logf!("race hooks installed");
}
