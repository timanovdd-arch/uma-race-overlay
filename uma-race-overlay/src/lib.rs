//! Игровая часть оверлея HP/скорости лошадей для Umamusume (Steam).
//!
//! Загружается Hachimi через `load_libraries` в config.json.
//! Делает ТОЛЬКО лёгкую работу внутри процесса игры: ставит il2cpp/minhook
//! хуки на классы гонки и пишет снимок состояния в JSON-файл во временной папке.
//! Сам оверлей рисует отдельная программа (uma_race_overlay_app.exe) в своём
//! процессе — так рендер не может уронить игру и не конфликтует с GUI Hachimi.

mod archive;
mod frames;
mod hooks;
mod il2cpp;
mod logger;
mod state;
mod writer;

use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, Ordering};

static STARTED: AtomicBool = AtomicBool::new(false);

fn start() {
    if STARTED.swap(true, Ordering::SeqCst) {
        return;
    }
    logger::log("uma_race_overlay attached");
    std::thread::spawn(hooks::init_thread);
    std::thread::spawn(writer::writer_thread);
}

#[no_mangle]
pub extern "system" fn DllMain(_hinst: *mut c_void, reason: u32, _reserved: *mut c_void) -> i32 {
    const DLL_PROCESS_ATTACH: u32 = 1;
    if reason == DLL_PROCESS_ATTACH {
        std::thread::spawn(start);
    }
    1
}
