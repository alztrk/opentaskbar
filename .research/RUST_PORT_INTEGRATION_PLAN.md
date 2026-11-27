# Rust Port Entegrasyon Planı — reference Paritesi (P0–P3, tek akşam)

> Hazırlanma: 2026-11-26. Kaynak: `.research` gap analizi (4 katman: Services / Widgets / Views / Models+infra).
> Kararlar: (1) P3 riskli maddeler **dahil**, feature flag arkasında; (2) **worktree + sıralı merge**.

---

## 0. Gerçekçi hedef ve Definition of Done

Kapsam gerçekten "hepsi": ~49 eksik ayar alanı, ~30 tamamen eksik servis/sistem, ~15 widget davranışı, ~25 UI/davranış, varlık/CI/test farkları. reference test suite ~1935 `Check(...)`, Rust 93 `#[test]` — **test sayısı paritesi tek akşamda mümkün değil**; bunu P4 olarak bırakıyoruz.

**Bu akşamın DoD:**
1. `cargo check --workspace` + `cargo test --workspace` + `cargo clippy --workspace -- -D warnings` + `cargo fmt --all -- --check` temiz.
2. Ayar anahtarı paritesi: `DockSettings` alanlarının tamamı Rust'ta tipik alan veya alias; `DockProfile` veri kaybı yok (round-trip testi).
3. Locale paritesi: 14 dil × (reference 999 + Rust 77 + yeni özellik anahtarları), anahtar sayısı eşit.
4. P0–P3 maddeleri ya gerçek implementasyon ya **adlandırılmış feature flag arkasında güvenli stub** (risk register, §5).
5. Smoke: `--settings`, `--restore`, dock açılışı, dikey dock, island açılışı, tam yedekleme export/import.

**Kabul edilen istisna:** Hiçbir ajan `Shell_TrayWnd` host'unu flag olmadan varsayılan açık bırakmaz (sistem tepsisi kaybı riski).

---

## 1. Zaman çizelgesi (T = başlangıç)

| Faz | Süre | Yapan | Çıktı |
|---|---|---|---|
| T+0:00–0:15 | 15 dk | Lead | 8 worktree oluştur, bu sözleşme dosyasını sabitle, subagent'ları **background** modda spawn et |
| T+0:15–3:00 | 2s 45 dk | 8 subagent (paralel) | Kendi worktree'sinde commit'lenmiş, derlenen değişiklikler |
| T+3:00–3:45 | 45 dk | Lead | Sıralı merge (§4), her merge sonrası `cargo check`; mod.rs/Cargo.toml union çözümü |
| T+3:45–4:30 | 45 dk | Lead + gerekiyorsa 2–3 subagent | Entegrasyon hatalarını düzelt, `--deny warnings` temizle |
| T+4:30–5:00 | 30 dk | Lead | Final test/clippy/fmt/smoke, rapor |

> Bir ajan erken biterse boş durmaz: kendi alanındaki P4 testlerini yazar (WS-H ile koordinasyon).

---

## 2. Ortak sözleşmeler (CONTRACT — tüm ajanlar okur)

Bu dosya `D:\projects\OpenTaskbar\.research\RUST_PORT_INTEGRATION_PLAN.md`. Hiçbir ajan başka ajanın dosyasına dokunmaz; ihtiyacı olan API'yi aşağıdaki isimlerle çağırır, eksikse lead'e mesaj bırakır.

