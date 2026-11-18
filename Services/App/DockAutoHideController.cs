using System.Runtime.InteropServices;
using System.Windows;
using System.Windows.Media;
using System.Windows.Media.Animation;
using System.Windows.Threading;
using OpenTaskbar.Models;

namespace OpenTaskbar.Services;

public sealed class DockAutoHideController : IDisposable
{
    private const uint MonitorDefaultToNearest = 2;
    private const uint GaRootOwner = 3;
    private const int DwmwaExtendedFrameBounds = 9;
    private const int DwmwaCloaked = 14;

    private static readonly CubicEase EaseOut = CreateFrozenEase(EasingMode.EaseOut);
    private static readonly CubicEase EaseInOut = CreateFrozenEase(EasingMode.EaseInOut);

    private readonly Window _window;
    private readonly nint _windowHandle;
    private readonly TranslateTransform _dockTranslate;
    private readonly FrameworkElement _dockSurface;
    private readonly Func<bool> _isInteractionLocked;
    private readonly DispatcherTimer _hideTimer;
    private readonly DispatcherTimer _revealedGuardTimer;
    private readonly char[] _classNameBuffer = new char[64];
    private bool _disposed;

    public AutoHideMode Mode { get; private set; } = AutoHideMode.Disabled;

    /// <summary>The screen edge the dock slides towards when it hides.</summary>
    public Models.DockEdge Edge { get; set; } = Models.DockEdge.Bottom;
    public bool AnimationsEnabled { get; set; } = true;

    /// <summary>Optional physical-pixel bounds of the visible dock surface (excluding transparent headroom/margins).</summary>
    public Func<(int Left, int Top, int Right, int Bottom)?>? DockBoundsProvider { get; set; }

    /// <summary>Optional cursor-position override for headless/sandboxed verification tests.</summary>
    internal Func<(int X, int Y)?>? CursorPositionProvider { get; set; }

    private TimeSpan MotionDuration(double milliseconds) => AnimationsEnabled && SystemParameters.ClientAreaAnimation
        ? TimeSpan.FromMilliseconds(milliseconds) : TimeSpan.Zero;
    public bool IsHidden { get; private set; }
    public bool IsMouseOverDock { get; private set; }
    public bool IsMouseAtEdge { get; private set; }
    public bool HasOverlap { get; private set; }

    /// <summary>True while a shell menu (the Start menu) is open; the dock then stays revealed like the native taskbar.</summary>
    public Func<bool>? IsShellMenuOpen { get; set; }

    /// <summary>
    /// Identifies foreground windows that belong to a shell menu rather than an application. The Start menu stays
    /// the (cloaked) foreground window after it closes, so it must not decide whether an application covers the dock.
    /// </summary>
    public Func<nint, bool>? IsShellMenuWindow { get; set; }

    /// <summary>The last application window that held the foreground, used while a shell menu holds it instead.</summary>
    public Func<nint>? LastApplicationWindowProvider { get; set; }

    /// <summary>
    /// All top-level application windows. In Smart mode any of them covering the dock keeps it hidden, not only
    /// the foreground one.
    /// </summary>
    public Func<IEnumerable<nint>>? BackgroundWindowsProvider { get; set; }

    /// <summary>Seam for tests: supplies the OS foreground window without depending on a real desktop.</summary>
    internal Func<nint> ForegroundProvider { get; set; } = NativeWindowTracker.GetForegroundApplicationWindow;

    public event Action<bool>? VisibilityChanged;

    public DockAutoHideController(
        Window window,
        nint windowHandle,
        TranslateTransform dockTranslate,
        FrameworkElement dockSurface,
        Func<bool> isInteractionLocked)
    {
        _window = window;
        _windowHandle = windowHandle;
        _dockTranslate = dockTranslate;
        _dockSurface = dockSurface;
        _isInteractionLocked = isInteractionLocked;

        _hideTimer = new DispatcherTimer(DispatcherPriority.Normal)
        {
            Interval = TimeSpan.FromMilliseconds(DockMetrics.AutoHideDelayMs)
        };
        _hideTimer.Tick += HideTimer_Tick;
        _revealedGuardTimer = new DispatcherTimer(DispatcherPriority.Background)
        {
            Interval = TimeSpan.FromMilliseconds(200)
        };
        _revealedGuardTimer.Tick += RevealedGuardTimer_Tick;
    }

