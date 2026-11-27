using System.IO;
using System.IO.Compression;
using System.Text.Json;
using System.Text.Json.Serialization;
using OpenTaskbar.Models;

namespace OpenTaskbar.Services;

/// <summary>
/// Creates, duplicates, exports and imports theme packs with strict validation. Theme packs are plain folders under
/// <see cref="ThemePacks.Root"/>; a shared file is a zip whose entries are relative to the theme folder.
/// </summary>
internal static class ThemePackTransfer
{
    internal const string ExportExtension = ".ottheme";
    internal const int MaxFileCount = 512;
    internal const long MaxUncompressedBytes = 64L * 1024 * 1024;
    internal const long MaxFileBytes = 16L * 1024 * 1024;

    private static readonly JsonSerializerOptions JsonOptions = new()
    {
        PropertyNameCaseInsensitive = true,
        ReadCommentHandling = JsonCommentHandling.Skip,
        AllowTrailingCommas = true
    };

    // Rust `ThemePackManifest::to_string_pretty` writes camelCase keys ("name", "preserveOriginalIcons")
    // and omits nulls. Emit the exact same wire form so Rust's strict reader accepts it directly.
    private static readonly JsonSerializerOptions ManifestWriteOptions = new()
    {
        PropertyNamingPolicy = JsonNamingPolicy.CamelCase,
        DefaultIgnoreCondition = JsonIgnoreCondition.WhenWritingNull,
        WriteIndented = true
    };

    private static readonly HashSet<string> AllowedExtensions = new(StringComparer.OrdinalIgnoreCase)
    {
        ".json", ".png", ".ico", ".jpg", ".jpeg", ".bmp", ".md", ".txt"
    };

    /// <summary>A safe folder name derived from <paramref name="name"/>, made unique under the themes root.</summary>
    internal static string CreateTheme(string name, string? author, string? basedOnId)
    {
        var root = ThemePacks.Root;
        Directory.CreateDirectory(root);
        var id = UniqueId(root, name);
        var target = Path.Combine(root, id);
        Directory.CreateDirectory(target);

        var preserveOriginalIcons = false;
        if (!string.IsNullOrWhiteSpace(basedOnId) && IsSafeId(basedOnId))
        {
            var source = Path.Combine(root, basedOnId);
            if (Directory.Exists(source))
            {
                preserveOriginalIcons = ThemePack.IsBuiltInTheme(basedOnId)
                    || ThemePack.Load(source).Pack?.Manifest.PreserveOriginalIcons == true;
                CopyDirectory(source, target);
            }
        }

        var manifestPath = Path.Combine(target, ThemePack.ManifestFileName);
        var manifest = new ThemePackManifest(
            string.IsNullOrWhiteSpace(name) ? id : name.Trim(),
            string.IsNullOrWhiteSpace(author) ? null : author.Trim(),
            null,
            preserveOriginalIcons);
        File.WriteAllText(manifestPath, JsonSerializer.Serialize(manifest, ManifestWriteOptions));
        return id;
    }

    /// <summary>Writes <paramref name="id"/> as a portable zip file. Returns an error key, or null on success.</summary>
    internal static string? ExportTheme(string id, string destinationPath)
    {
        if (!IsSafeId(id)) return "ErrorThemeInvalid";
        var root = ThemePacks.Root;
        var source = Path.Combine(root, id);
        if (!Directory.Exists(source)) return "ErrorThemeMissing";

        var files = Directory.EnumerateFiles(source, "*", SearchOption.AllDirectories).ToArray();
        if (files.Length == 0 || files.Length > MaxFileCount) return "ErrorThemeInvalid";
        if (files.Sum(file => new FileInfo(file).Length) > MaxUncompressedBytes) return "ErrorThemeInvalid";

        try
        {
            var parent = Path.GetDirectoryName(Path.GetFullPath(destinationPath));
            if (string.IsNullOrEmpty(parent)) return "ErrorThemeInstall";
            Directory.CreateDirectory(parent);
            if (File.Exists(destinationPath)) File.Delete(destinationPath);

            using var archive = ZipFile.Open(destinationPath, ZipArchiveMode.Create);
            foreach (var file in files)
            {
                var relative = Path.GetRelativePath(source, file).Replace(Path.DirectorySeparatorChar, '/');
                // The archive stores entries relative to the theme folder, under a folder named after the theme.
                archive.CreateEntryFromFile(file, id + "/" + relative, CompressionLevel.Optimal);
            }

            return null;
        }
        catch (Exception ex) when (ex is IOException or UnauthorizedAccessException or InvalidDataException)
        {
            AppLog.Warning($"Theme export failed: {ex.Message}");
            return "ErrorThemeInstall";
        }
    }

