using System.Buffers;
using System.IO;
using System.Runtime.InteropServices;
using System.Windows.Threading;
using OpenTaskbar.Models;

namespace OpenTaskbar.Services;

public sealed class NativeWindowTracker : IDisposable
{
    private const uint EventObjectCreate = 0x8000;
    private const uint EventObjectDestroy = 0x8001;
    private const uint EventObjectShow = 0x8002;
    private const uint EventObjectHide = 0x8003;
    private const uint EventObjectLocationChange = 0x800B;
    private const uint EventObjectNameChange = 0x800C;
    private const uint EventObjectCloaked = 0x8017;
    private const uint EventObjectUncloaked = 0x8018;
    private const uint EventSystemAlert = 0x0002;
    private const uint EventSystemForeground = 0x0003;
    private const uint WineventOutOfContext = 0x0000;
    private const uint WineventSkipOwnProcess = 0x0002;
    private const uint WinEventFlags = WineventOutOfContext | WineventSkipOwnProcess;
    private const int ObjectWindow = 0;
    private const int ChildIdSelf = 0;
    private const int WindowOwner = 4;
    private const int GwlExStyle = -20;
    private const uint GaRootOwner = 3;
    private const uint WmClose = 0x0010;
    private const int ShowRestore = 9;
    private const int ShowMinimize = 6;
    private const uint ProcessQueryLimitedInformation = 0x1000;
    private const int ErrorInsufficientBuffer = 122;
    private const int DwmwaCloaked = 14;
    private static readonly string WindowsSystemAppsDirectory = Path.Combine(
        Environment.GetFolderPath(Environment.SpecialFolder.Windows), "SystemApps") + Path.DirectorySeparatorChar;

    private readonly Dispatcher _dispatcher;
    private readonly DispatcherTimer _refreshTimer;
    private readonly DispatcherTimer _locationTimer;
    private readonly WinEventDelegate _eventCallback;
    private readonly EnumWindowsDelegate _enumCallback;
    private readonly HashSet<nint> _notificationWindows = [];
    // Only suppresses redundant name-change events from helper windows. Eligibility is always re-evaluated
    // during enumeration: a cached "ignore" entry would otherwise survive style changes and HWND reuse.
    private readonly HashSet<nint> _quietWindows = [];
    private readonly HashSet<nint> _snapshotHandles = [];
    private readonly Dictionary<nint, TrackedWindow> _previousByHandle = [];
    private readonly List<TrackedWindow> _pendingWindows = [];
    private readonly char[] _titleBuffer = new char[1024];
    private readonly char[] _classBuffer = new char[256];
    private nint _windowHook;
    private nint _locationHook;
    private nint _nameChangeHook;
    private nint _cloakHook;
    private nint _systemEventHook;
    private nint _excludedWindow;
    private nint _lastForegroundWindow;
    private nint _pendingLocationForeground;
    private bool _foregroundPublished;
    private int _refreshQueued;
    private int _urgentRefresh;
    private bool _disposed;

    public NativeWindowTracker(Dispatcher dispatcher)
    {
        _dispatcher = dispatcher;
        _refreshTimer = new DispatcherTimer(DispatcherPriority.Input, dispatcher)
        {
            Interval = TimeSpan.FromMilliseconds(60)
        };
        _refreshTimer.Tick += RefreshTimer_Tick;
        _locationTimer = new DispatcherTimer(DispatcherPriority.Background, dispatcher)
        {
            Interval = TimeSpan.FromMilliseconds(100)
        };
        _locationTimer.Tick += LocationTimer_Tick;
        _eventCallback = OnWinEvent;
        _enumCallback = CollectWindow;
    }

    public event EventHandler<IReadOnlyList<TrackedWindow>>? WindowsChanged;

    public event Action<nint>? ForegroundWindowChanged;

    public IReadOnlyList<TrackedWindow> Snapshot { get; private set; } = [];

    public bool NotificationsAvailable => _systemEventHook != 0;

