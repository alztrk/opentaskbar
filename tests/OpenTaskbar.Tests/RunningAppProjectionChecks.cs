using System.Windows.Threading;
using OpenTaskbar.Models;
using OpenTaskbar.Services;

internal static class RunningAppProjectionChecks
{
    private const string First = @"C:\Apps\First.exe";
    private const string Second = @"C:\Apps\Second.exe";
    private const string Pinned = @"C:\Apps\Pinned.exe";
    private const string Missing = @"C:\Apps\Missing.exe";

    public static int Run()
    {
        var checks = 0;
        void Check(bool condition, string message)
        {
            if (!condition) throw new InvalidOperationException(message);
            checks++;
        }

        IReadOnlyList<TrackedWindow> windows =
        [
            new(1, "First", First),
            new(2, "Second window", First.ToLowerInvariant(), HasNotification: true),
            new(3, "Second", Second),
            new(4, "Pinned", Pinned),
            new(5, "Missing", Missing),
            new(6, "No path", null)
        ];
        var onDisk = new HashSet<string>([First, Second, Pinned], StringComparer.OrdinalIgnoreCase);
        var excluded = new HashSet<string>([Pinned], StringComparer.OrdinalIgnoreCase);
        bool Exists(string path) => onDisk.Contains(path);

        var candidates = RunningAppProjection.Project(windows, excluded, foreground: 2,
            notificationBadgesEnabled: true, Exists);
        Check(candidates.Count == 2, "Only real, unpinned apps with a window become running app candidates.");
        Check(string.Equals(candidates[0].ExecutablePath, First, StringComparison.OrdinalIgnoreCase),
            "Candidates keep the enumeration order of their first window.");
        Check(candidates[0].WindowCount == 2 && candidates[0].IconWindow == 1,
            "Windows are grouped by executable and the topmost one supplies the icon.");
        Check(candidates[0].HasNotification && candidates[0].IsForeground,
            "Notification and foreground state come from any window in the group.");
        Check(candidates[1].WindowCount == 1 && !candidates[1].HasNotification && !candidates[1].IsForeground,
            "An unrelated app keeps its own state.");

        // One app can spawn helpers with different executables (e.g. FortiClient tray/popup).
        // Windows groups them by AppUserModelID; secondary docks must do the same so closing one
        // window does not make a "new" app button appear for a helper process.
        const string Helper = @"C:\Apps\VendorHelper.exe";
        IReadOnlyList<TrackedWindow> aumidWindows =
        [
            new(10, "Main", First, AppUserModelId: "Vendor.Suite_abc!App"),
            new(11, "Helper", Helper, AppUserModelId: "Vendor.Suite_abc!App"),
            new(12, "Other", Second, AppUserModelId: "Other.App_xyz!App")
        ];
        var aumidExists = new HashSet<string>([First, Helper, Second], StringComparer.OrdinalIgnoreCase);
        var aumidCandidates = RunningAppProjection.Project(aumidWindows,
            new HashSet<string>(StringComparer.OrdinalIgnoreCase), foreground: 11,
            notificationBadgesEnabled: true, aumidExists.Contains);
        Check(aumidCandidates.Count == 2 && aumidCandidates[0].WindowCount == 2
              && string.Equals(aumidCandidates[0].ExecutablePath, First, StringComparison.OrdinalIgnoreCase),
            "Windows sharing an AppUserModelID group into one candidate even with different executables.");
        Check(aumidCandidates[0].IsForeground,
            "A helper window carrying the same AUMID still marks the group as foreground.");

        const string packagedAumid = "Microsoft.WindowsTerminal_8wekyb3d8bbwe!App";
        var hostedWindow = new TrackedWindow(13, "Settings", @"C:\Windows\System32\ApplicationFrameHost.exe",
            AppUserModelId: packagedAumid);
        Check(RunningAppProjection.MatchesWindow(hostedWindow, @"C:\Windows\SystemApps\Settings.exe", packagedAumid),
            "A packaged app window hosted by ApplicationFrameHost must match its pinned app identity for previews.");
        Check(!RunningAppProjection.MatchesWindow(hostedWindow, @"C:\Windows\SystemApps\Settings.exe", "Other.App_xyz!App"),
            "A packaged app window must not match another app's preview by host executable alone.");

        var quiet = RunningAppProjection.Project(windows, excluded, foreground: 2,
            notificationBadgesEnabled: false, Exists);
        Check(quiet.All(candidate => !candidate.HasNotification),
            "Disabled notification badges must never light a running app.");
        Check(quiet.Any(candidate => candidate.IsForeground),
            "Foreground highlighting is independent of notification badges.");

        var noForeground = RunningAppProjection.Project(windows,
            new HashSet<string>(StringComparer.OrdinalIgnoreCase), 0, false, Exists);
        Check(noForeground.Count == 3 && noForeground.All(candidate => !candidate.IsForeground),
            "A zero foreground handle must clear every active app indicator.");
        Check(windows.Count == 6, "Projection must not mutate the snapshot it reads.");

        const string spotifyAumid = "SpotifyAB.SpotifyMusic_zpdnekdrzrea0!Spotify";
        const string spotifyExe = @"C:\Program Files\WindowsApps\SpotifyAB.SpotifyMusic_1.0_x64__zpdnekdrzrea0\Spotify.exe";
        Check(AppShortcutResolver.IsPackagedAppUserModelId(spotifyAumid),
            "Store apps with non-!App entrypoints (such as !Spotify) must be recognized as packaged AUMIDs.");
        Check(!AppShortcutResolver.IsPackagedAppUserModelId("com.squirrel.Discord.Discord"),
            "Win32 Squirrel AUMIDs must not be treated as Store packaged AUMIDs.");
        var spotifyLaunch = AppShortcutResolver.Resolve(spotifyExe, spotifyAumid, "Spotify");
        Check(string.Equals(spotifyLaunch.Target, $"shell:AppsFolder\\{spotifyAumid}", StringComparison.OrdinalIgnoreCase),
            "Packaged Store apps like Spotify must resolve to shell:AppsFolder\\{AUMID}.");

        // #173: a pinned app with arguments/AUMID must not produce a second running icon when the running
        // window has no AUMID; two pins sharing one executable must stay distinct.
        var pinnedSpotify = new DockApp("Spotify", spotifyExe)
        {
            LaunchArguments = "--pinned",
            AppUserModelId = spotifyAumid
        };
        var spotifyWindow = new TrackedWindow(14, "Spotify", spotifyExe);
        Check(OpenTaskbar.MainWindow.MatchesPinnedWindow(pinnedSpotify, spotifyWindow, 1),
            "A pin with arguments must match a same-path running window that has no AUMID.");
        Check(!OpenTaskbar.MainWindow.MatchesPinnedWindow(pinnedSpotify, spotifyWindow, 2),
            "Two pins on one executable must not both claim the same running window.");
        var deduped = RunningAppProjection.Project([spotifyWindow],
            new HashSet<string>(StringComparer.OrdinalIgnoreCase), 0, false, static _ => true,
            excludedWindow: window => OpenTaskbar.MainWindow.MatchesPinnedWindow(pinnedSpotify, window, 1));
        Check(deduped.Count == 0,
            "A running window already represented by a pin must not create a second running-app button.");

        RunAsyncTrackerChecks(Check);

        return checks;
    }

