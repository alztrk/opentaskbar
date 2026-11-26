# SON DÜZELTME TURU — sözleşme (orchestrator: ana oturum)

Kaynak: 8 denetim subagentının bulguları. Hedef: parite + hata + performans + güvenlik + test bulgularının **tamamı** kapatılacak. C# silme bu turdan SONRA, ayrı onayla.

## Dosya sahipliği (çakışma yok — her dosya tek ajan)

| WS | Worktree | Sahip dosyalar |
|---|---|---|
| 1 | fix-cfg | `opentaskbar-core/src/settings.rs` |
| 2 | fix-shell | `opentaskbar-shell/src/{lib.rs,tracker.rs,taskbar.rs,appbar.rs,native_tray.rs,journal.rs,session.rs,window_rules.rs,watchdog.rs,flyout_journal.rs,modifiers.rs}` |
| 3 | fix-start | `opentaskbar-shell/src/{flyout_reposition.rs,flyout_writer.rs}`, `opentaskbar-app/src/platform/start_menu_state.rs` |
| 4 | fix-devices | `opentaskbar-app/src/platform/{core_audio.rs,radio.rs,wireless_battery.rs,country_city.rs,app_identity.rs,taskbar_pin_monitor.rs,pins.rs,mod.rs}` |
| 5 | fix-crypto | `opentaskbar-app/src/platform/{cursor_auth.rs,update.rs}` |
| 6 | fix-widgets | `opentaskbar-app/src/widgets/**` (tümü) |
| 7 | fix-ui | `opentaskbar-app/src/ui/**` + `opentaskbar-app/src/design/**` |
| 8 | fix-dock | `opentaskbar-app/src/dock/**` |
| 9 | fix-tests | `.github/**`, `scripts/**`, `packaging/**`, `docs/**`, `README.md`, `CHANGELOG.md`, `PRIVACY.md`, `Rust-Port/README.md`, `opentaskbar-core/tests/**`, `opentaskbar-core/src/i18n.rs`, `opentaskbar-core/locales/**` |

`main.rs` ve tüm `Cargo.toml` **orchestrator'da** — mod satırlarını ve wiring'i o ekler.

## API sözleşmesi (ajanlar birbirini BEKLEMEDEN bu isimlerle kodlar)

### WS-2 shell
- `tracker::RunningWindow { aumid: String, ... }`, `GetApplicationUserModelId` okunur.
- `tracker::enumerate_windows_async(tx: std::sync::mpsc::Sender<Vec<RunningWindow>>)` — worker'da `enumerate_windows()` çağırır.
- `window_rules` modülü `lib.rs`'e bağlanır; `has_taskbar_button` ITaskbarList_Deleted + CLOAKED_SHELL + WS_EX kurallarını kapsar; `tracker` bunu kullanır.
- `shell::modifiers::{note_down(vk), note_up(vk), release_all()}` — `taskbar::send_chord` bunları `finally` mantığıyla çağırır.
- `session::install_shell_hooks(cb)` callback `catch_unwind` içinde; fn pointer `Option<fn()>` olarak saklanır (transmute kalkar).
- watchdog: 1 process/arg/heartbeat/clean-exit testi (inline).

### WS-3 Start/flyout
- `open_start_via_syscommand(owner_hwnd) -> bool` **non-blocking** (sadece post sonucu); doğrulama worker'da, `mark_opened()` yalnızca UNCLOAKED hook'tan — çift generation artışı kalkar.
- `close_start(hwnd)`: `mark_closed()` yalnızca kendi HWND'si hâlâ aktifse (token/HWND guard); gecikmeli eski çağrı yeni açılışı iptal etmesin.
- `was_open_at_press()` tekrar üretimde kullanılacak (WS-8 çağırır).
- `start_menu_state::HOST_CACHE` TTL 2 sn; cloaked Start "açık" sayılır; `is_open()` cached.
- `FlyoutKind::{Start,Search,ControlCenter}` + `reposition_flyout(kind, anchor, hwnd) -> bool`; `reposition_start` wrapper kalır. Search/CC için de gerçek worker.
- `start_menu_state::on_opened` callback'i hook thread'inde; WS-8 anchor'ı oradan alır.

