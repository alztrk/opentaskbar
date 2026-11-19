using System.Globalization;
using System.IO;
using OpenTaskbar.Models;
using OpenTaskbar.Services;

// `--headless` runs only the pure logic checks: no WPF Application, no windows, no synthetic keyboard input,
// no file dialogs and no desktop side effects. The default run keeps the full UI verification suite for CI.
var headless = args.Contains("--headless", StringComparer.OrdinalIgnoreCase);
var checks = 0;
CountdownChecks();
StopwatchChecks();
CatalogChecks();
WidgetCategoryChecks();
LocalizationChecks();
TaskManagerTextChecks();
SettingsChecks();
InitialLanguageChecks();
MultiMonitorChecks();
DockIconPixelChecks();
StartMenuAndOverlayChecks();
AppLaunchChecks();
TrayListChecks();
checks += RunningAppChecks.Run();
checks += AiUsageChecks.Run();
if (!headless) checks += WidgetViewChecks.Run();
checks += ThemePackChecks.Run();
checks += ThemeTransferChecks.Run();
checks += TaskbarRecoveryChecks.Run();
checks += ApplicationJumpListChecks.Run();
Console.WriteLine($"{checks} behavior checks passed.");

void Check(bool condition, string name)
{
    if (!condition) throw new InvalidOperationException(name);
    checks++;
}

// Launching runs off the UI thread now, so failures come back as error keys; nothing is started here.
void AppLaunchChecks()
{
    var missing = Path.Combine(Path.GetTempPath(), Path.GetRandomFileName() + ".exe");
    Check(OpenTaskbar.MainWindow.StartApplication(missing, string.Empty, elevated: false) == "ErrorAppFileNotFound",
        "A missing executable must report ErrorAppFileNotFound instead of starting anything.");
}

// The tray flyout shows its last list at once and rebuilds only when a fresh enumeration differs.
void TrayListChecks()
{
    IReadOnlyList<TrayApp> first = [new("OneDrive", @"C:\Apps\OneDrive.exe", null), new("Teams", @"C:\Apps\Teams.exe", null)];
    IReadOnlyList<TrayApp> sameWithNewIcons = [new("OneDrive", @"c:\apps\onedrive.exe", null), new("Teams", @"C:\Apps\Teams.exe", null)];
    IReadOnlyList<TrayApp> renamed = [new("OneDrive - Personal", @"C:\Apps\OneDrive.exe", null), new("Teams", @"C:\Apps\Teams.exe", null)];
    IReadOnlyList<TrayApp> shorter = [new("OneDrive", @"C:\Apps\OneDrive.exe", null)];
    Check(OpenTaskbar.Widgets.TrayWidget.SameApps(first, sameWithNewIcons), "Path casing alone must not rebuild the open tray list.");
    Check(!OpenTaskbar.Widgets.TrayWidget.SameApps(first, renamed), "A changed app name must refresh the open tray list.");
    Check(!OpenTaskbar.Widgets.TrayWidget.SameApps(first, shorter), "An app that exited must refresh the open tray list.");
}

