# Windows taskbar çalışma modeli ve OpenTaskbar sağlamlaştırma planı

> Bu döküman, `docs/windows-reliability-audit.md` (doğrulama matrisi) ile birlikte kullanılmalıdır.
> Audit "hangi senaryoları test etmeliyiz?" sorusunu cevaplar; bu döküman "Windows gerçekte nasıl çalışıyor ve
> biz neden tekrar tekrar aynı sınıf hataları üretiyoruz?" sorusunu cevaplar.
>
> Kaynaklar en sonda listelenmiştir. Dokümante edilmemiş davranışlar (`SetWindowBand`, `SHELLTRAYDATA`
> alan ofsetleri, Win11 `TaskbarCreated` değişimi) her Windows sürüm yükseltmesinde yeniden doğrulanmalıdır.

---

## Bölüm 1 — Windows görev çubuğu gerçekte nasıl çalışıyor?

### 1.1 `Shell_TrayWnd` ve bildirim alanının taşıma katmanı

Görünür görev çubuğu `Shell_TrayWnd` sınıfından bir penceredir ve normalde **explorer.exe** tarafından sağlanır.
`Shell_NotifyIcon` çizim yapmaz; SHELL32.dll içindeki bir "switchboard"dur:

1. `FindWindow("Shell_TrayWnd", ...)` ile hedef pencereyi bulur (bulunamazsa doğrudan başarısız olur).
2. Parametreleri **`WM_COPYDATA`** ile gönderir. `COPYDATASTRUCT.dwData`:
   - `0` → `SHAppBarMessage`
   - `1` → `Shell_NotifyIcon`
   - `2` → `SHEnableServiceObject` / `SHLoadInProc`