    /// <summary>
    /// Imports a shared theme file into the themes root. The archive may contain one theme folder at its root or a
    /// theme laid out directly at the root. Returns the imported theme id, or an error key prefixed with "!".
    /// </summary>
    internal static (string? Id, string? Error) ImportTheme(string archivePath)
    {
        try
        {
            if (!File.Exists(archivePath)) return (null, "ErrorThemeMissing");
            if (new FileInfo(archivePath).Length > MaxUncompressedBytes) return (null, "ErrorThemeInvalid");

            var root = ThemePacks.Root;
            Directory.CreateDirectory(root);
            var staging = Path.Combine(root, ".import-" + Guid.NewGuid().ToString("N"));
            Directory.CreateDirectory(staging);

            try
            {
                using (var archive = ZipFile.OpenRead(archivePath)) ExtractSafe(archive, staging);

                // Accept a single folder containing theme.json, or theme.json directly at the root.
                var candidates = Directory.Exists(Path.Combine(staging, ThemePack.IconFolder))
                    || File.Exists(Path.Combine(staging, ThemePack.ManifestFileName))
                    ? new[] { staging }
                    : Directory.EnumerateDirectories(staging).ToArray();
                var themeDir = candidates.FirstOrDefault(dir => File.Exists(Path.Combine(dir, ThemePack.ManifestFileName)));
                if (themeDir is null) return (null, "ErrorThemeInvalid");
                if (Validate(themeDir) is not null) return (null, "ErrorThemeInvalid");

                var (pack, error, _) = ThemePack.Load(themeDir);
                if (pack is null || error is not null) return (null, "ErrorThemeInvalid");

                var id = UniqueId(root, pack.DisplayName);
                var target = Path.Combine(root, id);
                if (string.Equals(Path.GetFullPath(themeDir), Path.GetFullPath(target), StringComparison.OrdinalIgnoreCase))
                    return (null, "ErrorThemeInvalid");
                Directory.Move(themeDir, target);
                return (id, null);
            }
            finally
            {
                if (Directory.Exists(staging)) Directory.Delete(staging, recursive: true);
            }
        }
        catch (Exception ex) when (ex is IOException or UnauthorizedAccessException or InvalidDataException
            or System.Security.SecurityException)
        {
            AppLog.Warning($"Theme import failed: {ex.Message}");
            return (null, "ErrorThemeInstall");
        }
    }

    /// <summary>Deletes a theme folder after a safety check on its id. Returns an error key, or null on success.</summary>
    internal static string? DeleteTheme(string id)
    {
        if (!IsSafeId(id)) return "ErrorThemeInvalid";
        var path = Path.GetFullPath(Path.Combine(ThemePacks.Root, id));
        var root = Path.GetFullPath(ThemePacks.Root) + Path.DirectorySeparatorChar;
        if (!path.StartsWith(root, StringComparison.OrdinalIgnoreCase) || !Directory.Exists(path))
            return "ErrorThemeMissing";
        try
        {
            Directory.Delete(path, recursive: true);
            return null;
        }
        catch (Exception ex) when (ex is IOException or UnauthorizedAccessException)
        {
            AppLog.Warning($"Theme delete failed: {ex.Message}");
            return "ErrorThemeInstall";
        }
    }

    /// <summary>Returns a validation error message, or null when every JSON file fits the theme schema.</summary>
    internal static string? Validate(string directory)
    {
        var manifestPath = Path.Combine(directory, ThemePack.ManifestFileName);
        if (!File.Exists(manifestPath)) return "theme.json missing.";
        if (!TryReadJson<ThemePackManifest>(manifestPath, out var manifest)) return "theme.json is not valid JSON.";
        if (manifest is null) return "theme.json is empty.";
        if (manifest.Author is { Length: > 200 } || manifest.Description is { Length: > 1000 })
            return "theme.json fields are too long.";

        var iconPath = Path.Combine(directory, ThemePack.IconFolder, IconPack.ManifestFileName);
        if (File.Exists(iconPath))
        {
            if (!TryReadJson<IconPackManifest>(iconPath, out var icons)) return "icons/pack.json is not valid JSON.";
            if (icons?.Icons is not null && icons.Icons.Count > MaxFileCount) return "icons/pack.json has too many entries.";
            if (icons?.Fallback is not null && icons.Fallback.IconScale is < 0.1 or > 2.0)
                return "icons/pack.json has an out-of-range icon scale.";
        }

        var widgetPath = Path.Combine(directory, ThemePack.WidgetFolder, WidgetPack.ManifestFileName);
        if (File.Exists(widgetPath))
        {
            if (!TryReadJson<WidgetPackManifest>(widgetPath, out var widgets)) return "widgets/pack.json is not valid JSON.";
            if (widgets?.Tokens is { Count: > MaxFileCount } || widgets?.Dark is { Count: > MaxFileCount }
                || widgets?.Light is { Count: > MaxFileCount }) return "widgets/pack.json has too many tokens.";
        }

        return null;
    }

