# Gap Analizi Özeti — Subagent Referansı

C# tarafında var, Rust portında olmayan/eksik olan maddeler. Detaylı kaynak: bu dosyanın maddelerini `D:\projects\OpenTaskbar\<dosya yolu>` üzerinden okuyarak doğrula. Rust karşılığı: `D:\projects\OpenTaskbar\crates\...`.

> NOT: Dosya adı eşleşmesi yeterli değil; içeriği oku. Birçok Rust dosyası stub/no-op.

---

## 1. Services\App

| C# dosya | Eksik davranış |
|---|---|
| `AppIdentity.cs` | MSIX/Store algılama (`GetCurrentPackageFullName`), Store update URI, StartupTask politikası |
| `ApplicationWindowMenu.cs` | Hedef pencerenin kendi GetMenu/GetSystemMenu ağacını okuyup WM_COMMAND/WM_SYSCOMMAND çalıştırma |
| `CountryCityService.cs` + `CountryFlag.cs` | countriesnow.space API, 21 ülke + 81 Türkiye ili fallback, önbellek; 14 bayrak PNG |
| `CursorAuthService.cs` | Cursor PKCE login, DPAPI `cursor-auth.bin`, refresh token, SessionChanged/RefreshRequested |
| `DevReloadService.cs` | FileSystemWatcher ile dil/theme hot-reload (250 ms debounce) |
| `RestoreCoordinator.cs` | Tüm restore yollarını semaphore ile serileştirme (3 sn), ShellTelemetry.Result korelasyonu |
| `ShellTelemetry.cs` | `EVENT name key=value` yapılandırılmış log, seq üreticisi, Result(reason/ok/verified/ms/error) |
| `StaTask.cs` | 3 STA kuyruğu (genel/window scan/search), BelowNormal öncelik |
| `StartupStopwatch.cs` | Faz bazlı startup timing (`Startup +NNNms`) |
| `AiUsageService.cs` | Cursor fetch (Rust `_ => {}` boş dal), pool/group ayrıştırma, CLI sign-in, Claude OAuth refresh, gh keyring, Spend/Breakdown/UnlimitedCredits/LimitReached/Severity, Retry-After |
| `ApplicationIconProvider.cs` | SHGetFileInfo + ExtractIconEx fallback, 12–256 px snap, hover icon varyantları, straight→premultiplied alpha, ShellItemDisplayName |
| `AppShortcutResolver.cs` | NameCache displayName/stem eşleme, webhelper marker, WScript.Shell .lnk indeksi, WorkingDirectory |
| `AppLog.cs` | thread id, OT_VERBOSE_LOG, ReportError, ResolveMessage, Shutdown |
| `WeatherService.cs` | Windows Geolocation, language parametresi, dayCount koruma, WeatherDay tarihleri, Describe() ikonları |
| `DockAutoHideController.cs` | RevealedGuardTimer, DWM extended frame bounds + CLOAKED, BackgroundWindowsProvider, CubicEase, InvalidateWindowOverlap |
| `DockFisheyeLayout.cs` | Raised-cosine + kritik sönümlü spring (StepSpring), ComputeNoOverlapMaxScale, FreeExpansion, ComputeSlotExtras, ComputeSafeLift, preview clearance |
| `DockItemOrdering.cs` | InsertionIndex histerezisi, SlideFor, MoveVisibleItem, ApplyPinnedGroups (başta start/search/taskview, sonda tray/bin) |
| `DockMetrics.cs` | DockSizeProfile (48/58/66), ResolveGap/ResolveHeight, WidgetCardHeight, ComputeFlyoutGap |
| `DynamicIslandPolicy.cs` | `OrderWidgets(..., timedOrder)` parametresi Rust imzasında yok |
| `IconPack.cs` | CornerRadius/SharpCorner (Leaf/Petal), Description, IconScale üst sınırı |
| `NotificationService.cs` | ShowInfoToast, UpdateRequested olayı, aktif toast yaşam döngüsü |
| `RunningAppIconCache.cs` | HWND bazlı AUMID retry (4 deneme/1 sn), miss sayacı, IsLoadingFor/Retain/IconLoaded, DPI generation clear |
| `RunningAppProjection.cs` | AUMID ile gruplama (ApplicationFrameHost), notification badge toplama, AggregateByExecutablePath |
| `TaskbarRecoveryWatchdog.cs` | Rust heartbeat event'i hiç tetiklenmiyor → 3 kaçırma toleransı devre dışı; arg parse, imza kontrolü |
| `ThemePackTransfer.cs` | CreateTheme (basedOnId + preserveOriginalIcons), manifest uzunluk sınırları |
| `ThemePacks.cs` | InstallBuiltInThemes (6 tema), ApplyWidgets typed override, GetNativeWindowsLogo, plaka türetme |
| `UpdateChecker.cs` | MSIX SupportsInAppUpdates, runas yükseltme, GetExecutablePath fallback |
| `WidgetPack.cs` | Resolve() typed conversion (Brush/Color/CornerRadius/Thickness/FontFamily), Dark+Light katmanı, gradient stops, 256 token sınırı |
| `WinKeyRevealMonitor.cs` | LLKHF_INJECTED filtresi, cancelledByOtherKey state makinesi, lifecycle |

