using System.ComponentModel;
using System.Diagnostics;
using System.IO;
using System.Runtime.InteropServices;
using System.Threading.Tasks;
using System.Windows;
using System.Windows.Automation;
using System.Windows.Interop;
using OpenTaskbar.Models;

namespace OpenTaskbar.Services;

/// <summary>
/// Opens Windows' own flyouts and settings pages, which stand in for the hidden taskbar's tray area.
/// </summary>
public static class ShellShortcuts
{
    public const ushort VkLeftWindows = 0x5B;
    public const ushort VkControl = 0x11;
    public const ushort VkLeft = 0x25;
    public const ushort VkRight = 0x27;
    public const ushort VkTab = 0x09;
    public const ushort VkD = 0x44;
    private const ushort VkA = 0x41;
    private const ushort VkN = 0x4E;
    private const ushort VkP = 0x50;
    private const ushort VkS = 0x53;
    private const uint MonitorDefaultToNearest = 2;
    private const uint SwpNoSize = 0x0001;
    private const uint SwpNoZOrder = 0x0004;
    private const uint SwpNoActivate = 0x0010;
    private const int ErrorCancelled = 1223;
    private static CancellationTokenSource? _repositionCts;
    private static CancellationTokenSource? _searchRepositionCts;
    private static CancellationTokenSource? _startMenuRepositionCts;
    private static readonly ShellFlyoutPositionRegistry FlyoutPositions = new(
        handle => { GetWindowThreadProcessId(handle, out var processId); return processId; },
        IsWindow,
        (handle, x, y) => SetWindowPos(handle, 0, x, y, 0, 0, SwpNoSize | SwpNoZOrder | SwpNoActivate),
        positions => ShellFlyoutPositionJournal.Write(positions),
        () => ShellFlyoutPositionJournal.Delete());
    public static Func<bool>? IsMediaWidgetActive { get; set; }

    /// <summary>
    /// Cancels every pending anchoring pass and puts Start, Search and Control Center back where Windows had them.
    /// Called on every taskbar shutdown path; restoring twice is harmless because the saved positions are cleared.
    /// </summary>
    public static void RestoreShellFlyoutPositions()
    {
        _repositionCts?.Cancel();
        _searchRepositionCts?.Cancel();
        _startMenuRepositionCts?.Cancel();
        FlyoutPositions.Restore();
    }

    /// <summary>
    /// Recovery path for a hard kill, when the in-process registry died with the app: use the journal the dock
    /// wrote before moving each flyout. Handles are PID-checked because Windows recycles HWNDs.
    /// </summary>
    public static void RestoreFlyoutPositionsFromJournal()
    {
        var ownedBySomeoneElse = false;
        foreach (var position in ShellFlyoutPositionJournal.Read())
        {
            if (!IsWindow(position.Handle)) continue;
            GetWindowThreadProcessId(position.Handle, out var processId);
            if (processId != position.ProcessId)
            {
                ownedBySomeoneElse = true;
                continue;
            }
            SetWindowPos(position.Handle, 0, position.Left, position.Top, 0, 0,
                SwpNoSize | SwpNoZOrder | SwpNoActivate);
        }
        if (!ownedBySomeoneElse) ShellFlyoutPositionJournal.Delete();
    }

    private static bool CanMoveShellWindow(int generation, CancellationToken cancellation) =>
        !cancellation.IsCancellationRequested && FlyoutPositions.Generation == generation;

    /// <summary>Win+A: Quick Settings (Windows 11) or Action Center (Windows 10), anchored above the clicked button when given.</summary>
    public static string? OpenQuickSettings(FrameworkElement? anchor = null)
    {
        var wasVisible = IsControlCenterVisible();
        var error = SendWindowsShortcut(VkA);
        if (error is null && anchor is not null && !wasVisible)
        {
            ScheduleControlCenterReposition(anchor);
        }
        return error;
    }

    /// <summary>Opens Windows' native available Wi-Fi networks flyout without throwing the user into full Settings.</summary>
    public static string? OpenAvailableNetworks(FrameworkElement? anchor = null)
    {
        var wasVisible = IsControlCenterVisible();
        var error = Start("ms-availablenetworks:", "ErrorSettingsPageOpen");
        if (error is null && anchor is not null && !wasVisible)
        {
            ScheduleControlCenterReposition(anchor);
        }
        return error;
    }

