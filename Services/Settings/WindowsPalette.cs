using System.Windows;
using System.Windows.Media;
using Microsoft.Win32;
using OpenTaskbar.Models;

namespace OpenTaskbar.Services;

/// <summary>
/// Reads the user's Windows personalization colors (Settings &gt; Personalization &gt; Colors) and
/// translates them into the brushes the design system uses. The result is applied as an override
/// layer underneath theme packs, so an explicitly selected pack still wins for the keys it defines.
/// </summary>
public static class WindowsPalette
{
    private const string AccentKey = @"Software\Microsoft\Windows\CurrentVersion\Explorer\Accent";
    private const string DwmKey = @"Software\Microsoft\Windows\DWM";

    private static readonly Dictionary<string, object> None = new(StringComparer.Ordinal);

    /// <summary>Brushes derived from Windows. Empty when Windows exposes no usable accent color.</summary>
    public static IReadOnlyDictionary<string, object> Current { get; private set; } = None;

    /// <summary>Re-reads Windows colors for the given theme. Returns false when no accent was found.</summary>
    public static bool Refresh(AppTheme theme)
    {
        var values = Read(theme);
        Current = values ?? None;
        return values is not null;
    }

    private static Dictionary<string, object>? Read(AppTheme theme)
    {
        Color accent;
        Color light1, light2, light3, dark1, dark2, dark3;

        using (var accentKey = Registry.CurrentUser.OpenSubKey(AccentKey))
        using (var dwmKey = Registry.CurrentUser.OpenSubKey(DwmKey))
        {
            // AccentPalette holds eight RGBA stops: light3, light2, light1, accent, dark1, dark2, dark3, dark4.
            if (accentKey?.GetValue("AccentPalette") is byte[] palette && palette.Length >= 28)
            {
                light3 = FromRgba(palette, 0);
                light2 = FromRgba(palette, 4);
                light1 = FromRgba(palette, 8);
                accent = FromRgba(palette, 12);
                dark1 = FromRgba(palette, 16);
                dark2 = FromRgba(palette, 20);
                dark3 = FromRgba(palette, 24);
            }
            else if (dwmKey?.GetValue("AccentColor") is int abgr)
            {
                accent = FromAbgr(abgr);
                light1 = Mix(accent, Colors.White, 0.35);
                light2 = Mix(accent, Colors.White, 0.55);
                light3 = Mix(accent, Colors.White, 0.75);
                dark1 = Mix(accent, Colors.Black, 0.25);
                dark2 = Mix(accent, Colors.Black, 0.45);
                dark3 = Mix(accent, Colors.Black, 0.65);
            }
            else if (dwmKey?.GetValue("ColorizationColor") is int argb)
            {
                accent = FromArgb(argb);
                light1 = Mix(accent, Colors.White, 0.35);
                light2 = Mix(accent, Colors.White, 0.55);
                light3 = Mix(accent, Colors.White, 0.75);
                dark1 = Mix(accent, Colors.Black, 0.25);
                dark2 = Mix(accent, Colors.Black, 0.45);
                dark3 = Mix(accent, Colors.Black, 0.65);
            }
            else
            {
                return null;
            }
        }

        var dark = theme == AppTheme.Dark;
        var values = new Dictionary<string, object>(StringComparer.Ordinal);
        var foreground = ContrastForeground(accent);

        void Solid(string key, Color color, byte alpha = 255)
        {
            var brush = new SolidColorBrush(Color.FromArgb(alpha, color.R, color.G, color.B));
            brush.Freeze();
            values[key] = brush;
        }

        void Gradient(string key, Color start, Color end)
        {
            var brush = new LinearGradientBrush(new GradientStopCollection
            {
                new GradientStop(start, 0),
                new GradientStop(end, 1)
            })
            {
                StartPoint = new Point(0, 0),
                EndPoint = new Point(0, 1)
            };
            brush.Freeze();
            values[key] = brush;
        }

        // The accent color is reserved for interactive controls (primary buttons, switches,
        // focus rings, selection marks and meter fills). Chrome surfaces stay neutral, like
        // Windows Settings itself, so an aggressive accent never floods the page.
        Solid("AccentBrush", accent);
        Solid("AccentText", foreground);
        // Darker accent stop for switch tracks, so a white thumb stays legible (as in Windows Settings).
        Solid("AccentTrack", dark2);
        Solid("StrokeFocus", accent);
        Solid("IndicatorBrush", accent, 0x90);
        Solid("IndicatorForegroundBrush", foreground);
        Solid("WidgetAccent", accent);
        Solid("WidgetAccentText", foreground);
        Gradient("WidgetAccentFill", accent, light1);
        Gradient("WindowsLogoBrush", light1, accent);
        Solid("SurfaceKeyboardFocus", dark ? dark2 : light3);

        // Neutral surface ramp modeled on the Windows Settings app: a tinted dark backdrop,
        // a distinct navigation column, and slightly elevated cards. Independent of accent.
        var page = Hex(dark ? "#1B2029" : "#F3F3F3");
        var sidebar = Hex(dark ? "#171C26" : "#EEEEEE");
        var card = Hex(dark ? "#252B37" : "#FFFFFF");
        var hover = Hex(dark ? "#2F3745" : "#F5F5F5");
        var track = Hex(dark ? "#141922" : "#E9E9E9");
        var border = Hex(dark ? "#3A4352" : "#E5E5E5");
        var divider = Hex(dark ? "#323A48" : "#E8E8E8");

        Solid("SettingsBackground", page);
        Solid("SettingsControl", card);
        // Hover is a translucent overlay, not a solid fill, so it adapts to whatever surface sits beneath.
        Solid("SettingsControlHover", dark ? Colors.White : Colors.Black, dark ? (byte)0x14 : (byte)0x0D);
        Solid("SettingsTrack", track);
        Solid("SettingsSidebarOverlay", sidebar);
        Solid("SurfacePopup", card, 0xF7);
        Solid("StrokeSubtle", border);
        Solid("MenuDivider", divider);
        Solid("SurfaceHover", dark ? Colors.White : Colors.Black, dark ? (byte)0x14 : (byte)0x0D);
        Solid("SurfacePressed", dark ? Colors.White : Colors.Black, dark ? (byte)0x22 : (byte)0x16);
        Solid("SurfaceSelected", dark ? Colors.White : Colors.Black, dark ? (byte)0x14 : (byte)0x0D);
        Solid("SurfaceSelectedStrong", dark ? Colors.White : Colors.Black, dark ? (byte)0x22 : (byte)0x16);
        Solid("ShellButtonHover", dark ? Colors.White : Colors.Black, dark ? (byte)0x22 : (byte)0x14);
        Solid("ShellButtonPressed", dark ? Colors.White : Colors.Black, dark ? (byte)0x33 : (byte)0x22);
        values["SurfaceHoverColor"] = Color.FromArgb(dark ? (byte)0x14 : (byte)0x0D, hover.R, hover.G, hover.B);

        Solid("TextPrimary", dark ? Color.FromRgb(243, 244, 246) : Color.FromRgb(27, 27, 27));
        Solid("TextSecondary", dark ? Color.FromRgb(170, 178, 192) : Color.FromRgb(97, 102, 110));
        Solid("TextMuted", Color.FromRgb(124, 134, 152), 0x80);

        // Glass sources: neutral tones; ThemeManager applies each surface's own opacity on top.
        Gradient("DockMaterial", dark ? Hex("#2B313E") : Colors.White, page);
        Gradient("MenuMaterial", dark ? Hex("#2A303C") : Colors.White, dark ? Hex("#1F2530") : Hex("#F7F7F7"));

        return values;
    }

