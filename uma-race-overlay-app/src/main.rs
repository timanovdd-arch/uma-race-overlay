//! Оверлей-приложение для Umamusume PvP.
//!
//! Запускается ОТДЕЛЬНО от игры (в своём процессе). Читает снимок состояния
//! гонки из JSON-файла, который пишет внутриигровой плагин uma_race_overlay.dll,
//! и рисует прозрачное click-through окно поверх игры с HP (стаминой) и скоростью
//! каждой лошади, включая лошадей других игроков в PvP.
//!
//! Управление: F8 — скрыть/показать таблицу, F6 — показать/спрятать чужих лошадей.

#![windows_subsystem = "windows"] // без консольного окна

mod course;
mod feedback;
mod gamedata;
mod replay;
mod sim;
mod theme;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// Версия для подписи фидбэка (релиз идёт как v0.2.0; Cargo-версия отстаёт).
const APP_VERSION: &str = "0.2.0";

/// Какое окно фидбэка открыто (одно за раз).
#[derive(PartialEq, Clone, Copy)]
enum FeedbackView {
    Closed,
    /// Меню-развилка: баг или идея.
    Menu,
    /// Окно бага: галочки «где произошло» + текст + приложить гонку.
    Bug,
    /// Окно идеи: только текст.
    Idea,
}

/// Области приложения для галочек «где произошёл баг».
const FB_AREAS: [&str; 9] = [
    "Overlay table / display",
    "Block analysis",
    "Spurt data (accel/speed)",
    "Win rate (%)",
    "Mine / rivals filter",
    "Mouse / clicking / window",
    "Hotkeys",
    "Startup / crash / freeze",
    "Other",
];

use egui::{Color32, RichText};
use egui_overlay::egui_render_three_d::ThreeDBackend;
use egui_overlay::egui_window_glfw_passthrough::GlfwBackend;
use egui_overlay::EguiOverlay;
use serde::{Deserialize, Serialize};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, VK_F10, VK_F11, VK_F6, VK_F7, VK_F8, VK_F9,
};

#[derive(Deserialize, Clone, Default)]
struct Horse {
    gate: i32,
    name: String,
    trainer: String,
    hp: f32,
    max_hp: f32,
    speed: f32,
    #[serde(default)]
    accel: f32,
    #[serde(default)]
    max_spurt_accel: f32,
    /// Максимальная скорость (m/s), достигнутая во время спурта.
    #[serde(default)]
    max_spurt_speed: f32,
    distance: f32,
    spurt: bool,
    finished: bool,
    order: i32,
    /// Стиль бега: 1 nige, 2 senko, 3 sashi, 4 oikomi (-1/0 = неизвестен).
    #[serde(default)]
    style: i32,
    /// Статы из HorseData: [скорость, выносливость, сила, упорство, ум], -1 = нет данных.
    #[serde(default)]
    stats: Vec<i32>,
    /// Аптитуды (1=G..8=S): [short, mile, middle, long, свой стиль бега].
    #[serde(default)]
    apt: Vec<i32>,
    /// Аптитуды поверхности (1=G..8=S): [турф, грунт].
    #[serde(default)]
    ground: Vec<i32>,
    /// Аптитуд (1=G..8=S) для фактической дистанции гонки. -1 = нет.
    #[serde(default = "neg1")]
    adist: i32,
    /// Аптитуд (1=G..8=S) для фактической поверхности гонки. -1 = нет.
    #[serde(default = "neg1")]
    aground: i32,
    /// Тип трассы: 1 турф, 2 грунт, 0 неизвестно.
    #[serde(default)]
    gtype: i32,
    /// «Моя лошадь» от игры (get_IsUser) — стабильный признак своих.
    #[serde(default)]
    is_user: bool,
    /// Мотивация 1..5 (3 = норма), -1 = неизвестна.
    #[serde(default = "neg1")]
    motiv: i32,
    /// Популярность от игры (1 = фаворит), -1 = неизвестна.
    #[serde(default = "neg1")]
    pop: i32,
    /// Скиллы: [[skill_id, level], ...].
    #[serde(default)]
    skills: Vec<(i32, i32)>,
    // --- пост-разбор гонки (заполняется плагином на финише) ---
    /// Статистика блока/закидывания посчитана (поля ниже валидны).
    #[serde(default)]
    stats_ready: bool,
    /// Суммарное время во фронт-блоке (сек).
    #[serde(default)]
    blocked_time: f32,
    /// Суммарное время во фронт-блоке ДО спурта (сек), «mid leg block».
    #[serde(default)]
    pre_spurt_blocked_time: f32,
    /// Число эпизодов блока.
    #[serde(default)]
    blocked_episodes: i32,
    /// Время в закидывании (掛かり), сек.
    #[serde(default)]
    kakari_time: f32,
    /// Абсолютное финишное время (сек) — для контрфактуального ранжирования.
    #[serde(default)]
    finish_time: f32,
    /// `FinishDiffTime` = отрыв до лошади на одно место впереди (margin), НЕ до победителя.
    #[serde(default)]
    finish_diff_time: f32,
    /// Оценка потерянной из-за блока дистанции (м).
    #[serde(default)]
    blocked_lost_dist: f32,
    /// Оценка потерянного из-за блока времени (сек).
    #[serde(default)]
    blocked_lost_time: f32,
    // --- блок во время спурта (главная метрика влияния) ---
    #[serde(default)]
    spurt_blocked_time: f32,
    #[serde(default)]
    spurt_blocked_episodes: i32,
    #[serde(default)]
    spurt_lost_dist: f32,
    #[serde(default)]
    spurt_lost_time: f32,
    /// Зажат в спурте до финиша → оценка потери занижена (неопределённость).
    #[serde(default)]
    spurt_unresolved: bool,
    /// Позиция в забеге среди ВСЕХ лошадей (вычисляется здесь, в JSON её нет).
    #[serde(skip)]
    rank: i32,
    /// Отрыв (сек) до лошади на одно место впереди (вычисляется здесь; NaN = нет).
    #[serde(skip)]
    gap_ahead: f32,
    /// Контрфактуальное место «без спурт-блока у всех» (вычисляется здесь; 0 = нет).
    #[serde(skip)]
    cf_place: i32,
    /// Оценка шанса победы 0..1 (вычисляется здесь; < 0 = нет данных).
    #[serde(skip)]
    win_chance: f32,
    /// «Block kill»: на сколько мест трафик/блок изменил среднее место ИМЕННО этой
    /// лошади по симулятору (avg_place с блок-моделью − без неё). >0 = блок стоил мест,
    /// <0 = выиграла от зажатых соперников. NaN = ещё не посчитано симулятором.
    #[serde(skip, default = "nan_f32")]
    block_kill: f32,
}

/// Дефолт для не-сериализуемых f32-полей, у которых 0.0 — валидное значение
/// (нужен признак «не посчитано»).
fn nan_f32() -> f32 {
    f32::NAN
}

impl Horse {
    /// Стат по индексу (0 скорость, 1 выносливость, 2 сила, 3 упорство, 4 ум).
    fn stat(&self, i: usize) -> Option<f32> {
        match self.stats.get(i) {
            Some(&v) if v > 0 => Some(v as f32),
            _ => None,
        }
    }

    /// Аптитуд по индексу (0..3 дистанции, 4 свой стиль); None если не передан.
    fn aptitude(&self, i: usize) -> Option<i32> {
        match self.apt.get(i) {
            Some(&v) if (1..=8).contains(&v) => Some(v),
            _ => None,
        }
    }
}

const fn max_u64() -> u64 {
    u64::MAX
}

const fn neg1() -> i32 {
    -1
}

#[derive(Deserialize, Default)]
struct RaceSnapshot {
    ts: u64,
    running: bool,
    /// Мс с момента последней пачки конструкторов лошадей. Старый плагин поля
    /// не пишет → MAX → режим «до старта» просто не активируется.
    #[serde(default = "max_u64")]
    ctor_age_ms: u64,
    /// Авто-детект курса из игры (RaceManager.RaceInfo → RaceCourseSet.Id).
    /// 0 = неизвестно (старый плагин/не прочиталось) → фолбэк на ручной course_id.
    #[serde(default)]
    course_id: i32,
    /// Тип гонки (`RaceInfo.RaceType`): обычная = 9, иное → метка события
    /// (Champions Meeting и т.п.). 0 = неизвестно.
    #[serde(default)]
    race_type: i32,
    /// `RaceInfo.RaceInstanceId`. 0 = неизвестно.
    #[serde(default)]
    race_instance_id: i32,
    #[serde(default)]
    horses: Vec<Horse>,
}

fn state_path() -> PathBuf {
    let base = std::env::var("TEMP").unwrap_or_else(|_| ".".into());
    PathBuf::from(base).join("uma_race_overlay_state.json")
}

fn cfg_path() -> PathBuf {
    let base = std::env::var("TEMP").unwrap_or_else(|_| ".".into());
    PathBuf::from(base).join("uma_race_overlay_cfg.json")
}

/// Настройки, переживающие перезапуск (пишутся рядом с файлом состояния).
#[derive(Serialize, Deserialize)]
struct Cfg {
    /// Имя «моего» тренера: лошади с этим тренером считаются своими.
    /// У своих лошадей тренер ЗАПОЛНЕН (имя игрока), у NPC — пустой.
    /// "" = ещё не определён: подхватывается автоматически из гонки,
    /// где ровно один непустой тренер, либо кликом в нижней панели.
    my_trainer: String,
    /// Сортировка «свои сверху»: сначала мои лошади, потом соперники
    /// (внутри групп — по позиции в гонке).
    #[serde(default)]
    mine_first: bool,
    /// Показывать колонку с оценкой шанса победы (F10). По умолчанию СКРЫТА
    /// (функция в разработке — показываем дисклеймер при первом включении).
    #[serde(default)]
    show_chance: bool,
    /// Позиция окна таблицы (egui), чтобы переживала перезапуск. NaN = не задана.
    #[serde(default = "nan")]
    win_x: f32,
    #[serde(default = "nan")]
    win_y: f32,
    /// Анонимный идентификатор установки для rate-limit фидбэка (без PII).
    #[serde(default)]
    install_id: String,
    /// Выбранный курс трассы (course_id из course_data). None = синтетическая
    /// геометрия (авто-детект из игры пока не сделан — задаётся вручную).
    #[serde(default)]
    course_id: Option<i32>,
}

fn default_true() -> bool {
    true
}

fn nan() -> f32 {
    f32::NAN
}

impl Default for Cfg {
    fn default() -> Self {
        Self {
            my_trainer: String::new(),
            mine_first: false,
            show_chance: false,
            win_x: f32::NAN,
            win_y: f32::NAN,
            install_id: String::new(),
            course_id: None,
        }
    }
}

fn load_cfg() -> Cfg {
    std::fs::read_to_string(cfg_path())
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}

/// Путь к ассету (donat.png и т.п.): рядом с exe, иначе в текущей папке.
fn asset_path(name: &str) -> Option<PathBuf> {
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let p = dir.join(name);
            if p.exists() {
                return Some(p);
            }
        }
    }
    let cwd = PathBuf::from(name);
    if cwd.exists() {
        Some(cwd)
    } else {
        None
    }
}

