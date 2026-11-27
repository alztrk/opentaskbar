using System.IO;
using System.IO.Compression;
using System.Reflection;
using System.Runtime.CompilerServices;
using System.Runtime.InteropServices;
using System.Text.Json;
using System.Text.Json.Nodes;
using System.Windows;
using System.Windows.Controls;
using System.Windows.Input;
using System.Windows.Media;
using System.Windows.Media.Imaging;
using System.Windows.Threading;
using OpenTaskbar;
using OpenTaskbar.Models;
using OpenTaskbar.Services;
using OpenTaskbar.Widgets;

internal static class FeatureAndMemoryChecks
{
    public static int Run(Application app)
    {
        SynchronizationContext.SetSynchronizationContext(new DispatcherSynchronizationContext(Dispatcher.CurrentDispatcher));
        var checks = 0;
        void Check(bool condition, string message)
        {
            if (!condition) throw new InvalidOperationException(message);
            checks++;
        }

        var previousLanguage = UiText.Current.Language;
        try
        {
            ThemeManager.Apply(AppTheme.Dark);
            UiText.Current.SetLanguage(AppLanguage.English);

            // 1. Lazy WidgetCoordinator parity & memory invariant
            var strip = new StackPanel();
            using (var lazyCoordinator = new WidgetCoordinator(strip, Dispatcher.CurrentDispatcher, _ => { }, lazy: true))
            {
                lazyCoordinator.Apply(new DockSettings(Widgets: []));
                Check(strip.Children.Count == 0, "Lazy WidgetCoordinator must instantiate 0 widgets when none are enabled.");

                lazyCoordinator.Apply(new DockSettings(Widgets: ["Clock", "Focus"]));
                Check(strip.Children.Count == 2 && strip.Children.OfType<ClockWidget>().Any() && strip.Children.OfType<FocusWidget>().Any(),
                    "Lazy WidgetCoordinator must instantiate only enabled widgets on demand.");

                var reordered = false;
                var firstWidget = lazyCoordinator.CurrentOrder[0];
                lazyCoordinator.OrderChanged = _ => reordered = true;
                lazyCoordinator.ReorderWidgets(0, 1);
                Check(reordered && lazyCoordinator.CurrentOrder[0] != firstWidget,
                    "Lazy WidgetCoordinator must support drag reordering.");

                // Materialize additional side-effect-free widgets without triggering OS location, audio, or radio APIs.
                var pureWidgets = new[] { "Clock", "Focus", "DayProgress", "Storage", "Battery" };
                lazyCoordinator.Apply(new DockSettings(Widgets: pureWidgets));
                Check(strip.Children.Count == pureWidgets.Length,
                    "Enabling additional widgets must materialize all requested controls.");
            }

            var aiLoads = 0;
            using (var ai = new AiUsageWidget(AiProviders.OpenCode, (_, _) =>
            {
                aiLoads++;
                return Task.FromResult<(AiUsageReport?, string?)>((null, "ErrorAiUsageSignIn"));
            }))
            {
                ai.Tick(DateTime.Now);
                Check(aiLoads == 0, "An inactive AI usage widget must not load.");
                ai.Activate();
                Check(aiLoads == 1 && ai.Visibility == Visibility.Visible
                    && AiCards(ai) is [{ ToolTip: string hint }] && hint.Contains("opencode", StringComparison.Ordinal),
                    "AI Usage must keep sign-in failures visible with a localized CLI hint.");
                ai.Tick(DateTime.Now);
                Check(aiLoads == 1, "A sign-in failure must not poll again without changed credentials.");
                ai.SetCompactState(true);
                Check(ai.BuildHoverFlyoutContent() is not null, "AI Usage must use the shared hover flyout.");
                ai.Deactivate();
                ai.Tick(DateTime.Now.AddHours(1));
                Check(aiLoads == 1, "Inactive AI widgets must not call their loader.");
            }

            var pending = new TaskCompletionSource<(AiUsageReport?, string?)>();
            CancellationToken aiCancellation = default;
            using (var ai = new AiUsageWidget(AiProviders.Codex, (_, cancellation) =>
            {
                aiCancellation = cancellation;
                return pending.Task;
            }))
            {
                ai.Activate();
                ai.Deactivate();
                Check(aiCancellation.IsCancellationRequested, "Disabling an AI widget must cancel its pending request.");
                pending.SetResult((new(AiProviders.Codex, "fixture", [new("AiUsageSession", 0.5, null, 18000)], DateTimeOffset.UtcNow), null));
                Dispatcher.CurrentDispatcher.Invoke(() => { }, DispatcherPriority.Background);
                Check(AiCards(ai) is [{ ToolTip: "Codex" }], "A late AI response must not restore a disabled widget.");
            }

            var aiNow = DateTimeOffset.UtcNow;
            AiUsageWidget CreateAiWidget(AiProviders provider) => new(provider, (p, _) => Task.FromResult<(AiUsageReport?, string?)>((new(p, "Plus",
                [new("AiUsageSession", 0.42, aiNow.AddHours(3), 18000), new("AiUsageWeekly", p == AiProviders.Codex ? 0.9 : 0.29,
                    aiNow.AddDays(5), 604800)], aiNow)
            { LimitReached = p == AiProviders.Codex }, null)));
            using (var claude = CreateAiWidget(AiProviders.Claude))
            using (var codex = CreateAiWidget(AiProviders.Codex))
            {
                claude.Activate();
                codex.Activate();
                Dispatcher.CurrentDispatcher.Invoke(() => { }, DispatcherPriority.Background);
                Check(AiCards(claude) is [{ Content: StackPanel { Children: [System.Windows.Shapes.Path { Data: not null }, TextBlock { Text: "Claude" }, StackPanel { Children: [StackPanel, StackPanel] }] } } claudeCard]
                    && claudeCard.ToolTip is string claudeTip && claudeTip.StartsWith("Claude Code 42%", StringComparison.Ordinal)
                    && claudeTip.Contains("Weekly 29%", StringComparison.Ordinal) && claudeTip.Contains("Resets in", StringComparison.Ordinal)
                    && AiCards(codex) is [{ Content: StackPanel { Children: [_, TextBlock { Text: "Codex" }, StackPanel { Children: [StackPanel, StackPanel] }] } } codexCard]
                    && codexCard.ToolTip is string codexTip && codexTip.Contains(UiText.Get("AiUsageLimitReached"), StringComparison.Ordinal),
                    "Each AI usage widget must show one card with logo, name, usage indicators and a one-line summary.");
                claude.ApplySettings(new DockSettings(AiUsageRing: AiUsageRing.Weekly));
                Check(AiCards(claude)[0].ToolTip is string weeklyTip && weeklyTip.StartsWith("Claude Code 29%", StringComparison.Ordinal),
                    "The ring setting must choose the window the dock shows.");
                claude.SetCompactState(true);
                Check(AiCards(claude) is [{ Content: StackPanel { Children: [_, TextBlock { Visibility: Visibility.Collapsed }, _] } }],
                    "Compact AI Usage must keep the logo and indicators and drop the name.");
                Check(claude.BuildHoverFlyoutContent() is not null && codex.BuildHoverFlyoutContent() is not null,
                    "Claude and Codex must use the shared hover flyout.");
            }

            // 2. Audio mixer checks (isolated via stubbed AppSessionSource; no live Core Audio or hardware sensors)
            checks += AudioMixerChecks.Run();

            // 4. WindowThumbnailPreview lazy creation & disposal
            var preview = new WindowThumbnailPreview((Style)app.FindResource("DockButton"));
            Check(!preview.IsVisible && preview.SourceHandle == 0 && preview.ActiveHandles.Count == 0,
                "WindowThumbnailPreview must start hidden with zero handles before first hover.");
            Check(!preview.ShowForWindows([], strip, null),
                "WindowThumbnailPreview must not open when given no valid windows.");
            preview.Hide();
            preview.Dispose();
            Check(true, "WindowThumbnailPreview must dispose cleanly without ever creating HWNDs when unused.");

            // 5. WeatherService frozen cached brushes
            var clear1 = WeatherService.GetConditionBackground(0);
            var clear2 = WeatherService.GetConditionBackground(0);
            Check(clear1.IsFrozen && ReferenceEquals(clear1, clear2),
                "WeatherService condition brushes must be frozen and cached.");

            // 6. DockFlyout visual-tree cleanup & weak target release
            var (flyout, contentRef) = ExerciseDockFlyout(app, Check);
            app.Dispatcher.Invoke(() => { }, DispatcherPriority.ContextIdle);
            GC.Collect();
            GC.WaitForPendingFinalizers();
            Check(!contentRef.IsAlive, "DockFlyout must release its child visual tree on close.");
            var popupField = typeof(DockFlyout).GetField("_popup", System.Reflection.BindingFlags.NonPublic | System.Reflection.BindingFlags.Instance)!;
            var popup = (System.Windows.Controls.Primitives.Popup)popupField.GetValue(flyout)!;
            Check(popup.PlacementTarget is null, "DockFlyout must clear PlacementTarget on close.");
            GC.KeepAlive(flyout);

            // 7. Shared ContextMenu + LauncherWindow lifecycle verification
            var sharedMenu = new ContextMenu
            {
                Items = { new MenuItem { Header = "Open" }, new MenuItem { Header = "Close" } }
            };
            var dockItem1 = new Button { ContextMenu = sharedMenu, DataContext = new DockApp("App1", "a1.exe") };
            var dockItem2 = new Button { ContextMenu = sharedMenu, DataContext = new DockApp("App2", "a2.exe") };
            Check(ReferenceEquals(dockItem1.ContextMenu, dockItem2.ContextMenu),
                "Shared ContextMenu must be reused across dock items without per-item duplication.");

            // Verify MediaWidget density and flyout contract without connecting to live Windows SMTC.
            using (var mediaWidget = new MediaWidget())
            {
                Check(mediaWidget.BuildHoverFlyoutContent() is null, "MediaWidget must use its dedicated volume popover instead of a generic flyout.");
                Check(mediaWidget.SupportsCompactMode, "MediaWidget must declare compact mode support.");
                Check(mediaWidget.FullWidthEstimate > mediaWidget.CompactWidthEstimate, "MediaWidget full width must exceed compact width.");
            }





            // 8. Heavy DockSettingsStore validation & round-trip
            var tempSettings = Path.Combine(Path.GetTempPath(), $"ot-heavy-{Guid.NewGuid():N}.json");
            try
            {
                var store = new DockSettingsStore(tempSettings);
                var heavy = new DockSettings(
                    Position: DockPosition.Center,
                    Theme: AppTheme.Dark,
                    Language: AppLanguage.English,
                    Widgets: ["Media", "SystemStats", "Focus", "Weather", "Audio", "QuickSettings", "Clock", "Storage", "DayProgress", "Stopwatch", "Countdown"],
                    InstalledWidgets: ["Media", "SystemStats", "Focus", "Weather", "Audio", "QuickSettings", "Clock", "Storage", "DayProgress", "Stopwatch", "Countdown"],
                    ShowRecycleBin: true,
                    ShowOnAllMonitors: true,
                    StatsMeters: SystemMeters.All,
                    Profiles: [new DockProfile("Work", [@"C:\Games\game.exe"])],
                    ActiveProfile: "Work");
                Check(store.Save(heavy) is null, "Heavy DockSettings must save without errors.");
                var (loaded, loadErr) = store.Load();
                Check(loadErr is null && loaded.StatsMeters == SystemMeters.All && loaded.Widgets?.Count == 11 && loaded.ShowOnAllMonitors,
                    "Heavy DockSettings must round-trip all 11 widgets, SystemMeters.All, and multi-monitor flags.");
            }
            finally
            {
                try { File.Delete(tempSettings); } catch (IOException) { }
            }

            // 9. TrayIcons PNG DecodePixelWidth capping (caps > 32px without upscaling <= 32px)
            var decodeMethod = typeof(TrayIcons).GetMethod("DecodeSnapshot",
                System.Reflection.BindingFlags.NonPublic | System.Reflection.BindingFlags.Static)!;
            var smallPng = EncodeSolidPng(16);
            var largePng = EncodeSolidPng(256);
            var decodedSmall = (BitmapSource?)decodeMethod.Invoke(null, [smallPng]);
            var decodedLarge = (BitmapSource?)decodeMethod.Invoke(null, [largePng]);
            Check(decodedSmall?.PixelWidth == 16, "Small tray PNG snapshots must not be upscaled.");
            Check(decodedLarge?.PixelWidth == 32, "Oversized tray PNG snapshots must be downsampled to 32px.");

            // 10. NativeTaskbarWindow & Live TrayHost checks
            var explorerTaskbar = NativeTaskbarWindow.Find();
            if (explorerTaskbar != 0)
            {
                GetWindowThreadProcessId(explorerTaskbar, out var ownerPid);
                Check(ownerPid != (uint)Environment.ProcessId, "NativeTaskbarWindow.Find must never return a window owned by OpenTaskbar.");
            }
            Check(!TrayHost.IsBroadcasting, "TrayHost.IsBroadcasting must be false when idle.");
            var testTray = new OpenTaskbar.Views.Shell.TrayShellButton();
            var liveEmptyContent = testTray.BuildLiveContent(Array.Empty<ManagedShell.WindowsTray.NotifyIcon>());
            Check(liveEmptyContent is StackPanel sp && sp.Children.Count >= 2,
                "TrayShellButton.BuildLiveContent must render a heading, empty caption and settings for an empty icon list.");

            // 11. Dock profiles lifecycle, migration, sync, validation, and localization checks
            CheckDockProfiles(Check);

            // 12. Focus mode toggle, non-destructive restore, widget polling stop, profile interaction, and localization checks
            CheckFocusModeFlags(Check);
            CheckFocusModeWidgets(Check);
            CheckFocusModeProfiles(Check);
            CheckFocusModePersistenceAndSettingsWindow(app.Dispatcher, Check);
            CheckLocalization("Focus Mode localization", ["FocusMode", "FocusModeDescription"], Check);

            // 13. Combined theme + settings full backup archive checks
            CheckFullBackupArchive(Check);

            // 14. Custom hotkey parsing, resolution, and per-profile persistence
            CheckCustomHotkeys(Check);

            // 15. Settings search indexing and filtering (issue #74)
            CheckSettingsSearch(app, Check);

            // 16. Thumbnail preview peek delay, grace window, and middle-click close (issue #93)
            CheckThumbnailPreviewPeekAndMiddleClick(app, Check);

            // 17. SettingsWindow lazy live-preview loading, theme-restore guard, and rapid close/reopen
            CheckSettingsOpenClosePerformance(app, Check);
        }
        finally
        {
            UiText.Current.SetLanguage(previousLanguage);
        }

        return checks;
    }

