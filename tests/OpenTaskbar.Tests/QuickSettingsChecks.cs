using System.Windows;
using System.Windows.Automation;
using System.Windows.Controls;
using System.Windows.Controls.Primitives;
using System.Windows.Media;
using System.Windows.Threading;
using OpenTaskbar.Models;
using OpenTaskbar.Services;
using OpenTaskbar.Views;
using OpenTaskbar.Widgets;

internal static class QuickSettingsChecks
{
    internal static int Run()
    {
        var now = new DateTimeOffset(2026, 10, 20, 7, 0, 0, TimeSpan.Zero);
        var emptyState = Convert.FromHexString("434201000A0201002A2A000000");
        Check(NightLightControl.DecodeState(emptyState) == false,
            "Windows' empty Night Light envelope must be treated as off.");

        var on = NightLightControl.EncodeChange(emptyState, true, now);
        Check(on is not null && NightLightControl.DecodeState(on) == true,
            "Enabling Night Light must write a parseable running state without an OS settings edit.");
        if (on is null) throw new InvalidOperationException("Night Light change was not encoded.");
        var off = NightLightControl.EncodeChange(on, false, now.AddSeconds(1));
        Check(off is not null && NightLightControl.DecodeState(off) == false,
            "Disabling Night Light must clear the running state.");
        Check(ReferenceEquals(on, NightLightControl.EncodeChange(on, true, now)),
            "An identical Night Light request must not rewrite the registry record.");
        Check(NightLightControl.EncodeChange(on[..^1], false, now) is null,
            "A malformed record must not be overwritten.");

        return 5;
    }

