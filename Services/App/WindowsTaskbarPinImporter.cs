using System.Diagnostics;
using System.IO;
using System.Reflection;
using System.Runtime.InteropServices;
using System.Text;
using System.Text.RegularExpressions;
using System.Xml.Linq;
using Microsoft.Win32;
using OpenTaskbar.Models;

namespace OpenTaskbar.Services;

public static class WindowsTaskbarPinImporter
{
    internal const string FileExplorerAumid = "Microsoft.Windows.Explorer";
    internal const string AppsFolderPrefix = "shell:AppsFolder\\";

    private const int ErrorSuccess = 0;
    private const int ErrorInsufficientBuffer = 122;
    private const uint StgmRead = 0x00000000;
    private const uint StgmReadWrite = 0x00000002;
    private const uint SlgpRawPath = 0x00000004;
    private const uint SigdnNormalDisplay = 0x00000000;
    private const uint SigdnDesktopAbsoluteParsing = 0x80028000;
    private const ushort VtBstr = 8;
    private const ushort VtLpwstr = 31;

    private static readonly Guid FolderIdAppsFolder = new("1E87508D-89C2-42F0-8A7E-645A0F50CA58");
    private static readonly Guid ShellItemIid = new("43826D1E-E718-42EE-BC55-A1E261C37BFE");
    private static readonly PropertyKey PkeyAppUserModelId = new(new Guid("9F4C2855-9F79-4B39-A8D0-E1D42DE1D5F3"), 5);
    private static readonly PropertyKey PkeyLinkTargetParsingPath = new(new Guid("B9B4B3FC-2B51-4A42-B5D8-324146AFCF25"), 2);

    private static readonly string[] FileExplorerClsidMarkers =
    [
        "{52205fd8-5dfb-447d-801a-d0b52f2e83e1}", // File Explorer taskbar shortcut
        "{20d04fe0-3aea-1069-a2d8-08002b30309d}", // This PC
        "{679f85cb-0220-4080-b29b-5540cc05aab6}", // Quick Access
        "{f874310e-b6b7-47dc-bc84-b9e6b38f5903}"  // Windows 11 Home
    ];

    private const string AppsFolderClsidPrefix = "::{4234d49b-0245-4df3-b780-3893943456e1}\\";

    private static readonly Regex PackagedAumidPattern = new(
        @"[A-Za-z0-9][A-Za-z0-9\.\-]*_[A-Za-z0-9]{13}![A-Za-z0-9][A-Za-z0-9\.\-_]*|(?<![A-Za-z0-9\.\-_])Microsoft\.Windows\.Explorer(?![A-Za-z0-9\.\-_])",
        RegexOptions.Compiled | RegexOptions.CultureInvariant);

    internal sealed record ShellLinkMetadata(
        string? TargetPath,
        string? Arguments,
        string? IconLocation,
        string? AppUserModelId,
        string? TargetParsingPath);

    internal sealed record ResolvedPin(
        string DisplayName,
        string ExecutablePath,
        string? LaunchArguments,
        string? ShortcutPath,
        string? IconLocation,
        string? AppUserModelId,
        string? DedupAumid);

    internal sealed class PinDeduplicator
    {
        private readonly HashSet<string> _seenKeys = new(StringComparer.OrdinalIgnoreCase);
        private readonly HashSet<string> _seenAumids = new(StringComparer.OrdinalIgnoreCase);
        private readonly HashSet<string> _seenNoArgExes = new(StringComparer.OrdinalIgnoreCase);

        public bool TryAddShortcut(ResolvedPin pin)
        {
            var key = BuildDedupKey(pin.ExecutablePath, pin.LaunchArguments, pin.DedupAumid);
            if (!_seenKeys.Add(key)) return false;

            if (!string.IsNullOrWhiteSpace(pin.DedupAumid))
            {
                _seenAumids.Add(pin.DedupAumid.Trim());
            }

            if (string.IsNullOrWhiteSpace(pin.LaunchArguments)
                && !pin.ExecutablePath.StartsWith(AppsFolderPrefix, StringComparison.OrdinalIgnoreCase))
            {
                _seenNoArgExes.Add(pin.ExecutablePath);
            }

            return true;
        }

        public bool TryAddTaskbandApp(string aumid, string executablePath)
        {
            var trimmedAumid = aumid.Trim();
            if (_seenAumids.Contains(trimmedAumid)) return false;

            if (!executablePath.StartsWith(AppsFolderPrefix, StringComparison.OrdinalIgnoreCase)
                && _seenNoArgExes.Contains(executablePath))
            {
                return false;
            }

            var key = BuildDedupKey(executablePath, null, trimmedAumid);
            if (!_seenKeys.Add(key)) return false;

            _seenAumids.Add(trimmedAumid);
            if (!executablePath.StartsWith(AppsFolderPrefix, StringComparison.OrdinalIgnoreCase))
            {
                _seenNoArgExes.Add(executablePath);
            }

            return true;
        }
    }

    internal static string BuildDedupKey(string executablePath, string? arguments, string? appUserModelId)
    {
        var normalizedArgs = DockApp.NormalizeArguments(arguments);
        var normalizedAumid = string.IsNullOrWhiteSpace(appUserModelId) ? string.Empty : appUserModelId.Trim();
        return $"{executablePath}\0{normalizedArgs}\0{normalizedAumid}";
    }

    private static string GetTaskbarPinsDirectory() => Path.GetFullPath(Path.Combine(
        Environment.GetFolderPath(Environment.SpecialFolder.ApplicationData),
        "Microsoft",
        "Internet Explorer",
        "Quick Launch",
        "User Pinned",
        "TaskBar"));