void CountdownChecks()
{
    var clock = new TestClock();
    var timer = new CountdownDockWidget(clock);
    timer.Refresh();
    Check(timer.State == CountdownState.NotConfigured && !timer.CanToggle && timer.Value == string.Empty,
        "A timer without a duration must not look configured.");

    var errorChanges = 0;
    timer.PropertyChanged += (_, e) => { if (e.PropertyName == nameof(timer.HasValidationError)) errorChanges++; };
    foreach (var invalid in new[] { "0", "1441", "-1", "abc", "1.5", "99999999999999" })
    {
        timer.MinutesText = invalid;
        Check(timer.HasValidationError && !timer.CanToggle, $"Reject invalid duration: {invalid}");
        timer.Toggle();
        Check(!timer.IsRunning, "An invalid duration must not start through a direct model action.");
    }
    Check(errorChanges > 0, "Invalid input must notify the visible validation state.");
    timer.MinutesText = " ";
    Check(!timer.HasValidationError && !timer.CanToggle, "Clearing the input must remove its validation error.");

    timer.MinutesText = "1";
    Check(timer.State == CountdownState.Ready && timer.Value == "00:01:00", "One minute must prepare a sixty-second timer.");
    timer.Toggle();
    clock.Advance(TimeSpan.FromSeconds(20));
    timer.Refresh();
    Check(timer.IsRunning && timer.Value == "00:00:40" && !timer.CanEditDuration, "Running time must count down.");
    timer.MinutesText = "5";
    Check(timer.MinutesText == "1", "Running duration must not be editable.");

    timer.Toggle();
    clock.Advance(TimeSpan.FromHours(1));
    timer.Refresh();
    Check(timer.State == CountdownState.Paused && timer.Value == "00:00:40", "Paused time must stay fixed.");
    timer.Toggle();
    clock.Advance(TimeSpan.FromSeconds(39.5));
    timer.Refresh();
    Check(timer.IsRunning && timer.Value == "00:00:01", "A fractional final second must remain visible.");
    clock.Advance(TimeSpan.FromSeconds(0.5));
    timer.Refresh();
    Check(timer.State == CountdownState.Finished && !timer.IsRunning && timer.Value == "00:00:00",
        "The timer must finish exactly at zero.");
    Check(timer.StatusText == UiText.Get("CountdownFinished") && timer.CanEditDuration,
        "Completion must have visible text and allow another duration.");
    clock.Advance(TimeSpan.FromHours(10));
    timer.Refresh();
    Check(timer.Value == "00:00:00", "A finished timer must never show negative time.");
    timer.Toggle();
    Check(timer.IsRunning && timer.Value == "00:01:00", "Restart must use the configured duration.");
    timer.Reset();
    Check(timer.State == CountdownState.Ready && !timer.IsRunning && timer.Value == "00:01:00",
        "Reset must stop the timer and restore its duration.");
    timer.MinutesText = "1440";
    Check(timer.Value == "24:00:00", "The upper duration boundary must not wrap at midnight.");
}

void StopwatchChecks()
{
    var clock = new TestClock();
    var stopwatch = new StopwatchDockWidget(clock);
    stopwatch.Refresh();
    Check(!stopwatch.IsRunning && stopwatch.Value == "00:00:00", "The stopwatch must start idle.");
    stopwatch.Toggle();
    clock.Advance(TimeSpan.FromSeconds(65));
    stopwatch.Refresh();
    Check(stopwatch.Value == "00:01:05", "Elapsed time must cross minute boundaries.");
    stopwatch.Toggle();
    clock.Advance(TimeSpan.FromHours(1));
    stopwatch.Refresh();
    Check(!stopwatch.IsRunning && stopwatch.Value == "00:01:05", "Paused stopwatch time must stay fixed.");
    stopwatch.Toggle();
    clock.Advance(TimeSpan.FromSeconds(5));
    stopwatch.Refresh();
    Check(stopwatch.Value == "00:01:10", "Resume must accumulate elapsed time.");
    clock.Advance(TimeSpan.FromHours(26));
    stopwatch.Refresh();
    Check(stopwatch.Value == "26:01:10", "Stopwatch hours must not wrap at midnight.");
    stopwatch.Reset();
    Check(!stopwatch.IsRunning && stopwatch.Value == "00:00:00", "Reset must clear and stop the stopwatch.");
}