    private static void CheckFocusModeFlags(Action<bool, string> Check)
    {
        var focused = new DockSettings(AnimationsEnabled: true, NotificationBadgesEnabled: true, FocusModeEnabled: true);
        Check(!focused.EffectiveAnimationsEnabled && !focused.EffectiveNotificationBadgesEnabled,
            "Focus Mode must suppress effective animations and notification badges.");
        Check(focused.AnimationsEnabled && focused.NotificationBadgesEnabled,
            "Focus Mode must preserve enabled animation and notification badge preferences.");

        var restored = focused with { FocusModeEnabled = false };
        Check(restored.EffectiveAnimationsEnabled && restored.EffectiveNotificationBadgesEnabled,
            "Turning off Focus Mode must restore enabled animations and notification badges.");

        foreach (var (animations, badges) in new[] { (false, true), (true, false), (false, false) })
        {
            var settings = restored with { AnimationsEnabled = animations, NotificationBadgesEnabled = badges };
            var suppressed = settings with { FocusModeEnabled = true };
            var resumed = suppressed with { FocusModeEnabled = false };
            Check(!suppressed.EffectiveAnimationsEnabled && !suppressed.EffectiveNotificationBadgesEnabled
                && resumed.EffectiveAnimationsEnabled == animations && resumed.EffectiveNotificationBadgesEnabled == badges,
                $"Toggling Focus Mode must honor AnimationsEnabled={animations} and NotificationBadgesEnabled={badges}.");
        }

        using var json = JsonDocument.Parse(SerializeSettings(focused));
        Check(!json.RootElement.TryGetProperty(nameof(DockSettings.EffectiveAnimationsEnabled), out _)
            && !json.RootElement.TryGetProperty(nameof(DockSettings.EffectiveNotificationBadgesEnabled), out _),
            "Serialized settings must omit both computed Focus Mode properties.");
    }

    private static void CheckFocusModeWidgets(Action<bool, string> Check)
    {
        var strip = new StackPanel();
        using var coordinator = new WidgetCoordinator(strip, Dispatcher.CurrentDispatcher, _ => { }, lazy: true);
        var isActive = typeof(OpenTaskbar.Widgets.DockWidget)
            .GetProperty("IsActive", BindingFlags.Instance | BindingFlags.Public | BindingFlags.NonPublic)
            ?? throw new InvalidOperationException("Missing DockWidget.IsActive property.");
        var timer = typeof(WidgetCoordinator).GetField("_timer", BindingFlags.Instance | BindingFlags.NonPublic)
            ?.GetValue(coordinator) as DispatcherTimer
            ?? throw new InvalidOperationException("Missing WidgetCoordinator polling timer.");
        var availabilityChanges = 0;
        coordinator.AvailabilityChanged += () => availabilityChanges++;

        var settings = new DockSettings(Widgets: ["Clock", "Focus"], FocusModeEnabled: false);
        coordinator.Apply(settings);
        Check(availabilityChanges == 1,
            "Applying multiple active widgets must batch dock availability changes into one layout notification.");
        availabilityChanges = 0;
        var widgets = coordinator.ActiveWidgets;
        Check(widgets.Count == 2 && widgets.GetValueOrDefault("Clock") is ClockWidget
            && widgets.GetValueOrDefault("Focus") is FocusWidget,
            "The lazy coordinator must create only the Clock and Focus widgets.");
        CheckWidgetState(active: true, Visibility.Visible, "Before enabling Focus Mode");

        var focused = settings with { FocusModeEnabled = true };
        coordinator.Apply(focused);
        Check(availabilityChanges == 1,
            "Deactivating multiple widgets must batch dock availability changes into one layout notification.");
        availabilityChanges = 0;
        CheckWidgetState(active: false, Visibility.Collapsed, "While Focus Mode is enabled");
        Check(focused.Widgets is ["Clock", "Focus"] && settings.Widgets is ["Clock", "Focus"],
            "Enabling Focus Mode must preserve the configured Clock and Focus widgets.");

        coordinator.Apply(focused with { FocusModeEnabled = false });
        Check(availabilityChanges == 1,
            "Reactivating multiple widgets must batch dock availability changes into one layout notification.");
        CheckWidgetState(active: true, Visibility.Visible, "After disabling Focus Mode");
        var restored = coordinator.ActiveWidgets;
        Check(restored.Count == widgets.Count
            && widgets.All(pair => ReferenceEquals(pair.Value, restored.GetValueOrDefault(pair.Key))
                && strip.Children.Contains(pair.Value)),
            "Disabling Focus Mode must reactivate the original widget instances.");

        void CheckWidgetState(bool active, Visibility visibility, string context)
        {
            Check(coordinator.HasActiveWidgets == active, $"{context}, HasActiveWidgets must be {active}.");
            Check(timer.IsEnabled == active, $"{context}, the widget polling timer's enabled state must be {active}.");
            foreach (var (id, widget) in widgets)
            {
                Check(isActive.GetValue(widget) is bool widgetActive && widgetActive == active,
                    $"{context}, {id}.IsActive must be {active}.");
                Check(widget.Visibility == visibility, $"{context}, {id} must be {visibility}.");
            }
        }
    }

    private static void CheckFocusModeProfiles(Action<bool, string> Check)
    {
        var defaults = DockProfileService.EnsureDefaultProfile(new DockSettings(
            Widgets: ["Clock"], InstalledWidgets: ["Clock", "Focus"], FocusModeEnabled: true));
        var (work, createError) = DockProfileService.Create(defaults, "Work");
        Check(createError is null && work is { ActiveProfile: "Work", FocusModeEnabled: true, Widgets: [] },
            "Creating a profile in Focus Mode must preserve the global toggle and start widget-free.");
        Check(work.Profiles?.SingleOrDefault(profile => profile.Name == "Work")?.Widgets is [],
            "A newly created profile must start with an empty widget snapshot.");

        var (restoredDefault, defaultError) = DockProfileService.Switch(work, "Default");
        Check(defaultError is null && restoredDefault is { ActiveProfile: "Default", FocusModeEnabled: true, Widgets: ["Clock"] },
            "Switching to Default must keep Focus Mode enabled and restore its Clock widget.");
        var (restoredWork, workError) = DockProfileService.Switch(restoredDefault, "Work");
        Check(workError is null && restoredWork is { ActiveProfile: "Work", FocusModeEnabled: true, Widgets: [] },
            "Switching back to Work must keep Focus Mode enabled and stay widget-free.");

        var unfocusedWork = restoredWork with { FocusModeEnabled = false };
        Check(unfocusedWork.Widgets is [], "Turning off Focus Mode must preserve Work's empty widget set.");
        var (unfocusedDefault, switchError) = DockProfileService.Switch(unfocusedWork, "Default");
        Check(switchError is null && unfocusedDefault is { ActiveProfile: "Default", FocusModeEnabled: false, Widgets: ["Clock"] },
            "Switching after disabling Focus Mode must keep it off and restore Default's Clock widget.");
        Check(unfocusedDefault.Profiles is [{ Name: "Default", Widgets: ["Clock"] }, { Name: "Work", Widgets: [] }],
            "Each profile must retain its configured widget snapshot through Focus Mode toggles and profile switches.");
    }

    private static void CheckFocusModePersistenceAndSettingsWindow(Dispatcher dispatcher, Action<bool, string> Check)
    {
        var tempPath = Path.Combine(Path.GetTempPath(), $"ot-focus-mode-{Guid.NewGuid():N}.json");
        try
        {
            var store = new DockSettingsStore(tempPath);
            File.WriteAllText(tempPath, "{}");
            var (legacy, legacyError) = store.Load();
            Check(legacyError is null && !legacy.FocusModeEnabled, "An empty settings object must load with Focus Mode off.");

            var initial = new DockSettings(Theme: ThemeManager.Current, Language: UiText.Current.Language,
                AnimationsEnabled: true, Widgets: ["Clock"], InstalledWidgets: ["Clock"],
                ThemePack: ThemePacks.Current?.Id, FocusModeEnabled: false);
            foreach (var enabled in new[] { true, false })
            {
                Check(store.Save(initial with { FocusModeEnabled = enabled }) is null,
                    $"Settings must save cleanly with FocusModeEnabled={enabled}.");
                var (loaded, loadError) = store.Load();
                Check(loadError is null && loaded.FocusModeEnabled == enabled,
                    $"Reloading settings must preserve FocusModeEnabled={enabled}.");
            }

            CheckFocusModeSettingsWindow(new ApplicationPreferences(store), dispatcher, Check);
        }
        finally
        {
            File.Delete(tempPath);
        }
    }