    public static (IReadOnlyList<DockApp> Apps, string? ErrorKey, int SkippedCount) Import()
    {
        var taskbarPinsPath = GetTaskbarPinsDirectory();
        var apps = new List<DockApp>();
        var dedup = new PinDeduplicator();
        var skippedCount = 0;

        if (!Directory.Exists(taskbarPinsPath))
        {
            ImportTaskbandPackagedPins(dedup, apps);
            return (apps, null, 0);
        }

        List<string> shortcutPaths;
        try
        {
            shortcutPaths = Directory.EnumerateFiles(taskbarPinsPath, "*", SearchOption.TopDirectoryOnly)
                .Where(path =>
                {
                    var ext = Path.GetExtension(path);
                    return string.Equals(ext, ".lnk", StringComparison.OrdinalIgnoreCase)
                        || string.Equals(ext, ".url", StringComparison.OrdinalIgnoreCase);
                })
                .OrderBy(Path.GetFileName, StringComparer.CurrentCultureIgnoreCase)
                .ToList();
        }
        catch (IOException)
        {
            ImportTaskbandPackagedPins(dedup, apps);
            return (apps, "ErrorTaskbarPinsRead", 0);
        }
        catch (UnauthorizedAccessException)
        {
            ImportTaskbandPackagedPins(dedup, apps);
            return (apps, "ErrorTaskbarPinsAccess", 0);
        }

        if (shortcutPaths.Count == 0)
        {
            ImportTaskbandPackagedPins(dedup, apps);
            return (apps, null, 0);
        }

        Type? shellType = null;
        object? shell = null;
        string? shortcutServiceError = null;
        var hasLnkFiles = shortcutPaths.Any(p => string.Equals(Path.GetExtension(p), ".lnk", StringComparison.OrdinalIgnoreCase));
        if (hasLnkFiles)
        {
            try
            {
                shellType = Type.GetTypeFromProgID("WScript.Shell");
                shell = shellType is null ? null : Activator.CreateInstance(shellType);
            }
            catch (COMException)
            {
                shortcutServiceError = "ErrorShortcutServiceImport";
            }
            catch (TargetInvocationException ex) when (ex.InnerException is COMException)
            {
                shortcutServiceError = "ErrorShortcutServiceImport";
            }

            if (shellType is null || shell is null)
            {
                shortcutServiceError ??= "ErrorShortcutServiceImport";
            }
        }

        try
        {
            foreach (var shortcutPath in shortcutPaths)
            {
                if (string.Equals(Path.GetExtension(shortcutPath), ".url", StringComparison.OrdinalIgnoreCase))
                {
                    var urlPin = TryResolveUrlShortcut(shortcutPath);
                    if (urlPin is null)
                    {
                        skippedCount++;
                        continue;
                    }

                    if (dedup.TryAddShortcut(urlPin))
                    {
                        apps.Add(ToDockApp(urlPin));
                    }
                    continue;
                }

                try
                {
                    string targetPath = string.Empty;
                    string arguments = string.Empty;
                    string iconLocation = string.Empty;

                    if (shellType is not null && shell is not null)
                    {
                        try
                        {
                            var shortcut = shellType.InvokeMember(
                                "CreateShortcut",
                                BindingFlags.InvokeMethod,
                                binder: null,
                                target: shell,
                                args: [shortcutPath]);
                            if (shortcut is not null)
                            {
                                try
                                {
                                    var shortcutType = shortcut.GetType();
                                    targetPath = ReadStringProperty(shortcutType, shortcut, "TargetPath");
                                    arguments = ReadStringProperty(shortcutType, shortcut, "Arguments");
                                    iconLocation = ReadStringProperty(shortcutType, shortcut, "IconLocation");
                                }
                                finally
                                {
                                    if (Marshal.IsComObject(shortcut))
                                    {
                                        _ = Marshal.FinalReleaseComObject(shortcut);
                                    }
                                }
                            }
                        }
                        catch (COMException)
                        {
                            // Fall through to COM IShellLinkW + IPropertyStore inspection below.
                        }
                        catch (TargetInvocationException ex) when (ex.InnerException is COMException)
                        {
                            // Fall through to COM IShellLinkW + IPropertyStore inspection below.
                        }
                    }

                    // Inspect the .lnk directly via COM IShellLinkW + IPersistFile + IPropertyStore whenever
                    // WScript.Shell has no usable executable target (File Explorer, Store/UWP, shell PIDL) or
                    // carries arguments / packaged paths (PWAs, Store desktop bridges) so we capture PKEY_AppUserModel_ID.
                    var wscriptExe = ResolveExecutablePath(targetPath, iconLocation);
                    ShellLinkMetadata? metadata = null;
                    if (wscriptExe is null
                        || !string.IsNullOrWhiteSpace(arguments)
                        || DockApp.IsSharedHostExecutable(wscriptExe)
                        || IsPackagedInstallPath(wscriptExe))
                    {
                        metadata = TryInspectShellLink(shortcutPath);
                    }

                    var resolved = ResolveShortcutEntry(
                        Path.GetFileNameWithoutExtension(shortcutPath),
                        Path.GetFullPath(shortcutPath),
                        targetPath,
                        arguments,
                        iconLocation,
                        metadata);

                    if (resolved is null)
                    {
                        skippedCount++;
                        continue;
                    }

                    // Duplicates of an already-imported pin are skipped silently and must not be reported as
                    // unreadable/unsupported shortcuts.
                    if (dedup.TryAddShortcut(resolved))
                    {
                        apps.Add(ToDockApp(resolved));
                    }
                }
                catch (COMException)
                {
                    skippedCount++;
                }
                catch (TargetInvocationException ex) when (ex.InnerException is COMException)
                {
                    skippedCount++;
                }
                catch (IOException)
                {
                    skippedCount++;
                }
                catch (UnauthorizedAccessException)
                {
                    skippedCount++;
                }
            }
        }
        catch (COMException)
        {
            ImportTaskbandPackagedPins(dedup, apps);
            return (apps, "ErrorShortcutServiceStopped", skippedCount);
        }
        catch (TargetInvocationException ex) when (ex.InnerException is COMException)
        {
            ImportTaskbandPackagedPins(dedup, apps);
            return (apps, "ErrorShortcutServiceStopped", skippedCount);
        }
        finally
        {
            if (shell is not null && Marshal.IsComObject(shell))
            {
                _ = Marshal.FinalReleaseComObject(shell);
            }
        }

        ImportTaskbandPackagedPins(dedup, apps);
        return (apps, apps.Count == 0 ? shortcutServiceError : null, skippedCount);
    }

    internal static ResolvedPin? ResolveShortcutEntry(
        string displayName,
        string shortcutPath,
        string? targetPath,
        string? arguments,
        string? iconLocation,
        ShellLinkMetadata? metadata,
        string? windowsAppsRoot = null)
    {
        var effectiveTarget = FirstNonWhiteSpace(targetPath, metadata?.TargetPath);
        var effectiveArgs = FirstNonWhiteSpace(arguments, metadata?.Arguments);
        var effectiveIcon = FirstNonWhiteSpace(iconLocation, metadata?.IconLocation);
        var parsingPath = NormalizeNullIfBlank(metadata?.TargetParsingPath);
        var rawAumid = NormalizeNullIfBlank(metadata?.AppUserModelId) ?? TryExtractAppsFolderAumid(parsingPath);

        if (IsFileExplorerPin(effectiveTarget, effectiveIcon, rawAumid, parsingPath)
            && string.IsNullOrWhiteSpace(effectiveArgs))
        {
            var explorerExe = GetWindowsExplorerPath();
            if (explorerExe is not null)
            {
                var name = !string.IsNullOrWhiteSpace(displayName)
                    ? displayName
                    : (windowsAppsRoot is null ? TryGetAppsFolderDisplayName(FileExplorerAumid) : null) ?? "File Explorer";
                return new ResolvedPin(
                    name,
                    explorerExe,
                    LaunchArguments: null,
                    ShortcutPath: shortcutPath,
                    IconLocation: effectiveIcon,
                    AppUserModelId: null,
                    DedupAumid: FileExplorerAumid);
            }
        }

        var executablePath = ResolveExecutablePath(effectiveTarget ?? string.Empty, effectiveIcon ?? string.Empty);
        if (executablePath is null && !string.IsNullOrWhiteSpace(parsingPath))
        {
            executablePath = ResolveExecutablePath(parsingPath, string.Empty);
        }

        if (executablePath is not null)
        {
            if (IsExplorerExecutable(executablePath) && string.IsNullOrWhiteSpace(effectiveArgs))
            {
                return new ResolvedPin(
                    string.IsNullOrWhiteSpace(displayName) ? "File Explorer" : displayName,
                    executablePath,
                    LaunchArguments: null,
                    ShortcutPath: shortcutPath,
                    IconLocation: effectiveIcon,
                    AppUserModelId: null,
                    DedupAumid: FileExplorerAumid);
            }

            var packagedAumid = !string.IsNullOrWhiteSpace(rawAumid) && rawAumid.Contains('!')
                ? rawAumid
                : ApplicationIconProvider.TryResolvePackagedAppUserModelId(executablePath);

            if ((DockApp.IsSharedHostExecutable(executablePath)
                    || DockApp.IsSystemPackagedInstallPath(executablePath))
                && !string.IsNullOrWhiteSpace(packagedAumid))
            {
                var resolvedHosted = TryResolvePackagedApp(packagedAumid, windowsAppsRoot);
                var hostedTarget = resolvedHosted?.ExecutablePath ?? $"{AppsFolderPrefix}{packagedAumid}";
                var hostedName = !string.IsNullOrWhiteSpace(displayName)
                    ? displayName
                    : resolvedHosted?.DisplayName ?? ExtractFallbackDisplayName(packagedAumid);
                return new ResolvedPin(
                    hostedName,
                    hostedTarget,
                    LaunchArguments: effectiveArgs,
                    ShortcutPath: shortcutPath,
                    IconLocation: effectiveIcon,
                    AppUserModelId: packagedAumid,
                    DedupAumid: packagedAumid);
            }

            // Preserve explicit AUMIDs for packaged Store apps, argument-bearing PWAs (msedge_proxy.exe /
            // chrome_proxy.exe), and shared UWP hosts so icons, launch, and running-window matching stay exact.
            var storedAumid = packagedAumid
                ?? (!string.IsNullOrWhiteSpace(rawAumid)
                    && (!string.IsNullOrWhiteSpace(effectiveArgs) || DockApp.IsSharedHostExecutable(executablePath))
                    ? rawAumid
                    : null);

            return new ResolvedPin(
                string.IsNullOrWhiteSpace(displayName) ? Path.GetFileNameWithoutExtension(executablePath) : displayName,
                executablePath,
                LaunchArguments: effectiveArgs,
                ShortcutPath: shortcutPath,
                IconLocation: effectiveIcon,
                AppUserModelId: storedAumid,
                DedupAumid: storedAumid ?? rawAumid);
        }

        if (!string.IsNullOrWhiteSpace(rawAumid))
        {
            var resolvedPackaged = TryResolvePackagedApp(rawAumid, windowsAppsRoot);
            if (resolvedPackaged is { } info)
            {
                var isExplorer = string.Equals(rawAumid, FileExplorerAumid, StringComparison.OrdinalIgnoreCase);
                var name = !string.IsNullOrWhiteSpace(displayName) ? displayName : info.DisplayName;
                return new ResolvedPin(
                    name,
                    info.ExecutablePath,
                    LaunchArguments: effectiveArgs,
                    ShortcutPath: shortcutPath,
                    IconLocation: effectiveIcon,
                    AppUserModelId: isExplorer ? null : rawAumid,
                    DedupAumid: rawAumid);
            }
        }

        return null;
    }

