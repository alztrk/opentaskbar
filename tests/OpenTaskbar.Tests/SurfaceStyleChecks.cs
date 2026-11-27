using System.Windows;
using System.Windows.Media;
using System.Windows.Media.Effects;
using OpenTaskbar.Models;
using OpenTaskbar.Services;

internal static class SurfaceStyleChecks
{
    public static int Run()
    {
        var app = Application.Current!;
        var checks = 0;
        void Check(bool condition, string message)
        {
            if (!condition) throw new InvalidOperationException(message);
            checks++;
        }

        try
        {
            ThemeManager.Apply(AppTheme.Dark, new DockSettings(
                DockBackground: SurfaceBackground.None, DockOpacity: 0.2,
                MenuBackground: SurfaceBackground.Solid, MenuOpacity: 0.3,
                WidgetBackground: SurfaceBackground.Glass, WidgetOpacity: 0.4,
                SettingsBackground: SurfaceBackground.Glass, SettingsOpacity: 0.5,
                MenuBorderThickness: 2.5, MenuBorderColor: "#80FF0000"));

            Check(Alpha(app, "DockMaterial") == 0, "None must fully remove the dock fill.");
            Check(Alpha(app, "DockBorderMaterial") == 0, "None must remove the dock rim.");
            Check((app.FindResource("DockShadow") as DropShadowEffect)?.Opacity == 0,
                "None must remove the dock shadow.");
            Check(Alpha(app, "MenuMaterial") == 255, "Solid must be fully opaque regardless of its opacity slider.");
            ThemeManager.SetSurface(SurfaceKind.Menu, SurfaceBackground.None, 0.3);
            Check(Alpha(app, "MenuMaterial") == 0 && Alpha(app, "MenuSheenMaterial") == 0,
                "None must remove both the flyout fill and its sheen.");
            ThemeManager.SetSurface(SurfaceKind.Menu, SurfaceBackground.Solid, 0.3);
            Check(Alpha(app, "WidgetMaterial") == 102, "Widget glass must use only its own 40% opacity.");
            Check(Alpha(app, "SettingsGlassMaterial") == 128, "Settings glass must use only its own 50% opacity.");
            Check(Alpha(app, "SettingsSidebarGlass") == 128 && Alpha(app, "SettingsContentGlass") == 0,
                "Settings sidebar must scale with SettingsOpacity and content must stay transparent in Glass mode.");
            ThemeManager.SetSurface(SurfaceKind.Settings, SurfaceBackground.Glass, 0.0);
            Check(Alpha(app, "SettingsGlassMaterial") == 0 && Alpha(app, "SettingsSidebarGlass") == 0 && Alpha(app, "SettingsContentGlass") == 0,
                "Settings Glass at 0% opacity must remove all tint layers.");
            ThemeManager.SetSurface(SurfaceKind.Settings, SurfaceBackground.None, 0.5);
            Check(Alpha(app, "SettingsGlassMaterial") == 0 && Alpha(app, "SettingsSidebarGlass") == 0 && Alpha(app, "SettingsContentGlass") == 0,
                "Settings None must remove all background fills.");
            ThemeManager.SetSurface(SurfaceKind.Settings, SurfaceBackground.Solid, 0.5);
            Check(Alpha(app, "SettingsGlassMaterial") == 255 && Alpha(app, "SettingsSidebarGlass") == 255 && Alpha(app, "SettingsContentGlass") == 255,
                "Settings Solid must make all window background layers opaque.");
            Check(app.FindResource("DockMaterial") != app.FindResource("MenuMaterial"),
                "Surfaces must not share a single generated brush.");

            // Border thickness/color are generated per surface from settings; a user color must win over the theme.
            Check((app.FindResource("MenuStrokeThickness") as Thickness?)?.Top == 2.5,
                "Menu border thickness must be generated from the setting.");
            Check((app.FindResource("MenuStrokeMaterial") as SolidColorBrush)?.Color == Color.FromArgb(0x80, 0xFF, 0x00, 0x00),
                "User border color must override the themed stroke brush.");
            ThemeManager.SetBorder(SurfaceKind.Menu, 0, null);
            Check((app.FindResource("MenuStrokeThickness") as Thickness?)?.Top == 0,
                "Border thickness must be able to be set to zero for no border.");
            Check(ThemeManager.TryParseColor("#AARRGGBB", out _) == false, "Invalid border colors must be rejected.");
            Check(ThemeManager.TryParseColor("#11223344", out var parsed) && parsed.A == 0x11,
                "Valid #AARRGGBB border colors must parse.");

            // Widget hover & pressed materials must respect both theme token color/alpha and WidgetOpacity scaling.
            Check(Alpha(app, "WidgetHoverMaterial") == (byte)Math.Round(0x22 * 0.4),
                "WidgetHoverMaterial in Glass mode must scale Dark WidgetCardHover alpha by WidgetOpacity.");
            Check(Alpha(app, "WidgetPressedMaterial") == (byte)Math.Round(0x38 * 0.4),
                "WidgetPressedMaterial in Glass mode must scale Dark WidgetCardPressed alpha by WidgetOpacity.");

            ThemeManager.SetSurface(SurfaceKind.Widget, SurfaceBackground.Solid, 0.4);
            Check(Alpha(app, "WidgetHoverMaterial") == 0x22 && Alpha(app, "WidgetPressedMaterial") == 0x38,
                "Widget hover/pressed in Solid mode must preserve 100% of the theme token alpha.");

            ThemeManager.SetSurface(SurfaceKind.Widget, SurfaceBackground.None, 0.4);
            Check(Alpha(app, "WidgetHoverMaterial") == 0x22 && Alpha(app, "WidgetPressedMaterial") == 0x38,
                "Widget hover/pressed in None mode must keep theme token alpha so dock widgets stay interactive.");

            // Theme pack overrides (both solid and gradient) must drive WidgetHoverMaterial/WidgetPressedMaterial live.
            var customHover = new SolidColorBrush(Color.FromArgb(0x40, 0xCB, 0xA6, 0xF7));
            customHover.Freeze();
            var customPressed = new SolidColorBrush(Color.FromArgb(0x60, 0xCB, 0xA6, 0xF7));
            customPressed.Freeze();
            ThemeManager.SetSurface(SurfaceKind.Widget, SurfaceBackground.Glass, 0.5);
            ThemeManager.SetOverrides(new Dictionary<string, object>(StringComparer.Ordinal)
            {
                ["WidgetCardHover"] = customHover,
                ["WidgetCardPressed"] = customPressed
            });
            var resolvedHover = (SolidColorBrush)app.FindResource("WidgetHoverMaterial");
            var resolvedPressed = (SolidColorBrush)app.FindResource("WidgetPressedMaterial");
            Check(resolvedHover.Color == Color.FromArgb(0x20, 0xCB, 0xA6, 0xF7),
                "WidgetHoverMaterial must apply custom theme color and scale its alpha by WidgetOpacity.");
            Check(resolvedPressed.Color == Color.FromArgb(0x30, 0xCB, 0xA6, 0xF7),
                "WidgetPressedMaterial must apply custom theme color and scale its alpha by WidgetOpacity.");

            // Custom hover without explicit pressed token must derive a stronger same-hue pressed brush.
            ThemeManager.SetSurface(SurfaceKind.Widget, SurfaceBackground.Solid, 1.0);
            ThemeManager.SetOverrides(new Dictionary<string, object>(StringComparer.Ordinal)
            {
                ["WidgetCardHover"] = customHover
            });
            var derivedPressed = (SolidColorBrush)app.FindResource("WidgetPressedMaterial");
            Check(derivedPressed.Color == Color.FromArgb(0x60, 0xCB, 0xA6, 0xF7),
                "Custom WidgetCardHover without WidgetCardPressed must derive a stronger same-color pressed brush.");

            // Gradient theme overrides must preserve gradient stops and scale each stop's alpha.
            var gradientHover = new LinearGradientBrush(
                Color.FromArgb(0x50, 0xAA, 0x55, 0xFF),
                Color.FromArgb(0x30, 0x38, 0xBD, 0xF8),
                new Point(0, 0), new Point(1, 0));
            gradientHover.Freeze();
            ThemeManager.SetSurface(SurfaceKind.Widget, SurfaceBackground.Glass, 0.5);
            ThemeManager.SetOverrides(new Dictionary<string, object>(StringComparer.Ordinal)
            {
                ["WidgetCardHover"] = gradientHover
            });
            var resolvedGrad = app.FindResource("WidgetHoverMaterial") as LinearGradientBrush;
            Check(resolvedGrad is not null && resolvedGrad.GradientStops.Count == 2
                && resolvedGrad.GradientStops[0].Color == Color.FromArgb(0x28, 0xAA, 0x55, 0xFF)
                && resolvedGrad.GradientStops[1].Color == Color.FromArgb(0x18, 0x38, 0xBD, 0xF8),
                "Gradient WidgetCardHover must scale every stop's alpha by WidgetOpacity.");

            // WindowsPalette must expose visible WidgetCardHover/WidgetCardPressed and matching SurfaceHoverColor.
            ThemeManager.SetOverrides(new Dictionary<string, object>(StringComparer.Ordinal));
            ThemeManager.Apply(AppTheme.Light, new DockSettings(WidgetBackground: SurfaceBackground.Solid, WidgetOpacity: 1.0));
            Check(Alpha(app, "WidgetHoverMaterial") == 0x18 && Alpha(app, "WidgetPressedMaterial") == 0x2A,
                "Light palette must provide distinct WidgetHoverMaterial and WidgetPressedMaterial.");

            // TrueBlack is an opaque pure-black fill with no rim, sheen or shadow on any surface.
            ThemeManager.Apply(AppTheme.Dark, new DockSettings(
                DockBackground: SurfaceBackground.TrueBlack,
                MenuBackground: SurfaceBackground.TrueBlack,
                WidgetBackground: SurfaceBackground.TrueBlack,
                SettingsBackground: SurfaceBackground.TrueBlack,
                DockBorderColor: "#FFFFFFFF"));
            Check((app.FindResource("DockMaterial") as SolidColorBrush)?.Color == Colors.Black,
                "TrueBlack dock fill must be pure black.");
            Check(Alpha(app, "DockBorderMaterial") == 0 && Alpha(app, "MenuSheenMaterial") == 0,
                "TrueBlack must remove the rim and sheen even with a user border color.");
            Check((app.FindResource("DockShadow") as DropShadowEffect)?.Opacity == 0,
                "TrueBlack must remove the dock shadow.");
            Check((app.FindResource("SettingsSidebarGlass") as SolidColorBrush)?.Color == Colors.Black
                && (app.FindResource("SettingsContentGlass") as SolidColorBrush)?.Color == Colors.Black,
                "TrueBlack Settings must paint both background layers pure black.");

            // Forced display care (low battery / Night Light) applies the same look without touching saved styles.
            ThemeManager.Apply(AppTheme.Dark, new DockSettings(DockBackground: SurfaceBackground.Glass));
            ThemeManager.SetForcedDisplayCare(true);
            Check((app.FindResource("DockMaterial") as SolidColorBrush)?.Color == Colors.Black,
                "Forced display care must temporarily force a pure-black dock fill.");
            Check(ThemeManager.GetSurface(SurfaceKind.Dock).Background == SurfaceBackground.Glass,
                "Forced display care must not rewrite the saved surface style.");
            ThemeManager.SetForcedDisplayCare(false);
            Check((app.FindResource("DockMaterial") is not SolidColorBrush forced
                    || forced.Color != Colors.Black),
                "Clearing forced display care must restore the saved surface style.");

            // Reduce-glow flattens gradient glow tokens while keeping the accent hue.
            ThemeManager.SetReduceGlow(true);
            Check(app.FindResource("WidgetAccentFill") is SolidColorBrush,
                "Reduce glow must flatten WidgetAccentFill to a solid brush.");
            Check(Alpha(app, "IconHoverGlow") == 0 && Alpha(app, "DockHighlight") == 0,
                "Reduce glow must remove the radial hover glow and dock highlight.");
            ThemeManager.SetReduceGlow(false);
            Check(app.FindResource("WidgetAccentFill") is GradientBrush,
                "Turning reduce-glow off must restore the themed gradient.");
        }
        finally
        {
            ThemeManager.SetOverrides(new Dictionary<string, object>(StringComparer.Ordinal));
            ThemeManager.Apply(AppTheme.Dark);
        }

        return checks;
    }

    private static byte Alpha(Application app, string key) => app.FindResource(key) switch
    {
        SolidColorBrush solid => solid.Color.A,
        GradientBrush gradient => gradient.GradientStops[0].Color.A,
        _ => byte.MaxValue
    };
}
