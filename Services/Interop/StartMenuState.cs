using System.Diagnostics;
using System.Runtime.InteropServices;

namespace OpenTaskbar.Services;

/// <summary>
/// Tracks whether the Windows Start menu is open so the dock's Start button can toggle it like the native taskbar
/// does instead of always reopening it. A closed Start menu often remains the foreground window but is cloaked
/// by DWM, so the open state follows the cloak events of the Start menu window as well as foreground changes.
/// </summary>
internal sealed class StartMenuState : IDisposable
{
    /// <summary>
    /// Processes that host the Start menu: StartMenuExperienceHost on Windows 10 and earlier Windows 11 builds,
    /// SearchHost for the redesigned Windows 11 Start menu.
    /// </summary>
    internal static readonly string[] StartMenuProcessNames = ["StartMenuExperienceHost", "SearchHost"];

    /// <summary>How long after the Start menu closed a press still counts as "it was open".</summary>
    internal static readonly TimeSpan DismissGrace = TimeSpan.FromMilliseconds(500);

    private const uint EventObjectCloaked = 0x8017;
    private const uint EventObjectUncloaked = 0x8018;
    private const uint WineventOutOfContext = 0x0000;
    private const uint WineventSkipOwnProcess = 0x0002;
    private const int ObjectWindow = 0;
    private const int DwmwaCloaked = 14;

    private readonly TimeProvider _clock;
    private readonly WinEventDelegate _cloakCallback;
    private nint _cloakHook;
    private bool _isOpen;
    private long _closedAt = long.MinValue;
    private uint _cachedProcessId;
    private bool _cachedIsStartMenu;

    public StartMenuState(TimeProvider? clock = null)
    {
        _clock = clock ?? TimeProvider.System;
        _cloakCallback = OnCloakEvent;
    }

    public bool IsOpen => _isOpen;

    /// <summary>Starts following the Start menu's cloak events; call on the UI thread once its message loop runs.</summary>
    public void StartWatching()
    {
        if (_cloakHook != 0) return;
        _cloakHook = SetWinEventHook(EventObjectCloaked, EventObjectUncloaked, 0, _cloakCallback, 0, 0,
            WineventOutOfContext | WineventSkipOwnProcess);
    }

    /// <summary>Call for every external foreground change.</summary>
    public void OnForegroundChanged(bool isStartMenu)
    {
        if (isStartMenu) MarkOpen();
        else MarkClosed();
    }

    /// <summary>Call when the Start menu window is cloaked (closed) or uncloaked (opened).</summary>
    public void OnStartMenuCloakChanged(bool cloaked)
    {
        if (cloaked) MarkClosed();
        else MarkOpen();
    }

    /// <summary>
    /// Call when the dock window itself becomes active (right-clicks activate it). The window hook skips our
    /// own process, so this is the only foreground sign that the Start menu lost to the dock.
    /// </summary>
    public void OnDockActivated() => MarkClosed();

    /// <summary>True when a Start button press should close the Start menu rather than open it.</summary>
    public bool WasOpenAtPress(bool openNow)
    {
        if (_isOpen || openNow) return true;
        return _closedAt != long.MinValue && _clock.GetElapsedTime(_closedAt) <= DismissGrace;
    }

    public void Reset()
    {
        _isOpen = false;
        _closedAt = long.MinValue;
    }

    private void MarkOpen()
    {
        _isOpen = true;
        _closedAt = long.MinValue;
    }

    private void MarkClosed()
    {
        if (!_isOpen) return;
        _isOpen = false;
        _closedAt = _clock.GetTimestamp();
    }

    private void OnCloakEvent(nint hook, uint eventType, nint windowHandle, int objectId, int childId, uint thread, uint time)
    {
        if (objectId != ObjectWindow || windowHandle == 0 || !IsStartMenuWindow(windowHandle)) return;
        // Only the window the user sees as Start matters: the foreground one (or, while closing, the one that was).
        if (eventType == EventObjectUncloaked && GetForegroundWindow() != windowHandle) return;
        OnStartMenuCloakChanged(cloaked: eventType == EventObjectCloaked);
    }

    internal static bool IsStartMenuProcessName(string? processName) =>
        processName is not null
        && StartMenuProcessNames.Contains(processName, StringComparer.OrdinalIgnoreCase);

    public bool IsStartMenuWindow(nint windowHandle)
    {
        if (windowHandle == 0) return false;
        GetWindowThreadProcessId(windowHandle, out var processId);
        if (processId == 0) return false;
        if (processId == _cachedProcessId) return _cachedIsStartMenu;

        bool isStartMenu;
        try
        {
            using var process = Process.GetProcessById((int)processId);
            isStartMenu = IsStartMenuProcessName(process.ProcessName);
        }
        catch (Exception ex) when (ex is ArgumentException or InvalidOperationException or System.ComponentModel.Win32Exception)
        {
            return false;
        }

        _cachedProcessId = processId;
        _cachedIsStartMenu = isStartMenu;
        return isStartMenu;
    }

    /// <summary>True while the Start menu is actually on screen (foreground and not cloaked).</summary>
    public bool IsStartMenuOpenNow()
    {
        var foreground = GetForegroundWindow();
        return IsStartMenuWindow(foreground) && IsWindowVisible(foreground) && !IsCloaked(foreground);
    }

    private static bool IsCloaked(nint windowHandle) =>
        DwmGetWindowAttribute(windowHandle, DwmwaCloaked, out var cloaked, sizeof(int)) == 0 && cloaked != 0;

    public void Dispose()
    {
        if (_cloakHook == 0) return;
        UnhookWinEvent(_cloakHook);
        _cloakHook = 0;
    }

    private delegate void WinEventDelegate(nint hook, uint eventType, nint windowHandle, int objectId, int childId, uint thread, uint time);

    [DllImport("user32.dll")]
    private static extern nint SetWinEventHook(uint eventMin, uint eventMax, nint module, WinEventDelegate callback, uint processId, uint threadId, uint flags);

    [DllImport("user32.dll")]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool UnhookWinEvent(nint hook);

    [DllImport("dwmapi.dll")]
    private static extern int DwmGetWindowAttribute(nint windowHandle, int attribute, out int value, int size);

    [DllImport("user32.dll")]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool IsWindowVisible(nint windowHandle);

    [DllImport("user32.dll")]
    private static extern nint GetForegroundWindow();

    [DllImport("user32.dll")]
    private static extern uint GetWindowThreadProcessId(nint windowHandle, out uint processId);
}