    internal static ResolvedPin? TryResolveUrlShortcut(string urlPath)
    {
        if (string.IsNullOrWhiteSpace(urlPath) || !File.Exists(urlPath)) return null;

        try
        {
            string? url = null;
            string? iconFile = null;
            string? iconIndex = null;
            var inInternetShortcut = false;

            foreach (var rawLine in File.ReadLines(urlPath))
            {
                var line = rawLine.Trim();
                if (line.Length == 0 || line.StartsWith(';') || line.StartsWith('#')) continue;
                if (line.StartsWith('[') && line.EndsWith(']'))
                {
                    inInternetShortcut = string.Equals(line, "[InternetShortcut]", StringComparison.OrdinalIgnoreCase);
                    continue;
                }

                if (!inInternetShortcut) continue;
                var eq = line.IndexOf('=');
                if (eq <= 0) continue;
                var key = line[..eq].Trim();
                var value = line[(eq + 1)..].Trim();
                if (string.Equals(key, "URL", StringComparison.OrdinalIgnoreCase))
                    url = value;
                else if (string.Equals(key, "IconFile", StringComparison.OrdinalIgnoreCase))
                    iconFile = value;
                else if (string.Equals(key, "IconIndex", StringComparison.OrdinalIgnoreCase))
                    iconIndex = value;
            }

            if (string.IsNullOrWhiteSpace(url) || !Uri.TryCreate(url, UriKind.Absolute, out _))
                return null;

            string? iconLocation = null;
            if (!string.IsNullOrWhiteSpace(iconFile))
            {
                iconLocation = !string.IsNullOrWhiteSpace(iconIndex)
                    ? $"{iconFile},{iconIndex}"
                    : iconFile;
            }

            var fullPath = Path.GetFullPath(urlPath);
            var name = Path.GetFileNameWithoutExtension(fullPath);
            if (string.IsNullOrWhiteSpace(name)) name = url;

            return new ResolvedPin(
                name,
                fullPath,
                LaunchArguments: null,
                ShortcutPath: fullPath,
                IconLocation: iconLocation,
                AppUserModelId: null,
                DedupAumid: null);
        }
        catch (Exception ex) when (ex is IOException or UnauthorizedAccessException or ArgumentException)
        {
            return null;
        }
    }

    private static DockApp ToDockApp(ResolvedPin pin) => new(pin.DisplayName, pin.ExecutablePath)
    {
        LaunchArguments = pin.LaunchArguments,
        ShortcutPath = pin.ShortcutPath,
        IconLocation = pin.IconLocation,
        AppUserModelId = pin.AppUserModelId
    };

    /// <summary>
    /// Windows 11 stores Microsoft Store taskbar pins in the Explorer Taskband registry blob rather than
    /// creating .lnk shortcuts under User Pinned\TaskBar. Import those packaged AUMIDs so Store pins stay
    /// visible on the dock even when their windows are closed or minimized to the tray.
    /// </summary>
    private static void ImportTaskbandPackagedPins(PinDeduplicator dedup, List<DockApp> apps)
    {
        byte[]? favorites;
        try
        {
            using var key = Registry.CurrentUser.OpenSubKey(@"Software\Microsoft\Windows\CurrentVersion\Explorer\Taskband");
            favorites = key?.GetValue("Favorites") as byte[];
        }
        catch (Exception ex) when (ex is System.Security.SecurityException or UnauthorizedAccessException or IOException)
        {
            return;
        }

        if (favorites is null || favorites.Length < 16) return;

        foreach (var aumid in ExtractAumidsFromTaskbandBytes(favorites))
        {
            var resolved = TryResolvePackagedApp(aumid);
            if (resolved is not { } info) continue;
            if (!dedup.TryAddTaskbandApp(aumid, info.ExecutablePath)) continue;

            var isExplorer = string.Equals(aumid, FileExplorerAumid, StringComparison.OrdinalIgnoreCase);
            apps.Add(new DockApp(info.DisplayName, info.ExecutablePath)
            {
                AppUserModelId = isExplorer ? null : aumid
            });
        }
    }

    internal static IReadOnlyList<string> ExtractAumidsFromTaskbandBytes(byte[] bytes)
    {
        if (bytes.Length < 4) return [];
        var seen = new HashSet<string>(StringComparer.OrdinalIgnoreCase);
        var result = new List<string>();
        // PIDL structures inside Taskband\Favorites are not guaranteed to be 2-byte aligned,
        // so scan both even (0) and odd (1) UTF-16LE byte offsets.
        for (var offset = 0; offset <= 1; offset++)
        {
            var count = bytes.Length - offset;
            count -= count % 2;
            if (count <= 0) continue;
            var text = Encoding.Unicode.GetString(bytes, offset, count);
            foreach (Match match in PackagedAumidPattern.Matches(text))
            {
                if (seen.Add(match.Value)) result.Add(match.Value);
            }
        }
        return result;
    }

    internal static (string DisplayName, string ExecutablePath)? TryResolvePackagedApp(string aumid, string? windowsAppsRoot = null)
    {
        if (string.IsNullOrWhiteSpace(aumid)) return null;
        var trimmedAumid = aumid.Trim();

        if (string.Equals(trimmedAumid, FileExplorerAumid, StringComparison.OrdinalIgnoreCase))
        {
            var explorerPath = GetWindowsExplorerPath();
            if (explorerPath is null) return null;
            var explorerName = (windowsAppsRoot is null ? TryGetAppsFolderDisplayName(FileExplorerAumid) : null)
                ?? "File Explorer";
            return (explorerName, explorerPath);
        }

        var bang = trimmedAumid.IndexOf('!');
        if (bang <= 0 || bang >= trimmedAumid.Length - 1) return null;
        var familyName = trimmedAumid[..bang];
        var appId = trimmedAumid[(bang + 1)..];
        var underscore = familyName.LastIndexOf('_');
        if (underscore <= 0 || underscore >= familyName.Length - 1) return null;
        var packageName = familyName[..underscore];
        var publisherId = familyName[(underscore + 1)..];

        var packageDirs = GetPackageDirectories(familyName, packageName, publisherId, windowsAppsRoot);
        foreach (var packageDir in packageDirs)
        {
            var manifestPath = Path.Combine(packageDir, "AppxManifest.xml");
            if (!File.Exists(manifestPath)) continue;
            try
            {
                var doc = XDocument.Load(manifestPath);
                var appElem = doc.Descendants()
                    .FirstOrDefault(e => string.Equals(e.Name.LocalName, "Application", StringComparison.Ordinal)
                        && string.Equals(e.Attribute("Id")?.Value?.Trim(), appId, StringComparison.OrdinalIgnoreCase));
                if (appElem is null) continue;

                string? readableExe = null;
                var relExe = appElem.Attribute("Executable")?.Value?.Trim();
                var entryPoint = appElem.Attribute("EntryPoint")?.Value?.Trim();
                // Pure UWP/AppContainer apps (e.g. Windows Settings with EntryPoint="AppObject.EntryPoint")
                // cannot be launched directly via their raw .exe and embed no Win32 icon resource; they must
                // route through shell:AppsFolder\<AUMID> for both icon loading and activation.
                if (!IsAppContainerEntryPoint(entryPoint)
                    && !string.IsNullOrWhiteSpace(relExe)
                    && !relExe.Contains('$'))
                {
                    try
                    {
                        var candidateExe = Path.GetFullPath(Path.Combine(packageDir, relExe.Replace('/', '\\')));
                        if (IsReadableFile(candidateExe))
                        {
                            readableExe = candidateExe;
                        }
                    }
                    catch (Exception ex) when (ex is ArgumentException or NotSupportedException or PathTooLongException)
                    {
                        readableExe = null;
                    }
                }

                var visualDisplayName = appElem.Descendants()
                    .FirstOrDefault(e => string.Equals(e.Name.LocalName, "VisualElements", StringComparison.Ordinal))
                    ?.Attribute("DisplayName")?.Value?.Trim();
                var manifestDisplayName = doc.Descendants()
                    .FirstOrDefault(e => string.Equals(e.Name.LocalName, "DisplayName", StringComparison.Ordinal))
                    ?.Value?.Trim();

                string? displayName = null;
                if (!IsMissingOrResource(visualDisplayName))
                {
                    displayName = visualDisplayName;
                }
                else if (IsResourceReference(visualDisplayName) && windowsAppsRoot is null)
                {
                    displayName = TryGetAppsFolderDisplayName(trimmedAumid);
                }

                if (IsMissingOrResource(displayName) && !IsMissingOrResource(manifestDisplayName))
                {
                    displayName = manifestDisplayName;
                }

                if (IsMissingOrResource(displayName) && windowsAppsRoot is null)
                {
                    displayName = TryGetAppsFolderDisplayName(trimmedAumid);
                }

                if (IsMissingOrResource(displayName))
                {
                    displayName = readableExe is not null
                        ? Path.GetFileNameWithoutExtension(readableExe)
                        : ExtractFallbackDisplayName(packageName, appId);
                }

                // When AppxManifest.xml has no readable executable (pure UWP/hosted apps or ACL-protected
                // WindowsApps targets), emit shell:AppsFolder\<AUMID> so both icon loading and activation
                // route through Windows Shell cleanly.
                var resolvedPath = readableExe ?? $"{AppsFolderPrefix}{trimmedAumid}";
                return (displayName!, resolvedPath);
            }
            catch (Exception ex) when (ex is IOException or UnauthorizedAccessException or System.Xml.XmlException)
            {
                continue;
            }
        }

        // If the package directory or manifest could not be read (e.g. protected system package or virtual
        // AppsFolder entry), ask Windows Shell directly via FOLDERID_AppsFolder.
        if (windowsAppsRoot is null && TryGetAppsFolderDisplayName(trimmedAumid) is { } shellDisplayName)
        {
            return (shellDisplayName, $"{AppsFolderPrefix}{trimmedAumid}");
        }

        return null;
    }

