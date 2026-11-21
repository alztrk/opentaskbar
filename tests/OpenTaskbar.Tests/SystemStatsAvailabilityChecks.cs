using System.Windows;
using OpenTaskbar.Models;
using OpenTaskbar.Widgets;

internal static class SystemStatsAvailabilityChecks
{
    public static int Run(Application app)
    {
        var checks = 0;
        void Check(bool condition, string message)
        {
            if (!condition) throw new InvalidOperationException(message);
            checks++;
        }

        var settings = new DockSettings
        {
            StatsMeters = SystemMeters.Cpu | SystemMeters.Memory | SystemMeters.Gpu | SystemMeters.GpuTemperature
        };
        using var widget = new SystemStatsWidget();
        widget.ApplySettings(settings);
        var widthChanges = 0;
        widget.ContentWidthChanged += (_, _) => widthChanges++;
        widget.Activate();

        Check(widget.GpuButton.Visibility == Visibility.Collapsed
              && widget.GpuTemperatureButton.Visibility == Visibility.Collapsed,
            "A newly activated widget must not show empty sensor tiles before a real reading.");

        widget.ApplySensorReading(widget.GpuButton, 0.1);
        Check(widget.GpuButton.Visibility == Visibility.Collapsed && widthChanges == 0,
            "A single reading must not resize the dock while the meter is still being validated.");
        widget.ApplySensorReading(widget.GpuButton, 0.2);
        Check(widget.GpuButton.Visibility == Visibility.Visible && widthChanges == 1,
            "A validated meter must appear and notify the host to refit the dock.");

        widget.ApplySensorReading(widget.GpuTemperatureButton, 48);
        widget.ApplySensorReading(widget.GpuTemperatureButton, 49);
        Check(widget.GpuTemperatureButton.Visibility == Visibility.Visible && widthChanges == 2,
            "The GPU temperature tile uses the same validation as GPU usage.");

        for (var i = 0; i < 4; i++) widget.ApplySensorReading(widget.GpuButton, null);
        Check(widget.GpuButton.Visibility == Visibility.Visible && widthChanges == 2,
            "A few transient misses must not make the dock jump in width.");
        widget.ApplySensorReading(widget.GpuButton, null);
        Check(widget.GpuButton.Visibility == Visibility.Collapsed && widthChanges == 3,
            "A meter that keeps failing must be hidden and trigger a refit.");

        widget.ApplySensorReading(widget.GpuButton, 0.3);
        Check(widget.GpuButton.Visibility == Visibility.Collapsed,
            "One recovered reading must not bring the meter back immediately.");
        widget.ApplySensorReading(widget.GpuButton, 0.4);
        Check(widget.GpuButton.Visibility == Visibility.Visible && widthChanges == 4,
            "Repeated recovered readings must restore the meter and refit the dock.");

        widget.Deactivate();
        return checks;
    }
}
