# Son 9 UI düzeltmesi — sözleşme

Kullanıcı maddeleri:
1. Uygulama boyu çevirilmemiş metinler → her ajan kendi dosyalarındaki İngilizce literal'leri `loc::t("Key")` ile değiştirir ve **kullandığı anahtarları çıktıda listeler** (toplu dil eklemesi WS-F'te).
2. Context menü alt menüsü fareyle üzerine gidince kayboluyor → WM_MOUSEMOVE/leave/submenu living-space hatası.
3. Animasyonlar açık ama çalışmıyor → toggle plumbing.
4. Cam ayarı var ama cam hissi yok → gerçek DWM/acrylic motoru.
5. Ayarlar > Dock en alt "Add Folder Stack" bölümünü kaldır.
6. Dynamic Island: hiçbir widget flyout'u açılmıyor; reference'tan kopyalanmamış tasarım/radius → detaylı incele + düzelt.
7. Ayarlar penceresi sabit genişlik/yükseklik, resize edilemez.
8. Temalar: "Yazı Tipi ve Geometri > Önizleme" kaldır; "Tema Galerisi" kaldır; "Temalarım" padding düzelt + 3 buton yerine tek `...` dropdown; yüzeyler için **ayrı ayrı** opaklık; kenarlık stili açıklama metnini kaldır.
9. Tema brush picker: saçma slider yığını yerine gerçek görsel renk seçici (renk alanı + hue/alpha + gradient stop'lar), ekran görüntüsündeki gibi değil.

## Dosya sahipliği (1 dosya = 1 ajan)

| WS | Worktree | Sahip dosyalar |
|---|---|---|
| A | fix-menu | `ui/menu.rs`, `ui/flyout.rs`, `ui/text_input.rs` |
| B | fix-glass | `design/**` (mod, theme, tokens, radius, brush, windows_palette, fonts, icons, brand_paths) |
| C | fix-dock | `dock/**` (mod.rs, dcomp.rs, layout.rs, dynamic_island.rs, preview.rs, tray.rs, show_desktop.rs), `opentaskbar-core/src/dynamic_island.rs` |
| D | fix-settings | `ui/settings.rs`, `ui/settings_search.rs`, `ui/components.rs`, `ui/prompt.rs`, `ui/mod.rs` |
| E | fix-widgets | `widgets/**` |
| F | fix-locales (2. faz) | `opentaskbar-core/src/i18n.rs`, `locales/**`, `opentaskbar-core/tests/i18n_parity.rs`, `scripts/check-locale-parity.ps1` |

`main.rs`, `Cargo.toml`, `.github`, diğer core dosyaları orchestrator'da.

## Çapraz API

- **WS-B**: `design::glass::{apply_window_backdrop(hwnd: usize, is_dark: bool) -> bool, accent_policy(hwnd, color, opacity) -> bool}` — DWM `DWMWA_SYSTEMBACKDROP_TYPE` + `SetWindowCompositionAttribute(ACCENT_ENABLE_ACRYLICBLURBEHIND)` fallback; GPUI/settings ve dock için tek kaynak. `design::theme::SurfaceStyle` her yüzey için bağımsız opacity setter/getter (`set_opacity(kind, f64)`, `opacity(kind)`).
- **WS-C**: dock HWND'sinde `apply_window_backdrop`; DI flyout açma: `widgets::open_widget_flyout` / `ui::flyout` çağrıları island widget tıklamasından tetiklenmeli; `DynamicIslandWindow.xaml.cs` ile 1:1 davranış (sayfa, chip, genişleme, radius) — mevcut `dock/dynamic_island.rs` ve `opentaskbar-core/src/dynamic_island.rs` karşılaştırılacak.
- **WS-D**: `WIN_W/WIN_H` sabit + `WindowOptions` resizable=false; `design::glass::apply_window_backdrop` ayarlar penceresine uygulanır; opaklık satırları 4 yüzey için ayrı.
- **WS-E**: `loc::t` anahtarları; `widgets/mod.rs` flyout başlıkları/metinleri.

## Kurallar
- Her ajan YALNIZCA kendi dosyalarını düzenler; `git add <kendi dosyaların>` ASLA `-A`; commit atar, merge/rebase yapmaz.
- Cross-WS API eksikse not bırakır.
- `cargo check --workspace` + kendi testleri; çıktıda **kullandığı tüm i18n anahtarlarını** listeler.