### C1 — Settings şeması (WS-A'nın sahipliğinde)
Rust `DockSettings`, `Models/DockSettings.cs:134-269` alanlarını **aynı PascalCase JSON anahtarıyla** içerir:
- Yeni tipik alanlar (48): `PreviewsEnabled`, `NotificationBadgesEnabled`, `MenuOpacity`, `WidgetOpacity`, `SettingsOpacity`, `InstalledWidgets`, `ContentAlignment`, `ShowMedia`, `ShowFocus`, `ShowAudio`, `ShowQuickSettings`, `FocusMinutes`, `FocusBreakMinutes`, `FocusSessions`, `StartAction`, `StartPlacement`, `StartWidgetIndex`, `WheelVolumeEnabled`, `ShowWeather`, `TargetDisplay`, `SecondaryApps`, `SecondaryWidgets`, `WidgetOrder`, `ThemeApplicationIcons`, `WidgetDensityOverrides`, `DockItemOrder`, `AiUsageProviders`, `AiUsageRing`, `ShowSearchButton`, `ShowTaskViewButton`, `DockWidthMode`, `DockWidthPixels`, `DockWidthPercent`, `DockTopGapUnit`, `DockTopGapPixels`, `DockTopGapPercent`, `DockBottomGapUnit`, `DockBottomGapPercent`, `DockHeightMode`, `DockHeightPixels`, `DockHeightPercent`, `WirelessBatteryPollSeconds`, `WirelessBatteryLowThreshold`, `IgnoredWirelessDevices`, `WirelessDeviceAliases`, `WirelessBatteryListenOnly`, `WeatherUnit`, `MirrorMainDock`.
- Alias'lar: `Widgets→EnabledWidgets`, `ShowClock→ShowClockWidget`, `ShowSystemStats→ShowSystemStatsWidget`, `Layout→FullWidth` (enum↔bool dönüşümü), `FocusModeEnabled→FocusMode`, `PinnedTrayIcons→UserPinnedBackgroundApps`.
- `DockProfile`: `#[serde(flatten)] extra: Map<String,Value>` **+zorunlu** alanlar `HiddenApps, InstalledWidgets, WidgetOrder, DockItemOrder, DockWidthMode/Pixels/Percent, Theme, ThemePack, ThemeApplicationIcons, LauncherHotkey, DockHotkey, CustomLauncherHotkey, CustomDockHotkey` (hepsi `default` ile).
- `pub fn validate(&self) -> Vec<&'static str>`: `DockSettingsStore.IsValid()` kuralları (hotkey çakışması, widget altküme invariantı, yüzde aralıkları 10..100, wireless poll 15..300, island order/placement, profil/grup sınırları).
- Varsayılanlar referans ile aynı: `DockOpacity=0.9`, `DockBottomGapPixels=6`, `WeatherCity=""`.
- Legacy migrasyonları: `DockBottomGap`, retired widget ID'leri, `MigrateLegacyAiUsage`, `MigrateSurfaceStyles`, `MigrateLegacyIconPadding`, `MigratePinnedTrayIcons`.

### C2 — Flyout blokları (WS-E sahipliğinde, WS-C tüketir)
`ui/flyout.rs` içine eklenecek:
```rust
FlyoutBlock::SeekSlider { id:u32, label:String, position_secs:u32, duration_secs:u32, ratio:f32 }
FlyoutBlock::TextInput  { id:u32, label:String, value:String, numeric:bool, min:f64, max:f64 }
FlyoutBlock::Chart      { label:String, series:Vec<(String, Vec<f32>)>, accent:Rgb }
FlyoutBlock::Image      { glyph:u16, path:Option<String>, size_dips:f32 }  // cover art / bayrak
FlyoutBlock::IconBar    { buttons:Vec<(u32,String,u16,bool)> }
LiveEvent::Seek { id:u32, ratio:f32 }
LiveEvent::TextCommit { id:u32, text:String }
```
Mevcut `ActionRow / Hero / CalendarGrid / ProgressBar / StatRow / BadgeRow / AudioSliderSection / ButtonBar / Separator / Note` korunur.

### C3 — i18n (WS-H sahipliğinde)
Yeni anahtarlar **14 dilin tamamına** eklenir (WS-H), diğer ajanlar `loc::t("Key")` ile kullanır:
`SpeedTest*`, `FocusBreak*`, `Radio*`, `FullBackup*`, `ThemeEditor*`, `Seek*`, `HardwareUnavailable`, `StartMenu*`, `ProfileHiddenApps`. Eksik anahtararse fallback = key adı; **derleme hatası değil**.

### C4 — Platform API (WS-B / WS-G sahipliğinde)
Tüketiciler bu isimleri kullanır; sağlayıcılar bunları `pub` yapar:
```rust
platform::app_identity::{is_packaged() -> bool, store_update_uri() -> String}
platform::start_menu_state::{is_open() -> bool, install_hook()}
platform::taskbar_pin_monitor::start_watch()   // RegNotifyChangeKeyValue
platform::core_audio::{list_devices(is_recording:bool), set_default_device(id:&str), subscribe()}
platform::radio::{state(kind:&str)->bool, set(kind:&str,on:bool)->bool}   // WinRT Radio
platform::country_city::{countries(), cities(country:&str)}
platform::cursor_auth::{sign_in_start(), sign_in_finish(), sign_out(), token()}
platform::shell_shortcuts_ex::{show_desktop_for_drag(), minimize_all(), release_stuck_modifiers(), replace_focused_text(&str)}
platform::win32_ex::{is_shell_overlay(hwnd), display_for(name:&str), set_default_audio_device(id:&str)}
platform::render_mode::{preference(), apply()}
shell::flyout_writer::{write_start(), write_search(), write_control_center()}  // + generation counter
widgets::telemetry::{ai_fetch_full(id), weather_forecast(), sample_gpu_temp(), media_seek(secs)}
```