    private static void CheckFocusModeSettingsWindow(ApplicationPreferences preferences, Dispatcher dispatcher, Action<bool, string> Check)
    {
        Check(preferences.LoadError is null, "The Focus Mode SettingsWindow fixture must load cleanly.");
        var changeCount = 0;
        preferences.Changed += (_, _) => changeCount++;
        var window = new SettingsWindow(preferences)
        {
            WindowStartupLocation = WindowStartupLocation.Manual,
            Left = -20000,
            Top = -20000,
            ShowInTaskbar = false,
            ShowActivated = false
        };
        try
        {
            window.Show();
            PumpDispatcher();
            Check(window.FocusModeToggle.IsChecked == false && window.AnimationsToggle.IsChecked == true,
                "SettingsWindow must initially show Focus Mode off and animations enabled.");
            Check(changeCount == 0, "Opening SettingsWindow must leave the Focus Mode preferences untouched.");

            Check(preferences.Save(preferences.Current with { FocusModeEnabled = true }) is null,
                "Enabling Focus Mode externally must save cleanly.");
            PumpDispatcher();
            Check(window.FocusModeToggle.IsChecked == true && window.AnimationsToggle.IsChecked == true,
                "SettingsWindow must reflect externally enabled Focus Mode and keep the animation toggle checked.");
            Check(changeCount == 1, "Syncing enabled Focus Mode must raise only the external Changed event.");

            Check(preferences.Save(preferences.Current with { FocusModeEnabled = false }) is null,
                "Disabling Focus Mode externally must save cleanly.");
            PumpDispatcher();
            Check(window.FocusModeToggle.IsChecked == false && window.AnimationsToggle.IsChecked == true,
                "SettingsWindow must reflect externally disabled Focus Mode and keep the animation toggle checked.");
            Check(preferences.Current is { FocusModeEnabled: false, AnimationsEnabled: true, Widgets: ["Clock"], InstalledWidgets: ["Clock"] },
                "External Focus Mode toggles must preserve widget and animation preferences.");
        }
        finally
        {
            window.CloseForShutdown();
        }

        Check(changeCount == 2, "Each external Focus Mode toggle must raise exactly one Changed event through window close.");

        void PumpDispatcher()
        {
            // Allow the settings window's debounced autosave to fire if syncing queued one.
            var frame = new DispatcherFrame();
            var timer = new DispatcherTimer(TimeSpan.FromMilliseconds(300), DispatcherPriority.Background,
                (_, _) => frame.Continue = false, dispatcher);
            try
            {
                timer.Start();
                Dispatcher.PushFrame(frame);
            }
            finally
            {
                timer.Stop();
            }
        }
    }

    private static void CheckSettingsSearch(Application app, Action<bool, string> Check)
    {
        var entries = new SettingsSearchEntry[]
        {
            new(0, "Dock", "Opacity", "Window opacity for the dock"),
            new(1, "Themes", "Theme pack", "Choose a theme"),
            new(2, "Shortcuts", "Launcher hotkey", "Keyboard shortcut to open search"),
        };

        Check(SettingsSearchMatcher.Search(entries, null).Count == 0, "A null settings search query must return no matches.");
        Check(SettingsSearchMatcher.Search(entries, string.Empty).Count == 0, "An empty settings search query must return no matches.");
        Check(SettingsSearchMatcher.Search(entries, " \t ").Count == 0, "A whitespace settings search query must return no matches.");

        Check(SettingsSearchMatcher.Search(entries, "oPaCiTy").Any(entry => entry.Title == "Opacity"),
            "Settings search must match Title case-insensitively.");
        Check(SettingsSearchMatcher.Search(entries, "WINDOW").Any(entry => entry.Description.Contains("Window", StringComparison.Ordinal)),
            "Settings search must match Description case-insensitively.");
        Check(SettingsSearchMatcher.Search(entries, "tHeMeS").Any(entry => entry.PageName == "Themes"),
            "Settings search must match PageName case-insensitively.");

        var multiWord = SettingsSearchMatcher.Search(entries, "hotkey keyboard");
        Check(multiWord.Count == 1 && multiWord[0].Title == "Launcher hotkey",
            "A multi-word settings query must match only when every term hits Title, Description, or PageName.");
        Check(SettingsSearchMatcher.Search(entries, "zzznomatch").Count == 0, "A settings query with no hits must return an empty list.");

        var ordered = SettingsSearchMatcher.Search(entries, "e");
        Check(ordered.Count == entries.Length
            && ordered[0].Title == entries[0].Title
            && ordered[1].Title == entries[1].Title
            && ordered[2].Title == entries[2].Title,
            "Settings search must preserve source order.");

        var tempPath = Path.Combine(Path.GetTempPath(), $"ot-search-{Guid.NewGuid():N}.json");
        SettingsWindow? window = null;
        var changeCount = 0;
        try
        {
            var preferences = new ApplicationPreferences(new DockSettingsStore(tempPath));
            preferences.Changed += (_, _) => changeCount++;
            UiText.Current.SetLanguage(AppLanguage.English);

            window = new SettingsWindow(preferences)
            {
                Left = -10000,
                Top = -10000,
                ShowInTaskbar = false,
                ShowActivated = false,
            };
            window.Show();
            PumpDispatcher(app.Dispatcher, 120);

            var indexed = window.CollectSearchEntries();
            Check(indexed.Select(entry => entry.TabIndex).Distinct().Count() == 7,
                "Settings search must index rows from all 7 tabs.");
            Check(indexed.Count > 0 && indexed.All(entry => !string.IsNullOrWhiteSpace(entry.PageName) && !string.IsNullOrWhiteSpace(entry.Title)),
                "Every indexed settings row must have a page name and a title.");

            var opacityHits = SettingsSearchMatcher.Search(indexed, "opacity");
            Check(opacityHits.Count > 0, "Searching for opacity must find opacity rows.");
            Check(SettingsSearchMatcher.Search(indexed, "language").Any(entry =>
                    entry.Title.Contains("Language", StringComparison.CurrentCultureIgnoreCase)),
                "Searching for language must find the Language row.");
            Check(SettingsSearchMatcher.Search(indexed, "theme").Count > 0, "Searching for theme must find Theme rows.");
            var shortcutHits = SettingsSearchMatcher.Search(indexed, "keyboard");
            if (shortcutHits.Count == 0) shortcutHits = SettingsSearchMatcher.Search(indexed, "shortcut");
            Check(shortcutHits.Count > 0 && shortcutHits.Any(entry =>
                    entry.PageName.Contains("Shortcut", StringComparison.CurrentCultureIgnoreCase)
                    || entry.Title.Contains("Shortcut", StringComparison.CurrentCultureIgnoreCase)
                    || entry.Description.Contains("Shortcut", StringComparison.CurrentCultureIgnoreCase)
                    || entry.Title.Contains("Keyboard", StringComparison.CurrentCultureIgnoreCase)
                    || entry.Description.Contains("Keyboard", StringComparison.CurrentCultureIgnoreCase)),
                "Searching for keyboard or shortcut must find Shortcuts rows.");
            Check(SettingsSearchMatcher.Search(indexed, "zzznomatch").Count == 0, "A live no-match query must return no indexed rows.");

            window.SettingsSearchBox.Text = "opacity";
            Check(window.SettingsSearchOverlay.Visibility == Visibility.Visible, "An opacity query must show the search overlay.");
            Check(window.SettingsSearchResultsPanel.Children.Count > 0, "An opacity query must populate search results.");
            Check(window.SettingsSearchEmptyText.Visibility != Visibility.Visible, "An opacity query must hide the empty-search message.");
            var opacityTabs = opacityHits.Select(entry => entry.TabIndex).ToHashSet();
            Check(window.SettingsTabs.Items.Count == 7, "Settings must keep 7 tabs while search is active.");
            for (var i = 0; i < window.SettingsTabs.Items.Count; i++)
            {
                var tab = (TabItem)window.SettingsTabs.Items[i]!;
                var expected = opacityTabs.Contains(i) ? Visibility.Visible : Visibility.Collapsed;
                Check(tab.Visibility == expected, "Search must keep matching tabs visible and collapse tabs with no matches.");
            }

            window.SettingsSearchBox.Text = "zzznomatch";
            Check(window.SettingsSearchOverlay.Visibility == Visibility.Visible, "A no-match query must keep the search overlay visible.");
            Check(window.SettingsSearchEmptyText.Visibility == Visibility.Visible, "A no-match query must show the empty-search message.");
            Check(window.SettingsSearchResultsGroup.Visibility != Visibility.Visible, "A no-match query must hide the results group.");
            for (var i = 0; i < window.SettingsTabs.Items.Count; i++)
            {
                Check(((TabItem)window.SettingsTabs.Items[i]!).Visibility == Visibility.Collapsed,
                    "A no-match query must collapse all 7 tabs.");
            }

            window.SettingsSearchBox.Text = string.Empty;
            Check(window.SettingsSearchOverlay.Visibility != Visibility.Visible, "Clearing search must hide the search overlay.");
            for (var i = 0; i < window.SettingsTabs.Items.Count; i++)
            {
                Check(((TabItem)window.SettingsTabs.Items[i]!).Visibility == Visibility.Visible,
                    "Clearing search must restore all 7 tabs.");
            }

            UiText.Current.SetLanguage(AppLanguage.Turkish);
            var turkish = window.CollectSearchEntries();
            Check(SettingsSearchMatcher.Search(turkish, "saydam").Count > 0
                && SettingsSearchMatcher.Search(turkish, "SAYDAM").Count > 0,
                "Turkish search must find saydamlığı rows case-insensitively.");
            Check(SettingsSearchMatcher.Search(turkish, "DİL").Any(entry => entry.Title == UiText.Get("Language")),
                "Turkish search must find Dil rows case-insensitively.");

            Check(changeCount == 0, "Settings search and indexing must not mutate preferences.");
        }
        finally
        {
            if (window is not null)
            {
                window.Close();
                PumpDispatcher(app.Dispatcher, 80);
            }

            UiText.Current.SetLanguage(AppLanguage.English);
            try { File.Delete(tempPath); } catch (IOException) { }
            try { File.Delete(tempPath + ".tmp"); } catch (IOException) { }
        }
    }

    private static void PumpDispatcher(Dispatcher dispatcher, int milliseconds)
    {
        var frame = new DispatcherFrame();
        var timer = new DispatcherTimer(TimeSpan.FromMilliseconds(milliseconds), DispatcherPriority.Background,
            (_, _) => frame.Continue = false, dispatcher);
        timer.Start();
        Dispatcher.PushFrame(frame);
        timer.Stop();
    }