    /// <summary>Win+N: notification center and calendar, opened natively via Windows shortcut.</summary>
    public static string? OpenNotificationCenter(FrameworkElement? anchor = null)
    {
        AppLog.Info("OpenNotificationCenter: invoked.");
        EnsureShellExperienceHost();
        var error = SendWindowsShortcut(VkN);
        if (error is not null) AppLog.Warning($"OpenNotificationCenter failed: {error}");
        return error;
    }

    private static void EnsureShellExperienceHost()
    {
        try
        {
            if (Process.GetProcessesByName("ShellExperienceHost").Length == 0)
            {
                AppLog.Info("EnsureShellExperienceHost: launching ShellExperienceHost package...");
                using var _ = Process.Start(new ProcessStartInfo("explorer.exe", "shell:AppsFolder\\Microsoft.Windows.ShellExperienceHost_cw5n1h2txyewy!App") { UseShellExecute = true });
            }
        }
        catch (Exception ex)
        {
            AppLog.Warning($"EnsureShellExperienceHost exception: {ex.Message}");
        }
    }

    /// <summary>Win+P: display projection flyout, anchored above the clicked button when given.</summary>
    public static string? OpenProjection(FrameworkElement? anchor = null)
    {
        var wasVisible = IsControlCenterVisible();
        var error = SendWindowsShortcut(VkP);
        if (error is null && anchor is not null && !wasVisible)
        {
            ScheduleControlCenterReposition(anchor);
        }
        return error;
    }

    /// <summary>Win+S: Windows Search flyout, opened natively via Windows shortcut and anchored above the clicked button when given.</summary>
    public static string? OpenWindowsSearch(FrameworkElement? anchor = null)
    {
        AppLog.Info("OpenWindowsSearch: invoked.");
        EnsureNativeTaskbarHidden();
        var error = SendWindowsShortcut(VkS);
        EnsureNativeTaskbarHidden();
        if (error is null && anchor is not null)
        {
            ScheduleSearchReposition(anchor);
        }
        if (error is not null) AppLog.Warning($"OpenWindowsSearch failed: {error}");
        return error;
    }

    /// <summary>Checks whether Windows Search flyout is currently open and uncloaked.</summary>
    public static bool IsSearchFlyoutOpen() => FindSearchWindows().Count > 0;

    /// <summary>Closes Windows Search flyout if currently open.</summary>
    public static void CloseWindowsSearch()
    {
        if (IsSearchFlyoutOpen())
        {
            SendWindowsShortcut(VkS);
            EnsureNativeTaskbarHidden();
        }
    }

    /// <summary>Forwards a virtual key down/up to the open Windows Search flyout.</summary>
    public static void ForwardKeyToSearch(ushort vk)
    {
        var windows = FindSearchWindows();
        if (windows.Count == 0) return;
        foreach (var hwnd in windows)
        {
            PostMessage(hwnd, WmKeyDown, (nint)vk, 0);
            PostMessage(hwnd, WmKeyUp, (nint)vk, 0);
        }
    }

    /// <summary>Synchronizes the dock search text query into Windows Search edit control.</summary>
    public static void SyncSearchQuery(string query)
    {
        Task.Run(() =>
        {
            try
            {
                var windows = FindSearchWindows();
                if (windows.Count == 0) return;

                foreach (var hwnd in windows)
                {
                    try
                    {
                        var root = AutomationElement.FromHandle(hwnd);
                        if (root is null) continue;

                        var editCondition = new PropertyCondition(AutomationElement.ControlTypeProperty, ControlType.Edit);
                        var edit = root.FindFirst(TreeScope.Descendants, editCondition);
                        if (edit is not null)
                        {
                            if (edit.TryGetCurrentPattern(ValuePattern.Pattern, out var pattern) && pattern is ValuePattern vp)
                            {
                                vp.SetValue(query);
                                return;
                            }
                        }
                    }
                    catch (Exception ex)
                    {
                        AppLog.Warning($"SyncSearchQuery element error: {ex.Message}");
                    }
                }
            }
            catch (Exception ex)
            {
                AppLog.Warning($"SyncSearchQuery failed: {ex.Message}");
            }
        });
    }