    public void Start(nint excludedWindow)
    {
        ObjectDisposedException.ThrowIf(_disposed, this);
        if (_windowHook != 0) return;
        _excludedWindow = excludedWindow;
        _windowHook = SetWinEventHook(EventObjectCreate, EventObjectHide, 0, _eventCallback, 0, 0, WinEventFlags);
        if (_windowHook == 0)
        {
            throw new InvalidOperationException("ErrorWindowEvents");
        }

        _nameChangeHook = SetWinEventHook(EventObjectNameChange, EventObjectNameChange, 0, _eventCallback, 0, 0, WinEventFlags);
        if (_nameChangeHook == 0)
        {
            UnhookWinEvent(_windowHook);
            _windowHook = 0;
            throw new InvalidOperationException("ErrorWindowTitleHook");
        }

        // Cloak changes are the only notification for a suspended/background UWP frame becoming (in)visible;
        // without them the snapshot can keep a phantom app button until an unrelated window event arrives.
        _cloakHook = SetWinEventHook(EventObjectCloaked, EventObjectUncloaked, 0, _eventCallback, 0, 0, WinEventFlags);
        _locationHook = SetWinEventHook(EventObjectLocationChange, EventObjectLocationChange, 0, _eventCallback, 0, 0, WinEventFlags);
        _systemEventHook = SetWinEventHook(EventSystemAlert, EventSystemForeground, 0, _eventCallback, 0, 0, WinEventFlags);
        RefreshWindows();
        PublishForeground(NormalizedForeground());
    }

    private nint NormalizedForeground()
    {
        var foreground = GetForegroundWindow();
        return foreground != 0 ? NormalizeRootWindow(foreground) : 0;
    }

    /// <summary>Publishes a foreground change once, including the 0 handle when focus returns to the desktop.</summary>
    private void PublishForeground(nint target)
    {
        if (_foregroundPublished && _lastForegroundWindow == target) return;
        _foregroundPublished = true;
        _lastForegroundWindow = target;
        ForegroundWindowChanged?.Invoke(target);
    }

    // This callback is invoked from native code; an exception escaping it is undefined behaviour and can take
    // down the process, so isolate all work (including subscriber callbacks) behind a logged guard.
    private void OnWinEvent(nint hook, uint eventType, nint windowHandle, int objectId, int childId, uint eventThread, uint eventTime)
    {
        try
        {
            HandleWinEvent(eventType, windowHandle, objectId, childId);
        }
        catch (Exception ex)
        {
            AppLog.Warning($"Window event {eventType:X} failed: {ex.Message}");
        }
    }

    private void HandleWinEvent(uint eventType, nint windowHandle, int objectId, int childId)
    {
        if (_disposed) return;

        if (eventType == EventObjectLocationChange)
        {
            if (objectId == ObjectWindow && childId == ChildIdSelf && windowHandle != 0)
            {
                var foreground = GetForegroundWindow();
                if (foreground != 0)
                {
                    var foregroundRoot = NormalizeRootWindow(foreground);
                    if (windowHandle == foreground || windowHandle == foregroundRoot)
                    {
                        _pendingLocationForeground = foregroundRoot;
                        if (!_locationTimer.IsEnabled) _locationTimer.Start();
                    }
                }
            }
            return;
        }

        if (eventType == EventSystemAlert && windowHandle != 0)
        {
            var target = NormalizeRootWindow(windowHandle);
            if (target != 0 && target != _excludedWindow && IsWindowVisible(target) && GetWindow(target, WindowOwner) == 0)
            {
                _notificationWindows.Add(target);
                QueueRefresh();
            }
            return;
        }

        if (eventType == EventSystemForeground)
        {
            var target = windowHandle != 0 ? NormalizeRootWindow(windowHandle) : 0;
            _notificationWindows.Remove(windowHandle);
            _notificationWindows.Remove(target);
            _locationTimer.Stop();
            QueueRefresh(urgent: true);
            PublishForeground(target);
            return;
        }

        if (objectId != ObjectWindow || childId != ChildIdSelf
            || eventType is not (EventObjectCreate or EventObjectDestroy or EventObjectShow or EventObjectHide
                or EventObjectNameChange or EventObjectCloaked or EventObjectUncloaked))
        {
            return;
        }

        if (eventType is EventObjectCreate or EventObjectDestroy)
        {
            _quietWindows.Remove(windowHandle);
        }
        else if (_quietWindows.Contains(windowHandle) && eventType == EventObjectNameChange)
        {
            return;
        }

        QueueRefresh(urgent: eventType is EventObjectShow or EventObjectDestroy or EventObjectCloaked or EventObjectUncloaked
            || (eventType == EventObjectNameChange && !_snapshotHandles.Contains(windowHandle)));
    }

