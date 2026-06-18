/**
 * Cloudflare Worker — релей фидбэка Uma Race Overlay → Telegram.
 *
 * ЗАЧЕМ: desktop-app шлёт баг/идею НЕ напрямую в Telegram, а на этот Worker.
 * Секреты (токен бота, chat_id разработчика) лежат ТОЛЬКО здесь, в env-секретах
 * Cloudflare, и в клиент не попадают → через exe на разработчика выйти нельзя.
 * Клиент присылает анонимный install_id (случайный, без личных данных).
 *
 * ЧТО ДЕЛАЕТ:
 *   1) Rate-limit: не более 25 сообщений в сутки (UTC-день) на install_id.
 *      При превышении — 429 (клиент показывает предупреждение), сообщение НЕ
 *      пересылается. Повторные превышения копятся → авто-бан.
 *   2) Чёрный список install_id и (хэша) IP: их сообщения игнорируются полностью
 *      (403), независимо от содержимого. Бан можно поставить и вручную (см. README).
 *   3) Вторичный лимит по IP — анти-обход через ротацию install_id.
 *   4) Пересборка сообщения и пересылка в Telegram (sendMessage + sendDocument
 *      для приложенной гонки).
 *
 * ПРОИЗВОДИТЕЛЬНОСТЬ: хранилище — Cloudflare KV (binding STORE). Ключи живут с TTL,
 * чтения/записи дёшевы; тянет тысячи пользователей (каждый до 25 запросов/сутки). KV
 * eventually-consistent: под пиковым бурстом одного юзера может проскочить пара
 * лишних — для анти-спама это приемлемо (для строгой атомарности — Durable Objects).
 *
 * ПРИВАТНОСТЬ ДАННЫХ: PII не собираем. install_id анонимен. IP НЕ храним в сыром
 * виде — только солёный SHA-256-хэш и только для счётчиков/бана; в Telegram IP не
 * пересылается. Все ключи, кроме бана, протухают по TTL.
 */

// ── Настройки защиты ──────────────────────────────────────────────────────
const RL_MAX_PER_DAY = 25; // не более 25 сообщений в сутки на install_id (UTC-день)
const RL_KEY_TTL_S = 26 * 60 * 60; // TTL дневного счётчика (с запасом после полуночи)
const VIO_THRESHOLD = 5; // столько превышений лимита → авто-бан install_id
const VIO_TTL_S = 60 * 60; // окно подсчёта превышений (1 час)
const BAN_TTL_S = 7 * 24 * 60 * 60; // авто-бан на 7 дней (null = навсегда)
const IP_MAX_PER_WINDOW = 30; // вторичный лимит: запросов с одного IP за окно
const IP_WINDOW_S = 60 * 60; // окно IP-лимита (1 час)
const MAX_BODY_BYTES = 512 * 1024; // максимум размера запроса (с вложением гонки)
const MAX_TEXT = 3500; // обрезка текста пользователя
const TG_MSG_LIMIT = 4096; // лимит Telegram на длину сообщения