    private static IReadOnlyList<string> GetPackageDirectories(
        string familyName,
        string packageName,
        string publisherId,
        string? windowsAppsRoot)
    {
        if (windowsAppsRoot is not null)
        {
            return EnumeratePackageDirectoriesFromRoot(windowsAppsRoot, packageName, publisherId);
        }

        var fromApi = GetPackageDirectoriesByFamily(familyName);
        if (fromApi.Count > 0) return fromApi;

        var defaultRoot = Path.Combine(Environment.GetFolderPath(Environment.SpecialFolder.ProgramFiles), "WindowsApps");
        return EnumeratePackageDirectoriesFromRoot(defaultRoot, packageName, publisherId);
    }

    private static IReadOnlyList<string> EnumeratePackageDirectoriesFromRoot(
        string root,
        string packageName,
        string publisherId)
    {
        if (!Directory.Exists(root)) return [];
        try
        {
            return Directory.EnumerateDirectories(root, $"{packageName}_*_{publisherId}")
                .OrderByDescending(Path.GetFileName, StringComparer.OrdinalIgnoreCase)
                .ToList();
        }
        catch (Exception ex) when (ex is IOException or UnauthorizedAccessException)
        {
            return [];
        }
    }

    private static IReadOnlyList<string> GetPackageDirectoriesByFamily(string familyName)
    {
        try
        {
            uint count = 0;
            uint bufferLength = 0;
            var res = GetPackagesByPackageFamily(familyName, ref count, nint.Zero, ref bufferLength, nint.Zero);
            if (res != ErrorInsufficientBuffer || count == 0 || bufferLength == 0)
            {
                return [];
            }

            var ptrsMem = Marshal.AllocHGlobal(checked((int)count * IntPtr.Size));
            var bufMem = Marshal.AllocHGlobal(checked((int)bufferLength * sizeof(char)));
            try
            {
                res = GetPackagesByPackageFamily(familyName, ref count, ptrsMem, ref bufferLength, bufMem);
                if (res != ErrorSuccess || count == 0) return [];

                var fullNames = new List<string>((int)count);
                for (var i = 0; i < count; i++)
                {
                    var strPtr = Marshal.ReadIntPtr(ptrsMem, i * IntPtr.Size);
                    if (strPtr != 0 && Marshal.PtrToStringUni(strPtr) is { Length: > 0 } fullName)
                    {
                        fullNames.Add(fullName);
                    }
                }

                fullNames.Sort(StringComparer.OrdinalIgnoreCase);
                fullNames.Reverse();

                var directories = new List<string>(fullNames.Count);
                foreach (var fullName in fullNames)
                {
                    if (TryGetPackagePathByFullName(fullName) is { } path && Directory.Exists(path))
                    {
                        directories.Add(path);
                    }
                }
                return directories;
            }
            finally
            {
                Marshal.FreeHGlobal(ptrsMem);
                Marshal.FreeHGlobal(bufMem);
            }
        }
        catch (Exception ex) when (ex is EntryPointNotFoundException or DllNotFoundException or OverflowException)
        {
            return [];
        }
    }

    private static string? TryGetPackagePathByFullName(string fullName)
    {
        uint pathLength = 0;
        var res = GetPackagePathByFullName(fullName, ref pathLength, null);
        if (res != ErrorInsufficientBuffer || pathLength == 0) return null;

        var buffer = new char[pathLength];
        res = GetPackagePathByFullName(fullName, ref pathLength, buffer);
        if (res != ErrorSuccess || pathLength == 0) return null;

        var charCount = (int)Math.Min(pathLength, (uint)buffer.Length);
        if (charCount > 0 && buffer[charCount - 1] == '\0') charCount--;
        return charCount > 0 ? new string(buffer, 0, charCount) : null;
    }

    internal static string? TryGetAppsFolderDisplayName(string aumid)
    {
        if (string.IsNullOrWhiteSpace(aumid)) return null;

        IShellItem? item = null;
        try
        {
            var folder = FolderIdAppsFolder;
            var iid = ShellItemIid;
            if (SHCreateItemInKnownFolder(ref folder, 0, aumid.Trim(), ref iid, out item) != 0 || item is null)
            {
                return null;
            }

            if (item.GetDisplayName(SigdnNormalDisplay, out var namePtr) != 0 || namePtr == 0)
            {
                return null;
            }

            try
            {
                var name = Marshal.PtrToStringUni(namePtr)?.Trim();
                return string.IsNullOrWhiteSpace(name) ? null : name;
            }
            finally
            {
                CoTaskMemFree(namePtr);
            }
        }
        catch (Exception ex) when (ex is COMException or EntryPointNotFoundException or DllNotFoundException)
        {
            return null;
        }
        finally
        {
            if (item is not null && Marshal.IsComObject(item))
            {
                _ = Marshal.FinalReleaseComObject(item);
            }
        }
    }

    internal static ShellLinkMetadata? TryInspectShellLink(string shortcutPath)
    {
        if (string.IsNullOrWhiteSpace(shortcutPath) || !File.Exists(shortcutPath)) return null;

        object? comObj = null;
        try
        {
            comObj = new ShellLinkCoClass();
            if (comObj is not IPersistFile persistFile || comObj is not IShellLinkW shellLink)
            {
                return null;
            }

            if (persistFile.Load(shortcutPath, StgmRead) < 0)
            {
                return null;
            }

            var pathBuf = new StringBuilder(1024);
            var targetPath = string.Empty;
            if (shellLink.GetPath(pathBuf, pathBuf.Capacity, nint.Zero, 0) == 0 && pathBuf.Length > 0)
            {
                targetPath = pathBuf.ToString().Trim();
            }
            else
            {
                pathBuf.Clear();
                if (shellLink.GetPath(pathBuf, pathBuf.Capacity, nint.Zero, SlgpRawPath) == 0 && pathBuf.Length > 0)
                {
                    targetPath = pathBuf.ToString().Trim();
                }
            }

            var argsBuf = new StringBuilder(2048);
            var arguments = shellLink.GetArguments(argsBuf, argsBuf.Capacity) == 0
                ? argsBuf.ToString().Trim()
                : string.Empty;

            var iconBuf = new StringBuilder(1024);
            string? iconLocation = null;
            if (shellLink.GetIconLocation(iconBuf, iconBuf.Capacity, out var iconIndex) == 0 && iconBuf.Length > 0)
            {
                iconLocation = $"{iconBuf},{iconIndex}";
            }

            string? appUserModelId = null;
            string? targetParsingPath = null;
            if (comObj is IPropertyStore propertyStore)
            {
                appUserModelId = ReadPropertyString(propertyStore, PkeyAppUserModelId);
                targetParsingPath = ReadPropertyString(propertyStore, PkeyLinkTargetParsingPath);
            }

            if (string.IsNullOrWhiteSpace(targetParsingPath) && shellLink.GetIDList(out var pidl) == 0 && pidl != 0)
            {
                try
                {
                    if (SHGetNameFromIDList(pidl, SigdnDesktopAbsoluteParsing, out var parsingPtr) == 0 && parsingPtr != 0)
                    {
                        try
                        {
                            targetParsingPath = Marshal.PtrToStringUni(parsingPtr)?.Trim();
                        }
                        finally
                        {
                            CoTaskMemFree(parsingPtr);
                        }
                    }
                }
                finally
                {
                    CoTaskMemFree(pidl);
                }
            }

            return new ShellLinkMetadata(
                NormalizeNullIfBlank(targetPath),
                NormalizeNullIfBlank(arguments),
                NormalizeNullIfBlank(iconLocation),
                NormalizeNullIfBlank(appUserModelId),
                NormalizeNullIfBlank(targetParsingPath));
        }
        catch (Exception ex) when (ex is COMException or InvalidCastException or IOException or UnauthorizedAccessException)
        {
            return null;
        }
        finally
        {
            if (comObj is not null && Marshal.IsComObject(comObj))
            {
                _ = Marshal.FinalReleaseComObject(comObj);
            }
        }
    }