    private static void EnsureNativeTaskbarHidden()
    {
        var taskbar = NativeTaskbarWindow.Find();
        if (taskbar != 0 && IsWindowVisible(taskbar))
        {
            ShowWindow(taskbar, SwHide);
            AppLog.Info("EnsureNativeTaskbarHidden: Shell_TrayWnd hidden.");
        }
        var secondary = FindWindow("Shell_SecondaryTrayWnd", null);
        if (secondary != 0 && IsWindowVisible(secondary))
        {
            ShowWindow(secondary, SwHide);
        }
    }

    private static List<nint> FindSearchWindows()
    {
        var list = new List<nint>();
        try
        {
            var pids = Process.GetProcessesByName("SearchHost")
                .Concat(Process.GetProcessesByName("SearchApp"))
                .Select(p => (uint)p.Id)
                .ToHashSet();

            if (pids.Count > 0)
            {
                EnumWindows((hwnd, _) =>
                {
                    if (!IsWindowVisible(hwnd)) return true;
                    if (IsWindowCloaked(hwnd)) return true;
                    GetWindowThreadProcessId(hwnd, out var pid);
                    if (pids.Contains(pid))
                    {
                        if (GetWindowRect(hwnd, out var rect))
                        {
                            var w = rect.Right - rect.Left;
                            var h = rect.Bottom - rect.Top;
                            if (w > 40 && h > 20)
                            {
                                list.Add(hwnd);
                            }
                        }
                    }
                    return true;
                }, 0);
            }
        }
        catch (Exception ex)
        {
            AppLog.Warning($"FindSearchWindows exception: {ex.Message}");
        }

        if (list.Count == 0)
        {
            foreach (var title in new[] { "Search", "Arama", "Ara" })
            {
                var h = FindWindow("Windows.UI.Core.CoreWindow", title);
                if (h != 0 && IsWindowVisible(h) && !IsWindowCloaked(h))
                {
                    list.Add(h);
                    break;
                }
            }
        }

        return list;
    }

    private static void ScheduleSearchReposition(FrameworkElement anchor)
    {
        AppLog.Info("ScheduleSearchReposition: scheduled.");
        var next = new CancellationTokenSource();
        var previous = Interlocked.Exchange(ref _searchRepositionCts, next);
        previous?.Cancel();
        previous?.Dispose();
        _ = RepositionSearchAsync(anchor, next.Token);
    }