### C5 — Tema API (WS-F sahipliğinde)
```rust
design::windows_palette::{read() -> WindowsPalette, install_listener()}  // SystemUsesLightTheme öncelikli
design::radius::RadiusManager::derive(base:i32) -> NineKeys
theme_packs::{install_bundled(), create_theme(based_on, preserve_icons), resolve_widget_tokens()}
```

### C6 — mod.rs / Cargo.toml
Lead sahipliğinde. Ajanlar kendi worktree'sinde yeni modül için **bir satır** `pub mod x;` ekleyebilir; lead merge'de **union** alır. Başka ajanın `mod.rs`/`Cargo.toml` değişikliği yapılmaz.

---

## 3. Workstream'ler ve dosya sahipliği

| WS | Sahip olunan dosyalar (yalnız bunlar) | Kapsam (gap analizi maddeleri) | Çıktı kriteri |
|---|---|---|---|
| **A — Config (kritik, önce merge)** | `opentaskbar-core/src/settings.rs`, `hotkey.rs`, `opentaskbar-core/tests/**` | §C1'in tamamı, `validate()`, migrasyonlar, hotkey token seti + `TryFromKeyEvent`, varsayılan eşitleme | `settings.json` round-trip testi; profil alanları kaybolmuyor; `validate()` 10+ kural |
| **B — Shell / Win32 / startup** | `opentaskbar-shell/src/**`, `opentaskbar-app/src/platform/{mod.rs,win32.rs,pins.rs,pins/**,app_identity.rs,start_menu_state.rs,taskbar_pin_monitor.rs,gpu_preference.rs,render_mode.rs,startup_registration.rs,shell_shortcuts_ex.rs,win32_ex.rs}` | watchdog heartbeat, StartMenuState, native flyout journal yazımı + generation, ShellShortcuts sistem araçları/ReleaseStuckModifiers/ReplaceFocusedText/ShowDesktopForDrag, Taskband pin monitor, MSIX identity + StartupTask, GPU exports, render-mode + CLI arg çözücü, DisplayService DIP, NativeTaskbarWindow lease, TaskbarWindowRules, TaskbarReplacementSession retry/verify, AppBarClient hung probe, VirtualDesktops current/count, structured ShellTelemetry (`core/log.rs`) | Heartbeat tetikleniyor; journal Start/Search/CC yazılıyor; restore doğrulama 3 deneme; MSIX/portable ayrımı |
| **C — Widget sunumu & flyout'lar** | `opentaskbar-app/src/widgets/{mod.rs,ai_flyout.rs,speedtest.rs,focus_break.rs,media_ext.rs,catalog_ops.rs}` | hover coordinator, AI flyout (spend/breakdown/reset/plan), NetSpeed hız testi, medya seek/cover/tint/volume, radyo toggle (C4), Focus Break + ses + oturum, countdown özel süre, centisecond, sparkline, weather tahmin/°F/ikon, GPU meter, BT cihaz rename/hide/interval/threshold/listen-only/diagnostics, storage fixed drive, audio device selector, keyboard target capture, per-widget density + hardware gizleme, katalog alias ID | Her widget için tıklama/hover/flyout referans ile aynı; 23 widget katalog |
| **D — Dock çekirdeği & etkileşim** | `opentaskbar-app/src/dock/{mod.rs,dcomp.rs,preview.rs,tray.rs,dynamic_island.rs,show_desktop.rs}` | **dikey dock (edge)** + side resize + mirrored dock, ikincil hafif şerit/shelf, hover panel wiring (C), magnify guard'ları/hover icon/expansion budget, fisheye spring, auto-hide+DWM bounds+overlay filtresi, item ordering histerezisi, klavye gezinmesi, OLE drag-out + drag-hover + recycle-bin drop + stack/widget drop, klasör stack görseli + watcher, grup flyout üyeleri + reorder, island **expanded details** + input pre-process, Aero Peek + fade, badge çizimi, tray strip canlı ikon + pin identiti + oryantasyon, Start aç/kapat + odak iadesi, session switch reset | Dikey dock çalışıyor; hover panel açılıyor; island expand; `DockLayoutChecks` mantığına karşılık gelen 15+ saf fonksiyon testi |
| **E — UI / Ayarlar / Flyout / Menü** | `opentaskbar-app/src/ui/{mod.rs,settings.rs,flyout.rs,menu.rs,components.rs,prompt.rs,text_input.rs,settings_search.rs}` | §C2 blokları, tema editörü entegrasyonu + dosya diyalogları + **tam yedekleme** UI, hotkey capture UI, hava durumu ülke/şehir seçici, AI OAuth giriş UI, tema galerisi, `DockTopGap*/Width*/Height*` UI, acrylic backdrop, hedef uygulama yerel menü komutları, DockFlyout mouse-hook/shadow/retain, dinamik arama kataloğu, WidgetPanelHost + flyout preview toggle | Tüm ayar satırları aranabilir; `.otbackup` export/import UI; flyout blokları C tarafından kullanılabilir |
| **F — Tema / palet / varlıklar** | `opentaskbar-app/src/design/{mod.rs,fonts.rs,icons.rs,brand_paths.rs,windows_palette.rs,radius.rs}`, `platform/{theme_packs.rs,icon_pack.rs,update.rs,toast.rs}`, `themes/**`, `Fonts/**`, `Services/Localization/Flags/**` kopyası | Windows vurgu paleti + canlı listener, RadiusManager 9 anahtar, SurfaceStyle 4 yüzey + SystemUsesLightTheme, WidgetPack typed token, **6 bundled tema + ilk açılış kurulumu**, 14 bayrak PNG, Inter fontları, CreateTheme + manifest limitleri, fallback plate/IconScale, toast info + lifecycle, update MSIX politikası + runas | Tema ilk açılışta kuruluyor; sistem rengi değişince canlı güncelleniyor; güncelleme hem portable hem MSIX |
| **G — Servisler / veri sağlayıcıları** | `opentaskbar-app/src/widgets/{telemetry.rs,diagnostics.rs}`, `platform/{wireless_battery.rs,jumplist.rs,icon_cache.rs,shell_files.rs,country_city.rs,cursor_auth.rs}` | AI: Cursor PKCE/DPAPI + CLI sign-in + OAuth refresh + gh keyring + spend/breakdown/severity; CountryCity API + 21 ülke + 81 il; Weather geolocation + language + dayCount; CoreAudio device list/switch + IMMNotificationClient + app-session scoring; night-light detayları; **wireless battery 10 vendor protokolü** + disk cache + outlier + alias; icon provider fallback/hover varyant; AppShortcutResolver name hint/webhelper; jumplist working dir; shell files path validation | Her.provider için en az bir saf fonksiyon testi; DPAPI round-trip; vendor report ayrıştırma testleri |
| **H — Test / CI / docs / packaging** | `crates/*/tests/**` (yeni), `.github/**`, `scripts/**`, `packaging/**`, `docs/**`, `README.md`, `CHANGELOG.md`, `PRIVACY.md`, `checksums.txt`, `opentaskbar-core/src/i18n.rs`, `opentaskbar-core/locales/**` | §C3 14 dil anahtar eşitliği, key-parity script'i (reference↔Rust), reference saf-mantık testlerinin Rust karşılığı (öncelik: AiUsage, FeatureAndMemory/.otbackup, WirelessBattery, MediaVolume, ShellRecovery, DockLayout), UI test harness'ı (mümkün olduğunca), rust-ci tüm branch, CodeQL/dependabot cargo, checksums/release-manifest, MSIX script, docs/privacy/design | 14 dil × eşit anahtar; `cargo test` yeşil; CI job'ları eklendi; parity script rapor üretiyor |