fn load_asset_string(name: &str) -> Option<String> {
    std::fs::read_to_string(asset_path(name)?).ok()
}

/// Загрузить QR в egui-текстуру. Сначала внешний donat.png рядом с exe (можно
/// переопределить), иначе — встроенный в exe (надёжно, файл носить не нужно).
fn load_qr_texture(ctx: &egui::Context) -> Option<egui::TextureHandle> {
    const EMBEDDED_QR: &[u8] = include_bytes!("../donat.png");
    let bytes = match asset_path("donat.png") {
        Some(p) => std::fs::read(p).unwrap_or_else(|_| EMBEDDED_QR.to_vec()),
        None => EMBEDDED_QR.to_vec(),
    };
    let img = image::load_from_memory(&bytes).ok()?.to_rgba8();
    let (w, h) = img.dimensions();
    let color = egui::ColorImage::from_rgba_unmultiplied([w as usize, h as usize], img.as_raw());
    Some(ctx.load_texture("donat_qr", color, egui::TextureOptions::LINEAR))
}

/// Ставит иконку окна (= иконка на панели задач) из встроенного icon.png.
fn set_window_icon(glfw_backend: &mut GlfwBackend) {
    use egui_overlay::egui_window_glfw_passthrough::glfw::PixelImage;
    const ICON_PNG: &[u8] = include_bytes!("../icon.png");
    let Ok(img) = image::load_from_memory(ICON_PNG) else { return };
    let rgba = img.to_rgba8();
    let (w, h) = rgba.dimensions();
    // GLFW ждёт пиксели как little-endian RGBA (u32 = R | G<<8 | B<<16 | A<<24).
    let pixels: Vec<u32> = rgba
        .pixels()
        .map(|p| {
            let [r, g, b, a] = p.0;
            (r as u32) | ((g as u32) << 8) | ((b as u32) << 16) | ((a as u32) << 24)
        })
        .collect();
    glfw_backend.window.set_icon_from_pixels(vec![PixelImage {
        width: w,
        height: h,
        pixels,
    }]);
}

/// Добавляет системные шрифты Windows с поддержкой CJK/кириллицы как фолбэк,
/// чтобы ники других игроков (японский/корейский/китайский) не рисовались
/// квадратами-«тофу». Встроенный шрифт egui покрывает только латиницу + базовую
/// кириллицу, поэтому подмешиваем шрифты из C:\Windows\Fonts (они штатно есть в
/// Windows 10/11). Файлы НЕ бандлим в exe — читаем системные, чтобы не раздувать
/// бинарник на ~30 МБ. egui сам идёт по списку семейства и подбирает глиф из
/// первого шрифта, где он есть. Вызывать один раз при старте.
fn install_system_fonts(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();
    // Кандидаты с широким покрытием. .ttc — коллекция, грузится индекс 0.
    // Порядок = приоритет фолбэка: японский (игра японская) → корейский → китайский.
    let candidates: [(&str, &str); 6] = [
        ("sys_yu_gothic", "C:/Windows/Fonts/YuGothR.ttc"),
        ("sys_meiryo", "C:/Windows/Fonts/meiryo.ttc"),
        ("sys_ms_gothic", "C:/Windows/Fonts/msgothic.ttc"),
        ("sys_malgun", "C:/Windows/Fonts/malgun.ttf"),
        ("sys_yahei", "C:/Windows/Fonts/msyh.ttc"),
        ("sys_simsun", "C:/Windows/Fonts/simsun.ttc"),
    ];
    let mut added: Vec<String> = Vec::new();
    for (name, path) in candidates {
        // Не грузим избыточно: одного японского + одного корейского/китайского
        // достаточно для покрытия. Берём первые удачные из каждой «группы».
        if let Ok(bytes) = std::fs::read(path) {
            fonts
                .font_data
                .insert(name.to_string(), egui::FontData::from_owned(bytes));
            added.push(name.to_string());
        }
    }
    if added.is_empty() {
        return; // системных шрифтов нет — оставляем дефолт egui
    }
    // Дописываем как фолбэк В КОНЕЦ обоих семейств (после латинского дефолта).
    for family in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
        let list = fonts.families.entry(family).or_default();
        for name in &added {
            if !list.contains(name) {
                list.push(name.clone());
            }
        }
    }
    ctx.set_fonts(fonts);
}

/// Открыть ссылку в браузере (Windows `start`).
fn open_url(url: &str) {
    let _ = std::process::Command::new("cmd")
        .args(["/C", "start", "", url])
        .spawn();
}

/// Позиция в egui-координатах, центрирующая окно размером w×h на ОСНОВНОМ
/// мониторе. Окно glfw начинается в (vx,vy) виртуального стола, поэтому
/// основной монитор (экранные 0,0) в egui-координатах = (-vx,-vy).
fn primary_center_egui(w: f32, h: f32) -> egui::Pos2 {
    use windows::Win32::UI::WindowsAndMessaging::{GetSystemMetrics, SM_CXSCREEN, SM_CYSCREEN};
    let (vx, vy, _, _) = virtual_screen();
    let (pw, ph) = unsafe {
        (
            GetSystemMetrics(SM_CXSCREEN) as f32,
            GetSystemMetrics(SM_CYSCREEN) as f32,
        )
    };
    egui::pos2(-vx as f32 + (pw - w) * 0.5, -vy as f32 + (ph - h) * 0.5)
}

