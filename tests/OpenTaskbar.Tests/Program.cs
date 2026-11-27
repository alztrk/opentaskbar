using System.Globalization;
using System.IO;
using OpenTaskbar.Models;
using OpenTaskbar.Services;

// `--headless` runs only the pure logic checks: no WPF Application, no windows, no synthetic keyboard input,
// no file dialogs and no desktop side effects. The default run keeps the full UI verification suite for CI.
var headless = args.Contains("--headless", StringComparer.OrdinalIgnoreCase);
var checks = 0;
CountdownChecks();
StopwatchChecks();
CatalogChecks();
WidgetCategoryChecks();
WidgetRegistryChecks();
LocalizationChecks();
TaskManagerTextChecks();
SettingsChecks();
RadiusChecks();
InitialLanguageChecks();
MultiMonitorChecks();
checks += DynamicIslandChecks.Run();
DockIconPixelChecks();
StartMenuAndOverlayChecks();
AppLaunchChecks();
TrayListChecks();
checks += PinnedTrayChecks.RunHeadless();
ShellTokenChecks();
LiveDragMathChecks();
DragDropAndShowDesktopChecks();
DockGroupsProfilesAndPinsChecks();
checks += RunningAppChecks.Run();
checks += TaskbarWindowChecks.Run();
checks += RunningAppProjectionChecks.Run();
checks += ShellFlyoutPositionChecks.Run();
checks += MediaVolumeChecks.Run();
checks += QuickSettingsChecks.Run();
checks += AiUsageChecks.Run();
if (!headless) checks += WidgetViewChecks.Run();
checks += ThemePackChecks.Run();
if (!headless) checks += SurfaceStyleChecks.Run();
checks += ThemeTransferChecks.Run();
checks += TaskbarRecoveryChecks.Run();
checks += SecondaryDockAndInteropChecks.Run();
checks += ApplicationJumpListChecks.Run();
checks += WirelessBatteryChecks.Run(headless);
checks += ShellRecoveryChecks.Run();
checks += TraySourceChecks.RunHeadless();
if (!headless)
{
    checks += StaCheckRunner.Run(TraySourceChecks.RunUi);
    checks += StaCheckRunner.Run(DockGestureChecks.RunUi);
}
Console.WriteLine($"{checks} behavior checks passed.");

void Check(bool condition, string name)
{
    if (!condition) throw new InvalidOperationException(name);
    checks++;
}

// Launching runs off the UI thread now, so failures come back as error keys; nothing is started here.
void AppLaunchChecks()
{
    var missing = Path.Combine(Path.GetTempPath(), Path.GetRandomFileName() + ".exe");
    Check(OpenTaskbar.MainWindow.StartApplication(missing, string.Empty, elevated: false) == "ErrorAppFileNotFound",
        "A missing executable must report ErrorAppFileNotFound instead of starting anything.");
    Check(WindowsTaskbarPinImporter.TryExtractAumidFromArguments(
            "/separate,shell:AppsFolder\\Microsoft.WindowsTerminal_8wekyb3d8bbwe!App")
        == "Microsoft.WindowsTerminal_8wekyb3d8bbwe!App",
        "Explorer-based Store pins must expose their AUMID from the shortcut arguments.");
    Check(WindowsTaskbarPinImporter.TryExtractAumidFromArguments("--not-a-packaged-app") is null,
        "Unrelated arguments must not be mistaken for a packaged app AUMID.");
    Check(WindowsTaskbarPinImporter.GetAppsFolderAumid(@"shell:AppsFolder\Microsoft.WindowsTerminal_8wekyb3d8bbwe!App")
        == "Microsoft.WindowsTerminal_8wekyb3d8bbwe!App",
        "AppsFolder-only pins must keep their packaged identity through backup and restore.");
    Check(WindowsTaskbarPinImporter.GetAppsFolderAumid(@"shell:AppsFolder\Microsoft.WindowsTerminal_8wekyb3d8bbwe!App extra") is null,
        "A stored AppsFolder pin must contain only an AUMID.");
}

// The tray flyout shows its last list at once and rebuilds only when a fresh enumeration differs.
void TrayListChecks()
{
    IReadOnlyList<TrayApp> first = [new("OneDrive", @"C:\Apps\OneDrive.exe", null), new("Teams", @"C:\Apps\Teams.exe", null)];
    IReadOnlyList<TrayApp> sameWithNewIcons = [new("OneDrive", @"c:\apps\onedrive.exe", null), new("Teams", @"C:\Apps\Teams.exe", null)];
    IReadOnlyList<TrayApp> renamed = [new("OneDrive - Personal", @"C:\Apps\OneDrive.exe", null), new("Teams", @"C:\Apps\Teams.exe", null)];
    IReadOnlyList<TrayApp> shorter = [new("OneDrive", @"C:\Apps\OneDrive.exe", null)];
    Check(OpenTaskbar.Views.Shell.TrayShellButton.SameApps(first, sameWithNewIcons), "Path casing alone must not rebuild the open tray list.");
    Check(!OpenTaskbar.Views.Shell.TrayShellButton.SameApps(first, renamed), "A changed app name must refresh the open tray list.");
    Check(!OpenTaskbar.Views.Shell.TrayShellButton.SameApps(first, shorter), "An app that exited must refresh the open tray list.");
}

// Permanent shell buttons must be addressable by a free-form dock-order token, including Show Desktop.
void ShellTokenChecks()
{
    var expected = new[] { "start", "search", "taskview", "tray", "peek", "bin" };
    Check(OpenTaskbar.MainWindow.ShellDockTokens.SequenceEqual(expected, StringComparer.OrdinalIgnoreCase),
        "Every permanent shell button must have a stable dock-order token.");
    var settings = new DockSettings(ShowTaskViewButton: false, ShowTrayButton: false, ShowDesktopButton: false);
    Check(!expected.Skip(2).Any(token => OpenTaskbar.MainWindow.ShellDockTokens.Contains(token, StringComparer.OrdinalIgnoreCase)
        && (token == "taskview" ? settings.ShowTaskViewButton
            : token == "tray" ? settings.ShowTrayButton
            : token == "peek" && settings.ShowDesktopButton)),
        "Shell button visibility must be independently controllable through DockSettings.");

    var pinned = DockItemOrdering.ApplyPinnedGroups(
        ["app:a", "tray", "app:b", "taskview", "start", "search"],
        new DockSettings(ShowTaskViewButton: true));
    Check(pinned.SequenceEqual(["start", "search", "taskview", "app:a", "app:b", "tray", "bin"], StringComparer.OrdinalIgnoreCase),
        "Native shell buttons must lead; background apps must trail with the Recycle Bin rightmost regardless of the saved order.");
    var hiddenPinned = DockItemOrdering.ApplyPinnedGroups(
        ["app:a", "tray", "start", "search", "taskview", "bin"],
        new DockSettings(ShowSearchButton: false, ShowTaskViewButton: false, ShowTrayButton: false, ShowRecycleBin: false));
    Check(hiddenPinned.SequenceEqual(["start", "app:a"], StringComparer.OrdinalIgnoreCase),
        "Hidden pinned buttons must be omitted rather than leave gaps between the groups.");
}

