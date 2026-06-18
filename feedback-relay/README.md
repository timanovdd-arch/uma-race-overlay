# Uma Feedback Relay (Cloudflare Worker)

Тонкий релей между оверлеем и Telegram. Держит секреты у себя (токен бота + твой
chat_id), enforce-ит rate-limit и чёрный список. Клиент знает только URL — на тебя
через exe выйти нельзя.

## Что защищает
- **До 25 сообщений в сутки** (UTC-день) на анонимный `install_id`. Превышение → `429`
  (клиент показывает «подожди»), сообщение не пересылается. Счётчик сбрасывается в полночь UTC.
- **Авто-бан**: 5 превышений подряд → бан install_id на 7 дней.
- **Чёрный список** install_id и (хэша) IP → их сообщения игнорируются (`403`).
- **Вторичный лимит по IP** (30/час) — против обхода ротацией install_id.
- PII не собираем; IP хранится только солёным SHA-256-хэшем и с TTL.

## Разовая настройка

Нужен бесплатный аккаунт Cloudflare и Node.js.

```powershell
npm install -g wrangler          # CLI Cloudflare
cd feedback-relay
wrangler login                   # откроет браузер, авторизуйся

# 1) Создать KV-хранилище и вставить выданный id в wrangler.toml (поле id):
wrangler kv namespace create STORE
#   → скопируй id из вывода в kv_namespaces.id в wrangler.toml

# 2) Секреты (вводятся интерактивно, в репозиторий НЕ попадают).
#    Готовые значения APP_KEY и IP_SALT — в gitignored `secrets.local.txt`
#    рядом с этим README (НЕ коммить их в публичный репо):
wrangler secret put BOT_TOKEN    # НОВЫЙ токен бота (см. «Ротация» ниже)
wrangler secret put CHAT_ID      # твой chat_id (из secrets.local.txt)
wrangler secret put APP_KEY      # значение APP_KEY из secrets.local.txt
wrangler secret put IP_SALT      # значение IP_SALT из secrets.local.txt

# 3) Деплой:
wrangler deploy
#   → выдаст URL вида https://uma-feedback-relay.<твой-аккаунт>.workers.dev
```

Пришли мне этот **URL** — впишу в клиент (`feedback_creds.txt` → `relay_url=…`,
`relay_key=` уже = APP_KEY) и пересоберу exe.

Проверка: открой URL в браузере — должно вернуть `{"status":"ok"}`.

## Ротация утёкшего токена (важно)
Текущий токен уже вшит в роздан­ный v0.2.0 → его надо **отозвать**, иначе твой
chat_id из старого бинаря останется досягаем:
1. В @BotFather: `/revoke` → выбрать бота → получить **новый** токен.
2. Новый токен поставить как секрет `BOT_TOKEN` (шаг 2 выше).
3. Старые бинари v0.2.0 (с отозванным токеном) больше не смогут писать напрямую.

## Ручной бан / разбан
Полный `install_id` виден в каждом сообщении строкой `ID: …` (он анонимный).
```powershell
# забанить (навсегда — без TTL; или добавь --ttl 604800 для 7 дней):
wrangler kv key put --binding=STORE "bl:<install_id>" "1" --remote
# разбанить:
wrangler kv key delete --binding=STORE "bl:<install_id>" --remote
```

## Настройки лимитов
Все пороги — константы вверху `worker.js` (`RL_MAX_PER_DAY`, `VIO_THRESHOLD`,
`BAN_TTL_S`, `IP_MAX_PER_WINDOW` …). Поменял → `wrangler deploy`.

## Заметка про строгость
KV eventually-consistent: при резком бурсте одного юзера может проскочить пара
лишних сообщений до синхронизации. Для анти-спама это ок. Нужна жёсткая атомарность
— перевести счётчики на Durable Objects (отдельная доработка).