/// Прямоугольник всего виртуального рабочего стола (все мониторы): (x,y,w,h).
fn virtual_screen() -> (i32, i32, i32, i32) {
    use windows::Win32::UI::WindowsAndMessaging::{
        GetSystemMetrics, SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN,
        SM_YVIRTUALSCREEN,
    };
    unsafe {
        let x = GetSystemMetrics(SM_XVIRTUALSCREEN);
        let y = GetSystemMetrics(SM_YVIRTUALSCREEN);
        let w = GetSystemMetrics(SM_CXVIRTUALSCREEN);
        let h = GetSystemMetrics(SM_CYVIRTUALSCREEN);
        if w <= 0 || h <= 0 {
            (0, 0, 1920, 1080)
        } else {
            (x, y, w, h)
        }
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Случайный анонимный идентификатор установки (32 hex-символа = 128 бит).
/// Не содержит личных данных — нужен только серверу для rate-limit/чёрного списка.
fn new_install_id() -> String {
    let mut s = String::with_capacity(32);
    for _ in 0..16 {
        s.push_str(&format!("{:02x}", fastrand::u8(..)));
    }
    s
}

struct App {
    path: PathBuf,
    placed: bool,
    visible: bool,
    sort_by_rank: bool,
    /// Колонка «base accel live» (текущее ускорение). Тоггл F9 / чекбокс.
    show_accel: bool,
    /// Колонки «spurt phase data» (макс. ускорение и макс. скорость в спурте).
    show_spurt: bool,
    /// Колонки «block analysis» (время в блоке/закидывании, влияние на место).
    show_block: bool,
    /// Показывать чужих лошадей (по дефолту видны только свои).
    show_enemies: bool,
    /// Показывать колонку «шанс победы» (F10). Стартует СКРЫТЫМ каждый запуск.
    show_chance: bool,
    /// Дисклеймер «функция в разработке» уже показан в этом запуске.
    chance_disclaimer_seen: bool,
    /// Окно дисклеймера сейчас открыто.
    chance_disclaimer_open: bool,
    my_trainer: String,
    mine_first: bool,
    interactive: bool,
    f8_was_down: bool,
    f9_was_down: bool,
    f7_was_down: bool,
    f6_was_down: bool,
    f10_was_down: bool,
    f11_was_down: bool,
    snapshot: RaceSnapshot,
    last_read: Instant,
    /// Данные master.mdb (скиллы, коэффициенты). None — БД не нашлась,
    /// винрейт падает обратно на простую эвристику по статам.
    gamedata: Option<std::sync::Arc<gamedata::GameData>>,
    /// Таблица геометрии трасс (course_data.json рядом с exe). None — файла нет,
    /// геометрия не используется (синтетика).
    courses: Option<std::sync::Arc<HashMap<i32, course::CourseGeom>>>,
    /// Выбранный курс (course_id) — пока вручную через cfg/UI (авто-детект из игры
    /// не сделан). None = синтетическая геометрия.
    course_id: Option<i32>,
    /// Текстовый буфер ввода course_id в UI.
    course_text: String,
    /// Готовый результат симуляции: (сигнатура гонки, винрейт по gate).
    sim_slot: std::sync::Arc<std::sync::Mutex<Option<(u64, HashMap<i32, (f32, f32)>)>>>,
    /// Симуляция уже крутится в фоне.
    sim_busy: std::sync::Arc<std::sync::atomic::AtomicBool>,
    /// Сохранённая позиция окна таблицы (NaN = по умолчанию).
    win_pos: Option<egui::Pos2>,
    /// Троттлинг записи cfg при перетаскивании окна.
    last_pos_save: Instant,
    /// «Замороженный» снимок последней гонки (лошади с рангами/win% + был ли
    /// это до старта). Показывается после финиша, пока не началась новая гонка,
    /// чтобы показатели не исчезали.
    frozen: Option<(Vec<Horse>, bool)>,
    /// Донат-окно при запуске (показывается один раз, до начала работы).
    donation_open: bool,
    /// Текстура QR-кода (грузится лениво из donat.png рядом с exe).
    qr_tex: Option<egui::TextureHandle>,
    /// Попытка загрузить QR уже была (не дёргать декодер каждый кадр).
    qr_tried: bool,
    /// Ссылка для доната (из «донат ссылка.txt» рядом с exe).
    donate_url: String,
    /// Конфиг релея фидбэка (URL Worker + app-key). Токена бота в клиенте НЕТ.
    relay_cfg: feedback::RelayConfig,
    /// Анонимный идентификатор установки (для server-side rate-limit; без PII).
    install_id: String,
    /// Какое окно фидбэка сейчас открыто.
    feedback_view: FeedbackView,
    /// Галочки «где произошёл баг» (по FB_AREAS).
    fb_areas: [bool; FB_AREAS.len()],
    /// Текст репорта/идеи.
    fb_text: String,
    /// Контакт для ответа (опционально).
    fb_contact: String,
    /// Приложить ли к багу данные последней гонки (JSON-файл состояния).
    fb_attach: bool,
    /// Статус последней отправки (общий с фоновым потоком).
    fb_status: feedback::Status,
    /// Окно-реплей (трек + скиллы + ползунок) открыто.
    replay_open: bool,
    /// Загруженное состояние реплея (последний архив гонки). None — ещё не грузили
    /// или архивов нет.
    replay: Option<replay::ReplayState>,
}

impl App {
    fn new() -> Self {
        let mut cfg = load_cfg();
        // Анонимный install_id для server-side rate-limit. Генерим один раз и
        // сохраняем; никаких личных данных — просто случайные 128 бит в hex.
        if cfg.install_id.is_empty() {
            cfg.install_id = new_install_id();
            if let Ok(text) = serde_json::to_string_pretty(&cfg) {
                let _ = std::fs::write(cfg_path(), text);
            }
        }
        let install_id = cfg.install_id.clone();
        Self {
            path: state_path(),
            placed: false,
            visible: true,
            sort_by_rank: true,
            show_accel: false,
            show_spurt: false,
            show_block: false,
            show_enemies: false,
            show_chance: false, // всегда скрыт на старте (функция в разработке)
            chance_disclaimer_seen: false,
            chance_disclaimer_open: false,
            my_trainer: cfg.my_trainer,
            mine_first: cfg.mine_first,
            interactive: false,
            f8_was_down: false,
            f9_was_down: false,
            f7_was_down: false,
            f6_was_down: false,
            f10_was_down: false,
            f11_was_down: false,
            snapshot: RaceSnapshot::default(),
            last_read: Instant::now() - Duration::from_secs(10),
            gamedata: gamedata::GameData::load().map(std::sync::Arc::new),
            courses: asset_path("course_data.json")
                .as_deref()
                .and_then(course::load_courses)
                .map(std::sync::Arc::new),
            course_id: cfg.course_id,
            course_text: cfg.course_id.map(|i| i.to_string()).unwrap_or_default(),
            sim_slot: std::sync::Arc::new(std::sync::Mutex::new(None)),
            sim_busy: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
            win_pos: if cfg.win_x.is_finite() && cfg.win_y.is_finite() {
                Some(egui::pos2(cfg.win_x, cfg.win_y))
            } else {
                None
            },
            last_pos_save: Instant::now(),
            frozen: None,
            donation_open: true,
            qr_tex: None,
            qr_tried: false,
            donate_url: load_asset_string("donate_link.txt")
                .or_else(|| load_asset_string("донат ссылка.txt"))
                .map(|s| s.trim().to_string())
                .filter(|s| s.starts_with("http"))
                .unwrap_or_else(|| "https://dalink.to/everlastingosu".to_string()),
            relay_cfg: feedback::load_config(load_asset_string),
            install_id,
            feedback_view: FeedbackView::Closed,
            fb_areas: [false; FB_AREAS.len()],
            fb_text: String::new(),
            fb_contact: String::new(),
            fb_attach: false,
            fb_status: Arc::new(Mutex::new(feedback::SendStatus::Idle)),
            replay_open: false,
            replay: None,
        }
    }

    fn save_cfg(&self) {
        let pos = self.win_pos.unwrap_or(egui::pos2(f32::NAN, f32::NAN));
        let cfg = Cfg {
            my_trainer: self.my_trainer.clone(),
            mine_first: self.mine_first,
            show_chance: self.show_chance,
            win_x: pos.x,
            win_y: pos.y,
            install_id: self.install_id.clone(),
            course_id: self.course_id,
        };
        if let Ok(text) = serde_json::to_string_pretty(&cfg) {
            let _ = std::fs::write(cfg_path(), text);
        }
    }

    /// Лошадь «моя». Приоритет — СТАБИЛЬНЫЙ флаг is_user от игры (не зависит от
    /// ника, работает у любого юзера без настройки). Если плагин его не отдаёт
    /// (старая версия / нет ни одного is_user) — фолбэк по имени тренера.
    fn is_mine(&self, h: &Horse) -> bool {
        if self.snapshot.horses.iter().any(|x| x.is_user) {
            return h.is_user;
        }
        !self.my_trainer.is_empty() && h.trainer == self.my_trainer
    }

    /// Возвращает true, если клавиша только что нажата (фронт), и обновляет флаг.
    fn key_pressed(vk: i32, was_down: &mut bool) -> bool {
        let down = unsafe { GetAsyncKeyState(vk) as u16 & 0x8000 != 0 };
        let pressed = down && !*was_down;
        *was_down = down;
        pressed
    }

    fn poll_hotkeys(&mut self) {
        if Self::key_pressed(VK_F8.0 as i32, &mut self.f8_was_down) {
            self.visible = !self.visible;
        }
        if Self::key_pressed(VK_F9.0 as i32, &mut self.f9_was_down) {
            self.show_accel = !self.show_accel;
        }
        if Self::key_pressed(VK_F7.0 as i32, &mut self.f7_was_down) {
            self.interactive = !self.interactive;
        }
        if Self::key_pressed(VK_F6.0 as i32, &mut self.f6_was_down) {
            self.show_enemies = !self.show_enemies;
        }
        if Self::key_pressed(VK_F10.0 as i32, &mut self.f10_was_down) {
            self.show_chance = !self.show_chance;
            self.on_chance_enabled();
        }
        if Self::key_pressed(VK_F11.0 as i32, &mut self.f11_was_down) {
            self.toggle_replay();
        }
    }

    /// Открыть/закрыть окно-реплей. При открытии (пере)загружает самый свежий
    /// архив гонки и считает привязки скиллов.
    fn toggle_replay(&mut self) {
        self.replay_open = !self.replay_open;
        if self.replay_open {
            self.load_replay();
        }
    }

    /// Загрузить последний архив гонки в окно-реплей (если есть).
    fn load_replay(&mut self) {
        if let Some(race) = replay::load_latest() {
            let gd = self.gamedata.as_deref();
            self.replay = Some(replay::ReplayState::new(race, gd));
        }
    }

    /// При включении винрейта — один раз за запуск показать дисклеймер «в разработке».
    fn on_chance_enabled(&mut self) {
        if self.show_chance && !self.chance_disclaimer_seen {
            self.chance_disclaimer_seen = true;
            self.chance_disclaimer_open = true;
        }
    }

    fn refresh(&mut self) {
        // читаем файл не чаще ~20 раз/с
        if self.last_read.elapsed() < Duration::from_millis(50) {
            return;
        }
        self.last_read = Instant::now();
        if let Ok(text) = std::fs::read_to_string(&self.path) {
            if let Ok(snap) = serde_json::from_str::<RaceSnapshot>(&text) {
                self.snapshot = snap;
            }
        }
    }

    /// Данные актуальны, если файл свежий и плагин помечает гонку как идущую.
    fn race_active(&self) -> bool {
        self.snapshot.running
            && !self.snapshot.horses.is_empty()
            && now_ms().saturating_sub(self.snapshot.ts) < 3000
    }

    /// Гонка загружена, но ещё не идёт (экраны до старта): лошади уже созданы
    /// конструкторами (< 90 с назад), а кадровый хук пока не тикает. В этом
    /// режиме показываем состав и шансы победы до выстрела стартера.
    fn pre_race(&self) -> bool {
        !self.snapshot.running
            && !self.snapshot.horses.is_empty()
            && now_ms().saturating_sub(self.snapshot.ts) < 3000
            && self.snapshot.ctor_age_ms < 90_000
            // Только ДО старта: никто ещё не побежал и не финишировал. После
            // финиша это уже не «pre-race», а итог (показывается «заморозкой»).
            && self
                .snapshot
                .horses
                .iter()
                .all(|h| !h.finished && h.distance < 5.0)
    }

    /// Заменяет эвристический win_chance результатом Monte Carlo симуляции
    /// (~200 виртуальных забегов со скиллами). Считается один раз на состав
    /// в фоновом потоке; пока не готово — остаётся эвристика.
    fn apply_sim(&mut self, horses: &mut [Horse]) {
        use std::hash::{Hash, Hasher};
        let Some(gd) = &self.gamedata else { return };
        if horses.iter().filter(|h| h.stat(0).is_some()).count() < 2 {
            return;
        }

        // Курс: приоритет — авто-детект из игры (snapshot.course_id от плагина),
        // фолбэк — ручной выбор (cfg/UI). >0 = валидный course_id.
        let effective_course_id: Option<i32> = if self.snapshot.course_id > 0 {
            Some(self.snapshot.course_id)
        } else {
            self.course_id
        };
        // course_id → реальная геометрия + точная дистанция.
        // None = синтетика и дистанция из maxHP.
        let geom: Option<course::CourseGeom> = effective_course_id
            .and_then(|id| Some(self.courses.as_ref()?.get(&id)?.clone()));
        let distance = geom
            .as_ref()
            .map(|g| g.distance)
            .unwrap_or_else(|| deduce_course_distance(horses).unwrap_or(2000.0) as f64);
        // Тип трассы — большинством голосов по лошадям (gtype от плагина).
        let ground: i32 = {
            let dirt = horses.iter().filter(|h| h.gtype == 2).count();
            let turf = horses.iter().filter(|h| h.gtype == 1).count();
            if dirt > turf {
                2
            } else {
                1
            }
        };

        // Сигнатура состава: пока она не меняется, результат закэширован.
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        (distance as i64).hash(&mut hasher);
        ground.hash(&mut hasher);
        effective_course_id.hash(&mut hasher);
        for h in horses.iter() {
            h.gate.hash(&mut hasher);
            h.style.hash(&mut hasher);
            h.stats.hash(&mut hasher);
            h.apt.hash(&mut hasher);
            h.ground.hash(&mut hasher);
            h.adist.hash(&mut hasher);
            h.aground.hash(&mut hasher);
            h.motiv.hash(&mut hasher);
            h.skills.hash(&mut hasher);
        }
        let sig = hasher.finish();

        if let Ok(slot) = self.sim_slot.lock() {
            if let Some((s, map)) = slot.as_ref() {
                if *s == sig {
                    for h in horses.iter_mut() {
                        if let Some((w, bk)) = map.get(&h.gate) {
                            h.win_chance = *w;
                            h.block_kill = *bk;
                        }
                    }
                    return;
                }
            }
        }

        if self.sim_busy.swap(true, std::sync::atomic::Ordering::SeqCst) {
            return; // прогон уже идёт
        }
        let cat = match distance as i32 {
            ..=1400 => 0,
            1401..=1800 => 1,
            1801..=2400 => 2,
            _ => 3,
        };
        let sim_horses: Vec<sim::SimHorse> = horses
            .iter()
            .filter(|h| h.stat(0).is_some())
            .map(|h| sim::SimHorse {
                gate: h.gate,
                style: if (1..=4).contains(&h.style) { h.style } else { 2 },
                speed: h.stat(0).unwrap_or(800.0) as f64,
                stamina: h.stat(1).unwrap_or(600.0) as f64,
                pow: h.stat(2).unwrap_or(600.0) as f64,
                guts: h.stat(3).unwrap_or(400.0) as f64,
                wiz: h.stat(4).unwrap_or(500.0) as f64,
                motivation: if (1..=5).contains(&h.motiv) { h.motiv } else { 3 },
                // Предпочитаем точный аптитуд от игры (active*), иначе по категории.
                apt_dist: if (1..=8).contains(&h.adist) {
                    h.adist
                } else {
                    h.aptitude(cat).unwrap_or(7)
                },
                apt_style: h.aptitude(4).unwrap_or(7),
                apt_ground: if (1..=8).contains(&h.aground) {
                    h.aground
                } else {
                    match (ground, h.ground.first(), h.ground.get(1)) {
                        (2, _, Some(&d)) if (1..=8).contains(&d) => d, // грунт
                        (_, Some(&t), _) if (1..=8).contains(&t) => t, // турф
                        _ => 7,
                    }
                },
                skills: h.skills.clone(),
            })
            .collect();
        let gd = gd.clone();
        let slot = self.sim_slot.clone();
        let busy = self.sim_busy.clone();
        std::thread::spawn(move || {
            // Состояние трассы (firm/good/soft/heavy) пока не извлекаем из игры —
            // считаем firm(良). Когда добавим — менять здесь.
            // course = выбранная вручную геометрия (cfg.course_id), иначе синтетика.
            let race = sim::RaceParams { distance, ground, condition: 1, course: geom };
            // 3000 прогонов: шум выборки ~±0.8% (ниже ошибки модели). simulate
            // сам распараллеливает по ядрам, оставляя одно ядро игре.
            let res = sim::simulate(&gd, &race, &sim_horses, 3000, sig);
            // «Block kill»: тот же состав/сид, но модель трафика выключена → разница
            // среднего места = на сколько мест блок сдвинул КАЖДУЮ лошадь.
            let res_nb = sim::simulate_no_block(&gd, &race, &sim_horses, 3000, sig);
            let map: HashMap<i32, (f32, f32)> = sim_horses
                .iter()
                .enumerate()
                .map(|(i, h)| {
                    let win = res.win[i] as f32;
                    // >0 = с блоком место ХУЖЕ (число больше) → блок стоил мест.
                    let block_kill = (res.avg_place[i] - res_nb.avg_place[i]) as f32;
                    (h.gate, (win, block_kill))
                })
                .collect();
            if let Ok(mut s) = slot.lock() {
                *s = Some((sig, map));
            }
            busy.store(false, std::sync::atomic::Ordering::SeqCst);
        });
    }
}

fn hp_color(pct: f32) -> Color32 {
    // Заливка бара HP — насыщенные тона, читаемые на светлом фоне (см. theme).
    theme::hp_fill(pct)
}

impl EguiOverlay for App {
    fn gui_run(
        &mut self,
        egui_context: &egui::Context,
        _gfx: &mut ThreeDBackend,
        glfw_backend: &mut GlfwBackend,
    ) {
        if !self.placed {
            self.placed = true;
            // Окно растягиваем на ВЕСЬ виртуальный рабочий стол (все мониторы),
            // прозрачное и сквозное. Тогда таблицу-egui можно перетащить куда
            // угодно, в т.ч. на другой монитор. По пустому месту клики проходят
            // в игру (passthrough), таблица ловит мышь только под курсором.
            let (vx, vy, vw, vh) = virtual_screen();
            glfw_backend.window.set_pos(vx, vy);
            glfw_backend.set_window_size([vw as f32, vh as f32]);
            set_window_icon(glfw_backend);
            // Подмешиваем системные CJK/кириллические шрифты, чтобы ники игроков
            // не превращались в квадраты.
            install_system_fonts(egui_context);
            // Светлая пастельная тема в стиле сайта (розовый/жёлтый/салатовый).
            // Окно-реплей рисует трек своим тёмным фоном — тема его не трогает.
            theme::install(egui_context);
        }

        self.poll_hotkeys();
        self.refresh();

        // Донат-окно при запуске: показываем первым, до остального оверлея.
        if self.donation_open {
            if !self.qr_tried {
                self.qr_tried = true;
                self.qr_tex = load_qr_texture(egui_context);
            }
            self.show_donation(egui_context);
            let over_ui =
                egui_context.is_pointer_over_area() || egui_context.wants_pointer_input();
            glfw_backend.set_passthrough(!over_ui);
            egui_context.request_repaint_after(Duration::from_millis(33));
            return;
        }

        let pre_race = self.pre_race();
        let active = self.race_active() || pre_race;
        let mut horses: Vec<Horse> = Vec::new();
        if self.visible && active {
            horses = self.snapshot.horses.clone();

            // Шанс победы считаем по ВСЕМ лошадям (до фильтра «только свои»),
            // чтобы проценты отражали весь состав забега. Сначала быстрая
            // эвристика, затем (когда фоновый прогон готов) — Monte Carlo
            // симуляция со скиллами поверх неё.
            compute_win_chances(&mut horses);
            self.apply_sim(&mut horses);

            // Имя своего тренера (для подписи/подсветки). СТАБИЛЬНО берём у
            // лошади с флагом is_user от игры (работает и в PvP, без ручного
            // выбора). Фолбэк: единственный непустой тренер (одиночный забег).
            if let Some(t) = horses
                .iter()
                .find(|h| h.is_user && !h.trainer.is_empty())
                .map(|h| h.trainer.clone())
            {
                if self.my_trainer != t {
                    self.my_trainer = t;
                    self.save_cfg();
                }
            } else if self.my_trainer.is_empty() {
                let mut named: Vec<&str> = horses
                    .iter()
                    .map(|h| h.trainer.as_str())
                    .filter(|t| !t.is_empty())
                    .collect();
                named.sort();
                named.dedup();
                if named.len() == 1 {
                    self.my_trainer = named[0].to_string();
                    self.save_cfg();
                }
            }

            // До старта все на нулевой дистанции — ранжируем по шансу победы,
            // в гонке — по пройденной дистанции/порядку финиша.
            let order_cmp: fn(&Horse, &Horse) -> std::cmp::Ordering =
                if pre_race { chance_cmp } else { rank_cmp };

            // Позицию в забеге считаем по ВСЕМ лошадям ДО фильтрации, чтобы
            // у своих трёх показывалось реальное место среди всех девяти.
            let mut by_rank = horses.clone();
            by_rank.sort_by(order_cmp);
            let rank_of: HashMap<i32, i32> = by_rank
                .iter()
                .enumerate()
                .map(|(i, h)| (h.gate, if h.finished { h.order } else { (i + 1) as i32 }))
                .collect();
            for h in &mut horses {
                h.rank = rank_of.get(&h.gate).copied().unwrap_or(0);
            }

            // Отрыв до лошади на одно место впереди. `finish_diff_time` из игры
            // (FinishDiffTime) — это и есть margin до соседа впереди; берём напрямую
            // (раньше ошибочно дифференцировали как будто это отрыв-до-победителя).
            let mut gap_of: HashMap<i32, f32> = HashMap::new();
            let finished: Vec<&Horse> =
                by_rank.iter().filter(|h| h.finished && h.stats_ready).collect();
            for w in finished.windows(2) {
                gap_of.insert(w[1].gate, w[1].finish_diff_time.max(0.0));
            }
            for h in &mut horses {
                h.gap_ahead = gap_of.get(&h.gate).copied().unwrap_or(f32::NAN);
            }

            // Контрфактуальное место «без блока» — из СИМУЛЯТОРА. apply_sim уже
            // проставил `block_kill` = на сколько мест трафик сдвинул среднее место
            // КАЖДОЙ лошади (avg место с блок-моделью − без неё, парное A/B). Возвращаем
            // реальному месту этот сдвиг (score = место − block_kill) и пересортировываем
            // всё поле. NaN (sim не готов) → сдвиг 0. Заменил старую эвристику
            // «finish_time − спурт-потеря» на полноценную реинтеракцию из sim.
            let mut cf: Vec<(i32, f32)> = finished
                .iter()
                .map(|h| {
                    let shift = if h.block_kill.is_nan() { 0.0 } else { h.block_kill };
                    (h.gate, h.rank as f32 - shift)
                })
                .collect();
            cf.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));
            let cf_place_of: HashMap<i32, i32> = cf
                .iter()
                .enumerate()
                .map(|(i, (g, _))| (*g, (i + 1) as i32))
                .collect();
            for h in &mut horses {
                h.cf_place = cf_place_of.get(&h.gate).copied().unwrap_or(0);
            }

            // Запоминаем «замороженный» снимок гонки (с рангами и win%), чтобы
            // после финиша показатели НЕ исчезали, а оставались как итог до
            // начала следующей гонки.
            self.frozen = Some((horses, pre_race));
        }

        // Источник отображения: живая гонка → свежие данные; гонки нет, но есть
        // прошлая → её «замороженный» итог. Сортировку/фильтр применяем ЗДЕСЬ,
        // чтобы переключатели (соперники/сортировка) работали и на итогах.
        let (mut horses, disp_pre_race, is_frozen) = match (self.frozen.clone(), active) {
            (Some((h, p)), true) => (h, p, false),
            (Some((h, p)), false) => (h, p, true),
            (None, _) => (Vec::new(), false, false),
        };
        if !horses.is_empty() {
            let order_cmp: fn(&Horse, &Horse) -> std::cmp::Ordering =
                if disp_pre_race { chance_cmp } else { rank_cmp };
            if self.sort_by_rank {
                horses.sort_by(order_cmp);
            } else {
                horses.sort_by_key(|h| h.gate);
            }
            // «Свои сверху»: стабильная сортировка сохраняет порядок внутри групп.
            if self.mine_first {
                horses.sort_by_key(|h| !self.is_mine(h));
            }
            // По дефолту — только свои; чужие включаются кнопкой/F6.
            if !self.show_enemies {
                let mine: Vec<Horse> =
                    horses.iter().filter(|h| self.is_mine(h)).cloned().collect();
                if !mine.is_empty() {
                    horses = mine;
                }
            }
        }

        // Окно показываем всегда, пока оверлей видим: гонка/итоги — таблица,
        // иначе — заглушка «waiting».
        if self.visible {
            self.show_main_window(egui_context, &horses, disp_pre_race, is_frozen);
        }
        if self.chance_disclaimer_open {
            self.show_chance_disclaimer(egui_context);
        }
        if self.feedback_view != FeedbackView::Closed {
            self.show_feedback(egui_context);
        }
        if self.replay_open {
            self.show_replay(egui_context);
        }

        // Окно сквозное, но когда курсор наведён на таблицу — автоматически
        // становится кликабельным (рекомендованный паттерн egui_overlay:
        // backend сам отслеживает курсор даже у passthrough-окна).
        // F7 — принудительный режим мыши на случай, если авто не сработает.
        let over_ui = egui_context.is_pointer_over_area() || egui_context.wants_pointer_input();
        glfw_backend.set_passthrough(!(self.interactive || over_ui));
        egui_context.request_repaint_after(Duration::from_millis(33));
    }
}