    /// <summary>
    /// Keeps an active guard running while the dock is revealed to catch missed pointer exits
    /// or foreground window shifts, stopping as soon as the dock hides.
    /// </summary>
    private void ArmRevealedGuard()
    {
        if (_disposed || Mode == AutoHideMode.Disabled || IsHidden) return;
        if (!_revealedGuardTimer.IsEnabled) _revealedGuardTimer.Start();
    }

    private void SyncRevealedGuardTimer()
    {
        if (_disposed || Mode == AutoHideMode.Disabled || IsHidden)
        {
            _revealedGuardTimer.Stop();
        }
        else if (!_revealedGuardTimer.IsEnabled)
        {
            _revealedGuardTimer.Start();
        }
    }

    private void RevealedGuardTimer_Tick(object? sender, EventArgs e)
    {
        if (_disposed || Mode == AutoHideMode.Disabled || IsHidden)
        {
            _revealedGuardTimer.Stop();
            return;
        }
        if (Mode == AutoHideMode.Smart)
        {
            RefreshSmartOverlap(GetForegroundWindow());
        }
        EvaluateState();
    }

    public void SetMode(AutoHideMode mode)
    {
        if (Mode == mode) return;
        Mode = mode;
        if (Mode == AutoHideMode.Disabled)
        {
            _hideTimer.Stop();
            _revealedGuardTimer.Stop();
            HasOverlap = false;
            if (IsHidden) AnimateReveal();
            return;
        }

        if (Mode == AutoHideMode.Smart)
        {
            RefreshSmartOverlap(GetForegroundWindow());
        }

        EvaluateState();
        ArmRevealedGuard();
    }

    public void OnMouseEnterDock()
    {
        IsMouseOverDock = true;
        _hideTimer.Stop();
        ArmRevealedGuard();
        if (IsHidden)
        {
            AnimateReveal();
        }
    }

    public void OnMouseLeaveDock()
    {
        IsMouseOverDock = false;
        EvaluateState();
    }

    public void OnMouseEnterEdge()
    {
        IsMouseAtEdge = true;
        _hideTimer.Stop();
        ArmRevealedGuard();
        if (IsHidden)
        {
            AnimateReveal();
        }
    }

    public void OnMouseLeaveEdge()
    {
        IsMouseAtEdge = false;
        EvaluateState();
    }

    public void CheckForegroundWindow(nint foregroundHwnd)
    {
        if (Mode == AutoHideMode.Disabled) return;
        // Always mode also re-evaluates so an opening Start menu reveals the hidden dock.
        if (Mode == AutoHideMode.Smart) RefreshSmartOverlap(foregroundHwnd);
        EvaluateState();
    }

    // A focusable widget popup activates our own dock window. That must not erase
    // the overlap state of the application beneath it; otherwise Smart auto-hide
    // stays revealed after dismissing the popup.
    private void RefreshSmartOverlap(nint foregroundHwnd)
    {
        if (foregroundHwnd == 0) foregroundHwnd = NativeWindowTracker.GetForegroundApplicationWindow();
        if (foregroundHwnd == 0) return;
        var root = GetAncestor(foregroundHwnd, GaRootOwner);
        var target = root != 0 ? root : foregroundHwnd;
        if (!IsWindow(target)) return;
        if (IsShellMenuWindow?.Invoke(target) == true)
        {
            // Judge the application beneath the Start menu rather than the menu itself.
            var application = LastApplicationWindowProvider?.Invoke() ?? 0;
            target = application != 0 && IsWindow(application) ? application : 0;
        }
        if (target != 0)
        {
            GetWindowThreadProcessId(target, out var pid);
            if (pid == Environment.ProcessId)
            {
                var fallback = ForegroundProvider();
                if (fallback != 0 && fallback != target)
                {
                    target = fallback;
                }
                else
                {
                    // Closing the Start menu from the dock leaves the dock itself in the foreground; judge the
                    // application that was in front before it, which still covers the dock area.
                    var application = LastApplicationWindowProvider?.Invoke() ?? 0;
                    target = application != 0 && application != target && IsWindow(application) ? application : 0;
                }
            }
        }

        // A small focused window in front of a maximized or full-screen one must not reveal the dock: the desktop
        // is still covered behind it, so any visible application window over the dock area keeps it hidden.
        HasOverlap = (target != 0 && EvaluateWindowOverlap(target)) || AnyOtherWindowCoversDock(target);
    }

