# Windows görev çubuğu çalışma modeli ve OpenTaskbar güvenilirliği

> Bu döküman, `docs/development/windows-reliability-audit.md` (doğrulama matrisi) ile birlikte kullanılmalıdır.
> Audit "hangi senaryoları test etmeliyiz?" sorusunu cevaplar; bu döküman "Windows gerçekte nasıl çalışıyor ve
> Rust uygulaması hangi mekanizmalara dayanıyor?" sorusunu cevaplar.
>
> Uygulama Rust-only'dır: kök workspace `Cargo.toml` + `crates/{opentaskbar-core,opentaskbar-shell,opentaskbar-app}`.
> Kaynaklar en sonda listelenmiştir. Belgelenmemiş davranışlar (`SetWindowBand`, `SHELLTRAYDATA` ofsetleri,
> Win11 `TaskbarCreated` değişimi) her Windows sürüm yükseltmesinde yeniden doğrulanmalıdır.

---

## Bölüm 1 — Windows görev çubuğu gerçekte nasıl çalışıyor?

### 1.1 `Shell_TrayWnd` ve bildirim alanı

Görünür görev çubuğu normalde `explorer.exe` tarafından sağlanan `Shell_TrayWnd` sınıfından bir penceredir.
`Shell_NotifyIcon` çizim yapmaz; SHELL32.dll içindeki bir "switchboard"dur:

1. `FindWindow("Shell_TrayWnd", ...)` ile hedef pencere bulunur (yoksa çağrı başarısız olur).
2. Parametreler **`WM_COPYDATA`** ile gönderilir. `COPYDATASTRUCT.dwData`: `0` → `SHAppBarMessage`,
   `1` → `Shell_NotifyIcon`, `2` → `SHEnableServiceObject` / `SHLoadInProc`.
3. Explorer pencere prosedürü mesajı işler ve callback mesajını uygulamanın `uCallbackMessage`
   penceresine geri gönderir. NT tabanlı sistemlerde çağrı **engellenerek** yapılır (~4 sn zaman aşımı).

Vista+ güvenlik detayı: `NIF_MESSAGE` verildiğinde `uCallbackMessage` numarası çağıran process'in mesaj
filtresine eklenir; böylece daha yüksek integrity seviyesindeki Explorer'dan gelen geri çağrı daha düşük
seviyeli istemciye ulaşabilir (UIPI).

Üçüncü parti dock'ların kullandığı iki yaklaşım:

- **Spy window:** `Shell_TrayWnd` sınıfıyla gizli pencere açıp aynı `WM_COPYDATA` mesajlarını yakalar ve
  gerçek Explorer penceresine **forward eder** (etmezse sistem tepsisi bozulur).
- **DLL injection + subclass:** `SetWindowLongPtrW` ile window proc değiştirilir, veri named pipe ile
  dışarı aktarılır. Daha fazla kontrol sağlar ama AV/EDR false positive ve sürüm bağımlılığı riski taşır;
  pipe yazımı `overlapped I/O` + üst sınır gerektirir.

### 1.2 `Shell_NotifyIcon` yaşam döngüsü

- Vista+ için `cbSize = sizeof(NOTIFYICONDATA)`; `NIM_ADD`'den hemen sonra **`NIM_SETVERSION(NOTIFYICON_VERSION_4)`** çağrılmalıdır.
- Kimlik: `hWnd + uID` (klasik) veya **`guidItem` + `NIF_GUID`** (Win7+ önerilen). GUID, exe yolu
  değişse bile tek kaydı korur; MSIX/güncelleme/taşıma sonrası "promoted" seçimi kaybolmaz.
- v4 callback semantiği gerçek Windows mesajı değildir; `uCallbackMessage` mesajının `lParam` içinde taşınır:
  `LOWORD(lParam)` olay (`NIN_*`, `WM_CONTEXTMENU`), `HIWORD(lParam)` ikon ID'si (16 bit),
  `GET_X/Y_LPARAM(wParam)` **signed** koordinatlar (negatif değerler sol/üst monitör için normaldir).
- Aynı fiziksel gesture için hem fare mesajı hem `NIN_SELECT` gelir; tıklama soyutlaması **dedupe** etmelidir.

### 1.3 Foreground izni alınamaz, verilir

