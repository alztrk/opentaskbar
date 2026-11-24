using System.Collections.Concurrent;
using System.IO;
using System.Runtime.CompilerServices;
using System.Runtime.InteropServices;
using System.Security.Cryptography;
using System.Text;
using System.Windows.Interop;
using System.Windows.Media.Imaging;
using System.Xml.Linq;
using OpenTaskbar.Models;

namespace OpenTaskbar.Services;

public static class ApplicationIconProvider
{
    private const uint ShellIcon = 0x00000100;
    private const int DefaultIconResolution = 64;

    /// <summary>Target icon pixel size matching current dock icon DIP and display DPI.</summary>
    public static int PreferredIconPixels { get; set; } = DefaultIconResolution;

    public static int ComputeTargetPixels(double iconDip, double dpiScale) =>
        Math.Max(16, (int)Math.Ceiling(iconDip * Math.Max(1.0, dpiScale)));

    /// <summary>Pixel sizes that icon resources commonly ship, drawn by the app author for that exact size.</summary>
    internal static readonly int[] NativeIconSizes = [16, 20, 24, 32, 40, 48, 64, 96, 128, 256];

    /// <summary>How far a native size may be from the requested one and still be used instead of it.</summary>
    internal const double NativeSnapTolerance = 0.12;

    /// <summary>
    /// Device-pixel size for dock application icons. Icons scaled to an arbitrary size (e.g. 38 px extracted and
    /// drawn at 33 px) are resampled twice and look soft. Snapping to a nearby native icon size and drawing the
    /// bitmap at exactly that many device pixels keeps them as crisp as the author drew them.
    /// </summary>
    public static int ComputeApplicationIconPixels(double iconDip, double dpiScale)
    {
        var desired = Math.Max(16, iconDip * Math.Max(1.0, dpiScale));
        var nearest = NativeIconSizes.MinBy(size => Math.Abs(size - desired));
        return Math.Abs(nearest - desired) <= desired * NativeSnapTolerance ? nearest : (int)Math.Round(desired);
    }

    /// <summary>
    /// Device-pixel size of the variant shown while the dock magnifies an icon. Drawing the resting bitmap scaled
    /// up blurs it; a larger native bitmap scaled down stays sharp. 0 disables hover variants.
    /// </summary>
    public static int HoverIconPixels { get; set; }

    // Resting dock icon -> its preloaded larger variant. Weak keys: entries go away with their icons.
    private static readonly ConditionalWeakTable<BitmapSource, BitmapSource> HoverVariants = new();

    // Cached WindowsApps executable path -> resolved AppxManifest AUMID (or null when unavailable).
    private static readonly ConcurrentDictionary<string, string?> PackagedAumidCache = new(StringComparer.OrdinalIgnoreCase);

    /// <summary>The smallest native icon size that covers <paramref name="restPixels"/> magnified by <paramref name="scale"/>.</summary>
    public static int ComputeHoverIconPixels(int restPixels, double scale)
    {
        var needed = (int)Math.Ceiling(restPixels * Math.Max(1.0, scale));
        foreach (var size in NativeIconSizes)
        {
            if (size >= needed) return size;
        }
        return needed;
    }

    /// <summary>The larger variant of a resting dock icon for magnification, or null when there is none.</summary>
    public static BitmapSource? GetHoverVariant(BitmapSource? icon)
    {
        if (icon is null) return null;
        lock (HoverVariants)
        {
            return HoverVariants.TryGetValue(icon, out var variant) ? variant : null;
        }
    }

    private static BitmapSource? RememberHoverVariant(BitmapSource? icon, Func<int, BitmapSource?> load)
    {
        var hoverPixels = HoverIconPixels;
        if (icon is null || hoverPixels <= icon.PixelWidth) return icon;
        lock (HoverVariants)
        {
            if (HoverVariants.TryGetValue(icon, out _)) return icon;
        }

        // Asynchronously extract and freeze off the UI render path to guarantee zero hover stalls.
        _ = Task.Run(() =>
        {
            try
            {
                var variant = load(hoverPixels);
                if (variant is not null && variant.PixelWidth > icon.PixelWidth)
                {
                    if (variant.CanFreeze && !variant.IsFrozen) variant.Freeze();
                    lock (HoverVariants)
                    {
                        HoverVariants.AddOrUpdate(icon, variant);
                    }
                }
            }
            catch (Exception ex)
            {
                AppLog.Warning($"Hover icon extraction failed: {ex.Message}");
            }
        });

        return icon;
    }