    private static void RunAsyncTrackerChecks(Action<bool, string> check)
    {
        using var uiReady = new ManualResetEventSlim(false);
        Dispatcher? uiDispatcher = null;
        var uiThreadId = -1;
        var uiThread = new Thread(() =>
        {
            uiDispatcher = Dispatcher.CurrentDispatcher;
            uiThreadId = Environment.CurrentManagedThreadId;
            uiReady.Set();
            Dispatcher.Run();
        })
        {
            IsBackground = true,
            Name = "NativeWindowTrackerTestDispatcher"
        };
        uiThread.SetApartmentState(ApartmentState.STA);
        uiThread.Start();
        check(uiReady.Wait(TimeSpan.FromSeconds(5)) && uiDispatcher is not null,
            "Test UI dispatcher thread must start promptly.");

        try
        {
            var dispatcher = uiDispatcher!;
            CheckAsyncOffloadAndCoalescing(dispatcher, uiThreadId, check);
            CheckImmutableSnapshotAndDeduping(dispatcher, check);
            CheckStopAndDisposeCancellationSafety(dispatcher, check);
            CheckStopCancelsPendingScanAndQueuedRefresh(dispatcher, check);

            // Fast sync foreground query must remain synchronous and safe from any caller.
            var fg = NativeWindowTracker.GetForegroundApplicationWindow();
            check(fg >= 0 || fg < 0, "GetForegroundApplicationWindow must return synchronously without throwing.");
        }
        finally
        {
            uiDispatcher?.InvokeShutdown();
            uiThread.Join(TimeSpan.FromSeconds(5));
        }
    }

