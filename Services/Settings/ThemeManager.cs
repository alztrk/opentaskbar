using System.Windows;
using System.Windows.Media;
using System.Windows.Media.Effects;
using OpenTaskbar.Models;

namespace OpenTaskbar.Services;

/// <summary>
/// Owns every generated surface material. Each of the four surfaces has its own background mode and
/// opacity, so changing the dock cannot wash out menus, widget panels or the Settings window anymore.
/// Also exposes the legacy single-opacity API used by the theme editor and Windows theme detection.
/// </summary>
public static class ThemeManager
{
    // Theme packs replace these sources instead of the generated materials (see the comment below).
    private static readonly string[] MaterialKeys = ["DockMaterial", "MenuMaterial"];
    private static ResourceDictionary? _palette;
    private static readonly ResourceDictionary Materials = new();
    // Windows-derived brushes; a selected theme pack still wins over these.
    private static readonly ResourceDictionary SystemOverrides = new();
    // Theme pack brushes; merged last so they win over the palette and Windows colors.
    private static readonly ResourceDictionary Overrides = new();
    private static readonly Dictionary<string, Brush> MaterialSources = new(StringComparer.Ordinal);
    private static readonly Dictionary<string, Brush> SystemMaterialSources = new(StringComparer.Ordinal);
    private static readonly Dictionary<SurfaceKind, SurfaceStyle> Styles = new();
    public static AppTheme Current { get; private set; } = AppTheme.Dark;

    // Legacy single-opacity state (pr-62 theme editor). The four-surface Styles below remain authoritative.
    private static double _opacity = 0.9;
    private static double _settingsOpacity = 0.95;
    public static AppTheme EffectiveTheme { get; private set; } = AppTheme.Dark;

    private static AppTheme WindowsTheme()
    {
        try
        {
            using var key = Microsoft.Win32.Registry.CurrentUser.OpenSubKey(
                @"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize");
            return key?.GetValue("AppsUseLightTheme") is int light && light != 0
                ? AppTheme.Light : AppTheme.Dark;
        }
        catch (Exception ex) when (ex is System.Security.SecurityException or UnauthorizedAccessException)
        {
            return AppTheme.Dark;
        }
    }

    public static void Apply(AppTheme theme, DockSettings? settings = null)
    {
        Current = theme;
        EffectiveTheme = theme == AppTheme.System ? WindowsTheme() : theme;
        var source = $"/OpenTaskbar;component/Design/Palettes/{EffectiveTheme}.xaml";
        var dictionaries = Application.Current.Resources.MergedDictionaries;
        if (_palette?.Source?.OriginalString != source)
        {
            var next = new ResourceDictionary { Source = new Uri(source, UriKind.Relative) };
            if (_palette is not null) dictionaries.Remove(_palette);
            dictionaries.Add(next);
            _palette = next;
        }
        dictionaries.Remove(Materials);
        dictionaries.Remove(SystemOverrides);
        dictionaries.Remove(Overrides);
        dictionaries.Add(Materials);
        dictionaries.Add(SystemOverrides);
        dictionaries.Add(Overrides);
        RadiusManager.Initialize();

        if (settings is null)
        {
            Styles.Clear();
            foreach (var kind in Enum.GetValues<SurfaceKind>())
                Styles[kind] = new SurfaceStyle(SurfaceBackground.Glass, 0.9);
        }
        else
        {
            Styles[SurfaceKind.Dock] = new SurfaceStyle(settings.DockBackground, settings.DockOpacity);
            Styles[SurfaceKind.Menu] = new SurfaceStyle(settings.MenuBackground, settings.MenuOpacity);
            Styles[SurfaceKind.Widget] = new SurfaceStyle(settings.WidgetBackground, settings.WidgetOpacity);
            Styles[SurfaceKind.Settings] = new SurfaceStyle(settings.SettingsBackground, settings.SettingsOpacity);
        }

        foreach (var kind in Enum.GetValues<SurfaceKind>()) Rebuild(kind);
        _opacity = Styles.TryGetValue(SurfaceKind.Dock, out var dock) ? dock.Opacity : 0.9;
        _settingsOpacity = Styles.TryGetValue(SurfaceKind.Settings, out var settingsStyle) ? settingsStyle.Opacity : 0.95;
    }

