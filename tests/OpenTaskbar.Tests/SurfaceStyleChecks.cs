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
                SettingsBackground: SurfaceBackground.Glass, SettingsOpacity: 0.5));

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
            Check(app.FindResource("DockMaterial") != app.FindResource("MenuMaterial"),
                "Surfaces must not share a single generated brush.");
        }
        finally
        {
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
