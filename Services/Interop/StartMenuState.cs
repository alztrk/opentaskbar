using System.Diagnostics;
using System.Runtime.InteropServices;

namespace OpenTaskbar.Services;

/// <summary>
/// Tracks whether the Windows Start menu is open so the dock can treat it like the native taskbar does: the
/// Start button toggles it instead of always reopening it, and auto-hide keeps the dock revealed while it is open.
/// A closed Start menu often remains the foreground window but is cloaked by DWM, so the open state follows the
/// cloak events of the Start menu window as well as foreground changes.
/// </summary>
internal sealed class StartMenuState : IDisposable
{
    /// <summary>
    /// Processes that host the Start menu: StartMenuExperienceHost on Windows 10 and Windows 11.
    /// SearchHost is intentionally excluded because it hosts Windows Search, which is a separate shell surface.
    /// </summary>
    internal static readonly string[] StartMenuProcessNames = ["StartMenuExperienceHost"];

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
    private nint _startWindow;

    public StartMenuState(TimeProvider? clock = null)
    {
        _clock = clock ?? TimeProvider.System;
        _cloakCallback = OnCloakEvent;
    }

    public event Action<nint>? StartMenuOpened;

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

    /// <summary>
    /// True when a Start button press should close the Start menu rather than open it.
    /// Unlike a global ambient timer, grace is strictly scoped to dismissal within the immediate pointer press gesture.
    /// </summary>
    public bool WasOpenAtPress(bool openNow, long pressTimestamp = 0)
    {
        if (_isOpen || openNow) return true;
        if (pressTimestamp == 0 || _closedAt == long.MinValue) return false;
        // If closed within 60ms around this specific pointer press, it was dismissed by the press itself.
        var delta = _closedAt - pressTimestamp;
        var deltaMs = delta * 1000.0 / _clock.TimestampFrequency;
        return deltaMs >= -60 && deltaMs <= 150;
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

    // Invoked from a native WinEvent hook: an escaping exception crosses the unmanaged boundary
    // and can terminate the process. Keep the body isolated behind a logged guard.
    private void OnCloakEvent(nint hook, uint eventType, nint windowHandle, int objectId, int childId, uint thread, uint time)
    {
        try
        {
            HandleCloakEvent(eventType, windowHandle, objectId);
        }
        catch (Exception ex)
        {
            AppLog.Warning($"Start-menu cloak event {eventType:X} failed: {ex.Message}");
        }
    }

    private void HandleCloakEvent(uint eventType, nint windowHandle, int objectId)
    {
        if (objectId != ObjectWindow || windowHandle == 0 || !IsStartMenuWindow(windowHandle)) return;
        if (eventType == EventObjectUncloaked)
        {
            if (!GetWindowRect(windowHandle, out var rect) || (rect.Right - rect.Left) <= 100 || (rect.Bottom - rect.Top) <= 100)
                return;
            _startWindow = windowHandle;
            StartMenuOpened?.Invoke(windowHandle);
            OnStartMenuCloakChanged(cloaked: false);
            return;
        }

        if (windowHandle == _startWindow || !IsStartMenuOpenNow())
            OnStartMenuCloakChanged(cloaked: true);
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

    /// <summary>
    /// True while the Start menu is actually on screen (visible and not cloaked). It is normally the foreground
    /// window, but Alt+Tab takes the foreground while the Start menu stays open beneath the switcher, so the
    /// last Start menu window is checked as well.
    /// </summary>
    public bool IsStartMenuOpenNow()
    {
        var foreground = GetForegroundWindow();
        if (IsStartMenuWindow(foreground))
        {
            _startWindow = foreground;
            return IsOnScreen(foreground);
        }
        if (_startWindow != 0 && IsStartMenuWindow(_startWindow) && IsOnScreen(_startWindow))
        {
            return true;
        }
        var windows = ShellShortcuts.FindStartMenuWindows();
        if (windows.Count > 0)
        {
            _startWindow = windows[0];
            return true;
        }
        return false;
    }

    private static bool IsOnScreen(nint windowHandle) => IsWindowVisible(windowHandle) && !IsCloaked(windowHandle);

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

    [DllImport("user32.dll")]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool GetWindowRect(nint windowHandle, out NativeRect rect);

    [StructLayout(LayoutKind.Sequential)]
    private struct NativeRect
    {
        public int Left;
        public int Top;
        public int Right;
        public int Bottom;
    }
}
