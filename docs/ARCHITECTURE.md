# Mosaic — архитектура и инструменты

Технический лист проекта: из чего состоит Mosaic, как идут данные и команды, что работает на каждой
платформе, какие библиотеки и инструменты используются и как расширять проект. Общее описание, история
и план — в [PROJECT.md](PROJECT.md). Актуально на версию **0.8.0**.

---

## 1. Общая схема

```
                 ┌──────────────────────── процесс Mosaic ─────────────────────────┐
 приложения ──▶  │  захват                 hub (Rust, tokio)                       │
                 │  ├ системный прокси ─┐  ┌───────────────────────────────┐       │
                 │  ├ локальный прокси ─┼▶ │ вход SOCKS5 + HTTP :7890      │       │      ┌──────────────┐
                 │  └ TUN-стек Mosaic ──┘  │ роутер правил (Routing)       │──────────────▶│ ядро: Xray   │──▶ сервер
                 │      (ipstack)          │ диспетчер: узел/прямо/блок    │  SOCKS   │   │ ядро: sing-box│──▶ сервер
                 │                         │ статистика, журнал соединений │       │      └──────────────┘
                 │                         └───────────┬───────────────────┘       │       (отдельные процессы,
                 │                                     └──── напрямую ─────────────────▶ сеть   по одному на ядро)
                 │  Controller: состояние, сессия, Smart Connect, обновления       │
                 └──────────────▲──────────────────────────────────────────────────┘
                                │ команды (JSON)
                 UI (HTML/JS): Tauri — Windows, Linux · WebView + JNI — Android
```

Два принципа, на которых держится всё остальное:

1. **Ядра не знают о правилах.** Каждое ядро получает список «слотов» (узел + локальный порт) и
   поднимает на каждый слот SOCKS5-вход `127.0.0.1:<порт>`, ведущий на этот узел. Всё решение «куда»
   принимает hub. Поэтому ядра можно смешивать и заменять, а правила — менять без перезапусков.
2. **Таблица маршрутизации неизменяема и подменяется целиком.** `SharedRouting =
   Arc<RwLock<Arc<Routing>>>`. Соединение берёт `Arc<Routing>` в момент открытия и работает с ним до
   конца. «Применить», смена узла, Smart Connect — это сборка новой таблицы и одна замена указателя.

---

## 2. Процессы на каждой платформе

| Платформа, режим | Процессы | Кто создаёт TUN |
| --- | --- | --- |
| Windows, прокси / системный прокси | mosaic.exe, xray.exe, sing-box.exe (по необходимости) | — |
| Windows, TUN «свой» | то же | mosaic.exe (wintun), нужны права администратора |
| Windows, TUN «адаптер sing-box» | + второй sing-box.exe (адаптер) | sing-box |
| Linux, прокси / системный прокси | mosaic, xray, sing-box | — |
| Linux, TUN «свой» | + mosaic-tun-helper (с cap_net_admin) | помощник; Mosaic подключается без прав |
| Linux, TUN «адаптер sing-box» | + sing-box-tun (с cap_net_admin) | sing-box |
| Android, VPN | процесс приложения (Kotlin + libmosaic.so), libxray.so, libsingbox.so | VpnService (система) |

Ядро запускается, только если есть хотя бы один узел для него (матрица возможностей, §6).

---

## 3. Путь данных

### 3.1. Соединение TCP

1. **Вход.**
   - Прокси-режимы: SOCKS5 `CONNECT` или HTTP `CONNECT`/обычный запрос на `:7890` (`hub.rs`).
   - TUN: `ipstack` собирает TCP-поток из пакетов; адрес назначения — из таблицы fake-IP, иначе
     сниффинг TLS SNI / HTTP Host (порты 80, 443, 8080, 8443), иначе сам IP (`tun_native.rs`, `sniff.rs`).
