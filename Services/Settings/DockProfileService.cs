using OpenTaskbar.Models;

namespace OpenTaskbar.Services;

/// <summary>
/// Manages named dock profiles: capturing live settings snapshots, switching between profiles,
/// and creating, renaming, or deleting profiles while keeping the active profile in sync.
/// </summary>
public static class DockProfileService
{
    public const string DefaultProfileName = "Default";

    /// <summary>Captures a profile snapshot of profile-scoped settings from <paramref name="settings"/>.</summary>
    public static DockProfile Capture(string name, DockSettings settings, IReadOnlyList<string>? hiddenApps = null)
    {
        var widgets = settings.Widgets?.ToArray() ?? [];
        var installedWidgets = MergeInstalledWidgets(settings.InstalledWidgets, widgets);
        // Keep unknown profile keys written by the other build when re-capturing a snapshot.
        var previous = settings.Profiles?.FirstOrDefault(profile =>
            string.Equals(profile.Name, name, StringComparison.OrdinalIgnoreCase));

        return new DockProfile(
            Name: name,
            HiddenApps: (hiddenApps ?? []).ToArray(),
            Widgets: widgets,
            InstalledWidgets: installedWidgets,
            WidgetOrder: settings.WidgetOrder?.ToArray(),
            DockItemOrder: settings.DockItemOrder?.ToArray(),
            DockWidthMode: settings.DockWidthMode,
            DockWidthPixels: settings.DockWidthPixels,
            DockWidthPercent: settings.DockWidthPercent,
            Theme: settings.Theme,
            ThemePack: settings.ThemePack,
            ThemeApplicationIcons: settings.ThemeApplicationIcons,
            LauncherHotkey: settings.LauncherHotkey,
            DockHotkey: settings.DockHotkey,
            AutoHide: settings.AutoHide,
            CustomLauncherHotkey: settings.CustomLauncherHotkey,
            CustomDockHotkey: settings.CustomDockHotkey)
        {
            Extra = previous is null ? [] : new Dictionary<string, System.Text.Json.JsonElement>(previous.Extra, StringComparer.Ordinal)
        };
    }

    /// <summary>Ensures at least one profile exists by seeding a Default profile when none are configured.</summary>
    public static DockSettings EnsureDefaultProfile(DockSettings settings)
    {
        if (settings.Profiles is { Count: > 0 })
            return settings;

        return settings with
        {
            Profiles = [Capture(DefaultProfileName, settings, [])],
            ActiveProfile = DefaultProfileName
        };
    }

    /// <summary>Updates the active profile slot with the current live values from <paramref name="settings"/>.</summary>
    public static DockSettings SyncActive(DockSettings settings)
    {
        if (settings.Profiles is not { Count: > 0 } || string.IsNullOrWhiteSpace(settings.ActiveProfile))
            return settings;

        var index = FindProfileIndex(settings.Profiles, settings.ActiveProfile);
        if (index < 0)
            return settings;

        var current = settings.Profiles[index];
        var updated = settings.Profiles.ToArray();
        updated[index] = Capture(current.Name, settings, current.HiddenApps);
        return settings with { Profiles = updated };
    }

    /// <summary>Saves edits to the outgoing active profile and applies the target profile's settings.</summary>
    public static (DockSettings Settings, string? Error) Switch(DockSettings settings, string name)
    {
        settings = EnsureDefaultProfile(settings);
        var profiles = settings.Profiles!;
        var targetIndex = FindProfileIndex(profiles, name);
        if (targetIndex < 0)
            return (settings, "ErrorProfileNotFound");

        var target = profiles[targetIndex];
        if (string.Equals(settings.ActiveProfile, target.Name, StringComparison.CurrentCultureIgnoreCase))
            return (settings with { ActiveProfile = target.Name }, null);

        var synced = SyncActive(settings);
        var syncedTarget = synced.Profiles![FindProfileIndex(synced.Profiles!, target.Name)];
        return (ApplyProfile(synced, syncedTarget), null);
    }