    private static async Task RepositionSearchAsync(FrameworkElement anchor, CancellationToken cancellation)
    {
        if (PresentationSource.FromVisual(anchor) is not HwndSource source || source.Handle == 0)
        {
            AppLog.Warning("RepositionSearchAsync: PresentationSource is null.");
            return;
        }

        Point topLeft;
        Point bottomRight;
        try
        {
            topLeft = anchor.PointToScreen(new Point(0, 0));
            bottomRight = anchor.PointToScreen(new Point(anchor.ActualWidth, anchor.ActualHeight));
        }
        catch (InvalidOperationException ex)
        {
            AppLog.Warning($"RepositionSearchAsync PointToScreen failed: {ex.Message}");
            return;
        }

        var monitor = MonitorFromWindow(source.Handle, MonitorDefaultToNearest);
        var monitorInfo = new MonitorInfo { Size = Marshal.SizeOf<MonitorInfo>() };
        if (monitor == 0 || !GetMonitorInfo(monitor, ref monitorInfo))
        {
            AppLog.Warning("RepositionSearchAsync: GetMonitorInfo failed.");
            return;
        }

        var edge = DockFlyout.Edge;
        var generation = FlyoutPositions.Generation;
        AppLog.Info($"RepositionSearchAsync: anchor bounds [{topLeft.X:F0},{topLeft.Y:F0} - {bottomRight.X:F0},{bottomRight.Y:F0}], edge: {edge}");

        try
        {
            for (var attempt = 0; attempt < 35; attempt++)
            {
                await Task.Delay(20, cancellation);
                if (cancellation.IsCancellationRequested) return;

                EnsureNativeTaskbarHidden();

                var searchWindows = FindSearchWindows();
                if (searchWindows.Count == 0) continue;

                AppLog.Info($"RepositionSearchAsync: attempt {attempt} found {searchWindows.Count} window(s).");

                var mainHwnd = searchWindows.OrderByDescending(h =>
                {
                    if (GetWindowRect(h, out var r)) return (long)(r.Right - r.Left) * (r.Bottom - r.Top);
                    return 0L;
                }).First();

                if (!GetWindowRect(mainHwnd, out var rect)) continue;
                var width = rect.Right - rect.Left;
                var height = rect.Bottom - rect.Top;
                if (width < 100 || height < 100) continue;
                FlyoutPositions.Remember(mainHwnd, rect.Left, rect.Top);

                var centerX = (int)Math.Round((topLeft.X + bottomRight.X) / 2);
                var centerY = (int)Math.Round((topLeft.Y + bottomRight.Y) / 2);
                int targetX;
                int targetY;
                var minY = monitorInfo.Work.Top + 8;
                var maxY = Math.Max(minY, monitorInfo.Work.Bottom - height - 8);

                switch (edge)
                {
                    case DockEdge.Left:
                        targetX = (int)Math.Round(bottomRight.X) + 12;
                        targetY = Math.Clamp(centerY - height / 2, minY, maxY);
                        break;
                    case DockEdge.Right:
                        targetX = (int)Math.Round(topLeft.X) - width - 12;
                        targetY = Math.Clamp(centerY - height / 2, minY, maxY);
                        break;
                    default:
                        var minX = monitorInfo.Work.Left + 8;
                        var maxX = Math.Max(minX, monitorInfo.Work.Right - width - 8);
                        targetX = Math.Clamp(centerX - width / 2, minX, maxX);
                        targetY = Math.Clamp((int)Math.Round(topLeft.Y) - height - 12, minY, maxY);
                        break;
                }

                if (!CanMoveShellWindow(generation, cancellation)) return;
                AppLog.Info($"RepositionSearchAsync: moving main window {mainHwnd:X} ({width}x{height}) from ({rect.Left},{rect.Top}) to ({targetX},{targetY})");
                SetWindowPos(mainHwnd, 0, targetX, targetY, 0, 0, SwpNoSize | SwpNoZOrder | SwpNoActivate);

                foreach (var aux in searchWindows.Where(h => h != mainHwnd))
                {
                    if (GetWindowRect(aux, out var auxRect))
                    {
                        var auxW = auxRect.Right - auxRect.Left;
                        var auxH = auxRect.Bottom - auxRect.Top;
                        var auxX = Math.Clamp(centerX - auxW / 2, monitorInfo.Work.Left + 8, monitorInfo.Work.Right - auxW - 8);
                        var auxY = Math.Clamp((int)Math.Round(topLeft.Y) - auxH - 8, minY, maxY);
                        FlyoutPositions.Remember(aux, auxRect.Left, auxRect.Top);
                        if (!CanMoveShellWindow(generation, cancellation)) return;
                        AppLog.Info($"RepositionSearchAsync: moving aux window {aux:X} ({auxW}x{auxH}) to ({auxX},{auxY})");
                        SetWindowPos(aux, 0, auxX, auxY, 0, 0, SwpNoSize | SwpNoZOrder | SwpNoActivate);
                    }
                }

                for (var frame = 0; frame < 5; frame++)
                {
                    await Task.Delay(30, cancellation);
                    if (!CanMoveShellWindow(generation, cancellation)) return;
                    EnsureNativeTaskbarHidden();
                    SetWindowPos(mainHwnd, 0, targetX, targetY, 0, 0, SwpNoSize | SwpNoZOrder | SwpNoActivate);
                }
                break;
            }
        }
        catch (OperationCanceledException)
        {
        }
    }

    /// <summary>
    /// Anchors and repositions the native Windows Start menu directly above or beside the dock Start button.
    /// </summary>
    public static void ScheduleStartMenuReposition(FrameworkElement anchor, nint hintHwnd = 0)
    {
        AppLog.Info("ScheduleStartMenuReposition: scheduled.");
        var next = new CancellationTokenSource();
        var previous = Interlocked.Exchange(ref _startMenuRepositionCts, next);
        previous?.Cancel();
        previous?.Dispose();
        _ = RepositionStartMenuAsync(anchor, hintHwnd, next.Token);
    }

