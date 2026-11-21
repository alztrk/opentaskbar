using System.Globalization;
using System.IO;
using System.Text.Json;
using System.Text.Json.Serialization;
using OpenTaskbar.Models;

namespace OpenTaskbar.Services;

public sealed class DockSettingsStore
{
    private const string RemovedMemoryWidgetId = "Memory";
    private const string RemovedSearchWidgetId = "Search";
    private const string RemovedDesktopsWidgetId = "Desktops";
    private const string RemovedTrayWidgetId = "Tray";
    private const string LegacyAiUsageWidgetId = "AiUsage";
    private static readonly string[] RemovedIds =
        [RemovedMemoryWidgetId, RemovedSearchWidgetId, RemovedDesktopsWidgetId, RemovedTrayWidgetId];
    private static readonly JsonSerializerOptions JsonOptions = CreateJsonOptions();

    private readonly string _path;
    private readonly AppLanguage _defaultLanguage;

    public DockSettingsStore(string? path = null, CultureInfo? defaultCulture = null)
    {
        _defaultLanguage = LanguageCatalog.FromCulture(defaultCulture ?? CultureInfo.CurrentUICulture);
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
            // The standalone memory widget was removed in favor of System Meters; Search is now a dock button.
            // Drop the retired identifiers before enforcing the installed subset invariant.
            if (settings is not null)
                settings = MigrateRemovedWidgets(settings);
            if (settings is not null)
                settings = MigrateLegacyAiUsage(settings);
            if (settings is not null)
                settings = MigrateSurfaceStyles(settings, document.RootElement);
            if (settings is null || !IsValid(settings))
            {
                return (CreateDefaultSettings(), "ErrorDockSettingsInvalid");
            }

            return (settings, null);
        }
        catch (JsonException)
        {
            return (CreateDefaultSettings(), "ErrorDockSettingsRead");
        }
        catch (IOException)
        {
            return (CreateDefaultSettings(), "ErrorDockSettingsDisk");
        }
        catch (UnauthorizedAccessException)
        {
            return (CreateDefaultSettings(), "ErrorDockSettingsAccess");
        }
    }

    private DockSettings CreateDefaultSettings() => new(Language: _defaultLanguage);

    /// <summary>
    /// Drops identifiers for retired widgets from every setting that can carry one. Search stopped being a widget
    /// and became a dock button beside Start; its enabled state carries over to <see cref="DockSettings.ShowSearchButton"/>.
    /// </summary>
    public static DockSettings MigrateRemovedWidgets(DockSettings settings)
    {
        static bool Contains(IReadOnlyList<string>? ids, string value) =>
            ids?.Contains(value, StringComparer.OrdinalIgnoreCase) == true;

        static IReadOnlyList<string>? Without(IReadOnlyList<string>? ids, params string[] removed) =>
            ids?.Where(id => !removed.Any(value => string.Equals(id, value, StringComparison.OrdinalIgnoreCase))).ToArray();

        static bool IsRemoved(string key) =>
            RemovedIds.Any(value => string.Equals(key, value, StringComparison.OrdinalIgnoreCase));

        var enabledSearch = Contains(settings.Widgets, RemovedSearchWidgetId);
        var mentionedSearch = enabledSearch
            || Contains(settings.InstalledWidgets, RemovedSearchWidgetId)
            || Contains(settings.WidgetOrder, RemovedSearchWidgetId)
            || Contains(settings.DockItemOrder, "widget:" + RemovedSearchWidgetId);
        var enabledDesktops = Contains(settings.Widgets, RemovedDesktopsWidgetId);
        var mentionedDesktops = enabledDesktops
            || Contains(settings.InstalledWidgets, RemovedDesktopsWidgetId)
            || Contains(settings.WidgetOrder, RemovedDesktopsWidgetId)
            || Contains(settings.DockItemOrder, "widget:" + RemovedDesktopsWidgetId);
        var enabledTray = Contains(settings.Widgets, RemovedTrayWidgetId);
        var mentionedTray = enabledTray
            || Contains(settings.InstalledWidgets, RemovedTrayWidgetId)
            || Contains(settings.WidgetOrder, RemovedTrayWidgetId)
            || Contains(settings.DockItemOrder, "widget:" + RemovedTrayWidgetId);

        var densities = settings.WidgetDensityOverrides;
        if (densities is not null && densities.Keys.Any(IsRemoved))
        {
            densities = densities.Where(pair => !IsRemoved(pair.Key))
                .ToDictionary(pair => pair.Key, pair => pair.Value, StringComparer.OrdinalIgnoreCase);
        }
        var removedTokens = RemovedIds.Select(id => "widget:" + id).ToArray();

        return settings with
        {
            Widgets = Without(settings.Widgets, RemovedIds),
            InstalledWidgets = Without(settings.InstalledWidgets, RemovedIds),
            WidgetOrder = Without(settings.WidgetOrder, RemovedIds),
            DockItemOrder = Without(settings.DockItemOrder, removedTokens),
            WidgetDensityOverrides = densities,
            // Only override the default when the file actually mentioned the old widget.
            ShowSearchButton = mentionedSearch ? enabledSearch : settings.ShowSearchButton,
            ShowTaskViewButton = mentionedDesktops ? enabledDesktops : settings.ShowTaskViewButton,
            ShowTrayButton = mentionedTray ? enabledTray : settings.ShowTrayButton
        };
    }

    /// <summary>
    /// Splits the retired single AI usage widget into one widget per provider: the providers it had enabled stay
    /// enabled, and every provider becomes available in the catalog, order and density settings.
    /// </summary>
    public static DockSettings MigrateLegacyAiUsage(DockSettings settings)
    {
        var all = DockWidgetService.AiUsageWidgets.Select(widget => widget.Id).ToArray();
        var enabled = DockWidgetService.AiUsageWidgets
            .Where(widget => settings.AiUsageProviders.HasFlag(widget.Provider)).Select(widget => widget.Id).ToArray();

        static IReadOnlyList<string>? Replace(IReadOnlyList<string>? ids, string legacy, IEnumerable<string> replacement) =>
            ids is null || !ids.Contains(legacy, StringComparer.OrdinalIgnoreCase)
                ? ids
                : ids.SelectMany(id => string.Equals(id, legacy, StringComparison.OrdinalIgnoreCase) ? replacement : [id])
                    .Distinct(StringComparer.OrdinalIgnoreCase).ToArray();

        var densities = settings.WidgetDensityOverrides;
        if (densities is not null && densities.TryGetValue(LegacyAiUsageWidgetId, out var density))
        {
            var migrated = densities.Where(pair => !string.Equals(pair.Key, LegacyAiUsageWidgetId, StringComparison.OrdinalIgnoreCase))
                .ToDictionary(pair => pair.Key, pair => pair.Value, StringComparer.OrdinalIgnoreCase);
            foreach (var id in all) migrated[id] = density;
            densities = migrated;
        }

        return settings with
        {
            Widgets = Replace(settings.Widgets, LegacyAiUsageWidgetId, enabled),
            InstalledWidgets = Replace(settings.InstalledWidgets, LegacyAiUsageWidgetId, all),
            WidgetOrder = Replace(settings.WidgetOrder, LegacyAiUsageWidgetId, all),
            DockItemOrder = Replace(settings.DockItemOrder, "widget:" + LegacyAiUsageWidgetId, all.Select(id => "widget:" + id)),
            WidgetDensityOverrides = densities,
            AiUsageProviders = AiProviders.None
        };
    }

    /// <summary>
    /// The retired single SurfaceOpacity slider fed every glass surface. Copy its value into the four
    /// independent styles once so upgrading users keep exactly the same appearance.
    /// </summary>
    private static DockSettings MigrateSurfaceStyles(DockSettings settings, JsonElement root)
    {
        if (root.TryGetProperty(nameof(DockSettings.DockBackground), out _)
            || !root.TryGetProperty(nameof(DockSettings.SurfaceOpacity), out var legacy)
            || legacy.ValueKind != JsonValueKind.Number
            || !legacy.TryGetDouble(out var value)
            || !double.IsFinite(value) || value is < 0.6 or > 1)
            return settings;

        return settings with
        {
            DockOpacity = value,
            MenuOpacity = value,
            WidgetOpacity = value,
            SettingsOpacity = value
        };
    }

    public string? Save(DockSettings settings)
    {
        if (!IsValid(settings))
            return "ErrorSettingsValuesInvalid";
        try
        {
            var directory = Path.GetDirectoryName(_path);
            if (directory is null)
            {
                return "ErrorDockSettingsPath";
            }

            Directory.CreateDirectory(directory);
            var temporaryPath = _path + "." + Guid.NewGuid().ToString("N") + ".tmp";
            try
            {
                File.WriteAllText(temporaryPath, JsonSerializer.Serialize(settings, JsonOptions));
                File.Move(temporaryPath, _path, overwrite: true);
            }
            finally
            {
                if (File.Exists(temporaryPath)) File.Delete(temporaryPath);
            }
            return null;
        }
        catch (IOException)
        {
            return "ErrorDockSettingsSave";
        }
        catch (UnauthorizedAccessException)
        {
            return "ErrorDockSettingsWriteAccess";
        }
    }

    private static bool IsValid(DockSettings settings) => Enum.IsDefined(settings.Position)
        && Enum.IsDefined(settings.Theme) && LanguageCatalog.IsSupported(settings.Language)
        && Enum.IsDefined(settings.AutoHide) && Enum.IsDefined(settings.Layout) && Enum.IsDefined(settings.Size)
        && Enum.IsDefined(settings.DockWidthMode)
        && settings.DockWidthPixels is >= 100 and <= 5000
        && settings.DockWidthPercent is >= 10 and <= 100
        // Keep these ranges identical to the Widgets tab sliders.
        && settings.FocusMinutes is >= 1 and <= 90
        && settings.FocusBreakMinutes is >= 1 and <= 30
        && settings.FocusSessions is >= 1 and <= 8
        && double.IsFinite(settings.SurfaceOpacity) && settings.SurfaceOpacity is >= 0.6 and <= 1
        && Enum.IsDefined(settings.DockBackground) && Enum.IsDefined(settings.MenuBackground)
        && Enum.IsDefined(settings.WidgetBackground) && Enum.IsDefined(settings.SettingsBackground)
        && double.IsFinite(settings.DockOpacity) && settings.DockOpacity is >= 0 and <= 1
        && double.IsFinite(settings.MenuOpacity) && settings.MenuOpacity is >= 0 and <= 1
        && double.IsFinite(settings.WidgetOpacity) && settings.WidgetOpacity is >= 0 and <= 1
        && double.IsFinite(settings.SettingsOpacity) && settings.SettingsOpacity is >= 0 and <= 1
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
        && (settings.AiUsageProviders & ~AiProviders.All) == 0
        && Enum.IsDefined(settings.AiUsageRing)
        && settings.WeatherCity is { Length: <= 80 }
        && settings.StackFolders?.Any(string.IsNullOrWhiteSpace) != true
        && settings.WidgetOrder?.Any(string.IsNullOrWhiteSpace) != true
        && settings.WidgetOrder?.Distinct(StringComparer.OrdinalIgnoreCase).Count() == settings.WidgetOrder?.Count
        && settings.Profiles?.Any(profile => string.IsNullOrWhiteSpace(profile.Name) || profile.HiddenApps is null) != true
        && settings.Profiles?.Select(profile => profile.Name).Distinct(StringComparer.CurrentCultureIgnoreCase).Count() == settings.Profiles?.Count
        && settings.AppGroups?.Any(group => string.IsNullOrWhiteSpace(group.Id)
            || string.IsNullOrWhiteSpace(group.Name) || group.ExecutablePaths is null) != true
        && settings.AppGroups?.Select(group => group.Id).Distinct(StringComparer.OrdinalIgnoreCase).Count() == settings.AppGroups?.Count
        && settings.AppGroups?.Select(group => group.Name).Distinct(StringComparer.CurrentCultureIgnoreCase).Count() == settings.AppGroups?.Count
        && settings.AppGroups?.SelectMany(group => group.ExecutablePaths ?? [])
            .Distinct(StringComparer.OrdinalIgnoreCase).Count()
            == settings.AppGroups?.Sum(group => (group.ExecutablePaths ?? []).Count)
        && (settings.DockCornerRadius is null || settings.DockCornerRadius is >= 0 and <= 48);

    public static bool Validate(DockSettings settings) => IsValid(settings);

    public static JsonSerializerOptions SerializerOptions => JsonOptions;

    private static JsonSerializerOptions CreateJsonOptions()
    {
        var options = new JsonSerializerOptions { WriteIndented = true };
        options.Converters.Add(new AppLanguageConverter());
        options.Converters.Add(new JsonStringEnumConverter());
        return options;
    }

    private sealed class AppLanguageConverter : JsonConverter<AppLanguage>
    {
        public override AppLanguage Read(ref Utf8JsonReader reader, Type typeToConvert, JsonSerializerOptions options)
        {
            if (reader.TokenType == JsonTokenType.Number && reader.TryGetInt32(out var legacyValue))
            {
                return legacyValue switch
                {
                    0 => AppLanguage.Turkish,
                    1 => AppLanguage.English,
                    2 => AppLanguage.Japanese,
                    _ => throw new JsonException("Legacy language value is not supported.")
                };
            }
            if (reader.TokenType != JsonTokenType.String) throw new JsonException("Language must be a culture name.");
            var value = reader.GetString();
            if (string.Equals(value, "Turkish", StringComparison.OrdinalIgnoreCase)) return AppLanguage.Turkish;
            if (string.Equals(value, "English", StringComparison.OrdinalIgnoreCase)) return AppLanguage.English;
            if (string.Equals(value, "Japanese", StringComparison.OrdinalIgnoreCase)) return AppLanguage.Japanese;
            if (value is not null)
            {
                try
                {
                    var language = new AppLanguage(value);
                    if (LanguageCatalog.IsSupported(language)) return language;
                }
                catch (CultureNotFoundException)
                {
                    throw new JsonException("Language culture is invalid.");
                }
            }

            throw new JsonException("Language is not supported.");
        }

        public override void Write(Utf8JsonWriter writer, AppLanguage value, JsonSerializerOptions options) =>
            writer.WriteStringValue(value.CultureName);
    }
}
