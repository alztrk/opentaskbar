using System.IO;
using System.Text.Json;
using OpenTaskbar.Models;

namespace OpenTaskbar.Services;

public sealed record PinBackup(string Name, string ExecutablePath, string? Arguments);

/// <summary>One-file backup of the dock settings and the pinned apps, for moving to another PC or restoring.</summary>
public static class SettingsBackup
{
    private sealed record BackupFile(int Version, DockSettings Settings, IReadOnlyList<PinBackup> Pins);

    public static string? Export(string path, DockSettings settings)
    {
        var pins = WindowsTaskbarPinImporter.Import().Apps
            .Select(app => new PinBackup(app.Name, app.ExecutablePath, app.LaunchArguments))
            .ToList();
        try
        {
            File.WriteAllText(path, JsonSerializer.Serialize(new BackupFile(1, settings, pins), DockSettingsStore.SerializerOptions));
            return null;
        }
        catch (Exception ex) when (ex is IOException or UnauthorizedAccessException)
        {
            AppLog.Warning($"Settings export failed: {ex.Message}");
            return "ErrorSettingsFileWrite";
        }
    }

    public static (DockSettings? Settings, IReadOnlyList<PinBackup> Pins, string? Error) Read(string path)
    {
        try
        {
            var backup = JsonSerializer.Deserialize<BackupFile>(File.ReadAllText(path), DockSettingsStore.SerializerOptions);
            if (backup?.Settings is null) return (null, [], "ErrorSettingsFileInvalid");
            var settings = DockSettingsStore.MigrateLegacyAiUsage(backup.Settings);
            if (!DockSettingsStore.Validate(settings)) return (null, [], "ErrorSettingsFileInvalid");
            var pins = (backup.Pins ?? []).Where(pin => !string.IsNullOrWhiteSpace(pin.ExecutablePath)).ToList();
            return (settings, pins, null);
        }
        catch (Exception ex) when (ex is IOException or UnauthorizedAccessException or JsonException or NotSupportedException)
        {
            AppLog.Warning($"Settings import failed: {ex.Message}");
            return (null, [], "ErrorSettingsFileInvalid");
        }
    }
}