### WS-4 cihazlar
- `core_audio::list_app_sessions() -> Vec<AppAudioSession>` gerçek IAudioSessionEnumerator; `set_app_volume/mute` yazılabilir. `media_ext` bunları kullanacak (WS-6).
- `radio::set(kind,on) -> bool`: `IAsyncAction` için `wait_async_completed` (GetResults YOK), fire-and-forget `refresh_async`, spawn+join yok. Başarılıysa true.
- `wireless_battery`: PnP Bluetooth okuyucu + G-Hub WebSocket + SteelSeries coreProps HTTP loopback poller'ları; `read_all_with_settings()` aggregator (outlier + disk cache), poll aralığı ayardan; HID probe thread'leri join/timeout.
- `pins::import_packaged_pins() -> Vec<PinnedApp>` + `load_pins()` bunu birleştirir; AUMID→exe/icon çözümü.
- `app_identity`: MSIX `StartupTask` read/write gerçek WinRT.

### WS-5 crypto/güncelleme
- `cursor_auth`: tüm HTTP `telemetry::winhttp_json` ile (secret ASLA argv'de); PKCE `BCryptGenRandom` 32 bayt; `token_json` = `serde_json::to_string`; PowerShell fallback kaldırılır veya stdin+`''` kaçış.
- `update::is_valid_sha256_digest`: `value.get(..7)` ile panic yok. `write_updater_script` Unicode `%` ve chcp 65001, BOM'suz.

### WS-6 widget verisi
- `media_ext::librewolf_timeline(source_app_id,title) -> Option<(f64,f64)>` gerçek UIA (production).
- `media_ext::confirm_seek(...)` bayat SMTC sample'ını reddeder; rate/duration=0 senaryoları.
- `WidgetRuntime::tick` storage/keyboard snapshot 1 sn TTL; animasyon tick sadece DComp transform.
- Antigravity/AI/BT/weather mevcut davranış korunur.

### WS-7 UI/tema
- `design::theme::apply_token_overrides(map)` — tema editörü kaydedilen token'ı gerçek palete uygular; brush picker (HSV/alpha/gradient stop) eklenir.
- `ui/settings`: ülke/şehir `platform::country_city` API'sine bağlanır (statik listeden çıkılır); Cursor UI `sign_in_start/finish` çağırır; AI CLI sign-in bağlanır; `StartPlacement` satırı eklenir.
- `ui/menu.rs`: çakışan `&mut` aliasing kaldırılır (raw pointer yazımı veya tek `&mut`), HWND bazlı re-entrancy guard.
- `ui/flyout.rs`: external menu class kontrolü UI thread'inde cache/worker; 16 ms anim korunur.

### WS-8 dock
- `StartPlacement End/InWidgets` canlı `apply_pinned_groups` + draw + hit-test + ayar UI (WS-7 satırı).
- Island: `set_expanded(true)` + `request_details()` çağrıları, 246px details panel, Escape.
- Aero Peek `EnableAeroPeek` registry gate.
- İkincil/ayna dock: `secondary_apps/secondary_widgets/mirror_main_dock/target_display` kullanılır; light strip.
- Notification badge: `set_notification_badge()` çağrı yeri + EVENT_SYSTEM_ALERT kaynağı.
- `mouse_wheel_hook`: `WindowFromPoint` kaldırılır, sadece koordinat post edilir; hit-test UI thread'inde.
- `tracker::enumerate_windows_async` kullanılır (WS-2 API); `refresh_widgets` çağrıları throttle.
- VSync re-post döngüsü tek seferlik guard; `DockWndProcGuard` HWND bazlı; deferred reload PostMessage hatasında flag geri alınır.

### WS-9 test/CI
- `window_rules` modül testlerini gerçek üretim API'sine bağla; `tracker` vektörleri.
- Backup: üretim `build_backup/inspect_backup/import_backup` + `Version != 1` reddi; yerel kopya testler üretime bağlanır; tautolojiler kaldırılır.
- MediaTimeline, radio::set, Night Light, watchdog, multi-monitor, auto-hide, updater script testleri üretim API'lerine karşı.
- `rust-ci.yml` path filtresine `Services/Localization/Languages/**`; CodeQL tüm branch; `CHANGELOG` 1129; checksums drift.
- locale parity 14× eşit (mevcut 1129 korunur).

## Kurallar
- Her ajan YALNIZCA kendi dosyalarını düzenler; `git add <kendi dosyaların>`, ASLA `-A`; commit atar, merge/rebase YAPMAZ.
- Cross-WS API worktree'de yoksa derleme hatasını düzeltmeye çalışmaz, not bırakır.
- Her ajan `cargo check --workspace` + kendi testlerini çalıştırır; son doğrulamayı orchestrator yapar.
- Worktree'ler: `fix-cfg`, `fix-shell`, `fix-start`, `fix-devices`, `fix-crypto`, `fix-widgets`, `fix-ui`, `fix-dock`, `fix-tests`.
