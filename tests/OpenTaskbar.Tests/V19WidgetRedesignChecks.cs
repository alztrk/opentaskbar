using System.Globalization;
using System.Windows;
using System.Windows.Controls;
using System.Windows.Media;
using OpenTaskbar.Models;
using OpenTaskbar.Services;
using OpenTaskbar.Views.Shell;
using OpenTaskbar.Widgets;

internal static class V19WidgetRedesignChecks
{
    public static int Run(Application app)
    {
        var checks = 0;
        void Check(bool condition, string message)
        {
            if (!condition) throw new InvalidOperationException(message);
            checks++;
        }

        // 1. BatteryWidget: low/mid/high palettes, no-battery graceful handling, charging state
        var low = BatteryWidget.PaletteForLevel(15);
        var mid = BatteryWidget.PaletteForLevel(45);
        var high = BatteryWidget.PaletteForLevel(80);
        Check(low.Top != mid.Top && mid.Top != high.Top, "Battery palettes must distinguish low, medium, and high levels.");

        using (var battery = new BatteryWidget())
        {
            battery.ApplySettings(new DockSettings(Language: AppLanguage.English));
            battery.ApplyStatus(new BatteryStatus(Present: false, Percent: 0, Charging: false, PluggedIn: false));
            var texts = LogicalDescendants<TextBlock>(battery).Select(t => t.Text).ToList();
            Check(texts.Contains("—"), "Missing battery must render an explicit unavailable placeholder instead of fake data.");

            battery.ApplyStatus(new BatteryStatus(Present: true, Percent: 76, Charging: true, PluggedIn: true));
            texts = LogicalDescendants<TextBlock>(battery).Select(t => t.Text).ToList();
            Check(texts.Contains("76%"), "Battery widget must display the real battery percentage.");
        }

        // 2. StorageWidget: multi-drive GB/TB formatting and compact mode
        Check(StorageWidget.FormatCapacityPair(213L * 1024 * 1024 * 1024, 476L * 1024 * 1024 * 1024, CultureInfo.InvariantCulture) == "213 / 476 GB",
            "StorageWidget must format GB capacity pairs cleanly.");
        Check(StorageWidget.FormatCapacityPair((long)(1.2 * 1024 * 1024 * 1024 * 1024), (long)(1.8 * 1024 * 1024 * 1024 * 1024), CultureInfo.InvariantCulture) == "1.2 / 1.8 TB",
            "StorageWidget must format TB capacity pairs cleanly.");

        using (var storage = new StorageWidget())
        {
            storage.ApplySettings(new DockSettings(Language: AppLanguage.English));
            storage.ApplyDrives([
                new DriveStorageInfo("C:", 213L * 1024 * 1024 * 1024, 476L * 1024 * 1024 * 1024),
                new DriveStorageInfo("D:", (long)(1.2 * 1024 * 1024 * 1024 * 1024), (long)(1.8 * 1024 * 1024 * 1024 * 1024))
            ]);
            var fullTexts = LogicalDescendants<TextBlock>(storage).Select(t => t.Text).ToList();
            Check(fullTexts.Contains("C:") && fullTexts.Contains("D:"), "StorageWidget must show multiple drives horizontally.");

            storage.SetCompactState(true);
            var compactTexts = LogicalDescendants<TextBlock>(storage).Select(t => t.Text).ToList();
            Check(compactTexts.Contains("C:") && !compactTexts.Contains("D:"), "Compact StorageWidget must hide secondary drives.");
        }

        // 3. DayProgressWidget: local time percentage and compact unit toggle
        using (var dayProgress = new DayProgressWidget())
        {
            dayProgress.ApplySettings(new DockSettings(Language: AppLanguage.English));
            dayProgress.Render(new DateTime(2026, 11, 18, 12, 0, 0), force: true);
            var texts = LogicalDescendants<TextBlock>(dayProgress).Select(t => t.Text).ToList();
            Check(texts.Contains("50%"), "DayProgressWidget must compute exact local day elapsed percentage.");
            Check(dayProgress.BuildHoverFlyoutContent() is null, "DayProgressWidget must not require a flyout.");
        }

        // 4. WeatherWidget: inline weather + 3-day forecast, no flyout
        using (var weather = new WeatherWidget())
        {
            weather.ApplySettings(new DockSettings(Language: AppLanguage.English));
            weather.ApplyReport(new WeatherReport(
                "Ankara", 18, 2, 17,
                [
                    new WeatherDay(new DateOnly(2026, 11, 18), 2, 14, 21),
                    new WeatherDay(new DateOnly(2026, 11, 19), 2, 13, 19),
                    new WeatherDay(new DateOnly(2026, 11, 20), 61, 12, 17),
                    new WeatherDay(new DateOnly(2026, 11, 21), 0, 11, 16)
                ],
                Humidity: 58,
                WindSpeed: 12));
            var texts = LogicalDescendants<TextBlock>(weather).Select(t => t.Text).ToList();
            Check(texts.Contains("18°") && texts.Contains("Ankara") && texts.Contains("21° / 14°") && texts.Contains("19° / 13°"),
                "WeatherWidget must display current weather, today range, and forecast directly inline.");
            Check(weather.BuildHoverFlyoutContent() is null, "WeatherWidget must not open a flyout.");
        }

        // 5. ClockWidget: system timezone & calendar flyout with year/month nav and Today jump
        using (var clockWidget = new ClockWidget())
        {
            clockWidget.ApplySettings(new DockSettings(Language: AppLanguage.English));
            var tz = ClockWidget.FormatLocalTimeZone();
            Check(tz.StartsWith("UTC", StringComparison.Ordinal), "ClockWidget must format the real system timezone.");
            var host = new ContentControl();
            var cal = clockWidget.BuildCalendar(host);
            var buttons = LogicalDescendants<Button>(cal).ToList();
            Check(buttons.Count >= 6, "Calendar flyout must provide year/month navigation and Today controls.");
        }

        // 6. FocusWidget: distinct Focus flyout with 15/25/50 presets
        using (var focus = new FocusWidget())
        {
            focus.ApplySettings(new DockSettings(Language: AppLanguage.English, FocusMinutes: 25));
            focus.Activate();
            var flyout = focus.BuildFocusFlyout();
            var buttonLabels = LogicalDescendants<Button>(flyout).Select(b => b.Content?.ToString()).ToList();
            Check(buttonLabels.Contains("15 min") && buttonLabels.Contains("25 min") && buttonLabels.Contains("50 min"),
                "Focus flyout must provide 15/25/50 minute presets.");
        }

        // 7. Stopwatch laps & Countdown presets
        var testClock = new FixedClock(new DateTimeOffset(2026, 11, 21, 12, 0, 0, TimeSpan.Zero));
        var sw = new StopwatchDockWidget(testClock);
        sw.Toggle();
        testClock.Advance(TimeSpan.FromSeconds(12.5));
        sw.RecordLap();
        Check(sw.Laps.Count == 1 && sw.HasLaps, "Stopwatch must record lap entries.");
        sw.Reset();
        Check(sw.Laps.Count == 0 && !sw.HasLaps, "Stopwatch reset must clear recorded laps.");

        var cd = new CountdownDockWidget(testClock);
        cd.SetPresetMinutes(15);
        Check(cd.State == CountdownState.Ready && cd.MinutesText == "15" && Math.Abs(cd.ProgressPercent - 100.0) < 0.01,
            "Countdown preset must configure duration and full progress.");

        // 8. AudioWidget: combined trigger + 3-section vertical mixer with single-chevron ComboBoxes
        using (var audio = new AudioWidget())
        {
            var flyout = audio.BuildFlyoutContent();
            var combos = LogicalDescendants<ComboBox>(flyout).ToList();
            Check(combos.Count == 2, "Audio mixer flyout must contain Output and Input ComboBox device selectors.");
        }

        // 9. Background Apps (TrayShellButton): clean flyout without explanatory paragraph
        var tray = new TrayShellButton();
        var trayContent = tray.BuildContent([new TrayApp("Steam", @"C:\Steam\steam.exe", null)]);
        var trayTexts = LogicalDescendants<TextBlock>(trayContent).Select(t => t.Text).ToList();
        Check(trayTexts.Contains("Steam") && !trayTexts.Contains(UiText.Get("TrayNote")),
            "Background Apps flyout must list apps cleanly without explanatory paragraphs.");

        return checks;
    }

    private static IEnumerable<T> LogicalDescendants<T>(DependencyObject root) where T : DependencyObject
    {
        foreach (var child in LogicalTreeHelper.GetChildren(root).OfType<DependencyObject>())
        {
            if (child is T match) yield return match;
            foreach (var nested in LogicalDescendants<T>(child)) yield return nested;
        }
    }

    private sealed class FixedClock(DateTimeOffset start) : TimeProvider
    {
        private DateTimeOffset _now = start;
        public void Advance(TimeSpan delta) => _now += delta;
        public override DateTimeOffset GetUtcNow() => _now;
        public override long GetTimestamp() => _now.Ticks;
        public override long TimestampFrequency => TimeSpan.TicksPerSecond;
    }
}