    internal static int RunUi()
    {
        using var releaseWrite = new ManualResetEventSlim(false);
        var on = 0;
        var writes = 0;
        using var widget = new QuickSettingsWidget
        {
            ReadNightLight = () => Volatile.Read(ref on) == 1,
            WriteNightLight = target =>
            {
                if (Interlocked.Increment(ref writes) == 1 && releaseWrite.Wait(TimeSpan.FromSeconds(5)))
                    Volatile.Write(ref on, target ? 1 : 0);
                return true;
            }
        };

        var scroll = new DockScrollViewer
        {
            Content = widget,
            MaxWidth = 320,
            HorizontalScrollBarVisibility = ScrollBarVisibility.Hidden,
            VerticalScrollBarVisibility = ScrollBarVisibility.Disabled
        };
        scroll.SetResourceReference(FrameworkElement.StyleProperty, "DockScrollViewerStyle");
        var host = new Window { Content = scroll, SizeToContent = SizeToContent.WidthAndHeight, ShowInTaskbar = false };
        host.Show();
        try
        {
            widget.Activate();
            var toggle = widget.FindName("NightLightToggle") as ToggleButton
                ?? throw new InvalidOperationException("Night Light must expose its dock button.");
            Click(toggle);
            Check(!toggle.IsEnabled && toggle.IsChecked == false
                && AutomationProperties.GetItemStatus(toggle) == UiText.Get("QuickTurningOn"),
                "The button must stay on the last Windows state and reject input while turning on.");

            Click(toggle);
            releaseWrite.Set();
            PumpUntil(() => toggle.IsEnabled);
            Check(writes == 1 && toggle.IsChecked == true,
                "A second click during the transition must not write, and confirmation must enable the button.");

            Click(toggle);
            Check(!toggle.IsEnabled && toggle.IsChecked == true
                && AutomationProperties.GetItemStatus(toggle) == UiText.Get("QuickTurningOff"),
                "Turning off must remain checked until Windows confirms the new state.");
            PumpUntil(() => toggle.IsEnabled);
            Check(writes == 2 && toggle.IsChecked == true
                && AutomationProperties.GetItemStatus(toggle).Contains(UiText.Get("QuickChangeFailed"), StringComparison.Ordinal),
                "An unconfirmed write must restore the actual state and advertise the failure.");

            var strip = widget.FindName("ControlsStrip") as StackPanel
                ?? throw new InvalidOperationException("Quick Settings must expose its inline controls.");
            var shortcuts = strip.Children.OfType<Button>().ToArray();
            string[] inlineKeys =
            [
                "QuickAvailableNetworks", "QuickCast", "QuickOpenWindowsPanel", "WidgetQuickSettings"
            ];
            var expectedInlineNames = inlineKeys.Select(UiText.Get).ToHashSet(StringComparer.CurrentCulture);
            var actualInlineNames = shortcuts.Select(AutomationProperties.GetName).ToHashSet(StringComparer.CurrentCulture);
            Check(shortcuts.Length == inlineKeys.Length && actualInlineNames.SetEquals(expectedInlineNames) && shortcuts.All(button => button.IsEnabled
                && button.Content is ContentControl { Content: Geometry }
                && !string.IsNullOrWhiteSpace(AutomationProperties.GetName(button))),
                "The 3 core inline shortcuts plus the More flyout button must have visible icons and accessible names.");

            var flyoutPanel = widget.BuildMoreFlyoutContent() as StackPanel
                ?? throw new InvalidOperationException("Quick Settings More flyout must return a StackPanel.");
            var flyoutGrid = flyoutPanel.Children.OfType<UniformGrid>().FirstOrDefault()
                ?? throw new InvalidOperationException("Quick Settings More flyout must contain a UniformGrid.");
            var flyoutButtons = flyoutGrid.Children.OfType<Button>().ToArray();
            Check(flyoutButtons.Length == 14,
                "All 14 extra Quick Settings shortcuts must be hosted in the More flyout.");

            host.UpdateLayout();
            var first = strip.Children[0];
            var last = shortcuts[^1];
            var firstOrigin = first.TranslatePoint(default, strip);
            var lastOrigin = last.TranslatePoint(default, strip);
            Check(strip.Orientation == Orientation.Horizontal
                && firstOrigin.X < lastOrigin.X && Math.Abs(firstOrigin.Y - lastOrigin.Y) <= 2,
                "Core Quick Settings controls and the More button must share a single row.");

            scroll.HorizontalScrollBarVisibility = ScrollBarVisibility.Disabled;
            scroll.VerticalScrollBarVisibility = ScrollBarVisibility.Hidden;
            widget.ApplySettings(new DockSettings(Edge: DockEdge.Left));
            host.UpdateLayout();
            firstOrigin = first.TranslatePoint(default, strip);
            lastOrigin = last.TranslatePoint(default, strip);
            Check(strip.Orientation == Orientation.Vertical && firstOrigin.Y < lastOrigin.Y
                && Math.Abs(firstOrigin.X - lastOrigin.X) <= 2 && lastOrigin.Y + last.ActualHeight <= strip.ActualHeight + 1,
                $"A side dock must fit the upright controls in one column. First: {firstOrigin}, last: {lastOrigin}, strip: {strip.ActualHeight}.");
            return 8;
        }
        finally
        {
            releaseWrite.Set();
            widget.Deactivate();
            host.Close();
        }
    }

    private static void Click(ToggleButton button) => button.RaiseEvent(new RoutedEventArgs(ButtonBase.ClickEvent));

    private static void PumpUntil(Func<bool> predicate)
    {
        var frame = new DispatcherFrame();
        var timeout = System.Diagnostics.Stopwatch.StartNew();
        var timer = new DispatcherTimer { Interval = TimeSpan.FromMilliseconds(20) };
        timer.Tick += (_, _) =>
        {
            if (predicate() || timeout.Elapsed >= TimeSpan.FromSeconds(5)) frame.Continue = false;
        };
        timer.Start();
        try { Dispatcher.PushFrame(frame); }
        finally { timer.Stop(); }
        Check(predicate(), "The quick settings transition did not settle.");
    }

    private static void Check(bool condition, string message)
    {
        if (!condition) throw new InvalidOperationException(message);
    }
}