void CatalogChecks()
{
    var clock = new TestClock();
    var catalog = new DockWidgetService(clock);
    Check(catalog.CreateWidgets(null).Count == 0, "New settings must not install widgets implicitly.");
    Check(catalog.CreateOptions([], []).All(option => !option.IsInstalled && !option.IsEnabled),
        "The available catalog must be optional.");
    var searchOption = catalog.CreateOptions([], []).SingleOrDefault(option => option.Id == DockWidgetService.SearchId);
    Check(searchOption is not null && searchOption.Label == UiText.Get("WidgetLauncher")
        && searchOption.Description == UiText.Get("WidgetLauncherDescription") && searchOption.HasSettings,
        "The Search widget must be listed in Settings with a localized label, description and options panel.");
    var first = catalog.CreateWidgets([DockWidgetService.StopwatchId, DockWidgetService.CountdownId]);
    var options = catalog.CreateOptions([], []);
    Check(DockWidgetService.AiUsageWidgets.Select(definition => definition.Provider).Order().SequenceEqual(
            Enum.GetValues<AiProviders>().Where(provider => provider is not (AiProviders.None or AiProviders.All)).Order())
        && DockWidgetService.AiUsageWidgets.All(definition => options.SingleOrDefault(option => option.Id == definition.Id) is { } aiOption
            && aiOption.Label == UiText.Get(definition.LabelKey) && aiOption.Description == UiText.Get(definition.LabelKey + "Description")
            && !aiOption.HasSettings)
        && options.All(option => option.Id != "AiUsage"),
        "Every AI provider must be its own catalog widget with a localized card and no settings panel.");
    var legacy = DockSettingsStore.MigrateLegacyAiUsage(new DockSettings(
        Widgets: ["AiUsage", "Clock"], InstalledWidgets: ["AiUsage", "Clock"], WidgetOrder: ["Clock", "AiUsage"],
        DockItemOrder: ["start", "widget:AiUsage"], AiUsageProviders: AiProviders.Claude | AiProviders.Codex,
        WidgetDensityOverrides: new Dictionary<string, WidgetDensityMode> { ["AiUsage"] = WidgetDensityMode.Compact }));
    Check(legacy.Widgets!.SequenceEqual(["AiUsageClaude", "AiUsageCodex", "Clock"])
        && legacy.InstalledWidgets!.Count == DockWidgetService.AiUsageWidgets.Count + 1
        && legacy.WidgetOrder!.Count == DockWidgetService.AiUsageWidgets.Count + 1 && legacy.WidgetOrder[0] == "Clock"
        && legacy.DockItemOrder!.Contains("widget:AiUsageOpenRouter") && !legacy.DockItemOrder!.Contains("widget:AiUsage")
        && legacy.WidgetDensityOverrides!["AiUsageCopilot"] == WidgetDensityMode.Compact && !legacy.WidgetDensityOverrides.ContainsKey("AiUsage")
        && legacy.AiUsageProviders == AiProviders.None && DockSettingsStore.Validate(legacy),
        "The retired single AI usage widget must migrate into one valid widget per provider.");
    Check(first.Count == 2 && first.OfType<ITimedDockWidget>().Count() == 2, "Both time widgets must be available.");
    var stopwatch = first.OfType<StopwatchDockWidget>().Single();
    var timer = first.OfType<CountdownDockWidget>().Single();
    timer.MinutesText = "1";
    stopwatch.Toggle();
    Check(stopwatch.IsRunning && !timer.IsRunning, "Time widgets must have independent state.");
    timer.Toggle();
    catalog.CreateWidgets([]);
    clock.Advance(TimeSpan.FromSeconds(30));
    var second = catalog.CreateWidgets([DockWidgetService.StopwatchId, DockWidgetService.CountdownId]);
    DockWidgetService.Refresh(second, AppLanguage.Turkish);
    Check(ReferenceEquals(stopwatch, second[0]) && ReferenceEquals(timer, second[1]), "Hiding must preserve the session instances.");
    Check(stopwatch.Value == "00:00:30" && timer.Value == "00:00:30", "Hidden widgets must retain elapsed time.");
}