2. **Владелец** (если хоть одно правило проверяет приложение — `needs_process`):
   Windows — `GetExtendedTcpTable`/`GetExtendedUdpTable` + `QueryFullProcessImageNameW`;
   Linux — `/proc/net/{tcp,udp}[6]` → inode → `/proc/<pid>/fd`; Android — хук в Kotlin
   `ConnectivityManager.getConnectionOwnerUid` → имя пакета (`procinfo.rs`).
3. **Решение** (`Routing::decide_full`): правила сверху вниз, первое совпадение; цели `direct`,
   `block`, `proxy` (выбранный узел), `node:<id>`. Если доменные правила не сработали, есть IP-правила и
   стратегия «резолвить, если не совпало» — имя резолвится (через узел или локально, по настройке) и
   решение принимается повторно. Иначе — «всё остальное» (`final_target`).
4. **Подключение** (`hub::connect_target`): узел — SOCKS5 к входу ядра; напрямую — свой сокет
   (`net.rs`), в TUN-режиме привязанный к физическому интерфейсу; блок — отказ.
5. **Передача** (`hub::relay`): двунаправленное копирование, счётчики байт в `stats.rs`, запись в журнал
   соединений (правило, узел, ядро, приложение, время, трафик).

### 3.2. UDP

Прокси-режимы: SOCKS5 `UDP ASSOCIATE`, на каждый поток свой UDP-relay (`udp.rs`). TUN: поток из
`ipstack` → `udp::relay_flow`. Решение — по тем же правилам, что и TCP. QUIC (UDP/443) по умолчанию
блокируется, чтобы браузеры перешли на TCP и подчинялись сниффингу и правилам.

### 3.3. DNS

| Где | Как |
| --- | --- |
| Прокси-режимы | Имена резолвит удалённая сторона (узел) или локальный резолвер для прямых соединений |
| TUN (свой стек) | Ответы даёт сам стек (`serve_dns`): A → fake-IP из 198.18.0.0/15 (по нему hub узнаёт домен); AAAA/HTTPS/SVCB → пусто; остальные типы → «удалённый DNS» (DNS-over-TCP через узел, по умолчанию 1.1.1.1); имена серверов узлов → настоящие адреса |
| TUN (адаптер sing-box) | То же внутри sing-box: `hijack-dns`, fakeip, удалённый DNS через hub, имена серверов → прямой DNS |
| IP-правила | Имя резолвится через узел (по умолчанию) или локально; кеш в `resolver::DnsCache` |
| Собственные запросы hub | `Resolver::Udp(direct_dns)` (по умолчанию 8.8.8.8), сокет привязан к физическому интерфейсу |

Как системный DNS попадает в туннель:

* **Windows:** DNS интерфейса Mosaic = 172.19.0.2, метрика 1, маршрут по умолчанию через TUN, сброс кеша
  DNS. Защита от утечек (WFP): порт 53 разрешён только через TUN, к 172.19.0.2 и для Mosaic и его ядер.
  После подключения — проверка: новое имя должно разрешиться в fake-IP, иначе отключение с объяснением
  (в том числе распознаётся правило NRPT, например от Tailscale).
* **Linux:** помощник прописывает в systemd-resolved для интерфейса Mosaic DNS 172.19.0.2 и домен `~.`
  (тогда resolved не спрашивает другие интерфейсы) и направляет DNS-серверы из `/etc/resolv.conf` в
  туннель маршрутами /32.
* **Android:** VpnService `addDnsServer(172.19.0.2)`.

### 3.4. Защита от петли (трафик ядер и hub не должен попадать обратно в туннель)

| Платформа, стек | Ядра → серверы | Прямые соединения hub |
| --- | --- | --- |
| Windows, свой | обходные маршруты /32 через физический шлюз (`netsh`) | `IP_UNICAST_IF` на физический интерфейс |
| Windows, sing-box | правило `process_name` → direct + `route_exclude_address` | через вход `direct-in` адаптера |
| Linux, свой | серверы как IP (резолв до подъёма TUN) + маршруты /32 от помощника | `SO_BINDTODEVICE` (без прав с Linux 5.7) |
| Linux, sing-box | `route_exclude_address` + серверы как IP | через `direct-in`; стек gVisor (брандмауэр не мешает) |
| Android | приложение исключено из VPN (`addDisallowedApplication`), серверы как IP | то же исключение |