## 2. Services\Hardware\WirelessBattery (Rust: tek `platform/wireless_battery.rs`)

- **AsusRogBatteryProvider**: VID 0x0B05 filtresi dışında yok — 0xFF00/0xFF01 usage, 0x12/0x07 request, 64-byte reply
- **Corsair**: VID 0x1B1C dışında yok — Bragi 0xFF42 property 0x000F/0x0010, legacy 0xFFC2
- **HyperX**: VID 0x03F0 dışında yok — 4 protokol ailesi, Kingston 0x0951
- **CompxAtkVxe**: 13+ vendor (Compx 3554, ATK 373B, Mchose, Lamzu, Keychron 3434…), 17/64/65/9-byte report, checksum — ciddi stub
- **WirelessKeyboard24gProvider**: sadece ürün adı heuristic'i; 2.4G HID protokolü yok
- **GamepadHidProvider**: DualSense USB/BT, DualShock4, Switch Pro 0x30 report yok
- **Logitech**: HID++ var ama G-Hub loopback JSON (`ParseGHubJson`, `ExtractGHubDevices`) yok
- **SteelSeries**: GG coreProps loopback URI keşfi yok
- **BleGatt**: BatteryLife/DevPkeyIsPresent, AepId filtresi yok
- **Aggregator**: disk önbelleği (64 KB), outlier koruması (%25), `IgnoredWirelessDevices`/`WirelessDeviceAliases`/poll ayarları, privacy diagnostic summary yok
- Mevcut: XInput, Razer, StandardHid, Win32HidTransport, modeller

## 3. Services\Interop