    private static void CheckAsyncOffloadAndCoalescing(Dispatcher dispatcher, int uiThreadId, Action<bool, string> check)
    {
        var scanCount = 0;
        var activeScans = 0;
        var maxConcurrentScans = 0;
        var scanThreadIds = new List<int>();
        var eventThreadIds = new List<int>();
        var eventCounts = 0;

        using var scan1Started = new ManualResetEventSlim(false);
        using var releaseScan1 = new ManualResetEventSlim(false);
        using var scan2Published = new ManualResetEventSlim(false);

        using var tracker = new NativeWindowTracker(dispatcher, ct =>
        {
            var current = Interlocked.Increment(ref activeScans);
            try
            {
                int observed;
                do
                {
                    observed = Volatile.Read(ref maxConcurrentScans);
                    if (current <= observed) break;
                }
                while (Interlocked.CompareExchange(ref maxConcurrentScans, current, observed) != observed);

                lock (scanThreadIds) scanThreadIds.Add(Environment.CurrentManagedThreadId);
                var n = Interlocked.Increment(ref scanCount);
                if (n == 1)
                {
                    scan1Started.Set();
                    releaseScan1.Wait(TimeSpan.FromSeconds(5));
                    return [new TrackedWindow(101, "Scan 1", First)];
                }

                return [new TrackedWindow(101, "Scan 1", First), new TrackedWindow(102, "Scan 2", Second)];
            }
            finally
            {
                Interlocked.Decrement(ref activeScans);
            }
        });

        tracker.WindowsChanged += (_, snapshot) =>
        {
            lock (eventThreadIds) eventThreadIds.Add(Environment.CurrentManagedThreadId);
            if (Interlocked.Increment(ref eventCounts) >= 2 && snapshot.Count == 2)
            {
                scan2Published.Set();
            }
        };

        // Start scan #1 on background worker.
        tracker.RefreshWindows();
        check(scan1Started.Wait(TimeSpan.FromSeconds(5)), "First window scan must start on the background worker.");

        // Hammer 25 concurrent refresh requests while scan #1 is active; all must coalesce into at most 1 queued rescan.
        Parallel.For(0, 25, _ => tracker.RefreshWindows());

        // Concurrently read Snapshot from another thread while scan #1 is in flight.
        var midScanSnapshot = tracker.Snapshot;
        check(midScanSnapshot is not null && midScanSnapshot.Count == 0,
            "Snapshot must remain thread-safe and valid while a background scan is in flight.");

        releaseScan1.Set();
        check(scan2Published.Wait(TimeSpan.FromSeconds(5)),
            "The single coalesced follow-up scan must run and publish its snapshot.");

        // Flush dispatcher queue to make sure no stray third scan is pending.
        dispatcher.Invoke(() => { }, DispatcherPriority.ApplicationIdle);

        check(Volatile.Read(ref maxConcurrentScans) == 1,
            $"Background window scans must be strictly serialized (maxConcurrentScans={maxConcurrentScans}).");
        check(Volatile.Read(ref scanCount) == 2,
            $"Bursty WinEvents must coalesce into 1 active + 1 queued scan (got {scanCount}).");
        check(scanThreadIds.All(id => id != uiThreadId),
            "Window enumeration must execute off the WPF UI dispatcher thread.");
        check(eventThreadIds.Count == 2 && eventThreadIds.All(id => id == uiThreadId),
            "WindowsChanged must be dispatched back onto the WPF UI dispatcher thread.");
        check(tracker.Snapshot.Count == 2 && tracker.Snapshot[1].Handle == 102,
            "Finalized Snapshot must reflect the latest coalesced scan.");
    }

