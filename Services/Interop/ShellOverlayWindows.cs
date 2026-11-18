using System.Runtime.InteropServices;

namespace OpenTaskbar.Services;

/// <summary>
/// Recognizes transient shell overlays that take the foreground without being applications, such as the
/// Alt+Tab switcher. They span the whole screen, so auto-hide must look past them to the application beneath.
/// </summary>
internal static class ShellOverlayWindows
{
    /// <summary>
    /// Window classes of the task switcher: XamlExplorerHostIslandWindow hosts Alt+Tab and Task View on
    /// Windows 11, MultitaskingViewFrame on Windows 10, TaskSwitcherWnd for the classic switcher.
    /// </summary>
    internal static readonly string[] TaskSwitcherClassNames =
        ["XamlExplorerHostIslandWindow", "MultitaskingViewFrame", "TaskSwitcherWnd"];

    internal static bool IsTaskSwitcherClassName(string? className) =>
        className is not null && TaskSwitcherClassNames.Contains(className, StringComparer.Ordinal);

    public static bool IsTaskSwitcher(nint windowHandle)
    {
        if (windowHandle == 0) return false;
        Span<char> buffer = stackalloc char[64];
        var length = GetClassName(windowHandle, ref MemoryMarshal.GetReference(buffer), buffer.Length);
        return length > 0 && IsTaskSwitcherClassName(new string(buffer[..length]));
    }

    [DllImport("user32.dll", CharSet = CharSet.Unicode)]
    private static extern int GetClassName(nint windowHandle, ref char className, int maxCount);
}
