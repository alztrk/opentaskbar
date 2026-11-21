namespace OpenTaskbar.Services;

/// <summary>
/// The Win32 rules that decide whether a top-level window is a real taskbar app window. Kept in one place so the
/// dock, the tray app list and the tests all agree on what counts as a running application; tray-only helper
/// windows must never be promoted to normal app buttons.
/// </summary>
internal static class TaskbarWindowRules
{
    internal const long AppWindow = 0x00040000;
    internal const long ToolWindow = 0x00000080;
    internal const long NoActivate = 0x08000000;
    internal const long WindowEdge = 0x00000100;
    internal const int CloakedShell = 0x00000002;
    internal const string DeletedFromTaskbarProperty = "ITaskList_Deleted";

    /// <summary>
    /// Mirrors the shell's own eligibility check: an owned window needs WS_EX_APPWINDOW to opt in, tool and
    /// non-activatable windows are hidden unless they opt in, and ITaskbarList::DeleteTab overrides everything.
    /// </summary>
    internal static bool HasTaskbarButton(long extendedStyle, bool hasOwner, bool deletedFromTaskbar)
    {
        var optedIn = (extendedStyle & AppWindow) != 0;
        return !deletedFromTaskbar
            && (!hasOwner || optedIn)
            && (optedIn || (extendedStyle & ToolWindow) == 0)
            && (optedIn || (extendedStyle & NoActivate) == 0);
    }

    /// <summary>A cloaked window is hidden from the taskbar, except for the shell cloak used by virtual desktops.</summary>
    internal static bool IsHiddenCloak(int cloaked) => (cloaked & ~CloakedShell) != 0;

    /// <summary>Immersive shell frames without WS_EX_WINDOWEDGE are internal surfaces, not app windows.</summary>
    internal static bool IsHiddenImmersiveFrame(ReadOnlySpan<char> className, long extendedStyle) =>
        (extendedStyle & WindowEdge) == 0
        && (className.SequenceEqual("Windows.UI.Core.CoreWindow")
            || className.SequenceEqual("ApplicationFrameWindow")
            || className.SequenceEqual("StartMenuSizingFrame")
            || className.SequenceEqual("Shell_LightDismissOverlay"));
}