    public static List<nint> FindStartMenuWindows()
    {
        var list = new List<nint>();
        try
        {
            var pids = Process.GetProcessesByName("StartMenuExperienceHost")
                .Select(p => (uint)p.Id)
                .ToHashSet();

            if (pids.Count > 0)
            {
                EnumWindows((hwnd, _) =>
                {
                    if (!IsWindowVisible(hwnd) || IsWindowCloaked(hwnd)) return true;
                    GetWindowThreadProcessId(hwnd, out var pid);
                    if (pids.Contains(pid))
                    {
                        if (GetWindowRect(hwnd, out var rect))
                        {
                            var w = rect.Right - rect.Left;
                            var h = rect.Bottom - rect.Top;
                            if (w > 100 && h > 100)
                            {
                                list.Add(hwnd);
                            }
                        }
                    }
                    return true;
                }, 0);
            }
        }
        catch (Exception ex)
        {
            AppLog.Warning($"FindStartMenuWindows exception: {ex.Message}");
        }

        if (list.Count == 0)
        {
            foreach (var title in new[] { "Start", "Başlat" })
            {
                var h = FindWindow("Windows.UI.Core.CoreWindow", title);
                if (h != 0 && IsWindowVisible(h) && !IsWindowCloaked(h))
                {
                    list.Add(h);
                    break;
                }
            }
        }

        return list;
    }

    /// <summary>
    /// Computes the host window coordinates required to place the Windows Start menu flyout directly above
    /// or beside the Start button. Handles both discrete popup windows and Windows 11 full-screen CoreWindow canvases.
    /// </summary>
    public static (int TargetX, int TargetY) ComputeStartMenuPosition(
        int windowWidth, int windowHeight,
        int workLeft, int workTop, int workRight, int workBottom,
        double anchorLeft, double anchorTop, double anchorRight, double anchorBottom,
        DockEdge edge, double dpiScale = 1.0, bool isLeftAligned = false)
    {
        var workWidth = workRight - workLeft;
        var isFullScreenContainer = windowWidth >= workWidth - 100;

        if (!isFullScreenContainer)
        {
            var centerY = (int)Math.Round((anchorTop + anchorBottom) / 2);
            var minY = workTop + 8;
            var maxY = Math.Max(minY, workBottom - windowHeight - 8);

            return edge switch
            {
                DockEdge.Left => (
                    (int)Math.Round(anchorRight) + 12,
                    Math.Clamp(centerY - windowHeight / 2, minY, maxY)
                ),
                DockEdge.Right => (
                    (int)Math.Round(anchorLeft) - windowWidth - 12,
                    Math.Clamp(centerY - windowHeight / 2, minY, maxY)
                ),
                _ => (
                    Math.Clamp((int)Math.Round(anchorLeft - 8), workLeft + 8, Math.Max(workLeft + 8, workRight - windowWidth - 8)),
                    Math.Clamp((int)Math.Round(anchorTop) - windowHeight - 12, minY, maxY)
                )
            };
        }

        var flyoutWidth = (int)Math.Round(642 * dpiScale);
        var flyoutHeight = (int)Math.Round(724 * dpiScale);

        int flyoutLocalX = isLeftAligned ? (int)Math.Round(12 * dpiScale) : (windowWidth - flyoutWidth) / 2;
        int flyoutLocalY = windowHeight - flyoutHeight - (int)Math.Round(12 * dpiScale);

        var workMinY = workTop + 8;
        var workMaxY = Math.Max(workMinY, workBottom - flyoutHeight - 8);

        int desiredFlyoutX;
        int desiredFlyoutY;

        switch (edge)
        {
            case DockEdge.Left:
                desiredFlyoutX = (int)Math.Round(anchorRight) + 12;
                var centerY = (int)Math.Round((anchorTop + anchorBottom) / 2);
                desiredFlyoutY = Math.Clamp(centerY - flyoutHeight / 2, workMinY, workMaxY);
                break;
            case DockEdge.Right:
                desiredFlyoutX = (int)Math.Round(anchorLeft) - flyoutWidth - 12;
                var rCenterY = (int)Math.Round((anchorTop + anchorBottom) / 2);
                desiredFlyoutY = Math.Clamp(rCenterY - flyoutHeight / 2, workMinY, workMaxY);
                break;
            default:
                var minX = workLeft + 8;
                var maxX = Math.Max(minX, workRight - flyoutWidth - 8);
                // The dock's Start button can be anywhere (StartFirst/StartLast, full-width docks, drag reorder),
                // so always derive the flyout position from its real screen rect rather than a registry assumption.
                desiredFlyoutX = Math.Clamp((int)Math.Round(anchorLeft - 8), minX, maxX);
                desiredFlyoutY = Math.Clamp((int)Math.Round(anchorTop) - flyoutHeight - 12, workMinY, workMaxY);
                break;
        }

        return (desiredFlyoutX - flyoutLocalX, desiredFlyoutY - flyoutLocalY);
    }

