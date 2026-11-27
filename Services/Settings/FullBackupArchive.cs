using System.IO;
using System.IO.Compression;
using System.Text;
using System.Text.Json;
using OpenTaskbar.Models;

namespace OpenTaskbar.Services;

/// <summary>
/// Exports, inspects and imports a single <c>.otbackup</c> zip archive containing dock settings, pinned apps and the
/// active custom theme pack.
/// </summary>
public static class FullBackupArchive
{
    public const string ExportExtension = ".otbackup";
    public const int CurrentVersion = 1;

    // Only these entries are ever written/extracted. No secrets/ DPAPI blobs may enter an archive.
    internal const string ManifestFileName = "manifest.json";
    internal const string SettingsFileName = "settings.json";
    internal const string ThemeFolderName = "theme";

    private static readonly JsonSerializerOptions ManifestJsonOptions = new()
    {
        WriteIndented = true,
        PropertyNameCaseInsensitive = true
    };

    private sealed record ArchiveManifest(int Version, bool HasTheme, string? ThemeId, string? ThemeName);

    public sealed record BackupPreview(DockSettings Settings, IReadOnlyList<PinBackup> Pins, bool HasTheme, string? ThemeName);

    private sealed record StagedBackup(
        string StagingDirectory,
        DockSettings Settings,
        IReadOnlyList<PinBackup> Pins,
        bool HasTheme,
        string? ThemeId,
        string? ThemeName,
        string? ThemeDirectory);

    /// <summary>
    /// Writes settings, pins and (when active and valid) the custom theme pack into a single portable archive.
    /// Returns a localization error key, or null on success.
    /// </summary>
    public static string? Export(
        string destinationPath,
        DockSettings settings,
        string? themesRoot = null,
        IReadOnlyList<PinBackup>? pinsOverride = null)
    {
        if (!DockSettingsStore.Validate(settings)) return "ErrorFullBackupInvalid";

        var temporaryPath = destinationPath + "." + Guid.NewGuid().ToString("N") + ".tmp";
        try
        {
            var fullDestination = Path.GetFullPath(destinationPath);
            var parent = Path.GetDirectoryName(fullDestination);
            if (string.IsNullOrEmpty(parent)) return "ErrorFullBackupWrite";
            Directory.CreateDirectory(parent);

            var hasTheme = false;
            string? themeId = null;
            string? themeName = null;
            string? themeDir = null;
            var themeFiles = Array.Empty<string>();

            if (!string.IsNullOrWhiteSpace(settings.ThemePack)
                && ThemePackTransfer.IsSafeId(settings.ThemePack)
                && !ThemePack.IsBuiltInTheme(settings.ThemePack))
            {
                var root = themesRoot ?? ThemePacks.Root;
                var candidateDir = Path.Combine(root, settings.ThemePack);
                if (Directory.Exists(candidateDir) && ThemePackTransfer.Validate(candidateDir) is null)
                {
                    var files = Directory.EnumerateFiles(candidateDir, "*", SearchOption.AllDirectories).ToArray();
                    if (IsExportableThemeFiles(files))
                    {
                        hasTheme = true;
                        themeId = settings.ThemePack;
                        themeDir = candidateDir;
                        themeFiles = files;
                        themeName = ThemePack.Load(candidateDir).Pack?.DisplayName ?? settings.ThemePack;
                    }
                }
            }

            var manifest = new ArchiveManifest(CurrentVersion, hasTheme, themeId, themeName);
            var settingsJson = SettingsBackup.Serialize(settings, pinsOverride);

            using (var archive = ZipFile.Open(temporaryPath, ZipArchiveMode.Create))
            {
                WriteTextEntry(archive, ManifestFileName, JsonSerializer.Serialize(manifest, ManifestJsonOptions));
                WriteTextEntry(archive, SettingsFileName, settingsJson);

                if (hasTheme && themeDir is not null)
                {
                    foreach (var file in themeFiles)
                    {
                        var relative = Path.GetRelativePath(themeDir, file).Replace(Path.DirectorySeparatorChar, '/');
                        archive.CreateEntryFromFile(file, ThemeFolderName + "/" + relative, CompressionLevel.Optimal);
                    }
                }
            }

            File.Move(temporaryPath, destinationPath, overwrite: true);
            return null;
        }
        catch (Exception ex) when (ex is IOException or UnauthorizedAccessException or InvalidDataException)
        {
            AppLog.Warning($"Full backup export failed: {ex.Message}");
            return "ErrorFullBackupWrite";
        }
        finally
        {
            TryDeleteFile(temporaryPath);
        }
    }