| C# dosya | Eksik davranış |
|---|---|
| `AppBarClient.cs` | SendMessageTimeout ile Explorer hung probe (500 ms), NotificationsSuspended reentrancy gate |
| `CoreAudio.cs` | GetDevices(isRecording), SetDefaultDevice/PolicyConfigClient, IMMNotificationClient, AudioEndpointMonitor ref-count, AppAudioSessionMatcher scoring |
| `DesktopFileTransfer/Drop.cs` | UIOption.AllDialogs, `\\.\`/`\\?\` path reddi, OperationCanceledException ayrımı |
| `DisplayService.cs` | DIP dönüşümü (96/dpi), GetTargetDisplay(deviceName/index), GetSecondaryMonitors |
| `NativeMemoryMethods.cs` | SetProcessWorkingSetSize, GC.Collect Optimized, 20 sn throttle, ScheduleIdleTrim |
| `NativeTaskbarWindow.cs` | Kendi sahte Shell_TrayWnd'sini atlama, TryAcquire lease, FindSecondaryLeases |
| `LibreWolfMediaTimeline.cs` | UI Automation Document/Slider, music.youtube.com tespiti, m:ss etiketinden ilerleme |
| `ShellOverlayWindows.cs` | Alt+Tab/Task View sınıfları (XamlExplorerHostIslandWindow, MultitaskingViewFrame, TaskSwitcherWnd) auto-hide filtresinde yok |
| `StartMenuState.cs` | SetWinEventHook CLOAKED/UNCLOAKED, 60–150 ms grace, WasOpenAtPress — tamamen yok |
| `WindowsTaskbarPinMonitor.cs` | RegNotifyChangeKeyValue ile Taskband Favorites izleme — yok |
| `ShellFlyoutPositionJournal.cs` | Rust formatı var AMA write()/read() hiç çağrılmıyor; generation counter/Restore yok |
| `ShellShortcuts.cs(+Search)` | RepositionSearch/StartMenuAsync, MeasureNativeSearch, SyncSearchQuery/ForwardKeyToSearch (UIA), ShowDesktopForDrag, MinimizeAllWindows, sistem araçları (lock/logoff/restart/shutdown/task manager), ReleaseStuckModifiers, DWM extended-frame ölçümü — büyük eksik |
| `SyntheticKeyboard.cs` | modifier tracking + finally release, ReleaseAll, 20 ms chord hold, ReplaceFocusedText, extended-key flag |
| `TaskbarReplacementSession.cs` | HideRetry (20/40 deneme), degrade budget 45, TaskbarCreated reconcile, WinEvent tray/shell hook, fullscreen z-order, yan dock reservation, restore doğrulama+3 deneme |
| `TaskbarWindowRules.cs` | ITaskList_Deleted, CloakedShell 0x2, WS_EX_WINDOWEDGE olmayan immersive sınıflar |
| `TrayHost.cs` | `tray_host.rs:12` açık no-op: gerçek Shell_TrayWnd host, UIPI, ABM forwarder, message forwarding yok |
| `TrayIcons/TrayIconSource.cs` | registry watcher + 30 sn refresh + debounce, 64 kayıt icon cache, transient tooltip filtresi, live icon birleştirme |
| `TrayPinIdentity/TrayIconRef.cs` | `|*` wildcard, PinKeyKind, 512/64 sınır, JSON pin migration |
| `VirtualDesktops.cs` | Registry VirtualDesktopIDs/CurrentVirtualDesktop → (Current, Count) |
| `WindowThumbnailPreview.cs` | Aero Peek (DwmActivateLivePreview), DWMWA_EXCLUDED_FROM_PEEK, peek grace 250/500 ms, 140 ms fade |

## 4. Services\Settings + Localization

- `RenderModePreference.cs`: software rendering tercihi (≈100 MB tasarruf) — Rust'ta yok
- `FullBackupArchive.cs`: `.otbackup` zip (manifest + settings + pins + aktif tema), export/inspect/import — Rust'ta sadece `settings.backup.json`
- `WindowsPalette.cs` + `WindowsPaletteListener.cs`: AccentPalette 8 stop → ~30 token, gizli HWND ile WM_SETTINGCHANGE/ImmersiveColorSet/DWMCOLORIZATION takibi — yok
- `DockSettingsStore.cs`: NormalizeLegacyJson, retired widget migration, `IsValid()` büyük bölümü
- `DockProfileService.cs`: HiddenApps, WidgetOrder, DockItemOrder, width mode, Theme/Pack, hotkey'ler + Capture/SyncActive/Switch
- `HotkeyBinding.cs`: TryFromKeyEvent, geniş token seti (`, . / ; ' [ ] \ \``, PageUp/Prior, Back)
- `RadiusManager.cs`: 9 anahtarlı concentric radius, DockSheenRadius, Derive()
- `SettingsBackdrop.cs`: Ayarlar penceresi acrylic + DWMWA_BORDER_COLOR=None + Win11 system backdrop — yok
- `SettingsBackup.cs`: pin'leri içeren PinBackup JSON — yok
- `StartupRegistration.cs`: MSIX StartupTask, StartupApproved\Run, shortcut sync
- `ThemeManager.cs`: 4 yüzey için SurfaceStyle + canlı rebuild, **SystemUsesLightTheme önceliği** (Rust yalnızca AppsUseLightTheme okuyor)
- `ApplicationPreferences.cs`: Changed olayı + SyncActive merkeziyeti
- `LanguageCatalog/UiText`: diskten Reload (dev reload), TryGet, Item[] bildirimi, culture-aware Format