    private void LocationTimer_Tick(object? sender, EventArgs e)
    {
        _locationTimer.Stop();
        if (_disposed) return;
        var foreground = GetForegroundWindow();
        var target = foreground != 0 ? NormalizeRootWindow(foreground) : _pendingLocationForeground;
        _pendingLocationForeground = 0;
        if (target != 0 && IsWindow(target)) PublishForeground(target);
    }

    private void QueueRefresh(bool urgent = false)
    {
        if (urgent) Interlocked.Exchange(ref _urgentRefresh, 1);
        if (Interlocked.Exchange(ref _refreshQueued, 1) != 0)
        {
            return;
        }

        _dispatcher.InvokeAsync(() =>
        {
            Interlocked.Exchange(ref _refreshQueued, 0);
            if (!_disposed)
            {
                if (Interlocked.Exchange(ref _urgentRefresh, 0) != 0)
                {
                    FlushRefresh();
                }
                else if (!_refreshTimer.IsEnabled)
                {
                    // Bound the wait from the first event; later events must not postpone discovery.
                    _refreshTimer.Start();
                }
            }
        }, DispatcherPriority.Input);
    }

    private void RefreshTimer_Tick(object? sender, EventArgs e)
    {
        FlushRefresh();
    }

    /// <summary>
    /// Drops cached foreground state after a secure-desktop transition. Without this, LogonUI's handle can survive
    /// as the "previous foreground window" and be reactivated on the first click after unlocking.
    /// </summary>
    public void ResyncForeground()
    {
        _refreshTimer.Stop();
        _locationTimer.Stop();
        _pendingLocationForeground = 0;
        RefreshWindows();
        PublishForeground(NormalizedForeground());
    }

    private void FlushRefresh()
    {
        _refreshTimer.Stop();
        RefreshWindows();
        PublishForeground(NormalizedForeground());
    }

    private void RefreshWindows()
    {
        _pendingWindows.Clear();
        _previousByHandle.Clear();
        for (var i = 0; i < Snapshot.Count; i++)
        {
            var item = Snapshot[i];
            _previousByHandle[item.Handle] = item;
        }

        EnumWindows(_enumCallback, 0);

        _notificationWindows.RemoveWhere(windowHandle => !IsWindow(windowHandle));
        _quietWindows.RemoveWhere(windowHandle => !IsWindow(windowHandle));

        if (Snapshot.SequenceEqual(_pendingWindows))
        {
            _pendingWindows.Clear();
            return;
        }

        var next = _pendingWindows.ToArray();
        _pendingWindows.Clear();
        _snapshotHandles.Clear();
        for (var i = 0; i < next.Length; i++)
        {
            _snapshotHandles.Add(next[i].Handle);
        }

        Snapshot = next;
        WindowsChanged?.Invoke(this, next);
    }

    // EnumWindows invokes this delegate from native code; never let an exception cross that boundary.
    private bool CollectWindow(nint windowHandle, nint state)
    {
        try
        {
            return CollectWindowCore(windowHandle);
        }
        catch (Exception ex)
        {
            AppLog.Warning($"Window enumeration failed for {windowHandle}: {ex.Message}");
            return true;
        }
    }