void WidgetCategoryChecks()
{
    var clock = new TestClock();
    var catalog = new DockWidgetService(clock);
    var options = catalog.CreateOptions([], []);

    Check(options.Count == 24, "Catalog must define exactly 24 widgets.");

    var timeWidgets = options.Where(o => o.Category == DockWidgetCategory.TimeAndWeather).Select(o => o.Id).ToArray();
    Check(timeWidgets.Length == 5
        && timeWidgets.Contains(DockWidgetService.ClockId)
        && timeWidgets.Contains(DockWidgetService.WeatherId)
        && timeWidgets.Contains("DayProgress")
        && timeWidgets.Contains(DockWidgetService.StopwatchId)
        && timeWidgets.Contains(DockWidgetService.CountdownId),
        "TimeAndWeather category must contain Clock, Weather, DayProgress, Stopwatch and Countdown.");

    var systemWidgets = options.Where(o => o.Category == DockWidgetCategory.SystemAndHardware).Select(o => o.Id).ToArray();
    Check(systemWidgets.Length == 5
        && systemWidgets.Contains(DockWidgetService.SystemStatsId)
        && systemWidgets.Contains(DockWidgetService.NetSpeedId)
        && systemWidgets.Contains(DockWidgetService.BatteryId)
        && systemWidgets.Contains(DockWidgetService.BluetoothBatteryId)
        && systemWidgets.Contains("Storage"),
        "SystemAndHardware category must contain SystemStats, NetSpeed, Battery, BluetoothBattery and Storage.");

    var aiWidgets = options.Where(o => o.Category == DockWidgetCategory.AiQuotas).Select(o => o.Id).ToArray();
    Check(aiWidgets.Length == 7
        && DockWidgetService.AiUsageWidgets.All(w => aiWidgets.Contains(w.Id)),
        "AiQuotas category must contain all 7 AI provider widgets.");

    var mediaWidgets = options.Where(o => o.Category == DockWidgetCategory.MediaAndControls).Select(o => o.Id).ToArray();
    Check(mediaWidgets.Length == 4
        && mediaWidgets.Contains(DockWidgetService.MediaId)
        && mediaWidgets.Contains(DockWidgetService.AudioId)
        && mediaWidgets.Contains(DockWidgetService.QuickSettingsId)
        && mediaWidgets.Contains(DockWidgetService.TrayId),
        "MediaAndControls category must contain Media, Audio, QuickSettings and Tray.");

    var prodWidgets = options.Where(o => o.Category == DockWidgetCategory.Productivity).Select(o => o.Id).ToArray();
    Check(prodWidgets.Length == 3
        && prodWidgets.Contains(DockWidgetService.FocusId)
        && prodWidgets.Contains(DockWidgetService.SearchId)
        && prodWidgets.Contains(DockWidgetService.DesktopsId),
        "Productivity category must contain Focus, Search and Desktops.");

    UiText.Current.SetLanguage(AppLanguage.Turkish);
    var sample = options.First(o => o.Category == DockWidgetCategory.AiQuotas);
    Check(sample.CategoryLabel == "Yapay Zekâ", "Category label must be localized in Turkish.");
    UiText.Current.SetLanguage(AppLanguage.English);
    Check(sample.CategoryLabel == "AI Quotas", "Category label must be localized in English.");
    UiText.Current.SetLanguage(AppLanguage.Turkish);
}

void LocalizationChecks()
{
    var option = new DockWidgetOption("Clock", "Saat", "Yerel saat", true, false, new DockWidget("Clock", "Saat"));
    var changes = new List<string?>();
    option.PropertyChanged += (_, e) => changes.Add(e.PropertyName);
    option.UpdateText("Clock", "Local time");
    Check(option.IsInstalled && !option.IsEnabled, "Localization must preserve installation and visibility.");
    Check(changes.SequenceEqual([nameof(option.Label), nameof(option.Description), nameof(option.CategoryLabel)]), "All localized fields must notify bindings.");
    changes.Clear();
    option.UpdateText("Clock", "Local time");
    Check(changes.Count == 0, "Unchanged text must not notify.");
    Check(option.MatchesSearch("TIME", CultureInfo.GetCultureInfo("en-US")), "English search must ignore English casing.");
    Check(!option.MatchesSearch("missing", CultureInfo.GetCultureInfo("en-US")), "An unmatched query must stay empty.");
    option.UpdateText("Pil", "Şarj durumu");
    Check(option.MatchesSearch("PİL", CultureInfo.GetCultureInfo("tr-TR")), "Turkish search must match dotted capital I.");

    UiText.Current.SetLanguage(AppLanguage.English);
    Check(UiText.Current.IsEnglish && UiText.Current.Language == AppLanguage.English, "UiText must reflect English language selection.");
    Check(UiText.Get("ErrorDockAttach") == "Cannot connect the dock window to Windows.", "Error keys must resolve to English.");

    UiText.Current.SetLanguage(AppLanguage.Turkish);
    Check(!UiText.Current.IsEnglish && UiText.Current.Language == AppLanguage.Turkish, "UiText must reflect Turkish language selection.");
    Check(UiText.Get("ErrorDockAttach") == "Dock penceresi Windows'a bağlanamadı.", "Error keys must resolve to Turkish.");
    Check(AppLog.ResolveMessage("ErrorDockAttach") == "Dock penceresi Windows'a bağlanamadı.",
        "Structured log entries must resolve the same text shown to the user.");
    Check(AppLog.ResolveMessage("raw exception text") == "raw exception text",
        "Unknown keys must fall back to raw text so logging never throws.");
    Check(UiText.Get("OpenLogFile") == "Günlük dosyasını aç", "The log-opening action must be localized.");
}