// Live reordering (#130): pure insertion-index and neighbour-slide math.
void LiveDragMathChecks()
{
    var slots = new (double Start, double Size)[]
    {
        (0, 40), (40, 40), (80, 40), (120, 40), (160, 40)
    };
    Check(DockItemOrdering.InsertionIndex(slots, 10) == 0, "Pointer in the first half of item 0 inserts before it.");
    Check(DockItemOrdering.InsertionIndex(slots, 60) == 1, "Pointer past item 1's center inserts after it.");
    Check(DockItemOrdering.InsertionIndex(slots, 200) == 5, "Pointer past the last center appends at the end.");
    Check(DockItemOrdering.InsertionIndex(slots, 60, currentIndex: 2, hysteresis: 12) == 2,
        "Holding the pointer on a target center must not pull the item back across the same boundary.");
    Check(DockItemOrdering.InsertionIndex(slots, 60, currentIndex: 1, hysteresis: 12) == 1,
        "Holding the pointer on a target center must not push the item across the same boundary.");
    Check(DockItemOrdering.InsertionIndex(slots, 58, currentIndex: 2, hysteresis: 12) == 2,
        "Small pointer jitter near a slot center must not reverse a live insertion.");
    Check(DockItemOrdering.InsertionIndex(slots, 47, currentIndex: 2, hysteresis: 12) == 1,
        "Moving back beyond the hysteresis band must reverse the live insertion.");
    Check(DockItemOrdering.InsertionIndex(slots, 63, currentIndex: 1, hysteresis: 12) == 1,
        "Small pointer jitter on the forward side must preserve the live insertion.");
    Check(DockItemOrdering.InsertionIndex(slots, 73, currentIndex: 1, hysteresis: 12) == 2,
        "Moving forward beyond the hysteresis band must advance the live insertion.");

    // Dragging item 1 to insertion index 4 opens the gap by shifting items 2 and 3 left.
    Check(DockItemOrdering.SlideFor(1, 1, 4, 40) == 0, "The dragged item never slides itself.");
    Check(DockItemOrdering.SlideFor(2, 1, 4, 40) == -40, "Items between old and new position shift back by one slot.");
    Check(DockItemOrdering.SlideFor(3, 1, 4, 40) == -40, "Every crossed neighbour shifts by the dragged slot size.");
    Check(DockItemOrdering.SlideFor(4, 1, 4, 40) == 0, "Items past the insertion point stay put.");

    // Dragging item 3 back to index 1 opens the gap by shifting items 1 and 2 right.
    Check(DockItemOrdering.SlideFor(2, 3, 1, 40) == 40, "Backward drags shift crossed neighbours forward.");
    Check(DockItemOrdering.SlideFor(1, 3, 1, 40) == 40, "The destination neighbour shifts even on equal index.");
    Check(DockItemOrdering.SlideFor(4, 3, 1, 40) == 0, "Items after the dragged one are unaffected on backward drags.");

    // The insertion index and the saved-order move must agree: item 1 dropping at index 4 lands at index 3.
    var visible = new[] { "app:a", "app:b", "app:c", "app:d", "app:e" };
    var moved = DockItemOrdering.MoveVisibleItem(visible, visible, "app:b", 4);
    Check(moved.SequenceEqual(["app:a", "app:c", "app:d", "app:b", "app:e"], StringComparer.OrdinalIgnoreCase),
        "Live gap position must match the order saved on drop.");
}