    /// <summary>
    /// Resolves the best icon for a pinned taskbar application:
    /// 1. Custom shortcut IconLocation (e.g. Chrome/Edge PWA .ico files when target is chrome_proxy.exe).
    /// 2. Packaged Store app Start menu icon via shell:AppsFolder\{AUMID} from AppxManifest.xml under WindowsApps.
    /// 3. Fallback to the executable's own icon.
    /// </summary>
    public static BitmapSource? GetForPinnedApp(string executablePath, string? iconLocation = null, string? appUserModelId = null, int targetSize = 0)
    {
        if (TryParseIconLocation(iconLocation, out var iconPath, out var iconIndex) && File.Exists(iconPath))
        {
            var sameAsExe = false;
            try
            {
                sameAsExe = string.Equals(Path.GetFullPath(iconPath), Path.GetFullPath(executablePath), StringComparison.OrdinalIgnoreCase);
            }
            catch (Exception ex) when (ex is ArgumentException or NotSupportedException or PathTooLongException)
            {
                sameAsExe = false;
            }

            if (!sameAsExe || iconIndex != 0)
            {
                var customIcon = iconIndex != 0
                    ? GetForResource(iconPath, iconIndex, targetSize)
                    : GetForResource(iconPath, 0, targetSize) ?? GetForFile(iconPath, targetSize);
                if (customIcon is not null) return customIcon;
            }
        }

        var isAppsFolderPath = executablePath.StartsWith("shell:AppsFolder\\", StringComparison.OrdinalIgnoreCase);
        var resolvedAumid = !string.IsNullOrWhiteSpace(appUserModelId)
            ? appUserModelId.Trim()
            : (isAppsFolderPath ? executablePath["shell:AppsFolder\\".Length..].Trim() : TryResolvePackagedAppUserModelId(executablePath));

        if (!string.IsNullOrWhiteSpace(resolvedAumid)
            && (isAppsFolderPath || resolvedAumid.Contains('!') || !File.Exists(executablePath)))
        {
            var shellSize = targetSize > 0 ? targetSize : PreferredIconPixels;
            // iconOnly must match the running-app path: without SIIGBF_ICONONLY the Shell may return the
            // app's window thumbnail (mirrored/rotated) instead of its Start-menu icon.
            var packagedIcon = GetForShellItem($"shell:AppsFolder\\{resolvedAumid}", shellSize, iconOnly: true);
            if (packagedIcon is not null) return packagedIcon;
        }

        if (isAppsFolderPath) return null;
        return GetForFile(executablePath, targetSize);
    }

    internal static bool TryParseIconLocation(string? iconLocation, out string path, out int index)
    {
        path = string.Empty;
        index = 0;
        if (string.IsNullOrWhiteSpace(iconLocation)) return false;

        var trimmed = iconLocation.Trim();
        var lastComma = trimmed.LastIndexOf(',');
        if (lastComma > 0 && int.TryParse(trimmed[(lastComma + 1)..].Trim(), out var parsedIndex))
        {
            index = parsedIndex;
            trimmed = trimmed[..lastComma].Trim();
        }

        trimmed = trimmed.Trim('"').Trim();
        if (string.IsNullOrWhiteSpace(trimmed)) return false;
        path = Environment.ExpandEnvironmentVariables(trimmed);
        return !string.IsNullOrWhiteSpace(path);
    }

