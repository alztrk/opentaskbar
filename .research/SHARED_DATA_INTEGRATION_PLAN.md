# Ortak Klasör / Ortak Dosya Entegrasyon Planı

Hedef: C# ve Rust **aynı `%LOCALAPPDATA%\OpenTaskbar` klasörünü ve aynı dosyaları** kullanacak. Şu an Rust iki kök kullanıyor: `OpenTaskBar-Rust` (settings/log/render-mode/icon-cache/watchdog) ve `OpenTaskbar` (journal/secrets/Themes/wireless-cache). "Test amaçlı mevcut klasör"纪实: geçiş sırasında C# klasörü referans alınacak.

Denetim kaynakları: 4 salt-okunur ajan (C# veri yolları, Rust yolları, ortak settings.json riski, operasyonel plan).

---

## Kritik bulgu (öncelik 0)

**C# `DockSettingsStore.Save` bilinmeyen anahtarları SİLER** (`JsonExtensionData` yok). Rust `#[serde(flatten)] extra` ile korur. Yani ortak `settings.json` kullanıldığında C# bir kez kaydettiğinde şu alanlar **kalıcı olarak yok olur**:

`ThemeTokens`, `AnimationFps`, `UiFontFamily/Size`, `FisheyeIntensity`, `MiddleClickAction`, `WidgetFlyoutTrigger`, `MagnificationExpandsDock`, `DockCustomWidthDips`, `Glass*`, `ShowStartButton`, `UserPinnedApps/UnpinnedApps`, `ShowFjordFilesButton`, `DynamicIslandEnable/AutoHide/Background/BackgroundColor/Opacity/UseTheme`.

Bu olmadan ortak dosya planı **başlatılamaz**. İlk iş C# tarafına `[JsonExtensionData]` eklemektir.

İkinci kritik: iki process farklı mutex kullanıyor (`OpenTaskbar.Singleton` / `OpenTaskBar-Rust.SingleInstance`) → yan yana çalışabilirler. `settings.json` yazımı "son yazan kazanır"; Rust sabit `settings.json.tmp` kullanıyor (eşzamanlı yazımda bozulma riski). Ortak short-lived kilit + GUID tmp + read-modify-write şart.

---

## Faz 1 — Merkezi yol katmanı (Rust)

Yeni `opentaskbar-core/src/paths.rs`:

```rust
pub const APP_DATA_FOLDER_NAME: &str = "OpenTaskbar";
pub fn shared_app_data_dir() -> PathBuf   // LOCALAPPDATA/OpenTaskbar, fallback temp_dir()
pub fn settings_path() -> PathBuf         // .../settings.json
pub fn rust_log_path() -> PathBuf         // .../opentaskbar-rust.log  (C# log.txt ile çakışmaz)
pub fn theme_root() -> PathBuf            // .../Themes  (büyük T)
pub fn secrets_dir() -> PathBuf           // .../secrets
pub fn watchdog_path() -> PathBuf         // .../OpenTaskbarWatchdog.exe
pub fn render_mode_path() -> PathBuf      // .../render-mode.json
pub fn rust_cache_dir() -> PathBuf        // .../rust-cache/icon-cache
pub fn journal_path(name: &str) -> PathBuf
```

Yönlendirilecek dosyalar:
- `core/settings.rs:2783-2793` (+ yedek ad + tmp ad), `core/log.rs:34-37`
- `main.rs` log çağrıları (149/169/187/207), `main.rs:110-115` Themes
- `shell/watchdog.rs:165-168`, `shell/journal.rs:17-23`, `flyout_writer.rs:63-75`, `flyout_journal.rs:114-226`
- `platform/render_mode.rs:29-31`, `icon_cache.rs:51-53`, `theme_packs.rs:1035-1041`, `cursor_auth.rs:56-65`
- `widgets/telemetry.rs:3028-3665` (DPAPI secrets)
- `ui/settings.rs:485` (küçük `themes` → `Themes`), backup yolları, log gösterimi
- `platform/wireless_battery.rs:4128`, `country_city.rs:147-155`, `media_ext.rs:647`
- `platform/update.rs:291-297` → C# ile birebir `%TEMP%\OpenTaskbar\update`

**Kapı G1:** Rust'ı açıp kapatınca `%LOCALAPPDATA%\OpenTaskbar` altında `settings.json`, `opentaskbar-rust.log`, `render-mode.json`, `Themes/`, `secrets/`, journal'lar oluşuyor; `OpenTaskBar-Rust` klasörü yeni dosya üretmiyor.

---

## Faz 2 — Ortak settings.json uyumluluğu

### C# tarafı (zorunlu)
1. `Models/DockSettings.cs` ve `DockProfile` → `[JsonExtensionData] public Dictionary<string, JsonElement> Extra { get; init; }`
2. `DockSettingsStore.Save` serializer'ı extension data'yı otomatik yazar; `SettingsBackup.Serialize` / `FullBackupArchive` aynı serializer'ı kullanır.
3. `DockSettingsStore.Load`: alan bazlı onarım (Rust `repair_widget_lists` mantığı) — tek alt-küme hatasında tüm dosyayı default yapma.
4. `Save`: write öncesi `IsValid()` → hatalıysa clamp/onar, yazmayı iptal etme.
5. Runtime dictionary'leri `OrdinalIgnoreCase`: `DynamicIslandPolicy.cs:7-13`, `WidgetCoordinator.cs:675`.
6. Widget map key'lerini canonicalize: `DynamicIslandPlacements`, `WidgetDensityOverrides`, `DockItemOrder` `widget:` token'ları.
7. `DynamicIslandEnabled` ya gerçek C# alanı olsun ya da Rust bu anahtarı yazmayı bırakıp kendi sidecar'ında tut-sun (karar: ortak dosya istendiği için **gerçek alan** eklenmeli).

### Rust tarafı
1. `save_to`: benzersiz GUID tmp (`settings.json.<guid>.tmp`), `sync_all`, `rename`; `validate()` yazmadan önce; hataları logla (`let _ = save_to(...)` yutar).
2. `dedupe_written_aliases`: çelişkide **C#-facing anahtar** kazansın (`Widgets`, `Layout`, `FocusModeEnabled`, `PinnedTrayIcons`, `ShowClock`, `ShowSystemStats`).
3. `magnification_expands_dock`: typed alan kesin öncelikli (zaten düzeltildi); `extra` bayığını migrate et.
4. `extra` üzerinden typed okuma yapan yerleri typed alanlara çevir: `dock/layout.rs` (`NotificationBadgesEnabled`), `ui/settings.rs` (`MenuOpacity/WidgetOpacity/SettingsOpacity`).
5. Ortak short-lived named mutex `Local\OpenTaskbar.Settings.Lock` + kritik bölgede **diskten taze oku → sadece sahip olunan alanları değiştir → yaz**.
6. `DynamicIslandPlacements` / `WidgetDensityOverrides` anahtarlarını `canonical_widget_setting_id` ile yaz.

### Test kapısı G2
- C# `Save` sonrası Rust-only anahtarların korunduğunu doğrulayan **gerçek C# ↔ Rust** round-trip testi.
- Rust `save_to` sonrası C# `DockSettingsStore.Load + IsValid + Save` turu.
- `{"stats":"*"}`, `{"clock":"Compact"}` map key testleri çift tarafta.
- İki process eşzamanlı yazma senaryosu (mutex + retry).

---

## Faz 3 — Paylaşılan dosya formatlarının birebirliği

| Dosya | C# formatı | Rust durumu | Yapılacak |
|---|---|---|---|
| `taskbar-state.txt` | tek uint, bit 1/2, bozuk→0, tmp+Move | Uyumlu (`journal.rs`) | Native C watchdog ile üçlü test |
| `flyout-positions.txt` | ≤16 satır `<HWND> <PID> <left> <top>`, invariant, BOM yok | Uyumlu; ayrı tmp adı (`flyout_writer` vs `flyout_journal`) | tmp adını birleştir |
| `secrets/*.bin` | DPAPI entropy `...AiSecretStore.v1` / `...CursorAuth.v1`, 64 KiB, GUID tmp | Uyumlu | Entropy/suffix testi, yedeklere girmeme testi |
| `Themes/<id>/` | büyük T, safe-id, staging `.import-<guid>`, boyut sınırları | Kök doğru; `ui/settings.rs` küçük `themes` düzeltildi | Case testi + import/export karşılıklı test |
| `wireless-battery-cache.json` | **UTF-8 BOM VAR**, PascalCase, 64 kayıt/65 KiB, 30 dk stale | Aynı ad; BOM/alan farkı kontrol edilmeli | BOM toleransı + alan eşleme testi |
| `render-mode.json` | `{"software":bool}` | Rust ayrı klasörde | Faz 1 ile taşındı |
| `opentaskbar.log` / `opentaskbar-rust.log` | C# `log.txt` + `log.old.txt`, 1 MB, BOM | Ayrı ad (öneri) | Rotasyon adı `opentaskbar-rust.log.old`; tek log istenirse ortak mutex + aynı eşik |
| `update/` | `%TEMP%\OpenTaskbar\update` | Rust LOCALAPPDATA'da | C# path'e taşı |
| `.otbackup` | manifest `Version=1`, settings `{Version,Settings,Pins}`, theme | Rust uyumlu | Secret'ların asla dahil olmadığı testi |

**Kapı G3:** C# yazan dosyayı Rust okur, Rust yazan dosyayı C# okur; watchdog (C) her iki journal'ı doğru yorumlar.

---

## Faz 4 — Operasyonel geçiş

1. **Push + tag (önce güvenlik):** `git push origin rust-port` (187 commit FF), backup tag `pre-shared-data-20261127`, mevcut `v0.3.4` korunur.
2. **master sync:** `git switch master && git pull --ff-only origin master` (9 commit).
3. **Entegrasyon PR:** `origin/master` ↔ `rust-port` → **14 çakışma**: `CHANGELOG.md`, `Widgets/AiUsageWidget.cs`, 12 dil kataloğu. Çözüm: CHANGELOG union, AI widget theirs(master), locale anahtar union + Antigravity 5 anahtarı C#'ta tut/Rust'a ekleme + `check-locale-parity.ps1` sabitleri (C# 1034 / Rust 1406). Kapı: cargo fmt/clippy/test/release build + dotnet build + headless test + locale parity.
4. **x86 watchdog:** yeni `build.rs` → `CARGO_CFG_TARGET_ARCH` ile `build.ps1 -Architecture x86 -Force -NoFallback`, `OT_WATCHDOG_EXE` env, `watchdog.rs` `include_bytes!(env!(...))`; PE machine x86=0x014C / x64=0x8664 doğrulaması; rust-ci path filtrelerine `tools/watchdog/**`, Rust scriptleri, `packaging/msix/AppxManifest.Rust.xml`, `release.yml` ekle; yeni `watchdog-arch` job'u.
5. **Sürüm kapısı:** `scripts/check-release-version.ps1` — tag ↔ csproj 0.3.5 ↔ Cargo (0.1.0 açıkça kayıt veya eşit) ↔ MSIX `0.3.5.0`; `publish-rust-msix.ps1` hardcoded 0.3.2 kaldır.
6. **Çift artifact release v0.3.5:** checksums + manifest `rustPort.crateVersion` + watchdog arch/sha alanları + SLSA/VT doğrulama; C# binary geri dönüş için kalır.

---

## Faz 5 — Canlı Windows smoke (canlı kanıt olmadan silme yok)

Ortam kaydı: OS build, commit SHA, EXE arch, monitör/DPI, `OT_VERSE_LOG=1`.

Senaryolar: Start/Search/CC anchoring, auto-hide+Alt+Tab, **x86 watchdog `taskkill /F` sonrası native taskbar**, app-dir watchdog önceliği, Wi-Fi toggle, çoklu monitör/mirror, LibreWolf seek, `.otbackup` round-trip + traversal reddi, updater Unicode path + ikinci örnek, Explorer restart (`TaskbarCreated`), checksum + attestation doğrulama.

Log yolu artık `%LOCALAPPDATA%\OpenTaskbar\opentaskbar-rust.log`; journal'lar `OpenTaskbar` klasöründe.

---

## Faz 6 — C# silme (sonra, ayrı PR'lar)

Korunur: `tools/watchdog/**`, `packaging/msix/Assets/**`, `AppxManifest.Rust.xml`, LICENSE/README/CHANGELOG/PRIVACY, `docs/**`, Rust scriptleri/CI, locale kataloğu önce `tests/reference/locales/` altına dondurulur.

Silinecek (iki dalga): WPF kaynakları/csproj/bin/obj/Design/kök Fonts/themes/C# testleri; C# publish/package betikleri; CodeQL C# job'u; dependabot nuget; release.yml C# build/asset satırları.

PR sırası: A (entegrasyon) → B (watchdog/CI) → C (v0.3.5 çift release) → smoke → D (C# silme) → E (Rust-only v0.4.0). Her PR tek commit revert ile geri alınabilir.

---

## Risk / geri dönüş (özet)

| Risk | Kapı | Geri dönüş |
|---|---|---|
| C# bilinmeyen anahtarları silmeye devam ederse Rust ayarları kaybolur | G2 gerçek round-trip testi | `[JsonExtensionData]` PR'ını revert etmeden düzelt; v0.3.5 C# binary korunur |
| İki process aynı settings.json'ı bozarsa | G2 eşzamanlı yazma testi + ortak mutex | Mutex + GUID tmp; bozuk dosya `invalid-<build>-<utc>` yedeğinden dön |
| Locale sayıları entegrasyonda güncellenmezse CI kırmızı | G1 parity script | 1034/1406 sabitlerini union sonrası güncelle |
| x86 watchdog x64 kalırsa 32-bit Windows'ta recovery ölür | G4 PE machine 0x014C + -NoFallback | build.ps1 fallback kapat; v0.3.5 ikilileri kullan |
| Tag ↔ sürüm uyuşmazlığı release'i yarım bırakır | check-release-version build öncesi | Tag push edilmeden düzelt; yayınlandıysa yama tag |
| C# silme sonrası release csproj ararsa çöker | PR-D aynı PR'da csproj kapılarını kaldırır | PR-D revert |

---

## Önerilen uygulama sırası

**Faz 1 → Faz 2 → Faz 3 → (push+tag, master sync, entegrasyon PR) → Faz 4 → Faz 5 smoke → Faz 6.**

En kritik tek nokta: **C# `[JsonExtensionData]` olmadan ortak settings.json güvenli değildir.** En yüksek maliyetli tek nokta: **entegrasyondaki 14 çakışma ve locale sayı sabitleri.**
