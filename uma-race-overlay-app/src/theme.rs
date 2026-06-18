//! Тёмно-розовая тема в стиле Umamusume + сайта (розовый/жёлтый/салатовый акценты
//! по тёмному фону) с лёгким узором и символами аптитудов ◎ ○ △ ✕.
//!
//! Тёмный фон («потемнее») держит данные ЯРКИМИ и читаемыми (как в игре), а UI —
//! розовая рамка/кнопки + диагональный узор на фоне.
//!
//! ВАЖНО: окно-реплей рисует трек/поле СВОИМ тёмным фоном (см. replay::paint_chart),
//! поэтому тема его не затрагивает — «поле гонки» остаётся как было.

// Палитра-справочник: часть именованных цветов держим «про запас».
#![allow(dead_code)]

use egui::{pos2, vec2, Align2, Color32, FontId, Frame, Margin, Pos2, Rect, Rounding, Sense, Stroke};

// --- Палитра акцентов (UI) ---------------------------------------------------
pub const TEXT: Color32 = Color32::from_rgb(240, 228, 236); // основной текст (тёплый кремовый)
pub const PINK: Color32 = Color32::from_rgb(255, 92, 154);
pub const PINK_SOFT: Color32 = Color32::from_rgb(255, 160, 195);
pub const YELLOW: Color32 = Color32::from_rgb(255, 200, 61);
pub const GREEN: Color32 = Color32::from_rgb(120, 220, 130);
pub const HINT: Color32 = Color32::from_rgb(200, 160, 180);

// Фоновые тона (тёмная роза/слива).
const WIN: Color32 = Color32::from_rgb(46, 31, 42); // фон окна
const PANEL: Color32 = Color32::from_rgb(38, 26, 35); // фон панели
const SUNK: Color32 = Color32::from_rgb(28, 19, 26); // утопленный (textedit/трек)
const STRIPE: Color32 = Color32::from_rgb(58, 40, 52); // чередование строк
const BORDER: Color32 = Color32::from_rgb(120, 70, 95); // мягкая рамка
/// Тёмный фон вложенных панелей (как у графика) — для боксов статов/легенды.
pub const PANEL_DARK: Color32 = Color32::from_rgb(26, 28, 34);

// --- Семантические цвета ДАННЫХ (яркие, на тёмном фоне) ----------------------
/// Свои лошади.
pub const C_MINE: Color32 = Color32::from_rgb(120, 200, 255);
/// Позитив / «чисто» / финиш / успех.
pub const C_GOOD: Color32 = Color32::from_rgb(120, 230, 140);
/// Предупреждение / «в разработке» / ожидание.
pub const C_WARN: Color32 = Color32::from_rgb(255, 200, 80);
/// Спурт / блок-оранжевый.
pub const C_SPURT: Color32 = Color32::from_rgb(255, 170, 60);
/// Середина шкалы win%.
pub const C_FADE: Color32 = Color32::from_rgb(220, 220, 160);
/// Плохо / красный.
pub const C_BAD: Color32 = Color32::from_rgb(255, 110, 110);
/// Дуэль (追い比べ) — фиолетовый.
pub const C_DUEL: Color32 = Color32::from_rgb(200, 130, 255);

/// Заливка прогресс-бара HP (ярко, читается на тёмном).
pub fn hp_fill(pct: f32) -> Color32 {
    if pct > 0.5 {
        Color32::from_rgb(76, 217, 100)
    } else if pct > 0.25 {
        Color32::from_rgb(242, 196, 15)
    } else {
        Color32::from_rgb(230, 57, 53)
    }
}

/// Устанавливает тёмно-розовую тему. Зовётся один раз при старте.
pub fn install(ctx: &egui::Context) {
    let mut style = (*ctx.style()).clone();
    let mut v = egui::Visuals::dark();

    v.override_text_color = Some(TEXT);
    v.hyperlink_color = Color32::from_rgb(255, 140, 185);
    v.warn_fg_color = C_WARN;
    v.error_fg_color = C_BAD;

    v.window_fill = WIN;
    v.panel_fill = PANEL;
    v.extreme_bg_color = SUNK;
    v.faint_bg_color = STRIPE;
    v.code_bg_color = SUNK;

    v.window_stroke = Stroke::new(1.5, BORDER);
    v.window_rounding = Rounding::same(14.0);
    v.menu_rounding = Rounding::same(12.0);

    v.selection.bg_fill = Color32::from_rgba_unmultiplied(255, 92, 154, 80);
    v.selection.stroke = Stroke::new(1.0, PINK_SOFT);

    let r = Rounding::same(9.0);
    // Не интерактивные (label, разделители).
    v.widgets.noninteractive.bg_fill = WIN;
    v.widgets.noninteractive.weak_bg_fill = WIN;
    v.widgets.noninteractive.bg_stroke = Stroke::new(1.0, Color32::from_rgb(70, 50, 64));
    v.widgets.noninteractive.fg_stroke = Stroke::new(1.0, TEXT);
    v.widgets.noninteractive.rounding = r;
    // Покой (кнопка по умолчанию).
    v.widgets.inactive.bg_fill = Color32::from_rgb(66, 46, 60);
    v.widgets.inactive.weak_bg_fill = Color32::from_rgb(66, 46, 60);
    v.widgets.inactive.bg_stroke = Stroke::new(1.0, Color32::from_rgb(150, 86, 116));
    v.widgets.inactive.fg_stroke = Stroke::new(1.0, Color32::from_rgb(255, 205, 224));
    v.widgets.inactive.rounding = r;
    // Наведение.
    v.widgets.hovered.bg_fill = Color32::from_rgb(96, 60, 82);
    v.widgets.hovered.weak_bg_fill = Color32::from_rgb(96, 60, 82);
    v.widgets.hovered.bg_stroke = Stroke::new(1.5, PINK);
    v.widgets.hovered.fg_stroke = Stroke::new(1.5, Color32::from_rgb(255, 228, 240));
    v.widgets.hovered.rounding = r;
    // Нажатие/актив.
    v.widgets.active.bg_fill = PINK;
    v.widgets.active.weak_bg_fill = PINK;
    v.widgets.active.bg_stroke = Stroke::new(1.5, Color32::from_rgb(255, 150, 190));
    v.widgets.active.fg_stroke = Stroke::new(1.5, Color32::WHITE);
    v.widgets.active.rounding = r;
    // Раскрытые.
    v.widgets.open.bg_fill = Color32::from_rgb(66, 46, 60);
    v.widgets.open.weak_bg_fill = Color32::from_rgb(66, 46, 60);
    v.widgets.open.bg_stroke = Stroke::new(1.0, PINK);
    v.widgets.open.fg_stroke = Stroke::new(1.0, Color32::from_rgb(255, 205, 224));
    v.widgets.open.rounding = r;

    style.visuals = v;
    style.spacing.button_padding = vec2(8.0, 4.0);
    style.spacing.item_spacing = vec2(8.0, 6.0);
    ctx.set_style(style);
}