void TaskManagerTextChecks()
{
    try
    {
        UiText.Current.SetLanguage(AppLanguage.Turkish);
        Check(UiText.Get("TaskManager") == "Görev Yöneticisi", "The Task Manager menu item must have a Turkish label.");
        Check(UiText.Get("ErrorTaskManagerOpen") == "Görev Yöneticisi açılamadı.", "The Task Manager failure must have Turkish text.");
        UiText.Current.SetLanguage(AppLanguage.English);
        Check(UiText.Get("TaskManager") == "Task Manager", "The Task Manager menu item must have an English label.");
        Check(UiText.Get("ErrorTaskManagerOpen") == "Cannot open Task Manager.",
            "Task Manager error keys must resolve to English.");
    }
    finally
    {
        UiText.Current.SetLanguage(AppLanguage.Turkish);
    }
}

void SettingsChecks()
{
    var path = Path.Combine(Path.GetTempPath(), Path.GetRandomFileName());
    try
    {
        var store = new DockSettingsStore(path);
        File.WriteAllText(path, "{\"Widgets\":[\"Clock\",\"Date\"]}");
        var legacy = store.Load();
        Check(legacy.Error is null && legacy.Settings.InstalledWidgets?.SequenceEqual(["Clock", "Date"]) == true,
            "Legacy selections must migrate before validation.");
        File.WriteAllText(path, "{\"Widgets\":[\"Clock\",\"Memory\"],\"InstalledWidgets\":[\"Clock\",\"Memory\",\"Storage\"]}");
        var removedMemory = store.Load();
        Check(removedMemory.Error is null && removedMemory.Settings.Widgets?.SequenceEqual(["Clock"]) == true
            && removedMemory.Settings.InstalledWidgets?.SequenceEqual(["Clock", "Storage"]) == true,
            "The removed memory widget must be dropped from stored settings without resetting them.");
        Check(store.Save(new DockSettings(Widgets: ["Countdown"], InstalledWidgets: [])) is not null,
            "Enabled widgets must be installed.");
        Check(store.Save(new DockSettings(Widgets: ["Unknown"], InstalledWidgets: ["Unknown"])) is not null,
            "Unknown widget identifiers must be rejected.");
        Check(store.Save(new DockSettings(InstalledWidgets: ["Clock", "Clock"])) is not null,
            "Duplicate installations must be rejected.");
        Check(store.Save(new DockSettings(Widgets: [], InstalledWidgets: ["Countdown"])) is null,
            "An installed widget may remain hidden.");
        var hidden = store.Load();
        Check(hidden.Error is null && hidden.Settings.Widgets?.Count == 0
            && hidden.Settings.InstalledWidgets?.Contains("Countdown") == true, "Hidden installation must survive reload.");
        Check(store.Save(new DockSettings(DockScale: 1.25)) is null && store.Load().Settings.DockScale == 1.25,
            "A valid dock scale must persist and reload.");
        Check(store.Save(new DockSettings(DockScale: 0.5)) is not null && store.Save(new DockSettings(DockScale: 2.0)) is not null,
            "Out-of-range dock scale values must be rejected.");
    }
    finally
    {
        File.Delete(path);
        File.Delete(path + ".tmp");
    }
}