    private static string? ReadPropertyString(IPropertyStore store, PropertyKey key)
    {
        var propKey = key;
        if (store.GetValue(ref propKey, out var value) < 0) return null;
        try
        {
            if (value.Type is VtLpwstr or VtBstr && value.Pointer != 0)
            {
                var str = Marshal.PtrToStringUni(value.Pointer)?.Trim();
                return string.IsNullOrWhiteSpace(str) ? null : str;
            }
            return null;
        }
        finally
        {
            _ = PropVariantClear(ref value);
        }
    }

    public static (string? ShortcutPath, string? Error) AddPin(
        string appName,
        string executablePath,
        string? launchArguments = null,
        string? iconLocation = null,
        string? appUserModelId = null)
    {
        var pinDirectory = GetTaskbarPinsDirectory();
        try
        {
            Directory.CreateDirectory(pinDirectory);
        }
        catch (Exception ex) when (ex is IOException or UnauthorizedAccessException)
        {
            return (null, "ErrorTaskbarPinFolderCreate");
        }

        var safeName = BuildSafePinFileName(appName, executablePath);
        var shortcutPath = Path.Combine(pinDirectory, safeName + ".lnk");

        // Packaged UWP/Store apps without a launchable filesystem .exe are pinned as AppsFolder shell links
        // with PKEY_AppUserModel_ID stamped so Windows and OpenTaskbar both resolve them cleanly.
        var isAppsFolderTarget = executablePath.StartsWith(AppsFolderPrefix, StringComparison.OrdinalIgnoreCase);
        var effectiveAumid = !string.IsNullOrWhiteSpace(appUserModelId)
            ? appUserModelId.Trim()
            : (isAppsFolderTarget
                ? executablePath[AppsFolderPrefix.Length..].Trim()
                : ApplicationIconProvider.TryResolvePackagedAppUserModelId(executablePath));

        if ((isAppsFolderTarget
                || DockApp.IsSharedHostExecutable(executablePath)
                || DockApp.IsSystemPackagedInstallPath(executablePath))
            && !string.IsNullOrWhiteSpace(effectiveAumid))
        {
            return CreatePackagedShellLink(shortcutPath, effectiveAumid, iconLocation)
                ? (shortcutPath, null)
                : (null, "ErrorTaskbarPin");
        }

        Type? shellType;
        object? shell;
        try
        {
            shellType = Type.GetTypeFromProgID("WScript.Shell");
            shell = shellType is null ? null : Activator.CreateInstance(shellType);
        }
        catch (COMException)
        {
            return (null, "ErrorShortcutServicePin");
        }
        catch (TargetInvocationException ex) when (ex.InnerException is COMException)
        {
            return (null, "ErrorShortcutServicePin");
        }

        if (shellType is null || shell is null)
        {
            return (null, "ErrorShortcutServicePin");
        }

        try
        {
            var shortcut = shellType.InvokeMember(
                "CreateShortcut",
                BindingFlags.InvokeMethod,
                binder: null,
                target: shell,
                args: [shortcutPath]);

            if (shortcut is null)
            {
                return (null, "ErrorShortcutCreate");
            }

            try
            {
                var shortcutType = shortcut.GetType();
                shortcutType.InvokeMember("TargetPath", BindingFlags.SetProperty, null, shortcut, [executablePath]);
                var workingDir = Path.GetDirectoryName(executablePath);
                if (!string.IsNullOrEmpty(workingDir))
                {
                    shortcutType.InvokeMember("WorkingDirectory", BindingFlags.SetProperty, null, shortcut, [workingDir]);
                }
                if (!string.IsNullOrWhiteSpace(launchArguments))
                {
                    shortcutType.InvokeMember("Arguments", BindingFlags.SetProperty, null, shortcut, [launchArguments]);
                }
                if (!string.IsNullOrWhiteSpace(iconLocation))
                {
                    shortcutType.InvokeMember("IconLocation", BindingFlags.SetProperty, null, shortcut, [iconLocation]);
                }
                shortcutType.InvokeMember("Save", BindingFlags.InvokeMethod, null, shortcut, null);
            }
            finally
            {
                if (Marshal.IsComObject(shortcut))
                {
                    _ = Marshal.FinalReleaseComObject(shortcut);
                }
            }

            if (!string.IsNullOrWhiteSpace(effectiveAumid))
            {
                TryWriteShortcutAppUserModelId(shortcutPath, effectiveAumid);
            }

            return (shortcutPath, null);
        }
        catch (Exception ex) when (ex is COMException or TargetInvocationException or IOException or UnauthorizedAccessException)
        {
            return (null, "ErrorTaskbarPin");
        }
        finally
        {
            if (Marshal.IsComObject(shell))
            {
                _ = Marshal.FinalReleaseComObject(shell);
            }
        }
    }

    /// <summary>
    /// Pins a user-selected <c>.exe</c>, <c>.lnk</c>, or <c>.url</c> file into Windows Taskbar pins and
    /// returns the corresponding <see cref="DockApp"/> ready for the dock.
    /// </summary>
    public static (DockApp? App, string? Error) PinFromFile(string filePath, Func<DockApp, bool>? isAlreadyPinned = null)
    {
        if (string.IsNullOrWhiteSpace(filePath) || !File.Exists(filePath))
        {
            return (null, "ErrorPinApplicationUnsupported");
        }

        var fullPath = Path.GetFullPath(filePath);
        var ext = Path.GetExtension(fullPath);

        if (string.Equals(ext, ".exe", StringComparison.OrdinalIgnoreCase))
        {
            string? description = null;
            try
            {
                description = FileVersionInfo.GetVersionInfo(fullPath).FileDescription?.Trim();
            }
            catch (Exception ex) when (ex is IOException or UnauthorizedAccessException or FileNotFoundException)
            {
                description = null;
            }

            var name = !string.IsNullOrWhiteSpace(description)
                ? description
                : Path.GetFileNameWithoutExtension(fullPath);
            var aumid = ApplicationIconProvider.TryResolvePackagedAppUserModelId(fullPath);
            var targetPath = fullPath;
            if (!string.IsNullOrWhiteSpace(aumid)
                && (DockApp.IsSystemPackagedInstallPath(fullPath)
                    || (TryResolvePackagedApp(aumid) is { ExecutablePath: var resolvedTarget }
                        && resolvedTarget.StartsWith(AppsFolderPrefix, StringComparison.OrdinalIgnoreCase))))
            {
                targetPath = $"{AppsFolderPrefix}{aumid}";
            }
            var candidate = new DockApp(name, targetPath) { AppUserModelId = aumid };
            if (isAlreadyPinned?.Invoke(candidate) == true)
            {
                return (candidate, null);
            }

            var (shortcutPath, error) = AddPin(name, targetPath, launchArguments: null, iconLocation: null, appUserModelId: aumid);
            if (error is not null || shortcutPath is null)
            {
                return (null, error ?? "ErrorAppPinFailed");
            }

            return (new DockApp(name, targetPath)
            {
                ShortcutPath = shortcutPath,
                AppUserModelId = aumid
            }, null);
        }

        if (string.Equals(ext, ".lnk", StringComparison.OrdinalIgnoreCase))
        {
            var resolved = ResolveShortcutFile(fullPath);
            if (resolved is null)
            {
                return (null, "ErrorPinApplicationUnsupported");
            }

            var candidate = ToDockApp(resolved);
            if (isAlreadyPinned?.Invoke(candidate) == true)
            {
                return (candidate, null);
            }

            var (pinnedPath, copyError) = CopyShortcutToTaskbarPins(fullPath, resolved.DisplayName, ".lnk");
            if (copyError is not null || pinnedPath is null)
            {
                return (null, copyError ?? "ErrorAppPinFailed");
            }

            return (ToDockApp(resolved with { ShortcutPath = pinnedPath }), null);
        }

        if (string.Equals(ext, ".url", StringComparison.OrdinalIgnoreCase))
        {
            var resolved = TryResolveUrlShortcut(fullPath);
            if (resolved is null)
            {
                return (null, "ErrorPinApplicationUnsupported");
            }

            var candidate = ToDockApp(resolved);
            if (isAlreadyPinned?.Invoke(candidate) == true)
            {
                return (candidate, null);
            }

            var (pinnedPath, copyError) = CopyShortcutToTaskbarPins(fullPath, resolved.DisplayName, ".url");
            if (copyError is not null || pinnedPath is null)
            {
                return (null, copyError ?? "ErrorAppPinFailed");
            }

            return (ToDockApp(resolved with { ExecutablePath = pinnedPath, ShortcutPath = pinnedPath }), null);
        }

        return (null, "ErrorPinApplicationUnsupported");
    }

