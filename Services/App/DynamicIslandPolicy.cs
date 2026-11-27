using OpenTaskbar.Models;

namespace OpenTaskbar.Services;

public static class DynamicIslandPolicy
{
    private static bool TryGetPlacement(
        IReadOnlyDictionary<string, string>? placements,
        string widgetId,
        out string? target)
    {
        target = null;
        if (placements is null) return false;
        var id = DockSettingsStore.CanonicalizeWidgetId(widgetId);
        if (placements.TryGetValue(id, out var exact))
        {
            target = exact;
            return true;
        }
        // Dictionaries created by older builds may still be case-sensitive.
        foreach (var pair in placements)
        {
            if (string.Equals(DockSettingsStore.CanonicalizeWidgetId(pair.Key), id,
                    StringComparison.OrdinalIgnoreCase))
            {
                target = pair.Value;
                return true;
            }
        }
        return false;
    }

    public static bool IsAssignedToDisplay(
        IReadOnlyDictionary<string, string>? placements,
        string widgetId,
        string deviceName) =>
        TryGetPlacement(placements, widgetId, out var target)
        && (!string.IsNullOrWhiteSpace(target)
            && (string.Equals(target, DockSettings.DynamicIslandAllDisplays, StringComparison.OrdinalIgnoreCase)
                || string.Equals(target, deviceName, StringComparison.OrdinalIgnoreCase)));

    public static bool IsAssignedToAnyDisplay(
        IReadOnlyDictionary<string, string>? placements,
        string widgetId,
        IReadOnlyList<DisplayInfo> displays) =>
        displays.Any(display => IsAssignedToDisplay(placements, widgetId, display.DeviceName));

    public static IReadOnlyList<string> WidgetsForDisplay(
        IEnumerable<string> selectedWidgets,
        IReadOnlyDictionary<string, string>? placements,
        string deviceName) =>
        selectedWidgets
            .Select(DockSettingsStore.CanonicalizeWidgetId)
            .Where(widgetId => IsAssignedToDisplay(placements, widgetId, deviceName))
            .ToArray();

    public static IReadOnlyList<string> OrderWidgets(
        IEnumerable<string> availableWidgets,
        IReadOnlyList<string>? displayOrder,
        IReadOnlyList<string>? widgetOrder,
        IReadOnlyList<string> defaultOrder,
        IEnumerable<string> timedOrder)
    {
        var available = new HashSet<string>(
            availableWidgets.Select(DockSettingsStore.CanonicalizeWidgetId),
            StringComparer.OrdinalIgnoreCase);
        return (displayOrder ?? [])
            .Concat(widgetOrder ?? [])
            .Concat(defaultOrder)
            .Concat(timedOrder)
            .Select(DockSettingsStore.CanonicalizeWidgetId)
            .Distinct(StringComparer.OrdinalIgnoreCase)
            .Where(available.Contains)
            .ToArray();
    }

    public static IReadOnlyList<string> MoveWidget(
        IReadOnlyList<string> order,
        string widgetId,
        int offset)
    {
        if (offset is not (-1 or 1)) return order;
        var index = -1;
        for (var i = 0; i < order.Count; i++)
        {
            if (string.Equals(order[i], widgetId, StringComparison.OrdinalIgnoreCase))
            {
                index = i;
                break;
            }
        }

        var target = index + offset;
        if (index < 0 || target < 0 || target >= order.Count) return order;
        var moved = order.ToArray();
        (moved[index], moved[target]) = (moved[target], moved[index]);
        return moved;
    }

    public static bool ShouldShow(
        DynamicIslandVisibilityMode visibility,
        bool desktopVisible,
        bool hideInFullscreen,
        bool fullscreen) =>
        !(hideInFullscreen && fullscreen)
        && (visibility switch
        {
            DynamicIslandVisibilityMode.Always => true,
            DynamicIslandVisibilityMode.DesktopOnly => desktopVisible,
            _ => false
        });

    public static double ClampWindowWidth(
        double preferred,
        double monitorWidth,
        double requestedMinimum,
        double requestedMaximum,
        double edgeInset = 16)
    {
        if (!double.IsFinite(monitorWidth) || monitorWidth <= 0) return Math.Clamp(preferred, 240, 960);
        var available = Math.Max(240, monitorWidth - Math.Max(0, edgeInset) * 2);
        var maximum = Math.Min(Math.Max(240, requestedMaximum), available);
        var minimum = Math.Min(Math.Max(240, requestedMinimum), maximum);
        return Math.Clamp(double.IsFinite(preferred) ? preferred : minimum, minimum, maximum);
    }
}