**Lead (bu oturum):** `main.rs`, tüm `lib.rs`, tüm `mod.rs`, tüm `Cargo.toml`, worktree yönetimi, merge, final doğrulama.

---

## 4. Merge planı ve kapılar

Merge sırası (bağımlılık):
```
A (config)  →  B (shell)  →  G (providers)  →  F (theme/assets)
            →  E (ui/flyout)  →  C (widgets)  →  D (dock)  →  H (tests/docs/CI)
```
Her merge adımı:
```powershell
git merge --no-ff --no-commit wt-<slug>
# mod.rs / Cargo.toml union çözümü (§C6)
cargo check --workspace        # FAIL ise merge'i düzeltmeden geçme
git commit -m "port(<area>): <özet>"
```
Final:
```powershell
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
# smoke: --settings, --restore, dock start, dikey dock, island, .otbackup round-trip
```

**Çakışma öngörüsü:** yalnızca `platform/mod.rs`, `ui/mod.rs`, `Cargo.toml` — lead union ile çözer. Diğer dosyalar ayrık sahiplikte olduğu için çakışma beklenmez.

---

## 5. Risk register ve feature flag'ler

| Risk | Flag (varsayılan) | Geri dönüş |
|---|---|---|
| Gerçek `Shell_TrayWnd`/`Shell_NotifyIcon` host'u tepsiyi kaybedebilir | `enable_live_tray_host = false` | Registry keşfi + no-op kalır; reference davranışı flag açılınca aktif |
| Native flyout yeniden konumlandırma / UIA arama senkronizasyonu Explorer'ı bozabilir | `native_flyout_reposition = false` | Journal yazımı kapalı, C watchdog yine eski journal'ı okur |
| 10 vendor HID protokolü yanlış rapor yorumlayabilir | `enable_vendor_battery_probes = false` | Genel HID capacity okuma kalır |
| GPUI tema editörü mevcut ayar UI'ını bozabilir | `enable_theme_editor = false` | Liste + import/export kalır |
| Watchdog heartbeat journal'ı yanlış temizleyebilir | — | Heartbeat yalnızca dock hayattayken set edilir; restore sonrası doğrulama zorunlu |
| Dikey dock DComp hit-test koordinatları | `enable_vertical_dock = true` (test var) | `DockEdge` alta zorlanır |
| Cursor OAuth ağ çağrıları | `enable_ai_oauth = true` | Token yoksa "sign in" durumu gösterilir |

