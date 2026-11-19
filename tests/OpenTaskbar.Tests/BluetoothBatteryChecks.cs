using System.Windows;
using System.Windows.Controls;
using OpenTaskbar.Widgets;

internal static class BluetoothBatteryChecks
{
    public static int Run(Application app)
    {
        var checks = 0;
        void Check(bool condition, string message)
        {
            if (!condition) throw new InvalidOperationException(message);
            checks++;
        }

        CheckWidgetProperties(Check);
        CheckDisplayWithDevices(Check);
        CheckMultipleDevices(Check);
        CheckClassification(Check);
        CheckNodeSelection(Check);
        CheckCompactMode(Check);
        CheckHoverFlyout(Check);

        return checks;
    }

    private static void CheckWidgetProperties(Action<bool, string> check)
    {
        var widget = new BluetoothBatteryWidget
        {
            BatteryReader = () => Task.FromResult<IReadOnlyList<BluetoothDeviceBatteryInfo>>([])
        };
        check(BluetoothBatteryWidget.Id == "BluetoothBattery", "Id must be 'BluetoothBattery'.");
        check(widget.SupportsCompactMode, "BluetoothBatteryWidget must support compact mode.");
        check(widget.ShrinkPriority == 32, "Shrink priority must be 32.");
        check(widget.CompactWidthEstimate == 46, "Compact width estimate must be 46 DIP.");
        check(widget.FullWidthEstimate == 78, "Full width estimate must be 78 DIP.");

        check(widget.Visibility == Visibility.Collapsed, "Widget must start collapsed before activation.");
        widget.Activate();
        check(widget.Visibility == Visibility.Visible, "Widget must become visible on activation.");
        widget.Deactivate();
        check(widget.Visibility == Visibility.Collapsed, "Widget must collapse on deactivation.");
    }

    private static void CheckDisplayWithDevices(Action<bool, string> check)
    {
        var widget = new BluetoothBatteryWidget();
        widget.BatteryReader = () => Task.FromResult<IReadOnlyList<BluetoothDeviceBatteryInfo>>(
        [
            new("ROG HARPE ACE AIM LAB", "ble1", true, 53),
            new("Xbox Wireless Controller", "ble2", false, 8)
        ]);

        widget.Activate();
        widget.RefreshBatteriesAsync().GetAwaiter().GetResult();

        check(widget.Content is Button, "Widget content must be a Button.");
        var button = (Button)widget.Content;
        check(button.ToolTip?.ToString()?.Contains("ROG HARPE ACE AIM LAB: 53%") == true,
            "Tooltip must contain device name and battery percentage.");

        // Test low battery state
        widget.BatteryReader = () => Task.FromResult<IReadOnlyList<BluetoothDeviceBatteryInfo>>(
        [
            new("Xbox Controller Low", "ble3", true, 12)
        ]);
        widget.RefreshBatteriesAsync().GetAwaiter().GetResult();

        check(button.ToolTip?.ToString()?.Contains("12%") == true,
            "Low battery device must be reflected in display.");
    }

    private static void CheckMultipleDevices(Action<bool, string> check)
    {
        var widget = new BluetoothBatteryWidget
        {
            BatteryReader = () => Task.FromResult<IReadOnlyList<BluetoothDeviceBatteryInfo>>(
            [
                new("Sony WH-1000XM5", "a", true, 85, BluetoothDeviceKind.Headphones),
                new("Logitech MX Master", "b", true, 42, BluetoothDeviceKind.Mouse),
                new("Apple Watch", "c", true, 17, BluetoothDeviceKind.Watch),
                new("Disconnected Pad", "d", false, 90, BluetoothDeviceKind.Gamepad)
            ])
        };
        widget.Activate();
        widget.RefreshBatteriesAsync().GetAwaiter().GetResult();

        var button = (Button)widget.Content!;
        var tiles = (StackPanel)button.Content!;
        check(tiles.Children.Count == 3, "Only connected devices must be listed in the card.");
        check(tiles.Orientation == Orientation.Horizontal, "Device tiles must be laid out side by side.");
        var percents = tiles.Children.OfType<Border>()
            .Select(tile => ((StackPanel)tile.Child!).Children[1])
            .OfType<TextBlock>().Select(text => text.Text).ToArray();
        check(percents.SequenceEqual(["85%", "42%", "17%"]), "Each tile must show its own battery percentage.");
        check(widget.FullWidthEstimate > 78, "The card must widen to fit several devices side by side.");
        var tooltip = button.ToolTip?.ToString() ?? string.Empty;
        check(tooltip.Contains("Sony WH-1000XM5: 85%") && tooltip.Contains("Apple Watch: 17%"),
            "The card tooltip must list every connected device.");
    }