    private bool CollectWindowCore(nint windowHandle)
    {
        if (windowHandle == _excludedWindow || !IsWindowVisible(windowHandle)) return true;

        GetWindowThreadProcessId(windowHandle, out var processId);
        if (processId == Environment.ProcessId) return true;

        var extendedStyle = GetWindowLongPtr(windowHandle, GwlExStyle).ToInt64();
        // Tray-only apps leave visible helper windows behind: non-activatable overlays, windows removed by
        // ITaskbarList::DeleteTab, tool windows and hidden immersive frames. Match the shell's rules so none
        // of them is promoted to a normal running-app button.
        if (!TaskbarWindowRules.HasTaskbarButton(extendedStyle, GetWindow(windowHandle, WindowOwner) != 0,
                GetProp(windowHandle, TaskbarWindowRules.DeletedFromTaskbarProperty) != 0))
        {
            return true;
        }

        // DWM_CLOAKED_SHELL is kept for windows on other virtual desktops; all other cloak reasons hide the window.
        if (DwmGetWindowAttribute(windowHandle, DwmwaCloaked, out var cloaked, sizeof(int)) == 0
            && TaskbarWindowRules.IsHiddenCloak(cloaked))
        {
            return true;
        }

        var classLength = GetClassName(windowHandle, _classBuffer, _classBuffer.Length);
        if (classLength > 0)
        {
            ReadOnlySpan<char> cls = _classBuffer.AsSpan(0, classLength);
            if (TaskbarWindowRules.IsHiddenImmersiveFrame(cls, extendedStyle))
            {
                _quietWindows.Add(windowHandle);
                return true;
            }
        }

        var titleLength = GetWindowTextLength(windowHandle);
        if (titleLength <= 0) return true;

        var readCapacity = Math.Min(titleLength + 1, _titleBuffer.Length);
        var actualLength = GetWindowText(windowHandle, _titleBuffer, readCapacity);
        if (actualLength <= 0) return true;

        ReadOnlySpan<char> trimmedSpan = _titleBuffer.AsSpan(0, actualLength).Trim();
        if (trimmedSpan.IsEmpty) return true;

        _previousByHandle.TryGetValue(windowHandle, out var previous);
        // A first lookup can fail for a just-starting process; retry it instead of hiding the app forever.
        var executablePath = previous?.ExecutablePath ?? GetExecutablePath(windowHandle);
        if (IsWindowsSystemApp(executablePath) || IsExplorerShellWindow(executablePath, windowHandle))
        {
            _quietWindows.Add(windowHandle);
            return true;
        }

        var hasNotification = _notificationWindows.Contains(windowHandle);
        if (previous is not null)
        {
            var title = trimmedSpan.SequenceEqual(previous.Title) ? previous.Title : trimmedSpan.ToString();
            _pendingWindows.Add(previous with { Title = title, HasNotification = hasNotification });
        }
        else
        {
            _pendingWindows.Add(new TrackedWindow(windowHandle, trimmedSpan.ToString(), executablePath, hasNotification));
        }

        return true;
    }

    private static bool IsForegroundRoot(nint windowHandle)
    {
        var active = GetForegroundWindow();
        return active == windowHandle
            || (active != 0 && NormalizeRootWindow(active) == NormalizeRootWindow(windowHandle));
    }

    private static nint NormalizeRootWindow(nint windowHandle)
    {
        if (windowHandle == 0) return 0;
        var root = GetAncestor(windowHandle, GaRootOwner);
        return root != 0 ? root : windowHandle;
    }