void InitialLanguageChecks()
{
    var path = Path.Combine(Path.GetTempPath(), Path.GetRandomFileName());
    var turkish = new DockSettingsStore(path, CultureInfo.GetCultureInfo("tr-TR"));
    var english = new DockSettingsStore(path, CultureInfo.GetCultureInfo("en-US"));
    var german = new DockSettingsStore(path, CultureInfo.GetCultureInfo("de-DE"));
    var unsupported = new DockSettingsStore(path, CultureInfo.GetCultureInfo("is-IS"));
    bool LoadsLanguage(DockSettingsStore store, AppLanguage language)
    {
        var result = store.Load();
        return result.Error is null && result.Settings.Language == language;
    }
    bool UsesDefaultLanguageOnError(DockSettingsStore store, AppLanguage language)
    {
        var result = store.Load();
        return result.Error is not null && result.Settings.Language == language;
    }

    try
    {
        Check(LoadsLanguage(turkish, AppLanguage.Turkish), "First launch must follow Turkish system UI language.");
        Check(LoadsLanguage(english, AppLanguage.English), "First launch must follow English system UI language.");
        Check(LoadsLanguage(german, new AppLanguage("de-DE")), "First launch must follow German system UI language.");
        Check(LoadsLanguage(unsupported, AppLanguage.English), "Unsupported system languages must use the documented English default.");
        File.WriteAllText(path, "{}");
        Check(LoadsLanguage(turkish, AppLanguage.Turkish), "A legacy file without a language must follow Turkish system UI language.");
        Check(LoadsLanguage(english, AppLanguage.English), "A legacy file without a language must follow English system UI language.");
        Check(english.Save(new DockSettings(Language: AppLanguage.Turkish)) is null, "An explicit Turkish preference must save.");
        Check(LoadsLanguage(english, AppLanguage.Turkish), "An explicit Turkish preference must survive an English system language.");
        Check(turkish.Save(new DockSettings(Language: AppLanguage.English)) is null, "An explicit English preference must save.");
        Check(LoadsLanguage(turkish, AppLanguage.English), "An explicit English preference must survive a Turkish system language.");
        File.WriteAllText(path, "{\"Language\":\"Unknown\"}");
        Check(UsesDefaultLanguageOnError(english, AppLanguage.English), "An invalid stored language must report an error in system-default settings.");
        File.WriteAllText(path, "{\"Language\":42}");
        Check(UsesDefaultLanguageOnError(turkish, AppLanguage.Turkish), "An undefined language value must report an error.");
        File.WriteAllText(path, "null");
        Check(english.Load() is { Error: not null }, "Null settings must remain an explicit error.");
    }
    finally
    {
        File.Delete(path);
        File.Delete(path + ".tmp");
    }
}

void MultiMonitorChecks()
{
    var path = Path.Combine(Path.GetTempPath(), Path.GetRandomFileName());
    try
    {
        var store = new DockSettingsStore(path);
        var targetSettings = new DockSettings(
            TargetDisplay: @"\\.\DISPLAY2",
            SecondaryApps: SecondaryAppScope.AllMonitors,
            SecondaryWidgets: false);
        Check(store.Save(targetSettings) is null, "TargetDisplay and Secondary settings must save without error.");
        var loaded = store.Load();
        Check(loaded.Error is null, "Saved multi-monitor settings must load without error.");
        Check(loaded.Settings.TargetDisplay == @"\\.\DISPLAY2", "TargetDisplay must persist and reload.");
        Check(loaded.Settings.SecondaryApps == SecondaryAppScope.AllMonitors, "SecondaryApps scope must persist and reload.");
        Check(!loaded.Settings.SecondaryWidgets, "SecondaryWidgets boolean must persist and reload.");

        var json = File.ReadAllText(path);
        Check(json.Contains("\"SecondaryApps\": \"AllMonitors\""), "SecondaryAppScope must serialize as string enum.");

        File.WriteAllText(path, "{}");
        var empty = store.Load();
        Check(empty.Error is null && empty.Settings.TargetDisplay is null, "Default TargetDisplay must be null (primary).");
        Check(empty.Settings.SecondaryApps == SecondaryAppScope.CurrentMonitorOnly, "Default SecondaryApps must be CurrentMonitorOnly.");
        Check(empty.Settings.SecondaryWidgets, "Default SecondaryWidgets must be true.");
    }
    finally
    {
        File.Delete(path);
        File.Delete(path + ".tmp");
    }

    var primary = DisplayService.GetTargetDisplay(null);
    ArgumentNullException.ThrowIfNull(primary);
    Check(!string.IsNullOrWhiteSpace(primary.DeviceName), "Target display must have a device name.");
    Check(primary.Width > 0 && primary.Height > 0, "Target display must have positive dimensions.");

    var fallback = DisplayService.GetTargetDisplay("NON_EXISTENT_DISPLAY_XYZ");
    ArgumentNullException.ThrowIfNull(fallback);
    Check(fallback.Handle == primary.Handle || fallback.IsPrimary, "Fallback display must be primary or first display.");

    var secondaries = DisplayService.GetSecondaryMonitors(null);
    Check(!secondaries.Contains(primary.Handle), "GetSecondaryMonitors must not contain the target monitor handle.");

    UiText.Current.SetLanguage(AppLanguage.English);
    Check(UiText.Get("TargetDisplay") == "Main dock display", "TargetDisplay must be localized in English.");
    Check(UiText.Get("SecondaryApps") == "Secondary display apps", "SecondaryApps must be localized in English.");
    Check(UiText.Get("SecondaryWidgets") == "Secondary display widgets", "SecondaryWidgets must be localized in English.");
    Check(UiText.Get("SecondaryAppsCurrent") == "Windows on this display only", "SecondaryAppsCurrent must be localized in English.");
    Check(UiText.Get("SecondaryAppsAll") == "All open windows", "SecondaryAppsAll must be localized in English.");

    UiText.Current.SetLanguage(AppLanguage.Turkish);
    Check(UiText.Get("TargetDisplay") == "Ana dock ekranı", "TargetDisplay must be localized in Turkish.");
    Check(UiText.Get("SecondaryApps") == "İkinci ekran uygulamaları", "SecondaryApps must be localized in Turkish.");
    Check(UiText.Get("SecondaryWidgets") == "İkinci ekranda widget göster", "SecondaryWidgets must be localized in Turkish.");
    Check(UiText.Get("SecondaryAppsCurrent") == "Yalnızca bu ekrandaki pencereler", "SecondaryAppsCurrent must be localized in Turkish.");
    Check(UiText.Get("SecondaryAppsAll") == "Tüm açık pencereler", "SecondaryAppsAll must be localized in Turkish.");
}