    /// <summary>Creates a new profile from the current live settings and activates it.</summary>
    public static (DockSettings Settings, string? Error) Create(DockSettings settings, string rawName)
    {
        settings = EnsureDefaultProfile(settings);
        var name = rawName?.Trim() ?? string.Empty;
        if (string.IsNullOrWhiteSpace(name))
            return (settings, "ErrorProfileNameEmpty");

        if (FindProfileIndex(settings.Profiles!, name) >= 0)
            return (settings, "ErrorProfileExists");

        var synced = SyncActive(settings);
        // A new profile starts with an empty widget set; only existing profiles carry widgets.
        // Clear both the captured snapshot and the live settings so Preferences_Changed rebuilds the dock
        // without the outgoing profile's widgets.
        var created = Capture(name, synced, []) with
        {
            Widgets = [],
            InstalledWidgets = [],
            WidgetOrder = null,
            DockItemOrder = RemoveWidgetTokens(synced.DockItemOrder)
        };
        return (synced with
        {
            Widgets = [],
            InstalledWidgets = [],
            WidgetOrder = null,
            DockItemOrder = created.DockItemOrder,
            Profiles = [.. synced.Profiles!, created],
            ActiveProfile = name
        }, null);
    }

    /// <summary>Renames an existing profile and updates <see cref="DockSettings.ActiveProfile"/> if it was active.</summary>
    public static (DockSettings Settings, string? Error) Rename(DockSettings settings, string currentName, string rawNewName)
    {
        settings = EnsureDefaultProfile(settings);
        var newName = rawNewName?.Trim() ?? string.Empty;
        if (string.IsNullOrWhiteSpace(newName))
            return (settings, "ErrorProfileNameEmpty");

        var profiles = settings.Profiles!;
        var index = FindProfileIndex(profiles, currentName);
        if (index < 0)
            return (settings, "ErrorProfileNotFound");

        var duplicateIndex = FindProfileIndex(profiles, newName);
        if (duplicateIndex >= 0 && duplicateIndex != index)
            return (settings, "ErrorProfileExists");

        var updated = profiles.ToArray();
        updated[index] = updated[index] with { Name = newName };
        var activeProfile = string.Equals(settings.ActiveProfile, currentName, StringComparison.CurrentCultureIgnoreCase)
            ? newName
            : settings.ActiveProfile;

        return (settings with { Profiles = updated, ActiveProfile = activeProfile }, null);
    }

    /// <summary>Deletes a profile unless it is the last remaining one, activating the first remaining profile if needed.</summary>
    public static (DockSettings Settings, string? Error) Delete(DockSettings settings, string name)
    {
        settings = EnsureDefaultProfile(settings);
        var profiles = settings.Profiles!;
        var index = FindProfileIndex(profiles, name);
        if (index < 0)
            return (settings, "ErrorProfileNotFound");

        if (profiles.Count <= 1)
            return (settings, "ErrorProfileDeleteLast");

        var wasActive = string.Equals(settings.ActiveProfile, profiles[index].Name, StringComparison.CurrentCultureIgnoreCase);
        var remaining = profiles.Where((_, i) => i != index).ToArray();
        var updated = settings with { Profiles = remaining };
        return wasActive ? (ApplyProfile(updated, remaining[0]), null) : (updated, null);
    }

    /// <summary>Updates the hidden pinned apps list for <paramref name="profileName"/> if present.</summary>
    public static DockSettings UpdateHiddenApps(DockSettings settings, string profileName, IReadOnlyList<string> hiddenApps)
    {
        if (settings.Profiles is not { Count: > 0 })
            return settings;

        var index = FindProfileIndex(settings.Profiles, profileName);
        if (index < 0)
            return settings;

        var updated = settings.Profiles.ToArray();
        updated[index] = updated[index] with
        {
            HiddenApps = hiddenApps.Distinct(StringComparer.OrdinalIgnoreCase).ToArray()
        };
        return settings with { Profiles = updated };
    }

