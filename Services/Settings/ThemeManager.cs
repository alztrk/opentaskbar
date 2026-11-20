using System.Windows;
using System.Windows.Media;
using OpenTaskbar.Models;

namespace OpenTaskbar.Services;

public static class ThemeManager
{
    // Dock and menu glass take the surface opacity, so a theme pack replaces their source instead of the material.
    private static readonly string[] MaterialKeys = ["DockMaterial", "MenuMaterial"];
    private static ResourceDictionary? _palette;
    private static readonly ResourceDictionary Materials = new();
    // Theme pack brushes; merged last so they win over the palette.
    private static readonly ResourceDictionary Overrides = new();
    private static readonly Dictionary<string, Brush> MaterialSources = new(StringComparer.Ordinal);
    private static double _opacity = 0.9;
    private static double _settingsOpacity = 0.95;

    public static void Apply(AppTheme theme, double opacity = 0.9)
    {
        var source = $"/OpenTaskbar;component/Design/Palettes/{theme}.xaml";
        var dictionaries = Application.Current.Resources.MergedDictionaries;
        if (_palette?.Source?.OriginalString != source)
        {
            var next = new ResourceDictionary { Source = new Uri(source, UriKind.Relative) };
            if (_palette is not null) dictionaries.Remove(_palette);
            dictionaries.Add(next);
            _palette = next;
        }
        dictionaries.Remove(Materials);
        dictionaries.Remove(Overrides);
        dictionaries.Add(Materials);
        dictionaries.Add(Overrides);
        SetSurfaceOpacity(opacity);
        SetSettingsOpacity(ThemeSettingsOpacity());
    }

    /// <summary>True for the dictionaries this class owns, which theme packs must not treat as token definitions.</summary>
    internal static bool IsManagedDictionary(ResourceDictionary dictionary) =>
        ReferenceEquals(dictionary, _palette) || ReferenceEquals(dictionary, Materials) || ReferenceEquals(dictionary, Overrides);

    /// <summary>Replaces the theme pack's brush and color overrides; an empty map restores the palette.</summary>
    public static void SetOverrides(IReadOnlyDictionary<string, object> values)
    {
        Overrides.Clear();
        MaterialSources.Clear();
        foreach (var (key, value) in values)
        {
            if (MaterialKeys.Contains(key) && value is Brush brush) MaterialSources[key] = brush;
            else Overrides[key] = value;
        }

        if (Overrides.Contains("DockBackgroundMode") && Overrides["DockBackgroundMode"] is "None")
        {
            Overrides["DockBorderMaterial"] = Brushes.Transparent;
            Overrides["DockHighlight"] = Brushes.Transparent;
        }
        if (_palette is not null) { SetSurfaceOpacity(_opacity); SetSettingsOpacity(ThemeSettingsOpacity()); }
    }

    public static void SetSurfaceOpacity(double opacity)
    {
        if (!double.IsFinite(opacity) || opacity is < 0.6 or > 1) throw new ArgumentOutOfRangeException(nameof(opacity));
        if (_palette is null) throw new InvalidOperationException("The theme must be initialized before its materials.");
        _opacity = opacity;
        var alpha = (byte)Math.Round(opacity * 255);
        var mode = Overrides.Contains("DockBackgroundMode")
            ? Overrides["DockBackgroundMode"] as string
            : Application.Current.Resources["DockBackgroundMode"] as string;
        foreach (var key in MaterialKeys)
        {
            var source = MaterialSources.TryGetValue(key, out var custom) ? custom : (Brush)_palette[key];
            Materials[key] = key == "DockMaterial" && mode == "None"
                ? Brushes.Transparent
                : WithAlpha(source, key == "DockMaterial" && mode == "Solid" ? byte.MaxValue : alpha);
        }
        Materials["DockShadowMaterial"] = mode == "None" ? null : Application.Current.TryFindResource("ShadowDock");
    }

    private static double ThemeSettingsOpacity()
    {
        if (Overrides.Contains("SettingsBackgroundOpacity") && Overrides["SettingsBackgroundOpacity"] is double themed)
            return Math.Clamp(themed, 0.6, 1);
        return Application.Current.TryFindResource("SettingsBackgroundOpacity") is double fallback
            ? Math.Clamp(fallback, 0.6, 1) : 0.95;
    }

    private static Brush WithScaledAlpha(Brush source, double opacity)
    {
        var brush = source.Clone();
        if (brush is SolidColorBrush solid)
        {
            var c = solid.Color;
            solid.Color = Color.FromArgb((byte)Math.Round(c.A * opacity), c.R, c.G, c.B);
        }
        else if (brush is GradientBrush gradient)
        {
            foreach (var stop in gradient.GradientStops)
            {
                var c = stop.Color;
                stop.Color = Color.FromArgb((byte)Math.Round(c.A * opacity), c.R, c.G, c.B);
            }
        }
        brush.Freeze();
        return brush;
    }

    public static void SetSettingsOpacity(double opacity)
    {
        if (!double.IsFinite(opacity) || opacity is < 0.6 or > 1) throw new ArgumentOutOfRangeException(nameof(opacity));
        if (_palette is null) throw new InvalidOperationException("The theme must be initialized before its materials.");
        _settingsOpacity = opacity;
        var source = Overrides.Contains("SettingsBackground") ? Overrides["SettingsBackground"] : _palette["SettingsBackground"];
        Materials["SettingsGlassMaterial"] = WithScaledAlpha((Brush)source, opacity);
    }

    private static Brush WithAlpha(Brush source, byte alpha)
    {
        var brush = source.Clone();
        switch (brush)
        {
            case SolidColorBrush solid:
                solid.Color = Color.FromArgb(alpha, solid.Color.R, solid.Color.G, solid.Color.B);
                break;
            case GradientBrush gradient:
                foreach (var stop in gradient.GradientStops)
                {
                    stop.Color = Color.FromArgb(alpha, stop.Color.R, stop.Color.G, stop.Color.B);
                }
                break;
        }

        brush.Freeze();
        return brush;
    }
}