    private static async Task RepositionStartMenuAsync(FrameworkElement anchor, nint hintHwnd, CancellationToken cancellation)
    {
        if (PresentationSource.FromVisual(anchor) is not HwndSource source || source.Handle == 0)
        {
            AppLog.Warning("RepositionStartMenuAsync: PresentationSource is null.");
            return;
        }

        Point topLeft;
        Point bottomRight;
        try
        {
            topLeft = anchor.PointToScreen(new Point(0, 0));
            bottomRight = anchor.PointToScreen(new Point(anchor.ActualWidth, anchor.ActualHeight));
        }
        catch (InvalidOperationException ex)
        {
            AppLog.Warning($"RepositionStartMenuAsync PointToScreen failed: {ex.Message}");
            return;
        }

        var monitor = MonitorFromWindow(source.Handle, MonitorDefaultToNearest);
        var monitorInfo = new MonitorInfo { Size = Marshal.SizeOf<MonitorInfo>() };
        if (monitor == 0 || !GetMonitorInfo(monitor, ref monitorInfo))
        {
            AppLog.Warning("RepositionStartMenuAsync: GetMonitorInfo failed.");
            return;
        }

        var edge = DockFlyout.Edge;
        var generation = FlyoutPositions.Generation;
        AppLog.Info($"RepositionStartMenuAsync: anchor bounds [{topLeft.X:F0},{topLeft.Y:F0} - {bottomRight.X:F0},{bottomRight.Y:F0}], edge: {edge}");

        try
        {
            for (var attempt = 0; attempt < 40; attempt++)
            {
                await Task.Delay(20, cancellation);
                if (cancellation.IsCancellationRequested) return;

                EnsureNativeTaskbarHidden();

                var startWindows = hintHwnd != 0 && IsWindowVisible(hintHwnd) && !IsWindowCloaked(hintHwnd)
                    ? new List<nint> { hintHwnd }
                    : FindStartMenuWindows();

                if (startWindows.Count == 0) continue;

                var mainHwnd = startWindows.OrderByDescending(h =>
                {
                    if (GetWindowRect(h, out var r)) return (long)(r.Right - r.Left) * (r.Bottom - r.Top);
                    return 0L;
                }).First();

                if (!GetWindowRect(mainHwnd, out var rect)) continue;
                var width = rect.Right - rect.Left;
                var height = rect.Bottom - rect.Top;
                if (width < 100 || height < 100) continue;
                // The Start canvas is monitor-sized and natively sits at the work-area origin. A previous
                // OpenTaskbar session may have left it displaced; remembering that rect would make every later
                // restore put it back in the wrong place.
                var canvasWorkWidth = monitorInfo.Work.Right - monitorInfo.Work.Left;
                var isFullScreenCanvas = width >= canvasWorkWidth - 100;
                FlyoutPositions.Remember(mainHwnd,
                    isFullScreenCanvas ? monitorInfo.Work.Left : rect.Left,
                    isFullScreenCanvas ? monitorInfo.Work.Top : rect.Top);

                var dpi = GetDpiForWindow(mainHwnd);
                if (dpi == 0) dpi = 96;
                var dpiScale = dpi / 96.0;

                var isLeftAligned = false;
                try
                {
                    using var key = Microsoft.Win32.Registry.CurrentUser.OpenSubKey(@"Software\Microsoft\Windows\CurrentVersion\Explorer\Advanced");
                    if (key?.GetValue("TaskbarAl") is int al && al == 0)
                    {
                        isLeftAligned = true;
                    }
                }
                catch
                {
                }

                var (targetX, targetY) = ComputeStartMenuPosition(
                    width, height,
                    monitorInfo.Work.Left, monitorInfo.Work.Top, monitorInfo.Work.Right, monitorInfo.Work.Bottom,
                    topLeft.X, topLeft.Y, bottomRight.X, bottomRight.Y,
                    edge, dpiScale, isLeftAligned);

                if (!CanMoveShellWindow(generation, cancellation)) return;
                AppLog.Info($"RepositionStartMenuAsync: moving start menu window {mainHwnd:X} ({width}x{height}) to ({targetX},{targetY})");
                SetWindowPos(mainHwnd, 0, targetX, targetY, 0, 0, SwpNoSize | SwpNoZOrder | SwpNoActivate);

                // Settle across extra frames to smoothly complete shell entrance transitions
                for (var frame = 0; frame < 6; frame++)
                {
                    await Task.Delay(25, cancellation);
                    if (!CanMoveShellWindow(generation, cancellation)) return;
                    EnsureNativeTaskbarHidden();
                    SetWindowPos(mainHwnd, 0, targetX, targetY, 0, 0, SwpNoSize | SwpNoZOrder | SwpNoActivate);
                }
                break;
            }
        }
        catch (OperationCanceledException)
        {
        }
    }