    private static int FindProfileIndex(IReadOnlyList<DockProfile> profiles, string name)
    {
        for (var i = 0; i < profiles.Count; i++)
        {
            if (string.Equals(profiles[i].Name, name, StringComparison.CurrentCultureIgnoreCase))
                return i;
        }
        return -1;
    }

    private static string[] MergeInstalledWidgets(IReadOnlyList<string>? installed, IReadOnlyList<string> widgets) =>
        (installed ?? []).Concat(widgets)
            .Select(DockSettingsStore.CanonicalizeWidgetId)
            .Distinct(StringComparer.OrdinalIgnoreCase).ToArray();

    /// <summary>Drops widget tokens from a dock item order so a widgets-free profile has no stale widget slots.</summary>
    private static IReadOnlyList<string>? RemoveWidgetTokens(IReadOnlyList<string>? order)
    {
        if (order is null) return null;
        var kept = order.Where(token => !token.StartsWith("widget:", StringComparison.OrdinalIgnoreCase)).ToArray();
        return kept.Length == order.Count ? order : kept;
    }

    private static bool IsLegacyProfile(DockProfile profile) =>
        profile.Widgets is null
        && profile.InstalledWidgets is null
        && profile.WidgetOrder is null
        && profile.DockItemOrder is null
        && profile.DockWidthMode is null
        && profile.DockWidthPixels is null
        && profile.DockWidthPercent is null
        && profile.Theme is null
        && profile.ThemePack is null
        && profile.ThemeApplicationIcons is null
        && profile.LauncherHotkey is null
        && profile.DockHotkey is null
        && profile.AutoHide is null
        && profile.CustomLauncherHotkey is null
        && profile.CustomDockHotkey is null;

    private static DockSettings ApplyProfile(DockSettings settings, DockProfile target)
    {
        if (IsLegacyProfile(target))
            return settings with { ActiveProfile = target.Name };

        var widgets = target.Widgets is not null ? target.Widgets.ToArray() : settings.Widgets;
        IReadOnlyList<string>? installedWidgets;
        if (target.InstalledWidgets is not null)
            installedWidgets = MergeInstalledWidgets(target.InstalledWidgets, widgets ?? []);
        else if (target.Widgets is not null)
            installedWidgets = MergeInstalledWidgets(settings.InstalledWidgets, widgets!);
        else
            installedWidgets = settings.InstalledWidgets;

        return settings with
        {
            Widgets = widgets,
            InstalledWidgets = installedWidgets,
            WidgetOrder = target.WidgetOrder?.ToArray(),
            DockItemOrder = target.DockItemOrder?.ToArray(),
            DockWidthMode = target.DockWidthMode ?? settings.DockWidthMode,
            DockWidthPixels = target.DockWidthPixels ?? settings.DockWidthPixels,
            DockWidthPercent = target.DockWidthPercent ?? settings.DockWidthPercent,
            Theme = target.Theme ?? settings.Theme,
            ThemePack = target.ThemePack,
            ThemeApplicationIcons = target.ThemeApplicationIcons ?? settings.ThemeApplicationIcons,
            LauncherHotkey = target.LauncherHotkey ?? settings.LauncherHotkey,
            DockHotkey = target.DockHotkey ?? settings.DockHotkey,
            CustomLauncherHotkey = target.CustomLauncherHotkey ?? (target.LauncherHotkey is not null ? null : settings.CustomLauncherHotkey),
            CustomDockHotkey = target.CustomDockHotkey ?? (target.DockHotkey is not null ? null : settings.CustomDockHotkey),
            AutoHide = target.AutoHide ?? settings.AutoHide,
            ActiveProfile = target.Name
        };
    }
}