3. Explorer'ın pencere prosedürü mesajı işler ve callback mesajını uygulamanın `uCallbackMessage`
   penceresine geri gönderir. NT tabanlı sistemlerde çağrı **engellenerek** yapılır; ~4 sn (Vista'da 7 sn)
   zaman aşımı vardır.

Vista+ güvenlik detayı: `NIF_MESSAGE` verildiğinde `uCallbackMessage` numarası **çağrı yapan process'in
mesaj filtresine eklenir**; böylece daha yüksek integrity seviyesindeki Explorer'dan gelen geri çağrı
daha düşük seviyeli istemciye ulaşabilir (UIPI).

Üçüncü parti dock'ların (ManagedShell, zebar, yasb) kullandığı iki yaklaşım:

- **Spy window:** `Shell_TrayWnd` sınıf adıyla gizli bir pencere açıp aynı `WM_COPYDATA` mesajlarını yakalar,
  sonra gerçek Explorer penceresine **forward eder** (etmezse sistem tepsisi bozulur).
- **Explorer DLL injection + subclass:** `SetWindowLongPtrW` ile `Shell_TrayWnd` window proc'u değiştirilir,
  veri named pipe ile dışarı aktarılır. Daha fazla control sağlar ama AV/EDR false positive, sürüm
  bağımlılığı ve process'i dondurma riski taşır. Pipe yazımı **overlapped I/O + 500 ms üst sınır** ile
  yapılmalıdır; aksi halde dock'un WriteFile bloklaması tüm görev çubuğunu dondurur.

### 1.2 `Shell_NotifyIcon` yaşam döngüsü

**Sürüm / `cbSize`:** Vista+ için `cbSize = sizeof(NOTIFYICONDATA)`, XP `NOTIFYICONDATA_V3_SIZE (504)`,
Win2000 `V2_SIZE (488)`, daha eskisi `V1_SIZE (88)`. `NIM_ADD`'den hemen sonra **`NIM_SETVERSION(NOTIFYICON_VERSION_4)`**
çağrılmalıdır (oturum kapanınca kalıcı değildir, `NIM_MODIFY` ile gerekmez).

**Kimlik:** `hWnd + uID` (klasik) veya **`guidItem` + `NIF_GUID`** (Win7+ önerilen).

| Kimlik | Dayanak | Kırılganlık |
|---|---|---|
| `hWnd + uID` | Process HWND'si + sayısal ID | HWND her başlatmada değişir; uID çakışabilir |
| `ExecutablePath + uID` (Shell'in sakladığı) | Dosya yolu | MSIX/güncelleme/taşıma → yeni kayıt, `IsPromoted` kaybı |
| `guidItem` | Uygulamanın sabit 128-bit GUID'i | Exe yolu değişse bile tek kayıt korunur; en kararlı kimlik |

GUID'in kritik önemi: bir MSIX uygulaması her sürümde `...\1.2.3.0_...` klasörüne kurulduğunda Shell,
`ExecutablePath + uID` ile eşleştirdiği için her güncellemede yeni bir ikon sanır ve kullanıcının "göster"
seçimini kaybeder. GUID ile Shell tek kaydı günceller.

**Komutlar:** `NIM_ADD → NIM_MODIFY / NIM_DELETE`, `NIM_SETFOCUS` (Shell32 bu durumda taskbar
sürecine açıkça `AllowSetForegroundWindow` çağırır), `NIM_SETVERSION`.

### 1.3 v4 callback semantiği ve tıklama sırası — en çok karıştırılan kısım

Bunlar gerçek Windows mesajları **değildir**; `uCallbackMessage` ile belirtilen mesajın `lParam` içinde taşınır:

```
NIN_SELECT=0x400, NIN_KEYSELECT=0x401, NIN_BALLOONSHOW..USERCLICK=0x402..0x405,
NIN_POPUPOPEN=0x406, NIN_POPUPCLOSE=0x407, WM_CONTEXTMENU=0x007B
```

**v0–v3:** `wParam = uID`, `lParam = WM_LBUTTONDOWN/UP/DBLCLK...` (fare mesajı numarası).
**Koordinat gelmez** — `GetCursorPos()` gerekir.

**v4 (doğru modern yorum):**
- `LOWORD(lParam)` = olay (`NIN_*` veya `WM_CONTEXTMENU`)
- `HIWORD(lParam)` = ikon ID'si (**16 bit ile sınırlı**; GUID kullanıldığında anlamsız)
- `GET_X_LPARAM(wParam)` / `GET_Y_LPARAM(wParam)` = **signed** koordinatlar (negatif değerler sol/üst
  monitör için normaldir; `LOWORD/HIWORD` kullanmak çoklu monitörde yanlış konum verir)

| Kullanıcı eylemi | Uygulamaya gelen sıra |
|---|---|
| Sol tık | `WM_LBUTTONDOWN → WM_LBUTTONUP → NIN_SELECT` |
| Sol çift tık | `DOWN → UP → WM_LBUTTONDBLCLK → UP` (+ `NIN_SELECT`) |
| Sağ tık | `WM_RBUTTONDOWN → WM_RBUTTONUP → WM_CONTEXTMENU` |
| Klavye Space/Enter | `NIN_KEYSELECT` |
| Klavye Menu / Shift+F10 | `WM_CONTEXTMENU` (koordinat `-1,-1` olabilir) |
| Hover | `NIN_POPUPOPEN / NIN_POPUPCLOSE` (`NIF_SHOWTIP` verilmezse standart tooltip bastırılır) |

**Kritik sonuç:** aynı fiziksel gesture için hem mouse mesajı hem `NIN_SELECT` gelir. İkisini de "Click"
sayan istemciler tek tıkta **çift olay** üretir (microsoft-ui-reactor #1180'de gerçek bir bug). Tıklama
soyutlaması mutlaka **dedupe** etmelidir.

### 1.4 Foreground izni alınamaz, verilir

`SetForegroundWindow` yalnızca şu durumlarda başarılı olur: masaüstü uygulaması (UWP değil), foreground
kilitli değil, aktif menü yok, **ve** en az biri: çağıran zaten foreground / foreground tarafından
başlatıldı / son girdiyi çağıran aldı / foreground yok / debug ediliyor.

Raymond Chen: *"Foreground activation permission is like love: you can't steal it, it has to be given to you."*

Bir dock, ikonun penceresine `SendNotifyMessage(WM_LBUTTONUP)` gönderdiğinde **girdiyi alan process dock'dır**,
hedef uygulama değildir. Uygulama `SetForegroundWindow` çağırırsa koşullar sağlanmaz → pencere arka planda
kalır, görev çubuğu **flaşlar**. Bu, "sabitlenen Steam ikonuna tıklayınca açılmıyor" şikayetinin tam
mekanizmasıdır.

Doğru yaklaşım (zebar, komorebi):
1. `GetWindowThreadProcessId(icon.HWnd, &pid)`
2. **`AllowSetForegroundWindow(pid)`**
3. Sonra mesajları gönder

Buna rağmen bazı "zombie" ikonlar (SecurityHealthSystray, microphone vb.) hiç yanıt vermez → `ms-settings:`
URI'si veya exe yolu ile **fallback** gerekir. Ayrıca `SendMessageTimeout(hwnd, WM_NULL, ..., 5000)` ile
asenkron aktivasyon doğrulanabilir; `AttachThreadInput` kötü bir çözümdür (hedef yanıt vermezse siz de
kilitlenirsiniz).

### 1.5 Tepsi metadata'sı nerede tutuluyor?

- **`HKCU\Control Panel\NotifyIconSettings`** (Vista+): her ikon için Windows üretimi anahtar;
  `ExecutablePath`, `IsPromoted` (1 = görünür/promoted, 0 = overflow), GUID. Win11 Ayarlar'daki "Other
  system tray icons" listesi buradan beslenir; `IsPromoted` değişikliği Explorer restart gerektirmez.
- **`...\TrayNotify\IconStreams`** (klasik): 20 bayt header + 1640 bayt kayıtlar; exe yolu ROT13 ile
  obfuscate, görünürlük baytı 0/1/2. Explorer bunu açılışta belleğe okur, kapanışta yazar → çalışma
  anındaki registry değişiklikleri **üzerine yazılabilir**.
- Win11'de taskbar XAML ile yeniden yazıldı; eski `ToolbarWindow32` / `TB_BUTTONCOUNT` yöntemleri çalışmaz;
  `IsPromoted` şeması da yenilendi. Network ve volume ikonları artık sabit, gizlenemez.

### 1.6 AppBar ve çalışma alanı protokolü

Doğru sıra (Microsoft klasik örneği):

```
ABM_NEW (uCallbackMessage ile; diğer her mesajdan önce)
→ ABM_QUERYPOS (Tam EKRAN koordinatı verilir, work area DEĞİL!)
→ sistem rc'yi "dikdörtgen çıkarma" ile değiştirir → sabit kenarı yeniden uygula
   (ör. ABE_BOTTOM: rc.top = rc.bottom - height)
→ ABM_SETPOS → dönen rc ile MoveWindow
→ ABM_WINDOWPOSCHANGED, WM_ACTIVATE → ABM_ACTIVATE
```

Sık yapılan hatalar:
- QUERYPOS atlanırsa sistem görev çubuğunun üstüne biner.
- SETPOS'un **döndürdüğü** `rc` kullanılmazsa bar ezilir / yanlış yerde kalır.
- Work area koordinatı verilirse "çift indentation" olur (hem siz hem QUERYPOS aynı payı düşer).
- `SPI_GETWORKAREA` yalnızca birincil monitörü verir; çoklu monitörde `GetMonitorInfo().rcWork` şart.
- `SPI_SETWORKAREA` + `SPIF_SENDCHANGE` senkron broadcast **~3.5 sn bloklayabilir**; yerine
  `PostMessage(HWND_BROADCAST, WM_SETTINGCHANGE)` önerilir (ExplorerPatcher #5118).
- `ABN_FULLSCREENAPP` açılışta barı `HWND_BOTTOM`'a, kapanışta `HWND_TOPMOST`'a almalı.
- `ABN_POSCHANGED` gelince yeniden QUERYPOS+SETPOS yapılmalı; yapılmazsa eski yerde kalır.
- Her ekran kenarı için **yalnızca bir** autohide appbar vardır; önce `ABM_GETAUTOHIDEBAR` kontrolü gerekir.

Work-area geri besleme döngüsü: `SPI_SETWORKAREA → WM_SETTINGCHANGE → ABN_POSCHANGED → ABM_SETPOS →
SPI_SETWORKAREA...`. Değer değişmediyse `SetWindowPos` çağrısını **atlamak** gerekir.

### 1.7 Explorer restart ve diğer kabuk sinyalleri

- **`TaskbarCreated`**: Explorer görev çubuğunu oluşturunca `RegisterWindowMessage("TaskbarCreated")`
  ile tüm üst seviye pencerelere broadcast eder. Alıcı şunları **yeniden kurmalıdır**: tray ikonları
  (`NIM_ADD + NIM_SETVERSION`), AppBar kaydı (`ABM_NEW → QUERYPOS → SETPOS`), shell hook'ları, work area,
  native taskbar gizleme politikası.
  - PowerToys deneyimi: Explorer hazır olmadığında ilk `NIM_ADD` **başarısız olabilir** ve uygulama bir
    daha `TaskbarCreated` almayabilir → `WM_WINDOWPOSCHANGING` içinde yeniden deneme gerekir.
  - Win10'da birincil monitör DPI'sı değişince de gelir; Win11'de bu davranış kaldırılmış olabilir →
    `WM_DPICHANGED` / `WM_DISPLAYCHANGE` ayrıca izlenmelidir.
- **`WH_SHELL` / `RegisterShellHookWindow`**: `HSHELL_WINDOWCREATED/DESTROYED/ACTIVATED`,
  `HSHELL_MONITORCHANGED`, `HSHELL_REDRAW`, `HSHELL_FLASH`. Explorer `RegisterShellHookWindow` kullanır;
  mesaj `RegisterWindowMessage("SHELLHOOK")` ile gelir. Genel kullanım için değildir, değiştirilebilir.
- **`SHChangeNotify`**: `SHCNE_ASSOCCHANGED` ikon önbelleğini geçersiz kılar; `SHCNE_UPDATEITEM` tek öğe
  için daha uygundur (Old New Thing: "evi yıkıp yeniden yapmak" yerine tek tuğla değiştir).
- **Cloaking**: `IsWindowVisible` tek başına yeterli değildir. `DwmGetWindowAttribute(DWMWA_CLOAKED)` ile
  `DWM_CLOAKED_APP/SHELL/INHERITED` kontrol edilmelidir.
- **Win11 z-order**: tek seferlik `SetWindowPos(HWND_TOPMOST)` yetmez; Start/Action Center/fullscreen
  geçişlerinde Explorer z-order'ı yeniden dayatır. Windhawk çözümü: her `EVENT_SYSTEM_FOREGROUND`'da
  topmost'i yeniden dayat + `WM_WINDOWPOSCHANGING` içinde `hwndInsertAfter`'ı `HWND_TOPMOST` yap.

---

## Bölüm 2 — Bizim uygulamamızın haritası

| Windows mekanizması | Koddaki yeri | Durum |
|---|---|---|
| `Shell_NotifyIcon` (ManagedShell) | `Services/Interop/TrayHost.cs:50-97`, `AppIcons()` `86-87`, `Click()` `90-97` | Spy-window modeli; `Click()` zaman aşımsız, `AllowSetForegroundWindow` yok |
| AppBar / work area | `Services/Interop/TaskbarReplacementSession.cs:353-360, 735-800, 1230-1261`, ABN_* `425-471` | `SHAppBarMessage` **senkron, zaman aşımsız** (`1435`); döngü kırma `bool` bayraklarla |
| AppBar forward (iyi örnek) | `TrayHost.cs:121-222` | `APPBARDATAV2` → Explorer PID, `SendMessageTimeout(..., 1000 ms)` — tek disiplinli çağrı |
| `TaskbarCreated` | `MainWindow.xaml.cs:307, 599-607`; `TaskbarReplacementSession.cs:382-423`; yayın `TrayHost.cs:228-232` | Yayın asenkron, `IsBroadcasting` hemen sıfırlanıyor → kendi mesajımızı "Explorer restart" sanabiliyoruz |
| Journal / kurtarma | `TaskbarReplacementSession.cs:1288-1354`, `218-255`, `506-548`; `ShellFlyoutPositionRegistry.cs` | Journal doğrulamadan sonra siliniyor (iyi); flyout journal'ı recycled handle'da silinmiyor |
| Watchdog (2. süreç) | `Services/App/TaskbarRecoveryWatchdog.cs`; `App.xaml.cs:30-45, 203-296` | 1 sn heartbeat, 4 sn timeout × 3 miss; **`AppLog.Initialize()` öncesinde çalışıyor** → log düşüyor |
| WinEvent / hook | `Services/Interop/NativeWindowTracker.cs:146-158, 234-258` | **Doğru örnek**: try/catch + `Dispatcher.InvokeAsync` |
| Registry anlık görüntüsü | `Services/Interop/TrayIcons.cs:33-117, 153-302` | Registry watcher yok, tooltip'e göre dedup, tek seferlik enumerasyon |
| Pin kimliği | `Services/Interop/TrayPinIdentity.cs`, `Views/Shell/PinnedTrayStrip.cs`, `TrayShellButton.cs` | GUID/path ayrımı var ama strateji kaydı pin anahtarından türetmeli (bkz. §3) |
| Kaydırma / yerleşim | `Views/DockScrollViewer.cs`, `MainWindow.Magnification.cs`, `MainWindow.Resizing.cs` | `ScrollChanged → reset → timer → resize → ResetScroll` geri besleme zinciri |

**Güçlü yönlerimizi genelleştirmeliyiz:** `NativeWindowTracker`'ın hook → dispatcher marshal + try/catch
deseni, `TrayHost.ForwardSharedAppBarMessage`'deki `SendMessageTimeout`, `ShellFlyoutPositionRegistry`'deki
lock + `volatile Generation` + PID kontrolü ve journal'ın doğrulamadan sonra silinmesi.

---

## Bölüm 3 — Neden tekrar tekrar aynı sınıf hataları çıkıyor?

Son üç düzeltme aslında tek bir kök sorunun üç farklı görünümüydü:

| Görüntü | Gerçek kök neden |
|---|---|
| Steam pin'ine tıklayınca açılmıyor | Tıklama stratejisi sentetik tray mesajına dayanıyordu; foreground izni olmadığı için uygulama penceresini açamıyordu. Ayrıca kayıtlı `path:` pin, canlı ikonun GUID'i yüzünden `guid:` sanılıyordu. |
| Discord ikonu dock'ta görünmüyor | Pin registry'den `path:` olarak kaydediliyor ama strip yalnızca canlı `NotifyIcon`'lardan seçim yapıyordu; gizli/olmayan ikon → eşleşme yok. |
| Kaydırma başa dönüyor | `ScrollChanged → magnifier reset → ertelenmiş ResizeDockToContent → FitDockToContent` zinciri; full-width modda `appsContentWidth = 0` olduğu için "taşma yok" kararı yanlıştı. |

Ortak kalıplar:

1. **Dört ayrı doğruluk kaynağı:** Win32 kabuk durumu + journal dosyaları + registry anlık görüntüsü +
   ManagedShell canlı koleksiyonu. Bunların senkronizasyonu `bool` bayrak, `DispatcherTimer` ve
   fire-and-forget `async` ile yapılıyor.
2. **Kayıtlı niyet ile canlı durum karışıyor:** Pin anahtarını yeniden üretip stratejiyi ona göre seçmek
   (Steam hatası), tooltip'e göre dedup (aynı isimli iki uygulama), `ExecutablePath` eşleşmesi (MSIX'te kırılır).
3. **Asenkron işlem doğrulanmıyor:** `SendNotifyMessage`, `Task.Run`, `async void` — "gönderdim, oldu" varsayımı.
4. **Yeniden giriş (reentrancy) bayraklarla kırılıyor:** `ABN_POSCHANGED → ABM_SETSTATE → ABN_STATECHANGE → ...`
   döngüsü idempotency'ye güveniyor; gerçek bir durum makinesi yok.
5. **Sessiz bozulma:** `TrayHost.Start` catch → null → registry listesi; watchdog/log yolları
   `AppLog.Initialize()` öncesinde; kullanıcı hiçbir şey görmüyor.

**Ders:** Bu hatalar "bir satır eksik" hataları değil; **tek doğruluk kaynağı + açık durum makinesi +
doğrulanmış asenkron** disiplininin eksikliği. Tekil yamalarla değil, mimariyle çözülmelidir.

---

## Bölüm 4 — Sağlamlaştırma planı (öncelikli)

### P0 — Sistemi gözlemlenebilir ve tek karar merkezine kavuşturmak

1. **AppLog'u en başta başlat.** `App.xaml.cs:30-47` ve `--restore`/watchdog yollarında
   `AppLog.Initialize()` ilk satır olmalı; şu an oradaki hatalar sessizce düşüyor.
2. **Korelasyon ID'li telemetri.** Her olaya seq kaydet:
   `TaskbarCreated(source=self|explorer|watchdog, seq)`, `AppBarSetPos(before, after, verified)`,
   `PinClick(keyKind, strategy)`, `TrayEnumeration(source=live|registry, count, ms)`,
   `ScrollReset(reason, before, after)`.
3. **TaskbarCreated kendi mesajını ayırt etsin.** `lParam`'a benzersiz sequence koy; `WindowProc`
   `IsBroadcasting` yerine sequence'i karşılaştırsın. Yayından sonra ManagedShell `Icons` koleksiyonunda
   beklenen yeniden kaydı **bounded wait** ile doğrula; olmadıysa ikinci yayın + `OperationReported`.
4. **Tıklama stratejisini kayıtlı pin anahtarından türet.** `TrayPinIdentity.FindMatchingKey(...)` sonucu
   `?? KeyFor(...)` — `KeyFor` tek başına kullanılmasın (bkz. `PinnedTrayStrip.cs` son hali).
5. **Restore yollarını tek koordinatöre bağla.** `ForceRestoreNativeTaskbar`, `Stop`, `Process_Exit`,
   crash handler ve watchdog → `RestoreCoordinator.RestoreAsync(reason)`; journal silme yalnızca
   koordinatörün doğrulamasıyla.

### P1 — Açık durum makinesi ve HWND yaşam döngüsü

6. `TaskbarReplacementSession` içindeki `bool` yığınını (`_active`, `_registered`, `_stateChangeStarted`,
   `_positioning`, `_clearingReservation`, `_fullScreenSuppressed`) tek bir enum durum makinesine çevir:
   `Uninitialized → Hiding → Registered → Positioning → Active → Stopping → Restoring → Failed`.
   OS olayları doğrudan alan yazmaz, `Post(Reconcile(reason))` üretir; geçersiz geçişler reddedilir.
7. **HwndLease(handle, pid, generation)**: her kullanımdan önce `IsWindow` + PID kontrolü; Explorer
   yeniden başlarsa otomatik yeniden abonelik. `SetTaskbarState` kendi HWND'sine **fallback etmesin**;
   `Find() == 0` → `Degraded` durumuna geç, `Reconcile` zamanlasın.
8. **AppBarClient sarmalayıcı:** tüm `SHAppBarMessage` çağrıları buraya taşınmalı; `SendMessageTimeout`,
   `OperationReported` ve yeniden giriş kilidi merkezileşmeli (iyi örnek: `TrayHost.cs:188-196`).
   `StopForRestart()` ya bağlanmalı ya kaldırılmalı — şu an ölü kod.
9. **TrayIconSource servisi:** ManagedShell canlı koleksiyonu birincil, registry snapshot ikincil;
   `RegistryKey` değişiklik bildirimi + process-start olayıyla yeniden enumerasyon. `TrayShellButton`
   (`_lastApps`) ve `PinnedTrayStrip` (`_fallbackApps`) **aynı snapshot'ı paylaşmalı**. `seenNames` dedup'u
   tooltip yerine `(exe, guid, uid)` üçlüsüne dayanmalı.
10. **Pin şeması:** `TrayPinIdentity`'yi string üretiminden `TrayIconRef(Guid?, NormalizedPath, Uid, Source)`
    değer nesnesine çevir; JSON olarak yapılandırılmış kaydet (`kind`, `path`, `guid`, `uid`,
    `schemaVersion`), eski `guid:`/`path:` string'leri için tek seferlik migrasyon.

### P2 — Kaydırma/yerleşim ve sınırlı asenkron

11. `DockScrollViewer` "desired offset" alanının **tek sahibi** olsun: `ScrollByWheel`, `AnimateScroll*`,
    `ResetHorizontalScroll`, `SmoothScrollFrame` aynı `_target` üzerinde çalışsın. `FitDockToContent`
    doğrudan reset çağırmasın; `ScrollOwner.OnContentOverflowChanged(bool)` + gesture state machine
    (aktif gesture sırasında reset yasak).
12. `ResizeDockToContent` 2-pass döngüsü ve `_deferredDockResize`, `_resizePending`,
    `_magnificationLayoutActive`, `_updatingMagnificationLayout`, `SuppressOverflowIndicators` bayrakları
    tek `LayoutController` içinde **ölç → karar → uygula → doğrula** fazlarına dönüşsün.
13. Tüm cross-process çağrılara `CancellationToken` + deadline; `async void` yerine `Task` +
    hata raporu. `TrayHost.Click` için yanıt vermeyen ikonlarda kısa timeout + exe/URI fallback.

### P3 — Test stratejisi

14. `INativeShell`, `IRegistryReader`, `IProcessEnumerator`, `IDispatcher`, `IClock` seam'leriyle
    **OS olaylarını simüle eden** testler: Explorer restart, HWND recycling, PID reuse, DPI değişimi,
    kaydırma sırasında resize, GUID'li ama path pin'li ikon, gizli ikon + registry fallback.
15. Mevcut testler yardımcı fonksiyonları test ediyor (`PinnedTrayChecks`, `DockLayoutChecks`); zincirin
    tamamını simüle eden entegrasyon testleri eklenmeli. Canlı OS'u değiştiren testler
    (`TaskbarRecoveryChecks.cs:57`) izole bir test profiline alınmalı.
16. Her Windows sürüm yükseltmesinde §1.1–1.9'daki dokümante edilmemiş davranışları yeniden doğrulayan
    bir "shell compatibility smoke test" çalıştırılmalı.

---

## Bölüm 5 — Kaynaklar

**Notification area / Shell_NotifyIcon**
- https://learn.microsoft.com/en-us/windows/win32/api/shellapi/nf-shellapi-shell_notifyiconw
- https://learn.microsoft.com/en-us/windows/win32/api/shellapi/ns-shellapi-notifyicondataa
- https://learn.microsoft.com/en-us/windows/win32/shell/notification-area
- https://geoffchappell.com/studies/windows/shell/shell32/api/shlnot/notifyicon.htm
- https://geoffchappell.com/studies/windows/shell/shell32/api/shlnot/copydata.htm
- https://github.com/microsoft/windows-rs/issues/5284 (GUID vs ExecutablePath, MSIX)
- https://github.com/microsoft/microsoft-ui-reactor/issues/1180 (çift olay dedupe)
- https://github.com/glzr-io/zebar/commit/50bee67551af104b4ff9b444db8aef911778245b (AllowSetForegroundWindow + click sırası)
- https://github.com/LGUG2Z/komorebi/commit/ea35d818a179837d179235d885b4ae0c2d71a55a (stale HWND, zombie icon)
- https://github.com/amnweb/yasb/commit/71d7f494ae068470412e3ab0a6e1b060bbde0250 (spy vs injection)
- https://lib.rs/crates/systray-util (WM_COPYDATA dwData değerleri)

**Foreground / aktivasyon**
- https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-setforegroundwindow
- https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-allowsetforegroundwindow
- https://devblogs.microsoft.com/oldnewthing/20090220-00?p=19083
- https://devblogs.microsoft.com/oldnewthing/20161118-00?p=94745
- https://learn.microsoft.com/en-us/windows/win32/api/dwmapi/nf-dwmapi-dwmgetwindowattribute

**AppBar / work area / kabuk olayları**
- https://learn.microsoft.com/en-us/windows/win32/shell/application-desktop-toolbars
- https://learn.microsoft.com/en-us/windows/win32/api/shellapi/nf-shellapi-shappbarmessage
- https://github.com/microsoft/Windows-classic-samples/blob/main/Samples/Win7Samples/winui/shell/legacysamples/appbar/AppBar.cpp
- https://learn.microsoft.com/en-us/windows/win32/shell/taskbar
- https://github.com/microsoft/PowerToys/blob/master/src/runner/tray_icon.cpp
- https://github.com/valinet/ExplorerPatcher/pull/5118 (work-area watchdog, debounce, SPI_SETWORKAREA)
- https://github.com/Milvasoft/kabuk/blob/main/docs/ARCHITECTURE.md (watchdog, TaskbarCreated sonrası yeniden kurulum)
- https://github.com/ChocolateyMilk/chocobar (1 sn reconciliation)
- https://github.com/cairoshell/ManagedShell/wiki/AppBar
- https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-registershellhookwindow

---

## Bölüm 6 — Doğrulama ve uyumluluk

P3 kontrolleri, §4'teki sağlamlaştırma değişikliklerini **canlı kabuğu değiştirmeden** doğrular. Testler explorer'ı
kapatıp açmaz, `TaskbarCreated` yayınlamaz, `SHAppBarMessage` göndermez ve HKCU'ya yazmaz; OS olayları inject edilmiş
dizi/değerlerle simüle edilir.

### Komutlar

```powershell
# Rust portu: biçim, lint ve workspace testleri (Rust-Port/ altında)
cd Rust-Port
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cd ..

# Dil ve sürüm kapıları
powershell -NoProfile -ExecutionPolicy Bypass -File scripts\check-locale-parity.ps1
powershell -NoProfile -ExecutionPolicy Bypass -File scripts\check-release-version.ps1 -Tag v0.4.0
```

### Yeni kontroller

| Dosya | Ne simüle eder |
|---|---|
| `tests/OpenTaskbar.Tests/ShellRecoveryChecks.cs` | `ShellTelemetry.NextSequence()` ile üretilen sequence'i `TrayHost._lastSelfBroadcastSequence` alanına enjekte edip `IsOwnBroadcast` + wParam marker birleşimini sınar; restore-etiketli yayının kendi yayınımız sayılmadığını doğrular. `RestoreCoordinator` tek-giriş kapısını (`SemaphoreSlim(1,1)`) ve eşzamanlı çağrıların `RestoreBusy` dönüp telemetri yazdığını doğrular (gerçek restore yolu çalıştırılmaz). `HwndLease`/`NativeTaskbarWindow` üzerinden Explorer HWND'si yoksa bozulmanın kendi pencemize yazmadığını kontrol eder. |
| `tests/OpenTaskbar.Tests/TraySourceChecks.cs` | `guid:`, `path:`, `path:\|uid`, `path:\|*`, yapılandırılmış JSON ve bozuk girdiler için `TrayIconRef.TryParse`/migrasyonu; `(path, guid, uid)` comparer kimliği; `TrayIconSource.Merge` üzerinde bellek içi anlık görüntü birleştirme (aynı exe + farklı GUID/UID ayrı kalır, aynı tooltip farklı exe'leri birleştirmez, eşleşmeyen fallback görünür). UI bölümü `PinnedTrayStrip` seam'leriyle, GUID'li canlı ikon `path:` pin'e eşleşse bile tıklamanın **saklı anahtarın** stratejisini kullandığını (`PinClickTelemetry`) doğrular. |
| `tests/OpenTaskbar.Tests/DockGestureChecks.cs` | `DockScrollViewer` üzerinde `ScrollChanged → ResetMagnification → ertelenmiş resize → Reset` zincirini `BeginGesture` / `SetContentOverflow(false)` / `SuppressScrollReset` / `ResetHorizontalScroll` / `EndGesture` çağrılarıyla simüle eder; gesture boyunca `HorizontalOffset` ve `DesiredHorizontalOffset` korunur, gesture bitince gerçek reset çalışır. |
| `scripts/check-shell-compat.ps1` | OS/build, explorer.exe varlığı/sürümü, `TaskbarCreated` mesaj kimliği ve canlı `Shell_TrayWnd` pencereleri, DPI ölçeği, monitör sayısı ve işlem integrity düzeyini **salt okunur** raporlar; Windows güncellemesi sonrası el kontrol listesini hatırlatır. |

### Eksik seam'ler (üretim kodu değiştirilmedi)

Aşağıdaki noktalar için testler reflection ile durum enjekte ediyor veya yalnızca kapı davranışını doğruluyor;
gelecekte seam eklenirse testler bunlara taşınmalı:

- `TrayHost._lastSelfBroadcastSequence` özel alanı için injectable sequence sağlayıcısı yok. Aday:
  `internal static bool IsOwnBroadcast(nuint wParam, IntPtr lParam)` saf yardımcı metodu ya da `internal` sequence özelliği.
- `RestoreCoordinator`, gerçek restore yolunu (`TaskbarReplacementSession.ForceRestoreNativeTaskbar()`) doğrudan
  çağırıyor; "iki eşzamanlı çağrı → birebir gerçek restore" ve "Explorer HWND yoksa degrade" senaryolarının OS'a
  dokunmadan test edilmesi için `internal static Func<string?, string?>? RestoreOverride` benzeri bir seam gerekir.
- `TrayIconSource.Merge` private static; testler reflection ile çağırıyor. `internal static` olması yeterli.
- `MainWindow.BeginScrollGestureScope` / `EndScrollGestureScope` ve `MagnificationLayoutPhase` özel/örnek
  üyeleri; zincir simülasyonu `DockScrollViewer` seviyesinde yapılıyor.

> Windows sürüm yükseltmesinden sonra §1.1–1.9'daki dokümante edilmemiş davranışları yukarıdaki üç komutla yeniden
> doğrulayın; smoke testi yalnızca gerçekleri raporlar, doğrulamayı kullanıcı yapar.
