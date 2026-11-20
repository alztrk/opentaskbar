using System;
using System.Runtime.InteropServices;
using System.Windows.Interop;

namespace OpenTaskbar.Services;

/// <summary>
/// Watches the Windows broadcasts that signal a personalization color change. A hidden,
/// zero-sized HWND receives the broadcasts without creating any visible UI; callbacks are
/// marshalled by the owner.
/// </summary>
public sealed class WindowsPaletteListener : IDisposable
{
    private const int WmSettingChange = 0x001A;
    private const int WmSysColorChange = 0x0015;
    private const int WmThemeChanged = 0x031A;
    private const int WmDwmColorizationColorChanged = 0x0320;

    private HwndSource? _source;
    private readonly HwndSourceHook _hook;

    public event EventHandler? Changed;

    public WindowsPaletteListener()
    {
        _hook = WndProc;
        // A hidden (zero-sized, tool-window) top-level HWND receives WM_SETTINGCHANGE broadcasts;
        // HWND_MESSAGE windows do not, so they cannot detect an accent change made in Windows Settings.
        var parameters = new HwndSourceParameters("OpenTaskbarPaletteListener")
        {
            Width = 0,
            Height = 0,
            WindowStyle = 0,
            ExtendedWindowStyle = 0x00000080 // WS_EX_TOOLWINDOW (keeps it out of the taskbar/Alt+Tab).
        };
        _source = new HwndSource(parameters);
        _source.AddHook(_hook);
    }

    private IntPtr WndProc(IntPtr hwnd, int message, IntPtr wParam, IntPtr lParam, ref bool handled)
    {
        var changed = message switch
        {
            WmSettingChange => IsImmersiveColorChange(lParam),
            WmSysColorChange or WmThemeChanged or WmDwmColorizationColorChanged => true,
            _ => false
        };
        if (changed) Changed?.Invoke(this, EventArgs.Empty);
        return IntPtr.Zero;
    }

    private static bool IsImmersiveColorChange(IntPtr lParam)
    {
        var section = Marshal.PtrToStringUni(lParam);
        return string.Equals(section, "ImmersiveColorSet", StringComparison.OrdinalIgnoreCase)
            || string.Equals(section, "WindowsThemeElement", StringComparison.OrdinalIgnoreCase);
    }

    public void Dispose()
    {
        if (_source is null) return;
        _source.RemoveHook(_hook);
        _source.Dispose();
        _source = null;
    }
}