    /// <summary>Validates an archive in a temporary staging folder and returns its preview metadata.</summary>
    public static (BackupPreview? Preview, string? Error) Inspect(string archivePath)
    {
        var staged = ValidateAndStage(archivePath);
        if (staged is null) return (null, "ErrorFullBackupInvalid");
        try
        {
            return (new BackupPreview(staged.Settings, staged.Pins, staged.HasTheme, staged.ThemeName), null);
        }
        finally
        {
            TryDeleteDirectory(staged.StagingDirectory);
        }
    }

    /// <summary>
    /// Validates an archive in a temporary staging folder before installing the bundled theme (when requested).
    /// Nothing is written to <paramref name="themesRoot"/> if validation fails or <paramref name="restoreTheme"/> is false.
    /// </summary>
    public static (DockSettings? Settings, IReadOnlyList<PinBackup> Pins, string? ImportedThemeId, string? Error) Import(
        string archivePath,
        bool restoreTheme,
        string? themesRoot = null)
    {
        var staged = ValidateAndStage(archivePath);
        if (staged is null) return (null, [], null, "ErrorFullBackupInvalid");
        try
        {
            if (!staged.HasTheme || !restoreTheme)
                return (staged.Settings, staged.Pins, null, null);

            var root = themesRoot ?? ThemePacks.Root;
            Directory.CreateDirectory(root);
            var id = ThemePackTransfer.UniqueId(root, staged.ThemeName ?? staged.ThemeId);
            var tempTarget = Path.Combine(root, ".import-" + Guid.NewGuid().ToString("N"));
            var target = Path.Combine(root, id);
            try
            {
                Directory.CreateDirectory(tempTarget);
                ThemePackTransfer.CopyDirectory(staged.ThemeDirectory!, tempTarget);
                Directory.Move(tempTarget, target);
            }
            finally
            {
                TryDeleteDirectory(tempTarget);
            }

            var profiles = staged.Settings.Profiles;
            if (profiles is not null && !string.IsNullOrWhiteSpace(staged.ThemeId))
            {
                profiles = profiles
                    .Select(profile => string.Equals(profile.ThemePack, staged.ThemeId, StringComparison.OrdinalIgnoreCase)
                        ? profile with { ThemePack = id }
                        : profile)
                    .ToArray();
            }

            return (staged.Settings with { ThemePack = id, Profiles = profiles }, staged.Pins, id, null);
        }
        catch (Exception ex) when (ex is IOException or UnauthorizedAccessException)
        {
            AppLog.Warning($"Full backup import failed: {ex.Message}");
            return (null, [], null, "ErrorFullBackupInvalid");
        }
        finally
        {
            TryDeleteDirectory(staged.StagingDirectory);
        }
    }

    private static StagedBackup? ValidateAndStage(string archivePath)
    {
        if (string.IsNullOrWhiteSpace(archivePath) || !File.Exists(archivePath))
            return null;

        var staging = Path.Combine(Path.GetTempPath(), "OpenTaskbar-backup-" + Guid.NewGuid().ToString("N"));
        var keepStaging = false;
        try
        {
            if (new FileInfo(archivePath).Length > ThemePackTransfer.MaxUncompressedBytes)
                return null;

            Directory.CreateDirectory(staging);
            using (var archive = ZipFile.OpenRead(archivePath))
                ExtractSafe(archive, staging);

            var manifestPath = Path.Combine(staging, ManifestFileName);
            if (!File.Exists(manifestPath)) return null;
            var manifest = JsonSerializer.Deserialize<ArchiveManifest>(File.ReadAllText(manifestPath), ManifestJsonOptions);
            if (manifest is null || manifest.Version != CurrentVersion) return null;

            var settingsPath = Path.Combine(staging, SettingsFileName);
            if (!File.Exists(settingsPath)) return null;
            var (settings, pins, settingsError) = SettingsBackup.Parse(File.ReadAllText(settingsPath));
            if (settingsError is not null || settings is null) return null;

            string? themeDir = null;
            string? themeId = null;
            string? themeName = null;
            if (manifest.HasTheme)
            {
                var candidateDir = Path.Combine(staging, ThemeFolderName);
                if (!Directory.Exists(candidateDir)) return null;
                if (ThemePackTransfer.Validate(candidateDir) is not null) return null;

                var (pack, packError, _) = ThemePack.Load(candidateDir);
                if (pack is null || packError is not null) return null;

                themeDir = candidateDir;
                themeId = manifest.ThemeId;
                themeName = !string.IsNullOrWhiteSpace(manifest.ThemeName) ? manifest.ThemeName : pack.DisplayName;
            }

            keepStaging = true;
            return new StagedBackup(staging, settings, pins, manifest.HasTheme, themeId, themeName, themeDir);
        }
        catch (Exception ex) when (ex is IOException or UnauthorizedAccessException or InvalidDataException
            or JsonException or NotSupportedException or System.Security.SecurityException or ArgumentException)
        {
            AppLog.Warning($"Full backup validation failed: {ex.Message}");
            return null;
        }
        finally
        {
            if (!keepStaging) TryDeleteDirectory(staging);
        }
    }