export default {
  async fetch(request, env, ctx) {
    // health-check
    if (request.method === "GET") return json({ status: "ok" }, 200);
    if (request.method !== "POST")
      return json({ status: "error", message: "method_not_allowed" }, 405);

    // размер запроса
    const len = Number(request.headers.get("content-length") || "0");
    if (len > MAX_BODY_BYTES)
      return json({ status: "error", message: "too_large" }, 413);

    // app-key — лёгкий фильтр от случайных сканеров URL (НЕ настоящая
    // аутентификация: ключ извлекаем из бинаря; основная защита — лимиты ниже).
    if (env.APP_KEY && request.headers.get("x-app-key") !== env.APP_KEY)
      return json({ status: "error", message: "unauthorized" }, 401);

    let form;
    try {
      form = await request.formData();
    } catch {
      return json({ status: "error", message: "bad_form" }, 400);
    }

    const installId = sanitizeId(form.get("install_id"));
    if (!installId) return json({ status: "error", message: "no_id" }, 400);

    const kv = env.STORE;
    const ip = request.headers.get("cf-connecting-ip") || "0.0.0.0";
    const ipHash = await sha256(ip + "|" + (env.IP_SALT || "salt"));

    // 1) Чёрный список (install_id ИЛИ хэш IP) → молча игнорируем.
    const [banned, ipBanned] = await Promise.all([
      kv.get("bl:" + installId),
      kv.get("blip:" + ipHash),
    ]);
    if (banned || ipBanned) return json({ status: "blocked" }, 403);

    // 2) Вторичный лимит по IP (анти-ротация install_id).
    const ipKey = "ipc:" + ipHash;
    const ipCount = parseInt((await kv.get(ipKey)) || "0", 10);
    if (ipCount >= IP_MAX_PER_WINDOW) {
      await kv.put("blip:" + ipHash, "1", { expirationTtl: BAN_TTL_S });
      return json({ status: "blocked" }, 403);
    }

    // 3) Основной rate-limit по install_id: не более 25 / сутки (UTC-день).
    const day = new Date().toISOString().slice(0, 10); // YYYY-MM-DD (UTC)
    const rlKey = "rl:" + installId + ":" + day;
    const sentToday = parseInt((await kv.get(rlKey)) || "0", 10);
    if (sentToday >= RL_MAX_PER_DAY) {
      // Превышение дневного лимита: считаем нарушения, при пороге → бан.
      const vioKey = "vio:" + installId;
      const vio = parseInt((await kv.get(vioKey)) || "0", 10) + 1;
      if (vio >= VIO_THRESHOLD) {
        await kv.put("bl:" + installId, "1", { expirationTtl: BAN_TTL_S });
      } else {
        await kv.put(vioKey, String(vio), { expirationTtl: VIO_TTL_S });
      }
      // retry_after = секунд до ближайшей полуночи UTC (когда счётчик сбросится).
      const now = new Date();
      const nextMidnight = Date.UTC(
        now.getUTCFullYear(),
        now.getUTCMonth(),
        now.getUTCDate() + 1
      );
      const retry = Math.ceil((nextMidnight - now.getTime()) / 1000);
      return json({ status: "rate_limited", retry_after_s: retry }, 429);
    }

    // Инкрементим дневной счётчик install_id и IP-счётчик (не блокируя ответ).
    ctx.waitUntil(
      Promise.all([
        kv.put(rlKey, String(sentToday + 1), { expirationTtl: RL_KEY_TTL_S }),
        kv.put(ipKey, String(ipCount + 1), { expirationTtl: IP_WINDOW_S }),
      ])
    );

    // 4) Пересборка и пересылка в Telegram.
    try {
      await forwardToTelegram(env, form, installId);
    } catch (e) {
      return json({ status: "error", message: "forward_failed" }, 502);
    }
    return json({ status: "ok" }, 200);
  },
};

// ── Хелперы ────────────────────────────────────────────────────────────────

function json(obj, status) {
  return new Response(JSON.stringify(obj), {
    status,
    headers: { "content-type": "application/json" },
  });
}

/** Допускаем только «безопасный» install_id (hex/base-ish, 8..64 симв.). */
function sanitizeId(v) {
  const s = String(v || "").replace(/[^a-zA-Z0-9_-]/g, "");
  return s.length >= 8 && s.length <= 64 ? s : null;
}

async function sha256(s) {
  const buf = await crypto.subtle.digest("SHA-256", new TextEncoder().encode(s));
  return [...new Uint8Array(buf)].map((b) => b.toString(16).padStart(2, "0")).join("");
}

function clip(v, n) {
  return String(v || "").slice(0, n);
}

/** Собирает текст сообщения (формат задаётся ЗДЕСЬ — менять без пересборки app). */
function composeMessage(form, installId) {
  const isIdea = form.get("kind") === "idea";
  let s = isIdea ? "💡 SUGGESTION\n" : "🐞 BUG REPORT\n";
  if (!isIdea) {
    const areas = clip(form.get("areas"), 300);
    if (areas) s += "Areas: " + areas + "\n";
    const toggles = clip(form.get("toggles"), 100);
    if (toggles) s += "Toggles: " + toggles + "\n";
  }
  s += `App: v${clip(form.get("app_version"), 16)} · ${clip(form.get("os"), 16)}\n`;
  const contact = clip(form.get("contact"), 128);
  if (contact) s += "Contact: " + contact + "\n";
  // Полный install_id (анонимный) — чтобы можно было вручную забанить автора
  // командой `wrangler kv key put bl:<id> 1` (см. README).
  s += "ID: " + installId + "\n";
  s += "\n" + clip(form.get("text"), MAX_TEXT);
  return s.slice(0, TG_MSG_LIMIT);
}

async function forwardToTelegram(env, form, installId) {
  const base = `https://api.telegram.org/bot${env.BOT_TOKEN}`;
  const text = composeMessage(form, installId);

  const r = await fetch(`${base}/sendMessage`, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({
      chat_id: env.CHAT_ID,
      text,
      disable_web_page_preview: true,
    }),
  });
  if (!r.ok) throw new Error("sendMessage " + r.status);

  // Приложенная гонка (если есть) — отдельным документом.
  const race = form.get("race");
  if (race && typeof race === "object" && race.size > 0) {
    const fd = new FormData();
    fd.set("chat_id", String(env.CHAT_ID));
    fd.set("caption", "race data · ID " + installId);
    fd.set("document", race, "race.json");
    const r2 = await fetch(`${base}/sendDocument`, { method: "POST", body: fd });
    if (!r2.ok) throw new Error("sendDocument " + r2.status);
  }
}