    private static void CheckThumbnailPreviewPeekAndMiddleClick(Application app, Action<bool, string> Check)
    {
        var anchor = new Border { Width = 80, Height = 32 };
        var host = new Window
        {
            Left = -10000,
            Top = -10000,
            Width = 120,
            Height = 40,
            ShowInTaskbar = false,
            ShowActivated = false,
            Content = anchor
        };
        WindowThumbnailPreview? preview = null;
        try
        {
            host.Show();
            PumpDispatcher(app.Dispatcher, 50);

            const nint hwnd1 = 0x1111;
            const nint hwnd2 = 0x2222;
            var win1 = new TrackedWindow(hwnd1, "One", null);
            var win2 = new TrackedWindow(hwnd2, "Two", null);
            var clock = 10_000L;
            var peekCalls = new List<(bool Show, nint Target, nint Caller)>();
            preview = new WindowThumbnailPreview((Style)app.FindResource("DockButton"))
            {
                PeekDelay = TimeSpan.FromMilliseconds(40),
                WindowExistsCheck = _ => true,
                ClockMs = () => clock,
                PeekAction = (show, target, caller) => peekCalls.Add((show, target, caller)),
                AnimationsEnabled = false
            };

            var dockHwnd = new System.Windows.Interop.WindowInteropHelper(host).Handle;
            Check(dockHwnd != 0, "The offscreen preview host must have a window handle.");

            void Enter(Border card) =>
                card.RaiseEvent(new MouseEventArgs(Mouse.PrimaryDevice, Environment.TickCount) { RoutedEvent = UIElement.MouseEnterEvent });
            void Leave(Border card) =>
                card.RaiseEvent(new MouseEventArgs(Mouse.PrimaryDevice, Environment.TickCount) { RoutedEvent = UIElement.MouseLeaveEvent });

            (Border Card1, Border Card2) ShowBoth()
            {
                Check(preview.ShowForWindows([win1, win2], anchor, null),
                    "Thumbnail preview must open for both tracked windows.");
                var card1 = preview.GetOverlayCardForTesting(hwnd1);
                var card2 = preview.GetOverlayCardForTesting(hwnd2);
                Check(card1 is not null && card2 is not null,
                    "Thumbnail preview must build an overlay card for each tracked window.");
                return (card1!, card2!);
            }

            void PeekAfterDelay(Border card, nint hwnd)
            {
                var before = peekCalls.Count;
                Enter(card);
                Check(peekCalls.Count == before && preview.PeekedWindow == 0,
                    "Peek must not fire before the delay elapses.");
                PumpDispatcher(app.Dispatcher, 90);
                Check(peekCalls.Count == before + 1 && peekCalls[^1] == (true, hwnd, dockHwnd) && preview.PeekedWindow == hwnd,
                    "Peek must show the hovered window for the dock after the delay.");
            }

            var (card1, card2) = ShowBoth();

            preview.PeekEnabledCheck = () => false;
            Enter(card1);
            PumpDispatcher(app.Dispatcher, 80);
            Check(peekCalls.Count == 0 && preview.PeekedWindow == 0,
                "Disabled peek must ignore hover and leave PeekedWindow at zero.");

            preview.PeekEnabledCheck = () => true;
            Leave(card1);
            Enter(card1);
            Check(peekCalls.Count == 0 && preview.PeekedWindow == 0,
                "Peek must wait for the delay and must not fire at 0ms.");
            PumpDispatcher(app.Dispatcher, 90);
            Check(peekCalls.Count == 1 && peekCalls[0] == (true, hwnd1, dockHwnd) && preview.PeekedWindow == hwnd1,
                "Peek must show hwnd1 for the dock after the delay.");

            Leave(card1);
            Check(peekCalls.Count == 2 && peekCalls[1] == (false, hwnd1, dockHwnd) && preview.PeekedWindow == 0,
                "Leaving a card must end the active peek.");

            clock += 50;
            var beforeSwitch = peekCalls.Count;
            Enter(card2);
            Check(peekCalls.Count == beforeSwitch + 1 && peekCalls[^1] == (true, hwnd2, dockHwnd) && preview.PeekedWindow == hwnd2,
                "A card switch inside the grace window must peek the new window immediately.");

            Leave(card2);
            Check(peekCalls[^1] == (false, hwnd2, dockHwnd) && preview.PeekedWindow == 0,
                "Leaving card 2 must end its peek before the grace window can expire.");
            clock += 400;
            var beforeExpired = peekCalls.Count;
            Enter(card1);
            Check(peekCalls.Count == beforeExpired && preview.PeekedWindow == 0,
                "A hover after the grace window must not peek until the timer elapses.");
            PumpDispatcher(app.Dispatcher, 90);
            Check(peekCalls[^1] == (true, hwnd1, dockHwnd) && preview.PeekedWindow == hwnd1,
                "Peek must start for card 1 once the delay elapses.");

            var previewClicks = 0;
            preview.PreviewClicked += (_, _) => previewClicks++;
            card1.RaiseEvent(new MouseButtonEventArgs(Mouse.PrimaryDevice, Environment.TickCount, MouseButton.Left)
            {
                RoutedEvent = UIElement.MouseLeftButtonDownEvent
            });
            Check(previewClicks == 1 && preview.SourceHandle == hwnd1
                && peekCalls[^1] == (false, hwnd1, dockHwnd) && preview.PeekedWindow == 0,
                "Primary click must raise PreviewClicked, select hwnd1, and end peek.");

            (card1, _) = ShowBoth();
            PeekAfterDelay(card1, hwnd1);
            preview.Hide();
            Check(peekCalls[^1] == (false, hwnd1, dockHwnd) && preview.PeekedWindow == 0,
                "Hide must end the active peek.");

            (card1, _) = ShowBoth();
            PeekAfterDelay(card1, hwnd1);
            preview.RemoveClosed([hwnd1], [win2]);
            Check(peekCalls[^1] == (false, hwnd1, dockHwnd) && preview.PeekedWindow == 0,
                "RemoveClosed must end peek for the closed card.");

            (card1, card2) = ShowBoth();
            var closeCount = 0;
            var clickCount = 0;
            preview.CloseRequested += (_, _) => closeCount++;
            preview.PreviewClicked += (_, _) => clickCount++;
            PeekAfterDelay(card2, hwnd2);
            // A stray middle MouseUp without a matching MouseDown on the same card must not close the window.
            card2.RaiseEvent(new MouseButtonEventArgs(Mouse.PrimaryDevice, Environment.TickCount, MouseButton.Middle)
            {
                RoutedEvent = UIElement.MouseUpEvent
            });
            Check(closeCount == 0 && preview.PeekedWindow == hwnd2,
                "A stray middle MouseUp without a prior MouseDown on the same card must not close the window.");
            card2.RaiseEvent(new MouseButtonEventArgs(Mouse.PrimaryDevice, Environment.TickCount, MouseButton.Middle)
            {
                RoutedEvent = UIElement.MouseDownEvent
            });
            card2.RaiseEvent(new MouseButtonEventArgs(Mouse.PrimaryDevice, Environment.TickCount, MouseButton.Middle)
            {
                RoutedEvent = UIElement.MouseUpEvent
            });
            Check(closeCount == 1 && preview.SourceHandle == hwnd2 && clickCount == 0 && preview.PeekedWindow == 0,
                "Middle-click must close only the hovered preview window and end peek.");

            var closeTarget = new Window
            {
                Width = 1,
                Height = 1,
                Left = -10000,
                Top = -10000,
                ShowInTaskbar = false,
                ShowActivated = false
            };
            var closeTargetClosed = false;
            closeTarget.Closed += (_, _) => closeTargetClosed = true;
            try
            {
                closeTarget.SourceInitialized += (_, _) =>
                {
                    if (PresentationSource.FromVisual(closeTarget) is not System.Windows.Interop.HwndSource source) return;
                    source.AddHook((nint _, int message, nint wParam, nint _, ref bool handled) =>
                    {
                        if (message == 0x0112 && (wParam.ToInt64() & 0xFFF0) == 0xF060)
                        {
                            handled = true;
                            closeTarget.Close();
                        }
                        return 0;
                    });
                };
                closeTarget.Show();
                var closeTargetHandle = new System.Windows.Interop.WindowInteropHelper(closeTarget).Handle;
                preview.CloseRequested += (_, _) => NativeWindowTracker.RequestClose(preview.SourceHandle);
                Check(preview.ShowForWindows([new TrackedWindow(closeTargetHandle, "Close target", null)], anchor, null),
                    "The preview must open for a closeable test window.");
                var closeButton = preview.GetCloseButtonForTesting(closeTargetHandle);
                Check(closeButton is not null, "Each preview card must expose its close button.");
                closeButton!.RaiseEvent(new RoutedEventArgs(System.Windows.Controls.Primitives.ButtonBase.ClickEvent));
                PumpDispatcher(app.Dispatcher, 100);
                Check(closeTargetClosed && closeCount == 2,
                    "Clicking a preview card's close button must request and close its source window.");
            }
            finally
            {
                if (!closeTargetClosed) closeTarget.Close();
            }

            preview.PeekAction = (_, _, _) => throw new ExternalException("simulated DWM failure");
            (card1, _) = ShowBoth();
            Enter(card1);
            PumpDispatcher(app.Dispatcher, 90);
            Check(preview.PeekedWindow == 0,
                "A failing peek must stay unhandled-exception free and leave PeekedWindow at zero.");
        }
        finally
        {
            preview?.Dispose();
            host.Close();
            PumpDispatcher(app.Dispatcher, 50);
        }
    }

    private static void CheckDockProfiles(Action<bool, string> Check)
    {
        var tempPath = Path.Combine(Path.GetTempPath(), $"ot-profiles-{Guid.NewGuid():N}.json");
        try
        {
            var store = new DockSettingsStore(tempPath);
            Check(!File.Exists(tempPath), "The first-launch profile fixture must start without a settings file.");
            var (missingFile, missingError) = store.Load();
            Check(missingError is null && HasDefaultProfile(missingFile),
                "Loading a missing settings file must seed and activate exactly one Default profile.");

            File.WriteAllText(tempPath, "{}");
            var (emptyJson, emptyError) = store.Load();
            Check(emptyError is null && HasDefaultProfile(emptyJson),
                "Loading an empty settings object must seed and activate exactly one Default profile.");

            foreach (var settings in new[] { new DockSettings(Profiles: null), new DockSettings(Profiles: []) })
            {
                var seeded = DockProfileService.EnsureDefaultProfile(settings);
                Check(HasDefaultProfile(seeded), "EnsureDefaultProfile must seed Default for null and empty profile lists.");
            }

            CheckLegacyDockProfiles(store, tempPath, Check);
            CheckDockProfileLifecycle(store, Check);
        }
        finally
        {
            File.Delete(tempPath);
        }

        CheckDockProfileValidation(Check);
        CheckLocalization("dock profile localization",
            ["ProfileRename", "ProfileRenamePrompt", "ErrorProfileNameEmpty", "ErrorProfileNotFound", "ErrorProfileDeleteLast"], Check);

        static bool HasDefaultProfile(DockSettings settings) =>
            settings is { ActiveProfile: "Default", Profiles: [{ Name: "Default" }] };
    }

    private static void CheckLegacyDockProfiles(DockSettingsStore store, string tempPath, Action<bool, string> Check)
    {
        File.WriteAllText(tempPath, """
            {
              "Profiles": [
                {"Name":"Work","HiddenApps":["C:\\Games\\game.exe"]},
                {"Name":"Gaming","HiddenApps":[]}
              ],
              "ActiveProfile": "Work",
              "Widgets": ["Clock"],
              "InstalledWidgets": ["Clock", "Weather"],
              "WidgetOrder": ["Weather", "Clock"],
              "DockItemOrder": ["widget:Clock", "start"],
              "DockWidthMode": "Percent",
              "DockWidthPixels": 1100,
              "DockWidthPercent": 75,
              "Theme": "Dark",
              "ThemePack": "Neon",
              "ThemeApplicationIcons": false,
              "LauncherHotkey": "CtrlSpace",
              "DockHotkey": "WinAltD",
              "AutoHide": "Always"
            }
            """);
        var (legacy, loadError) = store.Load();
        Check(loadError is null && legacy is { ActiveProfile: "Work", Profiles: [{ Name: "Work" }, { Name: "Gaming" }] },
            "Legacy two-property profiles must deserialize with their original names and active profile.");
        Check(legacy is { Widgets: ["Clock"], DockWidthMode: DockWidthMode.Percent, ThemePack: "Neon" },
            "Loading legacy profiles must retain the existing live widget, width, and theme settings.");
        var legacyWork = legacy.Profiles![0];
        var legacyGaming = legacy.Profiles![1];
        Check(legacyWork.HiddenApps.SequenceEqual([@"C:\Games\game.exe"]) && legacyGaming.HiddenApps.Count == 0,
            "Legacy profiles must deserialize both populated and empty hidden-app lists.");
        CheckProfileFields(new DockProfile("Work", []), legacyWork, "Deserializing legacy Work", Check);
        CheckProfileFields(new DockProfile("Gaming", []), legacyGaming, "Deserializing legacy Gaming", Check);
        var beforeEnsure = SerializeSettings(legacy);
        var ensured = DockProfileService.EnsureDefaultProfile(legacy);
        Check(SerializeSettings(ensured) == beforeEnsure && SerializeSettings(legacy) == beforeEnsure,
            "EnsureDefaultProfile must preserve existing profiles and their active name.");

        var (gaming, gamingError) = DockProfileService.Switch(legacy, "Gaming");
        Check(gamingError is null && gaming.ActiveProfile == "Gaming",
            "Switching to a legacy profile must update ActiveProfile.");
        CheckProfileFields(LiveProfileFields(legacy), LiveProfileFields(gaming), "Switching to legacy Gaming", Check);

        var edited = gaming with { Theme = AppTheme.Light, DockWidthPercent = 55 };
        var synced = DockProfileService.SyncActive(edited);
        Check(synced.ActiveProfile == "Gaming" && synced.Profiles![1].HiddenApps.Count == 0,
            "SyncActive must keep the legacy target active and preserve its hidden apps.");
        CheckProfileFields(LiveProfileFields(edited), synced.Profiles![1], "Syncing legacy Gaming", Check);
        CheckProfileFields(LiveProfileFields(legacy), synced.Profiles![0], "Preserving the outgoing Work snapshot", Check);

        var (work, workError) = DockProfileService.Switch(synced, "Work");
        Check(workError is null && work.ActiveProfile == "Work"
            && work.Profiles![0].HiddenApps.SequenceEqual([@"C:\Games\game.exe"]),
            "Switching back to Work must restore its active name and keep its legacy hidden apps.");
        CheckProfileFields(LiveProfileFields(legacy), LiveProfileFields(work), "Returning to legacy Work", Check);
    }