`SetForegroundWindow` yalnızca masaüstü uygulaması, foreground kilidi yok, aktif menü yok **ve** çağıranın
zaten foreground olması / son girdiyi alması / foreground yokluğu gibi koşullarda başarılı olur.
Dock, ikonun penceresine `SendNotifyMessage(WM_LBUTTONUP)` gönderdiğinde girdiyi alan process dock'dır;
hedef uygulama öne çıkamaz ve görev çubuğu flaşlar. Doğru yaklaşım:
`GetWindowThreadProcessId` → **`AllowSetForegroundWindow(pid)`** → mesajları gönder → yanıt vermeyen
"zombie" ikonlarda `ms-settings:` URI veya exe yolu fallback'i. `AttachThreadInput` kötü bir çözümdür.

### 1.4 AppBar ve çalışma alanı protokolü

Doğru sıra: `ABM_NEW` (callback mesajıyla) → `ABM_QUERYPOS` (Tam EKRAN koordinatı, work area DEĞİL)
→ sabit kenarı yeniden uygula → `ABM_SETPOS` → dönen `rc` ile `MoveWindow` → `ABM_WINDOWPOSCHANGED` /
`ABM_ACTIVATE`.

Sık yapılan hatalar: QUERYPOS atlanması, SETPOS'un döndürdüğü `rc`'nin kullanılmaması, work area
koordinatı verilmesi (çift indentation), `SPI_GETWORKAREA`'nın yalnızca birincil monitörü vermesi,
`SPI_SETWORKAREA + SPIF_SENDCHANGE` senkron broadcast'in ~3,5 sn bloklaması (yerine `PostMessage`),
`ABN_FULLSCREENAPP` / `ABN_POSCHANGED` yanıtlarının atlanması ve aynı kenarda birden fazla autohide appbar.
Değer değişmediyse `SetWindowPos` çağrısını atlamak gerekir; aksi halde geri besleme döngüsü oluşur.

### 1.5 Explorer restart ve kabuk sinyalleri

- **`TaskbarCreated`:** Explorer görev çubuğunu oluşturunca tüm üst seviye pencerelere broadcast edilir.
  Alıcı tray ikonlarını, AppBar kaydını, shell hook'larını, work area'yı ve gizleme politikasını yeniden kurmalıdır.
  PowerToys deneyimi: Explorer hazır olmadığında ilk `NIM_ADD` başarısız olabilir → `WM_WINDOWPOSCHANGING`
  içinde yeniden deneme gerekir. Win10'da DPI değişiminde de gelir; Win11'de `WM_DPICHANGED` /
  `WM_DISPLAYCHANGE` ayrıca izlenmelidir.
- **`WH_SHELL` / `RegisterShellHookWindow`:** `HSHELL_WINDOWCREATED/DESTROYED/ACTIVATED`,
  `HSHELL_MONITORCHANGED`, `HSHELL_REDRAW`, `HSHELL_FLASH`; mesaj `RegisterWindowMessage("SHELLHOOK")`.
- **Cloaking:** `IsWindowVisible` tek başına yeterli değildir; `DwmGetWindowAttribute(DWMWA_CLOAKED)` gerekir.
- **Win11 z-order:** tek seferlik `SetWindowPos(HWND_TOPMOST)` yetmez; Start/Action Center/fullscreen
  geçişlerinde Explorer z-order'ı yeniden dayatır; foreground olaylarında topmost yeniden uygulanmalıdır.

---

## Bölüm 2 — Rust uygulamasının haritası