/// Узор на фоне окна — в ДВА шага, чтобы он не вылезал за пределы окна:
///   1) `bg_reserve(ui)` ПЕРВОЙ строкой в теле окна — резервирует слот под фоном
///      (рисуется раньше виджетов, значит окажется ПОД ними);
///   2) `bg_fill(ui, idx)` ПОСЛЕДНЕЙ строкой (и перед каждым `return`) — когда уже
///      известен фактический размер содержимого (`min_rect`), заполняет слот
///      диагональной штриховкой, обрезанной строго по этому прямоугольнику.
/// Раньше узор рисовался по `clip_rect`, который у авто-размерного окна = весь
/// экран, поэтому полоски расползались на весь оверлей.
pub fn bg_reserve(ui: &egui::Ui) -> egui::layers::ShapeIdx {
    ui.painter().add(egui::Shape::Noop)
}

pub fn bg_fill(ui: &egui::Ui, idx: egui::layers::ShapeIdx) {
    let rect = ui.min_rect().expand(8.0);
    let step = 22.0;
    let stroke = Stroke::new(1.0, Color32::from_rgba_unmultiplied(255, 120, 170, 16));
    let mut shapes: Vec<egui::Shape> = Vec::new();
    let mut x = rect.left() - rect.height();
    while x < rect.right() {
        if let Some(seg) = clip_diag(rect, x) {
            shapes.push(egui::Shape::line_segment(seg, stroke));
        }
        x += step;
    }
    ui.painter().set(idx, egui::Shape::Vec(shapes));
}

/// Отрезок диагонали под 45° (вниз-вправо) от вертикали `x0`, обрезанный строго
/// по `rect`. None — диагональ прямоугольник не пересекает.
fn clip_diag(rect: Rect, x0: f32) -> Option<[Pos2; 2]> {
    // Точки прямой: (x0 + t, top + t), t = смещение по Y. Нужно x ∈ [left,right]
    // и t ∈ [0, height].
    let t0 = (rect.left() - x0).max(0.0);
    let t1 = (rect.right() - x0).min(rect.height());
    if t1 <= t0 {
        return None;
    }
    Some([pos2(x0 + t0, rect.top() + t0), pos2(x0 + t1, rect.top() + t1)])
}

/// Тёмная вложенная панель (как фон графика) с розовой рамкой — для статов/легенды,
/// чтобы параметры читались и были «обведены».
pub fn dark_panel() -> Frame {
    Frame::none()
        .fill(PANEL_DARK)
        .stroke(Stroke::new(1.0, Color32::from_rgba_unmultiplied(255, 92, 154, 110)))
        .rounding(Rounding::same(8.0))
        .inner_margin(Margin::same(8.0))
}

/// Ряд символов аптитудов ◎ ○ △ ✕ — РОВНО (равные интервалы, единая базовая
/// линия): рисуем painter'ом по сетке, а не лейблами (разная ширина глифов).
pub fn uma_marks(ui: &mut egui::Ui) {
    let glyphs: [(&str, Color32); 6] = [
        ("◎", PINK),
        ("○", YELLOW),
        ("△", GREEN),
        ("✕", Color32::from_rgb(255, 150, 190)),
        ("◎", GREEN),
        ("○", PINK),
    ];
    let n = glyphs.len() as f32;
    let (rect, _) = ui.allocate_exact_size(vec2(196.0, 22.0), Sense::hover());
    let painter = ui.painter();
    let step = rect.width() / n;
    let y = rect.center().y;
    for (i, (sym, col)) in glyphs.iter().enumerate() {
        let cx = rect.left() + step * (i as f32 + 0.5);
        painter.text(pos2(cx, y), Align2::CENTER_CENTER, *sym, FontId::proportional(15.0), *col);
    }
}
