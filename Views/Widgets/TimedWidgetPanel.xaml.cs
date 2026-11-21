using System.Globalization;
using System.Windows;
using System.Windows.Controls;
using OpenTaskbar.Models;
using OpenTaskbar.Services;

namespace OpenTaskbar.Views.Widgets;

public partial class TimedWidgetPanel : UserControl
{
    public TimedWidgetPanel() => InitializeComponent();

    private void Toggle_Click(object sender, RoutedEventArgs e)
    {
        if (DataContext is ITimedDockWidget widget) widget.Toggle();
    }

    private void Reset_Click(object sender, RoutedEventArgs e)
    {
        if (DataContext is ITimedDockWidget widget) widget.Reset();
    }

    private void Lap_Click(object sender, RoutedEventArgs e)
    {
        if (DataContext is StopwatchDockWidget stopwatch) stopwatch.RecordLap();
    }

    private void CountdownPreset_Click(object sender, RoutedEventArgs e)
    {
        if (DataContext is CountdownDockWidget countdown
            && sender is Button { Tag: string raw }
            && int.TryParse(raw, NumberStyles.Integer, CultureInfo.InvariantCulture, out var minutes))
        {
            countdown.SetPresetMinutes(minutes);
        }
    }
}