impl App {
    fn show_main_window(&mut self, ctx: &egui::Context, horses: &[Horse], pre_race: bool, frozen: bool) {
        // Итог завершённой гонки (для метки): данные есть и кто-то финишировал.
        let finished = frozen && horses.iter().any(|h| h.finished);
        // Позицию окна восстанавливаем из cfg (или дефолт), а после показа
        // запоминаем — чтобы перетаскивание на другой монитор сохранялось.
        let start_pos = self.win_pos.unwrap_or(egui::pos2(40.0, 90.0));
        // open=false при клике по крестику в заголовке; collapsible=true даёт
        // кнопку сворачивания (◀/▼) в шапке. Закрытие = спрятать оверлей (как F8).
        let mut open = true;
        let resp = egui::Window::new("Race Overlay")
            .default_size([620.0, 470.0])
            .default_pos(start_pos)
            .collapsible(true)
            .open(&mut open)
            .show(ctx, |ui| {
                let bg = theme::bg_reserve(ui);
                if horses.is_empty() {
                    // Вне гонки — заметная заглушка, окно можно таскать.
                    ui.add_space(6.0);
                    ui.label(RichText::new("Uma Race Overlay").size(20.0).strong());
                    ui.add_space(4.0);
                    ui.label(
                        RichText::new("Waiting for a race to start…")
                            .size(15.0)
                            .color(theme::C_WARN),
                    );
                    ui.add_space(2.0);
                    ui.label(
                        RichText::new("Enter a race and the table will appear here.")
                            .size(13.0),
                    );
                    ui.label(
                        RichText::new("Drag this window (by the title bar) to any monitor.")
                            .size(13.0),
                    );
                    ui.add_space(6.0);
                    ui.separator();
                    ui.label(
                        RichText::new("F8 hide · F10 %win rate · F9 base accel · F6 rivals")
                            .weak(),
                    );
                    ui.add_space(6.0);
                    self.feedback_button(ui);
                    ui.add_space(4.0);
                    theme::bg_fill(ui, bg);
                    return;
                }
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Umamusume PvP — HP / Speed").strong());
                    if finished {
                        ui.label(
                            RichText::new("● FINISHED — final results")
                                .color(theme::C_GOOD)
                                .small(),
                        )
                        .on_hover_text("Last race results. Updates when the next race starts.");
                    } else if frozen {
                        ui.label(RichText::new("last race").weak().small());
                    } else if pre_race {
                        ui.label(
                            RichText::new("pre-race")
                                .color(theme::C_WARN)
                                .small(),
                        );
                    }
                    ui.separator();
                    ui.checkbox(&mut self.sort_by_rank, "by position");
                    if ui
                        .checkbox(&mut self.mine_first, "mine on top")
                        .on_hover_text("My horses first, then rivals")
                        .changed()
                    {
                        self.save_cfg();
                    }
                    // Live acceleration column toggle (also key F9).
                    ui.checkbox(&mut self.show_accel, "base accel live")
                        .on_hover_text("Show the live acceleration column (m/s², key F9).");
                    // Spurt phase data: max spurt accel + max speed reached in spurt.
                    ui.checkbox(&mut self.show_spurt, "spurt phase data")
                        .on_hover_text(
                            "Show spurt phase stats: max spurt accel and\n\
                             max achieved speed during spurt.",
                        );
                    // Block analysis: time spent blocked + whether it likely cost a place.
                    ui.checkbox(&mut self.show_block, "block analysis")
                        .on_hover_text(
                            "Real race post-mortem from the game's own data:\n\
                             time spent blocked (front), kakari (rush), and an\n\
                             estimate of whether blocking cost a place.\n\
                             Fills in after the race finishes.",
                        );
                    if ui
                        .checkbox(&mut self.show_chance, "%win rate")
                        .on_hover_text(
                            "Win chance (key F10) — IN DEVELOPMENT, may be inaccurate.\n\
                             Monte Carlo simulation of ~500 virtual races using the\n\
                             game's own formulas: stats, aptitudes, motivation, HP/spurt\n\
                             and skills from master.mdb.\n\
                             Not modeled: positional blocking, slopes, weather.",
                        )
                        .changed()
                    {
                        self.on_chance_enabled();
                    }
                });
                ui.label(
                    RichText::new("F8 hide · F9 base accel · F6 rivals · F10 %win rate · buttons are clickable")
                        .weak()
                        .small(),
                );
                ui.separator();
                race_table(
                    ui,
                    horses,
                    self.show_accel,
                    self.show_spurt,
                    self.show_block,
                    self.show_chance,
                    &self.my_trainer,
                );
                ui.separator();
                self.bottom_bar(ui);
                theme::bg_fill(ui, bg);
            });

        // Запоминаем позицию окна после перетаскивания (с троттлингом записи).
        if let Some(r) = resp {
            let pos = r.response.rect.min;
            let moved = self.win_pos.map_or(true, |p| (p - pos).length() > 1.0);
            if moved {
                self.win_pos = Some(pos);
                if self.last_pos_save.elapsed() > Duration::from_millis(700) {
                    self.last_pos_save = Instant::now();
                    self.save_cfg();
                }
            }
        }

        // Крестик в заголовке = полностью закрыть программу (выйти из процесса).
        // Это то, чего ждёт пользователь от «закрыть». Временно спрятать оверлей —
        // отдельная клавиша F8 (меняет visible), процесс при этом живёт.
        if !open {
            self.save_cfg();
            std::process::exit(0);
        }
    }

    /// Донат-окно при запуске приложения.
    fn show_donation(&mut self, ctx: &egui::Context) {
        // default_pos (а НЕ anchor!) — иначе окно прибито и не перетаскивается.
        // open=крестик закрытия, collapsible=кнопка сворачивания в шапке.
        let mut open = true;
        egui::Window::new("Support the developer 💜")
            .collapsible(true)
            .resizable(false)
            .open(&mut open)
            .default_pos(primary_center_egui(380.0, 480.0))
            .show(ctx, |ui| {
                let bg = theme::bg_reserve(ui);
                ui.set_max_width(360.0);
                ui.vertical_centered(|ui| {
                    theme::uma_marks(ui);
                    ui.add_space(4.0);
                    ui.label(
                        RichText::new(
                            "Please help me pay my rent and improve the app. \
                             I'd appreciate any support. \
                             Thank you for all the support!",
                        )
                        .size(15.0),
                    );
                    ui.add_space(2.0);
                    ui.label(RichText::new("— SupperMommy").italics().weak());
                    ui.add_space(10.0);

                    // QR-код (donat.png рядом с exe).
                    if let Some(tex) = &self.qr_tex {
                        ui.label(RichText::new("Scan to donate").strong().size(14.0));
                        ui.image(egui::load::SizedTexture::new(
                            tex.id(),
                            egui::vec2(190.0, 190.0),
                        ));
                    } else {
                        ui.label(
                            RichText::new("(donat.png not found — place it next to the .exe)")
                                .weak()
                                .small(),
                        );
                    }
                    ui.add_space(8.0);

                    // Кликабельная донат-ссылка (заметная) + сырой URL для копирования.
                    let donate_btn = egui::Button::new(
                        RichText::new("💜  Donation link  💜")
                            .size(17.0)
                            .strong()
                            .color(Color32::WHITE),
                    )
                    .fill(theme::PINK)
                    .min_size(egui::vec2(220.0, 34.0));
                    if ui
                        .add(donate_btn)
                        .on_hover_text("Open the donation page in your browser")
                        .clicked()
                    {
                        open_url(&self.donate_url);
                    }
                    ui.add_space(2.0);
                    ui.label(RichText::new("or open this link:").small().weak());
                    ui.add(egui::Label::new(
                        RichText::new(&self.donate_url)
                            .small()
                            .color(theme::C_MINE),
                    ));
                    ui.add_space(12.0);

                    if ui
                        .add(
                            egui::Button::new(RichText::new("Continue").size(15.0).strong())
                                .min_size(egui::vec2(120.0, 30.0)),
                        )
                        .clicked()
                    {
                        self.donation_open = false;
                    }
                });
                theme::bg_fill(ui, bg);
            });
        // Крестик в заголовке = закрыть донат-окно (как кнопка Continue).
        if !open {
            self.donation_open = false;
        }
    }

    /// Одноразовое (за запуск) окно-предупреждение при включении винрейта.
    fn show_chance_disclaimer(&mut self, ctx: &egui::Context) {
        // Рядом с таблицей (а не в центре всех мониторов).
        let near = self.win_pos.unwrap_or(egui::pos2(40.0, 90.0)) + egui::vec2(24.0, 70.0);
        let mut open = true;
        egui::Window::new("%win rate — in development")
            .collapsible(true)
            .resizable(false)
            .open(&mut open)
            .default_pos(near)
            .show(ctx, |ui| {
                let bg = theme::bg_reserve(ui);
                ui.set_max_width(360.0);
                ui.label(
                    RichText::new("⚠ This feature is in development")
                        .color(theme::C_WARN)
                        .strong(),
                );
                ui.add_space(4.0);
                ui.label(
                    "The win-rate column is an experimental prediction (Monte Carlo \
                     simulation). It may be inaccurate — positional blocking, slopes \
                     and weather are not fully modeled yet. Treat the numbers as a \
                     rough estimate, not a guarantee.",
                );
                ui.add_space(8.0);
                ui.vertical_centered(|ui| {
                    if ui.button("Got it").clicked() {
                        self.chance_disclaimer_open = false;
                    }
                });
                theme::bg_fill(ui, bg);
            });
        if !open {
            self.chance_disclaimer_open = false;
        }
    }

    /// Окно-реплей: трек с маркерами скиллов, ползунок времени, живые статы.
    /// Геометрию курса берём из таблицы по course_id архива (для раскраски трека).
    fn show_replay(&mut self, ctx: &egui::Context) {
        let gd = self.gamedata.clone();
        // Геометрия курса для раскраски трека (опционально).
        let geom: Option<course::CourseGeom> = self.replay.as_ref().and_then(|r| {
            let cid = r.race.course_id;
            self.courses.as_ref()?.get(&cid).cloned()
        });
        // Размер/позиция: вписываем в ОСНОВНОЙ монитор (не растягиваем на все
        // экраны) и центрируем — окно широкое (горизонтальный график).
        let win_w = 1240.0_f32;
        let win_h = 620.0_f32;
        let center = primary_center_egui(win_w, win_h);
        let mut open = true;
        // Отдельный флаг для крестика в заголовке (нельзя одолжить `open` дважды:
        // он же используется внутренней кнопкой «✕ Close»).
        let mut win_open = true;
        let mut reload = false;
        let replay = &mut self.replay;
        // Крестик в заголовке + сворачивание (collapsible по умолчанию). ВАЖНО:
        // сам трек/поле (r.ui) не трогаем — оно рисует свой тёмный фон и не
        // зависит от темы.
        egui::Window::new("📈 Race map")
            .default_size([win_w, win_h])
            .collapsible(true)
            .open(&mut win_open)
            .default_pos(center)
            .show(ctx, |ui| {
                let bg = theme::bg_reserve(ui);
                ui.horizontal(|ui| {
                    if ui
                        .button("⟳ Reload latest")
                        .on_hover_text("Load the most recently finished race again.")
                        .clicked()
                    {
                        reload = true;
                    }
                    if ui.button("✕ Close").clicked() {
                        open = false;
                    }
                });
                ui.separator();
                match replay.as_mut() {
                    Some(r) => r.ui(ui, gd.as_deref(), geom.as_ref()),
                    None => {
                        ui.add_space(6.0);
                        ui.label(
                            RichText::new("No saved race found yet.")
                                .size(15.0)
                                .color(theme::C_WARN),
                        );
                        ui.label("Finish a race (it is archived automatically), then reload.");
                    }
                }
                theme::bg_fill(ui, bg);
            });
        // Закрыть по внутренней кнопке ИЛИ по крестику в заголовке окна.
        if !open || !win_open {
            self.replay_open = false;
        }
        if reload {
            self.load_replay();
        }
    }

    /// Bottom bar: rivals toggle, manual "my trainer" pick, feedback button.
    fn bottom_bar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            self.feedback_button(ui);
            ui.separator();
            if ui
                .button("📈 Race map")
                .on_hover_text(
                    "Open the race replay: stylized track with where each girl\n\
                     used which skill, a time scrubber and live stats (key F11).\n\
                     Shows the most recently finished race.",
                )
                .clicked()
            {
                self.toggle_replay();
            }
            ui.separator();
            if ui
                .checkbox(&mut self.show_enemies, "👥 rivals")
                .on_hover_text("Show other players' horses (key F6).")
                .changed()
            {}

            // Manual "my trainer" pick (if auto-detect guessed wrong, e.g. the
            // first race is straight PvP with several trainers). Click your
            // name — it is remembered permanently.
            if self.show_enemies {
                let mut trainers: Vec<String> = self
                    .snapshot
                    .horses
                    .iter()
                    .map(|h| h.trainer.clone())
                    .filter(|t| !t.is_empty())
                    .collect();
                trainers.sort();
                trainers.dedup();
                if !trainers.is_empty() {
                    ui.separator();
                    ui.label(RichText::new("me:").weak());
                    for t in trainers {
                        if ui.selectable_label(t == self.my_trainer, t.as_str()).clicked() {
                            self.my_trainer = t;
                            self.save_cfg();
                        }
                    }
                }
            }

            // Курс трассы (course_id) для реальной геометрии. Приоритет — авто-детект
            // из игры; ручной ввод остаётся фолбэком, когда авто недоступен.
            // Видно при включённом win rate.
            if self.show_chance && self.courses.is_some() {
                ui.separator();
                let auto = self.snapshot.course_id;
                if auto > 0 {
                    // Курс определён игрой — показываем и помечаем, есть ли в таблице.
                    let ok = self
                        .courses
                        .as_ref()
                        .and_then(|c| c.get(&auto))
                        .is_some();
                    ui.label(RichText::new("course:").weak())
                        .on_hover_text("Auto-detected from the game (RaceCourseSet id).");
                    ui.label(RichText::new(format!("auto {}", auto)).weak());
                    let mark = if ok {
                        RichText::new("✓").color(theme::C_GOOD)
                    } else {
                        RichText::new("not in table").color(Color32::from_rgb(230, 57, 53))
                    };
                    ui.label(mark);
                } else {
                    // Авто недоступно (старый плагин/вне гонки) — ручной ввод.
                    ui.label(RichText::new("course:").weak()).on_hover_text(
                        "Course id (e.g. Hanshin 2200m = 10906). Empty = generic geometry.",
                    );
                    let resp = ui.add(
                        egui::TextEdit::singleline(&mut self.course_text)
                            .desired_width(56.0)
                            .hint_text("id"),
                    );
                    if resp.changed() {
                        self.course_id = self.course_text.trim().parse::<i32>().ok();
                        self.save_cfg();
                    }
                    let ok = self
                        .course_id
                        .and_then(|id| self.courses.as_ref()?.get(&id))
                        .is_some();
                    let mark = match (self.course_id, ok) {
                        (None, _) => RichText::new("generic").weak(),
                        (Some(_), true) => RichText::new("✓").color(theme::C_GOOD),
                        (Some(_), false) => {
                            RichText::new("not found").color(Color32::from_rgb(230, 57, 53))
                        }
                    };
                    ui.label(mark);
                }
            }
        });
    }

    /// Кнопка-открывашка фидбэка (в нижней панели и в заглушке «waiting»).
    fn feedback_button(&mut self, ui: &mut egui::Ui) {
        if ui
            .button("✉ Feedback")
            .on_hover_text("Report a bug or suggest an idea (sends to the developer).")
            .clicked()
        {
            self.feedback_view = FeedbackView::Menu;
        }
    }

    /// Сбрасывает поля формы и открывает указанное окно фидбэка.
    fn fb_open(&mut self, view: FeedbackView) {
        self.fb_areas = [false; FB_AREAS.len()];
        self.fb_text.clear();
        self.fb_contact.clear();
        // По умолчанию прикладываем гонку, если есть завершённая.
        self.fb_attach = self.frozen.is_some();
        if let Ok(mut s) = self.fb_status.lock() {
            *s = feedback::SendStatus::Idle;
        }
        self.feedback_view = view;
    }

    /// Сводка активных тоглов (контекст для бага — компактно, не напрягает).
    fn toggles_summary(&self) -> String {
        let mut v: Vec<&str> = Vec::new();
        if self.show_accel {
            v.push("accel");
        }
        if self.show_spurt {
            v.push("spurt");
        }
        if self.show_block {
            v.push("block");
        }
        if self.show_chance {
            v.push("win%");
        }
        if self.show_enemies {
            v.push("rivals");
        }
        if v.is_empty() {
            "none".to_string()
        } else {
            v.join(",")
        }
    }

    /// Выбранные области бага («где произошло») через запятую.
    fn fb_areas_str(&self) -> String {
        FB_AREAS
            .iter()
            .enumerate()
            .filter(|(i, _)| self.fb_areas[*i])
            .map(|(_, n)| *n)
            .collect::<Vec<_>>()
            .join(", ")
    }

    /// Отправляет текущую форму (баг или идея) на релей в фоне. Формат сообщения
    /// собирает Worker — клиент шлёт только структурированные поля + анонимный id.
    fn fb_send(&mut self, bug: bool) {
        if !self.relay_cfg.configured() || self.fb_text.trim().is_empty() {
            return;
        }
        // Снимок гонки делаем здесь (в UI-потоке), чтобы фоновую отправку не
        // затронула перезапись живого state.json следующей гонкой.
        let race = if bug && self.fb_attach {
            std::fs::read(&self.path).ok()
        } else {
            None
        };
        let report = feedback::Report {
            install_id: self.install_id.clone(),
            kind_bug: bug,
            text: self.fb_text.trim().to_string(),
            contact: self.fb_contact.trim().to_string(),
            areas: if bug { self.fb_areas_str() } else { String::new() },
            toggles: if bug { self.toggles_summary() } else { String::new() },
            app_version: APP_VERSION.to_string(),
            os: std::env::consts::OS.to_string(),
            race,
        };
        feedback::spawn_send(self.relay_cfg.clone(), report, Arc::clone(&self.fb_status));
    }

    /// Рисует окно фидбэка (меню / баг / идея) в зависимости от feedback_view.
    fn show_feedback(&mut self, ctx: &egui::Context) {
        match self.feedback_view {
            FeedbackView::Menu => self.show_feedback_menu(ctx),
            FeedbackView::Bug => self.show_feedback_form(ctx, true),
            FeedbackView::Idea => self.show_feedback_form(ctx, false),
            FeedbackView::Closed => {}
        }
    }

    fn show_feedback_menu(&mut self, ctx: &egui::Context) {
        let near = self.win_pos.unwrap_or(egui::pos2(40.0, 90.0)) + egui::vec2(40.0, 60.0);
        let mut open = true;
        egui::Window::new("Send feedback ✉")
            .collapsible(true)
            .resizable(false)
            .open(&mut open)
            .default_pos(near)
            .show(ctx, |ui| {
                let bg = theme::bg_reserve(ui);
                ui.set_max_width(280.0);
                theme::uma_marks(ui);
                ui.label("Help improve the overlay — pick one:");
                ui.add_space(8.0);
                if ui
                    .add(
                        egui::Button::new(RichText::new("🐞  Report a bug").size(15.0))
                            .min_size(egui::vec2(240.0, 32.0)),
                    )
                    .clicked()
                {
                    self.fb_open(FeedbackView::Bug);
                }
                ui.add_space(4.0);
                if ui
                    .add(
                        egui::Button::new(RichText::new("💡  Suggest an idea").size(15.0))
                            .min_size(egui::vec2(240.0, 32.0)),
                    )
                    .clicked()
                {
                    self.fb_open(FeedbackView::Idea);
                }
                ui.add_space(8.0);
                ui.vertical_centered(|ui| {
                    if ui.button("Close").clicked() {
                        self.feedback_view = FeedbackView::Closed;
                    }
                });
                theme::bg_fill(ui, bg);
            });
        if !open {
            self.feedback_view = FeedbackView::Closed;
        }
    }

    /// Общая форма для бага (`bug=true`, с галочками + приложить гонку) и
    /// идеи (`bug=false`, только текст).
    fn show_feedback_form(&mut self, ctx: &egui::Context, bug: bool) {
        let near = self.win_pos.unwrap_or(egui::pos2(40.0, 90.0)) + egui::vec2(40.0, 60.0);
        let title = if bug { "🐞 Report a bug" } else { "💡 Suggest an idea" };
        let status = self.fb_status.lock().map(|s| s.clone()).unwrap_or(feedback::SendStatus::Idle);
        let sending = status == feedback::SendStatus::Sending;
        let sent = status == feedback::SendStatus::Sent;

        let mut open = true;
        egui::Window::new(title)
            .collapsible(true)
            .resizable(false)
            .open(&mut open)
            .default_pos(near)
            .show(ctx, |ui| {
                let bg = theme::bg_reserve(ui);
                ui.set_max_width(360.0);

                if sent {
                    // После успешной отправки — благодарность вместо формы.
                    ui.add_space(4.0);
                    ui.label(
                        RichText::new("✓ Sent — thank you!")
                            .color(theme::C_GOOD)
                            .size(15.0)
                            .strong(),
                    );
                    ui.add_space(8.0);
                    ui.vertical_centered(|ui| {
                        if ui.button("Close").clicked() {
                            self.feedback_view = FeedbackView::Closed;
                        }
                    });
                    theme::bg_fill(ui, bg);
                    return;
                }

                if bug {
                    ui.label(RichText::new("Where did it happen?").strong());
                    ui.add_space(2.0);
                    egui::Grid::new("fb_areas").num_columns(2).spacing([12.0, 2.0]).show(
                        ui,
                        |ui| {
                            for (i, name) in FB_AREAS.iter().enumerate() {
                                ui.checkbox(&mut self.fb_areas[i], *name);
                                if i % 2 == 1 {
                                    ui.end_row();
                                }
                            }
                        },
                    );
                    ui.add_space(6.0);
                    ui.label(RichText::new("Describe the bug:").strong());
                } else {
                    ui.label(RichText::new("Your idea / suggestion:").strong());
                }

                ui.add(
                    egui::TextEdit::multiline(&mut self.fb_text)
                        .desired_rows(4)
                        .desired_width(f32::INFINITY)
                        .hint_text(if bug {
                            "Steps to reproduce, what you expected vs what happened…"
                        } else {
                            "What would you like added or changed?"
                        }),
                );
                ui.add_space(4.0);

                if bug {
                    let has_race = self.frozen.is_some();
                    ui.add_enabled_ui(has_race, |ui| {
                        ui.checkbox(&mut self.fb_attach, "Attach last race data")
                            .on_hover_text(
                                "Sends the finished race (horses, blocks, skills) as a\n\
                                 file so the bug can be reproduced. No personal data.",
                            );
                    });
                    if !has_race {
                        ui.label(
                            RichText::new("(no finished race to attach yet)")
                                .weak()
                                .small(),
                        );
                    }
                    ui.add_space(4.0);
                }

                ui.horizontal(|ui| {
                    ui.label("Contact (optional):");
                    ui.add(
                        egui::TextEdit::singleline(&mut self.fb_contact)
                            .desired_width(200.0)
                            .hint_text("Telegram / Discord — so I can reply"),
                    );
                });
                ui.add_space(6.0);

                if !self.relay_cfg.configured() {
                    ui.label(
                        RichText::new("⚠ Feedback is not configured in this build yet.")
                            .color(theme::C_WARN)
                            .small(),
                    );
                    ui.add_space(4.0);
                }

                ui.horizontal(|ui| {
                    let can_send = self.relay_cfg.configured()
                        && !self.fb_text.trim().is_empty()
                        && !sending;
                    if ui
                        .add_enabled(can_send, egui::Button::new(RichText::new("Send").strong()))
                        .clicked()
                    {
                        self.fb_send(bug);
                    }
                    if ui.button("Back").clicked() {
                        self.feedback_view = FeedbackView::Menu;
                    }
                    if ui.button("Close").clicked() {
                        self.feedback_view = FeedbackView::Closed;
                    }
                    match &status {
                        feedback::SendStatus::Sending => {
                            ui.label(RichText::new("sending…").weak());
                        }
                        feedback::SendStatus::RateLimited(min) => {
                            ui.label(
                                RichText::new(format!(
                                    "⏳ Too soon — please wait ~{min} min and try again."
                                ))
                                .color(theme::C_WARN)
                                .small(),
                            );
                        }
                        feedback::SendStatus::Blocked => {
                            ui.label(
                                RichText::new("✗ Couldn't send (too many requests).")
                                    .color(theme::C_BAD)
                                    .small(),
                            );
                        }
                        feedback::SendStatus::Failed(e) => {
                            ui.label(
                                RichText::new(format!("✗ failed: {e}"))
                                    .color(theme::C_BAD)
                                    .small(),
                            );
                        }
                        _ => {}
                    }
                });
                theme::bg_fill(ui, bg);
            });
        if !open {
            self.feedback_view = FeedbackView::Closed;
        }
    }
}

