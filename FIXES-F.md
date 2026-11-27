# FIXES-F — Platform katmanı derin inceleme bulguları

Çalışma ağacı: `D:\projects\OpenTaskbar-wt-f` (branch `fix/deep-f`).
Yalnızca `main.rs`, `window_picker.rs`, `platform/**` ve Cargo manifestleri üzerinde çalışıldı.

## Durum

| Bulgu | Durum | Açıklama / test |
|---|---|---|
| **[Yüksek] GPU tercih sembolleri fonksiyon olarak dışa aktarılıyor** | **Fixed** | `gpu_preference.rs` artık `u32` / `i32` data statics ihraç ediyor; `#[used]` korundu, `.def` notu DATA yönünde güncellendi. Derleme zamanı `const _: u32/i32` doğrulaması + birim testi eklendi. |
| **[Yüksek] `is_valid_startup_executable()` Unicode bayt dilimi paniği** | **Fixed** | Bayt-tabanlı, char sınırı güvenli `get(..)` karşılaştırması. `OpenTaskbağ.exe` / `OpenTaskbarğ.exe` vektörleri için test eklendi. |
| **[Yüksek] Kablosuz pil: iki pasif sağlayıcı taranmıyor** | **Fixed** | `ALL_SOURCES` listesi `LogitechGHub` + `SteelSeriesGg` içeriyor; `read_all()` bu listeyi kullanıyor. Test, tüm `BatterySource` varyantlarının canlı yoklamada yer aldığını doğruluyor. |
| **[Yüksek] Kablosuz pil: iki veri modeli / dört global durum** | **Skipped (kısmi)** | `source_priority()` → `BatterySource::priority()` ve `ws_g_normalize_name()` → `normalize_name()` birleştirildi. `WirelessReading` ↔ `BatteryReading` / ayrı cache ve guard'ların tek tipe indirgenmesi ~4.7k satırlık modülde yüksek regresyon riski taşıyor ve deterministik testi yok; davranış koruma kuralı gereği yapılmadı. |
| **[Orta] WinHTTP + SHA-256 kopyaları** | **Fixed** | Yeni `platform::http` tek çekirdek (`HttpCfg`, `Proxy`, `parse_url`, `request`, `get_body`, `loopback_get`); `update`, `country_city`, `cursor_auth`, `wireless_battery` bu çekirdeğe geçti. Elle yazılan iki SHA-256 `sha2::Sha256` ile değiştirildi. `platform::win32_helpers` ortak `wide()` + `CREATE_NO_WINDOW` sağlıyor (12 dosya). URL/cfg/wide/json için testler eklendi. |
| **[Orta] Güncelleme sistemi iki paralel yığın** | **Kısmi fixed** | Ölü `download_and_apply`, `find_newer_detailed`, `ReleaseWithVersion` kaldırıldı; `version_parts` 4'ten fazla bileşeni `Version::parse` ile aynı şekilde reddediyor (test eklendi). `DetailedRelease` / `Release::from` dönüşümü korundu: `ui/settings.rs` (sahiplik dışı) `DetailedRelease::from(&release)` çağırıyor, tip birleştirmesi orayı değiştirmeden mümkün değil. |
| **[Orta] Toast: iki gösterim yolu, `action=update` hiç üretilmiyor** | **Kısmi fixed** | Tüm API'ler tek WinRT yolundan (`show_xml`) geçiyor; PowerShell/XML host, `script_path`, `powershell_script` ve kaynak-metin testleri kaldırıldı. `show_update_toast_for_release` WinRT yoluna delege ediyor. `action=update` üretimi korundu (geri uyumlu CLI); üretim toast'ı protocol tipinde kaldığı için abonelikler tetiklenmiyor — tıklama akışını değiştirmek entry-point/UI koordinasyonu gerektirir. |
| **[Orta] Başlatma kaydı iki kaynak** | **Fixed** | `win32::{is,set}_startup_enabled` artık `startup_registration::{read,set_enabled}` için ince uyumluluk sarmalayıcıları; MSIX politikası ve StartupApproved artık atlanmıyor. Davranış dışarıdan aynı. |
| **[Orta] `radio::set()` iyimser önbellek, hata gizli** | **Kısmi fixed** | Platform tarafında hata kanalı eklendi: `last_set_failure()` / `take_last_set_failure()`, başarısız yazımda önbellek `-1`'e döndürülüp gerçek durum yeniden okunuyor. `refresh_async()` RAII guard ile panik/spawn hatasında bayrağı temizliyor. Monitor için `stop_state_monitor()` iptali eklendi. Çelişkili yorum düzeltildi. Testler: hata yayılımı + rollback, panikte bayrak temizliği, monitor iptali. **UI bağlantısı yapılmadı** (`widgets/mod.rs` sahiplik dışı; yeni hata API'sini orası tüketmeli). |
| **[Orta] `pins::load_pins()` COM apartment dengesizliği** | **Fixed** | Yeni `platform::com::ComApartment` RAII guard'ı; `jumplist`, `pins`, `pins::launch`, `startup_registration` dört çağrı yolu da onu kullanıyor. `RPC_E_CHANGED_MODE` artık açıkça belgeleniyor. Test: iç içe guard çift-init durumunda `owns() == false`. |
| **[Orta] `icon_cache::Worker::spawn()` boru sızıntısı** | **Fixed** | `from_child()` eksik pipe'larda çocuğu `kill()` + `wait()` ediyor; `cleanup_failed_child()` için deterministik test ve `from_child()` hata yolu testi eklendi. |
| **[Orta] `relaxed_json` kopyaları / tema keşif kuralları** | **Kısmi fixed** | Ortak `platform::json_ext` (testli) her iki manifest okuyucusunda kullanılıyor; `validate_theme` sabitleri kullanıyor; `discover_installed()` = `discover(&root())` (güvenli-id + symlink kontrolleri artık aynı). Tek manifest tipine indirgeme atlandı: `ui/settings.rs` `ThemeManifest.name: String` alanına `IntoElement` olarak ihtiyaç duyuyor (sahiplik dışı). |
| **[Orta] Dock "Klasör Yığını" etiketleri** | **Skipped** | `Downloads` / `Documents` anahtarları 14 locale dosyasında yok; düzeltme sahiplik dışındaki `crates/opentaskbar-core/locales/**` dosyalarını değiştirmeyi gerektiriyor. |
| **[Orta] `PRIVACY.md` eksik ağ trafiği** | **Skipped** | Dosya sahiplik dışında. Not: loopback sağlayıcıları artık canlı taranıyor; belge güncellemesi ayrı bir agent tarafından yapılmalı. |
| **[Düşük] Bayat yorumlar / `allow(dead_code)`** | **Fixed** | `platform/mod.rs`, `main.rs`, `window_picker.rs`, `core_audio.rs`, `radio.rs`, `update.rs`, `icon_pack.rs` yorumları güncellendi; mojibake (`ΓåÆ` → `→`) düzeltildi; modül bazlı 16 `allow(dead_code)` kaldırıldı; birkaç gerçek ölü API silindi. Kullanıcısı olmayan `icon_cache::{ApplicationIconProvider, RenderImage, shell_item_display_name}` silindi; kalan herkese açık uyumluluk API'leri (`set_blocking`, `show_update_toast_for_release`, `win32` startup sarmalayıcıları) korundu. |
| **[Düşük] Kaynak-metni testleri** | **Fixed** | `shell_shortcuts_ex::release_stuck_modifiers_with` enjekte edilebilir `SendInput`/fallback sink'leriyle test ediliyor (kaynak dizgesi yerine gerçek key-up'lar). `cursor_auth` için in-process HTTP çağrısı sayacı ve kapalı loopback port üzerinden deterministik test eklendi. |

## Notlar

- `Cargo.toml` değişikliği gerekmedi: `sha2` zaten bağımlılık, `Devices_Radios` zaten açık.
- `crates/opentaskbar-core/tests/production_seams.rs` (sahiplik dışı) `radio::set` gövdesinin
  kaynak metnini doğruluyor; uyumluluk için `set_in_apartment(` / `cache_toggle(` /
  `refresh_async()` çağrıları bu gövdede açıkça tutuldu, test dosyasına dokunulmadı.
- Doğrulama (hepsi worktree içinde geçti): `cargo fmt --all`,
  `cargo check --workspace --all-targets`,
  `cargo clippy --workspace --all-targets -- -D warnings`,
  `cargo test --workspace` (0 başarısız).