    private static bool TryReadJson<T>(string path, out T? value)
    {
        value = default;
        try
        {
            value = JsonSerializer.Deserialize<T>(File.ReadAllText(path), JsonOptions);
            return true;
        }
        catch (Exception ex) when (ex is IOException or UnauthorizedAccessException or JsonException or NotSupportedException)
        {
            return false;
        }
    }

    private static void ExtractSafe(ZipArchive archive, string destination)
    {
        var root = Path.GetFullPath(destination);
        var prefix = Path.TrimEndingDirectorySeparator(root) + Path.DirectorySeparatorChar;
        var count = 0;
        long total = 0;
        long actualTotal = 0;
        foreach (var entry in archive.Entries)
        {
            if (string.IsNullOrEmpty(entry.Name)) continue; // directory marker
            if (++count > MaxFileCount) throw new InvalidDataException("Too many files.");
            total += entry.Length;
            if (total > MaxUncompressedBytes || entry.Length > MaxFileBytes) throw new InvalidDataException("Theme is too large.");

            var extension = Path.GetExtension(entry.Name);
            if (!IsAllowedExtension(extension)) throw new InvalidDataException($"File type '{extension}' is not allowed.");

            var relative = entry.FullName.Replace('\\', '/');
            if (Path.IsPathRooted(relative) || relative.Split('/').Any(part => part is ".." or "."))
                throw new InvalidDataException("Unsafe path in archive.");

            var full = Path.GetFullPath(Path.Combine(root, relative));
            if (!full.StartsWith(prefix, StringComparison.OrdinalIgnoreCase))
                throw new InvalidDataException("Unsafe path in archive.");

            Directory.CreateDirectory(Path.GetDirectoryName(full)!);
            long entryWritten = 0;
            var buffer = new byte[8192];
            try
            {
                using var source = entry.Open();
                using var target = new FileStream(full, FileMode.Create, FileAccess.Write, FileShare.None);
                while (true)
                {
                    var read = source.Read(buffer, 0, buffer.Length);
                    if (read == 0) break;
                    entryWritten += read;
                    actualTotal += read;
                    if (entryWritten > MaxFileBytes || actualTotal > MaxUncompressedBytes)
                    {
                        throw new InvalidDataException("Theme is too large.");
                    }
                    target.Write(buffer, 0, read);
                }
            }
            catch
            {
                try { if (File.Exists(full)) File.Delete(full); } catch { }
                throw;
            }
        }
    }

    internal static bool IsAllowedExtension(string extension) => AllowedExtensions.Contains(extension);

    internal static string UniqueId(string root, string? name)
    {
        var baseName = Sanitize(name) ?? "theme";
        var id = baseName;
        var suffix = 2;
        while (Directory.Exists(Path.Combine(root, id))) id = baseName + "-" + suffix++;
        return id;
    }

    private static string? Sanitize(string? name)
    {
        if (string.IsNullOrWhiteSpace(name)) return null;
        var chars = name.Trim().Select(c => char.IsLetterOrDigit(c) || c is '-' or '_' ? c : '-').ToArray();
        var cleaned = new string(chars).Trim('-');
        return string.IsNullOrEmpty(cleaned) ? null : cleaned;
    }

    internal static bool IsSafeId(string id) =>
        !string.IsNullOrWhiteSpace(id)
        && id.Length <= 120
        && id.All(c => char.IsLetterOrDigit(c) || c is '-' or '_')
        && !Path.IsPathRooted(id)
        && !id.Contains("..", StringComparison.Ordinal);

    internal static void CopyDirectory(string source, string destination)
    {
        foreach (var directory in Directory.EnumerateDirectories(source, "*", SearchOption.AllDirectories))
        {
            var relative = Path.GetRelativePath(source, directory);
            Directory.CreateDirectory(Path.Combine(destination, relative));
        }

        foreach (var file in Directory.EnumerateFiles(source, "*", SearchOption.AllDirectories))
        {
            var relative = Path.GetRelativePath(source, file);
            File.Copy(file, Path.Combine(destination, relative), overwrite: true);
        }
    }
}