// ---------------------------------------------------------------------------
// Оценка шанса победы.
//
// Полный симулятор гонки (как UmaLator) тащить не стали: скиллы и RNG ума
// всё равно не воспроизвести по доступным данным. Вместо этого — эвристика
// на формулах из декомпила движка гонок:
//   * дистанция трассы восстанавливается из стартового HP:
//     maxHP = дистанция + 0.8 * коэф_стиля * выносливость;
//   * аптитуд дистанции масштабирует вклад скорости (как в target speed),
//     аптитуд стиля — вклад ума (как в игре);
//   * веса статов зависят от категории дистанции (спринт/миля/средняя/длинная);
//   * итоговые очки прогоняются через softmax → проценты на весь состав.
// ---------------------------------------------------------------------------

/// Коэффициент стиля бега в формуле стартового HP (из движка игры).
fn style_hp_coef(style: i32) -> f32 {
    match style {
        1 => 0.95,  // nige (лидер)
        2 => 0.89,  // senko (преследователь)
        3 => 1.0,   // sashi (на отрезке)
        4 => 0.995, // oikomi (на финише)
        _ => 1.0,
    }
}

/// Множитель скорости по аптитуду дистанции (1=G..8=S, из движка игры).
fn dist_apt_coef(apt: Option<i32>) -> f32 {
    match apt {
        Some(a) => [0.1, 0.2, 0.4, 0.6, 0.8, 0.9, 1.0, 1.05][(a.clamp(1, 8) - 1) as usize],
        None => 1.0, // нет данных — не штрафуем
    }
}