    private static bool IsControlCenterVisible()
    {
        var hwnd = FindWindow("ControlCenterWindow", null);
        return hwnd != 0 && IsWindowVisible(hwnd);
    }

    private static void ScheduleControlCenterReposition(FrameworkElement anchor)
    {
        var next = new CancellationTokenSource();
        var previous = Interlocked.Exchange(ref _repositionCts, next);
        previous?.Cancel();
        previous?.Dispose();
        _ = RepositionControlCenterAsync(anchor, next.Token);
    }

    private static async Task RepositionControlCenterAsync(FrameworkElement anchor, CancellationToken cancellation)
    {
        if (PresentationSource.FromVisual(anchor) is not HwndSource source || source.Handle == 0) return;
        Point topLeft;
        Point bottomRight;
        try
        {
            topLeft = anchor.PointToScreen(new Point(0, 0));
            bottomRight = anchor.PointToScreen(new Point(anchor.ActualWidth, anchor.ActualHeight));
        }
        catch (InvalidOperationException)
        {
            return;
        }

        var monitor = MonitorFromWindow(source.Handle, MonitorDefaultToNearest);
        var monitorInfo = new MonitorInfo { Size = Marshal.SizeOf<MonitorInfo>() };
        if (monitor == 0 || !GetMonitorInfo(monitor, ref monitorInfo)) return;
        var edge = DockFlyout.Edge;
        var generation = FlyoutPositions.Generation;
        var movedOnce = false;

        try
        {
            for (var attempt = 0; attempt < 24; attempt++)
            {
                await Task.Delay(25, cancellation);
                if (cancellation.IsCancellationRequested) return;
                var hwnd = FindWindow("ControlCenterWindow", null);
                if (hwnd == 0 || !IsWindowVisible(hwnd) || !GetWindowRect(hwnd, out var rect)) continue;
                var width = rect.Right - rect.Left;
                var height = rect.Bottom - rect.Top;
                if (width < 80 || height < 80) continue;
                FlyoutPositions.Remember(hwnd, rect.Left, rect.Top);

                var centerX = (int)Math.Round((topLeft.X + bottomRight.X) / 2);
                var centerY = (int)Math.Round((topLeft.Y + bottomRight.Y) / 2);
                int targetX;
                int targetY;
                var minY = monitorInfo.Work.Top + 8;
                var maxY = Math.Max(minY, monitorInfo.Work.Bottom - height - 8);
                switch (edge)
                {
                    case DockEdge.Left:
                        targetX = (int)Math.Round(bottomRight.X) + 12;
                        targetY = Math.Clamp(centerY - height / 2, minY, maxY);
                        break;
                    case DockEdge.Right:
                        targetX = (int)Math.Round(topLeft.X) - width - 12;
                        targetY = Math.Clamp(centerY - height / 2, minY, maxY);
                        break;
                    default:
                        var minX = monitorInfo.Work.Left + 8;
                        var maxX = Math.Max(minX, monitorInfo.Work.Right - width - 8);
                        targetX = Math.Clamp(centerX - width / 2, minX, maxX);
                        targetY = Math.Clamp((int)Math.Round(topLeft.Y) - height - 12, minY, maxY);
                        break;
                }

                if (!CanMoveShellWindow(generation, cancellation)) return;
                SetWindowPos(hwnd, 0, targetX, targetY, 0, 0, SwpNoSize | SwpNoZOrder | SwpNoActivate);
                if (movedOnce) break;
                movedOnce = true;
                // Re-apply once more after ShellHost finishes its entrance animation.
                await Task.Delay(75, cancellation);
                if (!CanMoveShellWindow(generation, cancellation)) return;
                SetWindowPos(hwnd, 0, targetX, targetY, 0, 0, SwpNoSize | SwpNoZOrder | SwpNoActivate);
                break;
            }
        }
        catch (OperationCanceledException)
        {
        }
    }