    private static void CheckDockProfileLifecycle(DockSettingsStore store, Action<bool, string> Check)
    {
        var defaultFields = new DockSettings(
            Widgets: ["Clock"], InstalledWidgets: ["Clock", "Weather"], WidgetOrder: ["Weather", "Clock"],
            DockItemOrder: null, DockWidthMode: DockWidthMode.Auto, DockWidthPixels: 900, DockWidthPercent: 60,
            Theme: AppTheme.System, ThemePack: null, ThemeApplicationIcons: true,
            LauncherHotkey: HotkeyChoice.AltSpace, DockHotkey: HotkeyChoice.CtrlAltD, AutoHide: AutoHideMode.Disabled);
        var workFields = new DockSettings(
            Widgets: ["Focus"], InstalledWidgets: ["Focus", "Storage"], WidgetOrder: ["Storage", "Focus"],
            DockItemOrder: ["widget:Focus", "start"], DockWidthMode: DockWidthMode.Pixels, DockWidthPixels: 1200, DockWidthPercent: 70,
            Theme: AppTheme.Light, ThemePack: "Neon", ThemeApplicationIcons: false,
            LauncherHotkey: HotkeyChoice.CtrlSpace, DockHotkey: HotkeyChoice.WinAltD, AutoHide: AutoHideMode.Smart);
        var gamingFields = new DockSettings(
            Widgets: ["Media"], InstalledWidgets: ["Media", "Audio"], WidgetOrder: ["Audio", "Media"],
            DockItemOrder: ["start", "widget:Media"], DockWidthMode: DockWidthMode.Percent, DockWidthPixels: 1600, DockWidthPercent: 85,
            Theme: AppTheme.Dark, ThemePack: "Arcade", ThemeApplicationIcons: true,
            LauncherHotkey: HotkeyChoice.CtrlAltSpace, DockHotkey: HotkeyChoice.Disabled, AutoHide: AutoHideMode.Always);
        string[] defaultHidden = [@"C:\Apps\default.exe"];
        string[] workHidden = [@"C:\Apps\work.exe"];
        string[] gamingHidden = [@"C:\Games\game.exe"];

        var defaults = DockProfileService.UpdateHiddenApps(
            DockProfileService.EnsureDefaultProfile(defaultFields), "Default", defaultHidden);
        var (createdWork, createWorkError) = DockProfileService.Create(defaults, "Work");
        Check(createWorkError is null && createdWork is { ActiveProfile: "Work", Profiles: { Count: 2 } },
            "Create must append and activate Work.");
        CheckProfileSnapshot(createdWork, "Work", WithoutWidgets(defaultFields), [], "Creating Work from Default", Check);

        var work = DockProfileService.UpdateHiddenApps(
            workFields with { Profiles = createdWork.Profiles, ActiveProfile = createdWork.ActiveProfile }, "Work",
            [workHidden[0], @"c:\apps\WORK.exe"]);
        var (createdGaming, createGamingError) = DockProfileService.Create(work, "Gaming");
        Check(createGamingError is null && createdGaming is { ActiveProfile: "Gaming", Profiles: { Count: 3 } },
            "Create must append and activate Gaming.");
        CheckProfileSnapshot(createdGaming, "Gaming", WithoutWidgets(workFields), [], "Creating Gaming from Work", Check);
        CheckProfileSnapshot(createdGaming, "Work", workFields, workHidden, "Capturing Work before creating Gaming", Check);

        var gaming = DockProfileService.UpdateHiddenApps(
            gamingFields with { Profiles = createdGaming.Profiles, ActiveProfile = createdGaming.ActiveProfile }, "Gaming", gamingHidden);
        var workActive = SwitchTo(gaming, "Work", workFields, workHidden);

        // Edit globals after creating all snapshots so stale profile data cannot satisfy the isolation checks.
        var globals = workActive with
        {
            Language = AppLanguage.Japanese,
            Position = DockPosition.Right,
            Edge = DockEdge.Left,
            Size = DockSize.Large,
            DockScale = 1.25,
            WeatherCity = "Istanbul",
            CheckUpdates = false
        };
        var defaultActive = SwitchTo(globals, "Default", defaultFields, defaultHidden);
        Check(defaultActive is { ThemePack: null, DockItemOrder: null, DockWidthMode: DockWidthMode.Auto },
            "Switching from Work to Default must clear ThemePack and DockItemOrder and restore automatic width.");
        var gamingActive = SwitchTo(defaultActive, "Gaming", gamingFields, gamingHidden);
        workActive = SwitchTo(gamingActive, "Work", workFields, workHidden);

        CheckDockProfileRenameAndDelete(workActive, defaultActive, workFields, workHidden, gamingFields, gamingHidden, Check);
        CheckDockProfilePersistence(store, workActive, defaultFields, defaultHidden, Check);

        static DockSettings WithoutWidgets(DockSettings settings) => settings with
        {
            Widgets = [],
            InstalledWidgets = [],
            WidgetOrder = null,
            DockItemOrder = settings.DockItemOrder?
                .Where(token => !token.StartsWith("widget:", StringComparison.OrdinalIgnoreCase)).ToArray()
        };

        DockSettings SwitchTo(DockSettings source, string name, DockSettings expected, IReadOnlyList<string> hiddenApps)
        {
            var (switched, error) = DockProfileService.Switch(source, name);
            Check(error is null && switched.ActiveProfile == name, $"Switch must activate {name}.");
            CheckProfileFields(LiveProfileFields(expected), LiveProfileFields(switched), $"Switching to {name}", Check);
            CheckProfileSnapshot(switched, name, expected, hiddenApps, $"Restoring the {name} snapshot", Check);
            CheckGlobalSettings(source, switched, $"Switching to {name}", Check);
            return switched;
        }
    }

    private static void CheckDockProfileRenameAndDelete(
        DockSettings work, DockSettings defaults,
        DockSettings workFields, IReadOnlyList<string> workHidden,
        DockSettings gamingFields, IReadOnlyList<string> gamingHidden, Action<bool, string> Check)
    {
        var (renamedActive, activeError) = DockProfileService.Rename(work, "Work", "  Workroom  ");
        Check(activeError is null && renamedActive.ActiveProfile == "Workroom"
            && renamedActive.Profiles!.Select(profile => profile.Name).SequenceEqual(["Default", "Workroom", "Gaming"]),
            "Renaming the active profile must trim the name and update ActiveProfile.");
        CheckProfileSnapshot(renamedActive, "Workroom", workFields, workHidden, "Renaming active Work", Check);

        var (caseOnly, caseError) = DockProfileService.Rename(renamedActive, "Workroom", "WORKROOM");
        Check(caseError is null && caseOnly.ActiveProfile == "WORKROOM",
            "Renaming a profile with only a case change must succeed and update the active name.");
        CheckProfileSnapshot(caseOnly, "WORKROOM", workFields, workHidden, "Changing the case of Workroom", Check);

        var (renamedInactive, inactiveError) = DockProfileService.Rename(caseOnly, "Gaming", "  Play  ");
        Check(inactiveError is null && renamedInactive.ActiveProfile == "WORKROOM"
            && renamedInactive.Profiles!.Select(profile => profile.Name).SequenceEqual(["Default", "WORKROOM", "Play"]),
            "Renaming an inactive profile must trim the name and preserve ActiveProfile.");
        CheckProfileSnapshot(renamedInactive, "Play", gamingFields, gamingHidden, "Renaming inactive Gaming", Check);
        CheckProfileFields(LiveProfileFields(work), LiveProfileFields(renamedInactive), "Renaming profiles", Check);

        var beforeRename = SerializeSettings(renamedInactive);
        foreach (var (currentName, newName, expectedError) in new[]
        {
            ("Play", " workroom ", "ErrorProfileExists"),
            ("WORKROOM", "", "ErrorProfileNameEmpty"),
            ("WORKROOM", " \t ", "ErrorProfileNameEmpty"),
            ("Missing", "Personal", "ErrorProfileNotFound")
        })
        {
            var (unchanged, error) = DockProfileService.Rename(renamedInactive, currentName, newName);
            Check(error == expectedError && SerializeSettings(unchanged) == beforeRename
                && SerializeSettings(renamedInactive) == beforeRename,
                $"Rename must return {expectedError} and preserve the original settings.");
        }

        var (removedInactive, removeInactiveError) = DockProfileService.Delete(work, "Gaming");
        Check(removeInactiveError is null && removedInactive.ActiveProfile == "Work"
            && removedInactive.Profiles!.Select(profile => profile.Name).SequenceEqual(["Default", "Work"]),
            "Deleting inactive Gaming must remove it and keep Work active.");
        CheckProfileFields(LiveProfileFields(work), LiveProfileFields(removedInactive), "Deleting inactive Gaming", Check);

        var (removedActive, removeActiveError) = DockProfileService.Delete(defaults, "Default");
        Check(removeActiveError is null && removedActive.ActiveProfile == "Work"
            && removedActive.Profiles!.Select(profile => profile.Name).SequenceEqual(["Work", "Gaming"]),
            "Deleting active Default must activate Work, the first remaining profile.");
        CheckProfileFields(LiveProfileFields(workFields), LiveProfileFields(removedActive), "Deleting active Default", Check);
        CheckProfileSnapshot(removedActive, "Work", workFields, workHidden, "Activating Work after deletion", Check);
        CheckGlobalSettings(defaults, removedActive, "Deleting active Default", Check);

        var (lastProfile, removeGamingError) = DockProfileService.Delete(removedActive, "Gaming");
        Check(removeGamingError is null && lastProfile is { ActiveProfile: "Work", Profiles: [{ Name: "Work" }] },
            "Deleting the other inactive profile must leave only Work.");
        var beforeDelete = SerializeSettings(lastProfile);
        var (refused, lastError) = DockProfileService.Delete(lastProfile, "Work");
        Check(lastError == "ErrorProfileDeleteLast" && SerializeSettings(refused) == beforeDelete
            && SerializeSettings(lastProfile) == beforeDelete,
            "Deleting the last profile must return ErrorProfileDeleteLast and preserve the settings.");
    }

    private static void CheckDockProfilePersistence(
        DockSettingsStore store, DockSettings work, DockSettings defaultFields, IReadOnlyList<string> defaultHidden,
        Action<bool, string> Check)
    {
        Check(store.Save(work) is null, "The Work profile fixture must save to the temporary settings file.");
        var preferences = new ApplicationPreferences(store);
        Check(preferences.LoadError is null && preferences.Current.ActiveProfile == "Work",
            "ApplicationPreferences must open the settings file with Work active.");
        CheckProfileFields(LiveProfileFields(work), LiveProfileFields(preferences.Current), "Opening Work before editing", Check);

        var edited = preferences.Current with
        {
            Widgets = ["Countdown"],
            InstalledWidgets = ["Countdown", "DayProgress"],
            WidgetOrder = ["DayProgress", "Countdown"],
            DockItemOrder = ["widget:Countdown", "search", "start"],
            DockWidthMode = DockWidthMode.Fill,
            DockWidthPixels = 1420,
            DockWidthPercent = 92,
            Theme = AppTheme.Dark,
            ThemePack = null,
            ThemeApplicationIcons = true,
            LauncherHotkey = HotkeyChoice.Disabled,
            DockHotkey = HotkeyChoice.CtrlAltD,
            AutoHide = AutoHideMode.Always
        };
        string[] hiddenApps = [@"C:\Apps\edited.exe"];
        edited = DockProfileService.UpdateHiddenApps(edited, "Work", hiddenApps);

        var previousLanguage = UiText.Current.Language;
        var previousPack = ThemePacks.Current?.Id;
        var previousTheme = ThemeManager.Current;
        try
        {
            Check(preferences.Save(edited) is null && preferences.Current.ActiveProfile == "Work",
                "ApplicationPreferences.Save must persist the edits with Work active.");
            CheckProfileSnapshot(preferences.Current, "Work", edited, hiddenApps, "Saving the active Work snapshot", Check);

            var (defaults, defaultError) = DockProfileService.Switch(preferences.Current, "Default");
            Check(defaultError is null && defaults.ActiveProfile == "Default", "Switching away from saved Work must activate Default.");
            CheckProfileFields(LiveProfileFields(defaultFields), LiveProfileFields(defaults), "Switching away from saved Work", Check);
            CheckProfileSnapshot(defaults, "Default", defaultFields, defaultHidden, "Preserving Default after saving Work", Check);
            var (restoredWork, workError) = DockProfileService.Switch(defaults, "Work");
            Check(workError is null && restoredWork.ActiveProfile == "Work", "Switching back must reactivate saved Work.");
            CheckProfileFields(LiveProfileFields(edited), LiveProfileFields(restoredWork), "Switching back to saved Work", Check);
            CheckProfileSnapshot(restoredWork, "Work", edited, hiddenApps, "Restoring saved Work", Check);

            var (reloaded, loadError) = store.Load();
            Check(loadError is null && reloaded.ActiveProfile == "Work", "Reloading the settings file must retain Work as active.");
            CheckProfileFields(LiveProfileFields(edited), LiveProfileFields(reloaded), "Reloading live Work settings from disk", Check);
            CheckProfileSnapshot(reloaded, "Work", edited, hiddenApps, "Reloading the Work snapshot from disk", Check);
            CheckProfileSnapshot(reloaded, "Default", defaultFields, defaultHidden, "Reloading the Default snapshot from disk", Check);
            CheckGlobalSettings(work, reloaded, "Reloading Work from disk", Check);
        }
        finally
        {
            UiText.Current.SetLanguage(previousLanguage);
            ThemeManager.Apply(previousTheme);
            ThemePacks.Select(previousPack);
            ThemePacks.ApplyWidgets(previousTheme);
        }
    }

