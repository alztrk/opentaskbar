using System.Globalization;
using System.IO;
using System.Text.Json;
using System.Windows;
using System.Windows.Media;
using OpenTaskbar.Models;

namespace OpenTaskbar.Services;

/// <summary>
/// <c>widgets\pack.json</c> of a theme: design token overrides. <see cref="Tokens"/> apply to both themes;
/// <see cref="Dark"/> and <see cref="Light"/> apply on top for that theme only.
/// </summary>
public sealed record WidgetPackManifest(
    string? Name = null,
    IReadOnlyDictionary<string, JsonElement>? Tokens = null,
    IReadOnlyDictionary<string, JsonElement>? Dark = null,
    IReadOnlyDictionary<string, JsonElement>? Light = null);

/// <summary>
/// Restyles the dock and its widgets by replacing design tokens (the resource keys in <c>Design\</c>).
/// A value's type follows the token it replaces: brushes take <c>"#AARRGGBB"</c> or
/// <c>{ "stops": ["#..", "#.."], "direction": "vertical" }</c>, corner radii and thicknesses take a number or
/// <c>"left,top,right,bottom"</c>, sizes take a number and fonts take a family name.
/// </summary>
public sealed class WidgetPack
{
    public const string ManifestFileName = "pack.json";
    private const int MaxManifestBytes = 128 * 1024;
    private const int MaxGradientStops = 16;
    private const double MaxDimension = 2048;

    private static readonly JsonSerializerOptions JsonOptions = new()
    {
        PropertyNameCaseInsensitive = true,
        ReadCommentHandling = JsonCommentHandling.Skip,
        AllowTrailingCommas = true
    };

    private WidgetPack(WidgetPackManifest manifest) => Manifest = manifest;

    public WidgetPackManifest Manifest { get; }

    public static (WidgetPack? Pack, string? Error) Load(string directory)
    {
        var manifestPath = Path.Combine(directory, ManifestFileName);
        try
        {
            if (!File.Exists(manifestPath)) return (null, $"{manifestPath} not found.");
            if (new FileInfo(manifestPath).Length > MaxManifestBytes)
                return (null, $"{manifestPath} exceeds the {MaxManifestBytes}-byte limit.");
            var manifest = JsonSerializer.Deserialize<WidgetPackManifest>(File.ReadAllText(manifestPath), JsonOptions);
            return manifest is null ? (null, $"{manifestPath} is empty.") : (new WidgetPack(manifest), null);
        }
        catch (Exception ex) when (ex is IOException or UnauthorizedAccessException or JsonException or NotSupportedException)
        {
            return (null, $"{manifestPath}: {ex.Message}");
        }
    }

    /// <summary>
    /// Typed values for <paramref name="theme"/>. <paramref name="current"/> returns the value a token has now, which
    /// decides how its replacement is read; unknown tokens and unreadable values are reported in <paramref name="problems"/>.
    /// </summary>
    public IReadOnlyDictionary<string, object> Resolve(AppTheme theme, Func<string, object?> current, ICollection<string> problems)
    {
        var raw = new Dictionary<string, JsonElement>(StringComparer.Ordinal);
        foreach (var source in new[] { Manifest.Tokens, theme == AppTheme.Dark ? Manifest.Dark : Manifest.Light })
        {
            foreach (var (key, value) in source ?? new Dictionary<string, JsonElement>()) raw[key] = value;
        }

        var resolved = new Dictionary<string, object>(StringComparer.Ordinal);
        if (raw.Count > 256)
        {
            problems.Add("Widget theme contains too many token overrides.");
            return resolved;
        }
        foreach (var (key, value) in raw)
        {
            var existing = current(key);
            if (existing is null)
            {
                problems.Add($"Unknown token '{key}'.");
                continue;
            }

            var converted = Convert(value, existing);
            if (converted is null) problems.Add($"Token '{key}' expects a {existing.GetType().Name} value.");
            else resolved[key] = converted;
        }

        return resolved;
    }

