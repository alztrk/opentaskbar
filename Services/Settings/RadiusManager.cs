using System.Windows;
using OpenTaskbar.Models;

namespace OpenTaskbar.Services;

/// <summary>
/// Derives every corner radius from the user's dock radius using the concentric-radius rule: a nested element
/// inset by <c>d</c> keeps radius <c>r - d</c>. Theme packs still supply the deltas (their own radius hierarchy),
/// so a pack that uses tighter corners stays tighter while following the user's master value.
/// </summary>
internal static class RadiusManager
{
    private static readonly ResourceDictionary Derived = new();

    // key -> design-time radius at the reference dock radius (Radii.xaml defaults)
    private static readonly (string Key, int AtReference)[] Keys =
    [
        ("DockInnerRadius", 19),
        ("DockRadius", 20),
        ("ControlRadius", 12),
        ("PopupRadius", 12),
        ("WidgetRadius", 12),
        ("DockWidgetRadius", 12),
        ("DockWidgetActionRadius", 5),
        ("WidgetFieldRadius", 7),
        ("WidgetQuickActionRadius", 18)
    ];

    private static int _referenceDockRadius = 20;
    private static int _dockRadius = 20;
    private static bool _initialized;

    internal static ResourceDictionary Dictionary => Derived;

    /// <summary>Pure derivation, also used by the tests.</summary>
    internal static int Derive(int dockRadius, int referenceDockRadius, int referenceValue) =>
        Math.Max(0, dockRadius - (referenceDockRadius - referenceValue));

    internal static void Apply(int dockRadius)
    {
        _dockRadius = Math.Clamp(dockRadius, 0, 48);
        if (_initialized) Rebuild();
    }

    internal static void Initialize()
    {
        if (_initialized) return;
        var dictionaries = Application.Current.Resources.MergedDictionaries;
        dictionaries.Remove(Derived);
        dictionaries.Add(Derived);
        _initialized = true;
        Rebuild();
    }

    /// <summary>Recomputes from the current theme-pack token values (the radius hierarchy) and the user's radius.</summary>
    internal static void Rebuild()
    {
        if (!_initialized) return;
        _referenceDockRadius = FindBase("DockRadius", 20);
        Derived["DockRadius"] = new CornerRadius(_dockRadius);

        foreach (var (key, atReference) in Keys)
        {
            if (string.Equals(key, "DockRadius", StringComparison.Ordinal)) continue;
            var reference = FindBase(key, atReference);
            Derived[key] = new CornerRadius(Derive(_dockRadius, _referenceDockRadius, reference));
        }

        // The sheen only rounds the top corners; its horizontal inset follows the same concentric rule.
        var sheen = Derive(_dockRadius, _referenceDockRadius, FindBase("DockSheenRadius", 19));
        Derived["DockSheenRadius"] = new CornerRadius(sheen, sheen, 0, 0);
    }

    private static int FindBase(string key, int fallback)
    {
        var app = Application.Current;
        if (app is null) return fallback;
        return FindBase(app.Resources, key) ?? fallback;
    }

    private static int? FindBase(ResourceDictionary dictionary, string key)
    {
        for (var i = dictionary.MergedDictionaries.Count - 1; i >= 0; i--)
        {
            var merged = dictionary.MergedDictionaries[i];
            if (ThemeManager.IsManagedDictionary(merged)) continue;
            if (FindBase(merged, key) is { } nested) return nested;
        }

        return dictionary.Contains(key) && !ThemeManager.IsManagedDictionary(dictionary)
            && dictionary[key] is CornerRadius radius ? (int)Math.Round(radius.TopLeft) : null;
    }
}