/// Множитель ума по аптитуду стиля бега (1=G..8=S, из движка игры).
fn style_apt_coef(apt: Option<i32>) -> f32 {
    match apt {
        Some(a) => [0.1, 0.2, 0.4, 0.6, 0.75, 0.85, 1.0, 1.1][(a.clamp(1, 8) - 1) as usize],
        None => 1.0,
    }
}

/// Восстановить дистанцию трассы из стартовых HP состава (медиана по лошадям,
/// округление до сотни — все трассы кратны 100 м). None — данных нет.
fn deduce_course_distance(horses: &[Horse]) -> Option<f32> {
    let mut estimates: Vec<f32> = horses
        .iter()
        .filter_map(|h| {
            let stamina = h.stat(1)?;
            if h.max_hp <= 0.0 {
                return None;
            }
            let d = h.max_hp - 0.8 * style_hp_coef(h.style) * stamina;
            (600.0..5000.0).contains(&d).then_some(d)
        })
        .collect();
    if estimates.is_empty() {
        return None;
    }
    estimates.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let median = estimates[estimates.len() / 2];
    Some((median / 100.0).round() * 100.0)
}

/// Веса статов (скорость, выносливость, сила, упорство, ум) по категории
/// дистанции: 0 спринт (≤1400), 1 миля (≤1800), 2 средняя (≤2400), 3 длинная.
fn stat_weights(category: usize) -> [f32; 5] {
    match category {
        0 => [1.0, 0.25, 0.9, 0.3, 0.5],
        1 => [1.0, 0.45, 0.8, 0.3, 0.5],
        2 => [1.0, 0.7, 0.7, 0.35, 0.5],
        _ => [1.0, 0.95, 0.6, 0.4, 0.5],
    }
}