    private static void ExtractSafe(ZipArchive archive, string destination)
    {
        var root = Path.GetFullPath(destination);
        var prefix = Path.TrimEndingDirectorySeparator(root) + Path.DirectorySeparatorChar;
        var count = 0;
        long totalBytes = 0;
        var buffer = new byte[81920];

        // Directory markers are excluded here (equivalent to the original continue); file-entry validation and order are unchanged.
        foreach (var entry in archive.Entries.Where(e => !string.IsNullOrEmpty(e.Name)))
        {
            if (++count > ThemePackTransfer.MaxFileCount)
                throw new InvalidDataException("Too many files in backup archive.");

            var relative = entry.FullName.Replace('\\', '/');
            if (Path.IsPathRooted(relative)
                || relative.StartsWith('/')
                || relative.Contains(':')
                || relative.Split('/').Any(part => part is "" or ".." or "."))
            {
                throw new InvalidDataException("Unsafe path in backup archive.");
            }

            var isManifest = string.Equals(relative, ManifestFileName, StringComparison.OrdinalIgnoreCase);
            var isSettings = string.Equals(relative, SettingsFileName, StringComparison.OrdinalIgnoreCase);
            var isThemeEntry = relative.StartsWith(ThemeFolderName + "/", StringComparison.OrdinalIgnoreCase);
            if (!isManifest && !isSettings && !isThemeEntry)
                throw new InvalidDataException($"Unexpected entry '{relative}' in backup archive.");

            if (isThemeEntry && !ThemePackTransfer.IsAllowedExtension(Path.GetExtension(entry.Name)))
                throw new InvalidDataException($"File type '{Path.GetExtension(entry.Name)}' is not allowed.");

            var full = Path.GetFullPath(Path.Combine(root, relative));
            if (!full.StartsWith(prefix, StringComparison.OrdinalIgnoreCase))
                throw new InvalidDataException("Unsafe path in backup archive.");

            Directory.CreateDirectory(Path.GetDirectoryName(full)!);
            using var input = entry.Open();
            using var output = new FileStream(full, FileMode.Create, FileAccess.Write, FileShare.None);
            long entryBytes = 0;
            int read;
            while ((read = input.Read(buffer, 0, buffer.Length)) > 0)
            {
                entryBytes += read;
                totalBytes += read;
                if (entryBytes > ThemePackTransfer.MaxFileBytes || totalBytes > ThemePackTransfer.MaxUncompressedBytes)
                    throw new InvalidDataException("Backup archive exceeds size limit.");

                output.Write(buffer, 0, read);
            }
        }
    }

    private static bool IsExportableThemeFiles(string[] files)
    {
        if (files.Length == 0 || files.Length > ThemePackTransfer.MaxFileCount) return false;
        long totalBytes = 0;
        foreach (var file in files)
        {
            if (!ThemePackTransfer.IsAllowedExtension(Path.GetExtension(file))) return false;
            var length = new FileInfo(file).Length;
            if (length > ThemePackTransfer.MaxFileBytes) return false;
            totalBytes += length;
            if (totalBytes > ThemePackTransfer.MaxUncompressedBytes) return false;
        }

        return true;
    }

    private static void WriteTextEntry(ZipArchive archive, string entryName, string content)
    {
        var entry = archive.CreateEntry(entryName, CompressionLevel.Optimal);
        using var writer = new StreamWriter(entry.Open(), new UTF8Encoding(encoderShouldEmitUTF8Identifier: false));
        writer.Write(content);
    }

    private static void TryDeleteFile(string path)
    {
        try
        {
            if (File.Exists(path)) File.Delete(path);
        }
        catch (Exception ex) when (ex is IOException or UnauthorizedAccessException)
        {
            AppLog.Warning($"Temporary backup file cleanup failed: {ex.Message}");
        }
    }

    private static void TryDeleteDirectory(string path)
    {
        try
        {
            if (Directory.Exists(path)) Directory.Delete(path, recursive: true);
        }
        catch (Exception ex) when (ex is IOException or UnauthorizedAccessException)
        {
            AppLog.Warning($"Temporary backup folder cleanup failed: {ex.Message}");
        }
    }
}