    private bool AnyOtherWindowCoversDock(nint foregroundTarget)
    {
        if (BackgroundWindowsProvider?.Invoke() is not { } windows) return false;
        foreach (var window in windows)
        {
            if (window != foregroundTarget && EvaluateWindowOverlap(window)) return true;
        }
        return false;
    }

    public void EvaluateState()
    {
        if (_disposed || Mode == AutoHideMode.Disabled) return;
        if (!IsHidden) SyncPointerState();

        if (ShouldHide())
        {
            if (!IsHidden && !_hideTimer.IsEnabled)
            {
                _hideTimer.Start();
            }
        }
        else
        {
            _hideTimer.Stop();
            if (IsHidden)
            {
                AnimateReveal();
            }
        }
        SyncRevealedGuardTimer();
    }

    private void SyncPointerState()
    {
        (int X, int Y) cursor;
        if (CursorPositionProvider is not null)
        {
            if (CursorPositionProvider() is not { } providedCursor) return;
            cursor = providedCursor;
        }
        else if (GetCursorPos(out var nativeCursor))
        {
            cursor = (nativeCursor.X, nativeCursor.Y);
        }
        else
        {
            return;
        }
        var dpi = _windowHandle != 0 ? GetDpiForWindow(_windowHandle) : 96u;
        var scale = (dpi != 0 ? dpi : 96u) / 96d;
        var headroomPx = (int)Math.Ceiling(DockMetrics.Headroom * scale);

        (int Left, int Top, int Right, int Bottom)? boundsOpt = DockBoundsProvider?.Invoke();
        if (boundsOpt is null && _windowHandle != 0 && GetWindowRect(_windowHandle, out var winRect))
        {
            boundsOpt = (winRect.Left, winRect.Top, winRect.Right, winRect.Bottom);
        }

        if (boundsOpt is { } bounds)
        {
            var left = bounds.Left - (Edge == Models.DockEdge.Right ? headroomPx : 6);
            var right = bounds.Right + (Edge == Models.DockEdge.Left ? headroomPx : 6);
            var top = bounds.Top - (Edge == Models.DockEdge.Bottom ? headroomPx : 6);
            var bottom = bounds.Bottom + 6;
            IsMouseOverDock = cursor.X >= left && cursor.X <= right && cursor.Y >= top && cursor.Y <= bottom;
        }

        if (_windowHandle != 0)
        {
            var monitor = MonitorFromWindow(_windowHandle, MonitorDefaultToNearest);
            var info = new MonitorInfo { cbSize = (uint)Marshal.SizeOf<MonitorInfo>() };
            if (monitor != 0 && GetMonitorInfo(monitor, ref info))
            {
                var edgePx = Math.Max(4, (int)Math.Ceiling(DockMetrics.EdgeTriggerHeight * scale) + 2);
                var inMonitorX = cursor.X >= info.rcMonitor.Left && cursor.X <= info.rcMonitor.Right;
                var inMonitorY = cursor.Y >= info.rcMonitor.Top && cursor.Y <= info.rcMonitor.Bottom;
                var atEdge = inMonitorX && inMonitorY && Edge switch
                {
                    Models.DockEdge.Left => cursor.X <= info.rcMonitor.Left + edgePx,
                    Models.DockEdge.Right => cursor.X >= info.rcMonitor.Right - edgePx,
                    _ => cursor.Y >= info.rcMonitor.Bottom - edgePx
                };
                IsMouseAtEdge = atEdge;
            }
        }
    }

    private bool ShouldHide()
    {
        if (Mode == AutoHideMode.Disabled) return false;
        if (IsMouseOverDock || IsMouseAtEdge) return false;
        if (IsShellMenuOpen?.Invoke() == true) return false;
        if (_isInteractionLocked()) return false;
        if (Mode == AutoHideMode.Always) return true;
        if (Mode == AutoHideMode.Smart) return HasOverlap;
        return false;
    }