void DockIconPixelChecks()
{
    // Large dock at 100%: 38 DIP * 0.88 = 33.4 px must use the native 32 px artwork instead of resampling it.
    Check(ApplicationIconProvider.ComputeApplicationIconPixels(38 * 0.88, 1.0) == 32, "Large dock icons must snap to 32 px.");
    Check(ApplicationIconProvider.ComputeApplicationIconPixels(34 * 0.88, 1.0) == 32, "Medium dock icons must snap to 32 px.");
    Check(ApplicationIconProvider.ComputeApplicationIconPixels(28 * 0.88, 1.0) == 24, "Compact dock icons must snap to 24 px.");
    Check(ApplicationIconProvider.ComputeApplicationIconPixels(38 * 0.88, 1.25) == 40, "125% DPI must snap to the native 40 px size.");
    Check(ApplicationIconProvider.ComputeApplicationIconPixels(38 * 0.88, 1.5) == 48, "150% DPI must snap to the native 48 px size.");
    Check(ApplicationIconProvider.ComputeApplicationIconPixels(38 * 0.88 * 1.5, 1.5) == 75,
        "Sizes far from any native size must keep their own pixel size instead of jumping to a distant one.");
    Check(ApplicationIconProvider.ComputeApplicationIconPixels(10, 0.5) == 16, "Icons must never drop below 16 px.");

    // Magnified icons (up to 1.28x) use the smallest native size that covers the magnified size, so they are
    // scaled down instead of blurred up.
    Check(ApplicationIconProvider.ComputeHoverIconPixels(32, 1.28) == 48, "A 32 px icon magnified to 41 px must use 48 px artwork.");
    Check(ApplicationIconProvider.ComputeHoverIconPixels(24, 1.28) == 32, "A 24 px icon magnified to 31 px must use 32 px artwork.");
    Check(ApplicationIconProvider.ComputeHoverIconPixels(40, 1.28) == 64, "A 40 px icon magnified to 52 px must use 64 px artwork.");
    Check(ApplicationIconProvider.ComputeHoverIconPixels(300, 1.28) == 384, "Beyond the largest native size the exact magnified size is used.");
    Check(ApplicationIconProvider.GetHoverVariant(null) is null, "A missing icon has no hover variant.");
}