    /// <summary>Win+D: toggles the desktop. Used by the slim trailing Show Desktop strip.</summary>
    public static string? ToggleShowDesktop() => SendWindowsShortcut(VkD);

    public static string? OpenUri(string uri) => Start(uri, "ErrorSettingsPageOpen");

    /// <summary>
    /// Task Manager by its full path, so a program named taskmgr elsewhere on the search path is never started.
    /// The Shell handles its elevation request.
    /// </summary>
    public static string? OpenTaskManager() => Start(
        Path.Combine(Environment.GetFolderPath(Environment.SpecialFolder.System), "Taskmgr.exe"), "ErrorTaskManagerOpen");

    private static string? Start(string target, string failureKey)
    {
        try
        {
            using var _ = Process.Start(new ProcessStartInfo(target) { UseShellExecute = true });
            return null;
        }
        catch (Win32Exception ex) when (ex.NativeErrorCode == ErrorCancelled)
        {
            // The user declined the elevation prompt; that is a choice, not a failure.
            return null;
        }
        catch (Exception ex) when (ex is Win32Exception or InvalidOperationException)
        {
            AppLog.Warning($"Cannot open {target}: {ex.Message}");
            return UiText.Get(failureKey);
        }
    }

    private static string? SendWindowsShortcut(ushort key) => SendChord(VkLeftWindows, key);

    /// <summary>Presses the keys in order and releases them in reverse, with a guaranteed modifier cleanup.</summary>
    public static string? SendChord(params ushort[] keys) => SyntheticKeyboard.SendChord(keys);

    /// <summary>Drops modifiers left down by a shell shortcut interrupted by a lock or focus transition.</summary>
    public static void ReleaseStuckModifiers() => SyntheticKeyboard.ReleaseAll();

    [StructLayout(LayoutKind.Sequential)]
    private struct NativeRect
    {
        public int Left, Top, Right, Bottom;
    }

    [StructLayout(LayoutKind.Sequential)]
    private struct MonitorInfo
    {
        public int Size;
        public NativeRect Monitor;
        public NativeRect Work;
        public uint Flags;
    }

    private const int SwHide = 0;
    private const int DwmwaCloaked = 14;
    private const uint WmKeyDown = 0x0100;
    private const uint WmKeyUp = 0x0101;

    [DllImport("user32.dll", SetLastError = true)]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool PostMessage(nint hWnd, uint msg, nint wParam, nint lParam);

    [DllImport("user32.dll")]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool ShowWindow(nint hWnd, int nCmdShow);

    [DllImport("dwmapi.dll")]
    private static extern int DwmGetWindowAttribute(nint hwnd, int dwAttribute, out int pvAttribute, int cbAttribute);

    private static bool IsWindowCloaked(nint hwnd) =>
        DwmGetWindowAttribute(hwnd, DwmwaCloaked, out var cloaked, sizeof(int)) == 0 && cloaked != 0;

    private delegate bool EnumWindowsProc(nint hWnd, nint lParam);

    [DllImport("user32.dll")]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool EnumWindows(EnumWindowsProc enumProc, nint lParam);

    [DllImport("user32.dll")]
    private static extern uint GetWindowThreadProcessId(nint hWnd, out uint lpdwProcessId);
    [DllImport("user32.dll", CharSet = CharSet.Unicode)]
    private static extern nint FindWindow(string? lpClassName, string? lpWindowName);

    [DllImport("user32.dll")]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool IsWindowVisible(nint hWnd);

    [DllImport("user32.dll")]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool IsWindow(nint hWnd);

    [DllImport("user32.dll")]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool GetWindowRect(nint hWnd, out NativeRect lpRect);

    [DllImport("user32.dll")]
    private static extern nint MonitorFromWindow(nint hwnd, uint dwFlags);

    [DllImport("user32.dll", EntryPoint = "GetMonitorInfoW")]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool GetMonitorInfo(nint hMonitor, ref MonitorInfo lpmi);

    [DllImport("user32.dll", SetLastError = true)]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool SetWindowPos(nint hWnd, nint hWndInsertAfter, int x, int y, int cx, int cy, uint uFlags);

    [DllImport("user32.dll")]
    private static extern uint GetDpiForWindow(nint hwnd);
}
