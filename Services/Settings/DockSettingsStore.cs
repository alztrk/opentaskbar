using System.Globalization;
using System.IO;
using System.Text.Json;
using System.Text.Json.Serialization;
using OpenTaskbar.Models;

namespace OpenTaskbar.Services;

public sealed class DockSettingsStore
{
    private const string RemovedMemoryWidgetId = "Memory";
    private static readonly JsonSerializerOptions JsonOptions = CreateJsonOptions();

    private readonly string _path;
    private readonly AppLanguage _defaultLanguage;

    public DockSettingsStore(string? path = null, CultureInfo? defaultCulture = null)
    {
        _defaultLanguage = (defaultCulture ?? CultureInfo.CurrentUICulture).TwoLetterISOLanguageName switch
        {
            "tr" => AppLanguage.Turkish,
            "ja" => AppLanguage.Japanese,
            _ => AppLanguage.English
        };
        _path = path ?? Path.Combine(
            Environment.GetFolderPath(Environment.SpecialFolder.LocalApplicationData),
            "OpenTaskbar",
            "settings.json");
    }

    public (DockSettings Settings, string? Error) Load()
    {
        if (!File.Exists(_path))
        {
            return (CreateDefaultSettings(), null);
        }

        try
        {
            using var document = JsonDocument.Parse(File.ReadAllText(_path));
            var settings = document.RootElement.Deserialize<DockSettings>(JsonOptions);
            // Older files without a language follow the same system-language rule as a first launch.
            if (settings is not null && !document.RootElement.TryGetProperty(nameof(DockSettings.Language), out _))
                settings = settings with { Language = _defaultLanguage };
            // Normalize the legacy enabled-only format before enforcing the installed subset invariant.
            if (settings is { InstalledWidgets: null })
                settings = settings with { InstalledWidgets = settings.Widgets ?? Array.Empty<string>() };
            // The standalone memory widget was removed in favor of System Meters; drop stale identifiers.
            if (settings is not null)
                settings = settings with
                {
                    Widgets = settings.Widgets?.Where(id => id != RemovedMemoryWidgetId).ToArray(),
                    InstalledWidgets = settings.InstalledWidgets?.Where(id => id != RemovedMemoryWidgetId).ToArray()
                };
            if (settings is null || !IsValid(settings))
            {
                return (CreateDefaultSettings(), UiText.Get("ErrorDockSettingsInvalid"));
            }

            return (settings, null);
        }
        catch (JsonException)
        {
            return (CreateDefaultSettings(), UiText.Get("ErrorDockSettingsRead"));
        }
        catch (IOException)
        {
            return (CreateDefaultSettings(), UiText.Get("ErrorDockSettingsDisk"));
        }
        catch (UnauthorizedAccessException)
        {
            return (CreateDefaultSettings(), UiText.Get("ErrorDockSettingsAccess"));
        }
    }

    private DockSettings CreateDefaultSettings() => new(Language: _defaultLanguage);

    public string? Save(DockSettings settings)
    {
        if (!IsValid(settings))
            return UiText.Get("ErrorSettingsValuesInvalid");
        try
        {
            var directory = Path.GetDirectoryName(_path);
            if (directory is null)
            {
                return UiText.Get("ErrorDockSettingsPath");
            }

            Directory.CreateDirectory(directory);
            var temporaryPath = _path + ".tmp";
            File.WriteAllText(temporaryPath, JsonSerializer.Serialize(settings, JsonOptions));
            File.Move(temporaryPath, _path, overwrite: true);
            return null;
        }
        catch (IOException)
        {
            return UiText.Get("ErrorDockSettingsSave");
        }
        catch (UnauthorizedAccessException)
        {
            return UiText.Get("ErrorDockSettingsWriteAccess");
        }
    }

    private static bool IsValid(DockSettings settings) => Enum.IsDefined(settings.Position)
        && Enum.IsDefined(settings.Theme) && Enum.IsDefined(settings.Language)
        && Enum.IsDefined(settings.AutoHide) && Enum.IsDefined(settings.Layout) && Enum.IsDefined(settings.Size)
        // Keep these ranges identical to the Widgets tab sliders.
        && settings.FocusMinutes is >= 1 and <= 90
        && settings.FocusBreakMinutes is >= 1 and <= 30
        && settings.FocusSessions is >= 1 and <= 8
        && double.IsFinite(settings.SurfaceOpacity) && settings.SurfaceOpacity is >= 0.6 and <= 1
        && double.IsFinite(settings.DockScale) && settings.DockScale is >= 0.75 and <= 1.5
        && settings.PinOrder?.Any(string.IsNullOrWhiteSpace) != true
        && settings.PinOrder?.Distinct(StringComparer.OrdinalIgnoreCase).Count() == settings.PinOrder?.Count
        && settings.Widgets?.Any(widget => !DockWidgetService.SupportedIds.Contains(widget)) != true
        && settings.Widgets?.Distinct(StringComparer.Ordinal).Count() == settings.Widgets?.Count
        && settings.InstalledWidgets?.Any(widget => !DockWidgetService.SupportedIds.Contains(widget)) != true
        && settings.InstalledWidgets?.Distinct(StringComparer.Ordinal).Count() == settings.InstalledWidgets?.Count
        && settings.Widgets?.Any(widget => settings.InstalledWidgets?.Contains(widget, StringComparer.Ordinal) != true) != true
        && Enum.IsDefined(settings.LauncherHotkey) && Enum.IsDefined(settings.DockHotkey)
        && Enum.IsDefined(settings.StartAction) && Enum.IsDefined(settings.StartPlacement) && Enum.IsDefined(settings.Edge)
        && Enum.IsDefined(settings.DateStyle)
        && (settings.StatsMeters & ~SystemMeters.All) == 0
        && settings.WeatherCity is { Length: <= 80 }
        && settings.StackFolders?.Any(string.IsNullOrWhiteSpace) != true
        && settings.WidgetOrder?.Any(string.IsNullOrWhiteSpace) != true
        && settings.WidgetOrder?.Distinct(StringComparer.OrdinalIgnoreCase).Count() == settings.WidgetOrder?.Count
        && settings.Profiles?.Any(profile => string.IsNullOrWhiteSpace(profile.Name) || profile.HiddenApps is null) != true
        && settings.Profiles?.Select(profile => profile.Name).Distinct(StringComparer.CurrentCultureIgnoreCase).Count() == settings.Profiles?.Count;

    public static bool Validate(DockSettings settings) => IsValid(settings);

    public static JsonSerializerOptions SerializerOptions => JsonOptions;

    private static JsonSerializerOptions CreateJsonOptions()
    {
        var options = new JsonSerializerOptions { WriteIndented = true };
        options.Converters.Add(new JsonStringEnumConverter());
        return options;
    }
}
