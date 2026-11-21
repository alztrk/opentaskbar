using System.IO;
using System.Windows;
using System.Windows.Controls;
using System.Windows.Media;
using System.Windows.Media.Imaging;
using System.Windows.Threading;
using OpenTaskbar.Models;
using OpenTaskbar.Services;
using OpenTaskbar.Views.Widgets;
using OpenTaskbar.Widgets;

namespace OpenTaskbar.Tests;

internal static class WidgetScreenshotCapture
{
    private static string OutputDir =>
        Environment.GetEnvironmentVariable("OPENTASKBAR_SCREENSHOT_DIR") is { Length: > 0 } custom
            ? custom
            : Path.Combine(Path.GetTempPath(), "OpenTaskbar", "screenshots");

    public static void CaptureAll(Application app)
    {
        Directory.CreateDirectory(OutputDir);

        foreach (var theme in new[] { AppTheme.Dark, AppTheme.Light })
        {
            ThemeManager.Apply(theme);
            var suffix = theme == AppTheme.Dark ? "dark" : "light";

            var settings = new DockSettings(
                Language: AppLanguage.Turkish,
                Theme: theme,
                FocusMinutes: 25,
                FocusBreakMinutes: 5,
                FocusSessions: 4,
                StatsMeters: SystemMeters.Cpu | SystemMeters.Memory);

            // 1. Battery Widget (V19 liquid fill)
            using var batteryWidget = new BatteryWidget();
            batteryWidget.ApplySettings(settings);
            batteryWidget.ApplyStatus(new BatteryStatus(Present: true, Percent: 76, Charging: false, PluggedIn: true));
            batteryWidget.Visibility = Visibility.Visible;
            SavePng(app, batteryWidget, 140, 62, $"battery_{suffix}.png");

            // 2. Storage Widget (V19 horizontal multi-drive)
            using var storageWidget = new StorageWidget();
            storageWidget.ApplySettings(settings);
            storageWidget.ApplyDrives([
                new DriveStorageInfo("C:", 213L * 1024 * 1024 * 1024, 476L * 1024 * 1024 * 1024),
                new DriveStorageInfo("D:", (long)(1.2 * 1024 * 1024 * 1024 * 1024), (long)(1.8 * 1024 * 1024 * 1024 * 1024))
            ]);
            storageWidget.Visibility = Visibility.Visible;
            SavePng(app, storageWidget, 280, 62, $"storage_{suffix}.png");

            // 3. DayProgress Widget (V19 ring + percent + unit)
            using var dayProgressWidget = new DayProgressWidget();
            dayProgressWidget.ApplySettings(settings);
            dayProgressWidget.Render(new DateTime(2026, 11, 18, 16, 20, 0), force: true);
            dayProgressWidget.Visibility = Visibility.Visible;
            SavePng(app, dayProgressWidget, 140, 62, $"dayprogress_{suffix}.png");

            // 4. Stopwatch Widget
            var clock = new ParityClock(new DateTimeOffset(2026, 11, 18, 14, 30, 0, TimeSpan.Zero));
            var sw = new StopwatchDockWidget(clock);
            sw.Refresh();
            var swHost = new ContentControl { Content = sw };
            SavePng(app, swHost, 200, 62, $"stopwatch_{suffix}.png");

            // 5. Countdown Widget
            var cd = new CountdownDockWidget(clock) { MinutesText = "15" };
            cd.Refresh();
            var cdHost = new ContentControl { Content = cd };
            SavePng(app, cdHost, 200, 62, $"countdown_{suffix}.png");

            // 6. Clock Widget
            using var clockWidget = new ClockWidget();
            clockWidget.ApplySettings(settings);
            clockWidget.Activate();
            SavePng(app, clockWidget, 180, 62, $"clock_{suffix}.png");

            // 7. Media Widget
            using var mediaWidget = new MediaWidget();
            mediaWidget.ApplySettings(settings);
            mediaWidget.Visibility = Visibility.Visible;
            SavePng(app, mediaWidget, 340, 62, $"media_{suffix}.png");

            // 8. Audio Widget
            using var audioWidget = new AudioWidget();
            audioWidget.ApplySettings(settings);
            audioWidget.Visibility = Visibility.Visible;
            SavePng(app, audioWidget, 210, 62, $"audio_{suffix}.png");

            // 9. QuickSettings Widget
            using var qsWidget = new QuickSettingsWidget();
            qsWidget.ApplySettings(settings);
            qsWidget.Visibility = Visibility.Visible;
            SavePng(app, qsWidget, 210, 62, $"quicksettings_{suffix}.png");

            // 10. SystemStats Widget (CPU & RAM ring meters)
            using var statsWidget = new SystemStatsWidget();
            statsWidget.ApplySettings(settings);
            statsWidget.Visibility = Visibility.Visible;
            SavePng(app, statsWidget, 130, 62, $"systemstats_{suffix}.png");

            // 11. Focus Widget
            using var focusWidget = new FocusWidget();
            focusWidget.ApplySettings(settings);
            focusWidget.Activate();
            focusWidget.Visibility = Visibility.Visible;
            SavePng(app, focusWidget, 200, 62, $"focus_{suffix}.png");

            // 12. Weather Widget (V19 inline)
            using var weatherWidget = new WeatherWidget();
            weatherWidget.ApplySettings(settings);
            weatherWidget.ApplyReport(new WeatherReport(
                "İstanbul", 18, 2, 17,
                [
                    new WeatherDay(new DateOnly(2026, 11, 18), 2, 14, 21),
                    new WeatherDay(new DateOnly(2026, 11, 19), 2, 13, 19),
                    new WeatherDay(new DateOnly(2026, 11, 20), 61, 12, 17),
                    new WeatherDay(new DateOnly(2026, 11, 21), 0, 11, 16)
                ],
                Humidity: 58,
                WindSpeed: 12));
            weatherWidget.Visibility = Visibility.Visible;
            SavePng(app, weatherWidget, 620, 62, $"weather_{suffix}.png");

            // 12b. Audio Flyout (V19 3-section vertical mixer)
            var audioFlyout = audioWidget.BuildFlyoutContent();
            SavePng(app, (FrameworkElement)audioFlyout, 340, 380, $"audio_flyout_{suffix}.png");

            // 12c. Focus Flyout (V19 presets + progress)
            var focusFlyout = focusWidget.BuildFocusFlyout();
            SavePng(app, (FrameworkElement)focusFlyout, 340, 260, $"focus_flyout_{suffix}.png");

            // 13. Stack Icon
            var stack = new DockStack(Path.Combine(Environment.GetFolderPath(Environment.SpecialFolder.UserProfile), "Downloads"), app.Dispatcher);
            var stackIcon = MainWindow.CreateStackIcon(stack, 128);
            if (stackIcon is not null)
            {
                var img = new Image { Source = stackIcon, Width = 56, Height = 56, Stretch = Stretch.Uniform };
                SavePng(app, img, 80, 80, $"stack_icon_{suffix}.png");
            }
        }

        ThemeManager.Apply(AppTheme.Dark);
    }

    private static void SavePng(Application app, FrameworkElement element, int width, int height, string filename)
    {
        var host = new Border
        {
            Width = width,
            Height = height,
            Background = (Brush)app.FindResource("DockMaterial"),
            Padding = new Thickness(6),
            Child = element
        };
        host.SetResourceReference(TextBlock.ForegroundProperty, "TextPrimary");
        host.SetResourceReference(TextBlock.FontFamilyProperty, "InterfaceFont");
        host.Measure(new Size(width, height));
        host.Arrange(new Rect(0, 0, width, height));
        host.UpdateLayout();
        app.Dispatcher.Invoke(() => { }, DispatcherPriority.Render);

        var rtb = new RenderTargetBitmap(width, height, 96, 96, PixelFormats.Pbgra32);
        rtb.Render(host);

        var encoder = new PngBitmapEncoder();
        encoder.Frames.Add(BitmapFrame.Create(rtb));
        using var stream = File.Create(Path.Combine(OutputDir, filename));
        encoder.Save(stream);
        host.Child = null;
    }

    private sealed class ParityClock(DateTimeOffset now) : TimeProvider
    {
        public override DateTimeOffset GetUtcNow() => now;
    }
}