    private static void CheckImmutableSnapshotAndDeduping(Dispatcher dispatcher, Action<bool, string> check)
    {
        var eventCount = 0;
        var scanRuns = 0;
        using var firstPublished = new ManualResetEventSlim(false);
        using var secondScanDone = new ManualResetEventSlim(false);

        using var tracker = new NativeWindowTracker(dispatcher, _ =>
        {
            var run = Interlocked.Increment(ref scanRuns);
            if (run >= 2) secondScanDone.Set();
            return [new TrackedWindow(201, "Alpha", First)];
        });

        tracker.WindowsChanged += (_, _) =>
        {
            Interlocked.Increment(ref eventCount);
            firstPublished.Set();
        };

        tracker.RefreshWindows();
        check(firstPublished.Wait(TimeSpan.FromSeconds(5)), "Initial snapshot must publish via WindowsChanged.");

        var snapshot = tracker.Snapshot;
        var mutationRejected = false;
        if (snapshot is IList<TrackedWindow> mutableView)
        {
            try { mutableView.Clear(); }
            catch (NotSupportedException) { mutationRejected = true; }
        }
        else
        {
            mutationRejected = true;
        }
        check(mutationRejected, "Published Snapshot must be immutable.");

        // Trigger a second scan returning an identical window sequence; WindowsChanged must not fire again.
        tracker.RefreshWindows();
        check(secondScanDone.Wait(TimeSpan.FromSeconds(5)), "Second scan must complete.");
        dispatcher.Invoke(() => { }, DispatcherPriority.ApplicationIdle);
        check(Volatile.Read(ref eventCount) == 1,
            $"Unchanged window snapshots must not fire redundant WindowsChanged events (got {eventCount}).");
    }

    private static void CheckStopAndDisposeCancellationSafety(Dispatcher dispatcher, Action<bool, string> check)
    {
        var phase = 0;
        var cancelledObservedInStop = 0;
        var cancelledObservedInDispose = 0;
        var publishedSnapshots = new List<IReadOnlyList<TrackedWindow>>();

        using var scan1Started = new ManualResetEventSlim(false);
        using var releaseScan1 = new ManualResetEventSlim(false);
        using var scan1Exited = new ManualResetEventSlim(false);
        using var scan2Published = new ManualResetEventSlim(false);
        using var scan3Started = new ManualResetEventSlim(false);
        using var releaseScan3 = new ManualResetEventSlim(false);
        using var scan3Exited = new ManualResetEventSlim(false);

        var tracker = new NativeWindowTracker(dispatcher, ct =>
        {
            var current = Interlocked.Increment(ref phase);
            if (current == 1)
            {
                scan1Started.Set();
                releaseScan1.Wait(TimeSpan.FromSeconds(5));
                if (ct.IsCancellationRequested) Interlocked.Exchange(ref cancelledObservedInStop, 1);
                scan1Exited.Set();
                return [new TrackedWindow(301, "Stale Stopped Scan", First)];
            }

            if (current == 2)
            {
                return [new TrackedWindow(302, "Fresh After Stop", Second)];
            }

            scan3Started.Set();
            releaseScan3.Wait(TimeSpan.FromSeconds(5));
            if (ct.IsCancellationRequested) Interlocked.Exchange(ref cancelledObservedInDispose, 1);
            scan3Exited.Set();
            return [new TrackedWindow(303, "Stale Disposed Scan", First)];
        });

        try
        {
            tracker.WindowsChanged += (_, snapshot) =>
            {
                lock (publishedSnapshots) publishedSnapshots.Add(snapshot);
                if (snapshot.Any(w => w.Handle == 302)) scan2Published.Set();
            };

            // 1. Stop() while scan #1 is in flight must cancel it and drop its stale result.
            tracker.RefreshWindows();
            check(scan1Started.Wait(TimeSpan.FromSeconds(5)), "Scan #1 must start before Stop().");
            tracker.Stop();
            releaseScan1.Set();
            check(scan1Exited.Wait(TimeSpan.FromSeconds(5)), "Scan #1 must exit promptly after release.");
            dispatcher.Invoke(() => { }, DispatcherPriority.ApplicationIdle);

            check(Volatile.Read(ref cancelledObservedInStop) == 1,
                "Stop() must cancel the in-flight scan's CancellationToken.");
            check(publishedSnapshots.Count == 0 && tracker.Snapshot.Count == 0,
                "A scan cancelled by Stop() must not update Snapshot or fire WindowsChanged.");

            // 2. Tracker remains reusable after Stop().
            tracker.RefreshWindows();
            check(scan2Published.Wait(TimeSpan.FromSeconds(5)),
                "A scan triggered after Stop() must publish normally.");
            check(tracker.Snapshot.Count == 1 && tracker.Snapshot[0].Handle == 302,
                "Snapshot must update after restarting scans following Stop().");

            // 3. Dispose() while scan #3 is in flight must cancel it, clear Snapshot, and suppress any post-dispose events.
            tracker.RefreshWindows();
            check(scan3Started.Wait(TimeSpan.FromSeconds(5)), "Scan #3 must start before Dispose().");
            tracker.Dispose();
            releaseScan3.Set();
            check(scan3Exited.Wait(TimeSpan.FromSeconds(5)), "Scan #3 must exit promptly after release.");
            dispatcher.Invoke(() => { }, DispatcherPriority.ApplicationIdle);

            check(Volatile.Read(ref cancelledObservedInDispose) == 1,
                "Dispose() must cancel the in-flight scan's CancellationToken.");
            check(publishedSnapshots.Count == 1 && tracker.Snapshot.Count == 0,
                "Dispose() must clear Snapshot and suppress WindowsChanged for in-flight scans.");

            // 4. Subsequent refresh calls after Dispose() must be safe no-ops.
            var phaseBefore = Volatile.Read(ref phase);
            tracker.RefreshWindows();
            dispatcher.Invoke(() => { }, DispatcherPriority.ApplicationIdle);
            check(Volatile.Read(ref phase) == phaseBefore,
                "RefreshWindows() after Dispose() must not schedule any new background scan.");
        }
        finally
        {
            tracker.Dispose();
        }
    }