    internal static object? Convert(JsonElement value, object existing)
    {
        try
        {
            return existing switch
            {
                Brush => ParseBrush(value),
                Color => value.ValueKind == JsonValueKind.String ? ParseColor(value.GetString()) : null,
                CornerRadius => ParseNumbers(value) is { } n ? new CornerRadius(n[0], n[1], n[2], n[3]) : null,
                Thickness => ParseNumbers(value) is { } t ? new Thickness(t[0], t[1], t[2], t[3]) : null,
                double => value.ValueKind == JsonValueKind.Number && value.GetDouble() is var d && double.IsFinite(d) && d >= 0 && d <= MaxDimension ? d : null,
                FontFamily => value.ValueKind == JsonValueKind.String && value.GetString() is { Length: <= 128 } && !string.IsNullOrWhiteSpace(value.GetString())
                    ? new FontFamily(value.GetString()!) : null,
                string currentMode => value.ValueKind == JsonValueKind.String
                    && currentMode is "Glass" or "Solid" or "None"
                    && value.GetString() is "Glass" or "Solid" or "None"
                        ? value.GetString() : null,
                _ => null
            };
        }
        catch (Exception ex) when (ex is FormatException or InvalidOperationException or ArgumentException)
        {
            return null;
        }
    }

    private static Brush? ParseBrush(JsonElement value)
    {
        if (value.ValueKind == JsonValueKind.String)
        {
            var color = ParseColor(value.GetString());
            if (color is null) return null;
            var solid = new SolidColorBrush(color.Value);
            solid.Freeze();
            return solid;
        }

        if (value.ValueKind != JsonValueKind.Object || !value.TryGetProperty("stops", out var stops)
            || stops.ValueKind != JsonValueKind.Array || stops.GetArrayLength() is < 2 or > MaxGradientStops) return null;
        var colors = new List<Color>();
        foreach (var stop in stops.EnumerateArray())
        {
            if (stop.ValueKind != JsonValueKind.String || ParseColor(stop.GetString()) is not { } c) return null;
            colors.Add(c);
        }

        var direction = value.TryGetProperty("direction", out var d) && d.ValueKind == JsonValueKind.String ? d.GetString() : "vertical";
        var end = direction?.ToLowerInvariant() switch
        {
            "horizontal" => new Point(1, 0),
            "diagonal" => new Point(1, 1),
            _ => new Point(0, 1)
        };
        var start = new Point(0, 0);
        if (value.TryGetProperty("angle", out var angleValue) && angleValue.ValueKind == JsonValueKind.Number)
        {
            var angle = angleValue.GetDouble();
            if (!double.IsFinite(angle) || angle is < 0 or > 360) return null;
            var radians = angle * Math.PI / 180;
            start = new Point(.5 - Math.Sin(radians) / 2, .5 + Math.Cos(radians) / 2);
            end = new Point(.5 + Math.Sin(radians) / 2, .5 - Math.Cos(radians) / 2);
        }

        var positions = new double[colors.Count];
        if (value.TryGetProperty("positions", out var pos) && pos.ValueKind == JsonValueKind.Array)
        {
            if (pos.GetArrayLength() != colors.Count) return null;
            for (var i = 0; i < colors.Count; i++)
            {
                if (pos[i].ValueKind != JsonValueKind.Number) return null;
                var number = pos[i].GetDouble();
                if (!double.IsFinite(number) || number is < 0 or > 100) return null;
                positions[i] = number / 100;
            }
        }
        else
            for (var i = 0; i < colors.Count; i++)
                positions[i] = (double)i / (colors.Count - 1);

        var gradient = new LinearGradientBrush { StartPoint = start, EndPoint = end };
        for (var i = 0; i < colors.Count; i++)
            gradient.GradientStops.Add(new GradientStop(colors[i], positions[i]));
        gradient.Freeze();
        return gradient;
    }

    private static Color? ParseColor(string? text)
    {
        if (string.IsNullOrWhiteSpace(text) || !text.TrimStart().StartsWith('#')) return null;
        return (Color)ColorConverter.ConvertFromString(text.Trim());
    }

    // One number for all four sides, or four comma-separated numbers.
    private static double[]? ParseNumbers(JsonElement value)
    {
        if (value.ValueKind == JsonValueKind.Number)
        {
            var n = value.GetDouble();
            return double.IsFinite(n) && n >= 0 && n <= MaxDimension ? [n, n, n, n] : null;
        }

        if (value.ValueKind != JsonValueKind.String) return null;
        var parts = value.GetString()!.Split(',', StringSplitOptions.TrimEntries);
        if (parts.Length is not (1 or 2 or 4)) return null;
        var numbers = new double[parts.Length];
        for (var i = 0; i < parts.Length; i++)
        {
            if (!double.TryParse(parts[i], NumberStyles.Float, CultureInfo.InvariantCulture, out numbers[i])
                || !double.IsFinite(numbers[i]) || numbers[i] < 0 || numbers[i] > MaxDimension) return null;
        }

        return parts.Length switch
        {
            1 => [numbers[0], numbers[0], numbers[0], numbers[0]],
            2 => [numbers[0], numbers[1], numbers[0], numbers[1]],
            _ => numbers
        };
    }
}