## 5. Widgets (Rust: `widgets/mod.rs` chip spec + `ui/flyout.rs`)

1. **WidgetHoverCoordinator.cs** — 350 ms hover intent, inline genişleme, slider capture, imleç doğrulama, Suppress — tamamen yok
2. **AiUsageWidget.cs** — tüm AI flyout'ları (`open_widget_flyout` doğrudan `return false`); spend/breakdown/reset/plan/stale
3. **NetSpeedWidget.cs** — multi-stream hız testi, ping, 6 stream, sonuç grid'i, iptal/retry — yok
4. **MediaWidget.xaml.cs + MediaTimeline.cs** — albüm kapağı/tint, seek slider, dikey volume popover, 560px island genişleme, LibreWolf timeline; Rust `WidgetChipSpec::Media` sabit play glyph
5. **QuickSettingsWidget.xaml.cs** — WinRT Radio API gerçek Wi-Fi/BT toggle; Rust `false` sabiti + `ms-settings:` URI
6. **TelemetrySparkline.cs** — CPU/RAM/GPU alan grafiği; FlyoutBlock'ta chart yok
7. **FocusWidget.cs** — Break fazı, SystemSounds.Asterisk, Pulse, oturum numarası, FocusMinutes/BreakMinutes/Sessions
8. **CountdownDockWidget.cs** — özel süre girişi (1–1440, validasyon); stopwatch centisecond
9. **SystemStatsWidget** — GPU + GPU sıcaklık meter'ları (`stats_meters & 16/32`), sparkline, Resource Monitor, 5 miss/2 sample hysteresis, flyout canlı güncelleme
10. **WeatherWidget** — 3 günlük forecast (veri toplanıyor ama kullanılmıyor), ikonlar, WeatherUnit °F, 30 dk/2 dk retry
11. **BluetoothBatteryWidget** — cihaz rename/hide, RestoreIgnored, CyclePollInterval (15/30/60/120/300), threshold, listenOnly, CopyDiagnostics, charging bolt, stale opacity, 5 ayar alanı
12. **StorageWidget** — DriveType.Fixed && IsReady filtresi, kompakt mod, 30 sn async polling
13. **AudioWidget** — output/input device ComboBox, headphone ikonu, wheel remainder (120 delta = 10%), hover flyout
14. **KeyboardLayoutWidget** — son harici foreground penceresine WM_INPUTLANGCHANGEREQUEST, yatay wheel
15. **DockWidgetService/WidgetCoordinator** — per-widget density overrides, ShrinkPriority, hardware availability gizleme, alias ID (Date/ShowDesktop), HasSettings/live preview
16. **Views\Widgets\WidgetPanelHost/Frame/CatalogItem** — ayarlar kataloğu paneli, Escape/focus iade, 300 ms suppress, flyout preview toggle

Kısmen mevcut: Clock (takvim var ama canlı saat/first-day-of-week/DST adı/layout yok), Battery (çok iyi), Stopwatch/Countdown/Focus temel, Stats CPU/RAM, Storage, Audio mixer, QS toggle grid, Keyboard listesi, AI chip düzenleri, 23 widget kataloğu.

## 6. Views / UI