    /// <summary>True for the dictionaries this class owns, which theme packs must not treat as token definitions.</summary>
    internal static bool IsManagedDictionary(ResourceDictionary dictionary) =>
        ReferenceEquals(dictionary, _palette) || ReferenceEquals(dictionary, Materials)
        || ReferenceEquals(dictionary, SystemOverrides) || ReferenceEquals(dictionary, Overrides)
        || ReferenceEquals(dictionary, RadiusManager.Dictionary);

    /// <summary>
    /// Applies colors derived from the user's Windows personalization settings. They sit below theme
    /// pack overrides, so an explicitly selected pack keeps full control of the keys it defines.
    /// </summary>
    public static void SetWindowsPalette(IReadOnlyDictionary<string, object> values)
    {
        SystemOverrides.Clear();
        SystemMaterialSources.Clear();
        foreach (var (key, value) in values)
        {
            if (MaterialKeys.Contains(key) && value is Brush brush) SystemMaterialSources[key] = brush;
            else SystemOverrides[key] = value;
        }

        if (_palette is not null)
            foreach (var kind in Enum.GetValues<SurfaceKind>()) Rebuild(kind);
        RadiusManager.Rebuild();
    }

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

        if ((Overrides.Contains("DockBackgroundMode")
                ? Overrides["DockBackgroundMode"] : Application.Current.TryFindResource("DockBackgroundMode")) is "None")
        {
            Overrides["DockBorderMaterial"] = Brushes.Transparent;
            Overrides["DockHighlight"] = Brushes.Transparent;
        }
        if (_palette is not null)
        {
            foreach (var kind in Enum.GetValues<SurfaceKind>()) Rebuild(kind);
            RadiusManager.Rebuild();
        }
    }

    public static SurfaceStyle GetSurface(SurfaceKind kind) =>
        Styles.TryGetValue(kind, out var style) ? style : new SurfaceStyle(SurfaceBackground.Glass, 0.9);

    /// <summary>Live preview of one surface from the Settings window.</summary>
    public static void SetSurface(SurfaceKind kind, SurfaceBackground background, double opacity)
    {
        if (!Enum.IsDefined(kind) || !Enum.IsDefined(background)
            || !double.IsFinite(opacity) || opacity is < 0 or > 1)
            throw new ArgumentOutOfRangeException(nameof(opacity));
        if (_palette is null) throw new InvalidOperationException("The theme must be initialized before its materials.");
        Styles[kind] = new SurfaceStyle(background, opacity);
        if (kind == SurfaceKind.Dock) _opacity = opacity;
        Rebuild(kind);
    }

    /// <summary>
    /// Legacy single-opacity API: updates the dock/menu surface opacity while keeping the
    /// per-surface style system intact.
    /// </summary>
    public static void SetSurfaceOpacity(double opacity)
    {
        if (!double.IsFinite(opacity) || opacity is < 0 or > 1) throw new ArgumentOutOfRangeException(nameof(opacity));
        if (_palette is null) throw new InvalidOperationException("The theme must be initialized before its materials.");
        _opacity = opacity;
        foreach (var kind in new[] { SurfaceKind.Dock, SurfaceKind.Menu })
        {
            var style = GetSurface(kind);
            Styles[kind] = new SurfaceStyle(style.Background, opacity);
            Rebuild(kind);
        }
    }

    private static void Rebuild(SurfaceKind kind)
    {
        var style = GetSurface(kind);
        switch (kind)
        {
            case SurfaceKind.Dock:
                Materials["DockMaterial"] = Fill(MaterialSource("DockMaterial"), style);
                Materials["DockBorderMaterial"] = Stroke(ResolveBrush("DockBorderMaterial"), style);
                Materials["DockShadow"] = Shadow("ShadowDock", style);
                break;
            case SurfaceKind.Menu:
                Materials["MenuMaterial"] = Fill(MaterialSource("MenuMaterial"), style);
                Materials["MenuStrokeMaterial"] = Stroke(ResolveBrush("MenuStroke"), style);
                Materials["MenuSheenMaterial"] = Sheen(ResolveBrush("DockHighlight"), style);
                Materials["MenuShadow"] = Shadow("ShadowMenu", style);
                break;
            case SurfaceKind.Widget:
                // The widget panel used to share MenuMaterial; it now owns the same source with its own style.
                Materials["WidgetMaterial"] = Fill(MaterialSource("MenuMaterial"), style);
                Materials["WidgetStrokeMaterial"] = Stroke(ResolveBrush("MenuStroke"), style);
                Materials["WidgetSheenMaterial"] = Sheen(ResolveBrush("DockHighlight"), style);
                break;
            case SurfaceKind.Settings:
                Materials["SettingsGlassMaterial"] = Fill(ResolveBrush("SettingsBackground"), style);
                Materials["SettingsStrokeMaterial"] = Stroke(ResolveBrush("MenuStroke"), style);
                break;
        }

        // pr-62 theme editor expects a generated drop-shadow material derived from the dock tokens.
        if (kind == SurfaceKind.Dock)
        {
            var blur = Application.Current.TryFindResource("DockShadowBlur") is double b ? Math.Clamp(b, 0, 80) : 28;
            var shadowOpacity = Application.Current.TryFindResource("DockShadowOpacity") is double op ? Math.Clamp(op, 0, 1) : 0.45;
            var shadowColor = Application.Current.TryFindResource("DockShadowColor") is SolidColorBrush colorBrush
                ? colorBrush.Color : Colors.Black;
            var shadow = new DropShadowEffect
            {
                BlurRadius = blur,
                ShadowDepth = 4,
                Direction = 270,
                Color = shadowColor,
                Opacity = style.Background == SurfaceBackground.None ? 0 : shadowOpacity
            };
            shadow.Freeze();
            Materials["DockShadowMaterial"] = shadow;
        }
    }

    public static void UseWindowsAccent(bool enabled)
    {
        if (!enabled)
        {
            Materials.Remove("AccentBrush");
            Materials.Remove("AccentText");
            return;
        }
        var original = SystemParameters.WindowGlassColor;
        if (original.A == 0) return;
        var brush = new SolidColorBrush(Color.FromRgb(original.R, original.G, original.B));
        brush.Freeze();
        Materials["AccentBrush"] = brush;
        var brightness = (original.R * 0.2126 + original.G * 0.7152 + original.B * 0.0722) / 255;
        Materials["AccentText"] = brightness > 0.56 ? Brushes.Black : Brushes.White;
    }

    private static double ThemeSettingsOpacity()
    {
        if (Overrides.Contains("SettingsBackgroundOpacity") && Overrides["SettingsBackgroundOpacity"] is double themed)
            return Math.Clamp(themed, 0.6, 1);
        if (Styles.TryGetValue(SurfaceKind.Settings, out var style))
            return Math.Clamp(style.Opacity, 0.6, 1);
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

    private static Brush ResolveBrush(string key) =>
        Overrides.Contains(key) ? (Brush)Overrides[key]
        : SystemOverrides.Contains(key) ? (Brush)SystemOverrides[key]
        : (Brush)_palette![key];

    private static Brush MaterialSource(string key) =>
        MaterialSources.TryGetValue(key, out var custom) ? custom
        : SystemMaterialSources.TryGetValue(key, out var system) ? system
        : (Brush)_palette![key];

    private static Brush Fill(Brush source, SurfaceStyle style) => style.Background switch
    {
        SurfaceBackground.None => Brushes.Transparent,
        SurfaceBackground.Solid => WithAlpha(source, 255),
        _ => WithAlpha(source, (byte)Math.Round(style.Opacity * 255))
    };

    private static Brush Stroke(Brush source, SurfaceStyle style)
    {
        if (style.Background == SurfaceBackground.None) return Brushes.Transparent;
        var brush = source.Clone();
        brush.Freeze();
        return brush;
    }

    private static Brush Sheen(Brush source, SurfaceStyle style)
    {
        if (style.Background == SurfaceBackground.None) return Brushes.Transparent;
        var brush = source.Clone();
        brush.Freeze();
        return brush;
    }

    private static Effect Shadow(string key, SurfaceStyle style)
    {
        var source = Application.Current?.Resources[key] as DropShadowEffect;
        if (source is null) return new DropShadowEffect { Opacity = 0 };
        var effect = (DropShadowEffect)source.Clone();
        if (style.Background == SurfaceBackground.None) effect.Opacity = 0;
        effect.Freeze();
        return effect;
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
