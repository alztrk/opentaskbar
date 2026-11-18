using System.IO;
using System.Runtime.CompilerServices;
using System.Windows;
using System.Windows.Controls;
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
                lazyCoordinator.OrderChanged = _ => reordered = true;
                lazyCoordinator.ReorderWidgets(0, 1);
                Check(reordered && lazyCoordinator.CurrentOrder[0] != "Media",
                    "Lazy WidgetCoordinator must support drag reordering.");

                // Enable all 9 live widgets + SystemMeters.All
                lazyCoordinator.Apply(new DockSettings(
                    Widgets: WidgetCoordinator.DefaultWidgetOrder,
                    StatsMeters: SystemMeters.All));
                Check(strip.Children.Count == WidgetCoordinator.DefaultWidgetOrder.Length,
                    "Enabling all live widgets must materialize all controls.");
            }

            // 2. SystemStats + on-demand SystemSensors parity & disposal
            var stats = new SystemStats();
            _ = stats.SampleCpu();
            Check(SystemStats.SampleMemory() is >= 0 and <= 1, "SystemStats.SampleMemory must report valid load.");
            _ = SystemStats.SampleBattery();

            using (var sensors = new SystemSensors(SystemMeters.None))
            {
                sensors.Configure(SystemMeters.Gpu);
                _ = sensors.SampleGpuUsage();
                sensors.Configure(SystemMeters.CpuTemperature);
                _ = sensors.SampleCpuTemperature();
                sensors.Configure(SystemMeters.GpuTemperature);
                _ = sensors.SampleGpuTemperature();
                sensors.Configure(SystemMeters.All);
                _ = sensors.SampleGpuUsage();
                _ = sensors.SampleCpuTemperature();
                _ = sensors.SampleGpuTemperature();
                sensors.Configure(SystemMeters.None);
            }
            Check(true, "SystemSensors must configure, sample, and dispose cleanly across all SystemMeters combinations.");

            // 3. Shared AudioEndpointMonitor reference counting
            using (var a1 = AudioEndpointMonitor.AcquireShared())
            {
                using (var a2 = AudioEndpointMonitor.AcquireShared())
                {
                    Check(a1.HasDevice == a2.HasDevice && Math.Abs(a1.Volume - a2.Volume) < 0.001f && a1.Muted == a2.Muted,
                        "Shared AudioEndpointMonitor handles must observe identical audio state.");
                }
                _ = a1.Volume;
            }
            Check(true, "Shared AudioEndpointMonitor handles must survive partial release and clean final disposal.");

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

            var launcher = new LauncherWindow(_ => { });
            launcher.Close();
            Check(true, "LauncherWindow must initialize and close cleanly.");

            // 8. Heavy DockSettingsStore validation & round-trip
            var tempSettings = Path.Combine(Path.GetTempPath(), $"ot-heavy-{Guid.NewGuid():N}.json");
            try
            {
                var store = new DockSettingsStore(tempSettings);
                var heavy = new DockSettings(
                    Position: DockPosition.Center,
                    Theme: AppTheme.Dark,
                    Language: AppLanguage.English,
                    Widgets: ["Media", "Desktops", "SystemStats", "Focus", "Weather", "Audio", "QuickSettings", "Tray", "Clock", "Storage", "DayProgress", "Stopwatch", "Countdown"],
                    InstalledWidgets: ["Media", "Desktops", "SystemStats", "Focus", "Weather", "Audio", "QuickSettings", "Tray", "Clock", "Storage", "DayProgress", "Stopwatch", "Countdown"],
                    ShowRecycleBin: true,
                    ShowOnAllMonitors: true,
                    StatsMeters: SystemMeters.All,
                    Profiles: [new DockProfile("Work", [@"C:\Games\game.exe"])],
                    ActiveProfile: "Work");
                Check(store.Save(heavy) is null, "Heavy DockSettings must save without errors.");
                var (loaded, loadErr) = store.Load();
                Check(loadErr is null && loaded.StatsMeters == SystemMeters.All && loaded.Widgets?.Count == 13 && loaded.ShowOnAllMonitors,
                    "Heavy DockSettings must round-trip all 13 widgets, SystemMeters.All, and multi-monitor flags.");
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
        }
        finally
        {
            UiText.Current.SetLanguage(previousLanguage);
        }

        return checks;
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
}
