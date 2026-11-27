using System.Globalization;
using System.IO;
using System.Text.Json;
using System.Text.Json.Nodes;
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
            var rawJson = File.ReadAllText(_path);
            var normalizedJson = NormalizeLegacyJson(rawJson);
            using var document = JsonDocument.Parse(normalizedJson);
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
            if (settings is not null)
                settings = MigrateLegacyIconPadding(settings, document.RootElement);
            if (settings is not null)
                settings = MigratePinnedTrayIcons(settings, document.RootElement);
            if (settings is not null)
                settings = settings with { StatsMeters = settings.StatsMeters & SystemMeters.All };
            if (settings is not null)
                settings = DockProfileService.EnsureDefaultProfile(settings);
            if (settings is null || !IsValid(settings))
            {
                BackupInvalidSettingsFile();
                return (CreateDefaultSettings(), "ErrorDockSettingsInvalid");
            }

            return (settings, null);
        }
        catch (Exception ex) when (ex is JsonException or NotSupportedException or ArgumentException or FormatException or NullReferenceException)
        {
            BackupInvalidSettingsFile();
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

    /// <summary>
    /// Rewrites retired v0.2.x JSON tokens before System.Text.Json deserialization so upgrading users keep
    /// their existing settings instead of failing enum parsing and falling back to factory defaults.
    /// </summary>
    private static string NormalizeLegacyJson(string rawJson)
    {
        if (JsonNode.Parse(rawJson) is not JsonObject obj)
            return rawJson;

        var changed = false;

        // v0.2.x persisted "CpuTemperature" (removed from SystemMeters) and "Battery" (removed from SystemMeters.All).
        if (obj.TryGetPropertyValue(nameof(DockSettings.StatsMeters), out var metersNode) && metersNode is JsonValue metersValue)
        {
            if (metersValue.TryGetValue<string>(out var meterString))
            {
                var kept = meterString.Split(',', StringSplitOptions.RemoveEmptyEntries | StringSplitOptions.TrimEntries)
                    .Where(token => !string.Equals(token, "CpuTemperature", StringComparison.OrdinalIgnoreCase)
                        && !string.Equals(token, "Battery", StringComparison.OrdinalIgnoreCase))
                    .ToArray();
                var replacement = kept.Length == 0 ? nameof(SystemMeters.None) : string.Join(", ", kept);
                if (!string.Equals(replacement, meterString, StringComparison.Ordinal))
                {
                    obj[nameof(DockSettings.StatsMeters)] = replacement;
                    changed = true;
                }
            }
            else if (metersValue.TryGetValue<int>(out var meterBits))
            {
                var masked = meterBits & (int)SystemMeters.All;
                if (masked != meterBits)
                {
                    obj[nameof(DockSettings.StatsMeters)] = masked;
                    changed = true;
                }
            }
        }

        // v0.2.x stored bottom gap as "DockBottomGap" (int?); carry it over to DockBottomGapPixels when absent.
        if (!obj.ContainsKey(nameof(DockSettings.DockBottomGapPixels))
            && obj.TryGetPropertyValue("DockBottomGap", out var legacyBottomNode)
            && legacyBottomNode is JsonValue legacyBottomValue
            && legacyBottomValue.TryGetValue<int>(out var legacyBottomGap)
            && legacyBottomGap is >= 0 and <= 200)
        {
            obj[nameof(DockSettings.DockBottomGapPixels)] = legacyBottomGap;
            changed = true;
        }

        return changed ? obj.ToJsonString() : rawJson;
    }

    private DockSettings CreateDefaultSettings() => DockProfileService.EnsureDefaultProfile(new(Language: _defaultLanguage));

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
        var profiles = settings.Profiles?
            .Where(profile => profile is not null)
            .Select(profile => profile with
            {
                Widgets = Without(profile.Widgets, RemovedIds),
                InstalledWidgets = Without(profile.InstalledWidgets, RemovedIds),
                WidgetOrder = Without(profile.WidgetOrder, RemovedIds),
                DockItemOrder = Without(profile.DockItemOrder, removedTokens)
            }).ToArray();

        return settings with
        {
            Widgets = Without(settings.Widgets, RemovedIds),
            InstalledWidgets = Without(settings.InstalledWidgets, RemovedIds),
            WidgetOrder = Without(settings.WidgetOrder, RemovedIds),
            DockItemOrder = Without(settings.DockItemOrder, removedTokens),
            WidgetDensityOverrides = densities,
            Profiles = profiles,
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

        var allTokens = all.Select(id => "widget:" + id).ToArray();
        var profiles = settings.Profiles?.Select(profile => profile with
        {
            Widgets = Replace(profile.Widgets, LegacyAiUsageWidgetId, enabled),
            InstalledWidgets = Replace(profile.InstalledWidgets, LegacyAiUsageWidgetId, all),
            WidgetOrder = Replace(profile.WidgetOrder, LegacyAiUsageWidgetId, all),
            DockItemOrder = Replace(profile.DockItemOrder, "widget:" + LegacyAiUsageWidgetId, allTokens)
        }).ToArray();

        return settings with
        {
            Widgets = Replace(settings.Widgets, LegacyAiUsageWidgetId, enabled),
            InstalledWidgets = Replace(settings.InstalledWidgets, LegacyAiUsageWidgetId, all),
            WidgetOrder = Replace(settings.WidgetOrder, LegacyAiUsageWidgetId, all),
            DockItemOrder = Replace(settings.DockItemOrder, "widget:" + LegacyAiUsageWidgetId, allTokens),
            WidgetDensityOverrides = densities,
            Profiles = profiles,
            AiUsageProviders = AiProviders.None
        };
    }

    /// <summary>
    /// DockIconPaddingX/Y existed briefly; collapse them back to the single horizontal-only
    /// DockIconPadding so settings written in that window keep their spacing.
    /// </summary>
    private static DockSettings MigrateLegacyIconPadding(DockSettings settings, JsonElement root)
    {
        if (!root.TryGetProperty("DockIconPaddingX", out var legacy)
            || legacy.ValueKind != JsonValueKind.Number
            || !legacy.TryGetInt32(out var value)) return settings;
        return settings with { DockIconPadding = Math.Clamp(value, 0, (int)DockMetrics.MaxIconPadding) };
    }

    /// <summary>
    /// Keeps <see cref="DockSettings.PinnedTrayIcons"/> a plain string list for backward compatibility while
    /// parsing legacy <c>guid:</c>/<c>path:</c> keys and converting future structured JSON pin objects into their
    /// canonical legacy key. Invalid entries are dropped instead of invalidating the whole settings file.
    /// </summary>
    public static DockSettings MigratePinnedTrayIcons(DockSettings settings, JsonElement root)
    {
        if (!root.TryGetProperty(nameof(DockSettings.PinnedTrayIcons), out var array)
            || array.ValueKind != JsonValueKind.Array)
        {
            return settings;
        }

        var migrated = new List<string>();
        var seen = new HashSet<string>(StringComparer.OrdinalIgnoreCase);
        foreach (var element in array.EnumerateArray())
        {
            string? key = null;
            if (element.ValueKind == JsonValueKind.String)
            {
                key = element.GetString()?.Trim();
                if (string.IsNullOrWhiteSpace(key) || key.Length > TrayPinIdentity.MaxKeyLength) continue;
            }
            else if (element.ValueKind == JsonValueKind.Object
                && TrayIconRef.TryParse(element.GetRawText(), out var reference)
                && reference.IsPinnable)
            {
                key = reference.Key;
            }

            if (key is null || !seen.Add(key) || migrated.Count >= TrayPinIdentity.MaxPinnedIcons) continue;
            migrated.Add(key);
        }

        if (migrated.Count == 0)
        {
            return settings.PinnedTrayIcons is null ? settings : settings with { PinnedTrayIcons = null };
        }
        if (settings.PinnedTrayIcons is { Count: > 0 }
            && settings.PinnedTrayIcons.SequenceEqual(migrated, StringComparer.Ordinal))
        {
            return settings;
        }
        return settings with { PinnedTrayIcons = migrated };
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

    private static bool IsBorderValid(double thickness, string? color)
    {
        if (!double.IsFinite(thickness) || thickness is < 0 or > 12) return false;
        if (string.IsNullOrWhiteSpace(color)) return true;
        return ThemeManager.TryParseColor(color, out _);
    }

    private static bool IsValid(DockSettings settings) => Enum.IsDefined(settings.Position)
        && Enum.IsDefined(settings.Theme) && LanguageCatalog.IsSupported(settings.Language)
        && Enum.IsDefined(settings.AutoHide) && Enum.IsDefined(settings.Layout) && Enum.IsDefined(settings.Size)
        && Enum.IsDefined(settings.DockWidthMode)
        && settings.DockWidthPixels is >= 100 and <= 5000
        && settings.DockWidthPercent is >= 10 and <= 100
        && Enum.IsDefined(settings.DockHeightMode)
        && settings.DockHeightPixels is >= 24 and <= 400
        && settings.DockHeightPercent is >= 2 and <= 25
        && Enum.IsDefined(settings.DockTopGapUnit) && Enum.IsDefined(settings.DockBottomGapUnit)
        && settings.DockTopGapPixels is >= 0 and <= 200 && settings.DockTopGapPercent is >= 0 and <= 100
        && settings.DockBottomGapPixels is >= 0 and <= 200 && settings.DockBottomGapPercent is >= 0 and <= 100
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
        && IsBorderValid(settings.DockBorderThickness, settings.DockBorderColor)
        && IsBorderValid(settings.MenuBorderThickness, settings.MenuBorderColor)
        && IsBorderValid(settings.WidgetBorderThickness, settings.WidgetBorderColor)
        && IsBorderValid(settings.SettingsBorderThickness, settings.SettingsBorderColor)
        && double.IsFinite(settings.DockScale) && settings.DockScale is >= 0.75 and <= 1.5
        && settings.PinOrder?.Any(string.IsNullOrWhiteSpace) != true
        && settings.PinOrder?.Distinct(StringComparer.OrdinalIgnoreCase).Count() == settings.PinOrder?.Count
        && settings.Widgets?.Any(widget => !DockWidgetService.SupportedIds.Contains(widget)) != true
        && settings.Widgets?.Distinct(StringComparer.Ordinal).Count() == settings.Widgets?.Count
        && settings.InstalledWidgets?.Any(widget => !DockWidgetService.SupportedIds.Contains(widget)) != true
        && settings.InstalledWidgets?.Distinct(StringComparer.Ordinal).Count() == settings.InstalledWidgets?.Count
        && settings.Widgets?.Any(widget => settings.InstalledWidgets?.Contains(widget, StringComparer.Ordinal) != true) != true
        && Enum.IsDefined(settings.LauncherHotkey) && Enum.IsDefined(settings.DockHotkey)
        && HotkeyBinding.IsValidCustomString(settings.CustomLauncherHotkey)
        && HotkeyBinding.IsValidCustomString(settings.CustomDockHotkey)
        && Enum.IsDefined(settings.StartAction) && Enum.IsDefined(settings.StartPlacement) && Enum.IsDefined(settings.Edge)
        && Enum.IsDefined(settings.DateStyle) && Enum.IsDefined(settings.DateFormat)
        && Enum.IsDefined(settings.WeatherUnit)
        && Enum.IsDefined(settings.DynamicIslandVisibility)
        && (settings.StatsMeters & ~SystemMeters.All) == 0
        && (settings.AiUsageProviders & ~AiProviders.All) == 0
        && Enum.IsDefined(settings.AiUsageRing)
        && settings.WeatherCity is { Length: <= 80 }
        && settings.StackFolders?.Any(string.IsNullOrWhiteSpace) != true
        && settings.WidgetOrder?.Any(string.IsNullOrWhiteSpace) != true
        && settings.WidgetOrder?.Distinct(StringComparer.OrdinalIgnoreCase).Count() == settings.WidgetOrder?.Count
        && settings.Profiles?.All(profile => profile is not null && IsValidProfile(profile)) != false
        && settings.Profiles?.Select(profile => profile.Name).Distinct(StringComparer.CurrentCultureIgnoreCase).Count() == settings.Profiles?.Count
        && settings.AppGroups?.Any(group => group is null || string.IsNullOrWhiteSpace(group.Id)
            || string.IsNullOrWhiteSpace(group.Name) || group.ExecutablePaths is null) != true
        && settings.AppGroups?.Select(group => group.Id).Distinct(StringComparer.OrdinalIgnoreCase).Count() == settings.AppGroups?.Count
        && settings.AppGroups?.Select(group => group.Name).Distinct(StringComparer.CurrentCultureIgnoreCase).Count() == settings.AppGroups?.Count
        && settings.AppGroups?.SelectMany(group => group.ExecutablePaths ?? [])
            .Distinct(StringComparer.OrdinalIgnoreCase).Count()
            == settings.AppGroups?.Sum(group => (group.ExecutablePaths ?? []).Count)
        && (settings.DockCornerRadius is null || settings.DockCornerRadius is >= 0 and <= 48)
        && settings.WirelessBatteryPollSeconds is >= 15 and <= 300
        && settings.WirelessBatteryLowThreshold is 0 or 10 or 20 or 30
        && settings.BurnInShiftMinutes is >= 5 and <= 120
        && settings.OledLowBatteryThreshold is 0 or 10 or 20 or 30 or 50
        && settings.AnimationFpsCap is 0 or 30 or 60 or 120
        && IsValidIgnoredWirelessDevices(settings.IgnoredWirelessDevices)
        && IsValidWirelessDeviceAliases(settings.WirelessDeviceAliases)
        && IsValidDynamicIslandPlacements(settings.DynamicIslandPlacements)
        && IsValidDynamicIslandWidgetOrders(settings.DynamicIslandWidgetOrders)
        && (string.IsNullOrWhiteSpace(settings.ThemePack) || ThemePackTransfer.IsSafeId(settings.ThemePack.Trim()))
        && IsValidPinnedTrayIcons(settings.PinnedTrayIcons)
        && !HotkeysConflict(settings);

    private void BackupInvalidSettingsFile()
    {
        try
        {
            if (!File.Exists(_path)) return;
            var backupPath = $"{_path}.invalid-{DateTime.UtcNow:yyyyMMddHHmmss}";
            if (!File.Exists(backupPath))
            {
                File.Copy(_path, backupPath);
            }
        }
        catch (Exception ex) when (ex is IOException or UnauthorizedAccessException)
        {
        }
    }

    private static bool IsValidIgnoredWirelessDevices(IReadOnlyList<string>? ignored) =>
        ignored is null
        || (ignored.Count <= 128
            && !ignored.Any(entry => string.IsNullOrWhiteSpace(entry) || entry.Length > 256));

    private static bool IsValidWirelessDeviceAliases(IReadOnlyDictionary<string, string>? aliases) =>
        aliases is null
        || (aliases.Count <= 128
            && !aliases.Any(pair =>
                string.IsNullOrWhiteSpace(pair.Key)
                || pair.Key.Length > 256
                || string.IsNullOrWhiteSpace(pair.Value)
                || pair.Value.Length > 120));

    private static bool IsValidDynamicIslandPlacements(IReadOnlyDictionary<string, string>? placements) =>
        placements is null
        || (placements.Count <= DockWidgetService.CatalogDescriptors.Count
            && placements.Keys.Distinct(StringComparer.OrdinalIgnoreCase).Count() == placements.Count
            && placements.All(pair =>
                !string.IsNullOrWhiteSpace(pair.Key)
                && DockWidgetService.CatalogDescriptors.Any(widget =>
                    string.Equals(widget.Id, pair.Key, StringComparison.OrdinalIgnoreCase))
                && !string.IsNullOrWhiteSpace(pair.Value)
                && pair.Value.Length <= 32));

    private static bool IsValidDynamicIslandWidgetOrders(
        IReadOnlyDictionary<string, IReadOnlyList<string>>? orders) =>
        orders is null
        || (orders.Count <= 32
            && orders.All(pair =>
                !string.IsNullOrWhiteSpace(pair.Key)
                && pair.Key.Length <= 32
                && pair.Value is not null
                && pair.Value.Count <= DockWidgetService.CatalogDescriptors.Count
                && pair.Value.All(id => DockWidgetService.CatalogDescriptors.Any(widget =>
                    string.Equals(widget.Id, id, StringComparison.OrdinalIgnoreCase)))
                && pair.Value.Distinct(StringComparer.OrdinalIgnoreCase).Count() == pair.Value.Count));

    private static bool IsValidPinnedTrayIcons(IReadOnlyList<string>? pinned) =>
        pinned is null
        || (pinned.Count <= TrayPinIdentity.MaxPinnedIcons
            && !pinned.Any(entry => string.IsNullOrWhiteSpace(entry) || entry.Length > TrayPinIdentity.MaxKeyLength));

    /// <summary>Launcher and dock bindings must not resolve to the same combination, or dock focus hotkey dies.</summary>
    private static bool HotkeysConflict(DockSettings settings)
    {
        var launcher = HotkeyBinding.Resolve(settings.LauncherHotkey, settings.CustomLauncherHotkey);
        var dock = HotkeyBinding.Resolve(settings.DockHotkey, settings.CustomDockHotkey);
        return HotkeyBinding.EqualsBinding(launcher, dock);
    }

    private static bool IsValidProfile(DockProfile profile) =>
        !string.IsNullOrWhiteSpace(profile.Name)
        && profile.HiddenApps is not null
        && profile.Widgets?.Any(widget => !DockWidgetService.SupportedIds.Contains(widget)) != true
        && profile.Widgets?.Distinct(StringComparer.Ordinal).Count() == profile.Widgets?.Count
        && profile.InstalledWidgets?.Any(widget => !DockWidgetService.SupportedIds.Contains(widget)) != true
        && profile.InstalledWidgets?.Distinct(StringComparer.Ordinal).Count() == profile.InstalledWidgets?.Count
        && (profile.Widgets is null || profile.InstalledWidgets is null
            || profile.Widgets.All(widget => profile.InstalledWidgets.Contains(widget, StringComparer.Ordinal)))
        && profile.WidgetOrder?.Any(string.IsNullOrWhiteSpace) != true
        && profile.WidgetOrder?.Distinct(StringComparer.OrdinalIgnoreCase).Count() == profile.WidgetOrder?.Count
        && (profile.DockWidthMode is null || Enum.IsDefined(profile.DockWidthMode.Value))
        && (profile.DockWidthPixels is null || profile.DockWidthPixels is >= 100 and <= 5000)
        && (profile.DockWidthPercent is null || profile.DockWidthPercent is >= 10 and <= 100)
        && (profile.Theme is null || Enum.IsDefined(profile.Theme.Value))
        && (profile.LauncherHotkey is null || Enum.IsDefined(profile.LauncherHotkey.Value))
        && (profile.DockHotkey is null || Enum.IsDefined(profile.DockHotkey.Value))
        && HotkeyBinding.IsValidCustomString(profile.CustomLauncherHotkey)
        && HotkeyBinding.IsValidCustomString(profile.CustomDockHotkey)
        && (profile.AutoHide is null || Enum.IsDefined(profile.AutoHide.Value));

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