    private void HideTimer_Tick(object? sender, EventArgs e)
    {
        _hideTimer.Stop();
        if (!IsHidden) SyncPointerState();
        if (ShouldHide() && !IsHidden)
        {
            AnimateHide();
        }
    }

    public void AnimateReveal()
    {
        IsHidden = false;
        _hideTimer.Stop();
        ArmRevealedGuard();
        var duration = MotionDuration(DockMetrics.RevealMs);
        var vertical = Edge != Models.DockEdge.Bottom;
        if (duration == TimeSpan.Zero)
        {
            _dockTranslate.BeginAnimation(TranslateTransform.XProperty, null);
            _dockTranslate.BeginAnimation(TranslateTransform.YProperty, null);
            _dockTranslate.X = 0;
            _dockTranslate.Y = 0;
        }
        else
        {
            var anim = new DoubleAnimation
            {
                To = 0,
                Duration = duration,
                EasingFunction = EaseOut
            };
            _dockTranslate.BeginAnimation(TranslateTransform.XProperty, vertical ? anim : null);
            _dockTranslate.BeginAnimation(TranslateTransform.YProperty, vertical ? null : anim);
        }

        VisibilityChanged?.Invoke(true);
    }

    public void AnimateHide()
    {
        IsHidden = true;
        _revealedGuardTimer.Stop();
        var vertical = Edge != Models.DockEdge.Bottom;
        double hideOffset = Math.Max(DockMetrics.HideMinimumOffset, (vertical ? _dockSurface.ActualWidth : _dockSurface.ActualHeight) + 16);
        if (Edge == Models.DockEdge.Left) hideOffset = -hideOffset;
        var duration = MotionDuration(DockMetrics.HideMs);
        if (duration == TimeSpan.Zero)
        {
            _dockTranslate.BeginAnimation(TranslateTransform.XProperty, null);
            _dockTranslate.BeginAnimation(TranslateTransform.YProperty, null);
            _dockTranslate.X = vertical ? hideOffset : 0;
            _dockTranslate.Y = vertical ? 0 : hideOffset;
        }
        else
        {
            var anim = new DoubleAnimation
            {
                To = hideOffset,
                Duration = duration,
                EasingFunction = EaseInOut
            };
            _dockTranslate.BeginAnimation(vertical ? TranslateTransform.XProperty : TranslateTransform.YProperty, anim);
        }

        VisibilityChanged?.Invoke(false);
    }

    private bool EvaluateWindowOverlap(nint hwnd)
    {
        if (hwnd == 0) return false;
        var root = GetAncestor(hwnd, GaRootOwner);
        var target = root != 0 ? root : hwnd;

        if (target == _windowHandle || !IsWindow(target) || !IsWindowVisible(target) || IsIconic(target))
            return false;

        // Cloaked windows (other virtual desktops, suspended apps, a closed Start menu) are not on screen.
        if (DwmGetWindowAttribute(target, DwmwaCloaked, out int cloaked, sizeof(int)) == 0 && cloaked != 0)
            return false;

        GetWindowThreadProcessId(target, out var processId);
        if (processId == Environment.ProcessId)
            return false;

        var classLen = GetClassName(target, _classNameBuffer, _classNameBuffer.Length);
        if (classLen > 0)
        {
            ReadOnlySpan<char> cls = _classNameBuffer.AsSpan(0, classLen);
            if (cls.SequenceEqual("Progman")
                || cls.SequenceEqual("WorkerW")
                || cls.SequenceEqual("Shell_TrayWnd")
                || cls.SequenceEqual("Shell_SecondaryTrayWnd"))
            {
                return false;
            }
        }

        var dockMonitor = MonitorFromWindow(_windowHandle, MonitorDefaultToNearest);
        var windowMonitor = MonitorFromWindow(target, MonitorDefaultToNearest);
        if (dockMonitor != windowMonitor)
            return false;

        if (IsZoomed(target))
            return true;

        if (DwmGetWindowAttribute(target, DwmwaExtendedFrameBounds, out Rect winRect, Marshal.SizeOf<Rect>()) != 0
            && !GetWindowRect(target, out winRect))
        {
            return false;
        }

        Rect dockRect;
        if (DockBoundsProvider?.Invoke() is { } provided)
        {
            dockRect = new Rect
            {
                Left = provided.Left,
                Top = provided.Top,
                Right = provided.Right,
                Bottom = provided.Bottom
            };
        }
        else
        {
            if (!GetWindowRect(_windowHandle, out dockRect)) return false;
            var dpi = GetDpiForWindow(_windowHandle);
            var headroomPx = (int)Math.Round(DockMetrics.Headroom * (dpi != 0 ? dpi : 96u) / 96d);
            switch (Edge)
            {
                case Models.DockEdge.Left:
                    dockRect.Right = Math.Max(dockRect.Left, dockRect.Right - headroomPx);
                    break;
                case Models.DockEdge.Right:
                    dockRect.Left = Math.Min(dockRect.Right, dockRect.Left + headroomPx);
                    break;
                default:
                    dockRect.Top = Math.Min(dockRect.Bottom, dockRect.Top + headroomPx);
                    break;
            }
        }

        return winRect.Left < dockRect.Right
            && winRect.Right > dockRect.Left
            && winRect.Bottom > dockRect.Top
            && winRect.Top < dockRect.Bottom;
    }