    public static bool Activate(nint windowHandle)
    {
        if (!IsWindow(windowHandle))
        {
            return false;
        }

        if (IsIconic(windowHandle))
        {
            ShowWindowAsync(windowHandle, ShowRestore);
        }

        AllowSetForegroundWindow(-1);
        // Fast path: a dock click is the last input event, so this process may set the foreground window directly.
        // Attaching input queues and raising the window first waits on both apps' UI threads and cost 50-250 ms per
        // click; keep that only as the fallback for when Windows refuses the plain request.
        if (SetForegroundWindow(windowHandle) && IsForegroundRoot(windowHandle))
        {
            return true;
        }

        var foreground = GetForegroundWindow();
        var foregroundThread = foreground != 0 ? GetWindowThreadProcessId(foreground, out _) : 0;
        var targetThread = GetWindowThreadProcessId(windowHandle, out _);
        var currentThread = GetCurrentThreadId();
        var attachedForeground = foregroundThread != 0 && foregroundThread != currentThread
            && AttachThreadInput(currentThread, foregroundThread, true);
        var attachedTarget = targetThread != 0 && targetThread != currentThread && targetThread != foregroundThread
            && AttachThreadInput(currentThread, targetThread, true);
        try
        {
            BringWindowToTop(windowHandle);
            if (SetForegroundWindow(windowHandle))
            {
                return true;
            }
        }
        finally
        {
            if (attachedTarget)
            {
                AttachThreadInput(currentThread, targetThread, false);
            }
            if (attachedForeground)
            {
                AttachThreadInput(currentThread, foregroundThread, false);
            }
        }

        SwitchToThisWindow(windowHandle, true);
        var active = GetForegroundWindow();
        return active == windowHandle
            || (active != 0 && NormalizeRootWindow(active) == NormalizeRootWindow(windowHandle));
    }

    public static nint GetForegroundApplicationWindow()
    {
        var foreground = GetForegroundWindow();
        GetWindowThreadProcessId(foreground, out var processId);
        if (foreground == 0 || processId == Environment.ProcessId) return 0;
        var root = GetAncestor(foreground, GaRootOwner);
        return root != 0 ? root : foreground;
    }

    public static bool ToggleActivation(nint windowHandle, string executablePath, nint? foregroundAtPress = null)
    {
        if (!IsWindow(windowHandle))
        {
            return false;
        }

        var foregroundWindow = foregroundAtPress ?? GetForegroundApplicationWindow();
        if (foregroundWindow != 0)
        {
            var foregroundRoot = NormalizeRootWindow(foregroundWindow);
            if (IsWindow(foregroundRoot) && !IsIconic(foregroundRoot)
                && (foregroundRoot == windowHandle
                    || (!string.IsNullOrEmpty(executablePath)
                        && string.Equals(GetExecutablePath(foregroundRoot), executablePath, StringComparison.OrdinalIgnoreCase))))
            {
                return ShowWindowAsync(foregroundRoot, ShowMinimize);
            }
        }

        return Activate(windowHandle);
    }

    public static bool RequestClose(nint windowHandle)
    {
        return IsWindow(windowHandle) && PostMessage(windowHandle, WmClose, 0, 0);
    }

    public void Dispose()
    {
        if (_disposed)
        {
            return;
        }

        _disposed = true;
        _refreshTimer.Stop();
        _refreshTimer.Tick -= RefreshTimer_Tick;
        _locationTimer.Stop();
        _locationTimer.Tick -= LocationTimer_Tick;
        if (_windowHook != 0)
        {
            UnhookWinEvent(_windowHook);
            _windowHook = 0;
        }

        if (_locationHook != 0)
        {
            UnhookWinEvent(_locationHook);
            _locationHook = 0;
        }

        if (_nameChangeHook != 0)
        {
            UnhookWinEvent(_nameChangeHook);
            _nameChangeHook = 0;
        }

        if (_cloakHook != 0)
        {
            UnhookWinEvent(_cloakHook);
            _cloakHook = 0;
        }

        if (_systemEventHook != 0)
        {
            UnhookWinEvent(_systemEventHook);
            _systemEventHook = 0;
        }

        _notificationWindows.Clear();
        _quietWindows.Clear();
        _snapshotHandles.Clear();
        _previousByHandle.Clear();
        _pendingWindows.Clear();
        _excludedWindow = 0;
        _lastForegroundWindow = 0;
        _pendingLocationForeground = 0;
        _foregroundPublished = false;
        Snapshot = [];
    }