/// Считает win_chance (0..1) каждой лошади; у лошадей без статов остаётся -1,
/// проценты распределяются между остальными. Сумма по валидным = 1.
fn compute_win_chances(horses: &mut [Horse]) {
    for h in horses.iter_mut() {
        h.win_chance = -1.0;
    }

    let distance = deduce_course_distance(horses).unwrap_or(2000.0);
    let category = match distance as i32 {
        ..=1400 => 0,
        1401..=1800 => 1,
        1801..=2400 => 2,
        _ => 3,
    };
    let w = stat_weights(category);
    let w_sum: f32 = w.iter().sum();

    // Очки = взвешенное среднее статов с поправками аптитудов (масштаб ~ статов).
    let scores: Vec<Option<f32>> = horses
        .iter()
        .map(|h| {
            let speed = h.stat(0)? * dist_apt_coef(h.aptitude(category));
            let stamina = h.stat(1)?;
            let pow = h.stat(2)?;
            let guts = h.stat(3)?;
            let wiz = h.stat(4)? * style_apt_coef(h.aptitude(4));
            Some(
                (w[0] * speed + w[1] * stamina + w[2] * pow + w[3] * guts + w[4] * wiz) / w_sum,
            )
        })
        .collect();

    let valid: Vec<f32> = scores.iter().filter_map(|s| *s).collect();
    if valid.is_empty() {
        return;
    }
    let n = valid.len() as f32;

    // Softmax. Температура подобрана так, чтобы разрыв в ~60 очков взвешенного
    // среднего давал ~e раз больший шанс. Плюс примесь равномерного
    // распределения: исход гонки всегда с долей лотереи (RNG ума, скиллы).
    const TEMPERATURE: f32 = 60.0;
    const LUCK: f32 = 0.15;
    let max_score = valid.iter().cloned().fold(f32::MIN, f32::max);
    let exp_sum: f32 = valid.iter().map(|s| ((s - max_score) / TEMPERATURE).exp()).sum();

    for (h, score) in horses.iter_mut().zip(scores) {
        if let Some(s) = score {
            let softmax = ((s - max_score) / TEMPERATURE).exp() / exp_sum;
            h.win_chance = softmax * (1.0 - LUCK) + LUCK / n;
        }
    }
}

/// Порядок «по шансу победы» (до старта): фавориты сверху, без данных — вниз.
fn chance_cmp(a: &Horse, b: &Horse) -> std::cmp::Ordering {
    b.win_chance
        .partial_cmp(&a.win_chance)
        .unwrap_or(std::cmp::Ordering::Equal)
        .then(a.gate.cmp(&b.gate))
}

/// Порядок «по позиции в забеге»: финишировавшие — по порядку финиша,
/// остальные — по пройденной дистанции.
fn rank_cmp(a: &Horse, b: &Horse) -> std::cmp::Ordering {
    match (a.finished, b.finished) {
        (true, true) => a.order.cmp(&b.order),
        (true, false) => std::cmp::Ordering::Less,
        (false, true) => std::cmp::Ordering::Greater,
        (false, false) => b
            .distance
            .partial_cmp(&a.distance)
            .unwrap_or(std::cmp::Ordering::Equal),
    }
}

