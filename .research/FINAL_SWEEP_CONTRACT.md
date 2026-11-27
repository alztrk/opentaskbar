# Son temizlik turu — sözleşme

Hedef: (A) Kaldırılan yapay zekâ sağlayıcısına ait **her şeyi** kaldır; (B) 8 derin denetim raporundaki kalan tüm bulguları düzelt. Orchestrator: ana oturum. Her dosya tek ajana ait.

## Kaldırılan sağlayıcı — temizlik listesi (her ajan kendi dosyalarında yapacak)
- widget registry/descriptor/chip spec/flyout/katalog arama alias'ları
- `telemetry::AI_PROVIDERS`, `read_provider_token`, `ai_provider_detected`
- `core/settings.rs`: kaldırılan grup filtresi alanı + migrate/validate/default
- `ui/settings.rs`: kaldırılan sağlayıcının ayar satırları
- locale'ler: kullanımdan kaldırılan 5 AI grubu anahtarı
- README/CHANGELOG/docs/testlerdeki tüm eski sağlayıcı referansları
- `design/brand_paths.rs` kaldırılan sağlayıcının path'i
Not: eski sağlayıcı adı **hiçbir yerde** kalmamalı (`git grep -i <eski-ad>` → boş).

## Dosya sahipliği

| WS | Worktree | Sahip dosyalar |
|---|---|---|
| 1 | sweep-cfg | `opentaskbar-core/src/{settings.rs,dynamic_island.rs,hotkey.rs}`, `opentaskbar-core/tests/{settings_compat.rs,opentaskbar_core_tests.rs}` |
| 2 | sweep-i18n | `opentaskbar-core/src/i18n.rs`, `locales/**`, `tests/{i18n_parity.rs,telemetry_parity.rs}`, `scripts/check-locale-parity.ps1`, `README.md`, `CHANGELOG.md`, `PRIVACY.md`, `docs/**`, `Rust-Port/README.md` |
| 3 | sweep-shell | `opentaskbar-shell/src/**` |
| 4 | sweep-platform | `opentaskbar-app/src/platform/**` |
| 5 | sweep-design | `opentaskbar-app/src/design/**` |
| 6 | sweep-dock | `opentaskbar-app/src/dock/**` |
| 7 | sweep-flyout | `opentaskbar-app/src/ui/{flyout.rs,menu.rs,text_input.rs,components.rs}` |
| 8 | sweep-uisettings | `opentaskbar-app/src/ui/{settings.rs,settings_search.rs,prompt.rs,mod.rs}` |
| 9 | sweep-widgets | `opentaskbar-app/src/widgets/**` |
| 10 | sweep-picker | `opentaskbar-app/src/window_picker.rs` (yeni), `dock/window_picker.rs` (yeni) — sadece yeni dosyalar |
| 11 | sweep-fjord | `opentaskbar-app/src/fjord/**` (yeni), `ui/fjord_files.rs` (yeni), `ui/fjord_bar.rs` (yeni) — sadece yeni dosyalar |

`main.rs`, tüm `mod.rs` bildirimleri, `Cargo.toml`, `.github/workflows/**`, `packaging/**`, `scripts/publish-*.ps1` → **orchestrator**.

## Çapraz API sözleşmesi (ajanlar birbirini beklemeden bu isimlerle kodlar)

- **WS-1 cfg**: `settings::effective_width(monitor_w)->f32`, `effective_height()->f32`, `effective_top_gap()->i32`, `effective_bottom_gap()->i32`, `widget_density_for(id)->f32`, `apply_full_profile(&mut DockSettings,&DockProfile)`. `MagnificationExpandsDock` alanı eklenir.
- **WS-3 shell**: `journal::read_state_valid()->Option<u32>` (bit maske 0x03), `watchdog` heartbeat **auto-reset** event, `session::reconcile_after_taskbar_created()`, `session::run_hide_retry_step()`, `appbar::set_and_verify_state()`, `appbar::set_position_pinned()`, `tracker` AUMID (var) + `enumerate_windows_async` (var).
- **WS-4 platform**: `wifi::{scan_networks()->Vec<WifiNetwork>, connect(ssid,password)->bool, disconnect()}`, `core_audio::find_app_session` public, `wireless_battery::read_all_with_settings` kalıcı `OutlierGuard` + PnP kaynak, `update::find_newer_packaged_guard()`, `update::apply_with_elevation()`, `startup_registration::sync_all()` (StartupApproved + MSIX), `night_light` monotonic timestamp.
- **WS-5 design**: `icons::lucide(name)->Option<&'static str>` (93 path), `theme::apply_pack_tokens_to_palette()`, varsayılan font Inter, Fluent glyph fallback listesi.
- **WS-6 dock**: `dock::apply_runtime_settings()` tüm yeni ayarları kullanır; `window_picker::show(...)` çağrısı; `fjord::bar` komutları; WM_CLOSE; 400 ms hide; geçerli geometri sabitleri; layout helper'larını üretime bağla.
- **WS-7 flyout**: WM_CLOSE, klavye gezinme, focus return, target-based suppression, searchable dropdown sanallaştırma (`components.rs`).
- **WS-8 uisettings**: tüm yeni ayar satırları, eski sağlayıcı temizliği, profil 15 alan, update/startup doğru API'ler, tema token uygulama hook'u.
- **WS-9 widgets**: AI `AiFullDetail` cache'e yazılmalı (artık `default()` değil), BT device id dispatch, canlı flyout tick, clock culture/DST, weather birimler, stats gerçek GPU history, media AUMID, night light, AI creds/backoff, storage format, keyboard sticky.

## Yeni i18n anahtarları (WS-2 bunları 14 dile ekler; diğer ajanlar `loc::t("...")` kullanır)
`WindowPickerTitle, WindowPickerPrevious, WindowPickerNext, WindowPickerPage, MagnificationExpandsDock, MagnificationExpandsDockDescription, FjordFiles, FjordFilesOpen, FjordBarShow, FjordBarHide, WifiScan, WifiConnect, WifiDisconnect, WifiPassword, WifiNoNetworks, WifiConnectFailed, SearchNoResults, ContentAlignStart, ContentAlignCenter, ContentAlignEnd, PreviewDisabledHint, FocusMinutesLabel, FocusBreakMinutesLabel, FocusSessionsLabel, WidgetDensityCompact, WidgetDensityFull, AiUsageRingLabel`
Ayrıca eski sağlayıcının 5 anahtarı **silinir**. Sayısal kapılar yeni sayıya eşitlenir.

## Kurallar
- Her ajan YALNIZCA kendi dosyalarını düzenler; `git add <kendi dosyaların>`, ASLA `-A`; commit atar, merge/rebase yapmaz.
- Cross-WS API worktree'de yoksa derleme hatasını düzeltmeye çalışmaz, not bırakır.
- Her ajan `cargo check --workspace` + kendi testlerini çalıştırır; çıktıda eski sağlayıcı temizliği ve eklediği anahtarları listeler.
- Worktree'ler: `sweep-cfg`, `sweep-i18n`, `sweep-shell`, `sweep-platform`, `sweep-design`, `sweep-dock`, `sweep-flyout`, `sweep-uisettings`, `sweep-widgets`, `sweep-picker`, `sweep-fjord`.