    private delegate void WinEventDelegate(nint hook, uint eventType, nint windowHandle, int objectId, int childId, uint eventThread, uint eventTime);
    private delegate bool EnumWindowsDelegate(nint windowHandle, nint state);

    [DllImport("dwmapi.dll")]
    private static extern int DwmGetWindowAttribute(nint hwnd, int dwAttribute, out int pvAttribute, int cbAttribute);

    [DllImport("user32.dll", SetLastError = true)]
    private static extern nint SetWinEventHook(uint eventMin, uint eventMax, nint eventHookModule, WinEventDelegate callback, uint processId, uint threadId, uint flags);

    [DllImport("user32.dll", SetLastError = true)]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool UnhookWinEvent(nint hook);

    [DllImport("user32.dll")]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool EnumWindows(EnumWindowsDelegate callback, nint state);

    [DllImport("user32.dll")]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool IsWindowVisible(nint windowHandle);

    [DllImport("user32.dll")]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool IsWindow(nint windowHandle);

    [DllImport("user32.dll")]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool IsIconic(nint windowHandle);

    [DllImport("user32.dll")]
    private static extern nint GetWindow(nint windowHandle, int command);

    [DllImport("user32.dll", EntryPoint = "GetPropW", CharSet = CharSet.Unicode)]
    private static extern nint GetProp(nint windowHandle, string propertyName);

    [DllImport("user32.dll", CharSet = CharSet.Unicode)]
    private static extern int GetWindowText(nint windowHandle, char[] text, int maximumCount);

    [DllImport("user32.dll", CharSet = CharSet.Unicode)]
    private static extern int GetWindowTextLength(nint windowHandle);

    [DllImport("user32.dll")]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool SetForegroundWindow(nint windowHandle);

    [DllImport("user32.dll")]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool BringWindowToTop(nint windowHandle);

    [DllImport("user32.dll")]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool AllowSetForegroundWindow(int processId);

    [DllImport("user32.dll")]
    private static extern void SwitchToThisWindow(nint windowHandle, [MarshalAs(UnmanagedType.Bool)] bool altTab);

    [DllImport("user32.dll")]
    private static extern uint GetWindowThreadProcessId(nint windowHandle, out uint processId);

    [DllImport("kernel32.dll")]
    private static extern uint GetCurrentThreadId();

    private static string? GetExecutablePath(nint windowHandle)
    {
        var threadId = GetWindowThreadProcessId(windowHandle, out var processId);
        if (threadId == 0 || processId == 0)
        {
            return null;
        }

        var processHandle = OpenProcess(ProcessQueryLimitedInformation, false, processId);
        if (processHandle == 0)
        {
            return null;
        }

        var buffer = ArrayPool<char>.Shared.Rent(1024);
        try
        {
            var length = (uint)buffer.Length;
            if (QueryFullProcessImageName(processHandle, 0, buffer, ref length))
            {
                return new string(buffer, 0, (int)length);
            }

            if (Marshal.GetLastWin32Error() == ErrorInsufficientBuffer)
            {
                ArrayPool<char>.Shared.Return(buffer);
                buffer = ArrayPool<char>.Shared.Rent(32768);
                length = (uint)buffer.Length;
                if (QueryFullProcessImageName(processHandle, 0, buffer, ref length))
                {
                    return new string(buffer, 0, (int)length);
                }
            }

            return null;
        }
        finally
        {
            ArrayPool<char>.Shared.Return(buffer);
            CloseHandle(processHandle);
        }
    }