    /// <summary>
    /// For executables inside packaged install roots (<c>WindowsApps</c>, <c>SystemApps</c>, or
    /// <c>ImmersiveControlPanel</c>), reads <c>AppxManifest.xml</c> to build the AppUserModelID
    /// (<c>&lt;PackageFamilyName&gt;!&lt;ApplicationId&gt;</c>) used by <c>shell:AppsFolder</c>.
    /// </summary>
    internal static string? TryResolvePackagedAppUserModelId(string? executablePath)
    {
        if (string.IsNullOrWhiteSpace(executablePath)) return null;
        string fullExePath;
        try
        {
            fullExePath = Path.GetFullPath(executablePath);
        }
        catch (Exception ex) when (ex is ArgumentException or NotSupportedException or PathTooLongException)
        {
            return null;
        }

        if (!DockApp.IsPackagedInstallPath(fullExePath))
        {
            return null;
        }

        return PackagedAumidCache.GetOrAdd(fullExePath, ResolvePackagedAppUserModelIdCore);
    }

    private static string? ResolvePackagedAppUserModelIdCore(string fullExePath)
    {
        var dir = Path.GetDirectoryName(fullExePath);
        string? packageRoot = null;
        while (!string.IsNullOrEmpty(dir))
        {
            var folderName = Path.GetFileName(dir);
            if (string.Equals(folderName, "WindowsApps", StringComparison.OrdinalIgnoreCase)
                || string.Equals(folderName, "SystemApps", StringComparison.OrdinalIgnoreCase)
                || string.Equals(folderName, "Windows", StringComparison.OrdinalIgnoreCase))
            {
                break;
            }
            if (File.Exists(Path.Combine(dir, "AppxManifest.xml")))
            {
                packageRoot = dir;
                break;
            }
            dir = Path.GetDirectoryName(dir);
        }

        if (packageRoot is null) return null;

        var packageFolder = Path.GetFileName(packageRoot);
        try
        {
            var manifest = XDocument.Load(Path.Combine(packageRoot, "AppxManifest.xml"));
            var packageFamilyName = TryExtractPackageFamilyName(packageFolder, manifest);
            if (string.IsNullOrWhiteSpace(packageFamilyName)) return null;

            var applications = manifest.Descendants()
                .Where(e => string.Equals(e.Name.LocalName, "Application", StringComparison.Ordinal))
                .Select(e => (
                    Id: e.Attribute("Id")?.Value?.Trim(),
                    Executable: e.Attribute("Executable")?.Value?.Trim()))
                .Where(a => !string.IsNullOrWhiteSpace(a.Id))
                .ToArray();

            if (applications.Length == 0) return null;

            var relativeExe = Path.GetRelativePath(packageRoot, fullExePath).Replace('/', '\\');
            var match = applications.FirstOrDefault(a =>
                !string.IsNullOrWhiteSpace(a.Executable)
                && string.Equals(a.Executable.Replace('/', '\\'), relativeExe, StringComparison.OrdinalIgnoreCase));

            var appId = match.Id ?? (applications.Length == 1 ? applications[0].Id : null);
            return string.IsNullOrWhiteSpace(appId) ? null : $"{packageFamilyName}!{appId}";
        }
        catch (Exception ex) when (ex is IOException or UnauthorizedAccessException or System.Xml.XmlException)
        {
            AppLog.Warning($"Failed to parse AppxManifest for '{packageFolder}': {ex.Message}");
            return null;
        }
    }

    private static string? TryExtractPackageFamilyName(string packageFolder, XDocument manifest)
    {
        if (!string.IsNullOrWhiteSpace(packageFolder))
        {
            var parts = packageFolder.Split('_');
            // WindowsApps folder format: <Name>_<Version>_<Architecture>_<ResourceId>_<PublisherId>
            if (parts.Length >= 5 && !string.IsNullOrWhiteSpace(parts[0]) && IsPublisherId(parts[^1]))
            {
                return $"{parts[0]}_{parts[^1]}";
            }

            // SystemApps folder format: <Name>_<PublisherId>
            if (parts.Length >= 2 && IsPublisherId(parts[^1]))
            {
                var lastUnderscore = packageFolder.LastIndexOf('_');
                if (lastUnderscore > 0)
                {
                    return $"{packageFolder[..lastUnderscore]}_{parts[^1]}";
                }
            }
        }

        // Inbox system packages (e.g. C:\Windows\ImmersiveControlPanel) do not encode the publisher ID
        // in their directory name; derive <Identity/@Name>_<PublisherId> from AppxManifest.xml.
        var identity = manifest.Descendants()
            .FirstOrDefault(e => string.Equals(e.Name.LocalName, "Identity", StringComparison.Ordinal));
        var name = identity?.Attribute("Name")?.Value?.Trim();
        var publisher = identity?.Attribute("Publisher")?.Value?.Trim();
        if (string.IsNullOrWhiteSpace(name) || string.IsNullOrWhiteSpace(publisher))
        {
            return null;
        }

        return $"{name}_{ComputePublisherId(publisher)}";
    }

