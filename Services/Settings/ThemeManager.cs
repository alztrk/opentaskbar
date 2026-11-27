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
    // OLED/display-care replacements; merged after theme packs so "reduce glow" always wins.
    private static readonly ResourceDictionary OledOverrides = new();
    private static readonly Dictionary<string, Brush> MaterialSources = new(StringComparer.Ordinal);
    private static readonly Dictionary<string, Brush> SystemMaterialSources = new(StringComparer.Ordinal);
    private static readonly Dictionary<SurfaceKind, SurfaceStyle> Styles = new();
    private static readonly SolidColorBrush TrueBlackBrush = CreateTrueBlackBrush();

    private static SolidColorBrush CreateTrueBlackBrush()
    {
        var brush = new SolidColorBrush(Colors.Black);
        brush.Freeze();
        return brush;
    }

    /// <summary>True for an opaque pure-black fill: an explicit TrueBlack surface or a forced OLED state.</summary>
    private static bool IsTrueBlack(SurfaceStyle style) =>
        style.Background != SurfaceBackground.None
        && (style.Background == SurfaceBackground.TrueBlack || _forceDisplayCare);
    // Resource key prefixes for each surface's generated stroke thickness/color, e.g. "MenuStrokeThickness".
    private static string StrokePrefix(SurfaceKind kind) => kind switch
    {
        SurfaceKind.Dock => "DockBorder",
        SurfaceKind.Menu => "MenuStroke",
        SurfaceKind.Widget => "WidgetStroke",
        SurfaceKind.Settings => "SettingsStroke",
        _ => "MenuStroke"
    };
    public static AppTheme Current { get; private set; } = AppTheme.Dark;
    /// <summary>Raised after any palette, override, or surface style change rebuilds generated materials.</summary>
    public static event EventHandler? ThemeChanged;

    // Legacy single-opacity state (pr-62 theme editor). The four-surface Styles below remain authoritative.
    private static double _opacity = 0.9;
    private static double _settingsOpacity = 0.95;
    private static bool _reduceGlow;
    private static bool _forceDisplayCare;
    /// <summary>True while the automatic low-power trigger is temporarily applying the OLED look.</summary>
    public static bool IsDisplayCareForced => _forceDisplayCare;
    public static AppTheme EffectiveTheme { get; private set; } = AppTheme.Dark;

    internal static AppTheme WindowsTheme()
    {
        try
        {
            using var key = Microsoft.Win32.Registry.CurrentUser.OpenSubKey(
                @"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize");
            // OpenTaskbar replaces the Windows taskbar, so follow the taskbar/Start mode first
            // (SystemUsesLightTheme) and fall back to AppsUseLightTheme when absent.
            var value = key?.GetValue("SystemUsesLightTheme") ?? key?.GetValue("AppsUseLightTheme");
            return value is int light && light != 0
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
        dictionaries.Remove(OledOverrides);
        dictionaries.Add(Materials);
        dictionaries.Add(SystemOverrides);
        dictionaries.Add(Overrides);
        dictionaries.Add(OledOverrides);
        RadiusManager.Initialize();
        _reduceGlow = settings?.OledReduceGlowEnabled ?? false;

        if (settings is null)
        {
            Styles.Clear();
            foreach (var kind in Enum.GetValues<SurfaceKind>())
                Styles[kind] = new SurfaceStyle(SurfaceBackground.Glass, 0.9);
        }
        else
        {
            Styles[SurfaceKind.Dock] = new SurfaceStyle(settings.DockBackground, settings.DockOpacity, settings.DockBorderThickness, settings.DockBorderColor);
            Styles[SurfaceKind.Menu] = new SurfaceStyle(settings.MenuBackground, settings.MenuOpacity, settings.MenuBorderThickness, settings.MenuBorderColor);
            Styles[SurfaceKind.Widget] = new SurfaceStyle(settings.WidgetBackground, settings.WidgetOpacity, settings.WidgetBorderThickness, settings.WidgetBorderColor);
            Styles[SurfaceKind.Settings] = new SurfaceStyle(settings.SettingsBackground, settings.SettingsOpacity, settings.SettingsBorderThickness, settings.SettingsBorderColor);
        }

        RebuildOledOverrides();
        foreach (var kind in Enum.GetValues<SurfaceKind>()) Rebuild(kind);
        _opacity = Styles.TryGetValue(SurfaceKind.Dock, out var dock) ? dock.Opacity : 0.9;
        _settingsOpacity = Styles.TryGetValue(SurfaceKind.Settings, out var settingsStyle) ? settingsStyle.Opacity : 0.95;
        NotifyThemeChanged();
    }

    /// <summary>True when the active theme and all four surface styles already match the saved settings.</summary>
    public static bool IsApplied(DockSettings settings) =>
        _palette is not null
        && Current == settings.Theme
        && GetSurface(SurfaceKind.Dock) == new SurfaceStyle(settings.DockBackground, settings.DockOpacity, settings.DockBorderThickness, settings.DockBorderColor)
        && GetSurface(SurfaceKind.Menu) == new SurfaceStyle(settings.MenuBackground, settings.MenuOpacity, settings.MenuBorderThickness, settings.MenuBorderColor)
        && GetSurface(SurfaceKind.Widget) == new SurfaceStyle(settings.WidgetBackground, settings.WidgetOpacity, settings.WidgetBorderThickness, settings.WidgetBorderColor)
        && GetSurface(SurfaceKind.Settings) == new SurfaceStyle(settings.SettingsBackground, settings.SettingsOpacity, settings.SettingsBorderThickness, settings.SettingsBorderColor)
        && _reduceGlow == settings.OledReduceGlowEnabled;

    /// <summary>True for the dictionaries this class owns, which theme packs must not treat as token definitions.</summary>
    internal static bool IsManagedDictionary(ResourceDictionary dictionary) =>
        ReferenceEquals(dictionary, _palette) || ReferenceEquals(dictionary, Materials)
        || ReferenceEquals(dictionary, SystemOverrides) || ReferenceEquals(dictionary, Overrides)
        || ReferenceEquals(dictionary, OledOverrides)
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
        if (SystemOverrides.Contains("SurfaceHover") && !SystemOverrides.Contains("SurfaceHoverColor")
            && SystemOverrides["SurfaceHover"] is SolidColorBrush systemHover)
            SystemOverrides["SurfaceHoverColor"] = systemHover.Color;

        if (_palette is not null)
        {
            RebuildOledOverrides();
            foreach (var kind in Enum.GetValues<SurfaceKind>()) Rebuild(kind);
        }
        RadiusManager.Rebuild();
        if (_palette is not null) NotifyThemeChanged();
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
        if (Overrides.Contains("SurfaceHover") && !Overrides.Contains("SurfaceHoverColor")
            && Overrides["SurfaceHover"] is SolidColorBrush overrideHover)
            Overrides["SurfaceHoverColor"] = overrideHover.Color;

        if ((Overrides.Contains("DockBackgroundMode")
                ? Overrides["DockBackgroundMode"] : Application.Current.TryFindResource("DockBackgroundMode")) is "None")
        {
            Overrides["DockBorderMaterial"] = Brushes.Transparent;
            Overrides["DockHighlight"] = Brushes.Transparent;
        }
        if (_palette is not null)
        {
            RebuildOledOverrides();
            foreach (var kind in Enum.GetValues<SurfaceKind>()) Rebuild(kind);
            RadiusManager.Rebuild();
            NotifyThemeChanged();
        }
    }

    /// <summary>Live preview of the "reduce glow and shadows" switch from Settings.</summary>
    public static void SetReduceGlow(bool enabled)
    {
        if (_reduceGlow == enabled) return;
        _reduceGlow = enabled;
        if (_palette is not null)
        {
            RebuildOledOverrides();
            foreach (var kind in Enum.GetValues<SurfaceKind>()) Rebuild(kind);
            NotifyThemeChanged();
        }
    }

    /// <summary>
    /// Temporarily forces the pure-black, glow-free look without changing saved settings. Used by the
    /// automatic low-power trigger; the saved theme stays authoritative once this is cleared.
    /// </summary>
    public static void SetForcedDisplayCare(bool enabled)
    {
        if (_forceDisplayCare == enabled) return;
        _forceDisplayCare = enabled;
        if (_palette is not null)
        {
            RebuildOledOverrides();
            foreach (var kind in Enum.GetValues<SurfaceKind>()) Rebuild(kind);
            NotifyThemeChanged();
        }
    }

    private static bool FlatOled => _reduceGlow || _forceDisplayCare;

    /// <summary>
    /// Replaces the gradient glow tokens with flat equivalents. These keys are consumed directly as
    /// DynamicResources, so they cannot be handled by the generated per-surface materials alone.
    /// </summary>
    private static void RebuildOledOverrides()
    {
        OledOverrides.Clear();
        if (!FlatOled || _palette is null) return;

        var transparent = Brushes.Transparent;
        OledOverrides["IconHoverGlow"] = transparent;
        OledOverrides["DockHighlight"] = transparent;
        OledOverrides["DockInnerBevelMaterial"] = transparent;

        // Keep the accent color, but drop the gradient so no lit pixel is wasted on a decorative fill.
        var accent = ResolveBrush("AccentBrush") is SolidColorBrush solid
            ? solid.Color
            : Colors.LightSkyBlue;
        var flat = new SolidColorBrush(accent);
        flat.Freeze();
        OledOverrides["WidgetAccentFill"] = flat;

        var noShadow = new DropShadowEffect { Opacity = 0 };
        noShadow.Freeze();
        OledOverrides["ShadowDock"] = noShadow;
        OledOverrides["ShadowMenu"] = noShadow;
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
        var current = GetSurface(kind);
        Styles[kind] = new SurfaceStyle(background, opacity, current.BorderThickness, current.BorderColor);
        if (kind == SurfaceKind.Dock) _opacity = opacity;
        Rebuild(kind);
        NotifyThemeChanged();
    }

    /// <summary>Live preview of a surface's border thickness/color from the Settings window.</summary>
    public static void SetBorder(SurfaceKind kind, double thickness, string? color)
    {
        if (!Enum.IsDefined(kind) || !double.IsFinite(thickness) || thickness is < 0 or > 12)
            throw new ArgumentOutOfRangeException(nameof(thickness));
        if (_palette is null) throw new InvalidOperationException("The theme must be initialized before its materials.");
        var current = GetSurface(kind);
        Styles[kind] = new SurfaceStyle(current.Background, current.Opacity, thickness, color);
        Rebuild(kind);
        NotifyThemeChanged();
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
            Styles[kind] = new SurfaceStyle(style.Background, opacity, style.BorderThickness, style.BorderColor);
            Rebuild(kind);
        }
        NotifyThemeChanged();
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
                Materials["WidgetHoverMaterial"] = WidgetStateBrush(ResolveWidgetHoverSource(), style);
                Materials["WidgetPressedMaterial"] = WidgetStateBrush(ResolveWidgetPressedSource(), style);
                break;
            case SurfaceKind.Settings:
                var oledBlack = IsTrueBlack(style);
                Materials["SettingsGlassMaterial"] = Fill(ResolveBrush("SettingsBackground"), style);
                Materials["SettingsStrokeMaterial"] = Stroke(ResolveBrush("MenuStroke"), style);
                Materials["SettingsSidebarGlass"] = style.Background == SurfaceBackground.None
                    ? Brushes.Transparent
                    : oledBlack
                        ? TrueBlackBrush
                        : style.Background == SurfaceBackground.Solid
                            ? WithAlpha(ResolveBrush("SettingsSidebarOverlay"), 255)
                            // Scale sidebar tint with SettingsOpacity so Glass 0% is truly transparent acrylic.
                            : WithAlpha(ResolveBrush("SettingsSidebarOverlay"), (byte)Math.Round(style.Opacity * 255));
                // SettingsFrame already paints SettingsGlassMaterial behind the entire window, so keep the
                // content pane transparent in Glass/None to avoid double-stacking or blocking 0% Glass / None.
                Materials["SettingsContentGlass"] = oledBlack
                    ? TrueBlackBrush
                    : style.Background == SurfaceBackground.Solid
                        ? WithAlpha(ResolveBrush("SettingsBackground"), 255)
                        : Brushes.Transparent;
                break;
        }

        // Per-surface border thickness + optional user color. Applied after the themed stroke above so a user
        // override wins; when unset, the themed brush stays. DynamicResource consumers update live.
        var prefix = StrokePrefix(kind);
        var thickness = Math.Clamp(double.IsFinite(style.BorderThickness) ? style.BorderThickness : 1.0, 0.0, 12.0);
        Materials[prefix + "Thickness"] = new Thickness(thickness);
        if (style.Background == SurfaceBackground.None || IsTrueBlack(style))
        {
            Materials[prefix + "Material"] = Brushes.Transparent;
        }
        else if (!string.IsNullOrWhiteSpace(style.BorderColor) && TryParseColor(style.BorderColor, out var parsed))
        {
            var userBrush = new SolidColorBrush(parsed);
            userBrush.Freeze();
            Materials[prefix + "Material"] = userBrush;
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
                Opacity = style.Background == SurfaceBackground.None || IsTrueBlack(style) || FlatOled ? 0 : shadowOpacity
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

    private static void NotifyThemeChanged() => ThemeChanged?.Invoke(null, EventArgs.Empty);

    private static Brush WithScaledAlpha(Brush source, double opacity)
    {
        var brush = source.Clone();
        if (brush is SolidColorBrush solid)
        {
            var c = solid.Color;
            solid.Color = Color.FromArgb((byte)Math.Clamp((int)Math.Round(c.A * opacity), 0, 255), c.R, c.G, c.B);
        }
        else if (brush is GradientBrush gradient)
        {
            foreach (var stop in gradient.GradientStops)
            {
                var c = stop.Color;
                stop.Color = Color.FromArgb((byte)Math.Clamp((int)Math.Round(c.A * opacity), 0, 255), c.R, c.G, c.B);
            }
        }
        brush.Freeze();
        return brush;
    }

    private static Brush ResolveWidgetHoverSource()
    {
        if (Overrides.Contains("WidgetCardHover") && Overrides["WidgetCardHover"] is Brush overrideCardHover)
            return overrideCardHover;
        if (Overrides.Contains("SurfaceHover") && Overrides["SurfaceHover"] is Brush overrideSurfaceHover)
            return overrideSurfaceHover;
        if (SystemOverrides.Contains("WidgetCardHover") && SystemOverrides["WidgetCardHover"] is Brush systemCardHover)
            return systemCardHover;
        return ResolveBrush("WidgetCardHover");
    }

    private static Brush ResolveWidgetPressedSource()
    {
        if (Overrides.Contains("WidgetCardPressed") && Overrides["WidgetCardPressed"] is Brush overrideCardPressed)
            return overrideCardPressed;
        if (Overrides.Contains("SurfacePressed") && Overrides["SurfacePressed"] is Brush overrideSurfacePressed)
            return overrideSurfacePressed;
        if (Overrides.Contains("WidgetCardHover") && Overrides["WidgetCardHover"] is Brush overrideCardHover)
            return WithScaledAlpha(overrideCardHover, 1.5);
        if (SystemOverrides.Contains("WidgetCardPressed") && SystemOverrides["WidgetCardPressed"] is Brush systemCardPressed)
            return systemCardPressed;
        return ResolveBrush("WidgetCardPressed");
    }

    private static Brush WidgetStateBrush(Brush source, SurfaceStyle style)
    {
        // Glass mode scales the theme token's configured alpha by WidgetOpacity; Solid and None
        // preserve the theme token's configured color and alpha at 100% so dock widgets keep clear feedback.
        var scale = style.Background == SurfaceBackground.Glass ? style.Opacity : 1.0;
        if (FlatOled) scale *= 0.5;
        return WithScaledAlpha(source, scale);
    }

    public static void SetSettingsOpacity(double opacity)
    {
        if (!double.IsFinite(opacity) || opacity is < 0.6 or > 1) throw new ArgumentOutOfRangeException(nameof(opacity));
        if (_palette is null) throw new InvalidOperationException("The theme must be initialized before its materials.");
        _settingsOpacity = opacity;
        var source = Overrides.Contains("SettingsBackground") ? Overrides["SettingsBackground"] : _palette["SettingsBackground"];
        Materials["SettingsGlassMaterial"] = WithScaledAlpha((Brush)source, opacity);
    }

    /// <summary>Parses an #AARRGGBB / #RRGGBB string into a color for user border overrides.</summary>
    internal static bool TryParseColor(string? text, out Color color)
    {
        color = default;
        if (string.IsNullOrWhiteSpace(text)) return false;
        var value = text.Trim().TrimStart('#');
        try
        {
            if (value.Length == 8)
            {
                color = Color.FromArgb(
                    Convert.ToByte(value.Substring(0, 2), 16),
                    Convert.ToByte(value.Substring(2, 2), 16),
                    Convert.ToByte(value.Substring(4, 2), 16),
                    Convert.ToByte(value.Substring(6, 2), 16));
                return true;
            }
            if (value.Length == 6)
            {
                color = Color.FromRgb(
                    Convert.ToByte(value.Substring(0, 2), 16),
                    Convert.ToByte(value.Substring(2, 2), 16),
                    Convert.ToByte(value.Substring(4, 2), 16));
                return true;
            }
        }
        catch (FormatException) { }
        catch (ArgumentOutOfRangeException) { }
        return false;
    }

    private static Brush ResolveBrush(string key) =>
        Overrides.Contains(key) ? (Brush)Overrides[key]
        : SystemOverrides.Contains(key) ? (Brush)SystemOverrides[key]
        : (Brush)_palette![key];

    private static Brush MaterialSource(string key) =>
        MaterialSources.TryGetValue(key, out var custom) ? custom
        : SystemMaterialSources.TryGetValue(key, out var system) ? system
        : (Brush)_palette![key];

    private static Brush Fill(Brush source, SurfaceStyle style)
    {
        if (style.Background == SurfaceBackground.None) return Brushes.Transparent;
        if (IsTrueBlack(style)) return TrueBlackBrush;
        return style.Background == SurfaceBackground.Solid
            ? WithAlpha(source, 255)
            : WithAlpha(source, (byte)Math.Round(style.Opacity * 255));
    }

    private static Brush Stroke(Brush source, SurfaceStyle style)
    {
        if (style.Background == SurfaceBackground.None || IsTrueBlack(style)) return Brushes.Transparent;
        var brush = source.Clone();
        brush.Freeze();
        return brush;
    }

    private static Brush Sheen(Brush source, SurfaceStyle style)
    {
        if (style.Background == SurfaceBackground.None || IsTrueBlack(style) || FlatOled) return Brushes.Transparent;
        var brush = source.Clone();
        brush.Freeze();
        return brush;
    }

    private static Effect Shadow(string key, SurfaceStyle style)
    {
        var source = Application.Current?.Resources[key] as DropShadowEffect;
        if (source is null) return new DropShadowEffect { Opacity = 0 };
        var effect = (DropShadowEffect)source.Clone();
        if (style.Background == SurfaceBackground.None || IsTrueBlack(style) || FlatOled) effect.Opacity = 0;
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