    private static ResolvedPin? ResolveShortcutFile(string shortcutPath)
    {
        string targetPath = string.Empty;
        string arguments = string.Empty;
        string iconLocation = string.Empty;

        try
        {
            var shellType = Type.GetTypeFromProgID("WScript.Shell");
            var shell = shellType is null ? null : Activator.CreateInstance(shellType);
            if (shellType is not null && shell is not null)
            {
                try
                {
                    var shortcut = shellType.InvokeMember(
                        "CreateShortcut",
                        BindingFlags.InvokeMethod,
                        binder: null,
                        target: shell,
                        args: [shortcutPath]);
                    if (shortcut is not null)
                    {
                        try
                        {
                            var shortcutType = shortcut.GetType();
                            targetPath = ReadStringProperty(shortcutType, shortcut, "TargetPath");
                            arguments = ReadStringProperty(shortcutType, shortcut, "Arguments");
                            iconLocation = ReadStringProperty(shortcutType, shortcut, "IconLocation");
                        }
                        finally
                        {
                            if (Marshal.IsComObject(shortcut))
                            {
                                _ = Marshal.FinalReleaseComObject(shortcut);
                            }
                        }
                    }
                }
                finally
                {
                    if (Marshal.IsComObject(shell))
                    {
                        _ = Marshal.FinalReleaseComObject(shell);
                    }
                }
            }
        }
        catch (Exception ex) when (ex is COMException or TargetInvocationException or IOException or UnauthorizedAccessException)
        {
            // Fall through to COM IShellLinkW inspection below.
        }

        var metadata = TryInspectShellLink(shortcutPath);
        return ResolveShortcutEntry(
            Path.GetFileNameWithoutExtension(shortcutPath),
            Path.GetFullPath(shortcutPath),
            targetPath,
            arguments,
            iconLocation,
            metadata);
    }

    private static (string? ShortcutPath, string? Error) CopyShortcutToTaskbarPins(string sourcePath, string displayName, string extension)
    {
        var pinDirectory = GetTaskbarPinsDirectory();
        try
        {
            Directory.CreateDirectory(pinDirectory);
        }
        catch (Exception ex) when (ex is IOException or UnauthorizedAccessException)
        {
            return (null, "ErrorTaskbarPinFolderCreate");
        }

        var fullSource = Path.GetFullPath(sourcePath);
        if (string.Equals(Path.GetDirectoryName(fullSource), pinDirectory, StringComparison.OrdinalIgnoreCase))
        {
            return (fullSource, null);
        }

        var safeName = BuildSafePinFileName(displayName, fullSource);
        var destination = Path.Combine(pinDirectory, safeName + extension);
        var suffix = 2;
        while (File.Exists(destination))
        {
            destination = Path.Combine(pinDirectory, $"{safeName} ({suffix++}){extension}");
        }

        try
        {
            File.Copy(fullSource, destination, overwrite: false);
            return (destination, null);
        }
        catch (Exception ex) when (ex is IOException or UnauthorizedAccessException)
        {
            return (null, "ErrorTaskbarPin");
        }
    }

    private static bool CreatePackagedShellLink(string shortcutPath, string aumid, string? iconLocation)
    {
        object? comObj = null;
        IShellItem? shellItem = null;
        nint pidl = 0;
        try
        {
            comObj = new ShellLinkCoClass();
            if (comObj is not IShellLinkW shellLink || comObj is not IPersistFile persistFile)
            {
                return false;
            }

            var folder = FolderIdAppsFolder;
            var iid = ShellItemIid;
            if (SHCreateItemInKnownFolder(ref folder, 0, aumid, ref iid, out shellItem) == 0
                && shellItem is not null
                && SHGetIDListFromObject(shellItem, out pidl) == 0
                && pidl != 0)
            {
                _ = shellLink.SetIDList(pidl);
            }

            if (ApplicationIconProvider.TryParseIconLocation(iconLocation, out var iconPath, out var iconIndex))
            {
                _ = shellLink.SetIconLocation(iconPath, iconIndex);
            }

            if (comObj is IPropertyStore store)
            {
                WritePropertyString(store, PkeyAppUserModelId, aumid);
            }

            return persistFile.Save(shortcutPath, true) >= 0;
        }
        catch (Exception ex) when (ex is COMException or InvalidCastException or IOException or UnauthorizedAccessException)
        {
            return false;
        }
        finally
        {
            if (pidl != 0) CoTaskMemFree(pidl);
            if (shellItem is not null && Marshal.IsComObject(shellItem))
            {
                _ = Marshal.FinalReleaseComObject(shellItem);
            }
            if (comObj is not null && Marshal.IsComObject(comObj))
            {
                _ = Marshal.FinalReleaseComObject(comObj);
            }
        }
    }

    private static void TryWriteShortcutAppUserModelId(string shortcutPath, string aumid)
    {
        object? comObj = null;
        try
        {
            comObj = new ShellLinkCoClass();
            if (comObj is not IPersistFile persistFile || comObj is not IPropertyStore store)
            {
                return;
            }

            if (persistFile.Load(shortcutPath, StgmReadWrite) < 0) return;
            WritePropertyString(store, PkeyAppUserModelId, aumid);
            _ = persistFile.Save(shortcutPath, true);
        }
        catch (Exception ex) when (ex is COMException or InvalidCastException or IOException or UnauthorizedAccessException)
        {
            // Non-fatal: the shortcut itself was already created.
        }
        finally
        {
            if (comObj is not null && Marshal.IsComObject(comObj))
            {
                _ = Marshal.FinalReleaseComObject(comObj);
            }
        }
    }

    private static void WritePropertyString(IPropertyStore store, PropertyKey key, string value)
    {
        var propKey = key;
        var pv = new PropVariant
        {
            Type = VtLpwstr,
            Pointer = Marshal.StringToCoTaskMemUni(value)
        };
        try
        {
            if (store.SetValue(ref propKey, ref pv) >= 0)
            {
                _ = store.Commit();
            }
        }
        finally
        {
            _ = PropVariantClear(ref pv);
        }
    }

    public static string? RemovePin(string shortcutPath)
    {
        var pinDirectory = GetTaskbarPinsDirectory();
        var fullPath = Path.GetFullPath(shortcutPath);
        var ext = Path.GetExtension(fullPath);
        if (!string.Equals(Path.GetDirectoryName(fullPath), pinDirectory, StringComparison.OrdinalIgnoreCase)
            || (!string.Equals(ext, ".lnk", StringComparison.OrdinalIgnoreCase)
                && !string.Equals(ext, ".url", StringComparison.OrdinalIgnoreCase)))
        {
            return "ErrorTaskbarPinPath";
        }

        try
        {
            File.Delete(fullPath);
            return null;
        }
        catch (IOException)
        {
            return "ErrorTaskbarUnpin";
        }
        catch (UnauthorizedAccessException)
        {
            return "ErrorTaskbarUnpinAccess";
        }
    }

    private static string BuildSafePinFileName(string appName, string fallbackPath)
    {
        var invalidChars = Path.GetInvalidFileNameChars();
        var safeName = new string(appName.Where(ch => !invalidChars.Contains(ch)).ToArray()).Trim();
        if (string.IsNullOrWhiteSpace(safeName))
        {
            safeName = new string(Path.GetFileNameWithoutExtension(fallbackPath)
                .Where(ch => !invalidChars.Contains(ch)).ToArray()).Trim();
        }
        return string.IsNullOrWhiteSpace(safeName) ? "Application" : safeName;
    }

    private static string? ResolveExecutablePath(string targetPath, string iconLocation)
    {
        var normalizedTarget = Environment.ExpandEnvironmentVariables(targetPath.Trim().Trim('"'));
        if (normalizedTarget.Length > 0)
        {
            try
            {
                if (Path.GetExtension(normalizedTarget).Equals(".exe", StringComparison.OrdinalIgnoreCase)
                    && File.Exists(normalizedTarget))
                {
                    return Path.GetFullPath(normalizedTarget);
                }
            }
            catch (Exception ex) when (ex is ArgumentException or NotSupportedException or PathTooLongException)
            {
                // Ignore malformed shell parsing strings.
            }
        }

        if (string.IsNullOrWhiteSpace(normalizedTarget) && !string.IsNullOrWhiteSpace(iconLocation))
        {
            var iconPath = iconLocation.Split(',', 2)[0].Trim().Trim('"');
            var normalizedIconPath = Environment.ExpandEnvironmentVariables(iconPath);
            try
            {
                if (string.Equals(Path.GetFileName(normalizedIconPath), "explorer.exe", StringComparison.OrdinalIgnoreCase)
                    && File.Exists(normalizedIconPath))
                {
                    return Path.GetFullPath(normalizedIconPath);
                }
            }
            catch (Exception ex) when (ex is ArgumentException or NotSupportedException or PathTooLongException)
            {
                return null;
            }
        }

        return null;
    }