    private static bool IsPublisherId(string? value)
    {
        if (string.IsNullOrEmpty(value) || value.Length != 13) return false;
        foreach (var ch in value)
        {
            if (!char.IsAsciiLetterOrDigit(ch)) return false;
        }
        return true;
    }

    internal static string ComputePublisherId(string publisher)
    {
        const string alphabet = "0123456789abcdefghjkmnpqrstvwxyz";
        var hash = SHA256.HashData(Encoding.Unicode.GetBytes(publisher));
        ulong value = 0;
        for (var i = 0; i < 8; i++)
        {
            value = (value << 8) | hash[i];
        }

        Span<char> chars = stackalloc char[13];
        for (var i = 0; i < 13; i++)
        {
            var shift = 64 - (i + 1) * 5;
            var index = shift >= 0
                ? (int)((value >> shift) & 0x1F)
                : (int)((value << -shift) & 0x1F);
            chars[i] = alphabet[index];
        }

        return new string(chars);
    }

    public static BitmapSource? GetForFile(string path, int targetSize = 0)
    {
        if (targetSize <= 0) return RememberHoverVariant(GetForFile(path, PreferredIconPixels), size => GetForFile(path, size));
        var size = targetSize;
        if (TryResolvePackagedAppUserModelId(path) is { } packagedAumid)
        {
            var packagedIcon = GetForShellItem($"shell:AppsFolder\\{packagedAumid}", size, iconOnly: true);
            if (packagedIcon is not null)
            {
                return packagedIcon;
            }
        }
        if (string.Equals(Path.GetExtension(path), ".exe", StringComparison.OrdinalIgnoreCase))
        {
            var executableIcon = GetExecutableIcon(path, 0, size);
            if (executableIcon is not null)
            {
                return executableIcon;
            }
        }
        else if (File.Exists(path))
        {
            var directIcon = GetExecutableIcon(path, 0, size);
            if (directIcon is not null)
            {
                return directIcon;
            }
        }

        var fileInfo = new ShellFileInfo();
        var result = SHGetFileInfo(path, 0, ref fileInfo, (uint)Marshal.SizeOf<ShellFileInfo>(), ShellIcon);
        if (result == 0 || fileInfo.Icon == 0)
        {
            return null;
        }

        try
        {
            return ToBitmapSource(fileInfo.Icon);
        }
        finally
        {
            DestroyIcon(fileInfo.Icon);
        }
    }

    public static BitmapSource? GetForResource(string path, int index, int targetSize = 0)
    {
        var expanded = Environment.ExpandEnvironmentVariables(path);
        if (!File.Exists(expanded)) return null;
        if (targetSize > 0) return GetExecutableIcon(expanded, index, targetSize);
        return RememberHoverVariant(GetExecutableIcon(expanded, index, PreferredIconPixels),
            size => GetExecutableIcon(expanded, index, size));
    }

    /// <summary>
    /// Thumbnail or icon for any Shell parsing name: files, folders, or <c>shell:AppsFolder\{AUMID}</c> entries.
    /// </summary>
    public static BitmapSource? GetForShellItem(string parsingName, int size = 64, bool iconOnly = false)
    {
        var icon = LoadShellItemImage(parsingName, size, iconOnly);
        // Dock icons are requested at the preferred size; give those a sharper variant for magnification.
        return size == PreferredIconPixels
            ? RememberHoverVariant(icon, hoverSize => LoadShellItemImage(parsingName, hoverSize, iconOnly))
            : icon;
    }

