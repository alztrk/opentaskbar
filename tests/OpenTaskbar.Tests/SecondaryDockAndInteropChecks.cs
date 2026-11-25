using OpenTaskbar;
using OpenTaskbar.Models;
using OpenTaskbar.Services;

internal static class SecondaryDockAndInteropChecks
{
    private const string PinnedPath = @"C:\Apps\Pinned.exe";
    private const string UnpinnedPath = @"C:\Apps\Unpinned.exe";
    private const string MissingPath = @"C:\Apps\Missing.exe";

    private sealed class FakeProcess(uint id, Exception? idException = null) : IDisposable
    {
        public uint Id => idException is null ? id : throw idException;
        public int DisposeCount { get; private set; }
        public void Dispose() => DisposeCount++;
    }

    public static int Run()
    {
        var checks = 0;
        void Check(bool condition, string message)
        {
            if (!condition) throw new InvalidOperationException(message);
            checks++;
        }

        // Bug #7: Secondary docks must include running pinned apps while still filtering missing paths
        // and keeping primary running-app projection unchanged.
        IReadOnlyList<TrackedWindow> windows =
        [
            new(10, "Pinned App", PinnedPath, HasNotification: true),
            new(20, "Unpinned App", UnpinnedPath),
            new(30, "Missing App", MissingPath),
            new(40, "No Path", null)
        ];
        var existingPaths = new HashSet<string>([PinnedPath, UnpinnedPath], StringComparer.OrdinalIgnoreCase);
        bool Exists(string path) => existingPaths.Contains(path);

        var secondary = MainWindow.ProjectSecondaryDockCandidates(windows, foreground: 10, notificationBadgesEnabled: true, Exists);
        Check(secondary.Count == 2,
            "Secondary dock projection must include both pinned and unpinned running apps with valid paths.");
        Check(string.Equals(secondary[0].ExecutablePath, PinnedPath, StringComparison.OrdinalIgnoreCase)
            && secondary[0].IsForeground && secondary[0].HasNotification,
            "A foreground running pinned app must appear on secondary docks with foreground and notification state intact.");
        Check(string.Equals(secondary[1].ExecutablePath, UnpinnedPath, StringComparison.OrdinalIgnoreCase)
            && !secondary[1].IsForeground,
            "Unpinned running apps must remain included after pinned apps in Z-order.");

        // PR #163: Shared widget provider requests across mirrored docks.
        var reads = new OpenTaskbar.Widgets.SharedWidgetRequest<string, int>(TimeSpan.FromMinutes(1));
        var pendingRead = new TaskCompletionSource<int>();
        using (var closeDisplay = new CancellationTokenSource())
        {
            var firstRead = reads.ReadAsync("provider", () => pendingRead.Task, closeDisplay.Token);
            var secondRead = reads.ReadAsync("provider",
                () => throw new InvalidOperationException("Expected a shared provider request."), CancellationToken.None);
            closeDisplay.Cancel();
            pendingRead.SetResult(42);
            Check(firstRead.IsCanceled && secondRead.GetAwaiter().GetResult() == 42
                && reads.ReadAsync("provider", () => Task.FromResult(0), CancellationToken.None).Result == 42,
                "Closing one view must cancel only its wait while other views reuse the provider result.");
        }

        // #158: Dead/zero tray icon handles must be excluded as non-live windows.
        Check(!TrayHost.IsLiveWindow(0) && !TrayHost.IsLiveWindow(unchecked((nint)0x00DEAD01)),
            "Zero or invalid HWNDs must not be treated as live tray windows.");

        // #164: Standalone Win-key tap triggers dock reveal, while Win+key combinations do not.
        var winTracker = new WinKeySequenceTracker();
        winTracker.OnKeyDown(WinKeySequenceTracker.VkLeftWin);
        Check(winTracker.OnKeyUp(WinKeySequenceTracker.VkLeftWin),
            "Standalone Left Win press + release must trigger dock reveal.");
        winTracker.OnKeyDown(WinKeySequenceTracker.VkRightWin);
        winTracker.OnKeyDown(WinKeySequenceTracker.VkRightWin); // key repeat
        Check(winTracker.OnKeyUp(WinKeySequenceTracker.VkRightWin),
            "Held Right Win key repeat followed by release must still trigger dock reveal.");
        winTracker.OnKeyDown(WinKeySequenceTracker.VkLeftWin);
        winTracker.OnKeyDown(0x44); // 'D' (Win+D)
        Check(!winTracker.OnKeyUp(WinKeySequenceTracker.VkLeftWin),
            "Win+D chord must not trigger standalone Win dock reveal.");
        winTracker.OnKeyDown(WinKeySequenceTracker.VkLeftWin);
        winTracker.OnKeyDown(0x10); // Shift (Win+Shift+S)
        winTracker.OnKeyDown(0x53); // 'S'
        Check(!winTracker.OnKeyUp(WinKeySequenceTracker.VkLeftWin),
            "Win+Shift+S chord must not trigger standalone Win dock reveal.");
        winTracker.OnKeyDown(WinKeySequenceTracker.VkLeftWin);
        winTracker.Reset();
        Check(!winTracker.OnKeyUp(WinKeySequenceTracker.VkLeftWin),
            "Resetting tracker state (e.g. on lock/unlock) must cancel pending Win reveal.");

        // #156: Keyboard layout handle formatting produces stable uppercase ISO codes and display names.
        var enLayout = KeyboardLayoutService.DescribeHandle(0x0409);
        var trLayout = KeyboardLayoutService.DescribeHandle(0x041F);
        var ukLayout = KeyboardLayoutService.DescribeHandle(0x0422);
        Check(enLayout.ShortCode == "EN" && trLayout.ShortCode == "TR" && ukLayout.ShortCode == "UK"
            && !string.IsNullOrWhiteSpace(enLayout.DisplayName)
            && !string.IsNullOrWhiteSpace(trLayout.DisplayName)
            && !string.IsNullOrWhiteSpace(ukLayout.DisplayName),
            "KeyboardLayoutService must format en-US, tr-TR, and uk-UA handles cleanly.");

        var primaryExcluded = new HashSet<string>([PinnedPath], StringComparer.OrdinalIgnoreCase);
        var primary = RunningAppProjection.Project(windows, primaryExcluded, foreground: 10, notificationBadgesEnabled: true, Exists);
        Check(primary.Count == 1 && string.Equals(primary[0].ExecutablePath, UnpinnedPath, StringComparison.OrdinalIgnoreCase),
            "Primary dock running-app projection must still exclude pinned apps.");

        // Bug #8: Secondary dock AppBar reservation bounds must exclude our own reservation so resizing
        // replaces the existing band instead of stacking a second shelf above it.
        Check(SecondaryDockWindow.ExternalStripPixels(56, 56) == 0,
            "A strip equal to our own reservation must report zero external strip pixels.");
        Check(SecondaryDockWindow.ExternalStripPixels(76, 36) == 40,
            "Only the portion outside our own reservation counts as an external taskbar strip.");
        Check(SecondaryDockWindow.ExternalStripPixels(40, 64) == 0,
            "External strip height must clamp to zero if our stored reservation exceeds the current strip.");

        var initialBounds = SecondaryDockWindow.ComputeReservationBounds(monitorBottom: 1080, totalStripHeight: 0, ownReservedHeight: 0, additionalHeight: 56);
        Check(initialBounds == (1024, 1080),
            $"Initial reservation without Explorer strip must occupy 1024..1080 (got {initialBounds}).");

        var resizedBounds = SecondaryDockWindow.ComputeReservationBounds(monitorBottom: 1080, totalStripHeight: 56, ownReservedHeight: 56, additionalHeight: 64);
        Check(resizedBounds == (1016, 1080),
            $"Resizing our own shelf from 56px to 64px must occupy 1016..1080 instead of stacking above 1024 (got {resizedBounds}).");

        var mixedBounds = SecondaryDockWindow.ComputeReservationBounds(monitorBottom: 1080, totalStripHeight: 56, ownReservedHeight: 16, additionalHeight: 24);
        Check(mixedBounds == (1016, 1040),
            $"Resizing a partial 16px shelf to 24px above a 40px Explorer strip must occupy 1016..1040 (got {mixedBounds}).");

        var baseProfile = DockMetrics.For(DockSize.Medium);
        var effectiveShelf = MainWindow.ComputeEffectiveShelfHeight(baseProfile, isVertical: false, borderThickness: 2, externalBottomGap: 8, externalTopGap: 6);
        Check(effectiveShelf > baseProfile.Shelf,
            "EffectiveShelfHeight must include configured border and external gaps beyond Profile.Shelf.");

        foreach (var dpi in new uint[] { 96, 120, 144, 192 })
        {
            var firstPixels = SecondaryDockWindow.MissingShelfPixels(0, 0, dpi, baseProfile.Shelf);
            var resizedPixels = SecondaryDockWindow.MissingShelfPixels(firstPixels, firstPixels, dpi, effectiveShelf);
            var (top, bottom) = SecondaryDockWindow.ComputeReservationBounds(1440, firstPixels, firstPixels, resizedPixels);
            Check(bottom == 1440 && bottom - top == resizedPixels,
                $"Secondary shelf resize at {dpi}dpi must keep Bottom anchored at 1440 with exact height {resizedPixels} (got {top}..{bottom}).");
        }

        // Bug #10: Exiting fullscreen must invalidate cached placement and reposition back to HWND_TOPMOST.
        Check(TaskbarReplacementSession.WindowZOrder(true) == new nint(1),
            "Fullscreen suppression must demote the dock Z-order to HWND_BOTTOM.");
        Check(TaskbarReplacementSession.WindowZOrder(false) == new nint(-1),
            "Normal dock state must use HWND_TOPMOST.");

        var suppressed = false;
        (int Left, int Top, int Width, int Height, bool FullScreenSuppressed)? lastPos = (0, 1020, 1920, 60, false);
        var events = new List<string>();
        var enterChanged = TaskbarReplacementSession.ApplyFullScreenTransition(
            ref suppressed,
            ref lastPos,
            suppressed: true,
            cmd => events.Add($"show:{cmd}"),
            () => events.Add("reposition"),
            state => events.Add($"notify:{state}"));
        Check(enterChanged && suppressed && lastPos is null
            && events.SequenceEqual(["show:0", "notify:True"]),
            "Entering fullscreen must hide the dock, clear cached position, and notify without repositioning.");

        lastPos = (0, 1020, 1920, 60, true);
        events.Clear();
        var noopChanged = TaskbarReplacementSession.ApplyFullScreenTransition(
            ref suppressed,
            ref lastPos,
            suppressed: true,
            cmd => events.Add($"show:{cmd}"),
            () => events.Add("reposition"),
            state => events.Add($"notify:{state}"));
        Check(!noopChanged && suppressed && lastPos == (0, 1020, 1920, 60, true) && events.Count == 0,
            "Redundant fullscreen transitions must be no-ops.");

        var exitChanged = TaskbarReplacementSession.ApplyFullScreenTransition(
            ref suppressed,
            ref lastPos,
            suppressed: false,
            cmd => events.Add($"show:{cmd}"),
            () => events.Add("reposition"),
            state => events.Add($"notify:{state}"));
        Check(exitChanged && !suppressed && lastPos is null
            && events.SequenceEqual(["show:4", "reposition", "notify:False"]),
            "Exiting fullscreen must invalidate cached HWND_BOTTOM position, show without activation, reposition to HWND_TOPMOST, and notify.");

        // A full-screen video on a secondary display must keep that display's dock hidden after focus moves to
        // another monitor; previously only the foreground window was checked, so the dock covered the video.
        const nint videoMonitor = 0x100;
        const nint otherMonitor = 0x200;
        const nint videoWindow = 0x300;
        const nint focusedWindowElsewhere = 0x400;
        bool ForegroundFullScreen(nint window, nint monitor) => window == videoWindow && monitor == videoMonitor;
        bool AnyFullScreen(nint monitor) => monitor == videoMonitor;
        bool NoFullScreen(nint _) => false;
        Check(SecondaryDockWindow.ShouldHideForFullScreen(videoMonitor, focusedWindowElsewhere, ForegroundFullScreen, AnyFullScreen),
            "A secondary dock must stay hidden behind a full-screen window on its display when another display has focus.");
        Check(SecondaryDockWindow.ShouldHideForFullScreen(videoMonitor, videoWindow, ForegroundFullScreen, NoFullScreen),
            "A full-screen foreground window on the dock's display must hide the secondary dock even if the monitor scan skips it.");
        Check(!SecondaryDockWindow.ShouldHideForFullScreen(otherMonitor, videoWindow, ForegroundFullScreen, AnyFullScreen),
            "A full-screen window on another display must not hide this secondary dock.");
        Check(!SecondaryDockWindow.ShouldHideForFullScreen(0, videoWindow, ForegroundFullScreen, AnyFullScreen),
            "A secondary dock without a monitor handle must never report full-screen suppression.");

        // Bug #11: Own-process focus (menus / flyouts / keyboard nav) must preserve the external foreground handle,
        // while a genuine empty foreground (0) or new external window must still update it.
        const nint externalApp = 0x1234;
        const nint otherApp = 0x5678;
        const nint ownMenu = 0x9999;
        Check(MainWindow.ResolveEffectiveForeground(externalApp, isOwnProcessForeground: true, candidateForeground: 0) == externalApp,
            "Foreground poll observing own-process focus must preserve the last external foreground handle.");
        Check(MainWindow.ResolveEffectiveForeground(externalApp, isOwnProcessForeground: true, candidateForeground: ownMenu) == externalApp,
            "Foreground event carrying an own-process HWND must preserve the last external foreground handle.");
        Check(MainWindow.ResolveEffectiveForeground(externalApp, isOwnProcessForeground: false, candidateForeground: 0) == 0,
            "A genuine empty/non-own foreground must clear the active foreground handle to zero.");
        Check(MainWindow.ResolveEffectiveForeground(externalApp, isOwnProcessForeground: false, candidateForeground: otherApp) == otherApp,
            "Switching to another external application must update the foreground handle.");

        // Bug #12: ShellShortcuts.CollectProcessIds must dispose every Process instance across normal,
        // exited-process, and throwing paths.
        var search1 = new FakeProcess(101);
        var search2 = new FakeProcess(102);
        var searchDup = new FakeProcess(101);
        var pids = ShellShortcuts.CollectProcessIds<FakeProcess>(
            ["SearchHost", "SearchApp"],
            name => name == "SearchHost" ? [search1, search2] : [searchDup],
            static p => p.Id);
        Check(pids.SetEquals([101u, 102u])
            && search1.DisposeCount == 1 && search2.DisposeCount == 1 && searchDup.DisposeCount == 1,
            "CollectProcessIds must collect deduplicated PIDs and dispose every process handle.");

        var alive = new FakeProcess(201);
        var exited = new FakeProcess(202, new InvalidOperationException("Process has exited."));
        var partialPids = ShellShortcuts.CollectProcessIds<FakeProcess>(
            ["StartMenuExperienceHost"],
            _ => [exited, alive],
            static p => p.Id);
        Check(partialPids.SetEquals([201u]) && alive.DisposeCount == 1 && exited.DisposeCount == 1,
            "An exited process throwing InvalidOperationException must be skipped while every handle is still disposed.");

        var fatalFirst = new FakeProcess(301);
        var fatalThrower = new FakeProcess(302, new UnauthorizedAccessException("Access denied."));
        var fatalAfter = new FakeProcess(303);
        var fatalCaught = false;
        try
        {
            ShellShortcuts.CollectProcessIds<FakeProcess>(
                ["SearchHost"],
                _ => [fatalFirst, fatalThrower, fatalAfter],
                static p => p.Id);
        }
        catch (UnauthorizedAccessException)
        {
            fatalCaught = true;
        }
        Check(fatalCaught && fatalFirst.DisposeCount == 1 && fatalThrower.DisposeCount == 1 && fatalAfter.DisposeCount == 1,
            "An unexpected exception reading a process Id must still dispose every enumerated process handle.");

        var batch1A = new FakeProcess(401);
        var batch1B = new FakeProcess(402);
        var secondQueryFailed = false;
        try
        {
            ShellShortcuts.CollectProcessIds<FakeProcess>(
                ["SearchHost", "SearchApp"],
                name => name == "SearchHost" ? [batch1A, batch1B] : throw new InvalidOperationException("Query failed."),
                static p => p.Id);
        }
        catch (InvalidOperationException)
        {
            secondQueryFailed = true;
        }
        Check(secondQueryFailed && batch1A.DisposeCount == 1 && batch1B.DisposeCount == 1,
            "A failure querying a subsequent process name must leave earlier batches already disposed.");

        return checks;
    }
}
