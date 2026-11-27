using System;
using System.Runtime.InteropServices;
using System.Text;

namespace OpenTaskbar;

internal static class FjordBarNative
{
    internal const uint AppBarNew = 0;
    internal const uint AppBarRemove = 1;
    internal const uint AppBarQueryPosition = 2;
    internal const uint AppBarSetPosition = 3;
    internal const uint TopEdge = 1;
    internal const uint NoActivate = 0x0010;
    internal const uint NoOwnerZOrder = 0x0200;

    [StructLayout(LayoutKind.Sequential)]
    internal struct Rect : IEquatable<Rect>
    {
        public int Left, Top, Right, Bottom;
        public readonly int Width => Right - Left;
        public readonly int Height => Bottom - Top;
        public readonly bool Equals(Rect other) =>
            Left == other.Left && Top == other.Top && Right == other.Right && Bottom == other.Bottom;
        public override readonly bool Equals(object? obj) => obj is Rect other && Equals(other);
        public override readonly int GetHashCode() => HashCode.Combine(Left, Top, Right, Bottom);
    }

    [StructLayout(LayoutKind.Sequential)]
    internal struct AppBarData
    {
        public uint Size;
        public nint Window;
        public uint Callback, Edge;
        public Rect Bounds;
        public nint Parameter;
    }

    [DllImport("shell32.dll")] internal static extern nuint SHAppBarMessage(uint message, ref AppBarData data);
    [DllImport("user32.dll", CharSet = CharSet.Unicode)] internal static extern uint RegisterWindowMessage(string message);
    [DllImport("user32.dll")] internal static extern bool SetWindowPos(nint window, nint after, int x, int y, int width, int height, uint flags);
    [DllImport("user32.dll")] internal static extern bool ShowWindow(nint window, int command);
    [DllImport("user32.dll")] internal static extern bool IsWindowVisible(nint window);
    [DllImport("user32.dll")] internal static extern bool IsZoomed(nint window);
    [DllImport("user32.dll")] internal static extern bool IsIconic(nint window);
    [DllImport("user32.dll")] internal static extern bool GetWindowRect(nint window, out Rect rect);
    [DllImport("user32.dll")] internal static extern nint MonitorFromWindow(nint window, uint flags);
    [DllImport("user32.dll")] internal static extern nint GetForegroundWindow();
    [DllImport("user32.dll")] internal static extern bool SetForegroundWindow(nint window);
    [DllImport("user32.dll", CharSet = CharSet.Unicode)] private static extern int GetClassName(nint window, StringBuilder name, int capacity);
    [DllImport("user32.dll", EntryPoint = "GetWindowLongPtrW")] internal static extern nint GetWindowLongPtr(nint window, int index);
    [DllImport("user32.dll", EntryPoint = "SetWindowLongPtrW")] internal static extern nint SetWindowLongPtr(nint window, int index, nint value);
    [DllImport("dwmapi.dll")] private static extern int DwmGetWindowAttribute(nint window, int attribute, out int value, int size);

    internal static Rect PixelBounds(Services.DisplayInfo display)
    {
        var scale = display.Dpi / 96d;
        return new Rect
        {
            Left = (int)Math.Round(display.Bounds.Left * scale),
            Top = (int)Math.Round(display.Bounds.Top * scale),
            Right = (int)Math.Round(display.Bounds.Right * scale),
            Bottom = (int)Math.Round(display.Bounds.Bottom * scale)
        };
    }

    internal static bool IsFullscreen(nint foreground, Services.DisplayInfo display)
    {
        if (foreground == 0 || !IsWindowVisible(foreground) || IsIconic(foreground)
            || MonitorFromWindow(foreground, 0) != display.Handle || !GetWindowRect(foreground, out var window))
            return false;
        var name = new StringBuilder(48);
        GetClassName(foreground, name, name.Capacity);
        if (name.ToString() is "Progman" or "WorkerW" or "Shell_TrayWnd" or "Shell_SecondaryTrayWnd")
            return false;
        if (DwmGetWindowAttribute(foreground, 14, out var cloaked, sizeof(int)) == 0 && cloaked != 0)
            return false;
        const long caption = 0x00C00000;
        if (IsZoomed(foreground) && (GetWindowLongPtr(foreground, -16).ToInt64() & caption) != 0)
            return false;
        var bounds = PixelBounds(display);
        return window.Left <= bounds.Left + 2 && window.Top <= bounds.Top + 2
            && window.Right >= bounds.Right - 2 && window.Bottom >= bounds.Bottom - 2;
    }

}