void DragDropAndShowDesktopChecks()
{
    // Windows argument quoting must handle spaces, embedded quotes, trailing backslashes (drive roots), and prefix arguments.
    var quoted = OpenTaskbar.MainWindow.BuildQuotedArguments(
        [@"C:\My Files\report.pdf", @"C:\"],
        prefixArguments: "--reuse-window");
    Check(quoted == "--reuse-window \"C:\\My Files\\report.pdf\" \"C:\\\\\"",
        "BuildQuotedArguments must preserve prefix arguments and escape trailing backslashes cleanly.");

    var tempRoot = Path.Combine(Path.GetTempPath(), "OpenTaskbarDragDrop_" + Guid.NewGuid().ToString("N"));
    var fakeDesktop = Path.Combine(tempRoot, "Desktop");
    var sourceDir = Path.Combine(tempRoot, "Source");
    Directory.CreateDirectory(fakeDesktop);
    Directory.CreateDirectory(sourceDir);
    try
    {
        var sourceFile = Path.Combine(sourceDir, "note.txt");
        var desktopFile = Path.Combine(fakeDesktop, "already.txt");
        var fakeAppExe = Path.Combine(sourceDir, "tool.exe");
        File.WriteAllText(sourceFile, "hello");
        File.WriteAllText(desktopFile, "on desktop");
        File.WriteAllText(fakeAppExe, string.Empty);

        // Moving to Desktop must include external files, deduplicate, and skip Desktop itself, ancestors, drive roots, missing paths, and files already on Desktop.
        var movePaths = DesktopFileTransfer.FilterTransferPaths(
            [sourceFile, sourceFile, desktopFile, fakeDesktop, tempRoot, @"C:\", Path.Combine(sourceDir, "missing.txt")],
            fakeDesktop,
            copy: false);
        Check(movePaths.Count == 1 && string.Equals(movePaths[0], sourceFile, StringComparison.OrdinalIgnoreCase),
            "Desktop move filter must keep only valid external paths and skip items already on Desktop or ancestor folders.");

        // Copying to Desktop (Ctrl held) must allow duplicating a file that already sits on Desktop.
        var copyPaths = DesktopFileTransfer.FilterTransferPaths([sourceFile, desktopFile], fakeDesktop, copy: true);
        Check(copyPaths.Count == 2, "Desktop copy filter must allow copying files already located on the Desktop.");

        // App drop safety must skip self-launch and non-exe targets, and strip dropping an executable onto itself.
        Check(!OpenTaskbar.MainWindow.CanOpenFilesWithExecutable(fakeAppExe, currentProcessPath: fakeAppExe),
            "App drop must refuse self-launch when the target matches the current process.");
        Check(!OpenTaskbar.MainWindow.CanOpenFilesWithExecutable(sourceFile),
            "App drop must refuse non-executable targets.");
        Check(OpenTaskbar.MainWindow.CanOpenFilesWithExecutable(fakeAppExe, currentProcessPath: @"C:\Other\OpenTaskbar.exe"),
            "App drop must accept an existing external executable.");
        var appDropTargets = OpenTaskbar.MainWindow.FilterAppDropPaths(
            [sourceFile, fakeAppExe, Path.Combine(sourceDir, "ghost.txt")],
            fakeAppExe);
        Check(appDropTargets.Count == 1 && string.Equals(appDropTargets[0], sourceFile, StringComparison.OrdinalIgnoreCase),
            "App drop path filtering must omit the target executable itself and non-existent paths.");

        // Dock background drop must accept .exe and directories while rejecting plain documents.
        Check(WindowsTaskbarPinImporter.IsSupportedDockDropPath(fakeAppExe), "Dock drop must accept .exe files.");
        Check(WindowsTaskbarPinImporter.IsSupportedDockDropPath(sourceDir), "Dock drop must accept folders.");
        Check(!WindowsTaskbarPinImporter.IsSupportedDockDropPath(sourceFile), "Dock drop must ignore plain documents.");

        var resolvedExe = WindowsTaskbarPinImporter.TryResolveDroppedTarget(fakeAppExe);
        Check(resolvedExe is { IsDirectory: false } && string.Equals(resolvedExe.TargetPath, fakeAppExe, StringComparison.OrdinalIgnoreCase),
            "Dropped .exe must resolve to a pin target.");
        var resolvedDir = WindowsTaskbarPinImporter.TryResolveDroppedTarget(sourceDir);
        Check(resolvedDir is { IsDirectory: true } && string.Equals(resolvedDir.TargetPath, sourceDir, StringComparison.OrdinalIgnoreCase),
            "Dropped directory must resolve to a folder stack target.");
    }
    finally
    {
        try { Directory.Delete(tempRoot, recursive: true); } catch { }
    }
}

void DockGroupsProfilesAndPinsChecks()
{
    const string appA = @"C:\Apps\Alpha.exe";
    const string appB = @"C:\Apps\Bravo.exe";
    const string appC = @"C:\Apps\Charlie.exe";
    const string appExternal = @"C:\Apps\ExternalPin.exe";

    // Bug #1: Creating a new empty group and adding/dropping its first app must not delete the empty target group.
    var emptyGroup = new DockAppGroup("grp-new", "New Group", []);
    var afterRemoveUnassigned = OpenTaskbar.MainWindow.RemoveAppFromGroups([emptyGroup], appA, preserveGroupId: emptyGroup.Id);
    Check(afterRemoveUnassigned.Count == 1 && afterRemoveUnassigned[0].Id == emptyGroup.Id,
        "Removing an unassigned app from previous groups must preserve a newly created empty target group.");
    var populatedGroup = afterRemoveUnassigned
        .Select(g => g.Id == emptyGroup.Id ? g with { ExecutablePaths = [.. g.ExecutablePaths, appA] } : g)
        .ToList();
    Check(populatedGroup is [{ Id: "grp-new", ExecutablePaths: [appA] }],
        "Adding the first app to a newly created empty group must populate and retain the group.");

    // Moving the sole app from an existing group into a newly created empty group drops the emptied source group
    // while keeping and populating the new target group.
    var singleSourceGroup = new DockAppGroup("grp-old", "Old Group", [appA]);
    var afterMoveFromSingle = OpenTaskbar.MainWindow.RemoveAppFromGroups([singleSourceGroup, emptyGroup], appA, preserveGroupId: emptyGroup.Id);
    Check(afterMoveFromSingle is [{ Id: "grp-new", ExecutablePaths: [] }],
        "Removing the sole member of an old group must drop the old group while preserving the empty target group.");

    // Moving one app from a multi-app group into a newly created empty group preserves both groups.
    var multiSourceGroup = new DockAppGroup("grp-multi", "Multi Group", [appA, appB]);
    var afterMoveFromMulti = OpenTaskbar.MainWindow.RemoveAppFromGroups([multiSourceGroup, emptyGroup], appA, preserveGroupId: emptyGroup.Id);
    Check(afterMoveFromMulti is [{ Id: "grp-multi", ExecutablePaths: [appB] }, { Id: "grp-new", ExecutablePaths: [] }],
        "Moving one app out of a multi-app group into an empty target group must keep both groups.");

    // Dropping an app onto the same group where it is the sole member must preserve that group.
    var afterSameGroupDrop = OpenTaskbar.MainWindow.RemoveAppFromGroups([singleSourceGroup], appA, preserveGroupId: singleSourceGroup.Id);
    Check(afterSameGroupDrop is [{ Id: "grp-old", ExecutablePaths: [] }],
        "Dropping a sole member back onto its own group must preserve the target group for re-addition.");

    // Removing the last app from a group (unchecking in ToggleAppInGroup) must drop that group while keeping any unrelated empty group.
    var afterExplicitRemove = OpenTaskbar.MainWindow.RemoveAppFromGroups([singleSourceGroup, emptyGroup], appA);
    Check(afterExplicitRemove is [{ Id: "grp-new", ExecutablePaths: [] }],
        "Removing the last app from a group must delete only the group that became empty.");

    // Bug #2: "Hide in profile" (HiddenApps) must hide pinned apps on the dock when idle, show them from _runningApps
    // while running, and preserve HiddenApps when other items are reordered.
    var pinnedApps = new[]
    {
        new DockApp("Alpha", appA) { ShortcutPath = @"C:\Pins\Alpha.lnk" },
        new DockApp("Bravo", appB) { ShortcutPath = @"C:\Pins\Bravo.lnk" },
        new DockApp("Charlie", appC) { ShortcutPath = @"C:\Pins\Charlie.lnk" }
    };
    var profileSettings = new DockSettings(
        PinOrder: [appA, appB, appC],
        Profiles:
        [
            new DockProfile("Default", []),
            new DockProfile("Work", [appB])
        ],
        ActiveProfile: "Work");

    Check(OpenTaskbar.MainWindow.IsHiddenByProfile(profileSettings, appB)
        && OpenTaskbar.MainWindow.IsHiddenByProfile(profileSettings, appB.ToLowerInvariant())
        && !OpenTaskbar.MainWindow.IsHiddenByProfile(profileSettings, appA),
        "IsHiddenByProfile must match active profile HiddenApps case-insensitively.");

    var visiblePinsInWork = OpenTaskbar.MainWindow.GetVisiblePinnedPaths(pinnedApps, profileSettings);
    Check(visiblePinsInWork.SequenceEqual([appA, appC], StringComparer.OrdinalIgnoreCase),
        "GetVisiblePinnedPaths must exclude apps hidden by the active profile even when listed in PinOrder.");

    Check(OpenTaskbar.MainWindow.FindVisibleDockApp(pinnedApps, [], profileSettings, appB) is null,
        "FindVisibleDockApp must return null for a profile-hidden pinned app that is not running.");
    Check(ReferenceEquals(OpenTaskbar.MainWindow.FindVisibleDockApp(pinnedApps, [], profileSettings, appA), pinnedApps[0]),
        "FindVisibleDockApp must resolve non-hidden pinned apps normally.");

    var runningBravo = new DockApp("Bravo", appB) { WindowCount = 2, IsForeground = true };
    Check(ReferenceEquals(OpenTaskbar.MainWindow.FindVisibleDockApp(pinnedApps, [runningBravo], profileSettings, appB), runningBravo),
        "A profile-hidden pinned app that is currently running must resolve to its running DockApp instance.");

    var savedOrder = new[] { "start", "app:" + appA, "app:" + appB, "app:" + appC, "tray" };
    var visibleOrder = new[] { "app:" + appA, "app:" + appC };
    var reorderedWithHidden = DockItemOrdering.MoveVisibleItem(savedOrder, visibleOrder, "app:" + appC, 0);
    var reorderedWorkSettings = DockProfileService.SyncActive(profileSettings with { DockItemOrder = reorderedWithHidden });
    Check(OpenTaskbar.MainWindow.IsHiddenByProfile(reorderedWorkSettings, appB)
        && reorderedWorkSettings.DockItemOrder!.Contains("app:" + appB, StringComparer.OrdinalIgnoreCase),
        "Reordering visible dock items must keep HiddenApps intact and preserve the hidden app token slot.");

    var (defaultProfileSettings, switchErr) = DockProfileService.Switch(reorderedWorkSettings, "Default");
    Check(switchErr is null
        && OpenTaskbar.MainWindow.GetVisiblePinnedPaths(pinnedApps, defaultProfileSettings)
            .SequenceEqual([appA, appB, appC], StringComparer.OrdinalIgnoreCase)
        && ReferenceEquals(OpenTaskbar.MainWindow.FindVisibleDockApp(pinnedApps, [], defaultProfileSettings, appB), pinnedApps[1]),
        "Switching to a profile that does not hide the app must restore it to the visible pinned apps.");

    // Bug #9: Pinned apps added outside OpenTaskbar or via ImportPins must remain visible when PinOrder already exists.
    var pinsWithExternal = new[]
    {
        new DockApp("Alpha", appA) { ShortcutPath = @"C:\Pins\Alpha.lnk" },
        new DockApp("Bravo", appB) { ShortcutPath = @"C:\Pins\Bravo.lnk" },
        new DockApp("External", appExternal) { ShortcutPath = @"C:\Pins\External.lnk" },
        new DockApp("Charlie", appC) { ShortcutPath = @"C:\Pins\Charlie.lnk" }
    };
    var partialPinOrderSettings = new DockSettings(
        PinOrder: [appB, appA, @"C:\Apps\UnpinnedStale.exe"]);
    var mergedPins = OpenTaskbar.MainWindow.GetVisiblePinnedPaths(pinsWithExternal, partialPinOrderSettings);
    Check(mergedPins.SequenceEqual([appB, appA, appExternal, appC], StringComparer.OrdinalIgnoreCase),
        "GetVisiblePinnedPaths must keep PinOrder sequence for known pins, append externally pinned/imported apps missing from PinOrder, and drop stale unpinned entries.");
}

void CountdownChecks()
{
    var clock = new TestClock();
    var timer = new CountdownDockWidget(clock);
    timer.Refresh();
    Check(timer.State == CountdownState.NotConfigured && !timer.CanToggle && timer.Value == string.Empty,
        "A timer without a duration must not look configured.");

    var errorChanges = 0;
    timer.PropertyChanged += (_, e) => { if (e.PropertyName == nameof(timer.HasValidationError)) errorChanges++; };
    foreach (var invalid in new[] { "0", "1441", "-1", "abc", "1.5", "99999999999999" })
    {
        timer.MinutesText = invalid;
        Check(timer.HasValidationError && !timer.CanToggle, $"Reject invalid duration: {invalid}");
        timer.Toggle();
        Check(!timer.IsRunning, "An invalid duration must not start through a direct model action.");
    }
    Check(errorChanges > 0, "Invalid input must notify the visible validation state.");
    timer.MinutesText = " ";
    Check(!timer.HasValidationError && !timer.CanToggle, "Clearing the input must remove its validation error.");

    timer.MinutesText = "1";
    Check(timer.State == CountdownState.Ready && timer.Value == "00:01:00", "One minute must prepare a sixty-second timer.");
    timer.Toggle();
    clock.Advance(TimeSpan.FromSeconds(20));
    timer.Refresh();
    Check(timer.IsRunning && timer.Value == "00:00:40" && !timer.CanEditDuration, "Running time must count down.");
    timer.MinutesText = "5";
    Check(timer.MinutesText == "1", "Running duration must not be editable.");

    timer.Toggle();
    clock.Advance(TimeSpan.FromHours(1));
    timer.Refresh();
    Check(timer.State == CountdownState.Paused && timer.Value == "00:00:40", "Paused time must stay fixed.");
    timer.Toggle();
    clock.Advance(TimeSpan.FromSeconds(39.5));
    timer.Refresh();
    Check(timer.IsRunning && timer.Value == "00:00:01", "A fractional final second must remain visible.");
    clock.Advance(TimeSpan.FromSeconds(0.5));
    timer.Refresh();
    Check(timer.State == CountdownState.Finished && !timer.IsRunning && timer.Value == "00:00:00",
        "The timer must finish exactly at zero.");
    Check(timer.StatusText == UiText.Get("CountdownFinished") && timer.CanEditDuration,
        "Completion must have visible text and allow another duration.");
    clock.Advance(TimeSpan.FromHours(10));
    timer.Refresh();
    Check(timer.Value == "00:00:00", "A finished timer must never show negative time.");
    timer.Toggle();
    Check(timer.IsRunning && timer.Value == "00:01:00", "Restart must use the configured duration.");
    timer.Reset();
    Check(timer.State == CountdownState.Ready && !timer.IsRunning && timer.Value == "00:01:00",
        "Reset must stop the timer and restore its duration.");
    timer.MinutesText = "1440";
    Check(timer.Value == "24:00:00", "The upper duration boundary must not wrap at midnight.");
}

void StopwatchChecks()
{
    var clock = new TestClock();
    var stopwatch = new StopwatchDockWidget(clock);
    stopwatch.Refresh();
    Check(!stopwatch.IsRunning && stopwatch.Value == "00:00:00", "The stopwatch must start idle.");
    stopwatch.Toggle();
    clock.Advance(TimeSpan.FromSeconds(65));
    stopwatch.Refresh();
    Check(stopwatch.Value == "00:01:05", "Elapsed time must cross minute boundaries.");
    stopwatch.Toggle();
    clock.Advance(TimeSpan.FromHours(1));
    stopwatch.Refresh();
    Check(!stopwatch.IsRunning && stopwatch.Value == "00:01:05", "Paused stopwatch time must stay fixed.");
    stopwatch.Toggle();
    clock.Advance(TimeSpan.FromSeconds(5));
    stopwatch.Refresh();
    Check(stopwatch.Value == "00:01:10", "Resume must accumulate elapsed time.");
    clock.Advance(TimeSpan.FromHours(26));
    stopwatch.Refresh();
    Check(stopwatch.Value == "26:01:10", "Stopwatch hours must not wrap at midnight.");
    stopwatch.Reset();
    Check(!stopwatch.IsRunning && stopwatch.Value == "00:00:00", "Reset must clear and stop the stopwatch.");
}

void CatalogChecks()
{
    var clock = new TestClock();
    var catalog = new DockWidgetService(clock);
    Check(catalog.CreateWidgets(null).Count == 0, "New settings must not install widgets implicitly.");
    Check(catalog.CreateOptions([], []).All(option => !option.IsInstalled && !option.IsEnabled),
        "The available catalog must be optional.");
    Check(catalog.CreateOptions([], []).All(option => option.Id != "Search"),
        "Search must be a dock button, not a catalog widget.");
    var legacySearch = DockSettingsStore.MigrateRemovedWidgets(new DockSettings(
        Widgets: ["Search", "Clock"], InstalledWidgets: ["Search", "Clock"], WidgetOrder: ["Search", "Clock"],
        DockItemOrder: ["start", "widget:Search", "widget:Clock"],
        WidgetDensityOverrides: new Dictionary<string, WidgetDensityMode> { ["Search"] = WidgetDensityMode.Compact }));
    Check(legacySearch.Widgets!.SequenceEqual(["Clock"])
        && legacySearch.InstalledWidgets!.SequenceEqual(["Clock"])
        && legacySearch.WidgetOrder!.SequenceEqual(["Clock"])
        && legacySearch.DockItemOrder!.SequenceEqual(["start", "widget:Clock"])
        && legacySearch.WidgetDensityOverrides!.Count == 0
        && legacySearch.ShowSearchButton
        && DockSettingsStore.Validate(legacySearch),
        "The retired Search widget must migrate into a visible dock search button.");
    var legacyShell = DockSettingsStore.MigrateRemovedWidgets(new DockSettings(
        Widgets: ["Desktops", "Clock"], InstalledWidgets: ["Desktops", "Tray", "Clock"],
        WidgetOrder: ["Desktops", "Tray", "Clock"], DockItemOrder: ["start", "widget:Desktops", "widget:Tray", "widget:Clock"],
        ShowTaskViewButton: false, ShowTrayButton: false));
    Check(legacyShell.Widgets!.SequenceEqual(["Clock"])
        && legacyShell.InstalledWidgets!.SequenceEqual(["Clock"])
        && legacyShell.WidgetOrder!.SequenceEqual(["Clock"])
        && legacyShell.DockItemOrder!.SequenceEqual(["start", "widget:Clock"])
        && legacyShell.ShowTaskViewButton && !legacyShell.ShowTrayButton
        && DockSettingsStore.Validate(legacyShell),
        "The retired Desktops and Tray widgets must migrate into dock shell buttons with their enabled state.");
    var first = catalog.CreateWidgets([DockWidgetService.StopwatchId, DockWidgetService.CountdownId]);
    var options = catalog.CreateOptions([], []);
    Check(DockWidgetService.AiUsageWidgets.Select(definition => definition.Provider).Order().SequenceEqual(
            Enum.GetValues<AiProviders>().Where(provider => provider is not (AiProviders.None or AiProviders.All)).Order())
        && DockWidgetService.AiUsageWidgets.All(definition => options.SingleOrDefault(option => option.Id == definition.Id) is { } aiOption
            && aiOption.Label == UiText.Get(definition.LabelKey) && aiOption.Description == UiText.Get(definition.LabelKey + "Description")
            && !aiOption.HasSettings)
        && options.All(option => option.Id != "AiUsage"),
        "Every AI provider must be its own catalog widget with a localized card and no settings panel.");
    var legacy = DockSettingsStore.MigrateLegacyAiUsage(new DockSettings(
        Widgets: ["AiUsage", "Clock"], InstalledWidgets: ["AiUsage", "Clock"], WidgetOrder: ["Clock", "AiUsage"],
        DockItemOrder: ["start", "widget:AiUsage"], AiUsageProviders: AiProviders.Claude | AiProviders.Codex,
        WidgetDensityOverrides: new Dictionary<string, WidgetDensityMode> { ["AiUsage"] = WidgetDensityMode.Compact }));
    Check(legacy.Widgets!.SequenceEqual(["AiUsageClaude", "AiUsageCodex", "Clock"])
        && legacy.InstalledWidgets!.Count == DockWidgetService.AiUsageWidgets.Count + 1
        && legacy.WidgetOrder!.Count == DockWidgetService.AiUsageWidgets.Count + 1 && legacy.WidgetOrder[0] == "Clock"
        && legacy.DockItemOrder!.Contains("widget:AiUsageOpenRouter") && !legacy.DockItemOrder!.Contains("widget:AiUsage")
        && legacy.WidgetDensityOverrides!["AiUsageCopilot"] == WidgetDensityMode.Compact && !legacy.WidgetDensityOverrides.ContainsKey("AiUsage")
        && legacy.AiUsageProviders == AiProviders.None && DockSettingsStore.Validate(legacy),
        "The retired single AI usage widget must migrate into one valid widget per provider.");
    Check(first.Count == 2 && first.OfType<ITimedDockWidget>().Count() == 2, "Both time widgets must be available.");
    var stopwatch = first.OfType<StopwatchDockWidget>().Single();
    var timer = first.OfType<CountdownDockWidget>().Single();
    timer.MinutesText = "1";
    stopwatch.Toggle();
    Check(stopwatch.IsRunning && !timer.IsRunning, "Time widgets must have independent state.");
    timer.Toggle();
    catalog.CreateWidgets([]);
    clock.Advance(TimeSpan.FromSeconds(30));
    var second = catalog.CreateWidgets([DockWidgetService.StopwatchId, DockWidgetService.CountdownId]);
    DockWidgetService.Refresh(second, AppLanguage.Turkish);
    Check(ReferenceEquals(stopwatch, second[0]) && ReferenceEquals(timer, second[1]), "Hiding must preserve the session instances.");
    Check(stopwatch.Value == "00:00:30" && timer.Value == "00:00:30", "Hidden widgets must retain elapsed time.");
}

void WidgetCategoryChecks()
{
    var clock = new TestClock();
    var catalog = new DockWidgetService(clock);
    var options = catalog.CreateOptions([], []);

    Check(options.Count == 22, "Catalog must define exactly 22 widgets.");

    var timeWidgets = options.Where(o => o.CategoryEnum == DockWidgetCategory.TimeAndWeather).Select(o => o.Id).ToArray();
    Check(timeWidgets.Length == 5
        && timeWidgets.Contains(DockWidgetService.ClockId)
        && timeWidgets.Contains(DockWidgetService.WeatherId)
        && timeWidgets.Contains("DayProgress")
        && timeWidgets.Contains(DockWidgetService.StopwatchId)
        && timeWidgets.Contains(DockWidgetService.CountdownId),
        "TimeAndWeather category must contain Clock, Weather, DayProgress, Stopwatch and Countdown.");

    var systemWidgets = options.Where(o => o.CategoryEnum == DockWidgetCategory.SystemAndHardware).Select(o => o.Id).ToArray();
    Check(systemWidgets.Length == 5
        && systemWidgets.Contains(DockWidgetService.SystemStatsId)
        && systemWidgets.Contains(DockWidgetService.NetSpeedId)
        && systemWidgets.Contains(DockWidgetService.BatteryId)
        && systemWidgets.Contains(DockWidgetService.BluetoothBatteryId)
        && systemWidgets.Contains("Storage"),
        "SystemAndHardware category must contain SystemStats, NetSpeed, Battery, BluetoothBattery and Storage.");

    var aiWidgets = options.Where(o => o.CategoryEnum == DockWidgetCategory.AiQuotas).Select(o => o.Id).ToArray();
    Check(aiWidgets.Length == 7
        && DockWidgetService.AiUsageWidgets.All(w => aiWidgets.Contains(w.Id)),
        "AiQuotas category must contain all 7 AI provider widgets.");

    var mediaWidgets = options.Where(o => o.CategoryEnum == DockWidgetCategory.MediaAndControls).Select(o => o.Id).ToArray();
    Check(mediaWidgets.Length == 4
        && mediaWidgets.Contains(DockWidgetService.MediaId)
        && mediaWidgets.Contains(DockWidgetService.AudioId)
        && mediaWidgets.Contains(DockWidgetService.QuickSettingsId)
        && mediaWidgets.Contains(DockWidgetService.KeyboardLayoutId),
        "MediaAndControls category must contain Media, Audio, QuickSettings and KeyboardLayout.");

    var prodWidgets = options.Where(o => o.CategoryEnum == DockWidgetCategory.Productivity).Select(o => o.Id).ToArray();
    Check(prodWidgets.SequenceEqual([DockWidgetService.FocusId]),
        "Productivity category must contain only Focus.");

    UiText.Current.SetLanguage(AppLanguage.Turkish);
    var sample = options.First(o => o.CategoryEnum == DockWidgetCategory.AiQuotas);
    Check(sample.CategoryLabel == "Yapay Zekâ", "Category label must be localized in Turkish.");
    UiText.Current.SetLanguage(AppLanguage.English);
    Check(sample.CategoryLabel == "AI Quotas", "Category label must be localized in English.");
    UiText.Current.SetLanguage(AppLanguage.Turkish);
}

void WidgetRegistryChecks()
{
    var all = DockWidgetService.AllDescriptors;
    var catalog = DockWidgetService.CatalogDescriptors;
    var live = DockWidgetService.LiveDescriptors;

    // Validation, catalog and dock lists must all be projections of the one registry.
    Check(DockWidgetService.SupportedIds.Count == all.Count,
        "SupportedIds must be derived from the widget registry, not maintained separately.");
    Check(DockWidgetService.SupportedIds.Distinct(StringComparer.OrdinalIgnoreCase).Count() == all.Count,
        "Widget IDs must be unique case-insensitively.");
    Check(all.All(descriptor => DockWidgetService.SupportedIds.Contains(descriptor.Id)),
        "Every registered widget must be accepted by settings validation.");
    Check(catalog.All(descriptor => DockWidgetService.TryGetDescriptor(descriptor.Id, out _)),
        "Every catalog card must resolve through the registry.");
    Check(all.Count(descriptor => string.Equals(descriptor.Id, DockWidgetService.DateId, StringComparison.Ordinal)) == 1,
        "The legacy Date alias must stay registered so old settings files remain valid.");

    // Explicit dock order protects saved WidgetOrder / DockItemOrder arrays from refactors.
    var expectedOrder = new[]
    {
        "Media", "SystemStats", "Focus", "Weather", "NetSpeed", "BluetoothBattery", "Battery", "Storage",
        "DayProgress", "AiUsageClaude", "AiUsageCodex", "AiUsageOpenCode",
        "AiUsageCopilot", "AiUsageDeepSeek", "AiUsageOpenRouter", "AiUsageCursor", "Audio", "QuickSettings", "Clock",
        "KeyboardLayout"
    };
    Check(live.Select(descriptor => descriptor.Id).SequenceEqual(expectedOrder),
        "Remaining live widgets must preserve their dock order.");
    Check(DockSettingsStore.Validate(new DockSettings(Widgets: ["ShowDesktop"], InstalledWidgets: ["ShowDesktop"])),
        "Saved settings with the retired ShowDesktop widget must remain valid.");

    // OLED / display-care settings are validated independently and round-trip through the saved schema.
    var oledSettings = new DockSettings(
        DockBackground: SurfaceBackground.TrueBlack,
        OledReduceGlowEnabled: true, BurnInShiftEnabled: true, BurnInShiftMinutes: 15,
        OledAutoTriggerEnabled: true, OledLowBatteryThreshold: 50,
        OledAutoOnNightLight: true, AmbientModeEnabled: true, AnimationFpsCap: 120);
    Check(DockSettingsStore.Validate(oledSettings) && Enum.IsDefined(SurfaceBackground.TrueBlack),
        "TrueBlack surfaces and all display-care settings must validate.");
    Check(!DockSettingsStore.Validate(oledSettings with { BurnInShiftMinutes = 4 })
        && !DockSettingsStore.Validate(oledSettings with { BurnInShiftMinutes = 121 }),
        "Burn-in shift interval must stay within 5-120 minutes.");
    Check(!DockSettingsStore.Validate(oledSettings with { OledLowBatteryThreshold = 15 })
        && !DockSettingsStore.Validate(oledSettings with { AnimationFpsCap = 45 }),
        "Low-battery threshold and FPS cap must only accept the exposed choices.");
    var oledJson = System.Text.Json.JsonSerializer.Serialize(oledSettings, DockSettingsStore.SerializerOptions);
    var oledRoundTrip = System.Text.Json.JsonSerializer.Deserialize<DockSettings>(oledJson, DockSettingsStore.SerializerOptions);
    Check(oledRoundTrip == oledSettings, "Display-care settings must round-trip through settings.json.");
    Check(UiText.GetTranslations("SectionDisplayCare").Count == LanguageCatalog.Languages.Count,
        "Every display-care label must be translated in all bundled languages.");

    // Capability flags decide live previews instead of a second hard-coded ID list.
    Check(catalog.Where(descriptor => descriptor.SupportsLivePreview)
            .All(descriptor => live.Any(item => string.Equals(item.Id, descriptor.Id, StringComparison.OrdinalIgnoreCase))),
        "A card that advertises a live preview must have a registered dock factory.");
    Check(catalog.Where(descriptor => descriptor.Id is DockWidgetService.StopwatchId or DockWidgetService.CountdownId)
            .All(descriptor => !descriptor.SupportsLivePreview),
        "Timed tiles use their own panel and must not advertise a live dock preview.");
    Check(catalog.Where(descriptor => descriptor.HasSettings)
            .All(descriptor => DockWidgetService.TryGetDescriptor(descriptor.Id, out var found) && found.HasSettings),
        "Widget settings flags must come from the registry.");

    // Every catalog label and description must be translated in every embedded language.
    var assembly = typeof(LanguageCatalog).Assembly;
    var resources = assembly.GetManifestResourceNames()
        .Where(name => name.StartsWith("OpenTaskbar.Localization.Languages.", StringComparison.Ordinal)).ToArray();
    Check(resources.Length == 14, "Widget registry must be localized in 14 languages.");
    foreach (var resource in resources)
    {
        using var stream = assembly.GetManifestResourceStream(resource)!;
        using var document = System.Text.Json.JsonDocument.Parse(stream);
        foreach (var descriptor in catalog)
        {
            foreach (var key in new[] { descriptor.LabelKey, descriptor.LabelKey + "Description" })
            {
                Check(document.RootElement.TryGetProperty(key, out var value)
                    && value.ValueKind == System.Text.Json.JsonValueKind.String
                    && !string.IsNullOrWhiteSpace(value.GetString()),
                    $"{resource} needs a non-empty translation for '{key}' (widget {descriptor.Id}).");
            }
        }
    }
}

void LocalizationChecks()
{
    var option = new DockWidgetOption("Clock", "Saat", "Yerel saat", true, false, new DockWidget("Clock", "Saat"));
    var changes = new List<string?>();
    option.PropertyChanged += (_, e) => changes.Add(e.PropertyName);
    option.UpdateText("Clock", "Local time");
    Check(option.IsInstalled && !option.IsEnabled, "Localization must preserve installation and visibility.");
    Check(changes.SequenceEqual([nameof(option.Label), nameof(option.Description), nameof(option.CategoryLabel)]), "All localized fields must notify bindings.");
    changes.Clear();
    option.UpdateText("Clock", "Local time");
    Check(changes.Count == 0, "Unchanged text must not notify.");
    Check(option.MatchesSearch("TIME", CultureInfo.GetCultureInfo("en-US")), "English search must ignore English casing.");
    Check(!option.MatchesSearch("missing", CultureInfo.GetCultureInfo("en-US")), "An unmatched query must stay empty.");
    option.UpdateText("Pil", "Şarj durumu");
    Check(option.MatchesSearch("PİL", CultureInfo.GetCultureInfo("tr-TR")), "Turkish search must match dotted capital I.");

    UiText.Current.SetLanguage(AppLanguage.English);
    Check(UiText.Current.IsEnglish && UiText.Current.Language == AppLanguage.English, "UiText must reflect English language selection.");
    Check(UiText.Get("ErrorDockAttach") == "Cannot connect the dock window to Windows.", "Error keys must resolve to English.");

    UiText.Current.SetLanguage(AppLanguage.Turkish);
    Check(!UiText.Current.IsEnglish && UiText.Current.Language == AppLanguage.Turkish, "UiText must reflect Turkish language selection.");
    Check(UiText.Get("ErrorDockAttach") == "Dock penceresi Windows'a bağlanamadı.", "Error keys must resolve to Turkish.");
    // Theme editor labels are composed as ThemeField<token>; the key spelling must match the catalogs.
    Check(UiText.TryGet("ThemeFieldTextPrimary", out var primary) && primary == "Birincil metin",
        "Theme editor token labels must resolve in Turkish.");
    Check(UiText.TryGet("ThemeFieldDescription", out var description) && description == "Açıklama",
        "Theme editor form labels must resolve in Turkish without character corruption.");
    Check(AppLog.ResolveMessage("ErrorDockAttach") == "Dock penceresi Windows'a bağlanamadı.",
        "Structured log entries must resolve the same text shown to the user.");
    Check(AppLog.ResolveMessage("raw exception text") == "raw exception text",
        "Unknown keys must fall back to raw text so logging never throws.");
    Check(UiText.Get("OpenLogFile") == "Günlük dosyasını aç", "The log-opening action must be localized.");
}

void TaskManagerTextChecks()
{
    try
    {
        UiText.Current.SetLanguage(AppLanguage.Turkish);
        Check(UiText.Get("TaskManager") == "Görev Yöneticisi", "The Task Manager menu item must have a Turkish label.");
        Check(UiText.Get("ErrorTaskManagerOpen") == "Görev Yöneticisi açılamadı.", "The Task Manager failure must have Turkish text.");
        UiText.Current.SetLanguage(AppLanguage.English);
        Check(UiText.Get("TaskManager") == "Task Manager", "The Task Manager menu item must have an English label.");
        Check(UiText.Get("ErrorTaskManagerOpen") == "Cannot open Task Manager.",
            "Task Manager error keys must resolve to English.");
    }
    finally
    {
        UiText.Current.SetLanguage(AppLanguage.Turkish);
    }
}

void RadiusChecks()
{
    Check(RadiusManager.Derive(20, 20, 12) == 12, "At the reference radius a derived radius must keep its value.");
    Check(RadiusManager.Derive(32, 20, 12) == 24, "Nested radii must step up with the dock radius.");
    Check(RadiusManager.Derive(16, 20, 5) == 1, "Nested radii must step down with the dock radius.");
    Check(RadiusManager.Derive(0, 20, 12) == 0, "A sharp dock must make every nested corner sharp as well.");
    Check(RadiusManager.Derive(20, 20, 18) == 18, "Theme packs with a larger nested radius must keep that offset.");
}

void SettingsChecks()
{
    var path = Path.Combine(Path.GetTempPath(), Path.GetRandomFileName());
    try
    {
        var store = new DockSettingsStore(path);
        File.WriteAllText(path, "{\"Widgets\":[\"Clock\",\"Date\"]}");
        var legacy = store.Load();
        Check(legacy.Error is null && legacy.Settings.InstalledWidgets?.SequenceEqual(["Clock", "Date"]) == true,
            "Legacy selections must migrate before validation.");
        File.WriteAllText(path, "{\"Widgets\":[\"Clock\",\"Memory\"],\"InstalledWidgets\":[\"Clock\",\"Memory\",\"Storage\"]}");
        var removedMemory = store.Load();
        Check(removedMemory.Error is null && removedMemory.Settings.Widgets?.SequenceEqual(["Clock"]) == true
            && removedMemory.Settings.InstalledWidgets?.SequenceEqual(["Clock", "Storage"]) == true,
            "The removed memory widget must be dropped from stored settings without resetting them.");
        Check(store.Save(new DockSettings(Widgets: ["Countdown"], InstalledWidgets: [])) is not null,
            "Enabled widgets must be installed.");
        Check(store.Save(new DockSettings(Widgets: ["Unknown"], InstalledWidgets: ["Unknown"])) is not null,
            "Unknown widget identifiers must be rejected.");
        Check(store.Save(new DockSettings(InstalledWidgets: ["Clock", "Clock"])) is not null,
            "Duplicate installations must be rejected.");
        File.WriteAllText(path, "{\"SurfaceOpacity\":0.65}");
        var migratedSurfaces = store.Load();
        Check(migratedSurfaces.Error is null
            && Math.Abs(migratedSurfaces.Settings.DockOpacity - 0.65) < 0.001
            && Math.Abs(migratedSurfaces.Settings.MenuOpacity - 0.65) < 0.001
            && Math.Abs(migratedSurfaces.Settings.WidgetOpacity - 0.65) < 0.001
            && Math.Abs(migratedSurfaces.Settings.SettingsOpacity - 0.65) < 0.001,
            "The legacy SurfaceOpacity must migrate into all four surface styles.");
        Check(store.Save(new DockSettings(DockBackground: SurfaceBackground.None, DockOpacity: 0)) is null
            && store.Load().Settings.DockBackground == SurfaceBackground.None,
            "A fully transparent dock style must persist and reload.");
        Check(store.Save(new DockSettings(MenuOpacity: 1.2)) is not null,
            "Out-of-range surface opacity must be rejected.");
        Check(store.Save(new DockSettings(Widgets: [], InstalledWidgets: ["Countdown"])) is null,
            "An installed widget may remain hidden.");
        var hidden = store.Load();
        Check(hidden.Error is null && hidden.Settings.Widgets?.Count == 0
            && hidden.Settings.InstalledWidgets?.Contains("Countdown") == true, "Hidden installation must survive reload.");
        Check(store.Save(new DockSettings(DockScale: 1.25)) is null && store.Load().Settings.DockScale == 1.25,
            "A valid dock scale must persist and reload.");
        Check(store.Save(new DockSettings(DockScale: 0.5)) is not null && store.Save(new DockSettings(DockScale: 2.0)) is not null,
            "Out-of-range dock scale values must be rejected.");
        File.WriteAllText(path, "{\"Widgets\":[]}");
        var defaultHeight = store.Load();
        Check(defaultHeight.Error is null && defaultHeight.Settings.DockHeightMode == DockHeightMode.Auto
            && defaultHeight.Settings.DockHeightPixels == 48 && defaultHeight.Settings.DockHeightPercent == 7,
            "Older settings files must fall back to Auto dock height defaults.");
        Check(store.Save(new DockSettings(DockHeightMode: DockHeightMode.Fill, DockHeightPixels: 80, DockHeightPercent: 6)) is null
            && store.Load().Settings.DockHeightMode == DockHeightMode.Fill,
            "A valid dock height configuration must persist and reload.");
        Check(store.Save(new DockSettings(DockHeightPixels: 10)) is not null
            && store.Save(new DockSettings(DockHeightPercent: 60)) is not null
            && store.Save(new DockSettings(DockHeightMode: (DockHeightMode)99)) is not null,
            "Out-of-range dock height values must be rejected.");
    }
    finally
    {
        File.Delete(path);
        File.Delete(path + ".tmp");
    }
}

void InitialLanguageChecks()
{
    var path = Path.Combine(Path.GetTempPath(), Path.GetRandomFileName());
    var turkish = new DockSettingsStore(path, CultureInfo.GetCultureInfo("tr-TR"));
    var english = new DockSettingsStore(path, CultureInfo.GetCultureInfo("en-US"));
    var german = new DockSettingsStore(path, CultureInfo.GetCultureInfo("de-DE"));
    var unsupported = new DockSettingsStore(path, CultureInfo.GetCultureInfo("is-IS"));
    bool LoadsLanguage(DockSettingsStore store, AppLanguage language)
    {
        var result = store.Load();
        return result.Error is null && result.Settings.Language == language;
    }
    bool UsesDefaultLanguageOnError(DockSettingsStore store, AppLanguage language)
    {
        var result = store.Load();
        return result.Error is not null && result.Settings.Language == language;
    }

    try
    {
        Check(LoadsLanguage(turkish, AppLanguage.Turkish), "First launch must follow Turkish system UI language.");
        Check(LoadsLanguage(english, AppLanguage.English), "First launch must follow English system UI language.");
        Check(LoadsLanguage(german, new AppLanguage("de-DE")), "First launch must follow German system UI language.");
        Check(LoadsLanguage(unsupported, AppLanguage.English), "Unsupported system languages must use the documented English default.");
        File.WriteAllText(path, "{}");
        Check(LoadsLanguage(turkish, AppLanguage.Turkish), "A legacy file without a language must follow Turkish system UI language.");
        Check(LoadsLanguage(english, AppLanguage.English), "A legacy file without a language must follow English system UI language.");
        Check(english.Save(new DockSettings(Language: AppLanguage.Turkish)) is null, "An explicit Turkish preference must save.");
        Check(LoadsLanguage(english, AppLanguage.Turkish), "An explicit Turkish preference must survive an English system language.");
        Check(turkish.Save(new DockSettings(Language: AppLanguage.English)) is null, "An explicit English preference must save.");
        Check(LoadsLanguage(turkish, AppLanguage.English), "An explicit English preference must survive a Turkish system language.");
        File.WriteAllText(path, "{\"Language\":\"Unknown\"}");
        Check(UsesDefaultLanguageOnError(english, AppLanguage.English), "An invalid stored language must report an error in system-default settings.");
        File.WriteAllText(path, "{\"Language\":42}");
        Check(UsesDefaultLanguageOnError(turkish, AppLanguage.Turkish), "An undefined language value must report an error.");
        File.WriteAllText(path, "null");
        Check(english.Load() is { Error: not null }, "Null settings must remain an explicit error.");
    }
    finally
    {
        File.Delete(path);
        File.Delete(path + ".tmp");
    }
}

void MultiMonitorChecks()
{
    var path = Path.Combine(Path.GetTempPath(), Path.GetRandomFileName());
    try
    {
        var store = new DockSettingsStore(path);
        var targetSettings = new DockSettings(
            TargetDisplay: @"\\.\DISPLAY2",
            SecondaryApps: SecondaryAppScope.AllMonitors,
            SecondaryWidgets: false);
        Check(store.Save(targetSettings) is null, "TargetDisplay and Secondary settings must save without error.");
        var loaded = store.Load();
        Check(loaded.Error is null, "Saved multi-monitor settings must load without error.");
        Check(loaded.Settings.TargetDisplay == @"\\.\DISPLAY2", "TargetDisplay must persist and reload.");
        Check(loaded.Settings.SecondaryApps == SecondaryAppScope.AllMonitors, "SecondaryApps scope must persist and reload.");
        Check(!loaded.Settings.SecondaryWidgets, "SecondaryWidgets boolean must persist and reload.");

        var json = File.ReadAllText(path);
        Check(json.Contains("\"SecondaryApps\": \"AllMonitors\""), "SecondaryAppScope must serialize as string enum.");

        File.WriteAllText(path, "{}");
        var empty = store.Load();
        Check(empty.Error is null && empty.Settings.TargetDisplay is null, "Default TargetDisplay must be null (primary).");
        Check(empty.Settings.SecondaryApps == SecondaryAppScope.CurrentMonitorOnly, "Default SecondaryApps must be CurrentMonitorOnly.");
        Check(empty.Settings.SecondaryWidgets, "Default SecondaryWidgets must be true.");
    }
    finally
    {
        File.Delete(path);
        File.Delete(path + ".tmp");
    }

    var primary = DisplayService.GetTargetDisplay(null);
    ArgumentNullException.ThrowIfNull(primary);
    Check(!string.IsNullOrWhiteSpace(primary.DeviceName), "Target display must have a device name.");
    Check(primary.Width > 0 && primary.Height > 0, "Target display must have positive dimensions.");

    var fallback = DisplayService.GetTargetDisplay("NON_EXISTENT_DISPLAY_XYZ");
    ArgumentNullException.ThrowIfNull(fallback);
    Check(fallback.Handle == primary.Handle || fallback.IsPrimary, "Fallback display must be primary or first display.");

    var secondaries = DisplayService.GetSecondaryMonitors(null);
    Check(!secondaries.Contains(primary.Handle), "GetSecondaryMonitors must not contain the target monitor handle.");

    UiText.Current.SetLanguage(AppLanguage.English);
    Check(UiText.Get("TargetDisplay") == "Main dock display", "TargetDisplay must be localized in English.");
    Check(UiText.Get("SecondaryApps") == "Secondary display apps", "SecondaryApps must be localized in English.");
    Check(UiText.Get("SecondaryWidgets") == "Secondary display widgets", "SecondaryWidgets must be localized in English.");
    Check(UiText.Get("SecondaryAppsCurrent") == "Windows on this display only", "SecondaryAppsCurrent must be localized in English.");
    Check(UiText.Get("SecondaryAppsAll") == "All open windows", "SecondaryAppsAll must be localized in English.");

    UiText.Current.SetLanguage(AppLanguage.Turkish);
    Check(UiText.Get("TargetDisplay") == "Ana dock ekranı", "TargetDisplay must be localized in Turkish.");
    Check(UiText.Get("SecondaryApps") == "İkinci ekran uygulamaları", "SecondaryApps must be localized in Turkish.");
    Check(UiText.Get("SecondaryWidgets") == "İkinci ekranda widget göster", "SecondaryWidgets must be localized in Turkish.");
    Check(UiText.Get("SecondaryAppsCurrent") == "Yalnızca bu ekrandaki pencereler", "SecondaryAppsCurrent must be localized in Turkish.");
    Check(UiText.Get("SecondaryAppsAll") == "Tüm açık pencereler", "SecondaryAppsAll must be localized in Turkish.");
}

void DockIconPixelChecks()
{
    // Large dock at 100%: 38 DIP * 0.88 = 33.4 px must use the native 32 px artwork instead of resampling it.
    Check(ApplicationIconProvider.ComputeApplicationIconPixels(38 * 0.88, 1.0) == 32, "Large dock icons must snap to 32 px.");
    Check(ApplicationIconProvider.ComputeApplicationIconPixels(34 * 0.88, 1.0) == 32, "Medium dock icons must snap to 32 px.");
    Check(ApplicationIconProvider.ComputeApplicationIconPixels(28 * 0.88, 1.0) == 24, "Compact dock icons must snap to 24 px.");
    Check(ApplicationIconProvider.ComputeApplicationIconPixels(38 * 0.88, 1.25) == 40, "125% DPI must snap to the native 40 px size.");
    Check(ApplicationIconProvider.ComputeApplicationIconPixels(38 * 0.88, 1.5) == 48, "150% DPI must snap to the native 48 px size.");
    Check(ApplicationIconProvider.ComputeApplicationIconPixels(38 * 0.88 * 1.5, 1.5) == 75,
        "Sizes far from any native size must keep their own pixel size instead of jumping to a distant one.");
    Check(ApplicationIconProvider.ComputeApplicationIconPixels(10, 0.5) == 16, "Icons must never drop below 16 px.");

    // Magnified icons (up to 1.28x) use the smallest native size that covers the magnified size, so they are
    // scaled down instead of blurred up.
    Check(ApplicationIconProvider.ComputeHoverIconPixels(32, 1.28) == 48, "A 32 px icon magnified to 41 px must use 48 px artwork.");
    Check(ApplicationIconProvider.ComputeHoverIconPixels(24, 1.28) == 32, "A 24 px icon magnified to 31 px must use 32 px artwork.");
    Check(ApplicationIconProvider.ComputeHoverIconPixels(40, 1.28) == 64, "A 40 px icon magnified to 52 px must use 64 px artwork.");
    Check(ApplicationIconProvider.ComputeHoverIconPixels(300, 1.28) == 384, "Beyond the largest native size the exact magnified size is used.");
    Check(ApplicationIconProvider.GetHoverVariant(null) is null, "A missing icon has no hover variant.");

    // A snapped bitmap must not be drawn past its cell: that extra width/height used to appear only on the right
    // and bottom (LibreWolf at medium dock size), reading as a clipped icon.
    Check(DockMetrics.AppIconDipSize(34, 32, 1.0) == 32, "A 32 px source fits a 34 DIP cell without overshoot.");
    Check(DockMetrics.AppIconDipSize(28, 24, 1.0) == 24, "Compact icons are capped by their cell, not the old 36 DIP floor.");
    Check(DockMetrics.AppIconDipSize(38, 40, 1.0) == 38, "A larger native bitmap is capped to the dock cell.");
    Check(DockMetrics.AppIconDipSize(57, 81, 1.5) == 54, "DPI is divided out before capping so the drawn size stays in DIP.");

}

void StartMenuAndOverlayChecks()
{
    // Task switcher window classes
    Check(ShellOverlayWindows.IsTaskSwitcherClassName("XamlExplorerHostIslandWindow"), "Windows 11 switcher must be recognized.");
    Check(ShellOverlayWindows.IsTaskSwitcherClassName("MultitaskingViewFrame"), "Windows 10 switcher must be recognized.");
    Check(ShellOverlayWindows.IsTaskSwitcherClassName("TaskSwitcherWnd"), "Classic switcher must be recognized.");
    Check(!ShellOverlayWindows.IsTaskSwitcherClassName("CabinetWClass"), "Explorer cabinet must not be recognized as switcher.");
    Check(!ShellOverlayWindows.IsTaskSwitcherClassName("Notepad"), "Notepad must not be recognized as switcher.");
    Check(!ShellOverlayWindows.IsTaskSwitcherClassName(null), "Null class name must not match switcher.");

    // Start Menu process identification vs Windows Search flyout
    Check(StartMenuState.IsStartMenuProcessName("StartMenuExperienceHost"), "StartMenuExperienceHost must be recognized as Start Menu.");
    Check(StartMenuState.IsStartMenuProcessName("startmenuexperiencehost"), "Process name check must be case insensitive.");
    Check(!StartMenuState.IsStartMenuProcessName("SearchHost"), "SearchHost must NOT be treated as Start Menu.");
    Check(!StartMenuState.IsStartMenuProcessName("SearchApp"), "SearchApp must NOT be treated as Start Menu.");
    Check(!StartMenuState.IsStartMenuProcessName("explorer"), "explorer must not be treated as Start Menu.");
    Check(!StartMenuState.IsStartMenuProcessName(null), "Null process name must return false.");

    // Scoped press dismissal behavior
    var clock = new TestClock();
    var state = new StartMenuState(clock);
    Check(!state.IsOpen, "StartMenuState must start closed.");

    state.OnForegroundChanged(true);
    Check(state.IsOpen, "Foreground change to Start menu must mark it open.");
    Check(state.WasOpenAtPress(openNow: true), "When open now, WasOpenAtPress must return true.");

    state.OnForegroundChanged(false);
    Check(!state.IsOpen, "Foreground change away must mark it closed.");

    // Ambient check without pointer press must NOT return true
    Check(!state.WasOpenAtPress(openNow: false, pressTimestamp: 0),
        "Without active pointer press, a closed Start menu must never be considered open at press.");

    // A click occurring 500ms later must NOT be eaten
    var later = System.Diagnostics.Stopwatch.GetTimestamp() + (long)(0.5 * System.Diagnostics.Stopwatch.Frequency);
    Check(!state.WasOpenAtPress(openNow: false, pressTimestamp: later),
        "A later click must not be treated as open at press.");

    // Start menu positioning must follow the dock's actual Start button, wherever the user placed it (here X=570).
    // On the full-screen canvas the flyout sits centered when Windows taskbar alignment is centered, so the
    // canvas is offset by -397 to bring the flyout above the button.
    var (fullX, fullY) = ShellShortcuts.ComputeStartMenuPosition(
        windowWidth: 2560, windowHeight: 1374,
        workLeft: 0, workTop: 0, workRight: 2560, workBottom: 1374,
        anchorLeft: 570, anchorTop: 1379, anchorRight: 620, anchorBottom: 1429,
        edge: DockEdge.Bottom, dpiScale: 1.0, isLeftAligned: false);
    Check(fullX == -397, "Full-screen container must offset horizontally so the flyout aligns over the dock's Start button.");
    Check(fullY == 4, "Full-screen container must offset vertically to sit right above the dock.");

    // A left-aligned taskbar still follows the dock's Start button.
    var (leftX, _) = ShellShortcuts.ComputeStartMenuPosition(
        windowWidth: 2560, windowHeight: 1374,
        workLeft: 0, workTop: 0, workRight: 2560, workBottom: 1374,
        anchorLeft: 570, anchorTop: 1379, anchorRight: 620, anchorBottom: 1429,
        edge: DockEdge.Bottom, dpiScale: 1.0, isLeftAligned: true);
    Check(leftX == 550, "A left-aligned taskbar must still offset the flyout above the dock's Start button.");

    // Discrete window (e.g. Windows 10 or custom shell, 642x724):
    var (discreteX, discreteY) = ShellShortcuts.ComputeStartMenuPosition(
        windowWidth: 642, windowHeight: 724,
        workLeft: 0, workTop: 0, workRight: 2560, workBottom: 1374,
        anchorLeft: 570, anchorTop: 1379, anchorRight: 620, anchorBottom: 1429,
        edge: DockEdge.Bottom, dpiScale: 1.0, isLeftAligned: false);
    Check(discreteX == 562, "Discrete window must be positioned above the dock's actual Start button.");
    Check(discreteY == 642, "Discrete window must be clamped to the work area above the dock.");

    // Wide / full-width dock: the Start button sits at the far left. The real Start flyout can be a little wider
    // than the 642 DIP the placement math assumes, which shifts it left by half the difference. Its computed screen
    // position (window X plus its REAL local offset inside the canvas) must still stay inside the work area.
    foreach (var scale in new[] { 1.0, 1.25, 1.5 })
    {
        var (wideX, _) = ShellShortcuts.ComputeStartMenuPosition(
            windowWidth: 1920, windowHeight: 1080,
            workLeft: 0, workTop: 0, workRight: 1920, workBottom: 1032,
            anchorLeft: 6, anchorTop: 1037, anchorRight: 56, anchorBottom: 1087,
            edge: DockEdge.Bottom, dpiScale: scale, isLeftAligned: false);
        var realFlyoutWidth = (int)Math.Round(720 * scale);
        const int canvasWidth = 1920;
        var realFlyoutLocalX = (canvasWidth - realFlyoutWidth) / 2;
        Check(wideX + realFlyoutLocalX >= 8,
            $"Wide dock Start flyout at scale {scale} must stay fully on screen (left edge >= 8).");
    }
}

sealed class TestClock : TimeProvider
{
    private long _timestamp;
    public override long TimestampFrequency => TimeSpan.TicksPerSecond;
    public override long GetTimestamp() => _timestamp;
    public void Advance(TimeSpan elapsed) => _timestamp += elapsed.Ticks;
}