    internal static bool IsFileExplorerPin(
        string? targetPath,
        string? iconLocation,
        string? appUserModelId,
        string? targetParsingPath)
    {
        if (string.Equals(appUserModelId?.Trim(), FileExplorerAumid, StringComparison.OrdinalIgnoreCase))
        {
            return true;
        }

        if (!string.IsNullOrWhiteSpace(targetParsingPath))
        {
            var parsing = targetParsingPath.Trim();
            if (parsing.EndsWith("\\" + FileExplorerAumid, StringComparison.OrdinalIgnoreCase)
                || string.Equals(parsing, FileExplorerAumid, StringComparison.OrdinalIgnoreCase))
            {
                return true;
            }

            foreach (var marker in FileExplorerClsidMarkers)
            {
                if (parsing.Contains(marker, StringComparison.OrdinalIgnoreCase))
                {
                    return true;
                }
            }
        }

        if (string.IsNullOrWhiteSpace(targetPath) && !string.IsNullOrWhiteSpace(iconLocation))
        {
            var iconPath = Environment.ExpandEnvironmentVariables(iconLocation.Split(',', 2)[0].Trim().Trim('"'));
            if (string.Equals(Path.GetFileName(iconPath), "explorer.exe", StringComparison.OrdinalIgnoreCase))
            {
                return true;
            }
        }

        return false;
    }

    private static string? GetWindowsExplorerPath()
    {
        var path = Path.Combine(Environment.GetFolderPath(Environment.SpecialFolder.Windows), "explorer.exe");
        return File.Exists(path) ? Path.GetFullPath(path) : null;
    }

    private static bool IsExplorerExecutable(string path) =>
        string.Equals(Path.GetFileName(path), "explorer.exe", StringComparison.OrdinalIgnoreCase);

    private static bool IsPackagedInstallPath(string path) =>
        DockApp.IsPackagedInstallPath(path);

    internal static bool IsAppContainerEntryPoint(string? entryPoint) =>
        !string.IsNullOrWhiteSpace(entryPoint)
        && !string.Equals(entryPoint, "Windows.FullTrustApplication", StringComparison.OrdinalIgnoreCase)
        && !string.Equals(entryPoint, "Windows.PartialTrustApplication", StringComparison.OrdinalIgnoreCase);

    private static bool IsReadableFile(string path)
    {
        if (!File.Exists(path)) return false;
        try
        {
            using var _ = File.Open(path, FileMode.Open, FileAccess.Read, FileShare.ReadWrite | FileShare.Delete);
            return true;
        }
        catch (Exception ex) when (ex is IOException or UnauthorizedAccessException)
        {
            return false;
        }
    }

    internal static string? TryExtractAppsFolderAumid(string? targetParsingPath)
    {
        if (string.IsNullOrWhiteSpace(targetParsingPath)) return null;
        var trimmed = targetParsingPath.Trim();
        if (trimmed.StartsWith(AppsFolderPrefix, StringComparison.OrdinalIgnoreCase))
        {
            var candidate = trimmed[AppsFolderPrefix.Length..].Trim();
            return candidate.Length > 0 ? candidate : null;
        }

        var idx = trimmed.IndexOf(AppsFolderClsidPrefix, StringComparison.OrdinalIgnoreCase);
        if (idx >= 0)
        {
            var candidate = trimmed[(idx + AppsFolderClsidPrefix.Length)..].Trim();
            return candidate.Length > 0 ? candidate : null;
        }

        return null;
    }

    private static bool IsResourceReference(string? value) =>
        !string.IsNullOrWhiteSpace(value)
        && value.StartsWith("ms-resource:", StringComparison.OrdinalIgnoreCase);

    private static bool IsMissingOrResource(string? value) =>
        string.IsNullOrWhiteSpace(value) || IsResourceReference(value);

    private static string ExtractFallbackDisplayName(string packageNameOrAumid, string? appId = null)
    {
        var baseName = packageNameOrAumid;
        var bang = baseName.IndexOf('!');
        if (bang > 0) baseName = baseName[..bang];
        var underscore = baseName.LastIndexOf('_');
        if (underscore > 0) baseName = baseName[..underscore];
        var lastDot = baseName.LastIndexOf('.');
        if (lastDot >= 0 && lastDot < baseName.Length - 1)
        {
            baseName = baseName[(lastDot + 1)..];
        }
        return !string.IsNullOrWhiteSpace(baseName)
            ? baseName
            : (!string.IsNullOrWhiteSpace(appId) ? appId : packageNameOrAumid);
    }

    private static string? FirstNonWhiteSpace(string? first, string? second) =>
        !string.IsNullOrWhiteSpace(first) ? first.Trim()
        : (!string.IsNullOrWhiteSpace(second) ? second.Trim() : null);

    private static string? NormalizeNullIfBlank(string? value) =>
        string.IsNullOrWhiteSpace(value) ? null : value.Trim();

    private static string ReadStringProperty(Type shortcutType, object shortcut, string propertyName)
    {
        return shortcutType.InvokeMember(
            propertyName,
            BindingFlags.GetProperty,
            binder: null,
            target: shortcut,
            args: null) as string ?? string.Empty;
    }

    [DllImport("kernel32.dll", CharSet = CharSet.Unicode, ExactSpelling = true)]
    private static extern int GetPackagesByPackageFamily(
        string packageFamilyName,
        ref uint count,
        nint packageFullNames,
        ref uint bufferLength,
        nint buffer);

    [DllImport("kernel32.dll", CharSet = CharSet.Unicode, ExactSpelling = true)]
    private static extern int GetPackagePathByFullName(
        string packageFullName,
        ref uint pathLength,
        [Out] char[]? path);

    [DllImport("shell32.dll", CharSet = CharSet.Unicode)]
    private static extern int SHCreateItemInKnownFolder(
        ref Guid rfid,
        uint dwFlags,
        string pszItem,
        ref Guid riid,
        [MarshalAs(UnmanagedType.Interface)] out IShellItem? ppv);

    [DllImport("shell32.dll", CharSet = CharSet.Unicode)]
    private static extern int SHGetNameFromIDList(nint pidl, uint sigdnName, out nint ppszName);

    [DllImport("shell32.dll")]
    private static extern int SHGetIDListFromObject([MarshalAs(UnmanagedType.IUnknown)] object punk, out nint ppidl);

    [DllImport("ole32.dll")]
    private static extern int PropVariantClear(ref PropVariant pvar);

    [DllImport("ole32.dll")]
    private static extern void CoTaskMemFree(nint pv);

    [StructLayout(LayoutKind.Sequential)]
    private struct PropertyKey(Guid formatId, uint propertyId)
    {
        public Guid FormatId = formatId;
        public uint PropertyId = propertyId;
    }

    [StructLayout(LayoutKind.Explicit, Size = 24)]
    private struct PropVariant
    {
        [FieldOffset(0)] public ushort Type;
        [FieldOffset(8)] public nint Pointer;
    }

    [ComImport, Guid("00021401-0000-0000-C000-000000000046")]
    private class ShellLinkCoClass
    {
    }