| Windows mekanizması | Rust koddaki yeri | Davranış |
| :--- | :--- | :--- |
| Komut satırı modları | `crates/opentaskbar-app/src/main.rs` | *(yok)* dock, `--settings`, `--restore`, `--extract-icons`, `--update*` / `action=update*`, `--software-render`; tek örnek kilidi |
| Görev çubuğu gizle/restore, AppBar | `crates/opentaskbar-shell/src/taskbar.rs`, `appbar.rs` | `SHAppBarMessage` state/reservation, doğrulamalı `SetState`, ikincil taskbar'ları da kontrol eden görünürlük testi |
| Kurtarma journal'ı | `crates/opentaskbar-shell/src/journal.rs` | `taskbar-state.txt`, yalnızca `ABS_AUTOHIDE \| ABS_ALWAYSONTOP` bitleri, bozuk/eski değer reddi, atomic `*.tmp` yazımı |
| Flyout journal'ı | `crates/opentaskbar-shell/src/flyout_journal.rs`, `flyout_reposition.rs`, `flyout_writer.rs` | Start/Search/Control Center konumları, generation counter, HWND/PID doğrulaması, `native_flyout_reposition` tek anahtarı |
| Watchdog (2. süreç) | `crates/opentaskbar-shell/src/watchdog.rs`, `tools/watchdog/watchdog.c`, `build.rs` | Yerleşik C PE, `--watchdog=<pid>:<token>`, 1 sn heartbeat, 4 sn × 3 kaçırma, clean-exit event'i, x64 `0x8664` / x86 `0x014C` |
| Shell oturumu / `TaskbarCreated` | `crates/opentaskbar-shell/src/session.rs` | Kayıtlı mesaj, `wParam` kaynak etiketi (self/restore), idempotent yeniden kurulum |
| Pencere takibi ve kuralları | `crates/opentaskbar-shell/src/tracker.rs`, `window_rules.rs` | Taskbar düğmesi olan gerçek uygulama pencereleri, cloak/hidden/sahip filtreleri |
| Auto-hide / fullscreen | `crates/opentaskbar-app/src/dock/mod.rs`, `dock/layout.rs`, `platform/win32.rs` | Per-monitör 500 ms fullscreen taraması, shell overlay filtresi, flyout açıkken görünür kalma |
| GPUI dock ve ayarlar | `crates/opentaskbar-app/src/dock/`, `ui/`, `widgets/` | Fisheye, sürükle-bırak, 22 katalog widget'ı, on-demand Settings penceresi |
| Paylaşılan veri | `crates/opentaskbar-core/src/paths.rs` | `%LOCALAPPDATA%\OpenTaskbar`: `settings.json`, `opentaskbar.log`, `Themes\`, `secrets\`, journal'lar |
| Deterministik kontroller | `crates/opentaskbar-core/tests/` | `shell_recovery.rs`, `production_seams.rs`, `i18n_parity.rs`, `dock_layout_extra.rs`, diğer sözleşme testleri |

**Genelleştirilmiş iyi uygulamalar:** HWND'yi kullanmadan hemen önce doğrula (PID + generation),
asenkron işlemin sonucunu bekle/doğrula, journal'ı yalnızca görünür taskbar doğrulandıktan sonra sil,
kendi yayınladığın kabuk mesajını etiketle, ve tüm restore yollarını tek koordinatöre bağla.

---

## Bölüm 3 — Neden tekrar tekrar aynı sınıf hatalar çıkıyor?

Üç yaygın görünüm aynı kök sorunun farklı yüzleridir:

| Görünüm | Gerçek kök neden |
| :--- | :--- |
| Dock'a sabitlenen uygulama tıklanınca açılmıyor | Sentetik mesaj foreground izni olmadan gönderiliyor; kayıtlı kimlik ile canlı HWND/GUID karışıyor |
| Gizli/ardplan uygulaması normal çalışan uygulama gibi görünüyor | Pin niyeti ile canlı pencere durumu birbirine karıştırılıyor; filtre kuralları tek kaynaktan gelmiyor |
| Kaydırma başa dönüyor | Ölçüm → magnifier reset → ertelenmiş resize → reset geri besleme zinciri; "taşma yok" kararı yanlış geometriden çıkıyor |

Ortak kalıplar:

1. **Birden fazla doğruluk kaynağı:** kabuk durumu + journal dosyaları + kayıt/pin verisi + canlı pencere listesi.
2. **Kayıtlı niyet ile canlı durum karışıyor:** kimlik yeniden üretilip strateji ona göre seçiliyor, GUID/path eşleşmesi kırılıyor.
3. **Asenkron işlem doğrulanmıyor:** "gönderdim, oldu" varsayımı.
4. **Yeniden giriş bayraklarla kırılıyor:** gerçek bir durum makinesi yerine bool yığını.
5. **Sessiz bozulma:** hata log'a yazılmadan yutuluyor; kullanıcı hiçbir şey görmüyor.

**Ders:** Bu hatalar tek satır eksikliği değil; **tek doğruluk kaynağı + açık durum makinesi +
doğrulanmış asenkron** disiplininin eksikliğidir. Rust uygulamasında bu disiplin `paths.rs` (tek veri kökü),
`journal.rs` (tek kurtarma durumu), `session.rs` (tek shell oturumu) ve `watchdog.rs` (tek süreç sahibi)
etrafında toplanmıştır.

---

## Bölüm 4 — Mevcut sağlamlaştırma kontrolleri

### Kurtarma (P0)

- **Başlangıç:** `main.rs`, tek örnek kilidini aldıktan sonra bekleyen journal'ı onarır; watchdog arm edilir,
  dock `catch_unwind` ile çalıştırılır; panik halinde journal'dan restore yapılır.
- **Journal:** `journal.rs::effective_state` bozuk değeri reddeder, görünür taskbar'da eski journal'ı
  yok sayar ve yalnızca desteklenen bitleri uygular. `taskbar-state.txt` atomic `*.tmp` + rename ile yazılır.
- **Watchdog:** ayrı process, 1 sn heartbeat, üç kaçırmadan sonra restore, clean-exit event'i ile susturulur.
  `-NoFallback` ile yanlış mimari PE asla "geçerli" sayılmaz.
- **Manual:** `OpenTaskbar.exe --restore` dock çalışmasa da journal'ı oynatır.
- **Flyout'lar:** `flyout_journal.rs` özgün konumları kaydeder; `flyout_reposition.rs` generation ile
  eski açılışları iptal eder; `taskbar.rs` kendi `TaskbarCreated` yayınını `wParam` etiketiyle ayırt eder.

### Auto-hide ve fullscreen (P1)

- `dock/mod.rs::evaluate_auto_hide` per-monitör fullscreen durumunu 500 ms aralıklarla tarar; yalnızca
  gerçek taskbar düğmesi olan pencereler sayılır, shell overlay'leri (masaüstü, IME, GPU katmanları) sayılmaz.
- Task View / Alt-Tab / flyout açıkken dock görünür kalır; ikincil dock yalnızca kendi monitöründeki
  fullscreen uygulama için gizlenir.

### Doğruluk ve sınırlar (P2)

- Yazılar `Local\OpenTaskbar.Settings.Lock` ile serileştirilir; GUID temp dosya + rename kullanılır.
- Yüzde ve aralıklar `DockSettings::validate()` ile sınırlanır; AI sağlayıcı bit maskeleri ve widget ID'leri
  yükleyişte kanonikleştirilir.
- Tema/backup içe aktarımı absolute path ve `..` traversal'ını reddeder; token sayısı 256 ile sınırlıdır.

---

## Bölüm 5 — Kaynaklar

**Notification area / Shell_NotifyIcon**
- https://learn.microsoft.com/en-us/windows/win32/api/shellapi/nf-shellapi-shell_notifyiconw
- https://learn.microsoft.com/en-us/windows/win32/api/shellapi/ns-shellapi-notifyicondataa
- https://learn.microsoft.com/en-us/windows/win32/shell/notification-area
- https://geoffchappell.com/studies/windows/shell/shell32/api/shlnot/copydata.htm
- https://github.com/microsoft/windows-rs/issues/5284 (GUID vs ExecutablePath, MSIX)
- https://github.com/microsoft/microsoft-ui-reactor/issues/1180 (çift olay dedupe)
- https://github.com/glzr-io/zebar/commit/50bee67551af104b4ff9b444db8aef911778245b (AllowSetForegroundWindow + tıklama sırası)

**Foreground / aktivasyon**
- https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-setforegroundwindow
- https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-allowsetforegroundwindow
- https://devblogs.microsoft.com/oldnewthing/20090220-00?p=19083
- https://learn.microsoft.com/en-us/windows/win32/api/dwmapi/nf-dwmapi-dwmgetwindowattribute

**AppBar / work area / kabuk olayları**
- https://learn.microsoft.com/en-us/windows/win32/shell/application-desktop-toolbars
- https://learn.microsoft.com/en-us/windows/win32/api/shellapi/nf-shellapi-shappbarmessage
- https://learn.microsoft.com/en-us/windows/win32/shell/taskbar
- https://github.com/microsoft/PowerToys/blob/master/src/runner/tray_icon.cpp
- https://github.com/valinet/ExplorerPatcher/pull/5118 (work-area watchdog, debounce)
- https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-registershellhookwindow

---

## Bölüm 6 — Doğrulama ve kanıt

Tüm komutlar **kök workspace'ten** çalıştırılır; canlı kabuğu değiştiren bir test yoktur.
Deterministik testler Explorer'ı kapatıp açmaz, `TaskbarCreated` yayınlamaz ve HKCU'ya yazmaz.

```powershell
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo check --workspace

