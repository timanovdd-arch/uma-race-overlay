//! Отправка фидбэка (баг / идея) через Cloudflare Worker релей → Telegram.
//!
//! Клиент НЕ содержит токена бота и chat_id разработчика — они живут ТОЛЬКО в
//! Worker (env-секреты Cloudflare). Поэтому из exe выйти на разработчика нельзя.
//! Шлём анонимный `install_id` (случайный, без личных данных). Rate-limit и
//! чёрный список enforce-ятся на стороне Worker; клиент лишь показывает результат.
//!
//! HTTP-клиент не тащим (лишний TLS-стек к капризной glfw/CMake-сборке) — шеллим
//! встроенный в Windows `curl.exe` (multipart-POST) в фоновом потоке; статус
//! кладём в общий `Arc<Mutex<…>>`, UI опрашивает каждый кадр.

use std::sync::{Arc, Mutex};

/// Конфиг релея: URL Worker + опциональный app-key (лёгкий фильтр от сканеров).
#[derive(Clone, Default)]
pub struct RelayConfig {
    pub url: String,
    pub app_key: String,
}

impl RelayConfig {
    pub fn configured(&self) -> bool {
        self.url.starts_with("http")
    }
}

/// URL/ключ подставляет `build.rs` через `cargo:rustc-env` из gitignored-файла
/// `feedback_creds.txt` (или env `UMA_RELAY_URL`/`UMA_RELAY_KEY`). В публичном
/// исходнике — только эти `env!`, не сами значения. URL не секрет (это эндпоинт),
/// app_key — полусекрет (всё равно извлекаем, но не светим в публичном репо).
const EMBEDDED_URL: &str = env!("UMA_RELAY_URL");
const EMBEDDED_KEY: &str = env!("UMA_RELAY_KEY");

/// Конфиг: внешний `feedback.txt` рядом с exe (override) поверх вшитого.
/// Формат файла:
///   relay_url=https://uma-feedback-relay.<account>.workers.dev
///   relay_key=<app-key>
pub fn load_config(read_asset: impl Fn(&str) -> Option<String>) -> RelayConfig {
    let mut cfg = RelayConfig {
        url: EMBEDDED_URL.to_string(),
        app_key: EMBEDDED_KEY.to_string(),
    };
    if let Some(text) = read_asset("feedback.txt") {
        for line in text.lines() {
            let Some((k, v)) = line.trim().split_once('=') else {
                continue;
            };
            let v = v.trim().to_string();
            match k.trim().to_ascii_lowercase().as_str() {
                "url" | "relay_url" => cfg.url = v,
                "key" | "app_key" | "relay_key" => cfg.app_key = v,
                _ => {}
            }
        }
    }
    cfg
}

/// Статус последней отправки (для UI).
#[derive(Clone, PartialEq)]
pub enum SendStatus {
    Idle,
    Sending,
    Sent,
    /// Сервер ограничил частоту: сколько ПРИМЕРНО минут подождать.
    RateLimited(u64),
    /// Сервер отклонил (чёрный список) — клиенту показываем нейтрально.
    Blocked,
    Failed(String),
}

pub type Status = Arc<Mutex<SendStatus>>;

/// Поля одного репорта. Формат сообщения собирает Worker (меняется без пересборки
/// app), клиент шлёт структурированные поля.
pub struct Report {
    pub install_id: String,
    pub kind_bug: bool,
    pub text: String,
    pub contact: String,
    /// Выбранные области («где произошло») — через запятую.
    pub areas: String,
    /// Активные тоглы (контекст).
    pub toggles: String,
    pub app_version: String,
    pub os: String,
    /// Содержимое JSON последней гонки (вложение), если приложили.
    pub race: Option<Vec<u8>>,
}

/// Запускает отправку в фоне.
pub fn spawn_send(cfg: RelayConfig, rep: Report, status: Status) {
    if let Ok(mut s) = status.lock() {
        *s = SendStatus::Sending;
    }
    std::thread::spawn(move || {
        let res = send(&cfg, &rep);
        if let Ok(mut s) = status.lock() {
            *s = res;
        }
    });
}