    [ComImport, Guid("43826D1E-E718-42EE-BC55-A1E261C37BFE"), InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
    private interface IShellItem
    {
        [PreserveSig] int BindToHandler(nint pbc, ref Guid bhid, ref Guid riid, out nint ppv);
        [PreserveSig] int GetParent(out nint ppsi);
        [PreserveSig] int GetDisplayName(uint sigdnName, out nint ppszName);
        [PreserveSig] int GetAttributes(uint sfgaoMask, out uint psfgaoAttribs);
        [PreserveSig] int Compare(IShellItem psi, uint hint, out int piOrder);
    }

    [ComImport, Guid("000214F9-0000-0000-C000-000000000046"), InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
    private interface IShellLinkW
    {
        [PreserveSig] int GetPath([Out, MarshalAs(UnmanagedType.LPWStr)] StringBuilder pszFile, int cch, nint pfd, uint fFlags);
        [PreserveSig] int GetIDList(out nint ppidl);
        [PreserveSig] int SetIDList(nint pidl);
        [PreserveSig] int GetDescription([Out, MarshalAs(UnmanagedType.LPWStr)] StringBuilder pszName, int cch);
        [PreserveSig] int SetDescription([MarshalAs(UnmanagedType.LPWStr)] string pszName);
        [PreserveSig] int GetWorkingDirectory([Out, MarshalAs(UnmanagedType.LPWStr)] StringBuilder pszDir, int cch);
        [PreserveSig] int SetWorkingDirectory([MarshalAs(UnmanagedType.LPWStr)] string pszDir);
        [PreserveSig] int GetArguments([Out, MarshalAs(UnmanagedType.LPWStr)] StringBuilder pszArgs, int cch);
        [PreserveSig] int SetArguments([MarshalAs(UnmanagedType.LPWStr)] string pszArgs);
        [PreserveSig] int GetHotkey(out ushort pwHotkey);
        [PreserveSig] int SetHotkey(ushort wHotkey);
        [PreserveSig] int GetShowCmd(out int piShowCmd);
        [PreserveSig] int SetShowCmd(int iShowCmd);
        [PreserveSig] int GetIconLocation([Out, MarshalAs(UnmanagedType.LPWStr)] StringBuilder pszIconPath, int cch, out int piIcon);
        [PreserveSig] int SetIconLocation([MarshalAs(UnmanagedType.LPWStr)] string pszIconPath, int iIcon);
        [PreserveSig] int SetRelativePath([MarshalAs(UnmanagedType.LPWStr)] string pszPathRel, uint dwReserved);
        [PreserveSig] int Resolve(nint hwnd, uint fFlags);
        [PreserveSig] int SetPath([MarshalAs(UnmanagedType.LPWStr)] string pszFile);
    }

    [ComImport, Guid("0000010B-0000-0000-C000-000000000046"), InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
    private interface IPersistFile
    {
        [PreserveSig] int GetClassID(out Guid pClassID);
        [PreserveSig] int IsDirty();
        [PreserveSig] int Load([MarshalAs(UnmanagedType.LPWStr)] string pszFileName, uint dwMode);
        [PreserveSig] int Save([MarshalAs(UnmanagedType.LPWStr)] string pszFileName, [MarshalAs(UnmanagedType.Bool)] bool fRemember);
        [PreserveSig] int SaveCompleted([MarshalAs(UnmanagedType.LPWStr)] string pszFileName);
        [PreserveSig] int GetCurFile([MarshalAs(UnmanagedType.LPWStr)] out string ppszFileName);
    }

    [ComImport, Guid("886D8EEB-8CF2-4446-8D02-CDBA1DBDCF99"), InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
    private interface IPropertyStore
    {
        [PreserveSig] int GetCount(out uint cProps);
        [PreserveSig] int GetAt(uint iProp, out PropertyKey pkey);
        [PreserveSig] int GetValue(ref PropertyKey key, out PropVariant pv);
        [PreserveSig] int SetValue(ref PropertyKey key, ref PropVariant pv);
        [PreserveSig] int Commit();
    }
    private static void LogSkipped(string reason) => AppLog.Warning($"Skipped taskbar pin. {reason}.");

    /// <summary>
    /// Explorer-based pins for Store apps carry "shell:AppsFolder\\&lt;AUMID&gt;" in the arguments. Capturing it
    /// lets the launcher activate the packaged app instead of opening a bare Explorer window.
    /// </summary>
    internal static string? TryExtractAumidFromArguments(string? arguments)
    {
        if (string.IsNullOrWhiteSpace(arguments)) return null;
        const string marker = "shell:appsfolder\\";
        var index = arguments.IndexOf(marker, StringComparison.OrdinalIgnoreCase);
        if (index < 0) return null;
        var start = index + marker.Length;
        var end = arguments.IndexOfAny([' ', '"', '\t'], start);
        var candidate = (end < 0 ? arguments[start..] : arguments[start..end]).Trim();
        return AppShortcutResolver.IsPackagedAppUserModelId(candidate) ? candidate : null;
    }

    internal static string? GetAppsFolderAumid(string path)
    {
        if (!path.StartsWith(AppsFolderPrefix, StringComparison.OrdinalIgnoreCase)) return null;
        var aumid = path[AppsFolderPrefix.Length..];
        return AppShortcutResolver.IsPackagedAppUserModelId(aumid) ? aumid.Trim() : null;
    }

    internal sealed record DroppedPinTarget(
        string Name,
        string TargetPath,
        string? LaunchArguments,
        string? IconLocation,
        bool IsDirectory);

    /// <summary>Fast check used during DragOver to decide whether a dropped path can be pinned or added as a stack.</summary>
    internal static bool IsSupportedDockDropPath(string? path)
    {
        if (string.IsNullOrWhiteSpace(path)) return false;
        string full;
        try
        {
            full = Path.GetFullPath(path.Trim());
        }
        catch (Exception ex) when (ex is ArgumentException or NotSupportedException or PathTooLongException)
        {
            return false;
        }

        if (Directory.Exists(full)) return true;
        if (!File.Exists(full)) return false;
        var extension = Path.GetExtension(full);
        return extension.Equals(".exe", StringComparison.OrdinalIgnoreCase)
            || extension.Equals(".lnk", StringComparison.OrdinalIgnoreCase);
    }

    /// <summary>Resolves a dropped .exe, .lnk shortcut, or folder into a dock pin or folder stack target.</summary>
    internal static DroppedPinTarget? TryResolveDroppedTarget(string? path)
    {
        if (string.IsNullOrWhiteSpace(path)) return null;
        string full;
        try
        {
            full = Path.GetFullPath(path.Trim());
        }
        catch (Exception ex) when (ex is ArgumentException or NotSupportedException or PathTooLongException)
        {
            return null;
        }

        if (Directory.Exists(full))
        {
            var trimmed = Path.TrimEndingDirectorySeparator(full);
            var folderName = Path.GetFileName(trimmed);
            return new DroppedPinTarget(
                string.IsNullOrWhiteSpace(folderName) ? trimmed : folderName,
                trimmed,
                null,
                null,
                IsDirectory: true);
        }

        if (!File.Exists(full)) return null;

        var extension = Path.GetExtension(full);
        if (extension.Equals(".exe", StringComparison.OrdinalIgnoreCase))
        {
            var name = ResolveExecutableDisplayName(full);
            return new DroppedPinTarget(name, full, null, null, IsDirectory: false);
        }

        if (extension.Equals(".lnk", StringComparison.OrdinalIgnoreCase))
        {
            return TryResolveShortcutDropTarget(full);
        }

        return null;
    }

    private static string ResolveExecutableDisplayName(string executablePath)
    {
        try
        {
            var info = FileVersionInfo.GetVersionInfo(executablePath);
            if (!string.IsNullOrWhiteSpace(info.FileDescription)) return info.FileDescription.Trim();
            if (!string.IsNullOrWhiteSpace(info.ProductName)) return info.ProductName.Trim();
        }
        catch (Exception ex) when (ex is IOException or UnauthorizedAccessException or FileNotFoundException)
        {
        }

        return Path.GetFileNameWithoutExtension(executablePath);
    }

    private static DroppedPinTarget? TryResolveShortcutDropTarget(string shortcutPath)
    {
        Type? shellType;
        object? shell;
        try
        {
            shellType = Type.GetTypeFromProgID("WScript.Shell");
            shell = shellType is null ? null : Activator.CreateInstance(shellType);
        }
        catch (Exception ex) when (ex is COMException or TargetInvocationException)
        {
            return null;
        }

        if (shellType is null || shell is null) return null;

        try
        {
            var shortcut = shellType.InvokeMember(
                "CreateShortcut",
                BindingFlags.InvokeMethod,
                binder: null,
                target: shell,
                args: [shortcutPath]);
            if (shortcut is null) return null;

            try
            {
                var shortcutType = shortcut.GetType();
                var targetPath = ReadStringProperty(shortcutType, shortcut, "TargetPath");
                var arguments = ReadStringProperty(shortcutType, shortcut, "Arguments");
                var iconLocation = ReadStringProperty(shortcutType, shortcut, "IconLocation");
                var shortcutName = Path.GetFileNameWithoutExtension(shortcutPath);

                var executablePath = ResolveExecutablePath(targetPath, iconLocation);
                if (executablePath is not null)
                {
                    return new DroppedPinTarget(
                        string.IsNullOrWhiteSpace(shortcutName) ? ResolveExecutableDisplayName(executablePath) : shortcutName,
                        executablePath,
                        string.IsNullOrWhiteSpace(arguments) ? null : arguments,
                        string.IsNullOrWhiteSpace(iconLocation) ? null : iconLocation,
                        IsDirectory: false);
                }

                var normalizedTarget = Environment.ExpandEnvironmentVariables(targetPath.Trim().Trim('"'));
                if (!string.IsNullOrWhiteSpace(normalizedTarget) && Directory.Exists(normalizedTarget))
                {
                    var dirFull = Path.TrimEndingDirectorySeparator(Path.GetFullPath(normalizedTarget));
                    var dirName = Path.GetFileName(dirFull);
                    return new DroppedPinTarget(
                        string.IsNullOrWhiteSpace(shortcutName) ? (string.IsNullOrWhiteSpace(dirName) ? dirFull : dirName) : shortcutName,
                        dirFull,
                        null,
                        null,
                        IsDirectory: true);
                }

                return null;
            }
            finally
            {
                if (Marshal.IsComObject(shortcut))
                {
                    _ = Marshal.FinalReleaseComObject(shortcut);
                }
            }
        }
        catch (Exception ex) when (ex is COMException or TargetInvocationException or IOException or UnauthorizedAccessException or ArgumentException or NotSupportedException)
        {
            return null;
        }
        finally
        {
            if (Marshal.IsComObject(shell))
            {
                _ = Marshal.FinalReleaseComObject(shell);
            }
    }
    }

}