    private static void CheckDockProfileValidation(Action<bool, string> Check)
    {
        var profile = new DockProfile("Work", [], Widgets: ["Clock"], InstalledWidgets: ["Clock"]);
        var valid = new DockSettings(Profiles: [profile], ActiveProfile: "Work");
        Check(DockSettingsStore.Validate(valid), "The valid profile fixture must pass settings validation.");
        Check(!DockSettingsStore.Validate(valid with { Profiles = [profile, profile with { Name = "WORK" }] }),
            "Validation must reject profile names that differ only in case.");

        foreach (var (invalid, description) in new (DockProfile, string)[]
        {
            (profile with { Name = "" }, "an empty name"),
            (profile with { Name = " " }, "a whitespace-only name"),
            (profile with { Widgets = ["InvalidWidget"], InstalledWidgets = null }, "an invalid enabled widget ID"),
            (profile with { InstalledWidgets = ["Clock", "InvalidWidget"] }, "an invalid installed widget ID"),
            (profile with { InstalledWidgets = ["Weather"] }, "an enabled widget missing from InstalledWidgets"),
            (profile with { DockWidthPixels = 99 }, "a pixel width below 100"),
            (profile with { DockWidthPixels = 5001 }, "a pixel width above 5000"),
            (profile with { DockWidthPercent = 9 }, "a width percentage below 10"),
            (profile with { DockWidthPercent = 101 }, "a width percentage above 100"),
            (profile with { DockWidthMode = (DockWidthMode)99 }, "an undefined width mode"),
            (profile with { Theme = (AppTheme)99 }, "an undefined theme"),
            (profile with { LauncherHotkey = (HotkeyChoice)99 }, "an undefined launcher hotkey"),
            (profile with { DockHotkey = (HotkeyChoice)99 }, "an undefined dock hotkey"),
            (profile with { AutoHide = (AutoHideMode)99 }, "an undefined auto-hide mode")
        })
        {
            Check(!DockSettingsStore.Validate(valid with { Profiles = [invalid] }),
                $"Profile validation must reject {description}.");
        }
    }

    private static void CheckFullBackupArchive(Action<bool, string> Check)
    {
        const string themeName = "Midnight Custom";
        const string sourceThemeId = "midnight-source";
        PinBackup[] pins = [new PinBackup("App", @"C:\App\app.exe", null)];
        var settings = new DockSettings(Theme: AppTheme.Dark, DockOpacity: 0.75, Widgets: ["Clock"], InstalledWidgets: ["Clock"],
            ThemePack: sourceThemeId,
            Profiles: [
                new DockProfile("Backup", [], ThemePack: sourceThemeId),
                new DockProfile("Work", [], ThemePack: sourceThemeId.ToUpperInvariant()),
                new DockProfile("NordProfile", [], ThemePack: "nord")
            ],
            ActiveProfile: "Backup");
        DockSettings WithRemappedTheme(string? newThemeId) => settings with
        {
            ThemePack = newThemeId,
            Profiles = [
                new DockProfile("Backup", [], ThemePack: newThemeId),
                new DockProfile("Work", [], ThemePack: newThemeId),
                new DockProfile("NordProfile", [], ThemePack: "nord")
            ]
        };
        var tempPath = Path.GetTempPath();
        var stagingBefore = Directory.EnumerateDirectories(tempPath, "OpenTaskbar-backup-*")
            .ToHashSet(StringComparer.OrdinalIgnoreCase);
        var fixtureRoot = Directory.CreateTempSubdirectory("ot-full-backup-").FullName;
        try
        {
            var sourceThemesRoot = Path.Combine(fixtureRoot, "source-themes");
            var targetThemesRoot = Directory.CreateDirectory(Path.Combine(fixtureRoot, "target-themes")).FullName;
            var noThemeTargetRoot = Directory.CreateDirectory(Path.Combine(fixtureRoot, "no-theme-target")).FullName;
            var sourceThemeDir = Path.Combine(sourceThemesRoot, sourceThemeId);
            var iconPath = Path.Combine(ThemePack.IconFolder, "app.png");
            var themeJson = JsonSerializer.Serialize(new ThemePackManifest(Name: themeName));
            var iconBytes = EncodeSolidPng(16);
            Directory.CreateDirectory(Path.Combine(sourceThemeDir, ThemePack.IconFolder));
            File.WriteAllText(Path.Combine(sourceThemeDir, ThemePack.ManifestFileName), themeJson);
            File.WriteAllBytes(Path.Combine(sourceThemeDir, iconPath), iconBytes);

            var archivePath = Path.Combine(fixtureRoot, "custom" + FullBackupArchive.ExportExtension);
            var exportError = FullBackupArchive.Export(archivePath, settings, sourceThemesRoot, pinsOverride: pins);
            Check(exportError is null, $"Full backup export must succeed with an active custom theme. Error: {exportError}");
            var (preview, inspectError) = FullBackupArchive.Inspect(archivePath);
            CheckStagingCleanup("Inspecting the custom-theme backup");
            Check(inspectError is null && preview is { HasTheme: true, ThemeName: themeName },
                "Inspecting the custom-theme backup must report Midnight Custom.");
            CheckPayload(preview!.Settings, preview.Pins, settings, "Inspecting the custom-theme backup");

            CheckInvalidFullBackupArchives(archivePath, fixtureRoot, targetThemesRoot,
                SettingsBackup.Serialize(settings with { DockOpacity = 9.0 }, pins), CheckStagingCleanup, Check);

            var (imported, importedPins, importedThemeId, importError) =
                FullBackupArchive.Import(archivePath, restoreTheme: true, themesRoot: targetThemesRoot);
            CheckStagingCleanup("Importing the custom-theme backup");
            Check(importError is null && !string.IsNullOrWhiteSpace(importedThemeId) && imported?.ThemePack == importedThemeId,
                "Importing the custom-theme backup must activate the installed theme ID.");
            CheckPayload(imported, importedPins, WithRemappedTheme(importedThemeId), "Importing the custom-theme backup");
            var installedThemeDir = Path.Combine(targetThemesRoot, importedThemeId!);
            CheckThemeFiles(installedThemeDir, iconBytes);

            var editedIcon = EncodeSolidPng(8);
            File.WriteAllBytes(Path.Combine(installedThemeDir, iconPath), editedIcon);
            var (reimported, reimportedPins, duplicateId, duplicateError) =
                FullBackupArchive.Import(archivePath, restoreTheme: true, themesRoot: targetThemesRoot);
            CheckStagingCleanup("Reimporting the custom-theme backup");
            Check(duplicateError is null && duplicateId == importedThemeId + "-2" && reimported?.ThemePack == duplicateId,
                "Reimporting the custom-theme backup must activate a unique theme ID ending in -2.");
            CheckPayload(reimported, reimportedPins, WithRemappedTheme(duplicateId), "Reimporting the custom-theme backup");
            CheckThemeFiles(Path.Combine(targetThemesRoot, duplicateId!), iconBytes);
            CheckThemeFiles(installedThemeDir, editedIcon);

            var (withoutTheme, withoutThemePins, skippedId, skippedError) =
                FullBackupArchive.Import(archivePath, restoreTheme: false, themesRoot: noThemeTargetRoot);
            CheckStagingCleanup("Importing with theme restoration disabled");
            Check(skippedError is null && skippedId is null && !Directory.EnumerateFileSystemEntries(noThemeTargetRoot).Any(),
                "Importing with theme restoration disabled must leave the target directory empty.");
            CheckPayload(withoutTheme, withoutThemePins, settings, "Importing with theme restoration disabled");

            var builtIn = settings with { ThemePack = null };
            var builtInPath = Path.Combine(fixtureRoot, "built-in" + FullBackupArchive.ExportExtension);
            var builtInExportError = FullBackupArchive.Export(builtInPath, builtIn, sourceThemesRoot, pinsOverride: pins);
            Check(builtInExportError is null, $"Full backup export must succeed with the built-in look. Error: {builtInExportError}");
            var (builtInPreview, builtInInspectError) = FullBackupArchive.Inspect(builtInPath);
            CheckStagingCleanup("Inspecting the built-in backup");
            Check(builtInInspectError is null && builtInPreview is { HasTheme: false, ThemeName: null },
                "Inspecting the built-in backup must report no bundled theme.");
            CheckPayload(builtInPreview!.Settings, builtInPreview.Pins, builtIn, "Inspecting the built-in backup");
            var (builtInImport, builtInPins, builtInId, builtInError) =
                FullBackupArchive.Import(builtInPath, restoreTheme: true, themesRoot: noThemeTargetRoot);
            CheckStagingCleanup("Importing the built-in backup");
            Check(builtInError is null && builtInId is null && !Directory.EnumerateFileSystemEntries(noThemeTargetRoot).Any(),
                "Importing the built-in backup must leave the target directory empty even with theme restoration enabled.");
            CheckPayload(builtInImport, builtInPins, builtIn, "Importing the built-in backup");

            void CheckThemeFiles(string directory, byte[] expectedIcon)
            {
                var manifest = Path.Combine(directory, ThemePack.ManifestFileName);
                var icon = Path.Combine(directory, iconPath);
                Check(File.Exists(manifest) && File.ReadAllText(manifest) == themeJson,
                    $"Theme {Path.GetFileName(directory)} must retain its manifest.");
                Check(File.Exists(icon) && File.ReadAllBytes(icon).SequenceEqual(expectedIcon),
                    $"Theme {Path.GetFileName(directory)} must retain its own icon contents.");
            }
        }
        finally
        {
            Directory.Delete(fixtureRoot, recursive: true);
        }

        CheckLocalization("full backup localization", [
            "FullBackup", "FullBackupDescription", "FullBackupExport", "FullBackupImport", "FullBackupFileFilter",
            "FullBackupExportDone", "FullBackupImportDone", "FullBackupRestoreThemePrompt", "ErrorFullBackupInvalid", "ErrorFullBackupWrite"
        ], Check);

        void CheckPayload(DockSettings? actual, IReadOnlyList<PinBackup> actualPins, DockSettings expected, string context)
        {
            Check(actual is not null && SerializeSettings(actual) == SerializeSettings(expected), $"{context} must preserve the dock settings.");
            Check(actualPins.SequenceEqual(pins), $"{context} must preserve the pinned apps.");
        }

        void CheckStagingCleanup(string context) =>
            Check(stagingBefore.SetEquals(Directory.EnumerateDirectories(tempPath, "OpenTaskbar-backup-*")),
                $"{context} must clean up temporary backup staging directories.");
    }