    // Packaged processes such as WhatsApp.Root.exe report their AppUserModelID even when their windows do not.
    internal static string? GetPackagedAppId(nint windowHandle)
    {
        var threadId = GetWindowThreadProcessId(windowHandle, out var processId);
        if (threadId == 0 || processId == 0)
        {
            return null;
        }

        var processHandle = OpenProcess(ProcessQueryLimitedInformation, false, processId);
        if (processHandle == 0)
        {
            return null;
        }

        try
        {
            var length = 0u;
            if (GetApplicationUserModelId(processHandle, ref length, null) != ErrorInsufficientBuffer || length == 0)
            {
                return null;
            }

            var buffer = ArrayPool<char>.Shared.Rent((int)length);
            try
            {
                if (GetApplicationUserModelId(processHandle, ref length, buffer) != 0 || length == 0) return null;
                var charCount = (int)length;
                if (charCount > 0 && buffer[charCount - 1] == '\0') charCount--;
                return charCount > 0 ? new string(buffer, 0, charCount) : null;
            }
            finally
            {
                ArrayPool<char>.Shared.Return(buffer);
            }
        }
        finally
        {
            CloseHandle(processHandle);
        }
    }

    private static bool IsWindowsSystemApp(string? executablePath)
    {
        return executablePath is not null
            && executablePath.StartsWith(WindowsSystemAppsDirectory, StringComparison.OrdinalIgnoreCase);
    }

    private bool IsExplorerShellWindow(string? executablePath, nint windowHandle)
    {
        if (!string.Equals(Path.GetFileName(executablePath), "explorer.exe", StringComparison.OrdinalIgnoreCase))
        {
            return false;
        }

        var length = GetClassName(windowHandle, _classBuffer, _classBuffer.Length);
        if (length <= 0) return true;
        ReadOnlySpan<char> cls = _classBuffer.AsSpan(0, length);
        return !cls.SequenceEqual("CabinetWClass") && !cls.SequenceEqual("ExploreWClass");
    }

    [DllImport("kernel32.dll", CharSet = CharSet.Unicode)]
    private static extern int GetApplicationUserModelId(nint processHandle, ref uint length, char[]? applicationUserModelId);

    // GetWindowLongPtrW does not exist on 32-bit Windows; there it is a macro for GetWindowLongW.
    private static nint GetWindowLongPtr(nint windowHandle, int index) =>
        IntPtr.Size == 8 ? GetWindowLongPtr64(windowHandle, index) : GetWindowLong32(windowHandle, index);

    [DllImport("user32.dll", EntryPoint = "GetWindowLongPtrW")]
    private static extern nint GetWindowLongPtr64(nint windowHandle, int index);

    [DllImport("user32.dll", EntryPoint = "GetWindowLongW")]
    private static extern int GetWindowLong32(nint windowHandle, int index);

    [DllImport("user32.dll")]
    private static extern nint GetForegroundWindow();

    [DllImport("user32.dll")]
    private static extern nint GetAncestor(nint windowHandle, uint flags);

    [DllImport("user32.dll", CharSet = CharSet.Unicode)]
    private static extern int GetClassName(nint windowHandle, char[] className, int maximumCount);

    [DllImport("user32.dll", SetLastError = true)]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool PostMessage(nint windowHandle, uint message, nint wParam, nint lParam);

    [DllImport("user32.dll")]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool AttachThreadInput(uint threadIdAttach, uint threadIdAttachTo, [MarshalAs(UnmanagedType.Bool)] bool attach);

    [DllImport("kernel32.dll", SetLastError = true)]
    private static extern nint OpenProcess(uint desiredAccess, [MarshalAs(UnmanagedType.Bool)] bool inheritHandle, uint processId);

    [DllImport("kernel32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool QueryFullProcessImageName(nint processHandle, uint flags, char[] executablePath, ref uint size);

    [DllImport("kernel32.dll", SetLastError = true)]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool CloseHandle(nint handle);

    [DllImport("user32.dll")]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool ShowWindowAsync(nint windowHandle, int command);
}