    internal static string? GetShellItemDisplayName(string parsingName)
    {
        var itemId = typeof(IShellItem).GUID;
        if (SHCreateItemFromParsingName(parsingName, 0, ref itemId, out var shellObject) != 0
            || shellObject is not IShellItem item)
            return null;

        nint displayName = 0;
        try
        {
            return item.GetDisplayName(0, out displayName) == 0 ? Marshal.PtrToStringUni(displayName) : null;
        }
        finally
        {
            if (displayName != 0) Marshal.FreeCoTaskMem(displayName);
            Marshal.ReleaseComObject(item);
        }
    }

    private static BitmapSource? LoadShellItemImage(string parsingName, int size, bool iconOnly)
    {
        var factoryId = typeof(IShellItemImageFactory).GUID;
        if (SHCreateItemFromParsingName(parsingName, 0, ref factoryId, out var factoryObject) != 0
            || factoryObject is not IShellItemImageFactory factory)
        {
            return null;
        }

        nint bitmap = 0;
        try
        {
            // SIIGBF_BIGGERSIZEOK | (ICONONLY) | SIIGBF_SCALEUP
            var flags = 0x1 | (iconOnly ? 0x4 : 0) | 0x100;
            if (factory.GetImage(new NativeSize { Width = size, Height = size }, flags, out bitmap) != 0 || bitmap == 0)
            {
                return null;
            }

            // Shell bitmaps are 32bpp DIB sections with premultiplied alpha; Imaging.CreateBitmapSourceFromHBitmap
            // drops that alpha, so copy the pixels directly.
            var info = new NativeBitmap();
            if (GetObject(bitmap, Marshal.SizeOf<NativeBitmap>(), ref info) == 0 || info.Bits == 0 || info.BitsPixel != 32)
            {
                var fallback = Imaging.CreateBitmapSourceFromHBitmap(bitmap, 0, System.Windows.Int32Rect.Empty, BitmapSizeOptions.FromEmptyOptions());
                fallback.Freeze();
                return fallback;
            }

            var width = info.Width;
            var height = Math.Abs(info.Height);
            var stride = info.WidthBytes;
            var pixels = new byte[stride * height];
            for (var row = 0; row < height; row++)
            {
                // Positive heights are bottom-up DIBs.
                var sourceRow = info.Height > 0 ? height - 1 - row : row;
                Marshal.Copy(info.Bits + sourceRow * stride, pixels, row * stride, stride);
            }

            // Some shell images (notably packaged Settings) arrive with straight alpha even though the
            // format is 32bpp. Pbgra32 expects premultiplied channels; leaving them straight makes the
            // antialiased edges glow white. Premultiply explicitly so both variants render correctly.
            for (var offset = 0; offset < pixels.Length; offset += 4)
            {
                var alpha = pixels[offset + 3];
                if (alpha == 255) continue;
                pixels[offset] = (byte)(pixels[offset] * alpha / 255);
                pixels[offset + 1] = (byte)(pixels[offset + 1] * alpha / 255);
                pixels[offset + 2] = (byte)(pixels[offset + 2] * alpha / 255);
            }

            var result = BitmapSource.Create(width, height, 96, 96, System.Windows.Media.PixelFormats.Pbgra32, null, pixels, stride);
            result.Freeze();
            return result;
        }
        catch (Exception ex) when (ex is COMException or ArgumentException or InvalidCastException)
        {
            return null;
        }
        finally
        {
            if (bitmap != 0) DeleteObject(bitmap);
            Marshal.ReleaseComObject(factory);
        }
    }

    [StructLayout(LayoutKind.Sequential)]
    private struct NativeSize
    {
        public int Width;
        public int Height;
    }

