use std::path::Path;

// Встраивает иконку приложения в .exe при сборке под Windows + пробрасывает
// учётку Telegram-бота для кнопки фидбэка (СЕКРЕТ НЕ В git, см. ниже).
fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("icon.ico");
        // Не валим сборку, если rc-тулчейн недоступен — иконку окна (= панель
        // задач) всё равно ставим в рантайме (main.rs set_window_icon).
        if let Err(e) = res.compile() {
            println!("cargo:warning=exe icon embed skipped: {e}");
        }
    }
    embed_relay_config();
    println!("cargo:rerun-if-changed=icon.ico");
    println!("cargo:rerun-if-changed=build.rs");
}

/// Конфиг релея фидбэка (URL Worker + app-key) вшивается в exe через
/// `cargo:rustc-env`, чтобы `env!("UMA_RELAY_*")` в feedback.rs всегда был
/// определён (build.rs канонично объявляет зависимость и перевыставляет значение —
/// надёжнее флаки `option_env!`). СЕКРЕТА БОТА В КЛИЕНТЕ НЕТ — токен/chat_id живут
/// только в Worker; здесь лишь публичный URL и полусекретный app-key.
///
/// Источник (приоритет): env `UMA_RELAY_URL`/`UMA_RELAY_KEY`, иначе gitignored
/// `feedback_creds.txt` рядом с Cargo.toml в формате:
///   relay_url=https://uma-feedback-relay.<account>.workers.dev
///   relay_key=<app-key>
/// Файл В git НЕ коммитится (.gitignore). Пусто → кнопка покажет «not configured».
fn embed_relay_config() {
    println!("cargo:rerun-if-env-changed=UMA_RELAY_URL");
    println!("cargo:rerun-if-env-changed=UMA_RELAY_KEY");
    println!("cargo:rerun-if-changed=feedback_creds.txt");

    let (mut url, mut key) = (String::new(), String::new());
    if let Ok(text) = std::fs::read_to_string(Path::new("feedback_creds.txt")) {
        for line in text.lines() {
            if let Some((k, v)) = line.trim().split_once('=') {
                match k.trim().to_ascii_lowercase().as_str() {
                    "url" | "relay_url" => url = v.trim().to_string(),
                    "key" | "app_key" | "relay_key" => key = v.trim().to_string(),
                    _ => {}
                }
            }
        }
    }
    // Env-переменные имеют приоритет над файлом (удобно для CI/ручной сборки).
    if let Ok(v) = std::env::var("UMA_RELAY_URL") {
        if !v.is_empty() {
            url = v;
        }
    }
    if let Ok(v) = std::env::var("UMA_RELAY_KEY") {
        if !v.is_empty() {
            key = v;
        }
    }
    println!("cargo:rustc-env=UMA_RELAY_URL={url}");
    println!("cargo:rustc-env=UMA_RELAY_KEY={key}");
}