    private static void CheckInvalidFullBackupArchives(string validArchivePath, string fixtureRoot, string targetThemesRoot,
        string invalidSettingsJson, Action<string> checkStagingCleanup, Action<bool, string> Check)
    {
        const string invalidError = "ErrorFullBackupInvalid";
        const string themeManifest = "theme/theme.json";
        var corruptPath = ArchivePath("non-zip");
        File.WriteAllText(corruptPath, "This is not a zip archive.");
        CheckRejected(corruptPath);

        CheckInvalidZip("missing-manifest", zip => zip.GetEntry("manifest.json")!.Delete());
        CheckInvalidZip("unsupported-version", zip =>
        {
            var entry = zip.GetEntry("manifest.json")!;
            JsonNode manifest;
            using (var stream = entry.Open())
                manifest = JsonNode.Parse(stream)!;
            manifest["Version"] = 99;
            WriteEntry(zip, "manifest.json", manifest.ToJsonString());
        });
        CheckInvalidZip("invalid-settings", zip => WriteEntry(zip, "settings.json", invalidSettingsJson));
        CheckInvalidZip("path-traversal", zip => WriteEntry(zip, "../evil.json", "{}"));
        CheckInvalidZip("disallowed-extension", zip => WriteEntry(zip, "theme/payload.exe", "fixture"));
        CheckInvalidZip("missing-theme-manifest", zip => zip.GetEntry(themeManifest)!.Delete());
        CheckInvalidZip("invalid-theme-manifest", zip => WriteEntry(zip, themeManifest, "{ invalid JSON"));

        string ArchivePath(string name) => Path.Combine(fixtureRoot, name + FullBackupArchive.ExportExtension);

        void CheckInvalidZip(string name, Action<ZipArchive> mutate)
        {
            var path = ArchivePath(name);
            File.Copy(validArchivePath, path);
            using (var zip = ZipFile.Open(path, ZipArchiveMode.Update))
                mutate(zip);
            CheckRejected(path);
        }

        void CheckRejected(string path)
        {
            var name = Path.GetFileName(path);
            var (preview, inspectError) = FullBackupArchive.Inspect(path);
            checkStagingCleanup($"Inspecting {name}");
            Check(preview is null && inspectError == invalidError, $"Inspecting {name} must return {invalidError}.");
            var (settings, pins, importedId, importError) =
                FullBackupArchive.Import(path, restoreTheme: true, themesRoot: targetThemesRoot);
            checkStagingCleanup($"Importing {name}");
            Check(settings is null && pins.Count == 0 && importedId is null && importError == invalidError,
                $"Importing {name} must return {invalidError} without settings, pins, or a theme ID.");
            Check(!Directory.EnumerateFileSystemEntries(targetThemesRoot).Any(), $"Importing {name} must leave the target directory empty.");
        }

        static void WriteEntry(ZipArchive zip, string name, string content)
        {
            zip.GetEntry(name)?.Delete();
            using var writer = new StreamWriter(zip.CreateEntry(name).Open());
            writer.Write(content);
        }
    }

    private static void CheckLocalization(string context, IReadOnlyList<string> keys, Action<bool, string> Check)
    {
        var assembly = typeof(LanguageCatalog).Assembly;
        var resources = assembly.GetManifestResourceNames()
            .Where(name => name.StartsWith("OpenTaskbar.Localization.Languages.", StringComparison.Ordinal))
            .ToArray();
        Check(resources.Length == 14, $"There must be 14 embedded language resources for {context}.");
        foreach (var resource in resources)
        {
            using var stream = assembly.GetManifestResourceStream(resource)!;
            Check(stream.ReadByte() == 0x7B, $"{resource} must start with '{{' and have no UTF-8 BOM.");
            stream.Position = 0;
            using var document = JsonDocument.Parse(stream);
            foreach (var key in keys)
            {
                Check(document.RootElement.ValueKind == JsonValueKind.Object
                    && document.RootElement.TryGetProperty(key, out var value)
                    && value.ValueKind == JsonValueKind.String && !string.IsNullOrWhiteSpace(value.GetString()),
                    $"{resource} must have a non-empty translation for {key}.");
            }
        }
    }

    private static void CheckCustomHotkeys(Action<bool, string> Check)
    {
        void CheckParsed(string text, string display, uint modifiers, Key key, string message)
        {
            var expectedVk = (uint)KeyInterop.VirtualKeyFromKey(key);
            Check(HotkeyBinding.TryParse(text, out var binding)
                && binding.DisplayText == display
                && binding.Modifiers == modifiers
                && binding.VirtualKey == expectedVk
                && HotkeyBinding.TryParse(binding.DisplayText, out var roundTrip)
                && roundTrip.DisplayText == display
                && roundTrip.Modifiers == modifiers
                && roundTrip.VirtualKey == expectedVk,
                message);
        }

        CheckParsed("Ctrl+Alt+D", "Ctrl+Alt+D", HotkeyBinding.ModControl | HotkeyBinding.ModAlt, Key.D,
            "Ctrl+Alt+D must parse to its canonical binding and round-trip.");
        CheckParsed("ctrl+shift+k", "Ctrl+Shift+K", HotkeyBinding.ModControl | HotkeyBinding.ModShift, Key.K,
            "A lowercase Ctrl+Shift combo must canonicalize and round-trip.");
        CheckParsed("Alt+Space", "Alt+Space", HotkeyBinding.ModAlt, Key.Space,
            "Alt+Space must parse to its canonical binding and round-trip.");
        CheckParsed("Ctrl+Alt+Shift+F5", "Ctrl+Alt+Shift+F5",
            HotkeyBinding.ModControl | HotkeyBinding.ModAlt | HotkeyBinding.ModShift, Key.F5,
            "Ctrl+Alt+Shift+F5 must parse to its canonical binding and round-trip.");
        CheckParsed("Ctrl++", "Ctrl++", HotkeyBinding.ModControl, Key.OemPlus,
            "Ctrl++ must parse the OemPlus key and round-trip.");

        foreach (var invalid in new string?[]
        {
            null, "", "Disabled", "K", "Shift+K", "Win+Alt+D", "LWin+D",
            "Ctrl+Ctrl+A", "Ctrl+A+B", "Ctrl+Escape", "Ctrl+Snapshot", "Ctrl+99"
        })
        {
            Check(!HotkeyBinding.TryParse(invalid, out _),
                $"TryParse must reject '{invalid ?? "<null>"}'.");
        }

        void CheckKeyEvent(ModifierKeys modifiers, Key key, string display, string message)
        {
            Check(HotkeyBinding.TryFromKeyEvent(modifiers, key, out var binding, out var error)
                && error is null
                && binding.DisplayText == display,
                message);
        }

        CheckKeyEvent(ModifierKeys.Control | ModifierKeys.Shift, Key.K, "Ctrl+Shift+K",
            "Ctrl+Shift+K must be accepted from a key event.");
        CheckKeyEvent(ModifierKeys.Alt, Key.Space, "Alt+Space",
            "Alt+Space must be accepted from a key event.");
        CheckKeyEvent(ModifierKeys.Control, Key.D5, "Ctrl+5",
            "Ctrl+5 must be accepted from a key event.");

        foreach (var modifierKey in new[] { Key.LeftCtrl, Key.LeftAlt, Key.LeftShift, Key.LWin, Key.System })
        {
            Check(!HotkeyBinding.TryFromKeyEvent(ModifierKeys.Control, modifierKey, out _, out var error) && error is null,
                $"A modifier-only {modifierKey} event must be ignored without an error key.");
        }

        Check(!HotkeyBinding.TryFromKeyEvent(ModifierKeys.Windows, Key.D, out _, out var winError)
            && winError == "HotkeyWinNotAllowed",
            "The Windows modifier must be rejected.");
        Check(!HotkeyBinding.TryFromKeyEvent(ModifierKeys.None, Key.A, out _, out var noneError)
            && noneError == "HotkeyModifierRequired",
            "A key with no modifier must be rejected.");
        Check(!HotkeyBinding.TryFromKeyEvent(ModifierKeys.Shift, Key.A, out _, out var shiftError)
            && shiftError == "HotkeyModifierRequired",
            "Shift alone must be rejected.");

        var resolvedPreset = HotkeyBinding.Resolve(HotkeyChoice.CtrlAltD, null);
        Check(resolvedPreset?.DisplayText == "Ctrl+Alt+D"
            && resolvedPreset.Value.Modifiers == (HotkeyBinding.ModControl | HotkeyBinding.ModAlt)
            && resolvedPreset.Value.VirtualKey == 0x44,
            "Resolving CtrlAltD with no custom text must return Ctrl+Alt+D.");
        Check(HotkeyBinding.Resolve(HotkeyChoice.WinAltD, null) is null,
            "Resolving WinAltD must return null so MOD_WIN is never registered.");
        Check(HotkeyBinding.Resolve(HotkeyChoice.AltSpace, "Disabled") is null,
            "Resolving a Disabled custom string must return null.");
        var resolvedCustom = HotkeyBinding.Resolve(HotkeyChoice.AltSpace, "Ctrl+Shift+P");
        Check(resolvedCustom?.DisplayText == "Ctrl+Shift+P"
            && resolvedCustom.Value.Modifiers == (HotkeyBinding.ModControl | HotkeyBinding.ModShift)
            && resolvedCustom.Value.VirtualKey == (uint)KeyInterop.VirtualKeyFromKey(Key.P),
            "A custom string must override the legacy AltSpace choice.");

        Check(HotkeyBinding.TryParse("ctrl+alt+d", out var lowerCtrlAltD)
            && HotkeyBinding.EqualsBinding(lowerCtrlAltD, HotkeyBinding.FromChoice(HotkeyChoice.CtrlAltD)),
            "EqualsBinding must treat differently cased inputs as the same hotkey.");
        Check(!HotkeyBinding.EqualsBinding(null, null),
            "Two Disabled hotkeys must not count as a conflict.");

        Check(HotkeyBinding.IsValidCustomString(null), "A null custom hotkey must be valid.");
        Check(HotkeyBinding.IsValidCustomString("Disabled"), "Disabled must be a valid custom hotkey.");
        Check(HotkeyBinding.IsValidCustomString("Ctrl+Shift+P"), "Ctrl+Shift+P must be a valid custom hotkey.");
        Check(!HotkeyBinding.IsValidCustomString("Win+D"), "Win+D must be rejected as a custom hotkey.");
        Check(!HotkeyBinding.IsValidCustomString("Shift+P"), "Shift+P must be rejected as a custom hotkey.");

        var tempPath = Path.Combine(Path.GetTempPath(), $"ot-hotkeys-{Guid.NewGuid():N}.json");
        try
        {
            var store = new DockSettingsStore(tempPath);
            var saved = new DockSettings(
                CustomLauncherHotkey: "Ctrl+Shift+L",
                CustomDockHotkey: "Disabled");
            Check(DockSettingsStore.Validate(saved), "Custom launcher and Disabled dock hotkeys must validate.");
            Check(store.Save(saved) is null, "Custom hotkeys must save.");
            var (loaded, loadError) = store.Load();
            Check(loadError is null
                && loaded.CustomLauncherHotkey == "Ctrl+Shift+L"
                && loaded.CustomDockHotkey == "Disabled"
                && DockSettingsStore.Validate(loaded),
                "Custom hotkeys must survive a settings restart and stay valid.");

            var invalid = saved with { CustomDockHotkey = "Win+D" };
            Check(!DockSettingsStore.Validate(invalid) && store.Save(invalid) is not null,
                "Win+D must fail validation and must not save.");

            var captured = DockProfileService.Capture("Home", saved);
            Check(captured.CustomLauncherHotkey == "Ctrl+Shift+L" && captured.CustomDockHotkey == "Disabled",
                "Capture must copy both custom hotkeys.");

            var (withWork, createWorkError) = DockProfileService.Create(saved, "Work");
            Check(createWorkError is null && withWork.ActiveProfile == "Work"
                && withWork.CustomLauncherHotkey == "Ctrl+Shift+L"
                && withWork.CustomDockHotkey == "Disabled",
                "Create must keep the live custom hotkeys on the new profile.");
            Check(ProfileHotkeys(withWork, "Work", "Ctrl+Shift+L", "Disabled")
                && ProfileHotkeys(withWork, "Default", "Ctrl+Shift+L", "Disabled"),
                "Create must snapshot the same custom hotkeys onto Work and Default.");

            var edited = withWork with
            {
                CustomLauncherHotkey = "Alt+Space",
                CustomDockHotkey = "Ctrl+Alt+D"
            };
            var (withGaming, createGamingError) = DockProfileService.Create(edited, "Gaming");
            Check(createGamingError is null && withGaming.ActiveProfile == "Gaming"
                && withGaming.CustomLauncherHotkey == "Alt+Space"
                && withGaming.CustomDockHotkey == "Ctrl+Alt+D",
                "Create must activate Gaming with the edited custom hotkeys.");
            Check(ProfileHotkeys(withGaming, "Gaming", "Alt+Space", "Ctrl+Alt+D")
                && ProfileHotkeys(withGaming, "Work", "Alt+Space", "Ctrl+Alt+D")
                && ProfileHotkeys(withGaming, "Default", "Ctrl+Shift+L", "Disabled"),
                "Create must capture the outgoing profile and leave the untouched profile unchanged.");

            var (backToWork, workError) = DockProfileService.Switch(withGaming, "Work");
            Check(workError is null
                && backToWork.CustomLauncherHotkey == "Alt+Space"
                && backToWork.CustomDockHotkey == "Ctrl+Alt+D"
                && ProfileHotkeys(backToWork, "Default", "Ctrl+Shift+L", "Disabled"),
                "Switch must restore the target profile's custom hotkeys.");

            var (backToDefault, defaultError) = DockProfileService.Switch(backToWork, "Default");
            Check(defaultError is null
                && backToDefault.CustomLauncherHotkey == "Ctrl+Shift+L"
                && backToDefault.CustomDockHotkey == "Disabled"
                && ProfileHotkeys(backToDefault, "Work", "Alt+Space", "Ctrl+Alt+D")
                && ProfileHotkeys(backToDefault, "Gaming", "Alt+Space", "Ctrl+Alt+D"),
                "Switch must restore Default and keep each other profile's custom hotkeys.");
        }
        finally
        {
            try { File.Delete(tempPath); } catch (IOException) { }
        }

        static bool ProfileHotkeys(DockSettings settings, string name, string? launcher, string? dock)
        {
            var profile = settings.Profiles?.SingleOrDefault(item => item.Name == name);
            return profile is not null
                && profile.CustomLauncherHotkey == launcher
                && profile.CustomDockHotkey == dock;
        }
    }