    [ComImport, Guid("bcc18b79-ba16-442f-80c4-8a59c30c463b"), InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
    private interface IShellItemImageFactory
    {
        [PreserveSig]
        int GetImage(NativeSize size, int flags, out nint bitmap);
    }

    [ComImport, Guid("43826d1e-e718-42ee-bc55-a1e261c37bfe"), InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
    private interface IShellItem
    {
        [PreserveSig] int BindToHandler(nint bindContext, in Guid handler, in Guid iid, out nint result);
        [PreserveSig] int GetParent(out nint parent);
        [PreserveSig] int GetDisplayName(uint format, out nint name);
    }

    [DllImport("shell32.dll", CharSet = CharSet.Unicode, PreserveSig = true)]
    private static extern int SHCreateItemFromParsingName(string path, nint bindContext, ref Guid riid,
        [MarshalAs(UnmanagedType.Interface)] out object? item);

    [DllImport("gdi32.dll")]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool DeleteObject(nint handle);

    [StructLayout(LayoutKind.Sequential)]
    private struct NativeBitmap
    {
        public int Type;
        public int Width;
        public int Height;
        public int WidthBytes;
        public ushort Planes;
        public ushort BitsPixel;
        public nint Bits;
    }

    [DllImport("gdi32.dll", EntryPoint = "GetObjectW")]
    private static extern int GetObject(nint handle, int size, ref NativeBitmap bitmap);

    private static BitmapSource? GetExecutableIcon(string path, int index = 0, int targetSize = DefaultIconResolution)
    {
        // Primary path: Extract icon resource at the requested DPI-scaled pixel size.
        var icons = new nint[1];
        var ids = new uint[1];
        try
        {
            var extracted = PrivateExtractIcons(path, index, targetSize, targetSize, icons, ids, 1, 0);
            if (extracted != uint.MaxValue && extracted > 0 && icons[0] != 0)
            {
                try
                {
                    return ToBitmapSource(icons[0]);
                }
                finally
                {
                    DestroyIcon(icons[0]);
                }
            }
        }
        catch (Exception ex) when (ex is EntryPointNotFoundException or ExternalException or ArgumentException)
        {
            // Fall through to ExtractIconEx when PrivateExtractIcons is unavailable or the bitmap conversion fails.
        }

        var count = ExtractIconEx(path, index, out var largeIcon, out var smallIcon, 1);
        if (count == 0 || (largeIcon == 0 && smallIcon == 0))
        {
            return null;
        }

        try
        {
            return ToBitmapSource(largeIcon != 0 ? largeIcon : smallIcon);
        }
        finally
        {
            if (largeIcon != 0)
            {
                DestroyIcon(largeIcon);
            }

            if (smallIcon != 0)
            {
                DestroyIcon(smallIcon);
            }
        }
    }

    private static BitmapSource? ToBitmapSource(nint iconHandle)
    {
        var iconCopy = CopyIcon(iconHandle);
        if (iconCopy == 0)
        {
            return null;
        }

        try
        {
            var bitmap = Imaging.CreateBitmapSourceFromHIcon(iconCopy, System.Windows.Int32Rect.Empty,
                BitmapSizeOptions.FromEmptyOptions());
            bitmap.Freeze();
            return bitmap;
        }
        catch (Exception ex) when (ex is ExternalException or COMException or ArgumentException)
        {
            return null;
        }
        finally
        {
            DestroyIcon(iconCopy);
        }
    }

    [StructLayout(LayoutKind.Sequential, CharSet = CharSet.Unicode)]
    private struct ShellFileInfo
    {
        public nint Icon;
        public int IconIndex;
        public uint Attributes;
        [MarshalAs(UnmanagedType.ByValTStr, SizeConst = 260)]
        public string? DisplayName;
        [MarshalAs(UnmanagedType.ByValTStr, SizeConst = 80)]
        public string? TypeName;
    }

    [DllImport("user32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
    private static extern uint PrivateExtractIcons(
        string szFileName,
        int nIconIndex,
        int cxIcon,
        int cyIcon,
        nint[] phicon,
        uint[] piconid,
        uint nIcons,
        uint flags);

    [DllImport("shell32.dll", CharSet = CharSet.Unicode, EntryPoint = "ExtractIconExW")]
    private static extern uint ExtractIconEx(string file, int iconIndex, out nint largeIcon, out nint smallIcon, uint iconCount);

    [DllImport("shell32.dll", CharSet = CharSet.Unicode)]
    private static extern nint SHGetFileInfo(string path, uint fileAttributes, ref ShellFileInfo fileInfo, uint fileInfoSize, uint flags);

    [DllImport("user32.dll", SetLastError = true)]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool DestroyIcon(nint iconHandle);

    [DllImport("user32.dll", SetLastError = true)]
    private static extern nint CopyIcon(nint iconHandle);
}