/// Единая таблица. При show_accel/show_spurt к каждой строке добавляются колонки
/// справа (окно само расширяется вправо), строки идеально выровнены.
/// show_accel — «base accel live» (1 колонка); show_spurt — «spurt phase data»
/// (2 колонки: max spurt accel + max spurt speed).
fn race_table(
    ui: &mut egui::Ui,
    horses: &[Horse],
    show_accel: bool,
    show_spurt: bool,
    show_block: bool,
    show_chance: bool,
    my_trainer: &str,
) {
    let cols = 4
        + usize::from(show_chance)
        + if show_accel { 1 } else { 0 }
        + if show_spurt { 2 } else { 0 }
        + if show_block { 3 } else { 0 };
    egui::Grid::new("horses")
        .num_columns(cols)
        .striped(true)
        .spacing([14.0, 7.0])
        .show(ui, |ui| {
            ui.label(RichText::new("#").strong());
            ui.label(RichText::new("Horse").strong());
            if show_chance {
                ui.label(RichText::new("Win%").strong()).on_hover_text(
                    "Monte Carlo: ~200 virtual races using the game's formulas\n\
                     (stats, aptitudes, motivation, HP, spurt, skills).\n\
                     Positional blocking and slopes are not modeled.",
                );
            }
            ui.label(RichText::new("Stamina (HP)").strong());
            ui.label(RichText::new("Speed").strong());
            if show_accel {
                ui.label(RichText::new("accel m/s²").strong())
                    .on_hover_text("Live acceleration (base accel).");
            }
            if show_spurt {
                ui.label(RichText::new("max spurt accel").strong())
                    .on_hover_text("Max acceleration reached during spurt.");
                ui.label(RichText::new("max spurt speed").strong())
                    .on_hover_text("Max achieved speed during spurt (m/s).");
            }
            if show_block {
                ui.label(RichText::new("mid leg block").strong()).on_hover_text(
                    "Total time spent blocked from the front BEFORE the spurt began\n\
                     (early + middle leg), in seconds. Block here rarely costs the\n\
                     race — it mostly resolves before the decisive stage — but it\n\
                     shows how congested the horse's trip was.",
                );
                ui.label(RichText::new("spurt block").strong()).on_hover_text(
                    "Time spent blocked from the front DURING the spurt\n\
                     (and episode count). Block in the spurt is what really\n\
                     hurts — it chokes acceleration at the decisive stage.",
                );
                ui.label(RichText::new("place w/o block").strong()).on_hover_text(
                    "Counterfactual finishing place if NOBODY had been blocked,\n\
                     from the win-rate simulator: the field is run WITH vs WITHOUT the\n\
                     blocking model, each horse's real place is shifted by its modeled\n\
                     block effect, then re-ranked. 'P5→P2' = would have placed 2nd.\n\
                     Hover a cell for the exact place delta. A model estimate, not a guarantee.",
                );
            }
            ui.end_row();

            for h in horses.iter() {
                ui.label(format!("{}", h.rank));
                // Свои лошади подсвечены голубым (по флагу is_user от игры, иначе
                // по имени тренера).
                if h.is_user || (!my_trainer.is_empty() && h.trainer == my_trainer) {
                    ui.label(
                        RichText::new(horse_label(h)).color(theme::C_MINE),
                    );
                } else {
                    ui.label(horse_label(h));
                }

                if show_chance {
                    if h.win_chance >= 0.0 {
                        let p = h.win_chance;
                        // фавориты зелёные, аутсайдеры серые
                        let col = if p >= 0.20 {
                            theme::C_GOOD
                        } else if p >= 0.10 {
                            theme::C_FADE
                        } else {
                            Color32::GRAY
                        };
                        ui.label(
                            RichText::new(format!("{:.1}%", p * 100.0))
                                .color(col)
                                .strong()
                                .monospace(),
                        );
                    } else {
                        ui.label(RichText::new("—").weak());
                    }
                }

                if h.max_hp > 0.0 {
                    let pct = (h.hp / h.max_hp).clamp(0.0, 1.0);
                    ui.add(
                        egui::ProgressBar::new(pct)
                            .desired_width(180.0)
                            .fill(hp_color(pct))
                            .text(format!("{:.0}  ({:.0}%)", h.hp, pct * 100.0)),
                    );
                } else {
                    // до старта HP ещё неизвестен
                    ui.add(
                        egui::ProgressBar::new(0.0)
                            .desired_width(180.0)
                            .fill(Color32::from_gray(60))
                            .text(RichText::new("—").weak()),
                    );
                }

                if h.finished {
                    ui.label(RichText::new("done").weak());
                } else if h.speed > 0.01 {
                    // Индикатор спурта встроен в ячейку Speed (без отдельного столбца):
                    // во время спурта скорость оранжевая с пометкой.
                    if h.spurt {
                        ui.label(
                            RichText::new(format!("{:.2} m/s ⏫", h.speed))
                                .color(Color32::from_rgb(255, 115, 25))
                                .strong(),
                        )
                        .on_hover_text("SPURT");
                    } else {
                        ui.label(format!("{:.2} m/s", h.speed));
                    }
                } else {
                    // speed not available before the start
                    ui.label(RichText::new("—").weak());
                }

                if show_accel {
                    // ускорение: зелёное при разгоне, красное при торможении
                    let acc = h.accel;
                    let col = if acc > 0.02 {
                        theme::C_GOOD
                    } else if acc < -0.02 {
                        Color32::from_rgb(230, 57, 53)
                    } else {
                        Color32::GRAY
                    };
                    ui.label(RichText::new(format!("{:+.2}", acc)).color(col).monospace());
                }

                if show_spurt {
                    // max spurt accel (пик ускорения за спурт)
                    let peak = h.max_spurt_accel;
                    if peak > 0.0 {
                        ui.label(
                            RichText::new(format!("{:.2}", peak))
                                .color(theme::C_SPURT)
                                .strong()
                                .monospace(),
                        );
                    } else {
                        ui.label(RichText::new("—").weak());
                    }
                    // max achieved speed during spurt
                    let sp = h.max_spurt_speed;
                    if sp > 0.0 {
                        ui.label(
                            RichText::new(format!("{:.2} m/s", sp))
                                .color(theme::C_SPURT)
                                .strong()
                                .monospace(),
                        );
                    } else {
                        ui.label(RichText::new("—").weak());
                    }
                }

                if show_block {
                    let green = theme::C_GOOD;
                    let orange = theme::C_SPURT;
                    let red = Color32::from_rgb(230, 57, 53);
                    // Тултип со справкой по полному блоку/закидыванию.
                    let detail = format!(
                        "Total front block (any phase): {:.1}s ×{}. Kakari: {:.1}s.",
                        h.blocked_time, h.blocked_episodes, h.kakari_time
                    );
                    // Колонка «mid leg block»: суммарный блок ДО спурта (сек).
                    if !h.stats_ready {
                        ui.label(RichText::new("—").weak());
                    } else if h.pre_spurt_blocked_time < 0.05 {
                        ui.label(RichText::new("clean").color(green).monospace())
                            .on_hover_text(format!("No front block before the spurt.\n{}", detail));
                    } else {
                        ui.label(
                            RichText::new(format!("{:.1}s", h.pre_spurt_blocked_time))
                                .color(orange)
                                .monospace(),
                        )
                        .on_hover_text(format!(
                            "Blocked {:.1}s before the spurt (early + middle leg).\n{}",
                            h.pre_spurt_blocked_time, detail
                        ));
                    }
                    // Колонка «spurt block»: блок В СПУРТЕ (по реальным кадрам).
                    if !h.stats_ready {
                        ui.label(RichText::new("—").weak());
                    } else if h.spurt_blocked_time < 0.05 {
                        ui.label(RichText::new("clean").color(green).monospace())
                            .on_hover_text(format!("No block during the spurt.\n{}", detail));
                    } else {
                        ui.label(
                            RichText::new(format!(
                                "{:.1}s ×{}",
                                h.spurt_blocked_time, h.spurt_blocked_episodes
                            ))
                            .color(orange)
                            .monospace(),
                        )
                        .on_hover_text(format!(
                            "Blocked {:.1}s over {} episode(s) DURING the spurt \
                             (~{:.0} m lost).\n{}",
                            h.spurt_blocked_time, h.spurt_blocked_episodes, h.spurt_lost_dist, detail
                        ));
                    }

                    // Колонка «place w/o block»: контрфактуальное МЕСТО из симулятора
                    // (реальному месту возвращён модельный эффект блока, поле пересортировано).
                    // Точная дельта мест (block_kill) — в тултипе. cf==0 → sim ещё не готов.
                    let cf = h.cf_place;
                    let bk = h.block_kill;
                    let delta_line = if bk.is_nan() {
                        "Model block impact: simulating…".to_string()
                    } else {
                        format!("Model block impact on this horse: {:+.1} places (avg).", bk)
                    };
                    let tip = format!(
                        "Counterfactual finishing place from the win-rate simulator:\n\
                         the field is run WITH vs WITHOUT the blocking model, and each\n\
                         horse's real place is shifted by its modeled block effect, then re-ranked.\n\
                         {}\n\
                         A model estimate, not this exact race's outcome.",
                        delta_line
                    );
                    if cf == 0 {
                        ui.label(RichText::new("—").weak())
                            .on_hover_text("Counterfactual place — simulating…");
                    } else if cf < h.rank {
                        // без блока приехал бы ВЫШЕ
                        let col = if cf == 1 { red } else { orange };
                        ui.label(RichText::new(format!("P{}→P{}", h.rank, cf)).color(col).strong())
                            .on_hover_text(format!("{}\nWould have placed higher without the block.", tip));
                    } else if cf > h.rank {
                        // без блока приехал бы НИЖЕ — выиграл от зажатых соперников
                        ui.label(RichText::new(format!("P{}→P{}", h.rank, cf)).color(green))
                            .on_hover_text(format!("{}\nBenefited from rivals being boxed in.", tip));
                    } else {
                        ui.label(RichText::new(format!("P{}", h.rank)).weak())
                            .on_hover_text(format!("{}\nSame place without the block.", tip));
                    }
                }
                ui.end_row();
            }
        });
}

fn horse_label(h: &Horse) -> String {
    if h.trainer.is_empty() {
        format!("[{}] {}", h.gate, h.name)
    } else {
        format!("[{}] {} ({})", h.gate, h.name, h.trainer)
    }
}

fn main() {
    egui_overlay::start(App::new());
}

#[cfg(test)]
mod tests {
    use super::*;

    fn horse(stats: [i32; 5], style: i32, apt: [i32; 5], max_hp: f32) -> Horse {
        Horse {
            stats: stats.to_vec(),
            style,
            apt: apt.to_vec(),
            max_hp,
            win_chance: -1.0,
            ..Default::default()
        }
    }

    #[test]
    fn distance_deduced_from_hp() {
        // sashi (коэф 1.0), стамина 600: maxHP = 2400 + 0.8*600 = 2880
        let horses = vec![horse([1000, 600, 800, 400, 500], 3, [5, 6, 7, 7, 7], 2880.0)];
        assert_eq!(deduce_course_distance(&horses), Some(2400.0));
    }

    #[test]
    fn equal_horses_get_equal_chances() {
        let mut horses: Vec<Horse> = (0..9)
            .map(|_| horse([1000, 700, 900, 400, 600], 2, [5, 6, 7, 5, 7], 2700.0))
            .collect();
        compute_win_chances(&mut horses);
        let total: f32 = horses.iter().map(|h| h.win_chance).sum();
        assert!((total - 1.0).abs() < 1e-4, "сумма = {total}");
        for h in &horses {
            assert!((h.win_chance - 1.0 / 9.0).abs() < 1e-4);
        }
    }

    #[test]
    fn stronger_horse_is_favored() {
        let mut horses = vec![
            horse([1200, 900, 1100, 600, 800], 3, [5, 6, 8, 7, 8], 2900.0),
            horse([900, 700, 800, 400, 500], 3, [5, 6, 7, 7, 7], 2740.0),
            horse([900, 700, 800, 400, 500], 3, [5, 6, 7, 7, 7], 2740.0),
        ];
        compute_win_chances(&mut horses);
        assert!(horses[0].win_chance > horses[1].win_chance * 2.0);
        let total: f32 = horses.iter().map(|h| h.win_chance).sum();
        assert!((total - 1.0).abs() < 1e-4);
    }

    #[test]
    fn bad_distance_aptitude_hurts() {
        // одинаковые статы, но у второй аптитуд длинной дистанции G
        let mut horses = vec![
            horse([1000, 800, 900, 500, 600], 3, [3, 4, 6, 7, 7], 3440.0), // long A
            horse([1000, 800, 900, 500, 600], 3, [3, 4, 6, 1, 7], 3440.0), // long G
        ];
        compute_win_chances(&mut horses); // maxHP → дистанция 2800 (long)
        assert!(horses[0].win_chance > horses[1].win_chance * 3.0);
    }

    #[test]
    fn missing_stats_marked_unknown() {
        let mut horses = vec![
            horse([1000, 700, 900, 400, 600], 2, [5, 6, 7, 5, 7], 2700.0),
            horse([-1, -1, -1, -1, -1], -1, [-1; 5], 0.0),
            horse([1000, 700, 900, 400, 600], 2, [5, 6, 7, 5, 7], 2700.0),
        ];
        compute_win_chances(&mut horses);
        assert!(horses[1].win_chance < 0.0);
        let total: f32 = horses.iter().filter(|h| h.win_chance >= 0.0).map(|h| h.win_chance).sum();
        assert!((total - 1.0).abs() < 1e-4);
    }
}