    private static void CheckClassification(Action<bool, string> check)
    {
        // Generic classification from node name / Bluetooth service UUID — no model names involved.
        check(BluetoothBatteryWidget.ClassifyNode("Any Model XYZ Hands-Free AG",
                "BTHENUM\\{0000111e-0000-1000-8000-00805f9b34fb}_VID&00011532") == BluetoothDeviceKind.Headphones,
            "Any HFP headset node must be detected from its type, regardless of model name.");
        check(BluetoothBatteryWidget.ClassifyNode("Any Model XYZ", "BTHENUM\\{0000110b-0000-1000-8000-00805f9b34fb}") == BluetoothDeviceKind.Headphones,
            "Any A2DP audio node must be detected from its service UUID.");
        check(BluetoothBatteryWidget.ClassifyNode("Any Model XYZ Mouse", "") == BluetoothDeviceKind.Mouse,
            "Any mouse must be detected from its node name.");
        check(BluetoothBatteryWidget.ClassifyNode("Any Model XYZ Keyboard", "") == BluetoothDeviceKind.Keyboard,
            "Any keyboard must be detected from its node name.");
        check(BluetoothBatteryWidget.ClassifyNode("Any Model XYZ Gamepad", "") == BluetoothDeviceKind.Gamepad,
            "Any gamepad must be detected from its node name.");
        check(BluetoothBatteryWidget.ClassifyNode("Bilinmeyen Cihaz", "") == BluetoothDeviceKind.Generic,
            "Unknown devices must fall back to the generic icon.");
    }

    private static void CheckNodeSelection(Action<bool, string> check)
    {
        var older = new DateTimeOffset(2026, 11, 18, 10, 0, 0, TimeSpan.Zero);
        var newer = new DateTimeOffset(2026, 11, 19, 21, 0, 0, TimeSpan.Zero);

        check(BluetoothBatteryWidget.ShouldPreferNode(false, newer, true, older),
            "An active (Present=true) PnP node must always replace a stale ghost (Present=false) node.");
        check(!BluetoothBatteryWidget.ShouldPreferNode(true, older, false, newer),
            "A disconnected ghost (Present=false) node must never overwrite an active (Present=true) node.");
        check(BluetoothBatteryWidget.ShouldPreferNode(true, older, true, newer),
            "When two active nodes share a container, the newer arrival must win.");
        check(!BluetoothBatteryWidget.ShouldPreferNode(true, newer, true, older),
            "An older active node must not overwrite a newer active node.");
    }

    private static void CheckCompactMode(Action<bool, string> check)
    {
        var widget = new BluetoothBatteryWidget();
        widget.SetCompactState(true);
        check(widget.IsCompact, "Widget must enter compact mode.");
        widget.SetCompactState(false);
        check(!widget.IsCompact, "Widget must exit compact mode.");
    }

    private static void CheckHoverFlyout(Action<bool, string> check)
    {
        var widget = new BluetoothBatteryWidget();
        var clicked = false;
        widget.OpenSettingsAction = () => clicked = true;

        var flyout = widget.BuildHoverFlyoutContent();
        check(flyout is StackPanel, "Hover flyout must return a StackPanel.");

        if (widget.Content is Button button)
        {
            button.RaiseEvent(new RoutedEventArgs(Button.ClickEvent));
            check(clicked, "Clicking the card button must invoke the settings action.");
        }
    }
}