/// Прячем консольное окно curl (без мелькания чёрного квадрата при отправке).
#[cfg(windows)]
fn hide_console(cmd: &mut std::process::Command) {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    cmd.creation_flags(CREATE_NO_WINDOW);
}
#[cfg(not(windows))]
fn hide_console(_cmd: &mut std::process::Command) {}

fn send(cfg: &RelayConfig, rep: &Report) -> SendStatus {
    // Вложение гонки кладём во временный файл (curl -F @file). Снимок сделан в
    // вызывающем потоке (rep.race), поэтому от перезаписи живого state.json защищены.
    let tmp_race = rep.race.as_ref().and_then(|bytes| {
        let p = std::env::temp_dir().join(format!("uma_race_{}.json", rep.install_id));
        std::fs::write(&p, bytes).ok().map(|_| p)
    });
    let tmp_resp = std::env::temp_dir().join(format!("uma_fb_resp_{}.txt", rep.install_id));

    let mut cmd = std::process::Command::new("curl");
    cmd.args([
        "-sS",
        "-m",
        "30",
        "-X",
        "POST",
        &cfg.url,
        "-o",
        &tmp_resp.to_string_lossy(),
        "-w",
        "%{http_code}",
    ]);
    if !cfg.app_key.is_empty() {
        cmd.arg("-H").arg(format!("X-App-Key: {}", cfg.app_key));
    }
    // --form-string НЕ интерпретирует @/< в значениях → безопасно для произвольного
    // пользовательского текста (кириллица/CJK/переносы проходят как есть).
    let field = |cmd: &mut std::process::Command, k: &str, v: &str| {
        cmd.arg("--form-string").arg(format!("{k}={v}"));
    };
    field(&mut cmd, "install_id", &rep.install_id);
    field(&mut cmd, "kind", if rep.kind_bug { "bug" } else { "idea" });
    field(&mut cmd, "text", &rep.text);
    field(&mut cmd, "contact", &rep.contact);
    field(&mut cmd, "areas", &rep.areas);
    field(&mut cmd, "toggles", &rep.toggles);
    field(&mut cmd, "app_version", &rep.app_version);
    field(&mut cmd, "os", &rep.os);
    if let Some(p) = &tmp_race {
        cmd.arg("-F")
            .arg(format!("race=@{};type=application/json", p.display()));
    }
    hide_console(&mut cmd);

    let out = cmd.output();
    if let Some(p) = &tmp_race {
        let _ = std::fs::remove_file(p);
    }

    let result = match out {
        Ok(o) => {
            let code = String::from_utf8_lossy(&o.stdout).trim().to_string();
            let body = std::fs::read_to_string(&tmp_resp).unwrap_or_default();
            map_response(&code, &body, &o)
        }
        Err(e) => SendStatus::Failed(format!("curl not found ({e})")),
    };
    let _ = std::fs::remove_file(&tmp_resp);
    result
}

fn map_response(code: &str, body: &str, out: &std::process::Output) -> SendStatus {
    match code {
        "200" => SendStatus::Sent,
        "429" => {
            let secs = parse_retry(body).unwrap_or(900);
            SendStatus::RateLimited((secs + 59) / 60)
        }
        "403" => SendStatus::Blocked,
        "401" => SendStatus::Failed("not authorized".to_string()),
        "" => {
            // curl не получил HTTP-ответа (сеть/таймаут/нет соединения).
            let err = String::from_utf8_lossy(&out.stderr);
            let err = err.trim();
            SendStatus::Failed(if err.is_empty() {
                "no connection".to_string()
            } else {
                err.to_string()
            })
        }
        c => SendStatus::Failed(format!("server error ({c})")),
    }
}

fn parse_retry(body: &str) -> Option<u64> {
    serde_json::from_str::<serde_json::Value>(body)
        .ok()
        .and_then(|v| v.get("retry_after_s").and_then(|x| x.as_u64()))
}