    private static CubicEase CreateFrozenEase(EasingMode mode)
    {
        var ease = new CubicEase { EasingMode = mode };
        ease.Freeze();
        return ease;
    }

    public void Dispose()
    {
        if (_disposed) return;
        _disposed = true;
        _hideTimer.Stop();
        _hideTimer.Tick -= HideTimer_Tick;
        _revealedGuardTimer.Stop();
        _revealedGuardTimer.Tick -= RevealedGuardTimer_Tick;
    }

    [StructLayout(LayoutKind.Sequential)]
    private struct Rect
    {
        public int Left;
        public int Top;
        public int Right;
        public int Bottom;
    }

    [StructLayout(LayoutKind.Sequential)]
    private struct NativePoint
    {
        public int X;
        public int Y;
    }

    [StructLayout(LayoutKind.Sequential)]
    private struct MonitorInfo
    {
        public uint cbSize;
        public Rect rcMonitor;
        public Rect rcWork;
        public uint dwFlags;
    }

    [DllImport("user32.dll")]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool GetCursorPos(out NativePoint lpPoint);

    [DllImport("user32.dll", CharSet = CharSet.Unicode)]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool GetMonitorInfo(nint hMonitor, ref MonitorInfo lpmi);

    [DllImport("dwmapi.dll")]
    private static extern int DwmGetWindowAttribute(nint hwnd, int dwAttribute, out Rect pvAttribute, int cbAttribute);

    [DllImport("dwmapi.dll")]
    private static extern int DwmGetWindowAttribute(nint hwnd, int dwAttribute, out int pvAttribute, int cbAttribute);

    [DllImport("user32.dll")]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool GetWindowRect(nint hWnd, out Rect lpRect);

    [DllImport("user32.dll")]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool IsZoomed(nint hWnd);

    [DllImport("user32.dll")]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool IsWindow(nint hWnd);

    [DllImport("user32.dll")]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool IsWindowVisible(nint hWnd);

    [DllImport("user32.dll")]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool IsIconic(nint hWnd);

    [DllImport("user32.dll")]
    private static extern nint MonitorFromWindow(nint hWnd, uint dwFlags);

    [DllImport("user32.dll")]
    private static extern nint GetAncestor(nint hwnd, uint gaFlags);

    [DllImport("user32.dll")]
    private static extern nint GetForegroundWindow();

    [DllImport("user32.dll")]
    private static extern uint GetWindowThreadProcessId(nint hWnd, out uint lpdwProcessId);

    [DllImport("user32.dll")]
    private static extern uint GetDpiForWindow(nint hwnd);

    [DllImport("user32.dll", CharSet = CharSet.Unicode)]
    private static extern int GetClassName(nint hWnd, char[] lpClassName, int nMaxCount);
}