---

## 4. Управление (control plane)

* **Controller** (`controller.rs`) — единственный API ядра для UI и CLI. Держит `State` (узлы, подписки,
  правила, настройки — `state.json`) и `Session` (hub, процессы ядер, TUN, защита DNS) под одним
  `tokio::sync::Mutex`.
* **Команды UI**: на десктопе — команды Tauri (`app/src-tauri/src/main.rs`); на Android — одна точка
  `api::dispatch(cmd, json)` через JNI. Имена команд и аргументов одинаковые, поэтому `app.js` один.
* **Снимок** (`snapshot`): UI опрашивает раз в секунду; в нём состояние, статус, движки, задержки,
  платформа и возможности (`platform`, `engine_updates`, `native_tun_available`…). UI прячет то, чего
  нет на платформе (`data-platform`, классы `desktop-only` / `windows-only` / `android-only`,
  строки `ключ@android` / `ключ@linux`).
* **Фоновый цикл** (`run_background`, тик 10 с): Smart Connect (активный узел — каждые 20 с, все — раз
  в 3 мин), обновление подписок по расписанию, проверка новых версий Mosaic (Windows, Linux).
* **Подключение** (`connect`): ядра → hub → **проверка узла** → режим (системный прокси / TUN) → защита DNS
  и проверка → «connected». Любая ошибка — полный откат (`teardown`): TUN, адаптер, hub, ядра, фильтры DNS
  последними.
  * Проверка узла (`check_before_switch`, 0.10): HTTP-запрос на URL теста задержки через SOCKS ядра,
    6 с × 2 попытки, **до** переключения системы — мёртвый узел не оставляет пользователя без интернета.
    Не ответил: без Smart Connect — отказ `[node-down] имя :: причина`; со Smart Connect — параллельно
    проверяются остальные узлы, выбирается самый быстрый, а если никто — `[no-node]`. UI распознаёт метки
    и показывает понятное состояние с действиями.
  * Шаги (`progress`: prepare → engines → check → route → dns) читаются UI без блокировки контроллера
    (отдельный `std::sync::Mutex`), каждые 250 мс; иконка в трее берёт оттуда «жёлтое» состояние.
* **Здоровье** (`Status.health`): после подключения активный узел проверяется каждые 20 с (со Smart
  Connect — в его цикле). Две неудачи подряд — `degraded` («нет связи через сервер», жёлтая иконка);
  ручной выбор узла сбрасывает в `unknown` и запускает проверку на следующем тике.
* **Защита от WebRTC** (`Routing::guard_udp`, 0.10): первый датаграм UDP-потока проверяется на STUN
  (magic cookie 0x2112A442). Если правило отправило бы его напрямую — он идёт через активный узел (или
  блокируется). Только в TUN; адреса локальной сети не трогаются.

---

## 5. Данные на диске