Flags `DockSettings.extra` içinde taşınır; C1 tipik alan eklemez (reference karşılığı yok).

---

## 6. Subagent spawn şablonu (lead kullanır)

Her ajan **background** modda spawn edilir; prompt şu parçaları içerir:

```
GÖREV: WS-<X> — <kısa ad>.
WORKTREE: <mutlak yol>. Tüm read/edit/shell işlemlerinde mutlak yol kullan; shell'de workdir=<worktree>.
SÖZLEŞME: D:\projects\OpenTaskbar\.research\RUST_PORT_INTEGRATION_PLAN.md dosyasını oku (§2 C1–C6).
GAP ANALİZİ: <ilgili ajan raporundaki kendi alanın> — görevin budur.
SAHİPLİK: Yalnızca şu dosyaları düzenle: <WS tablosundaki liste>. Başka dosyaya dokunma.
  - mod.rs/Cargo.toml: en fazla birer `pub mod` satırı; lead union yapacak.
  - i18n: yeni anahtarları loc::t("...") ile kullan, WS-H ekleyecek.
  - API ihtiyacın varsa C4/C5 isimlerini kullan; yoksa lead'e not bırak, başkasının dosyasına yazma.
KURAL: Commit at (mesaj: "port(<alan>): <özet>"), merge/rebase YAPMA. Derlenemeyen kod bırakma:
  kendi worktree'nde cargo check --workspace çalıştır.
RİSK: <kendi alanının flag'i> varsayılan kapalı/true — tabloya uy.
ÇIKTI: changed dosyalar, eklediğin mod satırları, yeni pub API isimleri, bilinen eksikler.
```

Subagent sayısı: 8 + lead. Worktree isimleri: `wt-cfg`, `wt-shell`, `wt-widgets`, `wt-dock`, `wt-ui`, `wt-theme`, `wt-providers`, `wt-tests`.
Mevcut worktree'ler (`head-plus-hover`, `hover-baseline`, `taskbar-recovery-pr`) kullanılmaz.

---

## 7. Onay bekleyen tek operasyon

Bu plan onaylandığında lead sırayla:
1. 8 worktree oluşturur (`tools.opencode.worktree_create`),
2. 8 background subagent spawn eder,
3. T+3:00'te merge döngüsünü başlatır.