powershell -NoProfile -ExecutionPolicy Bypass -File .\scripts\check-locale-parity.ps1
powershell -NoProfile -ExecutionPolicy Bypass -File .\scripts\check-release-version.ps1 -Tag v0.3.5
```

| Kontrol | Ne doğrular |
| :--- | :--- |
| `crates/opentaskbar-core/tests/shell_recovery.rs` | Journal satır ayrıştırma, retry/backoff, watchdog argümanları, `finally` modifier semantiği, 3 denemeli restore doğrulaması |
| `crates/opentaskbar-core/tests/dock_layout_extra.rs` | Yay fiziği, raised-cosine falloff, overlap guard, gap/height çözümü, sabitlenmiş grup sırası |
| `crates/opentaskbar-core/tests/i18n_parity.rs`, `telemetry_parity.rs` | 14 × 1375 Rust anahtarı, çeviri regresyonları, yer tutucu korunması |
| `crates/opentaskbar-shell/src/journal.rs` (unit) | Bozuk/eski journal, `*.tmp` atomic yazım, desteklenen bit maskesi |
| `crates/opentaskbar-shell/src/watchdog.rs` (unit) | `--watchdog` argümanı, heartbeat token'ı, clean-exit sinyali |
| `scripts/check-locale-parity.ps1` | 14 × 1046 donmuş referans + 14 × 1375 Rust anahtarı, gerekli sözleşme anahtarları |
| `scripts/check-release-version.ps1` | Tag ↔ Cargo workspace `0.3.5` ↔ MSIX `0.3.5.0` |

### Canlı smoke kontrol listesi

Her satır gerçek Windows oturumunda çalıştırılmalı; başlangıç durumu **not run**'dur.
Ayrıntılı senaryolar ve pass koşulları `docs/development/windows-reliability-audit.md` içindedir.

| # | Senaryo | Kısa adım |
| :--- | :--- | :--- |
| S1 | Start/flyout anchoring | Start 10× hızlı aç/kapat, Search/CC, Explorer restart |
| S2 | Auto-hide ve shell overlay | Kenar hover, Task View/Alt-Tab, fullscreen uygulama, geri dönüş |
| S3 | Watchdog ve temiz çıkış (x64 + x86) | `taskkill /F` ve normal kapatma; PE machine doğrulaması |
| S4 | Radyo anahtarları | Wi-Fi/Bluetooth, airplane mode açık/kapalı |
| S5 | Çoklu monitör | Dock'u taşı, ikincil ekranı çıkar/tak, negatif sanal koordinat |
| S6 | Night Light ve medya | Registry payload, LibreWolf zaman çizelgesi, seek clamp |
| S7 | Tam yedekleme | Dışa/içe aktarım, traversal ve `.exe` girişleri |
| S8 | Güncelleyici ve tek örnek | Unicode `%` içeren yol, ikinci dock process'i |

### Kanıt formatı

Her vaka için şunu kaydet: vaka kimliği, commit, OS build, EXE mimarisi, monitör topolojisi/DPI,
atılan adımlar, beklenen/gerçek sonuç, ekran görüntüsü/video yolu,
`%LOCALAPPDATA%\OpenTaskbar\opentaskbar.log` excerpt'i ve varsa CPU/bellek/handle ölçümleri.
Statik inceleme veya simülasyon tek başına **geçerli kanıt değildir**; yapılamayan test **not run** olarak işaretlenir.
Kaynak seviyesindeki bir testin geçmesi canlı Windows uyumluluğunu kanıtlamaz; her Windows sürüm
yükseltmesinde §1'deki belgelenmemiş davranışlar yeniden doğrulanmalıdır.