    private static void CheckProfileSnapshot(
        DockSettings settings, string name, DockSettings expected, IReadOnlyList<string> hiddenApps,
        string context, Action<bool, string> Check)
    {
        var profile = settings.Profiles?.SingleOrDefault(item => item.Name == name);
        Check(profile is not null && profile.HiddenApps.SequenceEqual(hiddenApps), $"{context} must preserve the {name} hidden apps.");
        CheckProfileFields(LiveProfileFields(expected), profile!, context, Check);
    }

    private static DockProfile LiveProfileFields(DockSettings settings) => new(
        Name: string.Empty, HiddenApps: [], Widgets: settings.Widgets, InstalledWidgets: settings.InstalledWidgets,
        WidgetOrder: settings.WidgetOrder, DockItemOrder: settings.DockItemOrder, DockWidthMode: settings.DockWidthMode,
        DockWidthPixels: settings.DockWidthPixels, DockWidthPercent: settings.DockWidthPercent, Theme: settings.Theme,
        ThemePack: settings.ThemePack, ThemeApplicationIcons: settings.ThemeApplicationIcons, LauncherHotkey: settings.LauncherHotkey,
        DockHotkey: settings.DockHotkey, AutoHide: settings.AutoHide);

    private static void CheckProfileFields(DockProfile expected, DockProfile actual, string context, Action<bool, string> Check)
    {
        Check(SameOrder(expected.Widgets, actual.Widgets), $"{context} must preserve Widgets.");
        Check(SameOrder(expected.InstalledWidgets, actual.InstalledWidgets), $"{context} must preserve InstalledWidgets.");
        Check(SameOrder(expected.WidgetOrder, actual.WidgetOrder), $"{context} must preserve WidgetOrder.");
        Check(SameOrder(expected.DockItemOrder, actual.DockItemOrder), $"{context} must preserve DockItemOrder.");
        Check(actual.DockWidthMode == expected.DockWidthMode && actual.DockWidthPixels == expected.DockWidthPixels
            && actual.DockWidthPercent == expected.DockWidthPercent, $"{context} must preserve width mode, pixels, and percentage.");
        Check(actual.Theme == expected.Theme && actual.ThemePack == expected.ThemePack
            && actual.ThemeApplicationIcons == expected.ThemeApplicationIcons, $"{context} must preserve theme, theme pack, and icon theming.");
        Check(actual.LauncherHotkey == expected.LauncherHotkey && actual.DockHotkey == expected.DockHotkey,
            $"{context} must preserve both hotkeys.");
        Check(actual.AutoHide == expected.AutoHide, $"{context} must preserve AutoHide.");

        static bool SameOrder(IReadOnlyList<string>? expected, IReadOnlyList<string>? actual) =>
            expected is null ? actual is null : actual is not null && expected.SequenceEqual(actual);
    }

    private static void CheckGlobalSettings(DockSettings expected, DockSettings actual, string context, Action<bool, string> Check)
    {
        Check(actual.Language == expected.Language && actual.Position == expected.Position && actual.Edge == expected.Edge
            && actual.Size == expected.Size && actual.DockScale == expected.DockScale && actual.WeatherCity == expected.WeatherCity
            && actual.CheckUpdates == expected.CheckUpdates, $"{context} must preserve all global settings.");
    }

    private static string SerializeSettings(DockSettings settings) =>
        JsonSerializer.Serialize(settings, DockSettingsStore.SerializerOptions);

    private static void CheckSettingsOpenClosePerformance(Application app, Action<bool, string> Check)
    {
        var tempPath = Path.Combine(Path.GetTempPath(), $"ot-settings-perf-{Guid.NewGuid():N}.json");
        try
        {
            var store = new DockSettingsStore(tempPath);
            var initial = new DockSettings(
                Theme: ThemeManager.Current,
                Language: AppLanguage.English,
                AnimationsEnabled: false,
                DockBackground: SurfaceBackground.Glass,
                DockOpacity: 0.85,
                Widgets: ["Clock"],
                InstalledWidgets: ["Clock"]);
            Check(store.Save(initial) is null, "Initial settings for open/close performance check must save cleanly.");
            var preferences = new ApplicationPreferences(store);
            ThemeManager.Apply(preferences.Current.Theme, preferences.Current);
            Check(ThemeManager.IsApplied(preferences.Current), "ThemeManager.IsApplied must return true when theme and surfaces match.");

            var changeCount = 0;
            preferences.Changed += (_, _) => changeCount++;

            // 1. Opening SettingsWindow on the default Dock tab must NOT attach live widget previews.
            var window1 = new SettingsWindow(preferences)
            {
                WindowStartupLocation = WindowStartupLocation.Manual,
                Left = -20000,
                Top = -20000,
                ShowInTaskbar = false,
                ShowActivated = false
            };
            window1.Show();
            PumpDispatcher(app.Dispatcher, 60);

            var options1 = ((System.Windows.Data.ListCollectionView)window1.WidgetOptionsList.ItemsSource)
                .SourceCollection.Cast<DockWidgetOption>().ToArray();
            Check(window1.LivePreviewWidgetCount == 0 && options1.All(o => o.LivePreview is null),
                "Opening SettingsWindow on the default Dock tab must not eagerly instantiate live widget previews.");

            var themeChangedOnCleanClose = 0;
            EventHandler onThemeChanged = (_, _) => themeChangedOnCleanClose++;
            ThemeManager.ThemeChanged += onThemeChanged;
            try
            {
                window1.Close();
                PumpDispatcher(app.Dispatcher, 60);
            }
            finally
            {
                ThemeManager.ThemeChanged -= onThemeChanged;
            }
            Check(themeChangedOnCleanClose == 0,
                "Closing SettingsWindow without theme or surface preview changes must skip redundant ThemeManager.Apply.");
            Check(changeCount == 0, "Opening and closing SettingsWindow without edits must not mutate stored preferences.");

            // 2. Immediate reopen + selecting WidgetsTab attaches live previews, and closing disposes them cleanly.
            var window2 = new SettingsWindow(preferences)
            {
                WindowStartupLocation = WindowStartupLocation.Manual,
                Left = -20000,
                Top = -20000,
                ShowInTaskbar = false,
                ShowActivated = false
            };
            window2.Show();
            PumpDispatcher(app.Dispatcher, 60);
            Check(window2.LivePreviewWidgetCount == 0, "Reopened SettingsWindow must still start with zero live widget previews on DockTab.");

            window2.SettingsTabs.SelectedItem = window2.WidgetsTab;
            PumpDispatcher(app.Dispatcher, 60);
            var options2 = ((System.Windows.Data.ListCollectionView)window2.WidgetOptionsList.ItemsSource)
                .SourceCollection.Cast<DockWidgetOption>().ToArray();
            Check(window2.LivePreviewWidgetCount > 0
                && options2.Any(o => o.Id == "Clock" && o.LivePreview is OpenTaskbar.Widgets.DockWidget),
                "Selecting WidgetsTab must lazily attach live widget previews.");

            // Dirty a surface preview directly without saving and confirm close restores the saved surface state.
            ThemeManager.SetSurface(SurfaceKind.Dock, SurfaceBackground.Solid, 0.42);
            Check(!ThemeManager.IsApplied(preferences.Current),
                "ThemeManager.IsApplied must detect an unsaved surface preview change.");

            window2.Close();
            PumpDispatcher(app.Dispatcher, 60);
            Check(window2.LivePreviewWidgetCount == 0 && options2.All(o => o.LivePreview is null),
                "Closing SettingsWindow must dispose and detach all live widget previews.");
            Check(ThemeManager.IsApplied(preferences.Current),
                "Closing SettingsWindow after an unsaved surface preview change must restore the saved theme state.");
            Check(changeCount == 0,
                "Rapid close/reopen and visiting WidgetsTab must not fire spurious preference saves.");
        }
        finally
        {
            if (File.Exists(tempPath)) File.Delete(tempPath);
            if (File.Exists(tempPath + ".tmp")) File.Delete(tempPath + ".tmp");
        }
    }

    [MethodImpl(MethodImplOptions.NoInlining)]
    private static (DockFlyout Flyout, WeakReference ContentRef) ExerciseDockFlyout(
        Application app,
        Action<bool, string> check)
    {
        var flyout = new DockFlyout();
        var anchor = new Button();
        var content = new Border { Width = 40, Height = 40 };
        var hostWin = new Window { Content = anchor, Width = 100, Height = 60, ShowInTaskbar = false };
        hostWin.Show();
        try
        {
            flyout.Show(anchor, content, animate: false);
            check(flyout.IsOpen, "DockFlyout must open when shown.");
            flyout.Close();
            app.Dispatcher.Invoke(() => { }, DispatcherPriority.Background);
            check(!flyout.IsOpen, "DockFlyout must close cleanly.");
            check(flyout.RecentlyClosed(anchor), "DockFlyout must still detect recently closed target via WeakReference.");
        }
        finally
        {
            hostWin.Close();
        }

        return (flyout, new WeakReference(content));
    }

    private static byte[] EncodeSolidPng(int size)
    {
        var pixels = new byte[size * size * 4];
        var bmp = BitmapSource.Create(size, size, 96, 96, PixelFormats.Bgra32, null, pixels, size * 4);
        var encoder = new PngBitmapEncoder();
        encoder.Frames.Add(BitmapFrame.Create(bmp));
        using var ms = new MemoryStream();
        encoder.Save(ms);
        return ms.ToArray();
    }

    private static Button[] AiCards(AiUsageWidget widget) =>
        widget.Content is Button { Visibility: Visibility.Visible } card ? [card] : [];

    [DllImport("user32.dll")]
    private static extern uint GetWindowThreadProcessId(nint windowHandle, out uint processId);
}