    private static void CheckStopCancelsPendingScanAndQueuedRefresh(Dispatcher dispatcher, Action<bool, string> check)
    {
        var scanRuns = 0;
        var eventsFired = 0;
        using var scan1Started = new ManualResetEventSlim(false);
        using var releaseScan1 = new ManualResetEventSlim(false);
        using var scan1Exited = new ManualResetEventSlim(false);

        using var tracker = new NativeWindowTracker(dispatcher, _ =>
        {
            var run = Interlocked.Increment(ref scanRuns);
            if (run == 1)
            {
                scan1Started.Set();
                releaseScan1.Wait(TimeSpan.FromSeconds(5));
                scan1Exited.Set();
            }
            return [new TrackedWindow(401, "Cancelled Pending", First)];
        });

        tracker.WindowsChanged += (_, _) => Interlocked.Increment(ref eventsFired);

        // 1. Coalesce a pending scan while scan #1 is active, then call Stop() before scan #1 finishes.
        tracker.RefreshWindows();
        check(scan1Started.Wait(TimeSpan.FromSeconds(5)), "Scan #1 must start before coalescing.");
        tracker.RefreshWindows(); // transitions ScanRunning -> ScanRunningWithPending
        tracker.Stop();
        releaseScan1.Set();
        check(scan1Exited.Wait(TimeSpan.FromSeconds(5)), "Scan #1 must exit after release.");
        dispatcher.Invoke(() => { }, DispatcherPriority.ApplicationIdle);

        check(Volatile.Read(ref scanRuns) == 1,
            $"Stop() must cancel a coalesced ScanRunningWithPending follow-up scan (got {scanRuns} runs).");
        check(Volatile.Read(ref eventsFired) == 0 && tracker.Snapshot.Count == 0,
            "Cancelled active and pending scans must not publish a snapshot.");

        // 2. QueueRefresh(urgent: false) followed immediately by Stop() on the UI thread must not resurrect _refreshTimer.
        dispatcher.Invoke(() =>
        {
            tracker.QueueRefresh(urgent: false);
            tracker.Stop();
        }, DispatcherPriority.Normal);

        dispatcher.Invoke(() => { }, DispatcherPriority.ApplicationIdle);
        var timerEnabled = dispatcher.Invoke(() => tracker.IsRefreshTimerEnabled);
        check(!timerEnabled, "A QueueRefresh callback queued before Stop() must not resurrect _refreshTimer.");
    }
}