1. **MainWindow.Orientation.cs** — dikey dock (sol/sağ); Rust `DockEdge` okunmuyor, `compute_window_rect()` daima alt
2. **MainWindow.Features.cs** — ikincil hafif şerit + shelf reservation + ayna dock; klavye gezinmesi; Store update path
3. **SecondaryDockWindow.cs** — tamamen yok
4. **DynamicIslandWindow.xaml.cs** — `SetExpanded()` + 246px details panel + `GetDetails()` widget başına + 180 ms animasyon + global input pre-process; Rust Expand butonu flyout açıyor
5. **MainWindow.DragDrop.cs** — OLE DragDrop dışa sürükleme, 450 ms drag-hover app, RecycleBin drop, stack/widget/shortcut drop, BuildQuotedArguments
6. **MainWindow.Extras.cs** — DockStack 4-sütun önizleme + rozet + arka planda ikon yükleme + OpenFolderDialog
7. **MainWindow.Groups.cs** — grup flyout üye ikonları, ekle/çıkar, sürükle-reorder, live count/icon
8. **MainWindow.Magnification.cs** — non-magnified zone, native shell button, hover icon, scroll gesture scope, expansion budget
9. **ThemePropertyEditor.cs / ThemeBrushPicker.cs** — token editörü (6 kategori + Advanced, 350 ms autosave) ve HSV/gradient diyaloğu (16 stop) — yok
10. **SettingsWindow.xaml.cs** — tema editörü entegrasyonu, SaveFileDialog/OpenFileDialog, tam yedekleme, hotkey capture UI, hava durumu ülke/şehir seçici, AI OAuth (Cursor) UI, tema galerisi görselleri, pencere animasyonu/acrylic, DockTopGap/Width/Height UI, GitHub link
11. **SettingsWindow.Search.cs** — C# çalışan WPF ağacından tüm satırları topluyor; Rust statik ~50 satır katalog
12. **DockFlyout.cs** — keyfi WPF içeriği, RetainForExternalContextMenu + WH_MOUSE_LL #32768, 4 halkalı soft shadow, StaysOpen/focus lifecycle
13. **TrayShellButton/PinnedTrayStrip** — canlı Shell_NotifyIcon listesi, tıklama iletimi, GUID/UID/path pin stratejileri, abonelikler, oryantasyon
14. **WidgetCardFeedback.cs** — temalı dolgu/gradyan geçişleri, 80/120/160 ms press animasyonu, ThemeChanged aboneliği
15. **App.xaml.cs/Program.cs** — `--inspect`, `--dev`, `--smoke-test`, `--software-render`, SessionSwitch reset, WindowsPaletteListener, StartupRegistration.SyncOnStartup

Mevcut: live drag, temel fisheye, island sayfalama, arama, TextPrompt, scroll viewer, responsive layout, ikon glyph, shell düğmeleri, context menu, DComp preview, jumplist, recovery.

## 7. Models / varlıklar / CI / test

- **DockSettings**: C# 110 alan ↔ Rust 71; 49 alan tipik alan yok (extra'da korunuyor ama işlev yok); 6 alias yok; `DockProfile` `flatten extra` YOK → HiddenApps/Theme/hotkey'ler kalıcı siliniyor; `validate()` yok
- **DockStack.cs**: FileSystemWatcher, `.crdownload/.part` filtresi, HasNewItems, 2×2 önizleme
- **DockApp.cs**: DockKey, NormalizeArguments, IsSharedHostExecutable, AUMID
- **TrackedWindow.cs**: HasNotification, AUMID
- **DockWidgetOption.cs**: LivePreview, IsHardwareAvailable, HasSettings, MatchesSearch
- **themes/**: 6 bundled tema (catppuccin/gruvbox/nord/prism/solarized/tokyo-night) + widget pack — Rust'ta asset yok, `built_in_ids()` sadece testte
- **Fonts/**: Inter 4 ağırlık + OFL — Rust'ta yok
- **Flags/**: 14 PNG — Rust'ta yok
- **Locale**: C# 14×999 ↔ Rust 14×1071; C# fazlası 5 kullanımdan kaldırılmış anahtar; Rust fazlası 77 (DI/takvim/font)
- **Release**: publish/package/check script'leri, checksums.txt, release-manifest, SLSA/VirusTotal, MSIX AppxManifest + 5 logo, CodeQL(csharp), dependabot nuget — Rust'ta yok; rust-ci sadece rust-port branch'inde
- **Docs**: README.md 15 satır; CHANGELOG/PRIVACY/docs/*/Design README yok
- **Test**: C# 31 dosya ~1935 Check (gerçek pencere + sentetik input + görsel ağaç); Rust 93 #[test] saf fonksiyon. En büyük boşluklar: AiUsageChecks(151), FeatureAndMemoryChecks(221, .otbackup), DockLayoutChecks(286), WirelessBatteryChecks(106), MediaVolume/AudioMixer, ShellRecovery/TaskbarRecovery/flyout pozisyon, per-app mixer, telemetry locale parity.