void StartMenuAndOverlayChecks()
{
    // Task switcher window classes
    Check(ShellOverlayWindows.IsTaskSwitcherClassName("XamlExplorerHostIslandWindow"), "Windows 11 switcher must be recognized.");
    Check(ShellOverlayWindows.IsTaskSwitcherClassName("MultitaskingViewFrame"), "Windows 10 switcher must be recognized.");
    Check(ShellOverlayWindows.IsTaskSwitcherClassName("TaskSwitcherWnd"), "Classic switcher must be recognized.");
    Check(!ShellOverlayWindows.IsTaskSwitcherClassName("CabinetWClass"), "Explorer cabinet must not be recognized as switcher.");
    Check(!ShellOverlayWindows.IsTaskSwitcherClassName("Notepad"), "Notepad must not be recognized as switcher.");
    Check(!ShellOverlayWindows.IsTaskSwitcherClassName(null), "Null class name must not match switcher.");

    // Start Menu process identification vs Windows Search flyout
    Check(StartMenuState.IsStartMenuProcessName("StartMenuExperienceHost"), "StartMenuExperienceHost must be recognized as Start Menu.");
    Check(StartMenuState.IsStartMenuProcessName("startmenuexperiencehost"), "Process name check must be case insensitive.");
    Check(!StartMenuState.IsStartMenuProcessName("SearchHost"), "SearchHost must NOT be treated as Start Menu.");
    Check(!StartMenuState.IsStartMenuProcessName("SearchApp"), "SearchApp must NOT be treated as Start Menu.");
    Check(!StartMenuState.IsStartMenuProcessName("explorer"), "explorer must not be treated as Start Menu.");
    Check(!StartMenuState.IsStartMenuProcessName(null), "Null process name must return false.");

    // Scoped press dismissal behavior
    var clock = new TestClock();
    var state = new StartMenuState(clock);
    Check(!state.IsOpen, "StartMenuState must start closed.");

    state.OnForegroundChanged(true);
    Check(state.IsOpen, "Foreground change to Start menu must mark it open.");
    Check(state.WasOpenAtPress(openNow: true), "When open now, WasOpenAtPress must return true.");

    state.OnForegroundChanged(false);
    Check(!state.IsOpen, "Foreground change away must mark it closed.");

    // Ambient check without pointer press must NOT return true
    Check(!state.WasOpenAtPress(openNow: false, pressTimestamp: 0),
        "Without active pointer press, a closed Start menu must never be considered open at press.");

    // A click occurring 500ms later must NOT be eaten
    var later = System.Diagnostics.Stopwatch.GetTimestamp() + (long)(0.5 * System.Diagnostics.Stopwatch.Frequency);
    Check(!state.WasOpenAtPress(openNow: false, pressTimestamp: later),
        "A later click must not be treated as open at press.");

    // Start menu positioning: full-screen CoreWindow container offset math
    // On 2560x1440 screen at 100% DPI, Start button at X=570, Y=1379:
    // Window width is 2560, height is 1374. Flyout is 642x724.
    // flyoutLocalX = (2560 - 642) / 2 = 959.
    // desiredFlyoutX = 570 - 8 = 562.
    // targetX = 562 - 959 = -397.
    // Resulting screen flyout X: targetX + flyoutLocalX = -397 + 959 = 562 (directly above Start button!).
    var (fullX, fullY) = ShellShortcuts.ComputeStartMenuPosition(
        windowWidth: 2560, windowHeight: 1374,
        workLeft: 0, workTop: 0, workRight: 2560, workBottom: 1374,
        anchorLeft: 570, anchorTop: 1379, anchorRight: 620, anchorBottom: 1429,
        edge: DockEdge.Bottom, dpiScale: 1.0, isLeftAligned: false);
    Check(fullX == -397, "Full-screen container must offset horizontally so flyout aligns over the Start button.");
    Check(fullY == 4, "Full-screen container must offset vertically to sit right above the dock.");

    // Discrete window (e.g. Windows 10 or custom shell, 642x724):
    var (discreteX, discreteY) = ShellShortcuts.ComputeStartMenuPosition(
        windowWidth: 642, windowHeight: 724,
        workLeft: 0, workTop: 0, workRight: 2560, workBottom: 1374,
        anchorLeft: 570, anchorTop: 1379, anchorRight: 620, anchorBottom: 1429,
        edge: DockEdge.Bottom, dpiScale: 1.0, isLeftAligned: false);
    Check(discreteX == 562, "Discrete window must be positioned directly at the clamped anchor X.");
    Check(discreteY == 642, "Discrete window must be clamped to the work area above the dock.");
}

sealed class TestClock : TimeProvider
{
    private long _timestamp;
    public override long TimestampFrequency => TimeSpan.TicksPerSecond;
    public override long GetTimestamp() => _timestamp;
    public void Advance(TimeSpan elapsed) => _timestamp += elapsed.Ticks;
}
