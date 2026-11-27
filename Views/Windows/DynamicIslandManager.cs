using System.Windows.Controls;
using OpenTaskbar.Models;
using OpenTaskbar.Services;

namespace OpenTaskbar.Views.Windows;

public sealed class DynamicIslandManager : IDisposable
{
    private readonly Dictionary<string, DynamicIslandWindow> _windows = new(StringComparer.OrdinalIgnoreCase);
    private readonly Action<string> _reportError;
    private readonly Action<DockSettings> _saveSettings;
    private readonly Action<string> _removeWidget;
    private readonly Func<string, IEnumerable<Control>> _createPlacementMenuItems;
    private IReadOnlyList<DisplayInfo> _displays = [];
    private bool _disposed;

    public DynamicIslandManager(
        Action<string> reportError,
        Action<DockSettings> saveSettings,
        Action<string> removeWidget,
        Func<string, IEnumerable<Control>> createPlacementMenuItems)
    {
        _reportError = reportError;
        _saveSettings = saveSettings;
        _removeWidget = removeWidget;
        _createPlacementMenuItems = createPlacementMenuItems;
        RefreshDisplays();
    }

    public bool IsWidgetPlacedOnConnectedDisplay(DockSettings settings, string widgetId) =>
        settings.DynamicIslandEnabled
        && !settings.FocusModeEnabled
        && settings.Widgets?.Contains(widgetId, StringComparer.OrdinalIgnoreCase) == true
        && DynamicIslandPolicy.IsAssignedToAnyDisplay(settings.DynamicIslandPlacements, widgetId, _displays);

    public void RefreshDisplays()
    {
        if (_disposed) return;
        _displays = DisplayService.GetDisplays();
    }

    public void Apply(
        DockSettings settings,
        IReadOnlyDictionary<string, DockWidgetItem> timedWidgets)
    {
        if (_disposed) return;
        RefreshDisplays();
        if (!DynamicIslandPolicy.ShouldCreateWindow(settings.DynamicIslandEnabled, settings.FocusModeEnabled))
        {
            foreach (var window in _windows.Values) window.Dispose();
            _windows.Clear();
            return;
        }
        var selected = new HashSet<string>(settings.Widgets ?? [], StringComparer.OrdinalIgnoreCase);
        var placements = settings.DynamicIslandPlacements ?? new Dictionary<string, string>();
        var desired = _displays.ToDictionary(
            display => display.DeviceName,
            display => DynamicIslandPolicy.WidgetsForDisplay(selected, placements, display.DeviceName),
            StringComparer.OrdinalIgnoreCase);

        foreach (var stale in _windows.Keys.Where(deviceName => !desired.ContainsKey(deviceName)).ToArray())
        {
            _windows[stale].Dispose();
            _windows.Remove(stale);
        }

        foreach (var (deviceName, widgetIds) in desired)
        {
            var display = _displays.First(item => string.Equals(
                item.DeviceName, deviceName, StringComparison.OrdinalIgnoreCase));
            if (!_windows.TryGetValue(deviceName, out var window))
            {
                window = new DynamicIslandWindow(
                    display, _reportError, _saveSettings, _removeWidget, _createPlacementMenuItems);
                _windows.Add(deviceName, window);
            }
            window.Apply(display, widgetIds, timedWidgets, settings);
        }
    }

    public void Dispose()
    {
        if (_disposed) return;
        _disposed = true;
        foreach (var window in _windows.Values) window.Dispose();
        _windows.Clear();
    }
}