| | Windows | Linux | Android |
| --- | --- | --- | --- |
| Папка данных | `%APPDATA%\Mosaic` | `~/.config/mosaic` | `files/mosaic` |
| Состояние | `state.json` | то же | то же |
| Конфиги ядер на сессию | `run\xray.json`, `run\singbox.json`, `run\tun.json` | `run/…` | `run/…` |
| Обновлённые ядра | `engines\` (приоритет над встроенными) | `engines/` | — (обновляются с приложением) |
| Встроенные ядра | рядом с exe: `engines\` | пакет → копия в `bin/` (xray, sing-box, sing-box-tun, mosaic-tun-helper) | `nativeLibraryDir`: `libxray.so`, `libsingbox.so`; geo — `files/geo` |
| Автозапуск | `HKCU\…\Run` или задача планировщика (с правами) | `~/.config/autostart/mosaic.desktop` | `BootReceiver` |
| Прочее | системный прокси в реестре | правило polkit `/etc/polkit-1/rules.d/49-mosaic.rules` | — |

---

## 6. Ядра

* **Модель (IR):** `Node { protocol, transport, security, engine }` — `model.rs`. Импорт: `links.rs` (ссылки, base64, JSON Xray) и `formats.rs` (Clash/mihomo YAML, sing-box JSON, заголовки подписки `subscription-userinfo`, `profile-title`).
* **Матрица возможностей:** Xray — VLESS, VMess, Trojan, Shadowsocks (в т. ч. XHTTP); sing-box — всё,
  кроме XHTTP, плюс Hysteria2, TUIC, WireGuard. «Авто»: Xray для VLESS/VMess/Trojan, sing-box для остального.
* **Компиляторы:** `engines/xray.rs`, `engines/singbox.rs` (слоты → конфиг; TUN-адаптер — `compile_tun`).
* **Жизненный цикл:** `process.rs` — запуск без окна, логи в общий буфер, ожидание портов, остановка
  (на Unix SIGTERM, через 3 с SIGKILL).
* **Обновления:** `updates.rs` — последний релиз на GitHub → архив своей ОС → SHA-256 → распаковка во
  временную папку → `version` → замена с резервной копией → проверка с узлами → откат при ошибке.

---

## 7. Безопасность и права

| Что | Как |
| --- | --- |
| TUN на Windows | права администратора (UAC) |
| TUN на Linux | `cap_net_admin` на копии помощника / sing-box-tun; один запрос пароля через polkit (`pkexec setcap`); Mosaic сам без прав |
| TUN на Android | системное разрешение VPN |
| Обновления | только файлы с SHA-256 из релиза; без суммы — отказ |
| Доступ из LAN | выключен по умолчанию; без пароля — только для доверенных сетей |
| Утечки DNS | Windows: WFP + проверка после подключения (fail-closed); Linux: resolved `~.` + маршруты; Android: VpnService |

---

## 8. Инструменты и зависимости

### Языки и среды

| | |
| --- | --- |
| Rust (stable, edition 2021) | вся логика: hub, правила, DNS, TUN-стек, ядра, обновления |
| HTML/CSS/JavaScript без сборщика | интерфейс (`app/ui`), общий для всех платформ |
| Kotlin | Android-оболочка: WebView, VpnService, плитка, автозапуск |
| PowerShell, bash | скрипты загрузки ядер, CI |

### Rust-библиотеки

| Крейт | Зачем |
| --- | --- |
| `tokio` | асинхронная сеть, процессы ядер, таймеры |
| `serde`, `serde_json` | состояние, конфиги ядер, команды UI |
| `ipstack` | пользовательский TCP/IP-стек для своего TUN |
| `wintun` | драйвер TUN на Windows |
| `libc` | Unix: TUN-дескриптор, `SO_BINDTODEVICE`, capabilities, xattr |
| `ureq` | HTTP-клиент: подписки, GitHub Releases |
| `sha2` | проверка контрольных сумм обновлений |
| `zip`, `flate2`, `tar` | распаковка архивов ядер |
| `tauri` 2 (+ `tray-icon`) | настольное приложение, окно, трей |
| `jni` | мост Rust ↔ Kotlin на Android |

Собственные реализации без внешних зависимостей: парсер `geoip.dat`/`geosite.dat` (protobuf), DNS-кодек
и пул fake-IP, сниффинг SNI/Host, фильтры WFP, разбор ссылок всех протоколов.

### Android

AGP 8.7, Kotlin 2.0, compileSdk/targetSdk 35, minSdk 26, только arm64. Библиотеки: androidx core/activity/
lifecycle, `androidx.webkit` (WebViewAssetLoader), kotlinx-coroutines, `zxing-android-embedded` (QR).

### Внешние программы

| Программа | Где | Зачем |
| --- | --- | --- |
| Xray-core, sing-box | все | ядра |
| wintun.dll | Windows | TUN-драйвер своего стека |
| netsh, PowerShell | Windows | адреса, маршруты, DNS интерфейса; поиск маршрута по умолчанию |
| iproute2 (`ip`), `resolvectl` | Linux | помощник: интерфейс, маршруты, DNS |
| `setcap` (libcap), `pkexec` (polkit) | Linux | одноразовая выдача права на TUN |
| gsettings, kwriteconfig5/6 | Linux | системный прокси GNOME / KDE |

### Сборка и CI

| | |
| --- | --- |
| Tauri CLI 2 (`npx @tauri-apps/cli@2`) | установщик NSIS (Windows), deb/rpm/AppImage (Linux) |
| cargo-ndk + Android NDK, Gradle 8.11, JDK 17 | Android: `libmosaic.so`, APK |
| GitHub Actions | `windows.yml`, `linux.yml`, `android.yml`, `release.yml` (по тегу `v*`) |
| Actions | checkout, dtolnay/rust-toolchain, Swatinem/rust-cache (общий кеш по платформам), setup-node, setup-java, gradle/setup-gradle, cache (инструменты AppImage), upload/download-artifact, softprops/action-gh-release |

### Тестирование

* Юнит-тесты в `mosaic-core` (`cargo test --release -p mosaic-core`): парсеры ссылок, правила, geo,
  DNS, fake-IP, сниффинг, решения роутера, разбор HTTP-ответов, `/proc/net`, раскладки структур WFP,
  выбор архивов обновлений.
* Ручная проверка на устройствах: dnsleaktest, журнал соединений, строки диагностики `[tun] packets …`
  (раз в 5 с) и `[dns-check]` в логах.

---

## 9. Как расширять

* **Новый протокол:** вариант в `model::Protocol` → разбор ссылки в `links.rs` → компиляция в
  `engines/xray.rs` и/или `engines/singbox.rs` → строка в матрице возможностей (`supported_engines`).
* **Новое ядро:** `EngineKind` + `exe_name` + компилятор слотов + строки в `updates.rs` (имена архивов) и
  скриптах загрузки. Требование одно: SOCKS5-вход на каждый слот.
* **Новый тип правила:** `RuleKind` + `rules::matches` + подпись в `i18n.js` (`kind.<имя>`).
* **Новая платформа:** UI уже общий; нужны точка входа команд (`api::dispatch`), способ получить TUN
  (дескриптор → `tun_native` уже умеет работать с любым fd на Unix), защита от петли (§3.4) и упаковка.
* **Строки интерфейса:** ключи в `i18n.js` для `en` и `ru` (проверка совпадения ключей — скриптом node);
  вариант для платформы — `ключ@android` / `ключ@linux`.

---

## 10. Своё ядро (на будущее)

Архитектура уже допускает «ядро внутри процесса»: hub всё равно решает, куда идёт соединение, и ему не
важно, кто доставит его до сервера. План — гибрид, не замена Xray и sing-box:

| Протокол | На чём можно сделать в Rust | Сложность |
| --- | --- | --- |
| Shadowsocks 2022 / AEAD | `shadowsocks-rust` (как библиотека) | низкая |
| WireGuard | `boringtun` (userspace WireGuard от Cloudflare) | низкая–средняя |
| Trojan, VLESS поверх TLS / WebSocket / gRPC | `rustls`, `tokio-tungstenite`, `h2` | средняя |
| TUIC, Hysteria2 | `quinn` (QUIC) + свои протоколы поверх | средняя–высокая |
| VLESS + REALITY, XHTTP, отпечатки браузерного TLS (uTLS) | своя реализация рукопожатия; `rustls` не умеет маскироваться под Chrome | высокая |

Что это даст: на одно соединение меньше локальных переходов (без SOCKS между hub и ядром), меньше
памяти и процессов, мгновенный старт, а на iOS — вообще единственный реалистичный путь (один процесс,
лимит памяти около 50 МБ). Главный риск — антиблокировочная часть: Xray и sing-box быстро реагируют на
новые методы блокировок, а TLS-отпечаток Rust-библиотек легко отличить от браузера. Поэтому разумный
порядок — начать с WireGuard и Shadowsocks, а REALITY/XHTTP оставить ядрам Xray/sing-box.
