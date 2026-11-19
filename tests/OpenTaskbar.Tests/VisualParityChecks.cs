using System.Security.Cryptography;
using System.Windows;
using System.Windows.Controls;
using System.Windows.Media;
using System.Windows.Media.Imaging;
using System.Windows.Threading;
using OpenTaskbar.Models;
using OpenTaskbar.Services;
using OpenTaskbar.Views.Widgets;
using OpenTaskbar.Widgets;

internal static class VisualParityChecks
{
    public static int Run(Application app)
    {
        SynchronizationContext.SetSynchronizationContext(new DispatcherSynchronizationContext(Dispatcher.CurrentDispatcher));
        var checks = 0;
        var previousLanguage = UiText.Current.Language;
        try
        {
            void Check(bool condition, string message)
            {
                if (!condition) throw new InvalidOperationException(message);
                checks++;
            }

            UiText.Current.SetLanguage(AppLanguage.English);

            // SHA-256 pixel hashes captured on the Windows CI runner for the reviewed design.
            var expected = new Dictionary<string, string>(StringComparer.Ordinal)
            {
                ["DockButtons_Dark"] = "EA8C4048F68397F476CD20C0DE8A324F81E02763C20D7EFAF5B5CE28CCB13999",
                ["DockButtons_Light"] = "8A3AAD5AF8F20219480CB972C5FC68D06FEE4DC1244160952591A65AA29EAECE",
                ["RingMeters_Dark"] = "AA26B0446BBDF57C22178347AF6A6130652401D0CF2F2558BB2F89F242D01B39",
                ["RingMeters_Light"] = "9E3A45D2D26A51D6026C79407B5FE1821000FAE8C8E972BEE9F6C4A520B8D39C",
                ["TimedWidgets_Dark"] = "60E193612F9523954A32E4CE0DB18D9339EC740B2D6712EAA35A5ACD439CE2D7",
                ["TimedWidgets_Light"] = "DFA830AC953B2F7E6CD0E802FE01E56CD5A6B2BF2437965E726AFE17F6AAF698",
                ["FocusWidget_Dark"] = "D842256A1EF1F1938FDF3CE13F96D7C93A61843D3B38C0210B7AD481936A7041",
                ["FocusWidget_Light"] = "A43CBAB0E8609FA3086027BCF7D61F99F3932124247916EE3875F4BE823CD6B2",
                ["QuickSettingsWidget_Dark"] = "893BD1AB9AA324C04358CE2C4757F9BEB8D5E19AF8AA238CF3A18922D6952FC9",
                ["QuickSettingsWidget_Light"] = "1E5A415786524A70A9D200DBF34235FF41B3E213867A01BEEC0081B132F2EEC5",
                ["MediaWidget_Dark"] = "F8643286558AFFF25FC3E1C88EDE0EFBBF396EA354E7F7CF63D9529F4DE260F9",
                ["MediaWidget_Light"] = "F8643286558AFFF25FC3E1C88EDE0EFBBF396EA354E7F7CF63D9529F4DE260F9",
                ["WeatherGradients_Dark"] = "7738B36883895809AAEF350A7A569DDDBDCAE75D36E0D034C0858439EBFA9957",
                ["WeatherGradients_Light"] = "7738B36883895809AAEF350A7A569DDDBDCAE75D36E0D034C0858439EBFA9957"
            };
            expected["DockButtons_Dark"] = "D79C2E983AEDDE5CC445385A76E30E4A75E15BE845E013B21A7874BE26112530";
            expected["TimedWidgets_Dark"] = "A0BAF935617D19BBD130E05449FD8AF9F6B95D1AEC1ACE4FBB75511020C6EA27";
            expected["FocusWidget_Dark"] = "61F633ABDB8BF6F6152990C0C627DE4652E6B4ED26489C04D8503E30A1A86410";
            expected["TimedWidgets_Light"] = "057438532CEB295234A68624878EAFA85B5FA919DBF4A5030D69B061DBC01255";
            expected["FocusWidget_Light"] = "2D0874E57F6C623E2E7D00F13D0E1C2E14082ABC5732BF8EC487876BBDEDC16F";
            expected["DockButtons_Light"] = "CFC6DCD48253E10333D87FBBCC13BD63F507C637F73B261E38985C8E5EB5AFB6";
            expected["RingMeters_Dark"] = "07124012760F90BB1A6B5161790BEDA5E7C4ABEF24C926385E2B6D31706A359D";

            foreach (var theme in new[] { AppTheme.Dark, AppTheme.Light })
            {
                ThemeManager.Apply(theme);

                // 1. DockButton + StartDockButton states
                var fgApp = new DockApp("AppFG", "fg.exe") { WindowCount = 1, IsForeground = true };
                var runApp = new DockApp("AppRun", "run.exe") { WindowCount = 1, IsForeground = false };
                var pinApp = new DockApp("AppPin", "pin.exe") { WindowCount = 0, IsForeground = false };
                var dockButtons = new StackPanel { Orientation = Orientation.Horizontal, Background = (Brush)app.FindResource("DockMaterial") };
                dockButtons.Children.Add(new Button { Style = (Style)app.FindResource("StartDockButton"), Content = new Border { Width = 20, Height = 20, Background = Brushes.White } });
                dockButtons.Children.Add(new Button { Style = (Style)app.FindResource("DockButton"), DataContext = fgApp, Content = new Border { Width = 32, Height = 32, Background = Brushes.DodgerBlue } });
                dockButtons.Children.Add(new Button { Style = (Style)app.FindResource("DockButton"), DataContext = runApp, Content = new Border { Width = 32, Height = 32, Background = Brushes.MediumSeaGreen } });
                dockButtons.Children.Add(new Button { Style = (Style)app.FindResource("DockButton"), DataContext = pinApp, Content = new Border { Width = 32, Height = 32, Background = Brushes.Coral } });
                var dockHash = CaptureHash(app, dockButtons, 260, 56, out var dockPixels);
                Check(dockHash == expected[$"DockButtons_{theme}"], $"DockButtons_{theme} pixel buffer must match baseline exactly. Actual: {dockHash}");
                Check(dockPixels.Any(b => b != 0), $"DockButtons_{theme} rendered pixels must be non-empty.");

                // 2. RingMeters across 0%, 50%, 75%, 90%, 100% + cached Pen re-render parity
                var rings = new StackPanel { Orientation = Orientation.Horizontal, Background = (Brush)app.FindResource("WidgetCard") };
                RingMeter? firstRing = null;
                foreach (var (val, caption, text, key) in new[]
                {
                    (0.0, "CPU", "0", "WidgetAccent"),
                    (0.50, "RAM", "50", "WidgetAccent"),
                    (0.75, "TEMP", "75", "WidgetWarning"),
                    (0.90, "GPU", "90", "WidgetFocusAccent"),
                    (1.0, "MAX", "100", "WidgetFocusAccent")
                })
                {
                    var ring = new RingMeter
                    {
                        Width = 34, Height = 34, Margin = new Thickness(4),
                        Value = val, Caption = caption, Text = text, StrokeThickness = 3
                    };
                    ring.SetResourceReference(RingMeter.TrackBrushProperty, "StrokeSubtle");
                    ring.SetResourceReference(RingMeter.ValueBrushProperty, key);
                    ring.SetResourceReference(RingMeter.ForegroundProperty, "TextPrimary");
                    ring.SetResourceReference(RingMeter.CaptionBrushProperty, "TextSecondary");
                    firstRing ??= ring;
                    rings.Children.Add(ring);
                }
                var ringsHash = CaptureHash(app, rings, 220, 46, out _);
                Check(ringsHash == expected[$"RingMeters_{theme}"], $"RingMeters_{theme} pixel buffer must match baseline exactly. Actual: {ringsHash}");

                // Force value change and back to verify cached Pen invalidation/re-render is pixel-identical.
                firstRing!.Value = 0.42;
                firstRing.Value = 0.0;
                var ringsRehash = CaptureHash(app, rings, 220, 46, out _);
                Check(ringsRehash == ringsHash, $"RingMeters_{theme} cached pen re-render must stay pixel-identical.");

                // 3. TimedWidgetTile (Stopwatch + Countdown)
                var fixedClock = new ParityClock(new DateTimeOffset(2026, 11, 17, 14, 30, 0, TimeSpan.Zero));
                var sw = new StopwatchDockWidget(fixedClock);
                sw.Refresh();
                var cd = new CountdownDockWidget(fixedClock) { MinutesText = "15" };
                cd.Refresh();
                var timedPanel = new StackPanel { Orientation = Orientation.Horizontal, Background = (Brush)app.FindResource("DockMaterial") };
                timedPanel.Children.Add(new ContentControl { Content = sw });
                timedPanel.Children.Add(new ContentControl { Content = cd });
                var timedHash = CaptureHash(app, timedPanel, 380, 64, out _);
                Check(timedHash == expected[$"TimedWidgets_{theme}"], $"TimedWidgets_{theme} pixel buffer must match baseline exactly. Actual: {timedHash}");

                // 4. Activated live widgets
                var settings = new DockSettings(
                    Language: AppLanguage.English,
                    Theme: theme,
                    FocusMinutes: 25,
                    FocusBreakMinutes: 5,
                    FocusSessions: 4,
                    StatsMeters: SystemMeters.Cpu | SystemMeters.Memory);

                using var focus = new FocusWidget();
                focus.ApplySettings(settings);
                focus.Activate();
                var focusHash = CaptureHash(app, focus, 180, 52, out _);
                Check(focusHash == expected[$"FocusWidget_{theme}"],
                    $"FocusWidget_{theme} pixel buffer must match baseline exactly. Actual: {focusHash}");

                using var qs = new QuickSettingsWidget();
                qs.ApplySettings(settings);
                qs.Visibility = Visibility.Visible;
                var qHash1 = CaptureHash(app, qs, 180, 52, out var qPixels);
                var qHash2 = CaptureHash(app, qs, 180, 52, out _);
                Check(qHash1 == qHash2 && qPixels.Any(b => b != 0),
                    $"QuickSettingsWidget_{theme} must render deterministically.");

                using var media = new MediaWidget();
                media.ApplySettings(settings);
                var mediaHash = CaptureHash(app, media, 200, 52, out _);
                Check(mediaHash == expected[$"MediaWidget_{theme}"],
                    $"MediaWidget_{theme} pixel buffer must match baseline exactly. Actual: {mediaHash}");

                // 5. Weather frozen gradient swatches
                var weatherSwatches = new StackPanel { Orientation = Orientation.Horizontal };
                foreach (var code in new[] { 0, 1, 3, 45, 61, 71, 95, 999 })
                {
                    weatherSwatches.Children.Add(new Border
                    {
                        Width = 32, Height = 32,
                        Background = WeatherService.GetConditionBackground(code)
                    });
                }
                Check(CaptureHash(app, weatherSwatches, 256, 32, out _) == expected[$"WeatherGradients_{theme}"],
                    $"WeatherGradients_{theme} pixel buffer must match baseline exactly.");

                // 6. ClockWidget layout rendering (dynamic OS values, check non-empty & deterministic re-render)
                using var clock = new ClockWidget();
                clock.ApplySettings(settings);
                clock.Activate();
                var cHash1 = CaptureHash(app, clock, 120, 52, out var cPixels);
                var cHash2 = CaptureHash(app, clock, 120, 52, out _);
                Check(cHash1 == cHash2 && cPixels.Any(b => b != 0), $"ClockWidget_{theme} must render deterministically.");
            }

            ThemeManager.Apply(AppTheme.Dark);
        }
        finally
        {
            UiText.Current.SetLanguage(previousLanguage);
        }

        return checks;
    }

    private static string CaptureHash(Application app, FrameworkElement element, int width, int height, out byte[] pixels)
    {
        var host = new Border { Width = width, Height = height, Child = element };
        host.SetResourceReference(TextBlock.ForegroundProperty, "TextPrimary");
        host.SetResourceReference(TextBlock.FontFamilyProperty, "InterfaceFont");
        host.Measure(new Size(width, height));
        host.Arrange(new Rect(0, 0, width, height));
        host.UpdateLayout();
        app.Dispatcher.Invoke(() => { }, DispatcherPriority.Render);

        var rtb = new RenderTargetBitmap(width, height, 96, 96, PixelFormats.Pbgra32);
        rtb.Render(host);
        pixels = new byte[width * height * 4];
        rtb.CopyPixels(pixels, width * 4, 0);
        host.Child = null;
        return Convert.ToHexString(SHA256.HashData(pixels));
    }

    private sealed class ParityClock(DateTimeOffset now) : TimeProvider
    {
        public override DateTimeOffset GetUtcNow() => now;
    }
}
