using System.IO;
using System.Runtime.CompilerServices;
using System.Runtime.InteropServices;
using System.Windows.Interop;
using System.Windows.Media.Imaging;

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

    // Resting dock icon -> how to load its larger variant. Weak keys: entries go away with their icons, and the
    // variant is only extracted the first time that icon is magnified.
    private static readonly ConditionalWeakTable<BitmapSource, Lazy<BitmapSource?>> HoverVariants = new();

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
    public static BitmapSource? GetHoverVariant(BitmapSource? icon) =>
        icon is not null && HoverVariants.TryGetValue(icon, out var variant) ? variant.Value : null;

    private static BitmapSource? RememberHoverVariant(BitmapSource? icon, Func<int, BitmapSource?> load)
    {
        var hoverPixels = HoverIconPixels;
        if (icon is null || hoverPixels <= icon.PixelWidth) return icon;
        HoverVariants.AddOrUpdate(icon, new Lazy<BitmapSource?>(() =>
        {
            var variant = load(hoverPixels);
            // A source without larger artwork returns the same small size; that would not be any sharper.
            return variant is not null && variant.PixelWidth > icon.PixelWidth ? variant : null;
        }));
        return icon;
    }

    public static BitmapSource? GetForFile(string path, int targetSize = 0)
    {
        if (targetSize <= 0) return RememberHoverVariant(GetForFile(path, PreferredIconPixels), size => GetForFile(path, size));
        var size = targetSize;
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