    private static Color Hex(string value) => (Color)ColorConverter.ConvertFromString(value);

    private static Color FromRgba(byte[] bytes, int offset) =>
        Color.FromRgb(bytes[offset], bytes[offset + 1], bytes[offset + 2]);

    private static Color FromAbgr(int value) => Color.FromArgb(
        (byte)(value >> 24), (byte)value, (byte)(value >> 8), (byte)(value >> 16));

    private static Color FromArgb(int value) => Color.FromArgb(
        (byte)(value >> 24), (byte)(value >> 16), (byte)(value >> 8), (byte)value);

    private static Color Mix(Color first, Color second, double amount)
    {
        amount = Math.Clamp(amount, 0, 1);
        return Color.FromRgb(
            (byte)Math.Round(first.R + (second.R - first.R) * amount),
            (byte)Math.Round(first.G + (second.G - first.G) * amount),
            (byte)Math.Round(first.B + (second.B - first.B) * amount));
    }

    private static Color ContrastForeground(Color background) =>
        Luminance(background) < 0.18 ? Color.FromRgb(255, 255, 255) : Color.FromRgb(5, 7, 11);

    private static double Luminance(Color color)
    {
        static double Channel(byte value)
        {
            var srgb = value / 255d;
            return srgb <= 0.04045 ? srgb / 12.92 : Math.Pow((srgb + 0.055) / 1.055, 2.4);
        }

        return 0.2126 * Channel(color.R) + 0.7152 * Channel(color.G) + 0.0722 * Channel(color.B);
    }
}
