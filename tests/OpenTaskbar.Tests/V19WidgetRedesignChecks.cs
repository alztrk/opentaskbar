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

        // 4. WeatherWidget (#84): compact inline card matching Settings preview + full details in click flyout
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
            Check(weather.FullWidthEstimate <= 260,
                $"WeatherWidget FullWidthEstimate ({weather.FullWidthEstimate}) must stay compact and match the Settings preview.");
            var inlineTexts = LogicalDescendants<TextBlock>(weather).Select(t => t.Text).ToList();
            Check(inlineTexts.Contains("18°") && inlineTexts.Contains("Ankara") && inlineTexts.Contains("21° / 14°"),
                "WeatherWidget inline card must display current weather, place, and today range.");
            Check(!inlineTexts.Contains("19° / 13°") && !inlineTexts.Contains("58%"),
                "WeatherWidget inline card must keep 3-day forecast and secondary metrics out of the dock strip.");
            Check(weather.BuildHoverFlyoutContent() is null, "WeatherWidget must not open a hover flyout.");
            var flyout = weather.BuildFlyoutContent();
            var flyoutTexts = LogicalDescendants<TextBlock>(flyout).Select(t => t.Text).ToList();
            Check(flyoutTexts.Contains("17°") && flyoutTexts.Contains("58%")
                  && flyoutTexts.Contains("19° / 13°") && flyoutTexts.Contains("17° / 12°") && flyoutTexts.Contains("16° / 11°"),
                "WeatherWidget click flyout must display feels-like, humidity, and 3-day forecast.");

            // Flat 2x2 metric grid must render 4 detail metrics bound to theme text tokens in both Dark and Light themes.
            var metricsGrid = LogicalDescendants<System.Windows.Controls.Primitives.UniformGrid>(flyout).Single();
            var detailCells = metricsGrid.Children.OfType<DockPanel>().ToList();
            Check(detailCells.Count == 4, "WeatherWidget click flyout must render 4 detail metric items.");
            try
            {
                foreach (var theme in new[] { AppTheme.Dark, AppTheme.Light })
                {
                    ThemeManager.Apply(theme);
                    var expectedPrimary = app.TryFindResource("TextPrimary") as Brush;
                    var expectedSecondary = app.TryFindResource("TextSecondary") as Brush;
                    Check(expectedPrimary is not null && expectedSecondary is not null,
                        $"TextPrimary and TextSecondary resources must exist in {theme} theme.");
                    var themedFlyout = weather.BuildFlyoutContent();
                    var themedGrid = LogicalDescendants<System.Windows.Controls.Primitives.UniformGrid>(themedFlyout).Single();
                    var themedCells = themedGrid.Children.OfType<DockPanel>().ToList();
                    Check(themedCells.Count == 4
                          && !themedGrid.Children.OfType<Border>().Any()
                          && themedCells.All(cell => cell.Children.OfType<TextBlock>().ToList() is [var val, var lbl]
                              && ReferenceEquals(val.Foreground, expectedPrimary)
                              && ReferenceEquals(lbl.Foreground, expectedSecondary)),
                        $"WeatherWidget detail metrics must use flat themed text tokens in {theme} theme.");
                }
            }
            finally
            {
                ThemeManager.Apply(AppTheme.Dark);
            }
        }

        // 4b. Temperature unit: Celsius stays bare, Fahrenheit converts instantly and uses mph for wind.
        using (var fahrenheit = new WeatherWidget())
        {
            fahrenheit.ApplySettings(new DockSettings(Language: AppLanguage.English, WeatherUnit: TemperatureUnit.Fahrenheit));
            fahrenheit.ApplyReport(new WeatherReport(
                "Ankara", 10, 2, 20,
                [new WeatherDay(new DateOnly(2026, 11, 18), 2, 10, 20)],
                Humidity: 50,
                WindSpeed: 10));
            var inline = LogicalDescendants<TextBlock>(fahrenheit).Select(t => t.Text).ToList();
            Check(inline.Contains("50°F") && inline.Contains("68°F / 50°F"),
                "WeatherWidget must convert inline temperatures to Fahrenheit when the unit setting changes.");
            var flyout = LogicalDescendants<TextBlock>(fahrenheit.BuildFlyoutContent()).Select(t => t.Text).ToList();
            Check(flyout.Contains("68°F") && flyout.Contains("6 mph"),
                "Weather flyout must convert feels-like and wind speed for Fahrenheit users.");

            fahrenheit.ApplySettings(new DockSettings(Language: AppLanguage.English, WeatherUnit: TemperatureUnit.Celsius));
            inline = LogicalDescendants<TextBlock>(fahrenheit).Select(t => t.Text).ToList();
            Check(inline.Contains("10°") && inline.Contains("20° / 10°"),
                "Switching back to Celsius must repaint the cached report without refetching.");
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

            // Explicit date formats override the culture default while DateStyle controls visibility.
            clockWidget.ApplySettings(new DockSettings(
                Language: AppLanguage.English,
                DateStyle: ClockDateStyle.ShortDateOnly,
                DateFormat: ClockDateFormat.DdMmYy));
            clockWidget.RenderPreview();
            var dateText = (TextBlock)clockWidget.FindName("DateText")!;
            Check(dateText.Text.Length == 8 && dateText.Text[2] == '/' && dateText.Text[5] == '/'
                  && dateText.Text.Replace("/", "").All(char.IsDigit),
                "DD/MM/YY date format must render as a two-digit numeric date.");

            clockWidget.ApplySettings(new DockSettings(
                Language: AppLanguage.English,
                DateStyle: ClockDateStyle.ShortDateOnly,
                DateFormat: ClockDateFormat.YyyyMmDd));
            clockWidget.RenderPreview();
            Check(dateText.Text.Length == 10 && dateText.Text[4] == '-' && dateText.Text[7] == '-',
                "YYYY-MM-DD date format must render as an ISO-style numeric date.");
        }

        // 6. FocusWidget: distinct Focus flyout with 15/25/50 presets
        using (var focus = new FocusWidget())
        {
            focus.ApplySettings(new DockSettings(Language: AppLanguage.English, FocusMinutes: 25));
            focus.Activate();
            var flyout = focus.BuildFocusFlyout();
            var buttonLabels = LogicalDescendants<Button>(flyout).Select(b => b.Content?.ToString()).ToList();
            Check(buttonLabels.Contains(UiText.Format("MinutesShort", 15))
                  && buttonLabels.Contains(UiText.Format("MinutesShort", 25))
                  && buttonLabels.Contains(UiText.Format("MinutesShort", 50)),
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
